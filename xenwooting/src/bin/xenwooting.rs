use alsa::seq::{EvCtrl, EvNote, Event, EventType, PortCap, PortType, Seq};
use alsa::Direction;
use anyhow::{Context, Result};
use log::info;
use std::collections::{HashMap, HashSet};
use std::ffi::CString;
use std::fs;
use std::hash::Hasher;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime};
use wooting_analog_wrapper as sdk;
use wooting_analog_wrapper::{FromPrimitive, HIDCodes, KeycodeType};

use xenwooting::config::{ActionBindings, Config};
use xenwooting::hidmap::{mirror_cols_4x14, parse_hid_name, rotate_4x14, HidMap, KeyLoc};
use xenwooting::mts::MtsMaster;
use xenwooting::rgb::{parse_hex_rgb, Rgb};
use xenwooting::rgb_worker::{spawn_rgb_worker, try_send_drop, RgbCmd, RgbKey};
use xenwooting::wtn::Wtn;

fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    if a == b {
        return Ordering::Equal;
    }
    fn split_parts(s: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut i = 0usize;
        let bytes = s.as_bytes();
        while i < bytes.len() {
            let is_digit = bytes[i].is_ascii_digit();
            let mut j = i + 1;
            while j < bytes.len() && bytes[j].is_ascii_digit() == is_digit {
                j += 1;
            }
            out.push(s[i..j].to_string());
            i = j;
        }
        out
    }
    let ap = split_parts(a);
    let bp = split_parts(b);
    let n = ap.len().max(bp.len());
    for i in 0..n {
        let aa = ap.get(i);
        let bb = bp.get(i);
        match (aa, bb) {
            (None, None) => break,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(aa), Some(bb)) => {
                let an = aa.parse::<u64>();
                let bn = bb.parse::<u64>();
                if let (Ok(an), Ok(bn)) = (an, bn) {
                    if an != bn {
                        return an.cmp(&bn);
                    }
                    continue;
                }
                let c = aa.to_ascii_lowercase().cmp(&bb.to_ascii_lowercase());
                if c != Ordering::Equal {
                    return c;
                }
            }
        }
    }
    a.to_ascii_lowercase().cmp(&b.to_ascii_lowercase())
}

static RUNNING: AtomicBool = AtomicBool::new(true);

extern "C" fn handle_signal(_sig: libc::c_int) {
    // Best-effort cooperative shutdown.
    RUNNING.store(false, Ordering::SeqCst);
    // Hard stop (async-signal-safe) to ensure Ctrl-C works even if a native lib misbehaves.
    unsafe {
        libc::_exit(0);
    }
}

fn install_signal_handlers() {
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_flags = 0;
        // SAFETY: transmute handler to the expected union field.
        sa.sa_sigaction = handle_signal as usize;
        libc::sigemptyset(&mut sa.sa_mask);
        libc::sigaction(libc::SIGINT, &sa, std::ptr::null_mut());
        libc::sigaction(libc::SIGTERM, &sa, std::ptr::null_mut());
        libc::sigaction(libc::SIGQUIT, &sa, std::ptr::null_mut());
    }
}

const C0_HZ: f64 = 16.351_597_831_287_414; // A4=440 reference, C0

// (analog_to_u7 helpers removed; aftertouch is peak-mapped like the Teensy firmware)

#[derive(Debug, Clone)]
enum KeyEdge {
    Down {
        device_id: u64,
        hid: HIDCodes,
        analog: f32,
        ts: Instant,
    },
    Update {
        device_id: u64,
        hid: HIDCodes,
        analog: f32,
        ts: Instant,
    },
    Up {
        device_id: u64,
        hid: HIDCodes,
        analog: f32,
        ts: Instant,
    },
}

#[derive(Debug, Clone)]
enum VelocityProfile {
    Linear,
    Gamma { gamma: f32 },
    Log { k: f32 },
    InvLog { k: f32 },
}

impl VelocityProfile {
    fn name(&self) -> String {
        match self {
            VelocityProfile::Linear => "linear".to_string(),
            VelocityProfile::Gamma { gamma } => format!("gamma={}", gamma),
            VelocityProfile::Log { k } => format!("log k={}", k),
            VelocityProfile::InvLog { k } => format!("invlog k={}", k),
        }
    }

    fn apply(&self, n: f32) -> f32 {
        let n = n.clamp(0.0, 1.0);
        match self {
            VelocityProfile::Linear => n,
            VelocityProfile::Gamma { gamma } => n.powf(gamma.max(0.01)),
            VelocityProfile::Log { k } => {
                let kk = k.max(0.01);
                (1.0 + kk * n).ln() / (1.0 + kk).ln()
            }
            VelocityProfile::InvLog { k } => {
                let kk = k.max(0.01);
                let x = 1.0 - n;
                1.0 - (1.0 + kk * x).ln() / (1.0 + kk).ln()
            }
        }
    }
}

#[derive(Debug, Clone)]
struct LedState {
    dev_idx: u8,
    row: u8,
    col: u8,
    base_rgb: (u8, u8, u8),
}

#[derive(Debug, Clone)]
enum VelState {
    Tracking {
        started: Instant,
        peak: f32,
        out_ch: u8,
        note: u8,
        playing: bool,
        last_val: u8,
        led: Option<LedState>,
    },
    Aftershock {
        quiet_since: Option<Instant>,
        out_ch: u8,
        note: u8,
        playing: bool,
        last_val: u8,
        led: Option<LedState>,
    },
}

#[derive(Debug, Clone)]
enum AftertouchMode {
    PeakMapped,
    Off,
}

impl AftertouchMode {
    fn name(&self) -> &'static str {
        match self {
            AftertouchMode::PeakMapped => "peak-mapped",
            AftertouchMode::Off => "off",
        }
    }
}

// (AftertouchProfile removed; aftertouch is peak-mapped like the Teensy firmware)

struct AlsaMidiOut {
    seq: Seq,
    port: i32,
}

impl AlsaMidiOut {
    fn new(name: &str) -> Result<Self> {
        let seq = Seq::open(None, Some(Direction::Playback), false)
            .context("Failed to open ALSA sequencer")?;
        let cname = CString::new(name).context("Invalid ALSA client name")?;
        seq.set_client_name(&cname)
            .context("Failed to set ALSA client name")?;
        let pname = CString::new(name).context("Invalid ALSA port name")?;
        let caps = PortCap::READ | PortCap::SUBS_READ;
        let typ = PortType::MIDI_GENERIC | PortType::HARDWARE | PortType::APPLICATION;
        let port = seq
            .create_simple_port(&pname, caps, typ)
            .context("Failed to create ALSA port")?;
        Ok(Self { seq, port })
    }

    fn send_note(&mut self, on: bool, ch: u8, note: u8, vel: u8) -> Result<()> {
        let ev = EvNote {
            channel: ch,
            note,
            velocity: vel,
            off_velocity: if on { 0 } else { vel },
            duration: 0,
        };
        let mut e = Event::new(
            if on {
                EventType::Noteon
            } else {
                EventType::Noteoff
            },
            &ev,
        );
        e.set_source(self.port);
        e.set_subs();
        e.set_direct();
        self.seq
            .event_output_direct(&mut e)
            .context("Failed to output ALSA event")?;
        Ok(())
    }

    fn send_polytouch(&mut self, ch: u8, note: u8, pressure: u8) -> Result<()> {
        let ev = EvNote {
            channel: ch,
            note,
            velocity: pressure,
            off_velocity: 0,
            duration: 0,
        };
        let mut e = Event::new(EventType::Keypress, &ev);
        e.set_source(self.port);
        e.set_subs();
        e.set_direct();
        self.seq
            .event_output_direct(&mut e)
            .context("Failed to output ALSA event")?;
        Ok(())
    }

    fn send_cc(&mut self, ch: u8, cc: u8, value: u8) -> Result<()> {
        let ev = EvCtrl {
            channel: ch,
            param: cc as u32,
            value: value as i32,
        };
        let mut e = Event::new(EventType::Controller, &ev);
        e.set_source(self.port);
        e.set_subs();
        e.set_direct();
        self.seq
            .event_output_direct(&mut e)
            .context("Failed to output ALSA event")?;
        Ok(())
    }
}

fn config_path() -> Result<PathBuf> {
    let base = if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        PathBuf::from(xdg)
    } else {
        let home = std::env::var("HOME").context("HOME is not set")?;
        Path::new(&home).join(".config")
    };
    Ok(base.join("xenwooting").join("config.toml"))
}

fn write_default_config(path: &Path) -> Result<()> {
    let dir = path.parent().context("config has no parent")?;
    fs::create_dir_all(dir).with_context(|| format!("create_dir_all {}", dir.display()))?;

    // Write a minimal default .wtn so first run is usable.
    let wtn_dir = dir.join("wtn");
    fs::create_dir_all(&wtn_dir)
        .with_context(|| format!("create_dir_all {}", wtn_dir.display()))?;
    let default_wtn_path = wtn_dir.join("edo12.wtn");
    if !default_wtn_path.exists() {
        let mut out = String::new();
        for board in 0..=1u8 {
            out.push_str(&format!("[Board{}]\n", board));
            let start_key = if board == 0 { 12u8 } else { 68u8 };
            for i in 0..56u8 {
                let key = start_key.saturating_add(i);
                out.push_str(&format!("Key_{}={}\n", i, key));
                out.push_str(&format!("Chan_{}=1\n", i));
                out.push_str(&format!("Col_{}=303030\n", i));
            }
            out.push('\n');
        }
        fs::write(&default_wtn_path, out)
            .with_context(|| format!("write {}", default_wtn_path.display()))?;
    }

    let mut cfg = Config {
        midi_out_name: "XenWTN".to_string(),
        refresh_hz: 250.0,
        press_threshold: 0.10,
        press_threshold_step: 0.01,
        velocity_peak_track_ms: 12,
        aftershock_ms: 35,
        velocity_max_swing: 1.0,
        aftertouch_delta: 0.01,
        boards: vec![
            xenwooting::config::BoardConfig {
                device_id: None,
                wtn_board: 0,
                rotation_deg: 0,
                mirror_cols: false,
                meta_x: 0,
                meta_y: 5,
            },
            xenwooting::config::BoardConfig {
                device_id: None,
                wtn_board: 1,
                rotation_deg: 180,
                mirror_cols: false,
                meta_x: 2,
                meta_y: 0,
            },
        ],
        layouts: vec![],
        actions: ActionBindings::default_with_sane_keys(),
        rgb: xenwooting::config::RgbConfig::default(),
        control_bar: xenwooting::config::ControlBarConfig::default(),
        hid_overrides: vec![],
    };

    // Provide a placeholder layout pointing to a wtn path you can create.
    cfg.layouts.push(xenwooting::config::LayoutConfig {
        id: "edo12".to_string(),
        name: "12-EDO".to_string(),
        wtn_path: "wtn/edo12.wtn".to_string(),
        edo_divisions: 12,
        pitch_offset: 0,
    });

    let text = toml::to_string_pretty(&cfg).context("serialize default config")?;
    fs::write(path, text).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

fn load_config() -> Result<Config> {
    let path = config_path()?;
    if !path.exists() {
        write_default_config(&path)?;
        println!("Wrote default config: {}", path.display());
    }
    let text = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let cfg: Config = toml::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
    Ok(cfg)
}

fn edo_freq_hz(divisions: i32, pitch_index: i32) -> f64 {
    // freq = C0 * 2^(pitch/divisions)
    C0_HZ * 2f64.powf(pitch_index as f64 / divisions as f64)
}

fn set_mts_table(master: &MtsMaster, layout: &xenwooting::config::LayoutConfig) -> Result<()> {
    master.set_scale_name(&layout.name)?;
    master.enable_all_channels();
    let edo = layout.edo_divisions;

    // Provide a general 128-note tuning table as well, for clients that don't supply MIDI channel.
    {
        let mut freqs = [0.0f64; 128];
        for note in 0..128u8 {
            let pitch = (note as i32) + layout.pitch_offset;
            freqs[note as usize] = edo_freq_hz(edo, pitch);
        }
        master.set_note_tunings(&freqs);
    }

    for ch in 0..16u8 {
        let mut freqs = [0.0f64; 128];
        for note in 0..128u8 {
            let pitch = (ch as i32) * edo + (note as i32) + layout.pitch_offset;
            freqs[note as usize] = edo_freq_hz(edo, pitch);
        }
        master.set_multi_channel_note_tunings(ch, &freqs);
    }
    Ok(())
}

fn compute_rgb_index_by_device_id(
    configured_device_ids: &HashSet<u64>,
    rgb_count: u8,
) -> HashMap<u64, u8> {
    if rgb_count == 0 {
        return HashMap::new();
    }

    // Enumerate keyboard HID devices via sysfs.
    // We rely on HID_UNIQ as the serial string.
    let mut best_by_serial: HashMap<String, (u16, u16, String)> = HashMap::new();
    // serial -> (vid, pid, sort_key)

    let dir = std::path::Path::new("/sys/bus/hid/devices");
    let entries = match std::fs::read_dir(dir) {
        Ok(v) => v,
        Err(_) => return HashMap::new(),
    };

    for e in entries.flatten() {
        let name = e.file_name();
        let name = name.to_string_lossy();
        if !name.contains(":31E3:") {
            continue;
        }
        let uevent_path = e.path().join("uevent");
        let uevent = match std::fs::read_to_string(&uevent_path) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let mut serial: Option<String> = None;
        let mut vid: Option<u16> = None;
        let mut pid: Option<u16> = None;
        let mut phys: Option<String> = None;
        for line in uevent.lines() {
            if let Some(v) = line.strip_prefix("HID_UNIQ=") {
                let s = v.trim();
                if !s.is_empty() {
                    serial = Some(s.to_string());
                }
            } else if let Some(v) = line.strip_prefix("HID_ID=") {
                // Example: 0003:000031E3:00001342
                let parts: Vec<&str> = v.trim().split(':').collect();
                if parts.len() == 3 {
                    // Parse last 4 hex digits (VID/PID are 16-bit).
                    if parts[1].len() >= 4 {
                        vid = u16::from_str_radix(&parts[1][parts[1].len() - 4..], 16).ok();
                    }
                    if parts[2].len() >= 4 {
                        pid = u16::from_str_radix(&parts[2][parts[2].len() - 4..], 16).ok();
                    }
                }
            } else if let Some(v) = line.strip_prefix("HID_PHYS=") {
                let s = v.trim();
                if !s.is_empty() {
                    phys = Some(s.to_string());
                }
            }
        }

        let (Some(serial), Some(vid), Some(pid), Some(phys)) = (serial, vid, pid, phys) else {
            continue;
        };

        // Prefer the smallest phys string for stable ordering.
        match best_by_serial.get(&serial) {
            Some((_v, _p, best_phys)) if best_phys <= &phys => {}
            _ => {
                best_by_serial.insert(serial, (vid, pid, phys));
            }
        }
    }

    // Convert serials into Analog device_ids.
    let mut devices: Vec<(String, u64)> = Vec::new();
    for (serial, (vid, pid, sort_key)) in best_by_serial.iter() {
        let id = analog_device_id_from_serial(serial, *vid, *pid);
        if configured_device_ids.contains(&id) {
            devices.push((sort_key.clone(), id));
        }
    }

    devices.sort_by(|a, b| a.0.cmp(&b.0));
    let mut out: HashMap<u64, u8> = HashMap::new();
    for (idx, (_key, dev_id)) in devices.into_iter().enumerate() {
        if idx >= rgb_count as usize {
            break;
        }
        out.insert(dev_id, idx as u8);
    }
    out
}

fn analog_device_id_from_serial(serial: &str, vendor_id: u16, product_id: u16) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    let mut s = DefaultHasher::new();
    s.write_u16(vendor_id);
    s.write_u16(product_id);
    s.write(serial.as_bytes());
    s.finish()
}

fn compute_compact_col_offsets(hid_map: &HidMap, rotation_deg: u16, mirror_cols: bool) -> [u8; 4] {
    let mut min_col: [u8; 4] = [255, 255, 255, 255];
    for (_hid, loc0) in hid_map.all_locs() {
        let loc =
            match rotate_4x14(loc0, rotation_deg).and_then(|l| mirror_cols_4x14(l, mirror_cols)) {
                Ok(v) => v,
                Err(_) => continue,
            };
        if loc.midi_row < 4 {
            min_col[loc.midi_row as usize] = min_col[loc.midi_row as usize].min(loc.midi_col);
        }
    }
    for r in 0..4 {
        if min_col[r] == 255 {
            min_col[r] = 0;
        }
    }
    min_col
}

fn wtn_index_for_loc(loc: KeyLoc, compact_min_col: &[u8; 4]) -> Option<usize> {
    if loc.midi_row >= 4 || loc.midi_col >= 14 {
        return None;
    }
    let off = compact_min_col[loc.midi_row as usize];
    if loc.midi_col < off {
        return None;
    }
    let col = loc.midi_col - off;
    if col >= 14 {
        return None;
    }
    Some((loc.midi_row as usize) * 14 + (col as usize))
}

fn main() -> Result<()> {
    env_logger::init();

    // Install signal handlers as early as possible.
    install_signal_handlers();

    // Analog SDK init (required before any other SDK call)
    if !sdk::is_initialised() {
        let _ = sdk::initialise().0?;
    }
    let _ = sdk::set_keycode_mode(KeycodeType::HID).0?;

    let cfg = load_config()?;

    let cfg_dir = config_path()?
        .parent()
        .context("config path has no parent")?
        .to_path_buf();

    let resolve_path = |p: &str| {
        let pb = PathBuf::from(p);
        if pb.is_absolute() {
            pb
        } else {
            cfg_dir.join(pb)
        }
    };

    let args: Vec<String> = std::env::args().collect();
    let dump_hid = args.iter().any(|a| a == "--dump-hid");
    let test_midi = args.iter().any(|a| a == "--test-midi");
    let log_midi = args.iter().any(|a| a.starts_with("--log-midi"));
    let log_edges = args.iter().any(|a| a == "--log-edges");
    let log_poll = args.iter().any(|a| a == "--log-poll");
    let log_led = args.iter().any(|a| a == "--log-led");
    let no_rgb = args.iter().any(|a| a == "--no-rgb");
    let no_aftertouch = args.iter().any(|a| a == "--no-aftertouch");
    if args.iter().any(|a| a == "--print-devices") {
        let devices = sdk::get_connected_devices_info(32).0?;
        for d in devices {
            println!(
                "device_id={} name={} manufacturer={}",
                d.device_id, d.device_name, d.manufacturer_name
            );
        }
        return Ok(());
    }

    if dump_hid {
        eprintln!("--dump-hid enabled: printing key transitions only (no MIDI/RGB actions)");
    }
    if log_edges {
        eprintln!("--log-edges enabled: printing key transitions (in addition to normal behavior)");
    }
    if log_poll {
        eprintln!("--log-poll enabled: printing raw SDK poll stats");
    }
    if log_led {
        eprintln!("--log-led enabled: printing HID->LED mapping info");
    }
    if no_rgb {
        eprintln!("--no-rgb enabled: skipping RGB output");
    }
    if no_aftertouch {
        eprintln!("--no-aftertouch enabled: not sending poly aftertouch");
    }

    if test_midi {
        let mut midi_out = AlsaMidiOut::new(&cfg.midi_out_name)?;
        println!("Sending test MIDI on '{}' for 5 seconds", cfg.midi_out_name);
        for i in 0..5u8 {
            let note = 60 + i;
            midi_out.send_note(true, 0, note, 100)?;
            std::thread::sleep(Duration::from_millis(200));
            midi_out.send_note(false, 0, note, 0)?;
            std::thread::sleep(Duration::from_millis(800));
        }
        return Ok(());
    }

    if cfg.layouts.is_empty() {
        anyhow::bail!("No layouts configured. Edit config.toml and add at least one layout.");
    }

    // Layout list is hot-reloaded from config.toml.
    let mut layouts = cfg.layouts.clone();
    layouts.sort_by(|a, b| {
        let (ak, aid) = a.sort_key();
        let (bk, bid) = b.sort_key();
        natural_cmp(ak, bk).then_with(|| natural_cmp(aid, bid))
    });

    let mut hid_map = HidMap::default_60he_ansi_guess();
    if !cfg.hid_overrides.is_empty() {
        let overrides: Vec<(String, u8, u8, u8, u8)> = cfg
            .hid_overrides
            .iter()
            .map(|o| (o.hid.clone(), o.midi_row, o.midi_col, o.led_row, o.led_col))
            .collect();
        hid_map.apply_overrides(&overrides)?;
    }

    let mut actions_by_hid: HashMap<HIDCodes, String> = HashMap::new();
    for (action, hid_name) in cfg.actions.by_action.iter() {
        let hid = parse_hid_name(hid_name)?;
        actions_by_hid.insert(hid, action.clone());
    }

    let control_bar_row = cfg.control_bar.row;
    let mut control_bar_cols_by_hid: HashMap<HIDCodes, Vec<u8>> = HashMap::new();
    for (hid_name, cols) in cfg.control_bar.led_cols_by_hid.iter() {
        let hid = parse_hid_name(hid_name)?;
        control_bar_cols_by_hid.insert(hid, cols.as_vec());
    }

    let highlight_rgb = parse_hex_rgb(&cfg.rgb.highlight_hex).unwrap_or((255, 255, 255));

    // MTS master
    let master = MtsMaster::register(false)?;
    let mut layout_index: usize = 0;
    set_mts_table(&master, &layouts[layout_index])?;

    // Load wtn
    let mut wtn_path = resolve_path(&layouts[layout_index].wtn_path);
    let mut wtn =
        Wtn::load(&wtn_path).with_context(|| format!("Load wtn {}", wtn_path.display()))?;
    let mut wtn_mtime: Option<SystemTime> = fs::metadata(&wtn_path).and_then(|m| m.modified()).ok();
    let mut base_wtn = wtn.clone();
    let mut base_wtn_mtime = wtn_mtime;
    let mut last_wtn_check = Instant::now();

    // Config hot-reload (layouts only).
    let cfg_path = config_path()?;
    let mut cfg_sig: Option<(SystemTime, u64)> = fs::metadata(&cfg_path)
        .ok()
        .and_then(|m| m.modified().ok().map(|t| (t, m.len())));
    let mut last_cfg_check = Instant::now();

    // Webconfigurator preview mode (temporary mapping without saving).
    let preview_enabled_path = PathBuf::from("/tmp/xenwooting-preview.enabled");
    let preview_wtn_path = PathBuf::from("/tmp/xenwooting-preview.wtn");
    let highlight_path = PathBuf::from("/tmp/xenwooting-highlight.txt");
    let mut preview_enabled = false;
    let mut preview_layout_id: Option<String> = None;
    let mut preview_wtn_mtime: Option<SystemTime> = None;

    // Manual highlight state (from the web UI).
    let mut manual_highlight: Option<(u8, u8, u8, (u8, u8, u8))> = None;
    let mut highlight_mtime: Option<SystemTime> = None;

    // MIDI out
    let mut midi_out = AlsaMidiOut::new(&cfg.midi_out_name)?;
    info!("ALSA MIDI out ready: {}", cfg.midi_out_name);

    // Cache: base MIDI channels used by each wtn_board in the current mapping.
    let compute_used_channels = |wtn: &Wtn, board: u8| -> HashSet<u8> {
        let mut s: HashSet<u8> = HashSet::new();
        for i in 0..56usize {
            if let Some(cell) = wtn.cell(board, i) {
                s.insert(cell.chan_1based.saturating_sub(1));
            }
        }
        s
    };
    let mut used_ch_by_board: HashMap<u8, HashSet<u8>> = HashMap::new();
    used_ch_by_board.insert(0, compute_used_channels(&wtn, 0));
    used_ch_by_board.insert(1, compute_used_channels(&wtn, 1));

    // Per-device last sent damper (CC64) value (0..127). Used to reduce CC spam.
    let mut last_damper_cc64_by_device: HashMap<u64, u8> = HashMap::new();

    let rgb_enabled = cfg.rgb.enabled && !no_rgb;
    let (rgb_tx, rgb_rx) = crossbeam_channel::bounded::<RgbCmd>(1024);
    if rgb_enabled {
        spawn_rgb_worker(rgb_rx);
    }

    // Device -> board config selection
    let devices = loop {
        let devs = sdk::get_connected_devices_info(32).0?;
        if !devs.is_empty() {
            break devs;
        }
        eprintln!("No Analog devices detected; waiting...");
        thread::sleep(Duration::from_millis(1000));
    };

    // Map device_id -> BoardConfig
    let mut board_by_device: HashMap<u64, xenwooting::config::BoardConfig> = HashMap::new();
    let mut unassigned_cfg: Vec<xenwooting::config::BoardConfig> = cfg.boards.clone();
    // First assign explicit device_id configs
    unassigned_cfg.retain(|bcfg| {
        if let Ok(Some(id)) = bcfg.device_id_u64() {
            board_by_device.insert(id, bcfg.clone());
            false
        } else {
            true
        }
    });
    // Assign remaining configs by discovery order
    let mut cfg_iter = unassigned_cfg.into_iter();
    for d in &devices {
        if board_by_device.contains_key(&d.device_id) {
            continue;
        }
        if let Some(bcfg) = cfg_iter.next() {
            board_by_device.insert(d.device_id, bcfg);
        }
    }

    // IDs we are willing to poll / paint for.
    let configured_device_ids: HashSet<u64> = board_by_device.keys().copied().collect();

    // Per-device compaction offsets (per midi_row) so .wtn indices are "left-justified"
    // after applying rotation/mirroring. This makes Key_0 mean "first key of the first row"
    // from the user's perspective even on rotated boards.
    let mut compact_min_col_by_device: HashMap<u64, [u8; 4]> = HashMap::new();
    for (dev_id, bcfg) in board_by_device.iter() {
        compact_min_col_by_device.insert(
            *dev_id,
            compute_compact_col_offsets(&hid_map, bcfg.rotation_deg, bcfg.mirror_cols),
        );
    }

    // Robust mapping from Analog device_id -> RGB SDK device_index.
    //
    // The Analog SDK's DeviceID is derived from (vid, pid, serial). We can recover that same ID
    // from Linux sysfs (HID_UNIQ) and then map to the RGB SDK's current device indices. This keeps
    // LED routing stable across unplug/replug and independent of discovery order.
    let mut rgb_index_by_device_id: HashMap<u64, u8> = HashMap::new();

    let mut octave_shift: i8 = 0; // persistent shifts MIDI channel index
                                  // Momentary +1 channel (Space), per physical keyboard.
                                  // This makes Board0 Space affect only Board0 notes (same for Board1).
    let mut octave_hold_by_device: HashSet<u64> = HashSet::new();

    let velocity_profiles: Vec<VelocityProfile> = vec![
        VelocityProfile::Linear,
        VelocityProfile::Gamma { gamma: 1.6 },
        VelocityProfile::Gamma { gamma: 0.7 },
        VelocityProfile::Log { k: 12.0 },
        VelocityProfile::InvLog { k: 12.0 },
    ];
    let mut velocity_profile_idx: usize = 0;

    let mut aftertouch_mode = if no_aftertouch {
        AftertouchMode::Off
    } else {
        AftertouchMode::PeakMapped
    };

    // Global run-flag is controlled by signal handler.
    RUNNING.store(true, Ordering::SeqCst);

    let poll_period = Duration::from_secs_f32(1.0 / cfg.refresh_hz.max(1.0));

    // Poll the SDK from a single thread.
    // In practice this is more reliable than polling the SDK concurrently.
    let (tx, rx) = mpsc::channel::<KeyEdge>();
    // We'll refresh connected device IDs dynamically to support hotplug.
    let press_threshold_bits = Arc::new(std::sync::atomic::AtomicU32::new(
        cfg.press_threshold.to_bits(),
    ));
    let update_delta = cfg.aftertouch_delta.clamp(0.001, 0.2);
    let verbose = dump_hid || log_edges || log_midi || log_poll;
    let press_threshold_bits_poll = Arc::clone(&press_threshold_bits);
    let configured_device_ids_poll = configured_device_ids.clone();
    std::thread::spawn(move || {
        let mut down_by_device: HashMap<u64, HashSet<HIDCodes>> = HashMap::new();
        let mut last_analog_by_device: HashMap<u64, HashMap<HIDCodes, f32>> = HashMap::new();
        let mut poll_ids: Vec<u64> = Vec::new();
        let mut last_dev_refresh = std::time::Instant::now() - Duration::from_secs(999);
        let mut last_report = std::time::Instant::now() - Duration::from_secs(999);
        let mut err_count: u64 = 0;

        while RUNNING.load(Ordering::SeqCst) {
            let _ = sdk::set_keycode_mode(KeycodeType::HID);

            // Refresh connected devices at a slower cadence than the poll loop.
            if last_dev_refresh.elapsed() >= Duration::from_millis(500) {
                last_dev_refresh = std::time::Instant::now();
                match sdk::get_connected_devices_info(32).0 {
                    Ok(devs) => {
                        let mut new_ids: Vec<u64> = devs
                            .iter()
                            .filter(|d| configured_device_ids_poll.contains(&d.device_id))
                            .map(|d| d.device_id)
                            .collect();
                        new_ids.sort_unstable();
                        if new_ids != poll_ids {
                            if verbose {
                                eprintln!("poll_ids updated: {:?}", new_ids);
                            }
                            // Add new device maps.
                            for id in &new_ids {
                                down_by_device.entry(*id).or_insert_with(HashSet::new);
                                last_analog_by_device
                                    .entry(*id)
                                    .or_insert_with(HashMap::new);
                            }
                            // Drop removed device maps.
                            down_by_device.retain(|id, _| new_ids.contains(id));
                            last_analog_by_device.retain(|id, _| new_ids.contains(id));
                            poll_ids = new_ids;
                        }
                    }
                    Err(e) => {
                        if verbose {
                            eprintln!("get_connected_devices_info error={:?}", e);
                        }
                    }
                }
            }

            for device_id in &poll_ids {
                let data = match sdk::read_full_buffer_device(256, *device_id).0 {
                    Ok(v) => v,
                    Err(e) => {
                        err_count += 1;
                        if verbose && last_report.elapsed() > Duration::from_secs(1) {
                            eprintln!(
                                "poll device_id={} error={:?} err_count={} ",
                                device_id, e, err_count
                            );
                            last_report = std::time::Instant::now();
                        }
                        continue;
                    }
                };

                if log_poll && last_report.elapsed() > Duration::from_secs(1) {
                    let mut items: Vec<(u16, f32)> = data.iter().map(|(k, v)| (*k, *v)).collect();
                    items
                        .sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
                    let preview: Vec<String> = items
                        .iter()
                        .take(6)
                        .map(|(k, v)| format!("{}:{:.3}", k, v))
                        .collect();
                    eprintln!(
                        "poll device_id={} items={} top=[{}]",
                        device_id,
                        data.len(),
                        preview.join(",")
                    );
                    last_report = std::time::Instant::now();
                }

                let down = down_by_device.get_mut(device_id).unwrap();
                let last_analog = last_analog_by_device.get_mut(device_id).unwrap();
                for (code_u16, analog) in data.iter() {
                    if *code_u16 > 255 {
                        continue;
                    }
                    let Some(hid) = HIDCodes::from_u8(*code_u16 as u8) else {
                        continue;
                    };
                    let press_threshold = f32::from_bits(
                        press_threshold_bits_poll.load(std::sync::atomic::Ordering::Relaxed),
                    )
                    .clamp(0.0, 0.99);

                    let is_down_now = *analog > press_threshold;
                    let was_down = down.contains(&hid);
                    if is_down_now && !was_down {
                        down.insert(hid.clone());
                        last_analog.insert(hid.clone(), *analog);
                        if verbose {
                            eprintln!(
                                "send DOWN device_id={} hid={:?} analog={:.3}",
                                device_id, hid, analog
                            );
                        }
                        if tx
                            .send(KeyEdge::Down {
                                device_id: *device_id,
                                hid,
                                analog: *analog,
                                ts: Instant::now(),
                            })
                            .is_err()
                        {
                            eprintln!("poll thread: channel closed");
                            return;
                        }
                    } else if !is_down_now && was_down {
                        down.remove(&hid);
                        last_analog.remove(&hid);
                        if verbose {
                            eprintln!(
                                "send UP   device_id={} hid={:?} analog={:.3}",
                                device_id, hid, analog
                            );
                        }
                        if tx
                            .send(KeyEdge::Up {
                                device_id: *device_id,
                                hid,
                                analog: *analog,
                                ts: Instant::now(),
                            })
                            .is_err()
                        {
                            eprintln!("poll thread: channel closed");
                            return;
                        }
                    } else if is_down_now && was_down {
                        let prev = last_analog.get(&hid).copied().unwrap_or(0.0);
                        // Send updates while a key is held so peak tracking and aftertouch
                        // keep working even when the analog value doesn't change much.
                        //
                        // To keep bandwidth reasonable, only update the stored prev value
                        // when a meaningful analog delta occurred.
                        if (*analog - prev).abs() >= update_delta {
                            last_analog.insert(hid.clone(), *analog);
                        }
                        let _ = tx.send(KeyEdge::Update {
                            device_id: *device_id,
                            hid,
                            analog: *analog,
                            ts: Instant::now(),
                        });
                    }
                }
            }
            std::thread::sleep(poll_period);
        }
    });

    // Track hotplug of RGB devices; repaint base when device count changes.
    let mut last_rgb_count: u8 = if rgb_enabled { Rgb::device_count() } else { 0 };
    let mut last_rgb_check = Instant::now();

    // Track hotplug of analog devices; repaint base when configured boards appear/disappear.
    let mut last_connected_cfg_ids: HashSet<u64> = HashSet::new();
    let mut last_cfg_dev_check = Instant::now();

    let rebuild_rgb_index_map = |rgb_count: u8| -> HashMap<u64, u8> {
        if !rgb_enabled {
            return HashMap::new();
        }
        let mut m = compute_rgb_index_by_device_id(&configured_device_ids, rgb_count);
        if m.is_empty() {
            // Fallback to config mapping (legacy).
            for (dev_id, bcfg) in board_by_device.iter() {
                m.insert(
                    *dev_id,
                    cfg.rgb.rgb_device_index_for_wtn_board(bcfg.wtn_board),
                );
            }
        }
        m
    };

    rgb_index_by_device_id = rebuild_rgb_index_map(last_rgb_count);
    info!("RGB device_id->rgb_index map: {:?}", rgb_index_by_device_id);

    let paint_base = |wtn: &Wtn, paint_control_bar: bool, rgb_map: &HashMap<u64, u8>| {
        if !rgb_enabled {
            return;
        }
        if log_edges || log_poll || log_midi {
            eprintln!("paint_base: start");
        }
        for (dev_id, bcfg) in board_by_device.iter() {
            let wtn_board = bcfg.wtn_board;
            let Some(&dev_idx) = rgb_map.get(dev_id) else {
                continue;
            };

            // Paint base LEDs using the HID->KeyLoc map.
            //
            // This is critical because ANSI wide keys create holes in the LED column grid.
            // Highlighting uses KeyLoc.led_row/led_col; base paint must use the same mapping
            // or rows will appear shifted.
            let compact_min_col = compact_min_col_by_device
                .get(dev_id)
                .unwrap_or(&[0u8, 0u8, 0u8, 0u8]);

            for (_hid, loc0) in hid_map.all_locs().into_iter() {
                // Important: `wtn` is in *logical* orientation. If the board is rotated/mirrored,
                // map physical KeyLoc (midi_row/midi_col) to its logical lookup index.
                let loc = match rotate_4x14(loc0, bcfg.rotation_deg)
                    .and_then(|l| mirror_cols_4x14(l, bcfg.mirror_cols))
                {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                let Some(idx) = wtn_index_for_loc(loc, compact_min_col) else {
                    continue;
                };
                if let Some(cell) = wtn.cell(wtn_board, idx) {
                    try_send_drop(
                        &rgb_tx,
                        RgbCmd::SetKey(RgbKey {
                            device_index: dev_idx,
                            row: loc.led_row,
                            col: loc.led_col,
                            rgb: cell.col_rgb,
                        }),
                    );
                }
            }
            info!("Queued base LEDs for wtn_board {}", wtn_board);

            // Paint reserved control bar as red.
            // Note: layout changes repaint the 4x14 grid only; we intentionally do NOT repaint
            // the control bar during layout switching so action flashes are not overridden.
            if paint_control_bar {
                let red = (255u8, 0u8, 0u8);
                for c in 0..14u8 {
                    try_send_drop(
                        &rgb_tx,
                        RgbCmd::SetKey(RgbKey {
                            device_index: dev_idx,
                            row: control_bar_row,
                            col: c,
                            rgb: red,
                        }),
                    );
                }
            }
        }
        if log_edges || log_poll || log_midi {
            eprintln!("paint_base: done");
        }
    };

    paint_base(&wtn, true, &rgb_index_by_device_id);

    if log_edges || log_poll || log_midi {
        eprintln!("ready: waiting for key edges");
    }

    // Peak-tracked velocity state per key (device_id + HID).
    let mut vel_state: HashMap<(u64, HIDCodes), VelState> = HashMap::new();

    // RGB screensaver: blank LEDs after inactivity; wake on next key-down.
    // The wake key-down is ignored (no MIDI, no action binding).
    let mut screensaver_active = false;
    let mut last_activity = Instant::now();
    let mut pressed_keys: HashSet<(u64, HIDCodes)> = HashSet::new();
    let mut suppressed_keys: HashSet<(u64, HIDCodes)> = HashSet::new();

    let paint_off = |rgb_map: &HashMap<u64, u8>| {
        if !rgb_enabled {
            return;
        }
        let off = (0u8, 0u8, 0u8);
        for (dev_id, _bcfg) in board_by_device.iter() {
            let Some(&dev_idx) = rgb_map.get(dev_id) else {
                continue;
            };
            for (_hid, loc0) in hid_map.all_locs().into_iter() {
                try_send_drop(
                    &rgb_tx,
                    RgbCmd::SetKey(RgbKey {
                        device_index: dev_idx,
                        row: loc0.led_row,
                        col: loc0.led_col,
                        rgb: off,
                    }),
                );
            }
            // Also blank the reserved/control bar row.
            for c in 0..14u8 {
                try_send_drop(
                    &rgb_tx,
                    RgbCmd::SetKey(RgbKey {
                        device_index: dev_idx,
                        row: control_bar_row,
                        col: c,
                        rgb: off,
                    }),
                );
            }
        }
    };

    let refresh_vel_led_bases = |wtn: &Wtn, vel_state: &mut HashMap<(u64, HIDCodes), VelState>| {
        for ((device_id, hid), st) in vel_state.iter_mut() {
            let Some(bcfg) = board_by_device.get(device_id) else {
                continue;
            };
            let rotation = bcfg.rotation_deg;
            let compact_min_col = compact_min_col_by_device
                .get(device_id)
                .unwrap_or(&[0u8, 0u8, 0u8, 0u8]);

            let Some(loc0) = hid_map.loc_for(hid.clone()) else {
                continue;
            };
            let loc = match rotate_4x14(loc0, rotation)
                .and_then(|l| mirror_cols_4x14(l, bcfg.mirror_cols))
            {
                Ok(v) => v,
                Err(_) => continue,
            };
            let Some(idx) = wtn_index_for_loc(loc, compact_min_col) else {
                continue;
            };
            let Some(cell) = wtn.cell(bcfg.wtn_board, idx) else {
                continue;
            };

            match st {
                VelState::Tracking { led: Some(led), .. } => led.base_rgb = cell.col_rgb,
                VelState::Aftershock { led: Some(led), .. } => led.base_rgb = cell.col_rgb,
                _ => {}
            }
        }
    };

    while RUNNING.load(Ordering::SeqCst) {
        // If a keyboard is plugged/unplugged, RGB device indices become available/change.
        // Repaint the base LEDs so newly connected boards immediately show the current layout.
        if rgb_enabled && last_rgb_check.elapsed() >= Duration::from_millis(250) {
            last_rgb_check = Instant::now();
            let now = Rgb::device_count();
            if now != last_rgb_count {
                last_rgb_count = now;
                rgb_index_by_device_id = rebuild_rgb_index_map(last_rgb_count);
                info!("RGB device_id->rgb_index map: {:?}", rgb_index_by_device_id);
                paint_base(&wtn, true, &rgb_index_by_device_id);
                info!("RGB device count changed; repainted base ({})", now);
            }
        }

        if last_cfg_dev_check.elapsed() >= Duration::from_millis(500) {
            last_cfg_dev_check = Instant::now();
            if let Ok(devs) = sdk::get_connected_devices_info(32).0 {
                let now: HashSet<u64> = devs
                    .iter()
                    .map(|d| d.device_id)
                    .filter(|id| configured_device_ids.contains(id))
                    .collect();
                if now != last_connected_cfg_ids {
                    last_connected_cfg_ids = now;
                    rgb_index_by_device_id = rebuild_rgb_index_map(last_rgb_count);
                    info!("RGB device_id->rgb_index map: {:?}", rgb_index_by_device_id);
                    paint_base(&wtn, true, &rgb_index_by_device_id);
                    info!("Configured device set changed; repainted base");
                }
            }
        }

        // Reload layout list when config.toml changes (no service restart).
        if last_cfg_check.elapsed() >= Duration::from_millis(500) {
            last_cfg_check = Instant::now();
            if let Ok(meta) = fs::metadata(&cfg_path) {
                let modified = meta.modified().ok();
                let sig_now = modified.map(|t| (t, meta.len()));
                let changed = sig_now.is_some() && sig_now != cfg_sig;
                if changed {
                    cfg_sig = sig_now;
                    let cur_id = layouts.get(layout_index).map(|l| l.id.clone());
                    match load_config() {
                        Ok(new_cfg) => {
                            if new_cfg.layouts.is_empty() {
                                eprintln!(
                                    "config reload: layouts empty; keeping previous layout list"
                                );
                            } else if {
                                let mut sorted = new_cfg.layouts.clone();
                                sorted.sort_by(|a, b| {
                                    let (ak, aid) = a.sort_key();
                                    let (bk, bid) = b.sort_key();
                                    natural_cmp(ak, bk).then_with(|| natural_cmp(aid, bid))
                                });
                                sorted != layouts
                            } {
                                let mut new_layouts = new_cfg.layouts;
                                new_layouts.sort_by(|a, b| {
                                    let (ak, aid) = a.sort_key();
                                    let (bk, bid) = b.sort_key();
                                    natural_cmp(ak, bk).then_with(|| natural_cmp(aid, bid))
                                });

                                // Keep current layout id if possible.
                                let mut new_index: usize = 0;
                                if let Some(id) = cur_id {
                                    if let Some(i) = new_layouts.iter().position(|l| l.id == id) {
                                        new_index = i;
                                    }
                                }
                                if new_index >= new_layouts.len() {
                                    new_index = 0;
                                }

                                // If preview is enabled and a layout id is pinned in the preview file,
                                // force the preview switching logic to re-run with the new layout list.
                                if preview_enabled {
                                    layouts = new_layouts;
                                    layout_index = new_index;
                                    preview_layout_id = None;
                                    preview_wtn_mtime = None;
                                    info!("Reloaded config layouts ({} layouts)", layouts.len());
                                } else {
                                    // Apply changes without crashing the daemon.
                                    let mut ok = true;

                                    if let Err(e) = set_mts_table(&master, &new_layouts[new_index])
                                    {
                                        eprintln!("config reload: set_mts_table failed: {e}");
                                        ok = false;
                                    }

                                    let new_wtn_path =
                                        resolve_path(&new_layouts[new_index].wtn_path);
                                    let new_base = match Wtn::load(&new_wtn_path) {
                                        Ok(v) => v,
                                        Err(e) => {
                                            eprintln!(
                                                "config reload: load wtn failed ({}): {e}",
                                                new_wtn_path.display()
                                            );
                                            ok = false;
                                            base_wtn.clone()
                                        }
                                    };

                                    if ok {
                                        layouts = new_layouts;
                                        layout_index = new_index;
                                        wtn_path = new_wtn_path;
                                        base_wtn = new_base;
                                        base_wtn_mtime =
                                            fs::metadata(&wtn_path).and_then(|m| m.modified()).ok();
                                        wtn_mtime = base_wtn_mtime;
                                        wtn = base_wtn.clone();
                                        used_ch_by_board.insert(0, compute_used_channels(&wtn, 0));
                                        used_ch_by_board.insert(1, compute_used_channels(&wtn, 1));
                                        refresh_vel_led_bases(&wtn, &mut vel_state);
                                        paint_base(&wtn, false, &rgb_index_by_device_id);
                                        info!(
                                            "Reloaded config layouts ({} layouts)",
                                            layouts.len()
                                        );
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            eprintln!("config reload failed ({}): {e}", cfg_path.display());
                        }
                    }
                }
            }
        }
        // Preview mode and .wtn reloading.
        //
        // - Base (on-disk) wtn is always reloaded into `base_wtn`.
        // - If preview is enabled, `wtn` is loaded from /tmp/xenwooting-preview.wtn.
        // - Leaving preview restores `wtn = base_wtn`.
        if last_wtn_check.elapsed() >= Duration::from_millis(200) {
            last_wtn_check = Instant::now();

            // Refresh base wtn if the on-disk file changed.
            if let Ok(modified) = fs::metadata(&wtn_path).and_then(|m| m.modified()) {
                let changed = match base_wtn_mtime {
                    Some(prev) => modified != prev,
                    None => true,
                };
                if changed {
                    match Wtn::load(&wtn_path) {
                        Ok(new_wtn) => {
                            base_wtn = new_wtn;
                            base_wtn_mtime = Some(modified);
                            if !preview_enabled {
                                wtn = base_wtn.clone();
                                used_ch_by_board.insert(0, compute_used_channels(&wtn, 0));
                                used_ch_by_board.insert(1, compute_used_channels(&wtn, 1));
                                wtn_mtime = base_wtn_mtime;
                                refresh_vel_led_bases(&wtn, &mut vel_state);
                                paint_base(&wtn, false, &rgb_index_by_device_id);
                                info!("Reloaded wtn from disk: {}", wtn_path.display());
                            } else {
                                info!("Reloaded base wtn (preview active): {}", wtn_path.display());
                            }
                        }
                        Err(e) => {
                            eprintln!("wtn reload failed ({}): {e}", wtn_path.display());
                        }
                    }
                }
            }

            // Preview enable/disable + (optional) layout switching.
            let enabled_now = preview_enabled_path.exists();
            let layout_now: Option<String> = if enabled_now {
                fs::read_to_string(&preview_enabled_path)
                    .ok()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
            } else {
                None
            };

            if enabled_now != preview_enabled || layout_now != preview_layout_id {
                preview_enabled = enabled_now;
                preview_layout_id = layout_now.clone();

                if preview_enabled {
                    if let Some(id) = layout_now {
                        if let Some(i) = layouts.iter().position(|l| l.id == id) {
                            if i != layout_index {
                                layout_index = i;
                                eprintln!(
                                    "preview: switching layout -> {}",
                                    layouts[layout_index].id
                                );
                                set_mts_table(&master, &layouts[layout_index])?;
                                wtn_path = resolve_path(&layouts[layout_index].wtn_path);
                                base_wtn = Wtn::load(&wtn_path)
                                    .with_context(|| format!("Load wtn {}", wtn_path.display()))?;
                                base_wtn_mtime =
                                    fs::metadata(&wtn_path).and_then(|m| m.modified()).ok();
                                wtn_mtime = base_wtn_mtime;
                            }
                        }
                    }
                    // Force a reload of preview file.
                    preview_wtn_mtime = None;
                    info!("preview: enabled");
                } else {
                    // Restore base config.
                    wtn = base_wtn.clone();
                    used_ch_by_board.insert(0, compute_used_channels(&wtn, 0));
                    used_ch_by_board.insert(1, compute_used_channels(&wtn, 1));
                    wtn_mtime = base_wtn_mtime;
                    preview_wtn_mtime = None;
                    refresh_vel_led_bases(&wtn, &mut vel_state);
                    paint_base(&wtn, true, &rgb_index_by_device_id);
                    info!("preview: disabled");
                }
            }

            // If preview enabled, reload preview wtn on change.
            if preview_enabled {
                if let Ok(modified) = fs::metadata(&preview_wtn_path).and_then(|m| m.modified()) {
                    let changed = match preview_wtn_mtime {
                        Some(prev) => modified != prev,
                        None => true,
                    };
                    if changed {
                        match Wtn::load(&preview_wtn_path) {
                            Ok(new_wtn) => {
                                wtn = new_wtn;
                                used_ch_by_board.insert(0, compute_used_channels(&wtn, 0));
                                used_ch_by_board.insert(1, compute_used_channels(&wtn, 1));
                                preview_wtn_mtime = Some(modified);
                                refresh_vel_led_bases(&wtn, &mut vel_state);
                                paint_base(&wtn, false, &rgb_index_by_device_id);
                                info!("preview: reloaded wtn {}", preview_wtn_path.display());
                            }
                            Err(e) => {
                                eprintln!(
                                    "preview wtn reload failed ({}): {e}",
                                    preview_wtn_path.display()
                                );
                            }
                        }
                    }
                }
            }

            // Manual highlight via web UI.
            if let Ok(modified) = fs::metadata(&highlight_path).and_then(|m| m.modified()) {
                let changed = match highlight_mtime {
                    Some(prev) => modified != prev,
                    None => true,
                };
                if changed {
                    highlight_mtime = Some(modified);
                    if let Ok(text) = fs::read_to_string(&highlight_path) {
                        let mut board: Option<u8> = None;
                        let mut idx: Option<usize> = None;
                        let mut down: Option<bool> = None;
                        for line in text.lines() {
                            if let Some(v) = line.strip_prefix("board=") {
                                board = v.trim().parse::<u8>().ok();
                            } else if let Some(v) = line.strip_prefix("idx=") {
                                idx = v.trim().parse::<usize>().ok();
                            } else if let Some(v) = line.strip_prefix("down=") {
                                down =
                                    Some(v.trim() == "1" || v.trim().eq_ignore_ascii_case("true"));
                            }
                        }

                        // Restore previous highlight if needed.
                        if let Some((dev_idx, row, col, base_rgb)) = manual_highlight.take() {
                            let _ = try_send_drop(
                                &rgb_tx,
                                RgbCmd::SetKey(RgbKey {
                                    device_index: dev_idx,
                                    row,
                                    col,
                                    rgb: base_rgb,
                                }),
                            );
                        }

                        if down.unwrap_or(false) {
                            if let (Some(b), Some(i)) = (board, idx) {
                                // Find device_id for this wtn_board.
                                let mut target_dev: Option<u64> = None;
                                for (dev_id, bcfg) in board_by_device.iter() {
                                    if bcfg.wtn_board == b {
                                        target_dev = Some(*dev_id);
                                        break;
                                    }
                                }
                                if let Some(device_id) = target_dev {
                                    let Some(&dev_idx) = rgb_index_by_device_id.get(&device_id)
                                    else {
                                        continue;
                                    };
                                    let bcfg = board_by_device.get(&device_id).unwrap();
                                    let compact_min_col = compact_min_col_by_device
                                        .get(&device_id)
                                        .unwrap_or(&[0u8, 0u8, 0u8, 0u8]);

                                    // Invert mapping: find the physical LED location whose wtn index matches.
                                    let mut led_rc: Option<(u8, u8)> = None;
                                    for (_hid, loc0) in hid_map.all_locs().into_iter() {
                                        let loc = match rotate_4x14(loc0, bcfg.rotation_deg)
                                            .and_then(|l| mirror_cols_4x14(l, bcfg.mirror_cols))
                                        {
                                            Ok(v) => v,
                                            Err(_) => continue,
                                        };
                                        let Some(widx) = wtn_index_for_loc(loc, compact_min_col)
                                        else {
                                            continue;
                                        };
                                        if widx == i {
                                            led_rc = Some((loc.led_row, loc.led_col));
                                            break;
                                        }
                                    }

                                    if let Some((row, col)) = led_rc {
                                        // Compute base color from current active mapping.
                                        let base_rgb =
                                            wtn.cell(b, i).map(|c| c.col_rgb).unwrap_or((0, 0, 0));
                                        let _ = try_send_drop(
                                            &rgb_tx,
                                            RgbCmd::SetKey(RgbKey {
                                                device_index: dev_idx,
                                                row,
                                                col,
                                                rgb: highlight_rgb,
                                            }),
                                        );
                                        manual_highlight = Some((dev_idx, row, col, base_rgb));
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Timer tick: fire delayed NoteOn / NoteOff.
        {
            let peak_track = Duration::from_millis(cfg.velocity_peak_track_ms.max(1) as u64);
            let aftershock = Duration::from_millis(cfg.aftershock_ms.max(1) as u64);
            let threshold =
                f32::from_bits(press_threshold_bits.load(Ordering::Relaxed)).clamp(0.0, 0.99);
            let max_swing = cfg.velocity_max_swing.clamp(0.05, 1.0);

            let keys: Vec<(u64, HIDCodes)> = vel_state.keys().cloned().collect();
            for key in keys {
                let Some(st) = vel_state.get(&key).cloned() else {
                    continue;
                };
                match st {
                    VelState::Tracking {
                        started,
                        peak,
                        out_ch,
                        note,
                        playing,
                        last_val: _,
                        led,
                    } => {
                        if started.elapsed() >= peak_track {
                            let denom = (max_swing - threshold).max(0.001);
                            let n = ((peak - threshold) / denom).clamp(0.0, 1.0);
                            let n2 = velocity_profiles[velocity_profile_idx].apply(n);
                            let vel = (n2 * 126.0).round().clamp(0.0, 126.0) as u8 + 1;

                            if !playing {
                                let _ = midi_out.send_note(true, out_ch, note, vel);
                                // After firing, go to Aftershock state (like the Teensy state
                                // machine). While the key is still held, the next Update will
                                // return it to Tracking again, and repeated windows will emit
                                // aftertouch values.
                                vel_state.insert(
                                    key,
                                    VelState::Aftershock {
                                        quiet_since: None,
                                        out_ch,
                                        note,
                                        playing: true,
                                        last_val: vel,
                                        led,
                                    },
                                );
                            } else {
                                if matches!(aftertouch_mode, AftertouchMode::PeakMapped)
                                    && !no_aftertouch
                                {
                                    let _ = midi_out.send_polytouch(out_ch, note, vel);
                                }
                                vel_state.insert(
                                    key,
                                    VelState::Aftershock {
                                        quiet_since: None,
                                        out_ch,
                                        note,
                                        playing,
                                        last_val: vel,
                                        led,
                                    },
                                );
                            }
                        }
                    }
                    VelState::Aftershock {
                        quiet_since,
                        out_ch,
                        note,
                        playing,
                        last_val,
                        led,
                    } => {
                        let Some(qs) = quiet_since else {
                            continue;
                        };
                        if qs.elapsed() >= aftershock {
                            if playing {
                                // Use the last computed peak-mapped value as release velocity.
                                // This is simple, works well with Pianoteq, and mirrors the
                                // Teensy behavior where aftertouch uses the same mapped value.
                                let _ = midi_out.send_note(false, out_ch, note, last_val);
                            }
                            if let Some(led) = led {
                                let _ = try_send_drop(
                                    &rgb_tx,
                                    RgbCmd::SetKey(RgbKey {
                                        device_index: led.dev_idx,
                                        row: led.row,
                                        col: led.col,
                                        rgb: led.base_rgb,
                                    }),
                                );
                            }
                            vel_state.remove(&key);
                        }
                    }
                }
            }
        }

        let edge = match rx.recv_timeout(Duration::from_millis(10)) {
            Ok(e) => Some(e),
            Err(mpsc::RecvTimeoutError::Timeout) => None,
            Err(_) => break,
        };

        if edge.is_none() {
            // Idle tick: possibly activate screensaver.
            if rgb_enabled
                && !screensaver_active
                && pressed_keys.is_empty()
                && cfg.rgb.screensaver_timeout_sec > 0
                && last_activity.elapsed()
                    >= Duration::from_secs(cfg.rgb.screensaver_timeout_sec as u64)
            {
                info!(
                    "RGB screensaver: blank after {}s idle",
                    cfg.rgb.screensaver_timeout_sec
                );
                paint_off(&rgb_index_by_device_id);
                screensaver_active = true;
            }
            continue;
        }

        let edge = edge.unwrap();

        let (device_id, hid, analog, kind, ts) = match edge {
            KeyEdge::Down {
                device_id,
                hid,
                analog,
                ts,
            } => (device_id, hid, analog, "down", ts),
            KeyEdge::Update {
                device_id,
                hid,
                analog,
                ts,
            } => (device_id, hid, analog, "update", ts),
            KeyEdge::Up {
                device_id,
                hid,
                analog,
                ts,
            } => (device_id, hid, analog, "up", ts),
        };

        let key_id = (device_id, hid.clone());

        // Count only down/up as inactivity-resetting activity.
        // Update edges can be noisy due to analog jitter and should not prevent blanking.
        if kind != "update" {
            last_activity = Instant::now();
        }

        // Maintain pressed-key tracking even when suppressed.
        if kind == "down" {
            pressed_keys.insert(key_id.clone());
        } else if kind == "up" {
            pressed_keys.remove(&key_id);
        }

        // Wake behavior: first key-down after blanking wakes the LEDs but is ignored.
        if screensaver_active && kind == "down" {
            // Restore base LEDs immediately.
            info!("RGB screensaver: wake");
            paint_base(&wtn, true, &rgb_index_by_device_id);
            screensaver_active = false;
            suppressed_keys.insert(key_id.clone());
            continue;
        }

        // Suppressed keys do nothing until released.
        if suppressed_keys.contains(&key_id) {
            if kind == "up" {
                suppressed_keys.remove(&key_id);
            }
            continue;
        }

        let Some(bcfg) = board_by_device.get(&device_id) else {
            continue;
        };
        let wtn_board = bcfg.wtn_board;
        let rotation = bcfg.rotation_deg;

        let is_control_bar = control_bar_cols_by_hid.contains_key(&hid);

        // Runtime press threshold can be adjusted with control-bar keys.
        // These keys never generate notes.
        if kind == "down" {
            match hid {
                HIDCodes::LeftCtrl => {
                    let mut t = f32::from_bits(press_threshold_bits.load(Ordering::Relaxed));
                    t = (t - cfg.press_threshold_step).clamp(0.02, 0.98);
                    press_threshold_bits.store(t.to_bits(), Ordering::Relaxed);
                    info!("press_threshold now {:.3}", t);
                    continue;
                }
                HIDCodes::RightCtrl => {
                    let mut t = f32::from_bits(press_threshold_bits.load(Ordering::Relaxed));
                    t = (t + cfg.press_threshold_step).clamp(0.02, 0.98);
                    press_threshold_bits.store(t.to_bits(), Ordering::Relaxed);
                    info!("press_threshold now {:.3}", t);
                    continue;
                }
                HIDCodes::LeftAlt => {
                    velocity_profile_idx = (velocity_profile_idx + 1) % velocity_profiles.len();
                    info!(
                        "velocity_profile now {}",
                        velocity_profiles[velocity_profile_idx].name()
                    );
                    continue;
                }
                HIDCodes::RightAlt => {
                    aftertouch_mode = match aftertouch_mode {
                        AftertouchMode::PeakMapped => AftertouchMode::Off,
                        AftertouchMode::Off => AftertouchMode::PeakMapped,
                    };
                    info!("aftertouch_mode now {}", aftertouch_mode.name());
                    continue;
                }
                HIDCodes::Space => {
                    octave_hold_by_device.insert(device_id);
                    info!("octave_hold on (device_id={})", device_id);
                    continue;
                }
                _ => {}
            }
        } else if kind == "up" {
            if let HIDCodes::Space = hid {
                octave_hold_by_device.remove(&device_id);
                info!("octave_hold off (device_id={})", device_id);
                continue;
            }
        }

        // Compute debug LED coordinates (either from control bar mapping or hid_map).
        let dev_idx_dbg = rgb_index_by_device_id
            .get(&device_id)
            .copied()
            .unwrap_or(255);
        let mut lrow_dbg: Option<u8> = None;
        let mut lcol_dbg: Option<String> = None;
        if is_control_bar {
            if let Some(cs) = control_bar_cols_by_hid.get(&hid) {
                lrow_dbg = Some(control_bar_row);
                lcol_dbg = Some(
                    cs.iter()
                        .map(|c| c.to_string())
                        .collect::<Vec<String>>()
                        .join(","),
                );
            }
        } else if let Some(loc0) = hid_map.loc_for(hid.clone()) {
            if let Ok(loc) =
                rotate_4x14(loc0, rotation).and_then(|l| mirror_cols_4x14(l, bcfg.mirror_cols))
            {
                lrow_dbg = Some(loc.led_row);
                lcol_dbg = Some(loc.led_col.to_string());
            }
        }

        if dump_hid || log_edges {
            let (lr_s, lc_s) = match (lrow_dbg, lcol_dbg.as_deref()) {
                (Some(lr), Some(lc)) => (lr.to_string(), lc.to_string()),
                _ => ("<none>".to_string(), "<none>".to_string()),
            };
            if kind == "down" {
                println!(
                    "DOWN device_id={} hid={:?} analog={:.3} lRow={} lCol={} dev_idx={}",
                    device_id, hid, analog, lr_s, lc_s, dev_idx_dbg
                );
            } else if kind == "up" {
                println!(
                    "UP   device_id={} hid={:?} analog={:.3} lRow={} lCol={} dev_idx={}",
                    device_id, hid, analog, lr_s, lc_s, dev_idx_dbg
                );
            } else if log_edges {
                // keep updates quiet unless explicitly requested
                println!(
                    "UPD  device_id={} hid={:?} analog={:.3} lRow={} lCol={} dev_idx={}",
                    device_id, hid, analog, lr_s, lc_s, dev_idx_dbg
                );
            }
            if dump_hid {
                continue;
            }
        }

        // Control bar LED feedback (independent of whether a key is bound to an action).
        if rgb_enabled && is_control_bar {
            let Some(&dev_idx) = rgb_index_by_device_id.get(&device_id) else {
                continue;
            };
            if let Some(cols) = control_bar_cols_by_hid.get(&hid) {
                if kind == "down" {
                    for &lc in cols {
                        try_send_drop(
                            &rgb_tx,
                            RgbCmd::SetKey(RgbKey {
                                device_index: dev_idx,
                                row: control_bar_row,
                                col: lc,
                                rgb: highlight_rgb,
                            }),
                        );
                    }
                } else if kind == "up" {
                    for &lc in cols {
                        try_send_drop(
                            &rgb_tx,
                            RgbCmd::SetKey(RgbKey {
                                device_index: dev_idx,
                                row: control_bar_row,
                                col: lc,
                                rgb: (255, 0, 0),
                            }),
                        );
                    }
                }
            }
        }

        // Damper pedal (CC64): supports analog values (0..127) while held.
        // This is treated as an "action" but must run on down/update/up.
        if let Some(action) = actions_by_hid.get(&hid) {
            if action.as_str() == "damper" {
                // Determine which wtn board this physical key belongs to.
                let Some(bcfg) = board_by_device.get(&device_id) else {
                    continue;
                };
                let wtn_board = bcfg.wtn_board;

                // Determine which MIDI channels are used by this board in the current mapping.
                let base_chs: Vec<u8> = used_ch_by_board
                    .get(&wtn_board)
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .collect();

                // Apply current octave shift and per-device octave hold so CC targets the same
                // channels the board is currently outputting notes on.
                let hold = if octave_hold_by_device.contains(&device_id) {
                    1i16
                } else {
                    0i16
                };
                let mut out_chs: HashSet<u8> = HashSet::new();
                for base in base_chs {
                    let shifted = (base as i16) + (octave_shift as i16) + hold;
                    out_chs.insert(shifted.clamp(0, 15) as u8);
                }

                // Map analog (0..1) to CC value (0..127). Rescale so press_threshold == 0.
                let press_threshold =
                    f32::from_bits(press_threshold_bits.load(Ordering::Relaxed)).clamp(0.0, 0.99);
                let value: u8 = if kind == "up" {
                    0
                } else {
                    let norm =
                        ((analog - press_threshold) / (1.0 - press_threshold)).clamp(0.0, 1.0);
                    (norm * 127.0).round() as u8
                };

                // Rate limit by remembering the last sent value per device.
                let last = last_damper_cc64_by_device
                    .get(&device_id)
                    .copied()
                    .unwrap_or(255);
                if value != last {
                    for ch in out_chs.iter().copied() {
                        let _ = midi_out.send_cc(ch, 64, value);
                    }
                    last_damper_cc64_by_device.insert(device_id, value);
                }

                if kind == "up" {
                    last_damper_cc64_by_device.remove(&device_id);
                }
                continue;
            }
        }

        if kind == "down" {
            if let Some(action) = actions_by_hid.get(&hid) {
                match action.as_str() {
                    "layout_next" => {
                        layout_index = (layout_index + 1) % layouts.len();
                        eprintln!("layout_next -> {}", layouts[layout_index].id);
                        set_mts_table(&master, &layouts[layout_index])?;
                        wtn_path = resolve_path(&layouts[layout_index].wtn_path);
                        eprintln!("loading wtn: {}", wtn_path.display());
                        base_wtn = Wtn::load(&wtn_path)
                            .with_context(|| format!("Load wtn {}", wtn_path.display()))?;
                        base_wtn_mtime = fs::metadata(&wtn_path).and_then(|m| m.modified()).ok();
                        wtn_mtime = base_wtn_mtime;
                        if !preview_enabled {
                            wtn = base_wtn.clone();
                            used_ch_by_board.insert(0, compute_used_channels(&wtn, 0));
                            used_ch_by_board.insert(1, compute_used_channels(&wtn, 1));
                            refresh_vel_led_bases(&wtn, &mut vel_state);
                            paint_base(&wtn, false, &rgb_index_by_device_id);
                        }
                    }
                    "layout_prev" => {
                        if layout_index == 0 {
                            layout_index = layouts.len() - 1;
                        } else {
                            layout_index -= 1;
                        }
                        eprintln!("layout_prev -> {}", layouts[layout_index].id);
                        set_mts_table(&master, &layouts[layout_index])?;
                        wtn_path = resolve_path(&layouts[layout_index].wtn_path);
                        eprintln!("loading wtn: {}", wtn_path.display());
                        base_wtn = Wtn::load(&wtn_path)
                            .with_context(|| format!("Load wtn {}", wtn_path.display()))?;
                        base_wtn_mtime = fs::metadata(&wtn_path).and_then(|m| m.modified()).ok();
                        wtn_mtime = base_wtn_mtime;
                        if !preview_enabled {
                            wtn = base_wtn.clone();
                            used_ch_by_board.insert(0, compute_used_channels(&wtn, 0));
                            used_ch_by_board.insert(1, compute_used_channels(&wtn, 1));
                            refresh_vel_led_bases(&wtn, &mut vel_state);
                            paint_base(&wtn, false, &rgb_index_by_device_id);
                        }
                    }
                    "octave_up" => {
                        octave_shift = (octave_shift + 1).min(15);
                        info!("octave_shift now {}", octave_shift);
                    }
                    "octave_down" => {
                        octave_shift = (octave_shift - 1).max(-15);
                        info!("octave_shift now {}", octave_shift);
                    }
                    _ => {}
                }
                continue;
            }
        }

        // Control bar keys never produce MIDI notes.
        if is_control_bar {
            continue;
        }

        if let Some(loc) = hid_map.loc_for(hid.clone()) {
            let loc = mirror_cols_4x14(rotate_4x14(loc, rotation)?, bcfg.mirror_cols)?;
            let compact_min_col = compact_min_col_by_device
                .get(&device_id)
                .unwrap_or(&[0u8, 0u8, 0u8, 0u8]);
            let Some(idx) = wtn_index_for_loc(loc, compact_min_col) else {
                continue;
            };
            if let Some(cell) = wtn.cell(wtn_board, idx) {
                let base_ch = cell.chan_1based.saturating_sub(1);
                let hold = if octave_hold_by_device.contains(&device_id) {
                    1i16
                } else {
                    0i16
                };
                let shifted = (base_ch as i16) + (octave_shift as i16) + hold;
                let out_ch: u8 = shifted.clamp(0, 15) as u8;
                let note = cell.key;

                let press_threshold =
                    f32::from_bits(press_threshold_bits.load(Ordering::Relaxed)).clamp(0.0, 0.99);

                // Maintain per-key velocity state.
                if kind == "down" {
                    let (already_playing, prev_last_val) = match vel_state.get(&key_id) {
                        Some(VelState::Tracking {
                            playing, last_val, ..
                        }) => (*playing, *last_val),
                        Some(VelState::Aftershock {
                            playing, last_val, ..
                        }) => (*playing, *last_val),
                        None => (false, 0),
                    };

                    let led = if rgb_enabled {
                        if let Some(&dev_idx) = rgb_index_by_device_id.get(&device_id) {
                            let _ = try_send_drop(
                                &rgb_tx,
                                RgbCmd::SetKey(RgbKey {
                                    device_index: dev_idx,
                                    row: loc.led_row,
                                    col: loc.led_col,
                                    rgb: highlight_rgb,
                                }),
                            );
                            Some(LedState {
                                dev_idx,
                                row: loc.led_row,
                                col: loc.led_col,
                                base_rgb: cell.col_rgb,
                            })
                        } else {
                            None
                        }
                    } else {
                        None
                    };

                    vel_state.insert(
                        key_id,
                        VelState::Tracking {
                            started: ts,
                            peak: analog,
                            out_ch,
                            note,
                            playing: already_playing,
                            last_val: prev_last_val,
                            led,
                        },
                    );
                } else if kind == "update" {
                    if let Some(st) = vel_state.get_mut(&key_id) {
                        match st {
                            VelState::Tracking { peak, .. } => {
                                if analog > *peak {
                                    *peak = analog;
                                }
                            }
                            VelState::Aftershock {
                                out_ch,
                                note,
                                playing,
                                last_val,
                                led,
                                ..
                            } => {
                                // If still held, transition back to Tracking (Teensy state 2 -> 1).
                                if analog > press_threshold {
                                    let already_playing = *playing;
                                    let out_ch = *out_ch;
                                    let note = *note;
                                    let last_val = *last_val;
                                    let led = led.clone();
                                    *st = VelState::Tracking {
                                        started: ts,
                                        peak: analog,
                                        out_ch,
                                        note,
                                        playing: already_playing,
                                        last_val,
                                        led,
                                    };
                                }
                            }
                        }
                    }
                } else if kind == "up" {
                    match vel_state.remove(&key_id) {
                        Some(VelState::Tracking {
                            out_ch,
                            note,
                            playing,
                            last_val,
                            led,
                            ..
                        }) => {
                            if playing {
                                vel_state.insert(
                                    key_id,
                                    VelState::Aftershock {
                                        quiet_since: Some(ts),
                                        out_ch,
                                        note,
                                        playing,
                                        last_val,
                                        led,
                                    },
                                );
                            } else {
                                // Released before NoteOn; restore LED immediately.
                                if let Some(led) = led {
                                    let _ = try_send_drop(
                                        &rgb_tx,
                                        RgbCmd::SetKey(RgbKey {
                                            device_index: led.dev_idx,
                                            row: led.row,
                                            col: led.col,
                                            rgb: led.base_rgb,
                                        }),
                                    );
                                }
                            }
                        }
                        Some(VelState::Aftershock {
                            quiet_since: _,
                            out_ch,
                            note,
                            playing,
                            last_val,
                            led,
                        }) => {
                            // Release: start (or restart) the aftershock timer.
                            vel_state.insert(
                                key_id,
                                VelState::Aftershock {
                                    quiet_since: Some(ts),
                                    out_ch,
                                    note,
                                    playing,
                                    last_val,
                                    led,
                                },
                            );
                        }
                        None => {
                            // Not tracked.
                            if rgb_enabled {
                                if let Some(&dev_idx) = rgb_index_by_device_id.get(&device_id) {
                                    let _ = try_send_drop(
                                        &rgb_tx,
                                        RgbCmd::SetKey(RgbKey {
                                            device_index: dev_idx,
                                            row: loc.led_row,
                                            col: loc.led_col,
                                            rgb: cell.col_rgb,
                                        }),
                                    );
                                }
                            }
                        }
                    }
                }
            } else if log_midi {
                println!(
                    "NO_CELL  dev={} wtn_board={} hid={:?} idx={} (wtn missing?)",
                    device_id, wtn_board, hid, idx
                );
            }
        } else if log_midi {
            println!(
                "UNMAPPED dev={} wtn_board={} hid={:?}",
                device_id, wtn_board, hid
            );
        }
    }

    info!("xenwooting exiting");
    Ok(())
}
