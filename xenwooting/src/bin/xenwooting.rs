use alsa::seq::{EvCtrl, EvNote, Event, EventType, PortCap, PortType, Seq};
use alsa::Direction;
use anyhow::{Context, Result};
use log::{info, warn};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::CString;
use std::fs;
use std::hash::Hash;
use std::hash::Hasher;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
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

const DEFAULT_RUNTIME_DIR: &str = "/run/xenwooting";

fn env_path_buf(name: &str, default: PathBuf) -> PathBuf {
    match std::env::var_os(name) {
        Some(v) if !v.as_os_str().is_empty() => PathBuf::from(v),
        _ => default,
    }
}

fn runtime_dir() -> PathBuf {
    env_path_buf("XENWTN_RUNTIME_DIR", PathBuf::from(DEFAULT_RUNTIME_DIR))
}

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
    velocity_profile: String,
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

fn compute_layout_pitches(
    wtn: &Wtn,
    edo: i32,
    pitch_offset: i32,
    octave_shift: i8,
) -> HashMap<String, Vec<Option<i32>>> {
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
}

struct LivePublisher {
    seq: u64,
    last_hash: u64,
    last_publish_mode: Instant,
    last_publish_pressed: Instant,
    dirty_mode: bool,
    dirty_pressed: bool,
    layout_key: String,
    layout_pitches: HashMap<String, Vec<Option<i32>>>,
    layout_pitches_hash: u64,
}

struct LiveWriterSlot {
    gen: u64,
    pending: Option<LiveState>,
}

#[derive(Clone)]
struct LiveWriter {
    inner: Arc<(Mutex<LiveWriterSlot>, Condvar)>,
}

impl LiveWriter {
    fn new(path: PathBuf) -> Self {
        let inner = Arc::new((
            Mutex::new(LiveWriterSlot {
                gen: 0,
                pending: None,
            }),
            Condvar::new(),
        ));
        let inner2 = inner.clone();

        // Ensure the parent directory exists (tmpfs); best-effort.
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }

        std::thread::Builder::new()
            .name("live-writer".to_string())
            .spawn(move || {
                let mut last_gen: u64 = 0;
                loop {
                    let st_opt = {
                        let (lock, cv) = &*inner2;
                        let mut slot = lock.lock().unwrap();
                        while slot.gen == last_gen && slot.pending.is_none() {
                            slot = cv.wait(slot).unwrap();
                        }
                        last_gen = slot.gen;
                        slot.pending.take()
                    };

                    let Some(st) = st_opt else {
                        continue;
                    };

                    let text = match serde_json::to_string(&st) {
                        Ok(t) => t,
                        Err(e) => {
                            warn!("live state json encode failed: {e:?}");
                            continue;
                        }
                    };

                    let t0 = Instant::now();
                    if let Err(e) = write_file_atomic(&path, &text) {
                        warn!("live state write failed path={} err={e:?}", path.display());
                    }
                    let ms = t0.elapsed().as_millis().min(u64::MAX as u128) as u64;
                    if ms >= 10 {
                        info!("LIVE_WRITE_SLOW ms={}", ms);
                    }
                }
            })
            .ok();

        Self { inner }
    }

    fn submit(&self, st: LiveState) {
        let (lock, cv) = &*self.inner;
        if let Ok(mut slot) = lock.lock() {
            slot.gen = slot.gen.wrapping_add(1);
            slot.pending = Some(st);
            cv.notify_one();
        }
    }
}

impl LivePublisher {
    fn new() -> Self {
        Self {
            seq: 0,
            last_hash: 0,
            last_publish_mode: Instant::now() - Duration::from_secs(999),
            last_publish_pressed: Instant::now() - Duration::from_secs(999),
            dirty_mode: true,
            dirty_pressed: true,
            layout_key: String::new(),
            layout_pitches: HashMap::new(),
            layout_pitches_hash: 0,
        }
    }

    fn mark_mode_dirty(&mut self) {
        self.dirty_mode = true;
    }

    fn mark_layout_dirty(&mut self) {
        // Force a layout_pitches recompute even if the layout id/shift is unchanged.
        // Needed when the active WTN mapping changes (preview reload, on-disk reload).
        self.dirty_mode = true;
        self.layout_key.clear();
    }

    fn mark_pressed_dirty(&mut self) {
        self.dirty_pressed = true;
    }

    fn maybe_publish(
        &mut self,
        live_writer: &LiveWriter,
        wtn: &Wtn,
        layouts: &Vec<xenwooting::config::LayoutConfig>,
        layout_index: usize,
        press_threshold_bits: &AtomicU32,
        aftertouch_mode: &AftertouchMode,
        aftertouch_speed_max: f32,
        velocity_profile: &str,
        octave_shift: i8,
        screensaver_active: bool,
        preview_enabled: bool,
        guide_mode: GuideMode,
        guide_chord_key: &Option<String>,
        guide_root_pc: Option<i32>,
        pressed0: &HashSet<i32>,
        pressed1: &HashSet<i32>,
    ) {
        if !self.dirty_mode && !self.dirty_pressed {
            return;
        }

        // Rate-limit pressed updates (coalesce rapid changes).
        if self.dirty_pressed && self.last_publish_pressed.elapsed() < Duration::from_millis(33) {
            return;
        }

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
        if cur_layout_key != self.layout_key {
            self.layout_key = cur_layout_key;
            self.layout_pitches = compute_layout_pitches(wtn, edo, pitch_offset, octave_shift);
            let mut hh = std::collections::hash_map::DefaultHasher::new();
            for k in ["Board0", "Board1"] {
                hh.write(k.as_bytes());
                if let Some(v) = self.layout_pitches.get(k) {
                    for x in v.iter() {
                        match x {
                            Some(n) => hh.write_i32(*n),
                            None => hh.write_i32(i32::MIN),
                        }
                    }
                }
            }
            self.layout_pitches_hash = hh.finish();
            self.dirty_mode = true;
        }

        let mut v0: Vec<i32> = pressed0.iter().copied().collect();
        let mut v1: Vec<i32> = pressed1.iter().copied().collect();
        v0.sort();
        v1.sort();

        let mut pressed: HashMap<String, Vec<i32>> = HashMap::new();
        pressed.insert("Board0".to_string(), v0);
        pressed.insert("Board1".to_string(), v1);

        let press_threshold = f32::from_bits(press_threshold_bits.load(Ordering::Relaxed));

        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        hasher.write(self.layout_key.as_bytes());
        hasher.write_u64(self.layout_pitches_hash);
        hasher.write_u32(press_threshold.to_bits());
        hasher.write(aftertouch_mode.name().as_bytes());
        hasher.write_u32(aftertouch_speed_max.to_bits());
        hasher.write(velocity_profile.as_bytes());
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
        if h == self.last_hash {
            self.dirty_mode = false;
            self.dirty_pressed = false;
            return;
        }

        if self.dirty_mode && self.last_publish_mode.elapsed() < Duration::from_millis(15) {
            return;
        }

        if self.dirty_pressed {
            self.last_publish_pressed = Instant::now();
        }
        if self.dirty_mode {
            self.last_publish_mode = Instant::now();
        }

        self.last_hash = h;
        self.seq = self.seq.wrapping_add(1);

        let ts_ms = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let state = LiveState {
            version: 1,
            seq: self.seq,
            ts_ms,
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
                velocity_profile: velocity_profile.to_string(),
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
            layout_pitches: self.layout_pitches.clone(),
        };

        live_writer.submit(state);

        self.dirty_mode = false;
        self.dirty_pressed = false;
    }
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
        // Do not fsync for HUD state; it can stall the realtime loop.
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
        out_ch: u8,
        note: u8,
        playing: bool,
        led: Option<LedState>,
    },
}

#[derive(Debug, Clone)]
struct PendingNoteOff {
    due: Instant,
    ch: u8,
    note: u8,
    origin_device_id: Option<u64>,
    origin_hid: Option<HIDCodes>,
}

#[derive(Default)]
struct ActiveMask {
    w0: AtomicU64,
    w1: AtomicU64,
    w2: AtomicU64,
    w3: AtomicU64,
}

impl ActiveMask {
    fn clear_all(&self) {
        self.w0.store(0, Ordering::Relaxed);
        self.w1.store(0, Ordering::Relaxed);
        self.w2.store(0, Ordering::Relaxed);
        self.w3.store(0, Ordering::Relaxed);
    }

    fn set(&self, code: u8, down: bool) {
        let idx = (code as usize) / 64;
        let bit = (code as usize) % 64;
        let mask = 1u64 << bit;
        let w = match idx {
            0 => &self.w0,
            1 => &self.w1,
            2 => &self.w2,
            _ => &self.w3,
        };
        if down {
            w.fetch_or(mask, Ordering::Relaxed);
        } else {
            w.fetch_and(!mask, Ordering::Relaxed);
        }
    }

    fn words(&self) -> [u64; 4] {
        [
            self.w0.load(Ordering::Relaxed),
            self.w1.load(Ordering::Relaxed),
            self.w2.load(Ordering::Relaxed),
            self.w3.load(Ordering::Relaxed),
        ]
    }
}

struct CaptureSamplesState {
    base_ts_ms: u64,
    trigger_device_id: u64,
    start: Instant,
    end: Instant,
    start_unix_ms: u64,
    cfg_line: String,
    out_dir: PathBuf,

    // Fixed-size ring buffer for samples. Each sample is 20 bytes:
    // [t_us:u64][device_id:u64][hid_u8:u8][pad:u8][analog_q:u16]
    // Total: 8+8+1+1+2 = 20
    ring: Vec<u8>,
    write_idx: u32,
    wrapped: bool,
    dropped_samples: u64,
}

struct CaptureEventsState {
    base_ts_ms: u64,
    trigger_device_id: u64,
    start: Instant,
    end: Instant,
    start_unix_ms: u64,
    cfg_line: String,
    out_dir: PathBuf,

    csv_lines: Vec<String>,
    txt_lines: Vec<String>,
}

struct CaptureShared {
    active: AtomicBool,
    samples: Mutex<Option<CaptureSamplesState>>,
    events: Mutex<Option<CaptureEventsState>>,
    lock_drops: std::sync::atomic::AtomicU64,
    lag_ms: std::sync::atomic::AtomicU64,
}

const CAPTURE_REC_BYTES: usize = 20;

fn capture_write_sample(
    st: &mut CaptureSamplesState,
    t_us: u64,
    device_id: u64,
    hid: u8,
    analog_q: u16,
) {
    if st.ring.is_empty() {
        return;
    }
    let cap_recs = (st.ring.len() / CAPTURE_REC_BYTES) as u32;
    if cap_recs == 0 {
        return;
    }
    let idx = st.write_idx % cap_recs;
    let off = (idx as usize) * CAPTURE_REC_BYTES;

    st.ring[off..off + 8].copy_from_slice(&t_us.to_le_bytes());
    st.ring[off + 8..off + 16].copy_from_slice(&device_id.to_le_bytes());
    st.ring[off + 16] = hid;
    st.ring[off + 17] = 0u8;
    st.ring[off + 18..off + 20].copy_from_slice(&analog_q.to_le_bytes());

    st.write_idx = st.write_idx.wrapping_add(1);
    if !st.wrapped && st.write_idx >= cap_recs {
        st.wrapped = true;
    }
}

fn capture_stop_and_flush(
    capture: &Arc<CaptureShared>,
) -> std::io::Result<Option<(u64, String, String, String, String)>> {
    // Stop polling capture first to reduce contention.
    capture.active.store(false, Ordering::Relaxed);

    let lock_drops = capture.lock_drops.load(Ordering::Relaxed);
    let mut guard = capture
        .samples
        .lock()
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::Other, "capture lock poisoned"))?;
    let Some(st) = guard.take() else {
        // Best-effort: still clear events if present.
        if let Ok(mut eg) = capture.events.lock() {
            *eg = None;
        }
        return Ok(None);
    };
    let mut events_guard = capture.events.lock().map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::Other, "capture events lock poisoned")
    })?;
    let events = events_guard.take();

    let trigger_device_id = st.trigger_device_id;
    let base_ts_ms = st.base_ts_ms;
    let stopped_unix_ms = unix_time_ms();

    let cap_recs = st.ring.len() / CAPTURE_REC_BYTES;
    let count = if st.wrapped {
        cap_recs
    } else {
        (st.write_idx as usize).min(cap_recs)
    };

    std::fs::create_dir_all(&st.out_dir)?;
    let capture_csv_path = st.out_dir.join(format!("{base_ts_ms}_capture.csv"));
    let capture_txt_path = st.out_dir.join(format!("{base_ts_ms}_capture.txt"));
    let dump_csv_path = st.out_dir.join(format!("{base_ts_ms}_dump.csv"));
    let dump_txt_path = st.out_dir.join(format!("{base_ts_ms}_dump.txt"));

    // TXT metadata.
    {
        use std::io::Write;
        let mut f = std::io::BufWriter::new(std::fs::File::create(&capture_txt_path)?);
        writeln!(f, "timestamp_ms={}", base_ts_ms)?;
        writeln!(f, "trigger_device_id={}", st.trigger_device_id)?;
        writeln!(f, "start_unix_ms={}", st.start_unix_ms)?;
        writeln!(f, "stop_unix_ms={}", stopped_unix_ms)?;
        writeln!(f, "duration_ms_target={}", (st.end - st.start).as_millis())?;
        writeln!(f, "ring_bytes={}", st.ring.len())?;
        writeln!(f, "rec_bytes={}", CAPTURE_REC_BYTES)?;
        writeln!(f, "write_idx={}", st.write_idx)?;
        writeln!(f, "wrapped={}", st.wrapped)?;
        writeln!(f, "dropped_samples={}", st.dropped_samples)?;
        writeln!(f, "lock_drops={}", lock_drops)?;
        writeln!(f, "{}", st.cfg_line)?;
    }

    // CSV samples.
    {
        use std::io::Write;
        let mut f = std::io::BufWriter::new(std::fs::File::create(&capture_csv_path)?);
        writeln!(f, "t_us,device_id,hid,analog,analog_q")?;

        let start_idx = if st.wrapped {
            (st.write_idx as usize) % cap_recs
        } else {
            0
        };
        for i in 0..count {
            let idx = (start_idx + i) % cap_recs;
            let off = idx * CAPTURE_REC_BYTES;
            let t_us = u64::from_le_bytes(st.ring[off..off + 8].try_into().unwrap());
            let dev = u64::from_le_bytes(st.ring[off + 8..off + 16].try_into().unwrap());
            let hid_u8 = st.ring[off + 16];
            let analog_q = u16::from_le_bytes(st.ring[off + 18..off + 20].try_into().unwrap());
            let analog = (analog_q as f32) / 65535.0;
            let hid = HIDCodes::from_u8(hid_u8)
                .map(|h| format!("{h:?}"))
                .unwrap_or_else(|| format!("u8_{hid_u8}"));
            writeln!(
                f,
                "{t_us},{dev},{},{:.6},{analog_q}",
                csv_escape(&hid),
                analog
            )?;
        }
    }

    // Dump CSV/TXT for this capture window (event log with shared timebase).
    if let Some(mut ev) = events {
        ev.txt_lines
            .push(format!("stop_unix_ms={}", stopped_unix_ms));
        ev.txt_lines
            .push(format!("events_rows={}", ev.csv_lines.len()));
        // TXT
        {
            use std::io::Write;
            let mut f = std::io::BufWriter::new(std::fs::File::create(&dump_txt_path)?);
            for line in ev.txt_lines {
                writeln!(f, "{}", line)?;
            }
        }
        // CSV
        {
            use std::io::Write;
            let mut f = std::io::BufWriter::new(std::fs::File::create(&dump_csv_path)?);
            // Keep manual-dump columns compatible; add t_cap_ms + pressure.
            writeln!(f, "t_ms,t_cap_ms,event,kind,device_id,hid,analog,age_ms,peak,last,peak_speed,pressed,playing,ch,note,vel,pressure,delta,thr_down,thr_up,edge_t_cap_ms,send_delay_ms,lag_ms,msg")?;
            for line in ev.csv_lines {
                writeln!(f, "{}", line)?;
            }
        }
    } else {
        // Still create empty files so the UI can show "missing events" gracefully.
        std::fs::write(
            &dump_txt_path,
            format!(
                "timestamp_ms={}\nstop_unix_ms={}\n(no capture dump events)\n",
                base_ts_ms, stopped_unix_ms
            ),
        )?;
        std::fs::write(&dump_csv_path, "t_ms,t_cap_ms,event,kind,device_id,hid,analog,age_ms,peak,last,peak_speed,pressed,playing,ch,note,vel,pressure,delta,thr_down,thr_up,msg\n")?;
    }

    Ok(Some((
        trigger_device_id,
        capture_csv_path.display().to_string(),
        capture_txt_path.display().to_string(),
        dump_csv_path.display().to_string(),
        dump_txt_path.display().to_string(),
    )))
}

fn capture_stop_discard(capture: &Arc<CaptureShared>) -> bool {
    // Stop polling capture first to reduce contention.
    capture.active.store(false, Ordering::Relaxed);

    let mut had = false;
    if let Ok(mut guard) = capture.samples.lock() {
        had = guard.take().is_some();
    }
    if let Ok(mut eg) = capture.events.lock() {
        *eg = None;
    }
    had
}

fn schedule_midi_ping(
    midi_out: &mut AlsaMidiOut,
    note_on_count: &mut HashMap<(u8, u8), u32>,
    pending_noteoffs: &mut VecDeque<PendingNoteOff>,
) {
    // Quiet, short confirmation that a debug dump was triggered.
    // Use a short triad on channel 0.
    let ch = 0u8;
    let vel = 24u8;
    let dur = Duration::from_millis(60);
    for note in [72u8, 76u8, 79u8] {
        if midi_out.send_note(true, ch, note, vel).is_ok() {
            *note_on_count.entry((ch, note)).or_insert(0) += 1;
            pending_noteoffs.push_back(PendingNoteOff {
                due: Instant::now() + dur,
                ch,
                note,
                origin_device_id: None,
                origin_hid: None,
            });
        }
    }
}

#[derive(Debug, Clone)]
struct DebugEvent {
    t_ms: u64,
    msg: String,
}

fn dbg_push(dbg_ring: &mut VecDeque<DebugEvent>, start_ts: &Instant, msg: impl Into<String>) {
    if dbg_ring.len() >= 200 {
        dbg_ring.pop_front();
    }
    dbg_ring.push_back(DebugEvent {
        t_ms: start_ts.elapsed().as_millis() as u64,
        msg: msg.into(),
    });
}

fn rgb_send_critical(
    rgb_tx: &crossbeam_channel::Sender<RgbCmd>,
    dbg_ring: &mut VecDeque<DebugEvent>,
    start_ts: &Instant,
    rgb_drop_critical: &mut u64,
    cfg_line: &str,
    cmd: RgbCmd,
    what: String,
) {
    match rgb_tx.try_send(cmd) {
        Ok(()) => {
            dbg_push(dbg_ring, start_ts, format!("RGB ok {}", what));
        }
        Err(crossbeam_channel::TrySendError::Full(_)) => {
            *rgb_drop_critical = rgb_drop_critical.saturating_add(1);
            warn!(
                "RGB critical queue full; dropped {} (drops={})",
                what, *rgb_drop_critical
            );
            dbg_push(dbg_ring, start_ts, format!("RGB DROP {}", what));
            dbg_push(dbg_ring, start_ts, cfg_line.to_string());
        }
        Err(crossbeam_channel::TrySendError::Disconnected(_)) => {
            warn!("RGB channel disconnected; dropped {}", what);
            dbg_push(dbg_ring, start_ts, format!("RGB DISCONNECTED {}", what));
        }
    }
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

    fn send_cc(&mut self, ch: u8, cc: u32, value: u8) -> Result<()> {
        let ev = EvCtrl {
            channel: ch,
            param: cc,
            value: value as i32,
        };
        let mut e = Event::new(EventType::Controller, &ev);
        e.set_source(self.port);
        e.set_subs();
        e.set_direct();
        self.seq
            .event_output_direct(&mut e)
            .context("Failed to output ALSA CC event")?;
        Ok(())
    }

    fn send_pitchbend(&mut self, ch: u8, bend: i32) -> Result<()> {
        let ev = EvCtrl {
            channel: ch,
            param: 0,
            value: bend.clamp(-8192, 8191),
        };
        let mut e = Event::new(EventType::Pitchbend, &ev);
        e.set_source(self.port);
        e.set_subs();
        e.set_direct();
        self.seq
            .event_output_direct(&mut e)
            .context("Failed to output ALSA pitchbend event")?;
        Ok(())
    }

    fn panic_all(&mut self) {
        for ch in 0..16u8 {
            // Sustain off
            let _ = self.send_cc(ch, 64, 0);
            // All sound off
            let _ = self.send_cc(ch, 120, 0);
            // All notes off
            let _ = self.send_cc(ch, 123, 0);
        }
    }
}

fn bend_from_amounts(up_amt: f32, down_amt: f32) -> i32 {
    let x = (up_amt - down_amt).clamp(-1.0, 1.0);
    if x >= 0.0 {
        (x * 8191.0).round().clamp(0.0, 8191.0) as i32
    } else {
        (x * 8192.0).round().clamp(-8192.0, 0.0) as i32
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

fn state_base_dir() -> Result<PathBuf> {
    if let Ok(xdg) = std::env::var("XDG_STATE_HOME") {
        if !xdg.trim().is_empty() {
            return Ok(PathBuf::from(xdg));
        }
    }
    let home = std::env::var("HOME").context("HOME is not set")?;
    Ok(Path::new(&home).join(".local").join("state"))
}

fn resolve_output_dir(cfg: &Config) -> Result<PathBuf> {
    if let Some(s) = cfg.output_dir.as_deref() {
        let p = s.trim();
        if !p.is_empty() {
            return Ok(PathBuf::from(p));
        }
    }
    Ok(state_base_dir()?.join("xenwooting"))
}

fn unix_time_ms() -> u64 {
    use std::time::UNIX_EPOCH;
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis().min(u64::MAX as u128) as u64)
        .unwrap_or(0)
}

fn csv_escape(s: &str) -> String {
    // Minimal CSV escaping: quote if needed; double internal quotes.
    let needs = s.contains(',') || s.contains('"') || s.contains('\n') || s.contains('\r');
    if !needs {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        if ch == '"' {
            out.push('"');
            out.push('"');
        } else {
            out.push(ch);
        }
    }
    out.push('"');
    out
}

fn capture_events_push_row_at(capture: &Arc<CaptureShared>, when: Instant, row: [String; 24]) {
    capture_events_push_row_meta_at(capture, when, None, None, row)
}

fn capture_events_push_row_meta_at(
    capture: &Arc<CaptureShared>,
    when: Instant,
    edge_when: Option<Instant>,
    lag_ms: Option<u64>,
    mut row: [String; 24],
) {
    if !capture.active.load(Ordering::Relaxed) {
        return;
    }
    let Ok(mut guard) = capture.events.try_lock() else {
        return;
    };
    let Some(ev) = guard.as_mut() else {
        return;
    };
    if when > ev.end {
        return;
    }

    // Fill capture-relative timestamp.
    let t_cap_ms = when
        .duration_since(ev.start)
        .as_millis()
        .min(u64::MAX as u128) as u64;
    row[1] = t_cap_ms.to_string();

    // Optional: associate this row with an edge/scheduled time.
    if let Some(edge_when) = edge_when {
        if edge_when >= ev.start {
            let edge_t_cap_ms = edge_when
                .duration_since(ev.start)
                .as_millis()
                .min(u64::MAX as u128) as u64;
            row[20] = edge_t_cap_ms.to_string();
            if t_cap_ms >= edge_t_cap_ms {
                row[21] = (t_cap_ms - edge_t_cap_ms).to_string();
            } else {
                row[21] = String::new();
            }
        }
    }

    // Always set lag_ms.
    // Prefer explicit lag; else fall back to send_delay_ms; else use the current capture-wide lag.
    if let Some(lag_ms) = lag_ms {
        row[22] = lag_ms.to_string();
    } else if !row[21].is_empty() {
        row[22] = row[21].clone();
    } else {
        row[22] = capture.lag_ms.load(Ordering::Relaxed).to_string();
    }

    // CSV-escape everything; caller provides numeric strings too.
    let line: Vec<String> = row.iter().map(|s| csv_escape(s)).collect();
    ev.csv_lines.push(line.join(","));
}

fn capture_events_push_row(capture: &Arc<CaptureShared>, row: [String; 24]) {
    capture_events_push_row_at(capture, Instant::now(), row)
}

fn parse_kv_tokens<'a>(tokens: impl Iterator<Item = &'a str>) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for tok in tokens {
        if let Some((k, v)) = tok.split_once('=') {
            out.insert(k.to_string(), v.to_string());
        }
    }
    out
}

fn write_manual_dump_files(
    out_dir: &Path,
    ts_ms: u64,
    reason: &str,
    cfg_line: &str,
    dbg_ring: &VecDeque<DebugEvent>,
    rgb_drop_critical: u64,
) -> std::io::Result<(String, String)> {
    std::fs::create_dir_all(out_dir)?;

    let csv_path = out_dir.join(format!("{ts_ms}_{reason}.csv"));
    let txt_path = out_dir.join(format!("{ts_ms}_{reason}.txt"));

    // TXT: metadata + raw lines.
    {
        use std::io::Write;
        let mut f = std::io::BufWriter::new(std::fs::File::create(&txt_path)?);
        writeln!(f, "timestamp_ms={ts_ms}")?;
        writeln!(f, "reason={reason}")?;
        writeln!(f, "len={}", dbg_ring.len())?;
        writeln!(f, "rgb_drop_critical={rgb_drop_critical}")?;
        writeln!(f, "{cfg_line}")?;
        for e in dbg_ring.iter() {
            writeln!(f, "DBG {:>7}ms {}", e.t_ms, e.msg)?;
        }
    }

    // CSV: best-effort structured parse + raw msg.
    {
        use std::io::Write;
        let mut f = std::io::BufWriter::new(std::fs::File::create(&csv_path)?);
        let cols = [
            "t_ms",
            "t_cap_ms",
            "event",
            "kind",
            "device_id",
            "hid",
            "analog",
            "age_ms",
            "peak",
            "last",
            "peak_speed",
            "pressed",
            "playing",
            "ch",
            "note",
            "vel",
            "pressure",
            "delta",
            "thr_down",
            "thr_up",
            "msg",
        ];
        writeln!(f, "{}", cols.join(","))?;

        for e in dbg_ring.iter() {
            let body = e.msg.as_str();
            let mut row: HashMap<&str, String> = HashMap::new();
            row.insert("t_ms", e.t_ms.to_string());
            row.insert("msg", body.to_string());

            if let Some(rest) = body.strip_prefix("EDGE ") {
                // EDGE <kind> dev=... hid=... analog=...
                let mut it = rest.split_whitespace();
                let kind = it.next().unwrap_or("");
                row.insert("event", "EDGE".to_string());
                row.insert("kind", kind.to_string());
                let kv = parse_kv_tokens(it);
                for (k, v) in kv {
                    if let Some(col) = match k.as_str() {
                        "dev" => Some("device_id"),
                        "hid" => Some("hid"),
                        "analog" => Some("analog"),
                        _ => None,
                    } {
                        row.insert(col, v);
                    }
                }
            } else if let Some(rest) = body.strip_prefix("NOTEON_TICK ") {
                row.insert("event", "NOTEON_TICK".to_string());
                let kv = parse_kv_tokens(rest.split_whitespace());
                for (k, v) in kv {
                    let col = match k.as_str() {
                        "dev" => "device_id",
                        "hid" => "hid",
                        "age_ms" => "age_ms",
                        "playing" => "playing",
                        "pressed" => "pressed",
                        "peak" => "peak",
                        "last" => "last",
                        "peak_speed" => "peak_speed",
                        _ => continue,
                    };
                    row.insert(col, v);
                }
            } else if let Some(rest) = body.strip_prefix("TAP_NOTE ") {
                row.insert("event", "TAP_NOTE".to_string());
                let kv = parse_kv_tokens(rest.split_whitespace());
                for (k, v) in kv {
                    let col = match k.as_str() {
                        "dev" => "device_id",
                        "hid" => "hid",
                        "ch" => "ch",
                        "note" => "note",
                        "vel" => "vel",
                        "age_ms" => "age_ms",
                        "peak" => "peak",
                        "peak_speed" => "peak_speed",
                        _ => continue,
                    };
                    row.insert(col, v);
                }
            } else if let Some(rest) = body.strip_prefix("MIDI ") {
                // MIDI noteon ok ... or MIDI noteoff ok ...
                row.insert("event", "MIDI".to_string());
                if rest.starts_with("noteon") {
                    row.insert("kind", "noteon".to_string());
                } else if rest.starts_with("noteoff") {
                    row.insert("kind", "noteoff".to_string());
                }
                let kv = parse_kv_tokens(rest.split_whitespace());
                for (k, v) in kv {
                    let col = match k.as_str() {
                        "dev" => "device_id",
                        "hid" => "hid",
                        "ch" => "ch",
                        "note" => "note",
                        "vel" => "vel",
                        _ => continue,
                    };
                    row.insert(col, v);
                }
            } else if let Some(rest) = body.strip_prefix("POLL rapid_release ") {
                row.insert("event", "POLL".to_string());
                row.insert("kind", "rapid_release".to_string());
                let kv = parse_kv_tokens(rest.split_whitespace());
                for (k, v) in kv {
                    let col = match k.as_str() {
                        "device_id" => "device_id",
                        "hid" => "hid",
                        "peak" => "peak",
                        "analog" => "analog",
                        "delta" => "delta",
                        "thr_down" => "thr_down",
                        "thr_up" => "thr_up",
                        _ => continue,
                    };
                    row.insert(col, v);
                }
            } else if let Some(rest) = body.strip_prefix("SUPPRESS ") {
                row.insert("event", "SUPPRESS".to_string());
                row.insert(
                    "kind",
                    rest.split_whitespace().next().unwrap_or("").to_string(),
                );
                let kv = parse_kv_tokens(rest.split_whitespace());
                for (k, v) in kv {
                    let col = match k.as_str() {
                        "dev" => "device_id",
                        "hid" => "hid",
                        _ => continue,
                    };
                    row.insert(col, v);
                }
            } else {
                row.insert("event", "RAW".to_string());
            }

            let line: Vec<String> = cols
                .iter()
                .map(|c| csv_escape(row.get(*c).map(|s| s.as_str()).unwrap_or("")))
                .collect();
            writeln!(f, "{}", line.join(","))?;
        }
    }

    Ok((
        csv_path.display().to_string(),
        txt_path.display().to_string(),
    ))
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
        refresh_hz: 1000.0,
        press_threshold: 0.75,
        press_threshold_step: 0.05,
        aftertouch_press_threshold: 0.10,
        velocity_peak_track_ms: 6,
        aftershock_ms: 35,
        velocity_max_swing: 1.0,
        aftertouch_delta: 0.01,
        release_delta: 0.12,
        aftertouch_speed_max: 100.0,
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
                cc_analog_hid: None,
                cc_analog_cc: None,
            },
            xenwooting::config::BoardConfig {
                device_id: None,
                wtn_board: 1,
                rotation_deg: 180,
                mirror_cols: false,
                meta_x: 2,
                meta_y: 0,
                cc_analog_hid: None,
                cc_analog_cc: None,
            },
        ],
        layouts: vec![],
        actions: ActionBindings::default_with_sane_keys(),
        rgb: xenwooting::config::RgbConfig::default(),
        control_bar: xenwooting::config::ControlBarConfig::default(),
        hid_overrides: vec![],
        output_dir: None,
        capture_always_on: false,
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

    let output_dir = resolve_output_dir(&cfg)?;

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

    // Per-board analog-CC key: device_id -> (HID code, CC number).
    // Defaults come from BoardConfig::cc_analog(): wtn_board 0 -> CC#4, 1 -> CC#3.
    let mut cc_analog_by_device: HashMap<u64, (HIDCodes, u8)> = HashMap::new();
    for bcfg in cfg.boards.iter() {
        let Some((hid_name, cc)) = bcfg.cc_analog() else {
            continue;
        };
        let Some(dev_id) = bcfg.device_id_u64()? else {
            continue;
        };
        let hid = parse_hid_name(&hid_name)?;
        cc_analog_by_device.insert(dev_id, (hid, cc));
    }

    let control_bar_row = cfg.control_bar.row;
    let mut control_bar_cols_by_hid: HashMap<HIDCodes, Vec<u8>> = HashMap::new();
    for (hid_name, cols) in cfg.control_bar.led_cols_by_hid.iter() {
        let hid = parse_hid_name(hid_name)?;
        control_bar_cols_by_hid.insert(hid, cols.as_vec());
    }

    // Capture skips: control-bar + action keys are not "musical".
    let mut capture_skip_hids: HashSet<HIDCodes> = HashSet::new();
    for h in control_bar_cols_by_hid.keys() {
        capture_skip_hids.insert(h.clone());
    }
    for h in actions_by_hid.keys() {
        capture_skip_hids.insert(h.clone());
    }
    for (h, _) in cc_analog_by_device.values() {
        capture_skip_hids.insert(h.clone());
    }
    // Some keyboards emit arrows on an Fn layer for control-bar keys.
    // Treat these as non-musical as well.
    for h in [
        HIDCodes::ArrowLeft,
        HIDCodes::ArrowDown,
        HIDCodes::ArrowRight,
    ] {
        capture_skip_hids.insert(h);
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

    // Webconfigurator control plane files (tmpfs).
    let rt_dir = runtime_dir();
    let _ = fs::create_dir_all(&rt_dir);
    let preview_enabled_path = env_path_buf(
        "XENWTN_PREVIEW_ENABLED_PATH",
        rt_dir.join("preview.enabled"),
    );
    let preview_wtn_path = env_path_buf("XENWTN_PREVIEW_WTN_PATH", rt_dir.join("preview.wtn"));
    let trainer_mode_path = env_path_buf("XENWTN_TRAINER_MODE_PATH", rt_dir.join("trainer.mode"));
    let guide_path = env_path_buf("XENWTN_GUIDE_PATH", rt_dir.join("guide.json"));
    let highlight_path = env_path_buf("XENWTN_HIGHLIGHT_PATH", rt_dir.join("highlight.txt"));
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
    info!(
        "CFG refresh_hz={:.0} peak_track_ms={} press_threshold={:.3} aftertouch_press_threshold={:.3} release_delta={:.3} rgb_screensaver_timeout_sec={} output_dir={}",
        cfg.refresh_hz,
        cfg.velocity_peak_track_ms,
        cfg.press_threshold,
        cfg.aftertouch_press_threshold,
        cfg.release_delta,
        cfg.rgb.screensaver_timeout_sec,
        output_dir.display()
    );

    let start_ts = Instant::now();
    let mut dbg_ring: VecDeque<DebugEvent> = VecDeque::with_capacity(200);
    let mut rgb_drop_critical: u64 = 0;
    let (dbg_tx, dbg_rx) = crossbeam_channel::bounded::<String>(4096);

    let rgb_enabled = cfg.rgb.enabled && !no_rgb;
    let (rgb_tx, rgb_rx) = crossbeam_channel::bounded::<RgbCmd>(1024);
    if rgb_enabled {
        spawn_rgb_worker(rgb_rx);
    }

    // rgb_send_critical is implemented as a function (see above) to avoid borrow issues.

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

    // When aftertouch is enabled, use a fixed note-on threshold.
    let aftertouch_press_threshold: f32 = cfg.aftertouch_press_threshold.clamp(0.02, 0.98);
    let mut manual_press_threshold: f32 = cfg.press_threshold;

    let mut aftertouch_speed_max: f32 = cfg.aftertouch_speed_max.clamp(1.0, 1000.0);

    // Global run-flag is controlled by signal handler.
    RUNNING.store(true, Ordering::SeqCst);

    let poll_period = Duration::from_secs_f32(1.0 / cfg.refresh_hz.max(1.0));
    // Movement-based release (rapid-trigger). Set release_delta very high to effectively disable.
    // Recommended disable value: >= 1.0.
    let release_delta = cfg.release_delta.clamp(0.0, 10.0);
    let rapid_release_enabled = release_delta > 0.0 && release_delta < 0.95;

    // Poll the SDK from a single thread.
    // In practice this is more reliable than polling the SDK concurrently.
    let (tx, rx) = mpsc::channel::<KeyEdge>();
    // We'll refresh connected device IDs dynamically to support hotplug.
    let threshold_init = if matches!(aftertouch_mode, AftertouchMode::Off) {
        manual_press_threshold
    } else {
        aftertouch_press_threshold
    };
    let press_threshold_bits =
        Arc::new(std::sync::atomic::AtomicU32::new(threshold_init.to_bits()));
    let update_delta = cfg.aftertouch_delta.clamp(0.001, 0.2);
    let verbose = dump_hid || log_edges || log_midi || log_poll;
    let press_threshold_bits_poll = Arc::clone(&press_threshold_bits);
    let configured_device_ids_poll = configured_device_ids.clone();
    let dbg_tx_poll = dbg_tx.clone();

    // Per-device set of keys to sample during capture (bitmask, lock-free reads).
    let mut active_masks_by_device: HashMap<u64, Arc<ActiveMask>> = HashMap::new();
    for id in configured_device_ids.iter() {
        active_masks_by_device.insert(*id, Arc::new(ActiveMask::default()));
    }
    let active_masks_by_device_poll = active_masks_by_device.clone();

    let capture_skip_hids_poll = capture_skip_hids.clone();

    let capture = Arc::new(CaptureShared {
        active: AtomicBool::new(false),
        samples: Mutex::new(None),
        events: Mutex::new(None),
        lock_drops: AtomicU64::new(0),
        lag_ms: AtomicU64::new(0),
    });
    let capture_poll = Arc::clone(&capture);
    std::thread::spawn(move || {
        let active_masks_by_device_poll = active_masks_by_device_poll;
        let capture_skip_hids_poll = capture_skip_hids_poll;
        let mut down_by_device: HashMap<u64, HashSet<HIDCodes>> = HashMap::new();
        let mut last_analog_by_device: HashMap<u64, HashMap<HIDCodes, f32>> = HashMap::new();
        let mut active_by_device: HashMap<u64, HashSet<HIDCodes>> = HashMap::new();
        let mut last_seen_by_device: HashMap<u64, HashMap<HIDCodes, Instant>> = HashMap::new();
        let mut last_code_by_device: HashMap<u64, HashMap<HIDCodes, u8>> = HashMap::new();
        let mut peak_by_device: HashMap<u64, HashMap<HIDCodes, f32>> = HashMap::new();
        let mut startup_suppressed_by_device: HashMap<u64, HashSet<HIDCodes>> = HashMap::new();
        let mut did_init_device: HashSet<u64> = HashSet::new();
        let mut poll_err_since: HashMap<u64, Option<Instant>> = HashMap::new();

        // Edge detection is noisy near the threshold; use hysteresis.
        // Release hysteresis (thr_up = thr_down - hysteresis). Smaller values release sooner but
        // can be more sensitive to jitter near the threshold.
        const RELEASE_HYSTERESIS: f32 = 0.005;
        // Some SDK/plugins may only report a release as "missing from buffer" (or as a one-shot 0.0).
        // Treat a key that's missing for this long as released.
        const MISSING_RELEASE_MS: u64 = 50;
        // If the SDK read errors for this long, fail-safe release all active keys.
        const POLL_ERR_FAILSAFE_MS: u64 = 250;

        // Rate-limited anomaly counters.
        let mut missing_release_count: u64 = 0;
        let mut poll_err_release_count: u64 = 0;
        let mut startup_suppressed_count: u64 = 0;
        let mut last_stats = Instant::now();

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
                                active_by_device.entry(*id).or_insert_with(HashSet::new);
                                last_seen_by_device.entry(*id).or_insert_with(HashMap::new);
                                last_code_by_device.entry(*id).or_insert_with(HashMap::new);
                                peak_by_device.entry(*id).or_insert_with(HashMap::new);
                                startup_suppressed_by_device
                                    .entry(*id)
                                    .or_insert_with(HashSet::new);
                                poll_err_since.entry(*id).or_insert(None);
                            }
                            // Drop removed device maps.
                            down_by_device.retain(|id, _| new_ids.contains(id));
                            last_analog_by_device.retain(|id, _| new_ids.contains(id));
                            active_by_device.retain(|id, _| new_ids.contains(id));
                            last_seen_by_device.retain(|id, _| new_ids.contains(id));
                            last_code_by_device.retain(|id, _| new_ids.contains(id));
                            peak_by_device.retain(|id, _| new_ids.contains(id));
                            startup_suppressed_by_device.retain(|id, _| new_ids.contains(id));
                            poll_err_since.retain(|id, _| new_ids.contains(id));
                            did_init_device.retain(|id| new_ids.contains(id));
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
                let now = Instant::now();
                let data = match sdk::read_full_buffer_device(256, *device_id).0 {
                    Ok(v) => v,
                    Err(e) => {
                        err_count += 1;
                        let since = poll_err_since
                            .get_mut(device_id)
                            .expect("poll_err_since missing device")
                            .get_or_insert(now);
                        if since.elapsed() >= Duration::from_millis(POLL_ERR_FAILSAFE_MS) {
                            let down = down_by_device.get_mut(device_id).unwrap();
                            let active = active_by_device.get_mut(device_id).unwrap();
                            let last_analog = last_analog_by_device.get_mut(device_id).unwrap();
                            let last_seen = last_seen_by_device.get_mut(device_id).unwrap();
                            let peak = peak_by_device.get_mut(device_id).unwrap();
                            let startup_suppressed =
                                startup_suppressed_by_device.get_mut(device_id).unwrap();

                            // Fail-safe release all active keys.
                            let keys: Vec<HIDCodes> = active.iter().cloned().collect();
                            for hid in keys.iter().cloned() {
                                active.remove(&hid);
                                down.remove(&hid);
                                last_analog.remove(&hid);
                                last_seen.remove(&hid);
                                peak.remove(&hid);
                                startup_suppressed.remove(&hid);
                                let _ = tx.send(KeyEdge::Up {
                                    device_id: *device_id,
                                    hid,
                                    analog: 0.0,
                                    ts: Instant::now(),
                                });
                            }
                            poll_err_release_count = poll_err_release_count.saturating_add(1);
                            let _ = dbg_tx_poll.try_send(format!(
                                "poll_err_failsafe_release device_id={} releasing_keys={}",
                                device_id,
                                keys.len()
                            ));
                            // Reset timer so we don't spam releases.
                            *since = now;
                        }
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

                // Successful poll clears error timer.
                if let Some(s) = poll_err_since.get_mut(device_id) {
                    *s = None;
                }

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
                let active = active_by_device.get_mut(device_id).unwrap();
                let last_seen = last_seen_by_device.get_mut(device_id).unwrap();
                let last_code = last_code_by_device.get_mut(device_id).unwrap();
                let peak = peak_by_device.get_mut(device_id).unwrap();
                let startup_suppressed = startup_suppressed_by_device.get_mut(device_id).unwrap();

                // On first successful poll for this device, suppress keys already held so we don't
                // start notes/LEDs mid-press.
                if !did_init_device.contains(device_id) {
                    did_init_device.insert(*device_id);
                    startup_suppressed.clear();
                    for (code_u16, analog) in data.iter() {
                        if *code_u16 > 255 {
                            continue;
                        }
                        let Some(hid) = HIDCodes::from_u8(*code_u16 as u8) else {
                            continue;
                        };
                        if *analog > 0.001 {
                            startup_suppressed.insert(hid);
                        }
                    }
                }

                let mut seen: HashSet<HIDCodes> = HashSet::new();
                for (code_u16, analog) in data.iter() {
                    if *code_u16 > 255 {
                        continue;
                    }
                    let Some(hid) = HIDCodes::from_u8(*code_u16 as u8) else {
                        continue;
                    };

                    seen.insert(hid.clone());
                    last_code.insert(hid.clone(), *code_u16 as u8);
                    last_seen.insert(hid.clone(), now);

                    let press_threshold = f32::from_bits(
                        press_threshold_bits_poll.load(std::sync::atomic::Ordering::Relaxed),
                    )
                    .clamp(0.0, 0.99);

                    // Hysteresis prevents noisy releases from missing the Up edge.
                    let thr_down = press_threshold;
                    let thr_up = (press_threshold - RELEASE_HYSTERESIS).clamp(0.0, 0.99);

                    let was_down = down.contains(&hid);

                    // Track per-press peak for movement-based release (rapid trigger).
                    if was_down {
                        let p = peak.entry(hid.clone()).or_insert(*analog);
                        if *analog > *p {
                            *p = *analog;
                        }
                    }

                    let mut is_down_now = if was_down {
                        *analog > thr_up
                    } else {
                        *analog > thr_down
                    };

                    // Movement-based release: if a pressed key falls by at least release_delta from
                    // its peak since press, treat it as released even if still above thr_up.
                    if was_down && is_down_now {
                        if let Some(&p) = peak.get(&hid) {
                            let delta = (p - *analog).max(0.0);
                            // Avoid triggering for extremely shallow touches.
                            if rapid_release_enabled
                                && p >= (thr_down + 0.02)
                                && delta >= release_delta
                            {
                                is_down_now = false;
                                let _ = dbg_tx_poll.try_send(format!(
                                    "rapid_release device_id={} hid={:?} peak={:.3} analog={:.3} delta={:.3} thr_down={:.3} thr_up={:.3}",
                                    device_id, hid, p, analog, delta, thr_down, thr_up
                                ));
                            }
                        }
                    }

                    if is_down_now && !was_down {
                        down.insert(hid.clone());
                        last_analog.insert(hid.clone(), *analog);
                        peak.insert(hid.clone(), *analog);

                        if startup_suppressed.contains(&hid) {
                            // Don't emit a Down edge until the key has been released once.
                            startup_suppressed_count = startup_suppressed_count.saturating_add(1);
                            let _ = dbg_tx_poll.try_send(format!(
                                "startup_suppressed_down device_id={} hid={:?} analog={:.3}",
                                device_id, hid, analog
                            ));
                            continue;
                        }

                        active.insert(hid.clone());

                        // Capture sampling: start tracking this key's press dynamics immediately.
                        if capture_poll.active.load(Ordering::Relaxed)
                            && !capture_skip_hids_poll.contains(&hid)
                        {
                            if let Some(mask) = active_masks_by_device_poll.get(device_id) {
                                mask.set(*code_u16 as u8, true);
                            }
                        }

                        if verbose {
                            eprintln!(
                                "send DOWN device_id={} hid={:?} analog={:.3} thr={:.3}",
                                device_id, hid, analog, press_threshold
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
                        last_seen.remove(&hid);
                        last_code.remove(&hid);
                        peak.remove(&hid);
                        startup_suppressed.remove(&hid);

                        if capture_poll.active.load(Ordering::Relaxed) {
                            if let Some(mask) = active_masks_by_device_poll.get(device_id) {
                                mask.set(*code_u16 as u8, false);
                            }
                        }

                        // Only emit Up if we previously emitted Down.
                        if active.remove(&hid) {
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
                        }
                    } else if is_down_now && was_down {
                        if active.contains(&hid) {
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

                // Keys can "vanish" from the buffer without a 0.0 report on release.
                // If a key was down and hasn't been seen for a short time, synthesize an Up.
                let missing_timeout = Duration::from_millis(MISSING_RELEASE_MS);
                let down_list: Vec<HIDCodes> = down.iter().cloned().collect();
                for hid in down_list {
                    if seen.contains(&hid) {
                        continue;
                    }
                    let Some(last_ts) = last_seen.get(&hid).copied() else {
                        // No last_seen: treat as missing from now.
                        last_seen.insert(hid.clone(), now);
                        continue;
                    };
                    if now.duration_since(last_ts) < missing_timeout {
                        continue;
                    }

                    down.remove(&hid);
                    last_analog.remove(&hid);
                    last_seen.remove(&hid);
                    let code_opt = last_code.remove(&hid);
                    peak.remove(&hid);
                    startup_suppressed.remove(&hid);

                    if active.remove(&hid) {
                        missing_release_count = missing_release_count.saturating_add(1);
                        let _ = dbg_tx_poll.try_send(format!(
                            "synthetic_up_missing device_id={} hid={:?} missing_ms={}",
                            device_id,
                            hid,
                            now.duration_since(last_ts).as_millis()
                        ));
                        let _ = tx.send(KeyEdge::Up {
                            device_id: *device_id,
                            hid: hid.clone(),
                            analog: 0.0,
                            ts: Instant::now(),
                        });

                        if capture_poll.active.load(Ordering::Relaxed)
                            && !capture_skip_hids_poll.contains(&hid)
                        {
                            if let (Some(mask), Some(code)) =
                                (active_masks_by_device_poll.get(device_id), code_opt)
                            {
                                mask.set(code, false);
                            }
                        }
                    }
                }

                // Optional sample capture (triggered, writes to an in-memory ring buffer).
                // Sample keys that have become active during this capture window.
                if capture_poll.active.load(Ordering::Relaxed) {
                    if let Some(mask) = active_masks_by_device_poll.get(device_id) {
                        let ws = mask.words();
                        if !ws.iter().all(|w| *w == 0) {
                            if let Ok(mut guard) = capture_poll.samples.try_lock() {
                                if let Some(st) = guard.as_mut() {
                                    if now <= st.end {
                                        let t_us = now
                                            .duration_since(st.start)
                                            .as_micros()
                                            .min(u64::MAX as u128)
                                            as u64;

                                        let mut analog_by_code: [f32; 256] = [0.0; 256];
                                        for (code_u16, analog) in data.iter() {
                                            if *code_u16 > 255 {
                                                continue;
                                            }
                                            analog_by_code[*code_u16 as usize] =
                                                (*analog).clamp(0.0, 1.0);
                                        }

                                        for (word_idx, mut w) in ws.into_iter().enumerate() {
                                            while w != 0 {
                                                let bit = w.trailing_zeros() as u8;
                                                let code: u8 = (word_idx as u8) * 64 + bit;
                                                w &= w - 1;

                                                let a = analog_by_code[code as usize];
                                                let analog_q = (a * 65535.0).round() as u16;
                                                capture_write_sample(
                                                    st, t_us, *device_id, code, analog_q,
                                                );
                                            }
                                        }
                                    }
                                }
                            } else {
                                capture_poll.lock_drops.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                    }
                }
            }

            if last_stats.elapsed() >= Duration::from_secs(5) {
                if missing_release_count > 0
                    || poll_err_release_count > 0
                    || startup_suppressed_count > 0
                {
                    log::warn!(
                        "poll anomalies: missing_release={} poll_err_releases={} startup_suppressed={}",
                        missing_release_count,
                        poll_err_release_count,
                        startup_suppressed_count
                    );
                }
                missing_release_count = 0;
                poll_err_release_count = 0;
                startup_suppressed_count = 0;
                last_stats = Instant::now();
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

    // Paint the control-bar LEDs corresponding to the Left/Right Ctrl keys.
    // Used as a persistent mode indicator (capture armed).
    let paint_ctrl_indicator =
        |device_id: u64, rgb_map: &HashMap<u64, u8>, base_rgb: (u8, u8, u8), on: bool| {
            if !rgb_enabled {
                return;
            }
            let Some(&dev_idx) = rgb_map.get(&device_id) else {
                return;
            };
            // Armed capture mode indicator: yellow.
            let rgb = if on { (255u8, 255u8, 0u8) } else { base_rgb };
            for hid in [HIDCodes::LeftCtrl, HIDCodes::RightCtrl] {
                let Some(cols) = control_bar_cols_by_hid.get(&hid) else {
                    continue;
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
            }
        };

    // Paint the control-bar LEDs corresponding to the RightAlt key.
    // Used as a persistent mode indicator (aftertouch mode).
    let paint_aftertouch_mode_indicator =
        |device_id: u64,
         rgb_map: &HashMap<u64, u8>,
         base_rgb: (u8, u8, u8),
         mode: &AftertouchMode| {
            if !rgb_enabled {
                return;
            }
            let Some(&dev_idx) = rgb_map.get(&device_id) else {
                return;
            };
            let Some(cols) = control_bar_cols_by_hid.get(&HIDCodes::RightAlt) else {
                return;
            };
            // Default speed-mapped mode uses the base control-bar color.
            let rgb = match mode {
                AftertouchMode::SpeedMapped => base_rgb,
                AftertouchMode::PeakMapped => (255u8, 255u8, 0u8),
                AftertouchMode::Off => (0u8, 128u8, 255u8),
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
                      octave_hold_by_device: &HashSet<u64>,
                      capture_indicator_devices: &HashSet<u64>,
                      aftertouch_mode: &AftertouchMode| {
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

                // Ctrl capture-mode indicator overrides the base bar color.
                paint_ctrl_indicator(
                    *dev_id,
                    rgb_map,
                    rgb,
                    capture_indicator_devices.contains(dev_id),
                );

                // RightAlt aftertouch-mode indicator overrides the base bar color.
                paint_aftertouch_mode_indicator(*dev_id, rgb_map, rgb, aftertouch_mode);
            }
        }
        if log_edges || log_poll || log_midi {
            eprintln!("paint_base: done");
        }
    };

    let paint_control_bar = |rgb_map: &HashMap<u64, u8>,
                             rgb: (u8, u8, u8),
                             capture_indicator_devices: &HashSet<u64>,
                             octave_hold_by_device: &HashSet<u64>,
                             aftertouch_mode: &AftertouchMode| {
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

            paint_ctrl_indicator(
                *dev_id,
                rgb_map,
                rgb,
                capture_indicator_devices.contains(dev_id),
            );

            paint_spacebar_indicator(
                *dev_id,
                rgb_map,
                rgb,
                octave_hold_by_device.contains(dev_id),
            );

            paint_aftertouch_mode_indicator(*dev_id, rgb_map, rgb, aftertouch_mode);
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
                       octave_hold_by_device: &HashSet<u64>,
                       capture_indicator_devices: &HashSet<u64>| {
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

            paint_ctrl_indicator(
                *dev_id,
                rgb_map,
                bar,
                capture_indicator_devices.contains(dev_id),
            );
        }

        if mode == GuideMode::Active {
            info!(
                "guide: painted active (root_pc={:?}) lit_total={} lit_root={}",
                root_pc, lit_total, lit_root
            );
        }
    };

    let mut capture_indicator_devices: HashSet<u64> = HashSet::new();
    // Debounce the aftertouch-mode toggle key (RightAlt): avoid toggling multiple times due to
    // analog threshold bounce (down/up/down within a single physical press).
    let mut last_aftertouch_toggle_at: HashMap<u64, Instant> = HashMap::new();

    // Optional: start armed capture immediately at startup.
    if cfg.capture_always_on {
        // Prefer a configured board0 device as the trigger.
        let mut board0_devs: Vec<u64> = board_by_device
            .iter()
            .filter_map(|(dev_id, bcfg)| {
                if bcfg.wtn_board == 0 {
                    Some(*dev_id)
                } else {
                    None
                }
            })
            .collect();
        board0_devs.sort();
        if let Some(&trigger_device_id) = board0_devs.first() {
            for d in board0_devs.iter() {
                capture_indicator_devices.insert(*d);
            }

            // Start capture window. Keys are captured dynamically when they become active.
            for m in active_masks_by_device.values() {
                m.clear_all();
            }
            capture.lock_drops.store(0, Ordering::Relaxed);
            let base_ts_ms = unix_time_ms();
            let start = Instant::now();
            let end = start + Duration::from_secs(365 * 24 * 60 * 60);

            let thr_down0 =
                f32::from_bits(press_threshold_bits.load(Ordering::Relaxed)).clamp(0.0, 0.99);
            let thr_up0 = (thr_down0 - CAPTURE_RELEASE_HYSTERESIS).clamp(0.0, 0.99);
            let cfg_line0 = format!(
                "CFG refresh_hz={:.0} poll_ms={:.3} peak_track_ms={} thr={:.3} thr_up={:.3} hyst={:.3} thr_at={:.3} aftertouch_mode={} aftertouch_speed_max={:.2} release_delta={:.3} rapid_release_enabled={} screensaver_timeout_sec={}",
                cfg.refresh_hz,
                poll_period.as_secs_f32() * 1000.0,
                cfg.velocity_peak_track_ms,
                thr_down0,
                thr_up0,
                CAPTURE_RELEASE_HYSTERESIS,
                aftertouch_press_threshold,
                aftertouch_mode.name(),
                aftertouch_speed_max,
                release_delta,
                rapid_release_enabled,
                cfg.rgb.screensaver_timeout_sec,
            );

            if let Ok(mut guard) = capture.samples.lock() {
                *guard = Some(CaptureSamplesState {
                    base_ts_ms,
                    trigger_device_id,
                    start,
                    end,
                    start_unix_ms: base_ts_ms,
                    cfg_line: cfg_line0.clone(),
                    out_dir: output_dir.clone(),
                    ring: vec![0u8; CAPTURE_MAX_BYTES],
                    write_idx: 0,
                    wrapped: false,
                    dropped_samples: 0,
                });
            }
            if let Ok(mut guard) = capture.events.lock() {
                *guard = Some(CaptureEventsState {
                    base_ts_ms,
                    trigger_device_id,
                    start,
                    end,
                    start_unix_ms: base_ts_ms,
                    cfg_line: cfg_line0.clone(),
                    out_dir: output_dir.clone(),
                    csv_lines: Vec::new(),
                    txt_lines: vec![
                        format!("timestamp_ms={}", base_ts_ms),
                        format!("trigger_device_id={}", trigger_device_id),
                        format!("start_unix_ms={}", base_ts_ms),
                        format!("duration_ms_target={}", "always"),
                        cfg_line0.clone(),
                    ],
                });
            }

            capture.active.store(true, Ordering::Relaxed);
            info!(
                "capture_always_on: started (trigger_device_id={})",
                trigger_device_id
            );
        } else {
            warn!("capture_always_on: no configured board0 device found; ignoring");
        }
    }

    paint_base(
        &wtn,
        true,
        &rgb_index_by_device_id,
        trainer_mode,
        &octave_hold_by_device,
        &capture_indicator_devices,
        &aftertouch_mode,
    );

    // Ensure mode indicators paint after initial base.
    if rgb_enabled {
        let base_rgb = control_bar_rgb_for_tm(trainer_mode);
        for (dev_id, _bcfg) in board_by_device.iter() {
            paint_aftertouch_mode_indicator(
                *dev_id,
                &rgb_index_by_device_id,
                base_rgb,
                &aftertouch_mode,
            );
            paint_ctrl_indicator(
                *dev_id,
                &rgb_index_by_device_id,
                base_rgb,
                capture_indicator_devices.contains(dev_id),
            );
        }
    }

    if log_edges || log_poll || log_midi {
        eprintln!("ready: waiting for key edges");
    }

    // Peak-tracked velocity state per key (device_id + HID).
    let mut vel_state: HashMap<(u64, HIDCodes), VelState> = HashMap::new();

    // Pressed/playing pitches per key for the HUD. Updated incrementally on MIDI note on/off.
    let mut hud_pitch_by_key: HashMap<(u64, HIDCodes), i32> = HashMap::new();
    let mut hud_pressed0: HashSet<i32> = HashSet::new();
    let mut hud_pressed1: HashSet<i32> = HashSet::new();

    // Robust note tracking so NoteOff does not depend on vel_state being present.
    let mut note_by_key: HashMap<(u64, HIDCodes), (u8, u8)> = HashMap::new();
    let mut note_on_count: HashMap<(u8, u8), u32> = HashMap::new();
    let mut pending_noteoffs: VecDeque<PendingNoteOff> = VecDeque::new();

    // Last poly-aftertouch pressure sent per key.
    // We use this to avoid spamming duplicate polytouch values; capture logs reflect sends.
    let mut last_aftertouch_sent_by_key: HashMap<(u64, HIDCodes), u8> = HashMap::new();

    // Per-device pitchbend keys (control bar). These use the analog travel of the pitchbend keys
    // to drive channel pitchbend, scoped to channels used by that device's board.
    let mut bend_up_amt_by_device: HashMap<u64, f32> = HashMap::new();
    let mut bend_down_amt_by_device: HashMap<u64, f32> = HashMap::new();
    let mut last_pb_by_dev_ch: HashMap<(u64, u8), i32> = HashMap::new();

    // Per-board analog-CC last-sent dedup map. Uses u8::MAX as "never sent" sentinel.
    let mut last_cc_analog_by_dev_ch: HashMap<(u64, u8), u8> = HashMap::new();

    // Live HUD state published for /wtn/live.
    let live_state_path = env_path_buf("XENWTN_LIVE_STATE_PATH", rt_dir.join("live.json"));
    let live_writer = LiveWriter::new(live_state_path.clone());
    let mut live_pub = LivePublisher::new();

    // RGB screensaver: blank LEDs after inactivity; wake on next key-down.
    // The wake key-down is ignored (no MIDI, no action binding).
    let mut screensaver_active = false;
    let mut last_activity = Instant::now();
    let mut pressed_keys: HashSet<(u64, HIDCodes)> = HashSet::new();
    let mut suppressed_keys: HashSet<(u64, HIDCodes)> = HashSet::new();

    let mut last_leftctrl_down: Option<(u64, Instant)> = None;
    let mut last_rightctrl_down: Option<(u64, Instant)> = None;

    const CAPTURE_DURATION_MS: u64 = 30_000;
    // Fixed-size ring buffer for armed capture samples.
    // Reduced to keep memory footprint reasonable on small systems.
    const CAPTURE_MAX_BYTES: usize = 32 * 1024 * 1024;
    const CAPTURE_RELEASE_HYSTERESIS: f32 = 0.005;

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
                _ => {}
            }
        }
    };

    // Publish an initial live state so /wtn/live can load immediately.
    let vel_name0 = velocity_profiles[velocity_profile_idx].name();
    live_pub.maybe_publish(
        &live_writer,
        &wtn,
        &layouts,
        layout_index,
        press_threshold_bits.as_ref(),
        &aftertouch_mode,
        aftertouch_speed_max,
        &vel_name0,
        octave_shift,
        screensaver_active,
        preview_enabled,
        guide_mode,
        &guide_chord_key,
        guide_root_pc,
        &hud_pressed0,
        &hud_pressed1,
    );

    // Rate-limit lag logging to avoid adding load.
    let mut last_lag_log = Instant::now() - Duration::from_secs(999);

    while RUNNING.load(Ordering::SeqCst) {
        // Publish live HUD only when dirty.
        let live_t0 = Instant::now();
        let vel_name = velocity_profiles[velocity_profile_idx].name();
        live_pub.maybe_publish(
            &live_writer,
            &wtn,
            &layouts,
            layout_index,
            press_threshold_bits.as_ref(),
            &aftertouch_mode,
            aftertouch_speed_max,
            &vel_name,
            octave_shift,
            screensaver_active,
            preview_enabled,
            guide_mode,
            &guide_chord_key,
            guide_root_pc,
            &hud_pressed0,
            &hud_pressed1,
        );
        let live_ms = live_t0.elapsed().as_millis().min(u64::MAX as u128) as u64;
        if live_ms >= 10 {
            info!("LIVE_SLOW ms={}", live_ms);
        }
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
                    &capture_indicator_devices,
                    &aftertouch_mode,
                );
                info!("RGB device count changed; repainted base ({})", now);
            }
        }

        if last_cfg_dev_check.elapsed() >= Duration::from_millis(500) {
            last_cfg_dev_check = Instant::now();
            let t0 = Instant::now();
            let devs_res = sdk::get_connected_devices_info(32).0;
            let ms = t0.elapsed().as_millis().min(u64::MAX as u128) as u64;
            if ms >= 10 {
                info!("CFG_DEV_SLOW ms={}", ms);
            }
            if let Ok(devs) = devs_res {
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
                        &capture_indicator_devices,
                        &aftertouch_mode,
                    );
                    info!("Configured device set changed; repainted base");
                }
            }
        }

        // Reload layout list when config.toml changes (no service restart).
        if last_cfg_check.elapsed() >= Duration::from_millis(500) {
            last_cfg_check = Instant::now();
            let t0 = Instant::now();
            let meta_res = fs::metadata(&cfg_path);
            let meta_ms = t0.elapsed().as_millis().min(u64::MAX as u128) as u64;
            if meta_ms >= 10 {
                info!("CFG_META_SLOW ms={}", meta_ms);
            }
            if let Ok(meta) = meta_res {
                let modified = meta.modified().ok();
                let sig_now = modified.map(|t| (t, meta.len()));
                let changed = sig_now.is_some() && sig_now != cfg_sig;
                if changed {
                    cfg_sig = sig_now;
                    let cur_id = layouts.get(layout_index).map(|l| l.id.clone());
                    let t1 = Instant::now();
                    let cfg_res = load_config();
                    let cfg_ms = t1.elapsed().as_millis().min(u64::MAX as u128) as u64;
                    if cfg_ms >= 10 {
                        info!("CFG_LOAD_SLOW ms={}", cfg_ms);
                    }
                    match cfg_res {
                        Ok(new_cfg) => {
                            live_pub.mark_mode_dirty();
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

                                    let t_set = Instant::now();
                                    let set_res = set_mts_table(&master, &new_layouts[new_index]);
                                    let set_ms =
                                        t_set.elapsed().as_millis().min(u64::MAX as u128) as u64;
                                    if set_ms >= 10 {
                                        info!("CFG_SETMTS_SLOW ms={}", set_ms);
                                    }
                                    if let Err(e) = set_res {
                                        eprintln!("config reload: set_mts_table failed: {e}");
                                        ok = false;
                                    }

                                    let new_wtn_path =
                                        resolve_path(&new_layouts[new_index].wtn_path);
                                    let t_load = Instant::now();
                                    let load_res = Wtn::load(&new_wtn_path);
                                    let load_ms =
                                        t_load.elapsed().as_millis().min(u64::MAX as u128) as u64;
                                    if load_ms >= 10 {
                                        info!(
                                            "CFG_WTNLOAD_SLOW ms={} path={}",
                                            load_ms,
                                            new_wtn_path.display()
                                        );
                                    }
                                    let new_base = match load_res {
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
                                        live_pub.mark_layout_dirty();
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
                                            &capture_indicator_devices,
                                            &aftertouch_mode,
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
        // - If preview is enabled, `wtn` is loaded from the preview file on tmpfs.
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
                                        live_pub.mark_layout_dirty();
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
                            live_pub.mark_mode_dirty();
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
                                &capture_indicator_devices,
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
                            live_pub.mark_mode_dirty();
                            if !screensaver_active {
                                paint_base(
                                    &wtn,
                                    true,
                                    &rgb_index_by_device_id,
                                    trainer_mode,
                                    &octave_hold_by_device,
                                    &capture_indicator_devices,
                                    &aftertouch_mode,
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
                            live_pub.mark_mode_dirty();
                            if !screensaver_active {
                                paint_base(
                                    &wtn,
                                    true,
                                    &rgb_index_by_device_id,
                                    trainer_mode,
                                    &octave_hold_by_device,
                                    &capture_indicator_devices,
                                    &aftertouch_mode,
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
                                live_pub.mark_layout_dirty();
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
                                    &capture_indicator_devices,
                                    &aftertouch_mode,
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
                live_pub.mark_mode_dirty();

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
                    live_pub.mark_layout_dirty();
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
                            &capture_indicator_devices,
                            &aftertouch_mode,
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
                                live_pub.mark_layout_dirty();
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
                                        &capture_indicator_devices,
                                        &aftertouch_mode,
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

        // Receive all pending edges first, then run the timer tick.
        // This prevents firing delayed NoteOn after an Up edge is already queued.
        let mut edges: Vec<KeyEdge> = Vec::new();
        match rx.recv_timeout(Duration::from_millis(1)) {
            Ok(e) => edges.push(e),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(_) => break,
        }
        while let Ok(e) = rx.try_recv() {
            edges.push(e);
        }

        while let Ok(msg) = dbg_rx.try_recv() {
            dbg_push(&mut dbg_ring, &start_ts, format!("POLL {}", msg));
        }

        // Auto-stop capture when duration elapsed.
        if capture.active.load(Ordering::Relaxed) {
            let now = Instant::now();
            let should_stop = if let Ok(guard) = capture.samples.try_lock() {
                guard.as_ref().map(|st| now > st.end).unwrap_or(false)
            } else {
                false
            };
            if should_stop {
                match capture_stop_and_flush(&capture) {
                    Ok(Some((
                        dev_id,
                        capture_csv_path,
                        capture_txt_path,
                        dump_csv_path,
                        dump_txt_path,
                    ))) => {
                        capture_indicator_devices.remove(&dev_id);
                        let base_rgb = match guide_mode {
                            GuideMode::WaitRoot => (0u8, 0u8, 0u8),
                            GuideMode::Active => (0u8, 255u8, 0u8),
                            GuideMode::Off => control_bar_rgb_for_tm(trainer_mode),
                        };
                        paint_ctrl_indicator(dev_id, &rgb_index_by_device_id, base_rgb, false);
                        // Clear capture sampling masks.
                        for m in active_masks_by_device.values() {
                            m.clear_all();
                        }
                        // Note: do not clear last_aftertouch_sent_by_key here; keys may still be held.
                        dbg_push(
                            &mut dbg_ring,
                            &start_ts,
                            format!(
                                "CAPTURE stop (timeout) dev={} capture_csv={} capture_txt={} dump_csv={} dump_txt={}",
                                dev_id, capture_csv_path, capture_txt_path, dump_csv_path, dump_txt_path
                            ),
                        );
                        // Clear ring after saving (fresh for next manual dump).
                        dbg_ring.clear();
                    }
                    Ok(None) => {}
                    Err(e) => {
                        dbg_push(
                            &mut dbg_ring,
                            &start_ts,
                            format!("CAPTURE stop failed: {e}"),
                        );
                    }
                }
            }
        }

        // Lag monitoring (rate-limited): how far behind we are processing edges.
        let mut edge_lag_max_ms: u64 = 0;
        let mut edge_lag_over_50ms: u64 = 0;
        let mut edge_lag_over_200ms: u64 = 0;
        let mut edge_cnt: u64 = 0;
        let mut edge_cnt_updates: u64 = 0;
        let mut edge_cnt_downup: u64 = 0;

        // Capture dump row counters (helps identify lag sources without heavy profiling).
        let mut cap_rows_edge: u64 = 0;
        let mut cap_rows_midi: u64 = 0;
        let mut cap_rows_aftertouch: u64 = 0;

        let thr_down =
            f32::from_bits(press_threshold_bits.load(Ordering::Relaxed)).clamp(0.0, 0.99);
        let thr_up = (thr_down - CAPTURE_RELEASE_HYSTERESIS).clamp(0.0, 0.99);
        let cfg_line = format!(
            "CFG refresh_hz={:.0} poll_ms={:.3} peak_track_ms={} thr={:.3} thr_up={:.3} hyst={:.3} thr_at={:.3} aftertouch_mode={} aftertouch_speed_max={:.2} release_delta={:.3} rapid_release_enabled={} screensaver_timeout_sec={}",
            cfg.refresh_hz,
            poll_period.as_secs_f32() * 1000.0,
            cfg.velocity_peak_track_ms,
            thr_down,
            thr_up,
            CAPTURE_RELEASE_HYSTERESIS,
            aftertouch_press_threshold,
            aftertouch_mode.name(),
            aftertouch_speed_max,
            cfg.release_delta,
            rapid_release_enabled,
            cfg.rgb.screensaver_timeout_sec
        );

        let edge_t0 = Instant::now();
        let loop_now = edge_t0;
        for edge in edges {
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

            // No-op: pitchbend keys are digital (control bar).

            // Track processing lag relative to the edge timestamp.
            edge_cnt += 1;
            if kind == "update" {
                edge_cnt_updates += 1;
            } else {
                edge_cnt_downup += 1;
            }
            if loop_now > ts {
                let lag_ms = loop_now
                    .duration_since(ts)
                    .as_millis()
                    .min(u64::MAX as u128) as u64;
                if lag_ms > edge_lag_max_ms {
                    edge_lag_max_ms = lag_ms;
                }
                if lag_ms >= 50 {
                    edge_lag_over_50ms += 1;
                }
                if lag_ms >= 200 {
                    edge_lag_over_200ms += 1;
                }
            }

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
                    &capture_indicator_devices,
                    &aftertouch_mode,
                );
                screensaver_active = false;
                live_pub.mark_mode_dirty();
                suppressed_keys.insert(key_id.clone());
                dbg_push(
                    &mut dbg_ring,
                    &start_ts,
                    format!(
                        "SUPPRESS add (screensaver_wake) dev={} hid={:?}",
                        device_id, hid
                    ),
                );
                continue;
            }

            // Ctrl chord: press LeftCtrl and RightCtrl within 400ms.
            // Board0: toggle 30s capture (no MIDI ping).
            // Board1: debug ring dump + MIDI ping.
            if kind == "down" && (hid == HIDCodes::LeftCtrl || hid == HIDCodes::RightCtrl) {
                let now = Instant::now();
                let mut chord = false;
                if hid == HIDCodes::LeftCtrl {
                    last_leftctrl_down = Some((device_id, now));
                    if let Some((dev2, t)) = last_rightctrl_down {
                        chord = dev2 == device_id
                            && now.duration_since(t) <= Duration::from_millis(400);
                    }
                } else {
                    last_rightctrl_down = Some((device_id, now));
                    if let Some((dev2, t)) = last_leftctrl_down {
                        chord = dev2 == device_id
                            && now.duration_since(t) <= Duration::from_millis(400);
                    }
                }

                if chord {
                    let board = board_by_device
                        .get(&device_id)
                        .map(|b| b.wtn_board)
                        .unwrap_or(1u8);

                    if board == 0u8 {
                        if capture.active.load(Ordering::Relaxed) {
                            // Disarm capture (do not write files).
                            let had = capture_stop_discard(&capture);
                            for m in active_masks_by_device.values() {
                                m.clear_all();
                            }
                            // Note: do not clear last_aftertouch_sent_by_key here; keys may still be held.

                            let base_rgb = match guide_mode {
                                GuideMode::WaitRoot => (0u8, 0u8, 0u8),
                                GuideMode::Active => (0u8, 255u8, 0u8),
                                GuideMode::Off => control_bar_rgb_for_tm(trainer_mode),
                            };
                            for (dev_id, bcfg) in board_by_device.iter() {
                                if bcfg.wtn_board != 0 {
                                    continue;
                                }
                                capture_indicator_devices.remove(dev_id);
                                paint_ctrl_indicator(
                                    *dev_id,
                                    &rgb_index_by_device_id,
                                    base_rgb,
                                    false,
                                );
                            }
                            dbg_push(
                                &mut dbg_ring,
                                &start_ts,
                                format!(
                                    "CAPTURE disarm (manual) dev={} had_state={}",
                                    device_id, had
                                ),
                            );
                        } else {
                            // Start capture window. Keys are captured dynamically when they become active.
                            for m in active_masks_by_device.values() {
                                m.clear_all();
                            }
                            capture.lock_drops.store(0, Ordering::Relaxed);
                            let base_ts_ms = unix_time_ms();
                            let start = Instant::now();
                            let end = if cfg.capture_always_on {
                                start + Duration::from_secs(365 * 24 * 60 * 60)
                            } else {
                                start + Duration::from_millis(CAPTURE_DURATION_MS)
                            };

                            if let Ok(mut guard) = capture.samples.lock() {
                                *guard = Some(CaptureSamplesState {
                                    base_ts_ms,
                                    trigger_device_id: device_id,
                                    start,
                                    end,
                                    start_unix_ms: base_ts_ms,
                                    cfg_line: cfg_line.clone(),
                                    out_dir: output_dir.clone(),
                                    ring: vec![0u8; CAPTURE_MAX_BYTES],
                                    write_idx: 0,
                                    wrapped: false,
                                    dropped_samples: 0,
                                });
                            }
                            if let Ok(mut guard) = capture.events.lock() {
                                *guard = Some(CaptureEventsState {
                                    base_ts_ms,
                                    trigger_device_id: device_id,
                                    start,
                                    end,
                                    start_unix_ms: base_ts_ms,
                                    cfg_line: cfg_line.clone(),
                                    out_dir: output_dir.clone(),
                                    csv_lines: Vec::new(),
                                    txt_lines: vec![
                                        format!("timestamp_ms={}", base_ts_ms),
                                        format!("trigger_device_id={}", device_id),
                                        format!("start_unix_ms={}", base_ts_ms),
                                        if cfg.capture_always_on {
                                            format!("duration_ms_target={}", "always")
                                        } else {
                                            format!("duration_ms_target={}", CAPTURE_DURATION_MS)
                                        },
                                        cfg_line.clone(),
                                    ],
                                });
                            }

                            capture.active.store(true, Ordering::Relaxed);
                            // Note: do not clear last_aftertouch_sent_by_key here; keys may still be held.
                            // Capture indicator applies to board0 control bar.
                            for (dev_id, bcfg) in board_by_device.iter() {
                                if bcfg.wtn_board == 0 {
                                    capture_indicator_devices.insert(*dev_id);
                                }
                            }
                            let base_rgb = match guide_mode {
                                GuideMode::WaitRoot => (0u8, 0u8, 0u8),
                                GuideMode::Active => (0u8, 255u8, 0u8),
                                GuideMode::Off => control_bar_rgb_for_tm(trainer_mode),
                            };
                            for (dev_id, bcfg) in board_by_device.iter() {
                                if bcfg.wtn_board != 0 {
                                    continue;
                                }
                                paint_ctrl_indicator(
                                    *dev_id,
                                    &rgb_index_by_device_id,
                                    base_rgb,
                                    true,
                                );
                            }
                            dbg_push(
                                &mut dbg_ring,
                                &start_ts,
                                format!(
                                    "CAPTURE start dev={} dur_ms={} base_ts_ms={} ring_bytes={}",
                                    device_id, CAPTURE_DURATION_MS, base_ts_ms, CAPTURE_MAX_BYTES
                                ),
                            );
                        }
                    } else {
                        // Dump chord.
                        // If capture is active, stop+flush the capture pair (capture+dump) and do NOT create a
                        // separate standalone dump with a new timestamp.
                        if capture.active.load(Ordering::Relaxed) {
                            match capture_stop_and_flush(&capture) {
                                Ok(Some((
                                    dev_id,
                                    capture_csv_path,
                                    capture_txt_path,
                                    dump_csv_path,
                                    dump_txt_path,
                                ))) => {
                                    if !cfg.capture_always_on {
                                        capture_indicator_devices.remove(&dev_id);
                                        let base_rgb = match guide_mode {
                                            GuideMode::WaitRoot => (0u8, 0u8, 0u8),
                                            GuideMode::Active => (0u8, 255u8, 0u8),
                                            GuideMode::Off => control_bar_rgb_for_tm(trainer_mode),
                                        };
                                        paint_ctrl_indicator(
                                            dev_id,
                                            &rgb_index_by_device_id,
                                            base_rgb,
                                            false,
                                        );
                                    }
                                    for m in active_masks_by_device.values() {
                                        m.clear_all();
                                    }
                                    // Note: do not clear last_aftertouch_sent_by_key here; keys may still be held.
                                    dbg_push(
                                        &mut dbg_ring,
                                        &start_ts,
                                        format!(
                                            "CAPTURE stop (dump_chord) dev={} capture_csv={} capture_txt={} dump_csv={} dump_txt={}",
                                            dev_id, capture_csv_path, capture_txt_path, dump_csv_path, dump_txt_path
                                        ),
                                    );
                                    dbg_ring.clear();

                                    // In always-on mode, immediately restart capture so recording continues.
                                    if cfg.capture_always_on {
                                        for m in active_masks_by_device.values() {
                                            m.clear_all();
                                        }
                                        capture.lock_drops.store(0, Ordering::Relaxed);
                                        let base_ts_ms = unix_time_ms();
                                        let start = Instant::now();
                                        let end = start + Duration::from_secs(365 * 24 * 60 * 60);

                                        if let Ok(mut guard) = capture.samples.lock() {
                                            *guard = Some(CaptureSamplesState {
                                                base_ts_ms,
                                                trigger_device_id: dev_id,
                                                start,
                                                end,
                                                start_unix_ms: base_ts_ms,
                                                cfg_line: cfg_line.clone(),
                                                out_dir: output_dir.clone(),
                                                ring: vec![0u8; CAPTURE_MAX_BYTES],
                                                write_idx: 0,
                                                wrapped: false,
                                                dropped_samples: 0,
                                            });
                                        }
                                        if let Ok(mut guard) = capture.events.lock() {
                                            *guard = Some(CaptureEventsState {
                                                base_ts_ms,
                                                trigger_device_id: dev_id,
                                                start,
                                                end,
                                                start_unix_ms: base_ts_ms,
                                                cfg_line: cfg_line.clone(),
                                                out_dir: output_dir.clone(),
                                                csv_lines: Vec::new(),
                                                txt_lines: vec![
                                                    format!("timestamp_ms={}", base_ts_ms),
                                                    format!("trigger_device_id={}", dev_id),
                                                    format!("start_unix_ms={}", base_ts_ms),
                                                    format!("duration_ms_target={}", "always"),
                                                    cfg_line.clone(),
                                                ],
                                            });
                                        }
                                        capture.active.store(true, Ordering::Relaxed);
                                        capture_indicator_devices.insert(dev_id);
                                        let base_rgb = match guide_mode {
                                            GuideMode::WaitRoot => (0u8, 0u8, 0u8),
                                            GuideMode::Active => (0u8, 255u8, 0u8),
                                            GuideMode::Off => control_bar_rgb_for_tm(trainer_mode),
                                        };
                                        paint_ctrl_indicator(
                                            dev_id,
                                            &rgb_index_by_device_id,
                                            base_rgb,
                                            true,
                                        );
                                        dbg_push(
                                            &mut dbg_ring,
                                            &start_ts,
                                            format!(
                                                "CAPTURE restart (always_on) dev={} base_ts_ms={} ring_bytes={}",
                                                dev_id, base_ts_ms, CAPTURE_MAX_BYTES
                                            ),
                                        );
                                    }
                                }
                                Ok(None) => {}
                                Err(e) => {
                                    dbg_push(
                                        &mut dbg_ring,
                                        &start_ts,
                                        format!("CAPTURE stop failed: {e}"),
                                    );
                                }
                            }

                            schedule_midi_ping(
                                &mut midi_out,
                                &mut note_on_count,
                                &mut pending_noteoffs,
                            );
                        } else {
                            let ts_ms = unix_time_ms();
                            match write_manual_dump_files(
                                &output_dir,
                                ts_ms,
                                "dump",
                                &cfg_line,
                                &dbg_ring,
                                rgb_drop_critical,
                            ) {
                                Ok((csv_path, txt_path)) => {
                                    dbg_ring.clear();
                                    info!("MANUAL_DUMP saved csv={} txt={}", csv_path, txt_path);
                                }
                                Err(e) => {
                                    warn!("manual dump save failed: {e}");
                                }
                            }
                            schedule_midi_ping(
                                &mut midi_out,
                                &mut note_on_count,
                                &mut pending_noteoffs,
                            );
                        }
                    }

                    continue;
                }
            }

            // Suppressed keys do nothing until released.
            if suppressed_keys.contains(&key_id) {
                dbg_push(
                    &mut dbg_ring,
                    &start_ts,
                    format!("SUPPRESS hit dev={} hid={:?} kind={}", device_id, hid, kind),
                );
                if kind == "up" {
                    suppressed_keys.remove(&key_id);
                    dbg_push(
                        &mut dbg_ring,
                        &start_ts,
                        format!("SUPPRESS remove dev={} hid={:?}", device_id, hid),
                    );
                }
                continue;
            }

            let Some(bcfg) = board_by_device.get(&device_id) else {
                continue;
            };
            let wtn_board = bcfg.wtn_board;
            let rotation = bcfg.rotation_deg;

            // Some Fn-layer keys emit arrow HID codes; treat those as control-bar keys and map
            // their LED flash to the right-side control bar keys.
            let (is_control_bar, control_bar_led_hid, control_bar_flash_rgb) = match hid {
                HIDCodes::ArrowLeft => (true, Some(HIDCodes::RightAlt), Some((0, 255, 255))),
                HIDCodes::ArrowDown => (true, Some(HIDCodes::ContextMenu), Some((0, 255, 255))),
                HIDCodes::ArrowRight => (true, Some(HIDCodes::RightCtrl), Some((0, 255, 255))),
                _ => {
                    if control_bar_cols_by_hid.contains_key(&hid) {
                        (true, Some(hid.clone()), None)
                    } else {
                        (false, None, None)
                    }
                }
            };

            // Pitchbend keys:
            // LeftCtrl = bend up, LeftAlt = bend down.
            // Use analog position (including update edges) for continuous bend.
            if hid == HIDCodes::LeftCtrl || hid == HIDCodes::LeftAlt {
                let is_up = hid == HIDCodes::LeftCtrl;
                if kind == "down" || kind == "update" {
                    let a = analog.clamp(0.0, 1.0);
                    if is_up {
                        bend_up_amt_by_device.insert(device_id, a);
                    } else {
                        bend_down_amt_by_device.insert(device_id, a);
                    }
                } else if kind == "up" {
                    if is_up {
                        bend_up_amt_by_device.remove(&device_id);
                    } else {
                        bend_down_amt_by_device.remove(&device_id);
                    }
                }

                // Apply bend to all channels used by this board.
                // This assumes boards use disjoint MIDI channels if you want per-board isolation.
                let up_amt = bend_up_amt_by_device
                    .get(&device_id)
                    .copied()
                    .unwrap_or(0.0);
                let down_amt = bend_down_amt_by_device
                    .get(&device_id)
                    .copied()
                    .unwrap_or(0.0);
                let bend = bend_from_amounts(up_amt, down_amt);

                let hold_i16 = if octave_hold_by_device.contains(&device_id) {
                    1i16
                } else {
                    0i16
                };
                let mut chans: HashSet<u8> = HashSet::new();
                for idx in 0..(4u8 * 14u8) {
                    if let Some(cell) = wtn.cell(wtn_board, idx as usize) {
                        let base_ch = cell.chan_1based.saturating_sub(1);
                        let shifted = (base_ch as i16) + (octave_shift as i16) + hold_i16;
                        chans.insert(shifted.clamp(0, 15) as u8);
                    }
                }

                for ch in chans {
                    let k = (device_id, ch);
                    let last = last_pb_by_dev_ch.get(&k).copied().unwrap_or(i32::MIN);
                    if last != bend {
                        let _ = midi_out.send_pitchbend(ch, bend);
                        last_pb_by_dev_ch.insert(k, bend);
                    }
                }
            }

            // Per-board analog CC from a control-bar key (e.g., LeftMeta -> CC#4 on board0, CC#3 on board1).
            if let Some((cc_hid, cc_num)) = cc_analog_by_device.get(&device_id) {
                if &hid == cc_hid {
                    let hold_i16 = if octave_hold_by_device.contains(&device_id) {
                        1i16
                    } else {
                        0i16
                    };
                    let mut chans: HashSet<u8> = HashSet::new();
                    for idx in 0..(4u8 * 14u8) {
                        if let Some(cell) = wtn.cell(wtn_board, idx as usize) {
                            let base_ch = cell.chan_1based.saturating_sub(1);
                            let shifted = (base_ch as i16) + (octave_shift as i16) + hold_i16;
                            chans.insert(shifted.clamp(0, 15) as u8);
                        }
                    }

                    let value: u8 = if kind == "up" {
                        0
                    } else {
                        (analog.clamp(0.0, 1.0) * 127.0).round() as u8
                    };

                    for ch in &chans {
                        let k = (device_id, *ch);
                        let last = last_cc_analog_by_dev_ch
                            .get(&k)
                            .copied()
                            .unwrap_or(u8::MAX);
                        if last != value {
                            let _ = midi_out.send_cc(*ch, *cc_num as u32, value);
                            last_cc_analog_by_dev_ch.insert(k, value);
                        }
                    }

                    if kind == "up" {
                        for ch in 0u8..16u8 {
                            last_cc_analog_by_dev_ch.remove(&(device_id, ch));
                        }
                    }
                }
            }

            // Runtime press threshold can be adjusted with Fn-layer arrow keys.
            // These keys never generate notes.
            if kind == "down" {
                match hid {
                    HIDCodes::ArrowLeft => {
                        if matches!(aftertouch_mode, AftertouchMode::Off) {
                            manual_press_threshold = (manual_press_threshold
                                - cfg.press_threshold_step)
                                .clamp(0.02, 0.98);
                            press_threshold_bits
                                .store(manual_press_threshold.to_bits(), Ordering::Relaxed);
                            info!("press_threshold now {:.3}", manual_press_threshold);
                        } else {
                            aftertouch_speed_max = (aftertouch_speed_max
                                - cfg.aftertouch_speed_step)
                                .clamp(1.0, 1000.0);
                            info!("aftertouch_speed_max now {:.2}", aftertouch_speed_max);
                        }
                        live_pub.mark_mode_dirty();
                        // Fall through to control-bar LED feedback.
                    }
                    HIDCodes::ArrowRight => {
                        if matches!(aftertouch_mode, AftertouchMode::Off) {
                            manual_press_threshold = (manual_press_threshold
                                + cfg.press_threshold_step)
                                .clamp(0.02, 0.98);
                            press_threshold_bits
                                .store(manual_press_threshold.to_bits(), Ordering::Relaxed);
                            info!("press_threshold now {:.3}", manual_press_threshold);
                        } else {
                            aftertouch_speed_max = (aftertouch_speed_max
                                + cfg.aftertouch_speed_step)
                                .clamp(1.0, 1000.0);
                            info!("aftertouch_speed_max now {:.2}", aftertouch_speed_max);
                        }
                        live_pub.mark_mode_dirty();
                        // Fall through to control-bar LED feedback.
                    }
                    HIDCodes::ArrowDown => {
                        velocity_profile_idx = (velocity_profile_idx + 1) % velocity_profiles.len();
                        info!(
                            "velocity_profile now {}",
                            velocity_profiles[velocity_profile_idx].name()
                        );
                        live_pub.mark_mode_dirty();
                        // Fall through to control-bar LED feedback.
                    }
                    HIDCodes::RightAlt => {
                        // Debounce: ignore rapid re-triggers (common due to analog threshold bounce).
                        let now = Instant::now();
                        if let Some(prev) = last_aftertouch_toggle_at.get(&device_id) {
                            if now.duration_since(*prev) < Duration::from_millis(160) {
                                dbg_push(
                                    &mut dbg_ring,
                                    &start_ts,
                                    format!(
                                        "RightAlt debounce dev={} dt_ms={} kind={}",
                                        device_id,
                                        now.duration_since(*prev).as_millis(),
                                        kind
                                    ),
                                );
                                continue;
                            }
                        }
                        last_aftertouch_toggle_at.insert(device_id, now);

                        // Flash the mode key (white) on keypress.
                        if rgb_enabled {
                            if let Some(&dev_idx) = rgb_index_by_device_id.get(&device_id) {
                                if let Some(cols) = control_bar_cols_by_hid.get(&HIDCodes::RightAlt)
                                {
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
                                }
                            }
                        }

                        let prev_mode = aftertouch_mode.name();
                        aftertouch_mode = match aftertouch_mode {
                            AftertouchMode::SpeedMapped => AftertouchMode::PeakMapped,
                            AftertouchMode::PeakMapped => AftertouchMode::Off,
                            AftertouchMode::Off => AftertouchMode::SpeedMapped,
                        };
                        live_pub.mark_mode_dirty();
                        if matches!(aftertouch_mode, AftertouchMode::Off) {
                            press_threshold_bits
                                .store(manual_press_threshold.to_bits(), Ordering::Relaxed);
                        } else {
                            press_threshold_bits
                                .store(aftertouch_press_threshold.to_bits(), Ordering::Relaxed);
                        }
                        info!(
                            "aftertouch_mode {} -> {} (thr {:.2})",
                            prev_mode,
                            aftertouch_mode.name(),
                            f32::from_bits(press_threshold_bits.load(Ordering::Relaxed))
                        );

                        // Update mode indicator color on both boards.
                        if rgb_enabled {
                            let base_rgb = control_bar_rgb_for_tm(trainer_mode);
                            for (dev_id, _bcfg) in board_by_device.iter() {
                                if *dev_id == device_id {
                                    // Keep the keypress flash on this device.
                                    continue;
                                }
                                paint_aftertouch_mode_indicator(
                                    *dev_id,
                                    &rgb_index_by_device_id,
                                    base_rgb,
                                    &aftertouch_mode,
                                );
                            }
                        }
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
            if let Some(led_hid) = control_bar_led_hid.as_ref() {
                if let Some(cs) = control_bar_cols_by_hid.get(led_hid) {
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
                let Some(led_hid) = control_bar_led_hid.as_ref() else {
                    continue;
                };
                if let Some(cols) = control_bar_cols_by_hid.get(led_hid) {
                    if kind == "down" {
                        let flash_rgb = control_bar_flash_rgb.unwrap_or(highlight_rgb);
                        for &lc in cols {
                            try_send_drop(
                                &rgb_tx,
                                RgbCmd::SetKey(RgbKey {
                                    device_index: dev_idx,
                                    row: control_bar_row,
                                    col: lc,
                                    rgb: flash_rgb,
                                }),
                            );
                        }

                        // If capture mode is active, keep Ctrl indicator solid white.
                        if (hid == HIDCodes::LeftCtrl || hid == HIDCodes::RightCtrl)
                            && capture_indicator_devices.contains(&device_id)
                        {
                            let base_rgb = control_bar_rgb_for_tm(trainer_mode);
                            paint_ctrl_indicator(
                                device_id,
                                &rgb_index_by_device_id,
                                base_rgb,
                                true,
                            );
                        }
                    } else if kind == "up" {
                        // Restore the control bar to the current mode colors.
                        // Space's indicator overrides to white when octave-hold is enabled.
                        let base_rgb = control_bar_rgb_for_tm(trainer_mode);
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

                        // Persistent control-bar overlays.
                        paint_spacebar_indicator(
                            device_id,
                            &rgb_index_by_device_id,
                            base_rgb,
                            octave_hold_by_device.contains(&device_id),
                        );
                        paint_ctrl_indicator(
                            device_id,
                            &rgb_index_by_device_id,
                            base_rgb,
                            capture_indicator_devices.contains(&device_id),
                        );
                        paint_aftertouch_mode_indicator(
                            device_id,
                            &rgb_index_by_device_id,
                            base_rgb,
                            &aftertouch_mode,
                        );
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
                                paint_control_bar(
                                    &rgb_index_by_device_id,
                                    (255, 0, 0),
                                    &capture_indicator_devices,
                                    &octave_hold_by_device,
                                    &aftertouch_mode,
                                );
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
                            base_wtn_mtime =
                                fs::metadata(&wtn_path).and_then(|m| m.modified()).ok();
                            wtn_mtime = base_wtn_mtime;
                            if !preview_enabled {
                                wtn = base_wtn.clone();
                                live_pub.mark_layout_dirty();
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
                                    &capture_indicator_devices,
                                    &aftertouch_mode,
                                );
                            }
                            live_pub.mark_mode_dirty();
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
                                paint_control_bar(
                                    &rgb_index_by_device_id,
                                    (255, 0, 0),
                                    &capture_indicator_devices,
                                    &octave_hold_by_device,
                                    &aftertouch_mode,
                                );
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
                            base_wtn_mtime =
                                fs::metadata(&wtn_path).and_then(|m| m.modified()).ok();
                            wtn_mtime = base_wtn_mtime;
                            if !preview_enabled {
                                wtn = base_wtn.clone();
                                live_pub.mark_layout_dirty();
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
                                    &capture_indicator_devices,
                                    &aftertouch_mode,
                                );
                            }
                            live_pub.mark_mode_dirty();
                        }
                        "octave_up" => {
                            octave_shift = (octave_shift + 1).min(15);
                            info!("octave_shift now {}", octave_shift);
                            live_pub.mark_mode_dirty();
                        }
                        "octave_down" => {
                            octave_shift = (octave_shift - 1).max(-15);
                            info!("octave_shift now {}", octave_shift);
                            live_pub.mark_mode_dirty();
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
                            &capture_indicator_devices,
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
                        live_pub.mark_mode_dirty();
                    }

                    // Maintain per-key velocity state.
                    if kind == "down" {
                        dbg_push(
                            &mut dbg_ring,
                            &start_ts,
                            format!(
                                "EDGE down dev={} hid={:?} analog={:.3}",
                                device_id, hid, analog
                            ),
                        );

                        // Capture-session dump row (timebase-synced).
                        if capture.active.load(Ordering::Relaxed) {
                            let t_ms = start_ts.elapsed().as_millis().min(u64::MAX as u128) as u64;
                            let thr_down_now =
                                f32::from_bits(press_threshold_bits.load(Ordering::Relaxed))
                                    .clamp(0.0, 0.99);
                            let thr_up_now =
                                (thr_down_now - CAPTURE_RELEASE_HYSTERESIS).clamp(0.0, 0.99);
                            let lag_ms = if loop_now > ts {
                                loop_now
                                    .duration_since(ts)
                                    .as_millis()
                                    .min(u64::MAX as u128) as u64
                            } else {
                                0
                            };
                            capture_events_push_row_meta_at(
                                &capture,
                                ts,
                                None,
                                Some(lag_ms),
                                [
                                    t_ms.to_string(),
                                    String::new(),
                                    "EDGE".to_string(),
                                    "down".to_string(),
                                    device_id.to_string(),
                                    format!("{hid:?}"),
                                    format!("{analog:.6}"),
                                    String::new(),
                                    String::new(),
                                    String::new(),
                                    String::new(),
                                    String::new(),
                                    String::new(),
                                    String::new(),
                                    String::new(),
                                    String::new(),
                                    String::new(),
                                    String::new(),
                                    format!("{thr_down_now:.6}"),
                                    format!("{thr_up_now:.6}"),
                                    String::new(),
                                    String::new(),
                                    String::new(),
                                    String::new(),
                                ],
                            );
                            cap_rows_edge += 1;
                        }
                        let already_playing = note_by_key.contains_key(&key_id);

                        let led = if rgb_enabled {
                            if let Some(&dev_idx) = rgb_index_by_device_id.get(&device_id) {
                                rgb_send_critical(
                                    &rgb_tx,
                                    &mut dbg_ring,
                                    &start_ts,
                                    &mut rgb_drop_critical,
                                    &cfg_line,
                                    RgbCmd::SetKey(RgbKey {
                                        device_index: dev_idx,
                                        row: loc.led_row,
                                        col: loc.led_col,
                                        rgb: highlight_rgb,
                                    }),
                                    format!(
                                        "highlight dev={} hid={:?} r{}c{}",
                                        device_id, hid, loc.led_row, loc.led_col
                                    ),
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
                                out_ch,
                                note,
                                playing: already_playing,
                                led,
                            },
                        );

                        // NoteOn pitchbend is applied in the NoteOn tick (below).
                    } else if kind == "update" {
                        if log_edges {
                            dbg_push(
                                &mut dbg_ring,
                                &start_ts,
                                format!(
                                    "EDGE update dev={} hid={:?} analog={:.3}",
                                    device_id, hid, analog
                                ),
                            );
                        }

                        if capture.active.load(Ordering::Relaxed) {
                            let t_ms = start_ts.elapsed().as_millis().min(u64::MAX as u128) as u64;
                            let lag_ms = if loop_now > ts {
                                loop_now
                                    .duration_since(ts)
                                    .as_millis()
                                    .min(u64::MAX as u128) as u64
                            } else {
                                0
                            };
                            capture_events_push_row_meta_at(
                                &capture,
                                ts,
                                None,
                                Some(lag_ms),
                                [
                                    t_ms.to_string(),
                                    String::new(),
                                    "EDGE".to_string(),
                                    "update".to_string(),
                                    device_id.to_string(),
                                    format!("{hid:?}"),
                                    format!("{analog:.6}"),
                                    String::new(),
                                    String::new(),
                                    String::new(),
                                    String::new(),
                                    String::new(),
                                    String::new(),
                                    String::new(),
                                    String::new(),
                                    String::new(),
                                    String::new(),
                                    String::new(),
                                    String::new(),
                                    String::new(),
                                    String::new(),
                                    String::new(),
                                    String::new(),
                                    String::new(),
                                ],
                            );
                            cap_rows_edge += 1;
                        }

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
                            }
                        }
                    } else if kind == "up" {
                        dbg_push(
                            &mut dbg_ring,
                            &start_ts,
                            format!(
                                "EDGE up   dev={} hid={:?} analog={:.3}",
                                device_id, hid, analog
                            ),
                        );

                        if capture.active.load(Ordering::Relaxed) {
                            let t_ms = start_ts.elapsed().as_millis().min(u64::MAX as u128) as u64;
                            let thr_down_now =
                                f32::from_bits(press_threshold_bits.load(Ordering::Relaxed))
                                    .clamp(0.0, 0.99);
                            let thr_up_now =
                                (thr_down_now - CAPTURE_RELEASE_HYSTERESIS).clamp(0.0, 0.99);
                            let lag_ms = if loop_now > ts {
                                loop_now
                                    .duration_since(ts)
                                    .as_millis()
                                    .min(u64::MAX as u128) as u64
                            } else {
                                0
                            };
                            capture_events_push_row_meta_at(
                                &capture,
                                ts,
                                None,
                                Some(lag_ms),
                                [
                                    t_ms.to_string(),
                                    String::new(),
                                    "EDGE".to_string(),
                                    "up".to_string(),
                                    device_id.to_string(),
                                    format!("{hid:?}"),
                                    format!("{analog:.6}"),
                                    String::new(),
                                    String::new(),
                                    String::new(),
                                    String::new(),
                                    String::new(),
                                    String::new(),
                                    String::new(),
                                    String::new(),
                                    String::new(),
                                    String::new(),
                                    String::new(),
                                    format!("{thr_down_now:.6}"),
                                    format!("{thr_up_now:.6}"),
                                    String::new(),
                                    String::new(),
                                    String::new(),
                                    String::new(),
                                ],
                            );
                            cap_rows_edge += 1;
                        }
                        let removed = vel_state.remove(&key_id);

                        // Ensure next press can emit polytouch again even if it repeats the same value.
                        last_aftertouch_sent_by_key.remove(&key_id);

                        // HUD: remove pitch if this key had an active note.
                        if let Some(pitch) = hud_pitch_by_key.remove(&key_id) {
                            if wtn_board == 0 {
                                hud_pressed0.remove(&pitch);
                            } else if wtn_board == 1 {
                                hud_pressed1.remove(&pitch);
                            }
                            live_pub.mark_pressed_dirty();
                        }

                        // Restore LED immediately.
                        if let Some(VelState::Tracking { led: Some(led), .. }) = &removed {
                            rgb_send_critical(
                                &rgb_tx,
                                &mut dbg_ring,
                                &start_ts,
                                &mut rgb_drop_critical,
                                &cfg_line,
                                RgbCmd::SetKey(RgbKey {
                                    device_index: led.dev_idx,
                                    row: led.row,
                                    col: led.col,
                                    rgb: led.base_rgb,
                                }),
                                format!(
                                    "restore dev={} hid={:?} r{}c{}",
                                    device_id, hid, led.row, led.col
                                ),
                            );
                        } else if rgb_enabled {
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

                        // NoteOff: prefer the robust mapping; fall back to vel_state if needed.
                        let mut sent = false;
                        if let Some((ch, note0)) = note_by_key.remove(&key_id) {
                            let cnt = note_on_count.entry((ch, note0)).or_insert(1);
                            if *cnt > 1 {
                                dbg_push(
                                &mut dbg_ring,
                                &start_ts,
                                format!(
                                    "MIDI noteoff skip (refcount) dev={} hid={:?} ch={} note={} cnt={}",
                                    device_id, hid, ch, note0, *cnt
                                ),
                            );
                                *cnt -= 1;
                            } else {
                                note_on_count.remove(&(ch, note0));

                                match midi_out.send_note(false, ch, note0, 0) {
                                    Ok(()) => {
                                        dbg_push(
                                            &mut dbg_ring,
                                            &start_ts,
                                            format!(
                                                "MIDI noteoff ok dev={} hid={:?} ch={} note={}",
                                                device_id, hid, ch, note0
                                            ),
                                        );

                                        if capture.active.load(Ordering::Relaxed) {
                                            let when = Instant::now();
                                            let t_ms = when
                                                .duration_since(start_ts)
                                                .as_millis()
                                                .min(u64::MAX as u128)
                                                as u64;
                                            capture_events_push_row_meta_at(
                                                &capture,
                                                when,
                                                Some(ts),
                                                None,
                                                [
                                                    t_ms.to_string(),
                                                    String::new(),
                                                    "MIDI".to_string(),
                                                    "noteoff".to_string(),
                                                    device_id.to_string(),
                                                    format!("{hid:?}"),
                                                    String::new(),
                                                    String::new(),
                                                    String::new(),
                                                    String::new(),
                                                    String::new(),
                                                    String::new(),
                                                    String::new(),
                                                    ch.to_string(),
                                                    note0.to_string(),
                                                    String::new(),
                                                    String::new(),
                                                    String::new(),
                                                    String::new(),
                                                    String::new(),
                                                    String::new(),
                                                    String::new(),
                                                    String::new(),
                                                    "noteoff".to_string(),
                                                ],
                                            );
                                            cap_rows_midi += 1;
                                        }
                                    }
                                    Err(e) => {
                                        warn!(
                                            "MIDI noteoff failed ch={} note={} err={:?}",
                                            ch, note0, e
                                        );
                                        warn!(
                                            "PANIC: sending CC64=0 CC120=0 CC123=0 (noteoff error)"
                                        );
                                        dbg_push(
                                            &mut dbg_ring,
                                            &start_ts,
                                            format!("PANIC noteoff_error ch={} note={}", ch, note0),
                                        );
                                        dbg_push(&mut dbg_ring, &start_ts, cfg_line.clone());
                                        midi_out.panic_all();
                                        last_aftertouch_sent_by_key.clear();
                                        note_by_key.clear();
                                        note_on_count.clear();
                                        hud_pitch_by_key.clear();
                                        hud_pressed0.clear();
                                        hud_pressed1.clear();
                                        live_pub.mark_pressed_dirty();
                                    }
                                }
                            }
                            sent = true;
                        } else if let Some(VelState::Tracking {
                            out_ch,
                            note: note0,
                            playing,
                            ..
                        }) = removed
                        {
                            if playing {
                                match midi_out.send_note(false, out_ch, note0, 0) {
                                    Ok(()) => {
                                        dbg_push(
                                        &mut dbg_ring,
                                        &start_ts,
                                        format!(
                                            "MIDI noteoff ok (fallback) dev={} hid={:?} ch={} note={}",
                                            device_id, hid, out_ch, note0
                                        ),
                                    );

                                        if capture.active.load(Ordering::Relaxed) {
                                            let when = Instant::now();
                                            let t_ms = when
                                                .duration_since(start_ts)
                                                .as_millis()
                                                .min(u64::MAX as u128)
                                                as u64;
                                            capture_events_push_row_meta_at(
                                                &capture,
                                                when,
                                                Some(ts),
                                                None,
                                                [
                                                    t_ms.to_string(),
                                                    String::new(),
                                                    "MIDI".to_string(),
                                                    "noteoff_fallback".to_string(),
                                                    device_id.to_string(),
                                                    format!("{hid:?}"),
                                                    String::new(),
                                                    String::new(),
                                                    String::new(),
                                                    String::new(),
                                                    String::new(),
                                                    String::new(),
                                                    String::new(),
                                                    out_ch.to_string(),
                                                    note0.to_string(),
                                                    String::new(),
                                                    String::new(),
                                                    String::new(),
                                                    String::new(),
                                                    String::new(),
                                                    String::new(),
                                                    String::new(),
                                                    String::new(),
                                                    "noteoff_fallback".to_string(),
                                                ],
                                            );
                                            cap_rows_midi += 1;
                                        }
                                    }
                                    Err(e) => {
                                        warn!(
                                            "MIDI noteoff failed (fallback) ch={} note={} err={:?}",
                                            out_ch, note0, e
                                        );
                                        warn!(
                                            "PANIC: sending CC64=0 CC120=0 CC123=0 (noteoff error)"
                                        );
                                        dbg_push(
                                            &mut dbg_ring,
                                            &start_ts,
                                            format!(
                                                "PANIC noteoff_error_fallback ch={} note={}",
                                                out_ch, note0
                                            ),
                                        );
                                        midi_out.panic_all();
                                        last_aftertouch_sent_by_key.clear();
                                        note_by_key.clear();
                                        note_on_count.clear();
                                        hud_pitch_by_key.clear();
                                        hud_pressed0.clear();
                                        hud_pressed1.clear();
                                        live_pub.mark_pressed_dirty();
                                    }
                                }
                                sent = true;
                            }
                        }

                        if !sent {
                            // This is often a normal case for very fast taps: key released before the
                            // delayed NoteOn (peak tracking) ever fired, so there is no mapping.
                            // Only treat it as an error if we *believe* a NoteOn was sent.
                            match &removed {
                                Some(VelState::Tracking {
                                    started,
                                    peak,
                                    peak_speed,
                                    out_ch,
                                    note,
                                    playing: false,
                                    ..
                                }) => {
                                    // Option B: if the key is released before the delayed NoteOn tick,
                                    // emit a short tap note using the peak collected so far.
                                    const TAP_NOTE_OFF_MS: u64 = 20;
                                    let threshold = f32::from_bits(
                                        press_threshold_bits.load(Ordering::Relaxed),
                                    )
                                    .clamp(0.0, 0.99);
                                    let max_swing = cfg.velocity_max_swing.clamp(0.05, 1.0);
                                    let denom = (max_swing - threshold).max(0.001);

                                    let n = match aftertouch_mode {
                                        AftertouchMode::SpeedMapped => (peak_speed
                                            / aftertouch_speed_max.max(0.001))
                                        .clamp(0.0, 1.0),
                                        _ => ((peak - threshold) / denom).clamp(0.0, 1.0),
                                    };
                                    let n2 = velocity_profiles[velocity_profile_idx].apply(n);
                                    let vel = (n2 * 126.0).round().clamp(0.0, 126.0) as u8 + 1;

                                    dbg_push(
                                        &mut dbg_ring,
                                        &start_ts,
                                        format!(
                                            "TAP_NOTE dev={} hid={:?} ch={} note={} vel={} age_ms={} peak={:.3} peak_speed={:.3}",
                                            device_id,
                                            hid,
                                            out_ch,
                                            note,
                                            vel,
                                            started.elapsed().as_millis(),
                                            peak,
                                            peak_speed
                                        ),
                                    );

                                    match midi_out.send_note(true, *out_ch, *note, vel) {
                                        Ok(()) => {
                                            dbg_push(
                                                &mut dbg_ring,
                                                &start_ts,
                                                format!(
                                                    "MIDI noteon ok (tap) dev={} hid={:?} ch={} note={} vel={}",
                                                    device_id, hid, out_ch, note, vel
                                                ),
                                            );

                                            if capture.active.load(Ordering::Relaxed) {
                                                let when = Instant::now();
                                                let t_ms = when
                                                    .duration_since(start_ts)
                                                    .as_millis()
                                                    .min(u64::MAX as u128)
                                                    as u64;
                                                capture_events_push_row_meta_at(
                                                    &capture,
                                                    when,
                                                    Some(ts),
                                                    None,
                                                    [
                                                        t_ms.to_string(),
                                                        String::new(),
                                                        "MIDI".to_string(),
                                                        "noteon_tap".to_string(),
                                                        device_id.to_string(),
                                                        format!("{hid:?}"),
                                                        String::new(),
                                                        String::new(),
                                                        String::new(),
                                                        String::new(),
                                                        String::new(),
                                                        String::new(),
                                                        String::new(),
                                                        out_ch.to_string(),
                                                        note.to_string(),
                                                        vel.to_string(),
                                                        String::new(),
                                                        String::new(),
                                                        String::new(),
                                                        String::new(),
                                                        String::new(),
                                                        String::new(),
                                                        String::new(),
                                                        "noteon_tap".to_string(),
                                                    ],
                                                );
                                                cap_rows_midi += 1;
                                            }
                                            *note_on_count.entry((*out_ch, *note)).or_insert(0) +=
                                                1;

                                            // HUD pressed chord updates.
                                            let pitch = (*out_ch as i32) * edo
                                                + (*note as i32)
                                                + pitch_offset;
                                            hud_pitch_by_key.insert(key_id.clone(), pitch);
                                            if wtn_board == 0 {
                                                hud_pressed0.insert(pitch);
                                            } else if wtn_board == 1 {
                                                hud_pressed1.insert(pitch);
                                            }
                                            live_pub.mark_pressed_dirty();
                                            pending_noteoffs.push_back(PendingNoteOff {
                                                due: Instant::now()
                                                    + Duration::from_millis(TAP_NOTE_OFF_MS),
                                                ch: *out_ch,
                                                note: *note,
                                                origin_device_id: Some(device_id),
                                                origin_hid: Some(hid.clone()),
                                            });
                                        }
                                        Err(e) => {
                                            warn!(
                                                "MIDI noteon failed (tap) ch={} note={} vel={} err={:?}",
                                                out_ch, note, vel, e
                                            );
                                            dbg_push(&mut dbg_ring, &start_ts, cfg_line.clone());
                                        }
                                    }
                                    sent = true;
                                }
                                None => {
                                    dbg_push(
                                        &mut dbg_ring,
                                        &start_ts,
                                        format!(
                                            "UP without mapping (untracked) dev={} hid={:?}",
                                            device_id, hid
                                        ),
                                    );
                                    sent = true;
                                }
                                Some(VelState::Tracking { playing: true, .. }) => {
                                    warn!(
                                        "Up without note mapping (playing): device_id={} hid={:?}; sending CC64/CC120/CC123 panic",
                                        device_id, hid
                                    );
                                    dbg_push(
                                        &mut dbg_ring,
                                        &start_ts,
                                        format!(
                                            "PANIC Up without note mapping (playing) dev={} hid={:?}",
                                            device_id, hid
                                        ),
                                    );
                                    dbg_push(&mut dbg_ring, &start_ts, cfg_line.clone());
                                    midi_out.panic_all();
                                    last_aftertouch_sent_by_key.clear();
                                    note_by_key.clear();
                                    note_on_count.clear();
                                    hud_pitch_by_key.clear();
                                    hud_pressed0.clear();
                                    hud_pressed1.clear();
                                    live_pub.mark_pressed_dirty();
                                    sent = true;
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

            // End per-edge processing.
        }

        let edge_us = edge_t0.elapsed().as_micros().min(u64::MAX as u128) as u64;

        // Publish the latest observed lag so any event rows can carry it.
        capture.lag_ms.store(edge_lag_max_ms, Ordering::Relaxed);

        // Timings around top-of-loop calls (slow-path only): these are prime suspects for stalls.
        // Live HUD writes are timed in LivePublisher (LIVE_SLOW/LIVE_WRITE_SLOW).

        let cur_lag_ms = edge_lag_max_ms;
        let mut tick_us: u64 = 0;

        // Timer tick: fire peak-tracked NoteOn and periodic aftertouch.
        {
            let tick_t0 = Instant::now();
            let peak_track = Duration::from_millis(cfg.velocity_peak_track_ms.max(1) as u64);
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
                        last_analog,
                        last_analog_ts,
                        peak_speed,
                        mut at_level,
                        out_ch,
                        note,
                        playing,
                        led,
                    } => {
                        if started.elapsed() < peak_track {
                            continue;
                        }

                        // Guard against firing delayed NoteOn after the key has been released.
                        if !playing && !pressed_keys.contains(&key) {
                            dbg_push(
                                &mut dbg_ring,
                                &start_ts,
                                format!(
                                    "LATE_NOTEON_SKIP dev={} hid={:?} age_ms={} peak={:.3} last={:.3}",
                                    key.0,
                                    key.1,
                                    started.elapsed().as_millis(),
                                    peak,
                                    last_analog
                                ),
                            );
                            vel_state.remove(&key);
                            continue;
                        }

                        dbg_push(
                            &mut dbg_ring,
                            &start_ts,
                            format!(
                                "NOTEON_TICK dev={} hid={:?} age_ms={} playing={} pressed={} peak={:.3} last={:.3} peak_speed={:.3}",
                                key.0,
                                key.1,
                                started.elapsed().as_millis(),
                                playing,
                                pressed_keys.contains(&key),
                                peak,
                                last_analog,
                                peak_speed
                            ),
                        );

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
                        if n2 > at_level {
                            at_level = n2;
                        }
                        at_level = at_level.clamp(0.0, 1.0);

                        let pressure: u8 = match aftertouch_mode {
                            AftertouchMode::SpeedMapped | AftertouchMode::PeakMapped => {
                                (at_level * 127.0).round().clamp(0.0, 127.0) as u8
                            }
                            AftertouchMode::Off => 0,
                        };

                        // No continuous per-key pitchbend: pitchbend is driven by dedicated control keys.

                        if !playing {
                            // Apply current per-device channel pitchbend before NoteOn.
                            let up_amt = bend_up_amt_by_device.get(&key.0).copied().unwrap_or(0.0);
                            let down_amt =
                                bend_down_amt_by_device.get(&key.0).copied().unwrap_or(0.0);
                            let bend: i32 = bend_from_amounts(up_amt, down_amt);
                            let kpb = (key.0, out_ch);
                            let last = last_pb_by_dev_ch.get(&kpb).copied().unwrap_or(i32::MIN);
                            if last != bend {
                                let _ = midi_out.send_pitchbend(out_ch, bend);
                                last_pb_by_dev_ch.insert(kpb, bend);
                            }

                            let noteon_ok = match midi_out.send_note(true, out_ch, note, vel) {
                                Ok(()) => true,
                                Err(e) => {
                                    warn!(
                                        "MIDI noteon failed ch={} note={} vel={} err={:?}",
                                        out_ch, note, vel, e
                                    );
                                    dbg_push(
                                        &mut dbg_ring,
                                        &start_ts,
                                        format!(
                                            "MIDI noteon failed ch={} note={} vel={}",
                                            out_ch, note, vel
                                        ),
                                    );
                                    dbg_push(&mut dbg_ring, &start_ts, cfg_line.clone());
                                    false
                                }
                            };

                            if noteon_ok {
                                dbg_push(
                                    &mut dbg_ring,
                                    &start_ts,
                                    format!(
                                        "MIDI noteon ok dev={} hid={:?} ch={} note={} vel={}",
                                        key.0, key.1, out_ch, note, vel
                                    ),
                                );

                                if capture.active.load(Ordering::Relaxed) {
                                    let when = Instant::now();
                                    let t_ms = when
                                        .duration_since(start_ts)
                                        .as_millis()
                                        .min(u64::MAX as u128)
                                        as u64;
                                    capture_events_push_row_meta_at(
                                        &capture,
                                        when,
                                        None,
                                        Some(cur_lag_ms),
                                        [
                                            t_ms.to_string(),
                                            String::new(),
                                            "MIDI".to_string(),
                                            "noteon".to_string(),
                                            key.0.to_string(),
                                            format!("{:?}", key.1),
                                            String::new(),
                                            String::new(),
                                            String::new(),
                                            String::new(),
                                            String::new(),
                                            String::new(),
                                            String::new(),
                                            out_ch.to_string(),
                                            note.to_string(),
                                            vel.to_string(),
                                            String::new(),
                                            String::new(),
                                            String::new(),
                                            String::new(),
                                            String::new(),
                                            String::new(),
                                            String::new(),
                                            "noteon".to_string(),
                                        ],
                                    );
                                    cap_rows_midi += 1;
                                }
                                note_by_key.insert(key.clone(), (out_ch, note));
                                *note_on_count.entry((out_ch, note)).or_insert(0) += 1;

                                // HUD pressed chord updates.
                                if let Some(layout) = layouts.get(layout_index) {
                                    let edo = layout.edo_divisions;
                                    let pitch_offset = layout.pitch_offset;
                                    let pitch =
                                        (out_ch as i32) * edo + (note as i32) + pitch_offset;
                                    hud_pitch_by_key.insert(key.clone(), pitch);
                                    let hud_board = board_by_device
                                        .get(&key.0)
                                        .map(|b| b.wtn_board)
                                        .unwrap_or(0u8);
                                    if hud_board == 0 {
                                        hud_pressed0.insert(pitch);
                                    } else if hud_board == 1 {
                                        hud_pressed1.insert(pitch);
                                    }
                                    live_pub.mark_pressed_dirty();
                                }

                                if !no_aftertouch {
                                    match aftertouch_mode {
                                        AftertouchMode::PeakMapped
                                        | AftertouchMode::SpeedMapped => {
                                            let last = last_aftertouch_sent_by_key
                                                .get(&key)
                                                .copied()
                                                .unwrap_or(255);
                                            if pressure != last {
                                                let ok = midi_out
                                                    .send_polytouch(out_ch, note, pressure)
                                                    .is_ok();
                                                if ok {
                                                    last_aftertouch_sent_by_key
                                                        .insert(key.clone(), pressure);
                                                    if capture.active.load(Ordering::Relaxed) {
                                                        let when = Instant::now();
                                                        let t_ms = when
                                                            .duration_since(start_ts)
                                                            .as_millis()
                                                            .min(u64::MAX as u128)
                                                            as u64;
                                                        capture_events_push_row_meta_at(
                                                            &capture,
                                                            when,
                                                            None,
                                                            Some(cur_lag_ms),
                                                            [
                                                                t_ms.to_string(),
                                                                String::new(),
                                                                "AFTERTOUCH".to_string(),
                                                                "polytouch".to_string(),
                                                                key.0.to_string(),
                                                                format!("{:?}", key.1),
                                                                String::new(),
                                                                String::new(),
                                                                String::new(),
                                                                String::new(),
                                                                String::new(),
                                                                String::new(),
                                                                String::new(),
                                                                out_ch.to_string(),
                                                                note.to_string(),
                                                                String::new(),
                                                                pressure.to_string(),
                                                                String::new(),
                                                                String::new(),
                                                                String::new(),
                                                                String::new(),
                                                                String::new(),
                                                                String::new(),
                                                                "polytouch".to_string(),
                                                            ],
                                                        );
                                                        cap_rows_aftertouch += 1;
                                                    }
                                                }
                                            }
                                        }
                                        AftertouchMode::Off => {}
                                    }
                                }
                            }

                            vel_state.insert(
                                key,
                                VelState::Tracking {
                                    started: Instant::now(),
                                    peak: last_analog,
                                    last_analog,
                                    last_analog_ts,
                                    peak_speed: 0.0,
                                    at_level,
                                    out_ch,
                                    note,
                                    playing: noteon_ok,
                                    led,
                                },
                            );
                        } else {
                            if !no_aftertouch {
                                match aftertouch_mode {
                                    AftertouchMode::PeakMapped | AftertouchMode::SpeedMapped => {
                                        let last = last_aftertouch_sent_by_key
                                            .get(&key)
                                            .copied()
                                            .unwrap_or(255);
                                        if pressure != last {
                                            let ok = midi_out
                                                .send_polytouch(out_ch, note, pressure)
                                                .is_ok();
                                            if ok {
                                                last_aftertouch_sent_by_key
                                                    .insert(key.clone(), pressure);
                                                if capture.active.load(Ordering::Relaxed) {
                                                    let when = Instant::now();
                                                    let t_ms = when
                                                        .duration_since(start_ts)
                                                        .as_millis()
                                                        .min(u64::MAX as u128)
                                                        as u64;
                                                    capture_events_push_row_meta_at(
                                                        &capture,
                                                        when,
                                                        None,
                                                        Some(cur_lag_ms),
                                                        [
                                                            t_ms.to_string(),
                                                            String::new(),
                                                            "AFTERTOUCH".to_string(),
                                                            "polytouch".to_string(),
                                                            key.0.to_string(),
                                                            format!("{:?}", key.1),
                                                            String::new(),
                                                            String::new(),
                                                            String::new(),
                                                            String::new(),
                                                            String::new(),
                                                            String::new(),
                                                            String::new(),
                                                            out_ch.to_string(),
                                                            note.to_string(),
                                                            String::new(),
                                                            pressure.to_string(),
                                                            String::new(),
                                                            String::new(),
                                                            String::new(),
                                                            String::new(),
                                                            String::new(),
                                                            String::new(),
                                                            "polytouch".to_string(),
                                                        ],
                                                    );
                                                    cap_rows_aftertouch += 1;
                                                }
                                            }
                                        }
                                    }
                                    AftertouchMode::Off => {}
                                }
                            }

                            vel_state.insert(
                                key,
                                VelState::Tracking {
                                    started: Instant::now(),
                                    peak: last_analog,
                                    last_analog,
                                    last_analog_ts,
                                    peak_speed: 0.0,
                                    at_level,
                                    out_ch,
                                    note,
                                    playing,
                                    led,
                                },
                            );
                        }
                    }
                }
            }

            tick_us = tick_t0.elapsed().as_micros().min(u64::MAX as u128) as u64;
        }

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
            live_pub.mark_mode_dirty();
        }

        // Scheduled NoteOffs for tap notes.
        {
            let now = Instant::now();
            while let Some(p) = pending_noteoffs.front() {
                if p.due > now {
                    break;
                }
                let p = pending_noteoffs.pop_front().unwrap();
                let k = (p.ch, p.note);
                let Some(cnt) = note_on_count.get_mut(&k) else {
                    // Already released.
                    continue;
                };
                if *cnt > 1 {
                    *cnt -= 1;
                    dbg_push(
                        &mut dbg_ring,
                        &start_ts,
                        format!(
                            "MIDI noteoff skip (refcount) ch={} note={} cnt={}",
                            p.ch, p.note, *cnt
                        ),
                    );
                    continue;
                }
                note_on_count.remove(&k);
                match midi_out.send_note(false, p.ch, p.note, 0) {
                    Ok(()) => {
                        let orig_dev = p.origin_device_id;
                        let orig_hid = p.origin_hid.clone();

                        dbg_push(
                            &mut dbg_ring,
                            &start_ts,
                            format!("MIDI noteoff ok (scheduled) ch={} note={}", p.ch, p.note),
                        );

                        if capture.active.load(Ordering::Relaxed) {
                            if let (Some(dev), Some(hid)) = (orig_dev, orig_hid.clone()) {
                                let when = Instant::now();
                                let t_ms = when
                                    .duration_since(start_ts)
                                    .as_millis()
                                    .min(u64::MAX as u128)
                                    as u64;
                                capture_events_push_row_meta_at(
                                    &capture,
                                    when,
                                    Some(p.due),
                                    None,
                                    [
                                        t_ms.to_string(),
                                        String::new(),
                                        "MIDI".to_string(),
                                        "noteoff_scheduled".to_string(),
                                        dev.to_string(),
                                        format!("{hid:?}"),
                                        String::new(),
                                        String::new(),
                                        String::new(),
                                        String::new(),
                                        String::new(),
                                        String::new(),
                                        String::new(),
                                        p.ch.to_string(),
                                        p.note.to_string(),
                                        String::new(),
                                        String::new(),
                                        String::new(),
                                        String::new(),
                                        String::new(),
                                        String::new(),
                                        String::new(),
                                        String::new(),
                                        "noteoff_scheduled".to_string(),
                                    ],
                                );
                                cap_rows_midi += 1;
                            }
                        }

                        // HUD: scheduled noteoff ends the tap note.
                        if let (Some(dev), Some(hid)) = (orig_dev, orig_hid) {
                            let key_id = (dev, hid);
                            if let Some(bcfg) = board_by_device.get(&dev) {
                                if let Some(pitch) = hud_pitch_by_key.remove(&key_id) {
                                    if bcfg.wtn_board == 0 {
                                        hud_pressed0.remove(&pitch);
                                    } else if bcfg.wtn_board == 1 {
                                        hud_pressed1.remove(&pitch);
                                    }
                                    live_pub.mark_pressed_dirty();
                                }
                            }
                        }
                    }
                    Err(e) => {
                        warn!(
                            "MIDI noteoff failed (scheduled) ch={} note={} err={:?}",
                            p.ch, p.note, e
                        );
                        warn!("PANIC: sending CC64=0 CC120=0 CC123=0 (scheduled noteoff error)");
                        dbg_push(
                            &mut dbg_ring,
                            &start_ts,
                            format!("PANIC scheduled_noteoff_error ch={} note={}", p.ch, p.note),
                        );
                        dbg_push(&mut dbg_ring, &start_ts, cfg_line.clone());
                        midi_out.panic_all();
                        last_aftertouch_sent_by_key.clear();
                        note_by_key.clear();
                        note_on_count.clear();
                        pending_noteoffs.clear();
                        hud_pitch_by_key.clear();
                        hud_pressed0.clear();
                        hud_pressed1.clear();
                        live_pub.mark_pressed_dirty();
                    }
                }
            }
        }

        // Rate-limited lag log: include basic timing breakdown.
        if edge_cnt > 0 && edge_lag_max_ms >= 50 {
            if edge_lag_max_ms >= 500 || last_lag_log.elapsed() >= Duration::from_secs(2) {
                last_lag_log = Instant::now();
                let lock_drops = capture.lock_drops.load(Ordering::Relaxed);
                let msg = format!(
                    "LAG edges={} max_ms={} over50={} over200={} downup={} update={} vel_state={} lock_drops={} edge_us={} tick_us={} cap_edge={} cap_midi={} cap_at={}",
                    edge_cnt,
                    edge_lag_max_ms,
                    edge_lag_over_50ms,
                    edge_lag_over_200ms,
                    edge_cnt_downup,
                    edge_cnt_updates,
                    vel_state.len(),
                    lock_drops,
                    edge_us,
                    tick_us,
                    cap_rows_edge,
                    cap_rows_midi,
                    cap_rows_aftertouch,
                );
                dbg_push(&mut dbg_ring, &start_ts, msg.clone());
                info!("{}", msg);
            }
        }
    }

    info!("xenwooting exiting");
    Ok(())
}
