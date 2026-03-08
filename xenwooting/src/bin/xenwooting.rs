use alsa::seq::{EvNote, Event, EventType, PortCap, PortType, Seq};
use alsa::Direction;
use anyhow::{Context, Result};
use log::info;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::ffi::CString;
use std::fs;
use std::hash::Hash;
use std::hash::Hasher;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
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

const LIVE_STATE_PATH: &str = "/tmp/xenwooting-live.json";

#[derive(Debug, Clone, Serialize)]
struct LiveLayout {
    id: String,
    name: String,
    edo: i32,
    pitch_offset: i32,
}

#[derive(Debug, Clone, Serialize)]
struct LiveMode {
    press_threshold: f32,
    aftertouch: String,
    aftertouch_speed_max: f32,
    octave_shift: i8,
    screensaver_active: bool,
    preview_enabled: bool,
    guide_mode: String,
    guide_chord_key: Option<String>,
    guide_root_pc: Option<i32>,
}

#[derive(Debug, Clone, Serialize)]
struct LiveState {
    version: u8,
    seq: u64,
    ts_ms: u64,
    layout: LiveLayout,
    mode: LiveMode,
    pressed: HashMap<String, Vec<i32>>,
    layout_pitches: HashMap<String, Vec<Option<i32>>>,
}

fn write_file_atomic(path: &Path, text: &str) -> Result<()> {
    let tmp = PathBuf::from(format!(
        "{}.tmp-{}-{}",
        path.display(),
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
    ));
    {
        let mut f = fs::File::create(&tmp).with_context(|| format!("create {}", tmp.display()))?;
        f.write_all(text.as_bytes())
            .with_context(|| format!("write {}", tmp.display()))?;
        let _ = f.sync_all();
    }
    fs::rename(&tmp, path)
        .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

fn dim_rgb(rgb: (u8, u8, u8), factor: f32) -> (u8, u8, u8) {
    let f = factor.clamp(0.0, 1.0);
    let r = (rgb.0 as f32 * f).round().clamp(0.0, 255.0) as u8;
    let g = (rgb.1 as f32 * f).round().clamp(0.0, 255.0) as u8;
    let b = (rgb.2 as f32 * f).round().clamp(0.0, 255.0) as u8;
    (r, g, b)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GuideMode {
    Off,
    WaitRoot,
    Active,
}

fn guide_idle_rgb(
    base: (u8, u8, u8),
    note: u8,
    edo: i32,
    pitch_offset: i32,
    guide_mode: GuideMode,
    pcs_abs: &HashSet<i32>,
    dim: f32,
) -> (u8, u8, u8) {
    if guide_mode == GuideMode::Off {
        return base;
    }
    if edo <= 0 {
        return dim_rgb(base, dim);
    }
    let mut pc = (note as i32) + pitch_offset;
    pc %= edo;
    if pc < 0 {
        pc += edo;
    }
    if guide_mode == GuideMode::Active && pcs_abs.contains(&pc) {
        base
    } else {
        dim_rgb(base, dim)
    }
}

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
        // Speed tracking for speed-mapped aftertouch.
        last_analog: f32,
        last_analog_ts: Instant,
        peak_speed: f32,
        at_level: f32,
        at_last_ts: Instant,
        out_ch: u8,
        note: u8,
        playing: bool,
        last_val: u8,
        led: Option<LedState>,
    },
    Aftershock {
        quiet_since: Option<Instant>,
        at_level: f32,
        at_last_ts: Instant,
        out_ch: u8,
        note: u8,
        playing: bool,
        last_val: u8,
        led: Option<LedState>,
    },
}

#[derive(Debug, Clone)]
enum AftertouchMode {
    SpeedMapped,
    PeakMapped,
    Off,
}

impl AftertouchMode {
    fn name(&self) -> &'static str {
        match self {
            AftertouchMode::SpeedMapped => "speed-mapped",
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
        aftertouch_speed_max: 74.0,
        aftertouch_speed_step: 2.0,
        aftertouch_speed_attack_ms: 12,
        aftertouch_speed_decay_ms: 250,
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
    let trainer_mode_path = PathBuf::from("/tmp/xenwooting-trainer.mode");
    let guide_path = PathBuf::from("/tmp/xenwooting-guide.json");
    let highlight_path = PathBuf::from("/tmp/xenwooting-highlight.txt");
    let mut preview_enabled = false;
    let mut preview_layout_id: Option<String> = None;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum TrainerMode {
        Off,
        Wait,
        Active,
    }
    let mut trainer_mode = TrainerMode::Off;

    #[derive(Debug, Clone, Deserialize)]
    struct GuideCfg {
        enabled: bool,
        layout_id: Option<String>,
        chord_key: Option<String>,
        pcs_root: Option<Vec<i32>>,
        dim: Option<f32>,
    }

    let mut guide_mode = GuideMode::Off;
    let mut guide_layout_id: Option<String> = None;
    let mut guide_chord_key: Option<String> = None;
    let mut guide_pcs_root: Vec<i32> = vec![];
    let mut guide_pcs_abs: HashSet<i32> = HashSet::new();
    let mut guide_root_pc: Option<i32> = None;
    let mut guide_dim: f32 = 0.20;
    let mut guide_last_sig: u64 = 0;
    let mut guide_meta_sig: Option<(SystemTime, u64)> = None;
    let mut preview_wtn_mtime: Option<SystemTime> = None;

    // Manual highlight state (from the web UI).
    let mut manual_highlight: Option<(u8, u8, u8, (u8, u8, u8))> = None;
    let mut highlight_mtime: Option<SystemTime> = None;

    // MIDI out
    let mut midi_out = AlsaMidiOut::new(&cfg.midi_out_name)?;
    info!("ALSA MIDI out ready: {}", cfg.midi_out_name);

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
        AftertouchMode::SpeedMapped
    };

    // When aftertouch is enabled, use a fixed low note-on threshold so expressive control works
    // even with very light presses.
    const AFTERTOUCH_PRESS_THRESHOLD: f32 = 0.02;
    let mut manual_press_threshold: f32 = cfg.press_threshold;

    let mut aftertouch_speed_max: f32 = cfg.aftertouch_speed_max.clamp(1.0, 200.0);

    // Global run-flag is controlled by signal handler.
    RUNNING.store(true, Ordering::SeqCst);

    let poll_period = Duration::from_secs_f32(1.0 / cfg.refresh_hz.max(1.0));

    // Poll the SDK from a single thread.
    // In practice this is more reliable than polling the SDK concurrently.
    let (tx, rx) = mpsc::channel::<KeyEdge>();
    // We'll refresh connected device IDs dynamically to support hotplug.
    let threshold_init = if matches!(aftertouch_mode, AftertouchMode::Off) {
        manual_press_threshold
    } else {
        AFTERTOUCH_PRESS_THRESHOLD
    };
    let press_threshold_bits =
        Arc::new(std::sync::atomic::AtomicU32::new(threshold_init.to_bits()));
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

    let control_bar_rgb_for_tm = |tm: TrainerMode| -> (u8, u8, u8) {
        match tm {
            TrainerMode::Wait => (0u8, 0u8, 0u8),
            TrainerMode::Active => (0u8, 255u8, 0u8),
            TrainerMode::Off => (255u8, 0u8, 0u8),
        }
    };

    // Paint the control-bar LEDs corresponding to the Space key.
    // When octave-hold is enabled, these LEDs are forced to white.
    let paint_spacebar_indicator =
        |device_id: u64, rgb_map: &HashMap<u64, u8>, base_rgb: (u8, u8, u8), hold: bool| {
            if !rgb_enabled {
                return;
            }
            let Some(&dev_idx) = rgb_map.get(&device_id) else {
                return;
            };
            let Some(cols) = control_bar_cols_by_hid.get(&HIDCodes::Space) else {
                return;
            };
            let rgb = if hold {
                (255u8, 255u8, 255u8)
            } else {
                base_rgb
            };
            for &c in cols.iter() {
                try_send_drop(
                    &rgb_tx,
                    RgbCmd::SetKey(RgbKey {
                        device_index: dev_idx,
                        row: control_bar_row,
                        col: c,
                        rgb,
                    }),
                );
            }
        };

    let paint_base = |wtn: &Wtn,
                      paint_control_bar: bool,
                      rgb_map: &HashMap<u64, u8>,
                      tm: TrainerMode,
                      octave_hold_by_device: &HashSet<u64>| {
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

            // Paint reserved control bar.
            // Note: layout changes repaint the 4x14 grid only; we intentionally do NOT repaint
            // the control bar during layout switching so action flashes are not overridden.
            if paint_control_bar {
                let rgb = control_bar_rgb_for_tm(tm);
                for c in 0..14u8 {
                    try_send_drop(
                        &rgb_tx,
                        RgbCmd::SetKey(RgbKey {
                            device_index: dev_idx,
                            row: control_bar_row,
                            col: c,
                            rgb,
                        }),
                    );
                }

                // Space octave-hold indicator overrides the base bar color.
                paint_spacebar_indicator(
                    *dev_id,
                    rgb_map,
                    rgb,
                    octave_hold_by_device.contains(dev_id),
                );
            }
        }
        if log_edges || log_poll || log_midi {
            eprintln!("paint_base: done");
        }
    };

    let paint_control_bar = |rgb_map: &HashMap<u64, u8>, rgb: (u8, u8, u8)| {
        if !rgb_enabled {
            return;
        }
        for (dev_id, _bcfg) in board_by_device.iter() {
            let Some(&dev_idx) = rgb_map.get(dev_id) else {
                continue;
            };
            for c in 0..14u8 {
                try_send_drop(
                    &rgb_tx,
                    RgbCmd::SetKey(RgbKey {
                        device_index: dev_idx,
                        row: control_bar_row,
                        col: c,
                        rgb,
                    }),
                );
            }
        }
    };

    let paint_guide = |wtn: &Wtn,
                       rgb_map: &HashMap<u64, u8>,
                       edo: i32,
                       pitch_offset: i32,
                       mode: GuideMode,
                       pcs_root: &Vec<i32>,
                       root_pc: Option<i32>,
                       dim: f32,
                       octave_hold_by_device: &HashSet<u64>| {
        if !rgb_enabled {
            return;
        }
        if mode == GuideMode::Off {
            return;
        }
        // edo/pitch_offset passed in to avoid borrowing layouts/layout_index.

        let mut pcs_abs: HashSet<i32> = HashSet::new();
        if mode == GuideMode::Active {
            if let Some(rpc) = root_pc {
                for pc in pcs_root.iter() {
                    let mut x = (pc + rpc) % edo;
                    if x < 0 {
                        x += edo;
                    }
                    pcs_abs.insert(x);
                }
            }
        }

        let mut lit_root = 0u32;
        let mut lit_total = 0u32;

        for (dev_id, bcfg) in board_by_device.iter() {
            let wtn_board = bcfg.wtn_board;
            let Some(&dev_idx) = rgb_map.get(dev_id) else {
                continue;
            };
            let compact_min_col = compact_min_col_by_device
                .get(dev_id)
                .unwrap_or(&[0u8, 0u8, 0u8, 0u8]);

            for (_hid, loc0) in hid_map.all_locs().into_iter() {
                let loc = match rotate_4x14(loc0, bcfg.rotation_deg)
                    .and_then(|l| mirror_cols_4x14(l, bcfg.mirror_cols))
                {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                let Some(idx) = wtn_index_for_loc(loc, compact_min_col) else {
                    continue;
                };
                let Some(cell) = wtn.cell(wtn_board, idx) else {
                    continue;
                };

                let mut pc = (cell.key as i32) + pitch_offset;
                pc %= edo;
                if pc < 0 {
                    pc += edo;
                }

                let on = mode == GuideMode::Active && pcs_abs.contains(&pc);
                let rgb = if on {
                    lit_total += 1;
                    if root_pc == Some(pc) {
                        lit_root += 1;
                    }
                    cell.col_rgb
                } else {
                    dim_rgb(cell.col_rgb, dim)
                };

                try_send_drop(
                    &rgb_tx,
                    RgbCmd::SetKey(RgbKey {
                        device_index: dev_idx,
                        row: loc.led_row,
                        col: loc.led_col,
                        rgb,
                    }),
                );
            }

            let bar = match mode {
                GuideMode::WaitRoot => (0u8, 0u8, 0u8),
                GuideMode::Active => (0u8, 255u8, 0u8),
                GuideMode::Off => (255u8, 0u8, 0u8),
            };
            for c in 0..14u8 {
                try_send_drop(
                    &rgb_tx,
                    RgbCmd::SetKey(RgbKey {
                        device_index: dev_idx,
                        row: control_bar_row,
                        col: c,
                        rgb: bar,
                    }),
                );
            }

            // Space octave-hold indicator overrides the base bar color.
            paint_spacebar_indicator(
                *dev_id,
                rgb_map,
                bar,
                octave_hold_by_device.contains(dev_id),
            );
        }

        if mode == GuideMode::Active {
            info!(
                "guide: painted active (root_pc={:?}) lit_total={} lit_root={}",
                root_pc, lit_total, lit_root
            );
        }
    };

    paint_base(
        &wtn,
        true,
        &rgb_index_by_device_id,
        trainer_mode,
        &octave_hold_by_device,
    );

    if log_edges || log_poll || log_midi {
        eprintln!("ready: waiting for key edges");
    }

    // Peak-tracked velocity state per key (device_id + HID).
    let mut vel_state: HashMap<(u64, HIDCodes), VelState> = HashMap::new();

    // Live HUD state published for /wtn/live.
    let live_state_path = PathBuf::from(LIVE_STATE_PATH);
    let mut live_seq: u64 = 0;
    let mut live_last_ts_ms: u64 = 0;
    let mut live_last_publish = Instant::now();
    let mut live_last_hash: u64 = 0;
    let mut live_layout_key: String = String::new();
    let mut live_layout_pitches: HashMap<String, Vec<Option<i32>>> = HashMap::new();
    let mut live_layout_pitches_hash: u64 = 0;

    let compute_layout_pitches = |wtn: &Wtn,
                                  edo: i32,
                                  pitch_offset: i32,
                                  octave_shift: i8|
     -> HashMap<String, Vec<Option<i32>>> {
        let mut out: HashMap<String, Vec<Option<i32>>> = HashMap::new();
        for board in 0..=1u8 {
            let mut pitches: Vec<Option<i32>> = Vec::with_capacity(56);
            for idx in 0..56usize {
                let Some(cell) = wtn.cell(board, idx) else {
                    pitches.push(None);
                    continue;
                };
                let base_ch = (cell.chan_1based.saturating_sub(1)) as i16;
                let shifted = base_ch + (octave_shift as i16);
                let out_ch = shifted.clamp(0, 15) as i32;
                let pitch = out_ch * edo + (cell.key as i32) + pitch_offset;
                pitches.push(Some(pitch));
            }
            out.insert(format!("Board{}", board), pitches);
        }
        out
    };

    let mut publish_live =
        |wtn: &Wtn,
         layouts: &Vec<xenwooting::config::LayoutConfig>,
         layout_index: usize,
         press_threshold_bits: &AtomicU32,
         aftertouch_mode: &AftertouchMode,
         aftertouch_speed_max: f32,
         octave_shift: i8,
         screensaver_active: bool,
         preview_enabled: bool,
         guide_mode: GuideMode,
         guide_chord_key: &Option<String>,
         guide_root_pc: Option<i32>,
         board_by_device: &HashMap<u64, xenwooting::config::BoardConfig>,
         vel_state: &HashMap<(u64, HIDCodes), VelState>| {
            let layout = match layouts.get(layout_index) {
                Some(v) => v,
                None => return,
            };
            let edo = layout.edo_divisions;
            let pitch_offset = layout.pitch_offset;

            // Recompute layout pitch list only when layout/shift changes.
            let cur_layout_key = format!(
                "{}:{}:{}:{}",
                layout.id, layout.edo_divisions, layout.pitch_offset, octave_shift
            );
            if cur_layout_key != live_layout_key {
                live_layout_key = cur_layout_key;
                live_layout_pitches = compute_layout_pitches(wtn, edo, pitch_offset, octave_shift);
                let mut hh = std::collections::hash_map::DefaultHasher::new();
                for k in ["Board0", "Board1"] {
                    hh.write(k.as_bytes());
                    if let Some(v) = live_layout_pitches.get(k) {
                        for x in v.iter() {
                            match x {
                                Some(n) => hh.write_i32(*n),
                                None => hh.write_i32(i32::MIN),
                            }
                        }
                    }
                }
                live_layout_pitches_hash = hh.finish();
            }

            let mut pressed0: HashSet<i32> = HashSet::new();
            let mut pressed1: HashSet<i32> = HashSet::new();
            for ((device_id, _hid), st) in vel_state.iter() {
                let Some(bcfg) = board_by_device.get(device_id) else {
                    continue;
                };
                let (out_ch, note, is_pressed) = match st {
                    VelState::Tracking { out_ch, note, .. } => (*out_ch, *note, true),
                    VelState::Aftershock {
                        quiet_since,
                        out_ch,
                        note,
                        ..
                    } => (*out_ch, *note, quiet_since.is_none()),
                };
                if !is_pressed {
                    continue;
                }
                let pitch = (out_ch as i32) * edo + (note as i32) + pitch_offset;
                if bcfg.wtn_board == 0 {
                    pressed0.insert(pitch);
                } else if bcfg.wtn_board == 1 {
                    pressed1.insert(pitch);
                }
            }

            let mut v0: Vec<i32> = pressed0.into_iter().collect();
            let mut v1: Vec<i32> = pressed1.into_iter().collect();
            v0.sort();
            v1.sort();

            let mut pressed: HashMap<String, Vec<i32>> = HashMap::new();
            pressed.insert("Board0".to_string(), v0);
            pressed.insert("Board1".to_string(), v1);

            let press_threshold = f32::from_bits(press_threshold_bits.load(Ordering::Relaxed));

            // Hash only the meaningful content; ts_ms/seq should not cause spurious updates.
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            hasher.write(live_layout_key.as_bytes());
            hasher.write_u64(live_layout_pitches_hash);
            hasher.write_u32(press_threshold.to_bits());
            hasher.write(aftertouch_mode.name().as_bytes());
            hasher.write_u32(aftertouch_speed_max.to_bits());
            hasher.write_i8(octave_shift);
            hasher.write_u8(if screensaver_active { 1 } else { 0 });
            hasher.write_u8(if preview_enabled { 1 } else { 0 });
            hasher.write_u8(match guide_mode {
                GuideMode::Off => 0,
                GuideMode::WaitRoot => 1,
                GuideMode::Active => 2,
            });
            if let Some(k) = guide_chord_key {
                hasher.write(k.as_bytes());
            }
            hasher.write_i32(guide_root_pc.unwrap_or(i32::MIN));
            for p in pressed.get("Board0").into_iter().flatten() {
                hasher.write_i32(*p);
            }
            hasher.write_u8(0);
            for p in pressed.get("Board1").into_iter().flatten() {
                hasher.write_i32(*p);
            }
            let h = hasher.finish();
            if h == live_last_hash {
                return;
            }

            // Don't write too frequently if something is oscillating.
            if live_last_publish.elapsed() < Duration::from_millis(15) {
                return;
            }
            live_last_publish = Instant::now();
            live_last_hash = h;
            live_seq = live_seq.wrapping_add(1);
            live_last_ts_ms = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;

            let state = LiveState {
                version: 1,
                seq: live_seq,
                ts_ms: live_last_ts_ms,
                layout: LiveLayout {
                    id: layout.id.clone(),
                    name: layout.name.clone(),
                    edo,
                    pitch_offset,
                },
                mode: LiveMode {
                    press_threshold,
                    aftertouch: aftertouch_mode.name().to_string(),
                    aftertouch_speed_max,
                    octave_shift,
                    screensaver_active,
                    preview_enabled,
                    guide_mode: match guide_mode {
                        GuideMode::Off => "off".to_string(),
                        GuideMode::WaitRoot => "wait_root".to_string(),
                        GuideMode::Active => "active".to_string(),
                    },
                    guide_chord_key: guide_chord_key.clone(),
                    guide_root_pc,
                },
                pressed,
                layout_pitches: live_layout_pitches.clone(),
            };

            let text = match serde_json::to_string(&state) {
                Ok(t) => t,
                Err(_) => return,
            };
            let _ = write_file_atomic(&live_state_path, &text);
        };

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

    let refresh_vel_led_bases = |wtn: &Wtn,
                                 vel_state: &mut HashMap<(u64, HIDCodes), VelState>,
                                 edo: i32,
                                 pitch_offset: i32,
                                 guide_mode: GuideMode,
                                 guide_pcs_abs: &HashSet<i32>,
                                 guide_dim: f32| {
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

            let idle = guide_idle_rgb(
                cell.col_rgb,
                cell.key,
                edo,
                pitch_offset,
                guide_mode,
                guide_pcs_abs,
                guide_dim,
            );

            match st {
                VelState::Tracking { led: Some(led), .. } => led.base_rgb = idle,
                VelState::Aftershock { led: Some(led), .. } => led.base_rgb = idle,
                _ => {}
            }
        }
    };

    // Publish an initial live state so /wtn/live can load immediately.
    publish_live(
        &wtn,
        &layouts,
        layout_index,
        press_threshold_bits.as_ref(),
        &aftertouch_mode,
        aftertouch_speed_max,
        octave_shift,
        screensaver_active,
        preview_enabled,
        guide_mode,
        &guide_chord_key,
        guide_root_pc,
        &board_by_device,
        &vel_state,
    );

    while RUNNING.load(Ordering::SeqCst) {
        publish_live(
            &wtn,
            &layouts,
            layout_index,
            press_threshold_bits.as_ref(),
            &aftertouch_mode,
            aftertouch_speed_max,
            octave_shift,
            screensaver_active,
            preview_enabled,
            guide_mode,
            &guide_chord_key,
            guide_root_pc,
            &board_by_device,
            &vel_state,
        );
        // If a keyboard is plugged/unplugged, RGB device indices become available/change.
        // Repaint the base LEDs so newly connected boards immediately show the current layout.
        if rgb_enabled && last_rgb_check.elapsed() >= Duration::from_millis(250) {
            last_rgb_check = Instant::now();
            let now = Rgb::device_count();
            if now != last_rgb_count {
                last_rgb_count = now;
                rgb_index_by_device_id = rebuild_rgb_index_map(last_rgb_count);
                info!("RGB device_id->rgb_index map: {:?}", rgb_index_by_device_id);
                paint_base(
                    &wtn,
                    true,
                    &rgb_index_by_device_id,
                    trainer_mode,
                    &octave_hold_by_device,
                );
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
                    paint_base(
                        &wtn,
                        true,
                        &rgb_index_by_device_id,
                        trainer_mode,
                        &octave_hold_by_device,
                    );
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
                                        let edo = layouts[layout_index].edo_divisions;
                                        let pitch_offset = layouts[layout_index].pitch_offset;
                                        refresh_vel_led_bases(
                                            &wtn,
                                            &mut vel_state,
                                            edo,
                                            pitch_offset,
                                            guide_mode,
                                            &guide_pcs_abs,
                                            guide_dim,
                                        );
                                        paint_base(
                                            &wtn,
                                            false,
                                            &rgb_index_by_device_id,
                                            trainer_mode,
                                            &octave_hold_by_device,
                                        );
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

            // Guide mode config (xenwooting-owned chord trainer).
            let meta_sig_now: Option<(SystemTime, u64)> = fs::metadata(&guide_path)
                .ok()
                .and_then(|m| m.modified().ok().map(|t| (t, m.len())));
            if meta_sig_now != guide_meta_sig {
                guide_meta_sig = meta_sig_now;

                let guide_now: Option<GuideCfg> = fs::read_to_string(&guide_path)
                    .ok()
                    .and_then(|t| serde_json::from_str::<GuideCfg>(&t).ok());
                let mut gh = std::collections::hash_map::DefaultHasher::new();
                if let Some(g) = &guide_now {
                    g.enabled.hash(&mut gh);
                    if let Some(id) = &g.layout_id {
                        id.hash(&mut gh);
                    }
                    if let Some(k) = &g.chord_key {
                        k.hash(&mut gh);
                    }
                    if let Some(p) = &g.pcs_root {
                        for x in p.iter() {
                            x.hash(&mut gh);
                        }
                    }
                    ((g.dim.unwrap_or(0.20) * 1000.0) as i32).hash(&mut gh);
                }
                let sig = gh.finish();
                if sig != guide_last_sig {
                    guide_last_sig = sig;

                    if let Some(g) = guide_now {
                        if g.enabled {
                            guide_layout_id = g.layout_id.clone();
                            guide_chord_key = g.chord_key.clone();
                            guide_pcs_root = g.pcs_root.unwrap_or_default();
                            guide_dim = g.dim.unwrap_or(0.20).clamp(0.0, 1.0);
                            guide_root_pc = None;
                            guide_pcs_abs.clear();

                            // Disable preview if active (guide owns LED output).
                            if preview_enabled {
                                let _ = fs::remove_file(&preview_enabled_path);
                                let _ = fs::remove_file(&preview_wtn_path);
                                let _ = fs::remove_file(&trainer_mode_path);
                                preview_enabled = false;
                                preview_layout_id = None;
                                preview_wtn_mtime = None;
                                info!("preview: disabled (guide enabled)");
                            }

                            // Switch layout if requested.
                            if let Some(id) = &guide_layout_id {
                                if let Some(i) = layouts.iter().position(|l| &l.id == id) {
                                    if i != layout_index {
                                        layout_index = i;
                                        info!(
                                            "guide: switching layout -> {}",
                                            layouts[layout_index].id
                                        );
                                        set_mts_table(&master, &layouts[layout_index])?;
                                        wtn_path = resolve_path(&layouts[layout_index].wtn_path);
                                        base_wtn = Wtn::load(&wtn_path).with_context(|| {
                                            format!("Load wtn {}", wtn_path.display())
                                        })?;
                                        base_wtn_mtime =
                                            fs::metadata(&wtn_path).and_then(|m| m.modified()).ok();
                                        wtn = base_wtn.clone();
                                        refresh_vel_led_bases(
                                            &wtn,
                                            &mut vel_state,
                                            layouts[layout_index].edo_divisions,
                                            layouts[layout_index].pitch_offset,
                                            GuideMode::WaitRoot,
                                            &guide_pcs_abs,
                                            guide_dim,
                                        );
                                    }
                                }
                            }

                            guide_mode = GuideMode::WaitRoot;
                            trainer_mode = TrainerMode::Wait;
                            last_activity = Instant::now();
                            if screensaver_active {
                                info!("RGB screensaver: wake (guide enabled)");
                                screensaver_active = false;
                            }
                            paint_guide(
                                &wtn,
                                &rgb_index_by_device_id,
                                layouts
                                    .get(layout_index)
                                    .map(|l| l.edo_divisions)
                                    .unwrap_or(12),
                                layouts
                                    .get(layout_index)
                                    .map(|l| l.pitch_offset)
                                    .unwrap_or(0),
                                guide_mode,
                                &guide_pcs_root,
                                guide_root_pc,
                                guide_dim,
                                &octave_hold_by_device,
                            );
                            info!("guide: enabled (wait_root) chord_key={:?}", guide_chord_key);
                        } else {
                            // explicit disabled
                            guide_mode = GuideMode::Off;
                            trainer_mode = TrainerMode::Off;
                            guide_layout_id = None;
                            guide_chord_key = None;
                            guide_pcs_root.clear();
                            guide_pcs_abs.clear();
                            guide_root_pc = None;
                            if !screensaver_active {
                                paint_base(
                                    &wtn,
                                    true,
                                    &rgb_index_by_device_id,
                                    trainer_mode,
                                    &octave_hold_by_device,
                                );
                            }
                            info!("guide: disabled");
                        }
                    } else {
                        // file missing -> off
                        if guide_mode != GuideMode::Off {
                            guide_mode = GuideMode::Off;
                            trainer_mode = TrainerMode::Off;
                            guide_layout_id = None;
                            guide_chord_key = None;
                            guide_pcs_root.clear();
                            guide_pcs_abs.clear();
                            guide_root_pc = None;
                            if !screensaver_active {
                                paint_base(
                                    &wtn,
                                    true,
                                    &rgb_index_by_device_id,
                                    trainer_mode,
                                    &octave_hold_by_device,
                                );
                            }
                            info!("guide: disabled (file removed)");
                        }
                    }
                }
            }

            // trainer_mode is derived from guide_mode now.

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
                                wtn_mtime = base_wtn_mtime;
                                let edo = layouts[layout_index].edo_divisions;
                                let pitch_offset = layouts[layout_index].pitch_offset;
                                refresh_vel_led_bases(
                                    &wtn,
                                    &mut vel_state,
                                    edo,
                                    pitch_offset,
                                    guide_mode,
                                    &guide_pcs_abs,
                                    guide_dim,
                                );
                                paint_base(
                                    &wtn,
                                    false,
                                    &rgb_index_by_device_id,
                                    trainer_mode,
                                    &octave_hold_by_device,
                                );
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
                    // Treat preview as activity (prevents screensaver from immediately blanking).
                    last_activity = Instant::now();
                    if screensaver_active {
                        // Wake screensaver immediately on preview enable so the next physical key-down
                        // can be used as a trainer root (not consumed as a wake key).
                        info!("RGB screensaver: wake (preview enabled)");
                        screensaver_active = false;
                    }
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
                    wtn_mtime = base_wtn_mtime;
                    preview_wtn_mtime = None;
                    let edo = layouts[layout_index].edo_divisions;
                    let pitch_offset = layouts[layout_index].pitch_offset;
                    refresh_vel_led_bases(
                        &wtn,
                        &mut vel_state,
                        edo,
                        pitch_offset,
                        guide_mode,
                        &guide_pcs_abs,
                        guide_dim,
                    );
                    // If the RGB screensaver is active, keep LEDs blank.
                    if !screensaver_active {
                        paint_base(
                            &wtn,
                            true,
                            &rgb_index_by_device_id,
                            trainer_mode,
                            &octave_hold_by_device,
                        );
                    }
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
                                preview_wtn_mtime = Some(modified);
                                // Treat preview updates as activity.
                                last_activity = Instant::now();
                                if screensaver_active {
                                    info!("RGB screensaver: wake (preview update)");
                                    screensaver_active = false;
                                }
                                let edo = layouts[layout_index].edo_divisions;
                                let pitch_offset = layouts[layout_index].pitch_offset;
                                refresh_vel_led_bases(
                                    &wtn,
                                    &mut vel_state,
                                    edo,
                                    pitch_offset,
                                    guide_mode,
                                    &guide_pcs_abs,
                                    guide_dim,
                                );
                                if !screensaver_active {
                                    paint_base(
                                        &wtn,
                                        true,
                                        &rgb_index_by_device_id,
                                        trainer_mode,
                                        &octave_hold_by_device,
                                    );
                                }
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
                        last_analog: _,
                        last_analog_ts: _,
                        peak_speed,
                        mut at_level,
                        mut at_last_ts,
                        out_ch,
                        note,
                        playing,
                        last_val: _,
                        led,
                    } => {
                        if started.elapsed() >= peak_track {
                            let n = match aftertouch_mode {
                                AftertouchMode::SpeedMapped => {
                                    (peak_speed / aftertouch_speed_max.max(0.001)).clamp(0.0, 1.0)
                                }
                                _ => {
                                    let denom = (max_swing - threshold).max(0.001);
                                    ((peak - threshold) / denom).clamp(0.0, 1.0)
                                }
                            };

                            let n2 = velocity_profiles[velocity_profile_idx].apply(n);
                            let vel = (n2 * 126.0).round().clamp(0.0, 126.0) as u8 + 1;

                            // Aftertouch should be monotonic while held: it never decreases.
                            // This makes "press and hold" preserve the last loudness level.
                            if n2 > at_level {
                                at_level = n2;
                            }
                            at_level = at_level.clamp(0.0, 1.0);
                            at_last_ts = Instant::now();

                            let pressure: u8 = match aftertouch_mode {
                                AftertouchMode::SpeedMapped => {
                                    (at_level * 127.0).round().clamp(0.0, 127.0) as u8
                                }
                                AftertouchMode::PeakMapped => {
                                    (at_level * 126.0).round().clamp(0.0, 126.0) as u8 + 1
                                }
                                AftertouchMode::Off => 0,
                            };

                            if !playing {
                                let _ = midi_out.send_note(true, out_ch, note, vel);

                                // Emit an initial aftertouch value as soon as the note starts.
                                // This helps setups where aftertouch is mapped to loudness.
                                if !no_aftertouch {
                                    match aftertouch_mode {
                                        AftertouchMode::PeakMapped
                                        | AftertouchMode::SpeedMapped => {
                                            let _ = midi_out.send_polytouch(out_ch, note, pressure);
                                        }
                                        AftertouchMode::Off => {}
                                    }
                                }

                                // After firing, go to Aftershock state (like the Teensy state
                                // machine). While the key is still held, the next Update will
                                // return it to Tracking again, and repeated windows will emit
                                // aftertouch values.
                                vel_state.insert(
                                    key,
                                    VelState::Aftershock {
                                        quiet_since: None,
                                        at_level,
                                        at_last_ts,
                                        out_ch,
                                        note,
                                        playing: true,
                                        last_val: vel,
                                        led,
                                    },
                                );
                            } else {
                                if !no_aftertouch {
                                    match aftertouch_mode {
                                        AftertouchMode::PeakMapped => {
                                            let _ = midi_out.send_polytouch(out_ch, note, pressure);
                                        }
                                        AftertouchMode::SpeedMapped => {
                                            let _ = midi_out.send_polytouch(out_ch, note, pressure);
                                        }
                                        AftertouchMode::Off => {}
                                    }
                                }
                                vel_state.insert(
                                    key,
                                    VelState::Aftershock {
                                        quiet_since: None,
                                        at_level,
                                        at_last_ts,
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
                        at_level: _,
                        at_last_ts: _,
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
                // If a web-driven preview (e.g. chord trainer) is active, disable it so it can't
                // keep re-applying colors after wake.
                let _ = fs::remove_file(&preview_enabled_path);
                let _ = fs::remove_file(&preview_wtn_path);
                let _ = fs::remove_file(&trainer_mode_path);
                let _ = fs::remove_file(&guide_path);
                preview_enabled = false;
                preview_layout_id = None;
                guide_mode = GuideMode::Off;
                trainer_mode = TrainerMode::Off;
                guide_layout_id = None;
                guide_chord_key = None;
                guide_pcs_root.clear();
                guide_root_pc = None;
                paint_off(&rgb_index_by_device_id);
                screensaver_active = true;
            }

            publish_live(
                &wtn,
                &layouts,
                layout_index,
                press_threshold_bits.as_ref(),
                &aftertouch_mode,
                aftertouch_speed_max,
                octave_shift,
                screensaver_active,
                preview_enabled,
                guide_mode,
                &guide_chord_key,
                guide_root_pc,
                &board_by_device,
                &vel_state,
            );
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
            paint_base(
                &wtn,
                true,
                &rgb_index_by_device_id,
                trainer_mode,
                &octave_hold_by_device,
            );
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
                    if matches!(aftertouch_mode, AftertouchMode::Off) {
                        manual_press_threshold =
                            (manual_press_threshold - cfg.press_threshold_step).clamp(0.02, 0.98);
                        press_threshold_bits
                            .store(manual_press_threshold.to_bits(), Ordering::Relaxed);
                        info!("press_threshold now {:.3}", manual_press_threshold);
                    } else {
                        aftertouch_speed_max =
                            (aftertouch_speed_max - cfg.aftertouch_speed_step).clamp(1.0, 200.0);
                        info!("aftertouch_speed_max now {:.2}", aftertouch_speed_max);
                    }
                    continue;
                }
                HIDCodes::RightCtrl => {
                    if matches!(aftertouch_mode, AftertouchMode::Off) {
                        manual_press_threshold =
                            (manual_press_threshold + cfg.press_threshold_step).clamp(0.02, 0.98);
                        press_threshold_bits
                            .store(manual_press_threshold.to_bits(), Ordering::Relaxed);
                        info!("press_threshold now {:.3}", manual_press_threshold);
                    } else {
                        aftertouch_speed_max =
                            (aftertouch_speed_max + cfg.aftertouch_speed_step).clamp(1.0, 200.0);
                        info!("aftertouch_speed_max now {:.2}", aftertouch_speed_max);
                    }
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
                        AftertouchMode::PeakMapped => AftertouchMode::SpeedMapped,
                        AftertouchMode::SpeedMapped => AftertouchMode::Off,
                        AftertouchMode::Off => AftertouchMode::PeakMapped,
                    };
                    if matches!(aftertouch_mode, AftertouchMode::Off) {
                        press_threshold_bits
                            .store(manual_press_threshold.to_bits(), Ordering::Relaxed);
                    } else {
                        press_threshold_bits
                            .store(AFTERTOUCH_PRESS_THRESHOLD.to_bits(), Ordering::Relaxed);
                    }
                    info!(
                        "aftertouch_mode now {} (thr {:.2})",
                        aftertouch_mode.name(),
                        f32::from_bits(press_threshold_bits.load(Ordering::Relaxed))
                    );
                    continue;
                }
                HIDCodes::Space => {
                    if octave_hold_by_device.contains(&device_id) {
                        octave_hold_by_device.remove(&device_id);
                        info!("octave_hold toggled off (device_id={})", device_id);
                    } else {
                        octave_hold_by_device.insert(device_id);
                        info!("octave_hold toggled on (device_id={})", device_id);
                    }
                    paint_spacebar_indicator(
                        device_id,
                        &rgb_index_by_device_id,
                        control_bar_rgb_for_tm(trainer_mode),
                        octave_hold_by_device.contains(&device_id),
                    );
                    continue;
                }
                _ => {}
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
                    // Restore the control bar to the current mode colors.
                    // Space's indicator overrides to white when octave-hold is enabled.
                    let base_rgb = control_bar_rgb_for_tm(trainer_mode);
                    if hid == HIDCodes::Space {
                        paint_spacebar_indicator(
                            device_id,
                            &rgb_index_by_device_id,
                            base_rgb,
                            octave_hold_by_device.contains(&device_id),
                        );
                    } else {
                        for &lc in cols {
                            try_send_drop(
                                &rgb_tx,
                                RgbCmd::SetKey(RgbKey {
                                    device_index: dev_idx,
                                    row: control_bar_row,
                                    col: lc,
                                    rgb: base_rgb,
                                }),
                            );
                        }
                    }
                }
            }
        }

        if kind == "down" {
            if let Some(action) = actions_by_hid.get(&hid) {
                match action.as_str() {
                    "layout_next" => {
                        if preview_enabled {
                            let _ = fs::remove_file(&preview_enabled_path);
                            let _ = fs::remove_file(&preview_wtn_path);
                            let _ = fs::remove_file(&trainer_mode_path);
                            preview_enabled = false;
                            preview_layout_id = None;
                            preview_wtn_mtime = None;
                            info!("preview: disabled due to layout change");
                        }
                        if guide_mode != GuideMode::Off {
                            let _ = fs::remove_file(&guide_path);
                            guide_mode = GuideMode::Off;
                            trainer_mode = TrainerMode::Off;
                            guide_layout_id = None;
                            guide_chord_key = None;
                            guide_pcs_root.clear();
                            guide_root_pc = None;
                            info!("guide: disabled due to layout change");
                            // Layout switching does not repaint the control bar; restore it explicitly.
                            paint_control_bar(&rgb_index_by_device_id, (255, 0, 0));
                            // Consume the first layout keypress to exit guide mode only.
                            continue;
                        }
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
                            let edo = layouts[layout_index].edo_divisions;
                            let pitch_offset = layouts[layout_index].pitch_offset;
                            refresh_vel_led_bases(
                                &wtn,
                                &mut vel_state,
                                edo,
                                pitch_offset,
                                guide_mode,
                                &guide_pcs_abs,
                                guide_dim,
                            );
                            paint_base(
                                &wtn,
                                false,
                                &rgb_index_by_device_id,
                                trainer_mode,
                                &octave_hold_by_device,
                            );
                        }
                    }
                    "layout_prev" => {
                        if preview_enabled {
                            let _ = fs::remove_file(&preview_enabled_path);
                            let _ = fs::remove_file(&preview_wtn_path);
                            let _ = fs::remove_file(&trainer_mode_path);
                            preview_enabled = false;
                            preview_layout_id = None;
                            preview_wtn_mtime = None;
                            info!("preview: disabled due to layout change");
                        }
                        if guide_mode != GuideMode::Off {
                            let _ = fs::remove_file(&guide_path);
                            guide_mode = GuideMode::Off;
                            trainer_mode = TrainerMode::Off;
                            guide_layout_id = None;
                            guide_chord_key = None;
                            guide_pcs_root.clear();
                            guide_root_pc = None;
                            info!("guide: disabled due to layout change");
                            // Layout switching does not repaint the control bar; restore it explicitly.
                            paint_control_bar(&rgb_index_by_device_id, (255, 0, 0));
                            // Consume the first layout keypress to exit guide mode only.
                            continue;
                        }
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
                            let edo = layouts[layout_index].edo_divisions;
                            let pitch_offset = layouts[layout_index].pitch_offset;
                            refresh_vel_led_bases(
                                &wtn,
                                &mut vel_state,
                                edo,
                                pitch_offset,
                                guide_mode,
                                &guide_pcs_abs,
                                guide_dim,
                            );
                            paint_base(
                                &wtn,
                                false,
                                &rgb_index_by_device_id,
                                trainer_mode,
                                &octave_hold_by_device,
                            );
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
                let edo = layouts
                    .get(layout_index)
                    .map(|l| l.edo_divisions)
                    .unwrap_or(12);
                let pitch_offset = layouts
                    .get(layout_index)
                    .map(|l| l.pitch_offset)
                    .unwrap_or(0);

                // Guide: first non-control-bar key-down selects the root pitch class.
                // This does not suppress MIDI; it only affects LED painting.
                if kind == "down" && guide_mode == GuideMode::WaitRoot {
                    let mut pc = (note as i32) + pitch_offset;
                    pc %= edo;
                    if pc < 0 {
                        pc += edo;
                    }
                    guide_root_pc = Some(pc);
                    guide_mode = GuideMode::Active;
                    trainer_mode = TrainerMode::Active;
                    guide_pcs_abs.clear();
                    for rel in guide_pcs_root.iter() {
                        let mut x = (rel + pc) % edo;
                        if x < 0 {
                            x += edo;
                        }
                        guide_pcs_abs.insert(x);
                    }
                    last_activity = Instant::now();
                    info!(
                        "guide: root selected pc={} chord_key={:?}",
                        pc, guide_chord_key
                    );
                    paint_guide(
                        &wtn,
                        &rgb_index_by_device_id,
                        edo,
                        pitch_offset,
                        guide_mode,
                        &guide_pcs_root,
                        guide_root_pc,
                        guide_dim,
                        &octave_hold_by_device,
                    );

                    // Ensure any currently-held keys restore to the correct (dimmed) guide idle colors.
                    refresh_vel_led_bases(
                        &wtn,
                        &mut vel_state,
                        edo,
                        pitch_offset,
                        guide_mode,
                        &guide_pcs_abs,
                        guide_dim,
                    );
                }

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
                                base_rgb: guide_idle_rgb(
                                    cell.col_rgb,
                                    cell.key,
                                    edo,
                                    pitch_offset,
                                    guide_mode,
                                    &guide_pcs_abs,
                                    guide_dim,
                                ),
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
                            last_analog: analog,
                            last_analog_ts: ts,
                            peak_speed: 0.0,
                            at_level: 0.0,
                            at_last_ts: ts,
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
                            VelState::Tracking {
                                peak,
                                last_analog,
                                last_analog_ts,
                                peak_speed,
                                ..
                            } => {
                                if analog > *peak {
                                    *peak = analog;
                                }
                                // Speed tracking for speed-mapped aftertouch.
                                let dt = (ts - *last_analog_ts).as_secs_f32();
                                if dt > 0.0 {
                                    let s = (analog - *last_analog) / dt;
                                    if s.is_finite() && s > *peak_speed {
                                        *peak_speed = s.max(0.0);
                                    }
                                }
                                *last_analog = analog;
                                *last_analog_ts = ts;
                            }
                            VelState::Aftershock {
                                at_level,
                                at_last_ts,
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
                                    let at_level = *at_level;
                                    let at_last_ts = *at_last_ts;
                                    *st = VelState::Tracking {
                                        started: ts,
                                        peak: analog,
                                        last_analog: analog,
                                        last_analog_ts: ts,
                                        peak_speed: 0.0,
                                        at_level,
                                        at_last_ts,
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
                            at_level,
                            at_last_ts,
                            led,
                            ..
                        }) => {
                            if playing {
                                vel_state.insert(
                                    key_id,
                                    VelState::Aftershock {
                                        quiet_since: Some(ts),
                                        at_level,
                                        at_last_ts,
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
                            at_level,
                            at_last_ts,
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
                                    at_level,
                                    at_last_ts,
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
                                            rgb: guide_idle_rgb(
                                                cell.col_rgb,
                                                cell.key,
                                                edo,
                                                pitch_offset,
                                                guide_mode,
                                                &guide_pcs_abs,
                                                guide_dim,
                                            ),
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
