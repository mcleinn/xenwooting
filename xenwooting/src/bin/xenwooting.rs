use alsa::seq::{EvNote, Event, EventType, PortCap, PortType, Seq};
use alsa::Direction;
use anyhow::{Context, Result};
use log::info;
use std::collections::{HashMap, HashSet};
use std::ffi::CString;
use std::fs;
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
        midi_out_name: "XenWooting".to_string(),
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

fn main() -> Result<()> {
    env_logger::init();

    // Install signal handlers as early as possible.
    install_signal_handlers();

    // Wooting Analog SDK init (required before any other SDK call)
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
    set_mts_table(&master, &cfg.layouts[layout_index])?;

    // Load wtn
    let mut wtn_path = resolve_path(&cfg.layouts[layout_index].wtn_path);
    let mut wtn =
        Wtn::load(&wtn_path).with_context(|| format!("Load wtn {}", wtn_path.display()))?;
    let mut wtn_mtime: Option<SystemTime> = fs::metadata(&wtn_path).and_then(|m| m.modified()).ok();
    let mut last_wtn_check = Instant::now();

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
        eprintln!("No Wooting Analog devices detected; waiting...");
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

    let mut octave_shift: i8 = 0; // persistent shifts MIDI channel index
    let mut octave_hold: bool = false; // momentary +1 channel (Space)

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
    let poll_ids: Vec<u64> = devices
        .iter()
        .filter(|d| board_by_device.contains_key(&d.device_id))
        .map(|d| d.device_id)
        .collect();
    if log_poll {
        eprintln!("poll_ids={:?}", poll_ids);
    }
    let press_threshold_bits = Arc::new(std::sync::atomic::AtomicU32::new(
        cfg.press_threshold.to_bits(),
    ));
    let update_delta = cfg.aftertouch_delta.clamp(0.001, 0.2);
    let verbose = dump_hid || log_edges || log_midi || log_poll;
    let press_threshold_bits_poll = Arc::clone(&press_threshold_bits);
    std::thread::spawn(move || {
        let mut down_by_device: HashMap<u64, HashSet<HIDCodes>> = HashMap::new();
        let mut last_analog_by_device: HashMap<u64, HashMap<HIDCodes, f32>> = HashMap::new();
        for id in &poll_ids {
            down_by_device.insert(*id, HashSet::new());
            last_analog_by_device.insert(*id, HashMap::new());
        }
        let mut last_report = std::time::Instant::now() - Duration::from_secs(999);
        let mut err_count: u64 = 0;

        while RUNNING.load(Ordering::SeqCst) {
            let _ = sdk::set_keycode_mode(KeycodeType::HID);
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

    let paint_base = |wtn: &Wtn, paint_control_bar: bool| {
        if !rgb_enabled {
            return;
        }
        if log_edges || log_poll || log_midi {
            eprintln!("paint_base: start");
        }
        for (_dev_id, bcfg) in board_by_device.iter() {
            let wtn_board = bcfg.wtn_board;
            let dev_idx = cfg.rgb.rgb_device_index_for_wtn_board(wtn_board);

            // Paint 4x14 from .wtn.
            //
            // Important: `wtn` is in *logical* orientation. If the board is rotated, we must
            // map each physical (mr,mc) to its logical lookup index.
            for mr in 0..4u8 {
                for mc in 0..14u8 {
                    let loc = KeyLoc {
                        midi_row: mr,
                        midi_col: mc,
                        led_row: mr + 1,
                        led_col: mc,
                    };
                    let loc = match rotate_4x14(loc, bcfg.rotation_deg)
                        .and_then(|l| mirror_cols_4x14(l, bcfg.mirror_cols))
                    {
                        Ok(v) => v,
                        Err(_) => continue,
                    };
                    let idx = (loc.midi_row as usize) * 14 + (loc.midi_col as usize);
                    if let Some(cell) = wtn.cell(wtn_board, idx) {
                        try_send_drop(
                            &rgb_tx,
                            RgbCmd::SetKey(RgbKey {
                                device_index: dev_idx,
                                row: mr + 1,
                                col: mc,
                                rgb: cell.col_rgb,
                            }),
                        );
                    }
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

    paint_base(&wtn, true);

    // Track hotplug of RGB devices; repaint base when device count changes.
    let mut last_rgb_count: u8 = if rgb_enabled { Rgb::device_count() } else { 0 };
    let mut last_rgb_check = Instant::now();

    if log_edges || log_poll || log_midi {
        eprintln!("ready: waiting for key edges");
    }

    // Peak-tracked velocity state per key (device_id + HID).
    let mut vel_state: HashMap<(u64, HIDCodes), VelState> = HashMap::new();

    let refresh_vel_led_bases = |wtn: &Wtn, vel_state: &mut HashMap<(u64, HIDCodes), VelState>| {
        for ((device_id, hid), st) in vel_state.iter_mut() {
            let Some(bcfg) = board_by_device.get(device_id) else {
                continue;
            };
            let rotation = bcfg.rotation_deg;

            let Some(loc0) = hid_map.loc_for(hid.clone()) else {
                continue;
            };
            let loc = match rotate_4x14(loc0, rotation)
                .and_then(|l| mirror_cols_4x14(l, bcfg.mirror_cols))
            {
                Ok(v) => v,
                Err(_) => continue,
            };
            let idx = (loc.midi_row as usize) * 14 + (loc.midi_col as usize);
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
                paint_base(&wtn, true);
                info!("RGB device count changed; repainted base ({})", now);
            }
        }
        // Detect .wtn changes on disk and reload without restarting the service.
        // This lets the configurator update XenWooting immediately after Save.
        if last_wtn_check.elapsed() >= Duration::from_millis(200) {
            last_wtn_check = Instant::now();
            if let Ok(modified) = fs::metadata(&wtn_path).and_then(|m| m.modified()) {
                let changed = match wtn_mtime {
                    Some(prev) => modified != prev,
                    None => true,
                };
                if changed {
                    match Wtn::load(&wtn_path) {
                        Ok(new_wtn) => {
                            wtn = new_wtn;
                            wtn_mtime = Some(modified);
                            refresh_vel_led_bases(&wtn, &mut vel_state);
                            paint_base(&wtn, false);
                            info!("Reloaded wtn from disk: {}", wtn_path.display());
                        }
                        Err(e) => {
                            eprintln!("wtn reload failed ({}): {e}", wtn_path.display());
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
            Ok(e) => e,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(_) => break,
        };

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
                    octave_hold = true;
                    info!("octave_hold on");
                    continue;
                }
                _ => {}
            }
        } else if kind == "up" {
            if let HIDCodes::Space = hid {
                octave_hold = false;
                info!("octave_hold off");
                continue;
            }
        }

        // Compute debug LED coordinates (either from control bar mapping or hid_map).
        let dev_idx_dbg = cfg.rgb.rgb_device_index_for_wtn_board(wtn_board);
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
            let dev_idx = cfg.rgb.rgb_device_index_for_wtn_board(wtn_board);
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

        if kind == "down" {
            if let Some(action) = actions_by_hid.get(&hid) {
                match action.as_str() {
                    "layout_next" => {
                        layout_index = (layout_index + 1) % cfg.layouts.len();
                        eprintln!("layout_next -> {}", cfg.layouts[layout_index].id);
                        set_mts_table(&master, &cfg.layouts[layout_index])?;
                        wtn_path = resolve_path(&cfg.layouts[layout_index].wtn_path);
                        eprintln!("loading wtn: {}", wtn_path.display());
                        wtn = Wtn::load(&wtn_path)
                            .with_context(|| format!("Load wtn {}", wtn_path.display()))?;
                        wtn_mtime = fs::metadata(&wtn_path).and_then(|m| m.modified()).ok();
                        refresh_vel_led_bases(&wtn, &mut vel_state);
                        paint_base(&wtn, false);
                    }
                    "layout_prev" => {
                        if layout_index == 0 {
                            layout_index = cfg.layouts.len() - 1;
                        } else {
                            layout_index -= 1;
                        }
                        eprintln!("layout_prev -> {}", cfg.layouts[layout_index].id);
                        set_mts_table(&master, &cfg.layouts[layout_index])?;
                        wtn_path = resolve_path(&cfg.layouts[layout_index].wtn_path);
                        eprintln!("loading wtn: {}", wtn_path.display());
                        wtn = Wtn::load(&wtn_path)
                            .with_context(|| format!("Load wtn {}", wtn_path.display()))?;
                        wtn_mtime = fs::metadata(&wtn_path).and_then(|m| m.modified()).ok();
                        refresh_vel_led_bases(&wtn, &mut vel_state);
                        paint_base(&wtn, false);
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
            let idx = (loc.midi_row as usize) * 14 + (loc.midi_col as usize);
            if let Some(cell) = wtn.cell(wtn_board, idx) {
                let base_ch = cell.chan_1based.saturating_sub(1);
                let hold = if octave_hold { 1i16 } else { 0i16 };
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
                        let dev_idx = cfg.rgb.rgb_device_index_for_wtn_board(wtn_board);
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
                                let dev_idx = cfg.rgb.rgb_device_index_for_wtn_board(wtn_board);
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
