use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_midi_out_name")]
    pub midi_out_name: String,

    #[serde(default = "default_refresh_hz")]
    pub refresh_hz: f32,

    #[serde(default = "default_press_threshold")]
    pub press_threshold: f32,

    #[serde(default = "default_press_threshold_step")]
    pub press_threshold_step: f32,

    /// Time window to track the press peak (ms) before firing NoteOn.
    #[serde(default = "default_velocity_peak_track_ms")]
    pub velocity_peak_track_ms: u32,

    /// Quiet time (ms) below threshold before firing NoteOff.
    #[serde(default = "default_aftershock_ms")]
    pub aftershock_ms: u32,

    /// Peak normalization reference (0..1). Used as "max swing" when mapping peak to velocity.
    #[serde(default = "default_velocity_max_swing")]
    pub velocity_max_swing: f32,

    /// Minimum analog delta (0..1) needed to emit an Update edge.
    /// This affects aftertouch smoothness and MIDI bandwidth.
    #[serde(default = "default_aftertouch_delta")]
    pub aftertouch_delta: f32,

    /// Movement-based release threshold (0..1). If a pressed key's analog value falls by at least
    /// this amount from its peak since press, it is treated as released (rapid-trigger style).
    #[serde(default = "default_release_delta")]
    pub release_delta: f32,

    /// In speed-mapped aftertouch mode, this is the normalization reference for d(analog)/dt.
    /// Larger values make the same physical speed produce smaller aftertouch values.
    #[serde(default = "default_aftertouch_speed_max")]
    pub aftertouch_speed_max: f32,

    /// Step used by control-bar buttons when adjusting aftertouch_speed_max.
    #[serde(default = "default_aftertouch_speed_step")]
    pub aftertouch_speed_step: f32,

    /// Attack time (ms) for speed-mapped aftertouch envelope.
    #[serde(default = "default_aftertouch_speed_attack_ms")]
    pub aftertouch_speed_attack_ms: u32,

    /// Decay time (ms) for speed-mapped aftertouch envelope.
    #[serde(default = "default_aftertouch_speed_decay_ms")]
    pub aftertouch_speed_decay_ms: u32,

    #[serde(default)]
    pub boards: Vec<BoardConfig>,

    #[serde(default)]
    pub layouts: Vec<LayoutConfig>,

    #[serde(default)]
    pub actions: ActionBindings,

    #[serde(default)]
    pub rgb: RgbConfig,

    #[serde(default)]
    pub control_bar: ControlBarConfig,

    #[serde(default)]
    pub hid_overrides: Vec<HidOverride>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlBarConfig {
    /// Physical LED row to treat as the reserved/control bar.
    ///
    /// NOTE: This is the device's LED matrix row index (0..5), not your desk orientation.
    #[serde(default = "default_control_bar_row")]
    pub row: u8,

    /// Map HID key name -> LED column(s) (0..13) on the control bar.
    ///
    /// Accepts either a single integer or an array of integers in TOML:
    /// - `Space = 7`
    /// - `Space = [5, 6, 7, 8]`
    #[serde(default)]
    pub led_cols_by_hid: HashMap<String, OneOrManyU8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum OneOrManyU8 {
    One(u8),
    Many(Vec<u8>),
}

impl OneOrManyU8 {
    pub fn as_vec(&self) -> Vec<u8> {
        match self {
            OneOrManyU8::One(v) => vec![*v],
            OneOrManyU8::Many(vs) => vs.clone(),
        }
    }
}

fn default_control_bar_row() -> u8 {
    5
}

impl Default for ControlBarConfig {
    fn default() -> Self {
        // Best-effort 60HE ANSI bottom-row guess.
        // Users can (and should) override this per device if their LEDs are shifted.
        let mut led_cols_by_hid = HashMap::new();
        led_cols_by_hid.insert("LeftCtrl".to_string(), OneOrManyU8::One(0));
        led_cols_by_hid.insert("LeftMeta".to_string(), OneOrManyU8::One(1));
        led_cols_by_hid.insert("LeftAlt".to_string(), OneOrManyU8::One(2));
        // Spacebar tends to span multiple LEDs.
        led_cols_by_hid.insert("Space".to_string(), OneOrManyU8::Many(vec![4, 5, 6, 7, 8]));
        led_cols_by_hid.insert("RightAlt".to_string(), OneOrManyU8::One(10));
        // Common right-side positions on 60HE ANSI.
        led_cols_by_hid.insert("ContextMenu".to_string(), OneOrManyU8::One(11));
        led_cols_by_hid.insert("RightCtrl".to_string(), OneOrManyU8::One(12));
        Self {
            row: default_control_bar_row(),
            led_cols_by_hid,
        }
    }
}

fn default_midi_out_name() -> String {
    "XenWTN".to_string()
}

fn default_refresh_hz() -> f32 {
    1000.0
}

fn default_press_threshold() -> f32 {
    // Note-on threshold (0..1). Kept low because velocity uses a peak tracker.
    0.10
}

fn default_press_threshold_step() -> f32 {
    0.05
}

fn default_velocity_peak_track_ms() -> u32 {
    6
}

fn default_aftershock_ms() -> u32 {
    35
}

fn default_velocity_max_swing() -> f32 {
    1.0
}

fn default_aftertouch_delta() -> f32 {
    0.01
}

fn default_release_delta() -> f32 {
    0.12
}

fn default_aftertouch_speed_max() -> f32 {
    74.0
}

fn default_aftertouch_speed_step() -> f32 {
    2.0
}

fn default_aftertouch_speed_attack_ms() -> u32 {
    12
}

fn default_aftertouch_speed_decay_ms() -> u32 {
    250
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoardConfig {
    /// Analog device_id (u64), as string.
    ///
    /// TOML integers are signed (i64) and cannot represent the full u64 range, so we store it
    /// as a string in config (e.g. "16353264950129218108").
    pub device_id: Option<String>,

    /// Which .wtn board section to use for this device, e.g. 0 or 1.
    pub wtn_board: u8,

    /// Rotation of the playable 4x14 grid in degrees. Supported: 0 or 180.
    #[serde(default)]
    pub rotation_deg: u16,

    /// Mirror the playable 4x14 grid left-right for .wtn lookup.
    /// This only affects the logical mapping (pitch/color), not the physical LED coordinates.
    #[serde(default)]
    pub mirror_cols: bool,

    /// Meta-grid placement (MIDI-only logical space). Not used for .wtn lookup yet,
    /// but carried for future expansion.
    #[serde(default)]
    pub meta_x: i32,
    #[serde(default)]
    pub meta_y: i32,
}

impl BoardConfig {
    pub fn device_id_u64(&self) -> anyhow::Result<Option<u64>> {
        let Some(s) = self.device_id.as_deref() else {
            return Ok(None);
        };
        let id: u64 = s
            .trim()
            .parse()
            .map_err(|e| anyhow::anyhow!("Invalid device_id '{}': {}", s, e))?;
        Ok(Some(id))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayoutConfig {
    pub id: String,
    pub name: String,
    pub wtn_path: String,
    pub edo_divisions: i32,
    #[serde(default)]
    pub pitch_offset: i32,
}

impl LayoutConfig {
    pub fn sort_key(&self) -> (&str, &str) {
        // Primary: human name; fallback: id.
        let primary = if self.name.trim().is_empty() {
            self.id.as_str()
        } else {
            self.name.as_str()
        };
        (primary, self.id.as_str())
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ActionBindings {
    /// Map action name -> HID key name, e.g. "layout_next" -> "Fn".
    #[serde(default)]
    pub by_action: HashMap<String, String>,
}

impl ActionBindings {
    pub fn default_with_sane_keys() -> Self {
        let mut by_action = HashMap::new();
        // NOTE: HIDCodes does not include a dedicated "Fn" key. On some layouts the physical
        // key labeled Fn may present as another HID code (often RightAlt/RightMeta/etc.).
        // Treat these as defaults only; override in config after you confirm what your device emits.
        by_action.insert("layout_prev".to_string(), "RightCtrl".to_string());
        by_action.insert("layout_next".to_string(), "RightAlt".to_string());
        by_action.insert("octave_down".to_string(), "LeftAlt".to_string());
        by_action.insert("octave_up".to_string(), "ContextMenu".to_string());
        Self { by_action }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RgbConfig {
    /// If true, xenwooting will attempt to drive the RGB SDK.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// After this many seconds of inactivity, switch off all LEDs (screensaver).
    ///
    /// Set to 0 to disable.
    #[serde(default = "default_screensaver_timeout_sec")]
    pub screensaver_timeout_sec: u32,

    /// Map wtn_board -> rgb_sdk_device_index (0-based). If missing, uses wtn_board as index.
    ///
    /// TOML table keys are strings, so we store keys as strings (e.g. "0" = 1).
    #[serde(default)]
    pub device_index_by_wtn_board: HashMap<String, u8>,

    /// Highlight color for pressed keys.
    #[serde(default = "default_highlight_hex")]
    pub highlight_hex: String,
}

fn default_true() -> bool {
    true
}

fn default_screensaver_timeout_sec() -> u32 {
    300
}

fn default_highlight_hex() -> String {
    "FFFFFF".to_string()
}

impl Default for RgbConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            screensaver_timeout_sec: default_screensaver_timeout_sec(),
            device_index_by_wtn_board: HashMap::new(),
            highlight_hex: default_highlight_hex(),
        }
    }
}

impl RgbConfig {
    pub fn rgb_device_index_for_wtn_board(&self, wtn_board: u8) -> u8 {
        let k = wtn_board.to_string();
        self.device_index_by_wtn_board
            .get(&k)
            .copied()
            .unwrap_or(wtn_board)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HidOverride {
    pub hid: String,
    pub midi_row: u8,
    pub midi_col: u8,
    pub led_row: u8,
    pub led_col: u8,
}
