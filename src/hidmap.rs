use anyhow::Result;
use std::collections::HashMap;
use wooting_analog_wrapper::{FromPrimitive, HIDCodes};

#[derive(Debug, Clone, Copy)]
pub struct KeyLoc {
    pub midi_row: u8, // 0..3 (playable rows)
    pub midi_col: u8, // 0..13
    pub led_row: u8,  // physical RGB row (0..5)
    pub led_col: u8,  // physical RGB col (0..13)
}

#[derive(Debug, Clone)]
pub struct HidMap {
    loc_by_hid: HashMap<HIDCodes, KeyLoc>,
}

impl HidMap {
    pub fn default_60he_ansi_guess() -> Self {
        // This is an intentional *guess* for a standard 60% ANSI layout.
        // Users can override positions via config.
        let mut m: HashMap<HIDCodes, KeyLoc> = HashMap::new();

        // Row 1 (physical RGB row 1) -> MIDI row 0
        let r_led = 1u8;
        let r_midi = 0u8;
        let row1: &[(HIDCodes, u8)] = &[
            (HIDCodes::Escape, 0),
            (HIDCodes::N1, 1),
            (HIDCodes::N2, 2),
            (HIDCodes::N3, 3),
            (HIDCodes::N4, 4),
            (HIDCodes::N5, 5),
            (HIDCodes::N6, 6),
            (HIDCodes::N7, 7),
            (HIDCodes::N8, 8),
            (HIDCodes::N9, 9),
            (HIDCodes::N0, 10),
            (HIDCodes::Minus, 11),
            (HIDCodes::Equal, 12),
            (HIDCodes::Backspace, 13),
        ];
        for (hid, c) in row1 {
            m.insert(
                hid.clone(),
                KeyLoc {
                    midi_row: r_midi,
                    midi_col: *c,
                    led_row: r_led,
                    led_col: *c,
                },
            );
        }

        // Row 2 (physical RGB row 2) -> MIDI row 1
        let r_led = 2u8;
        let r_midi = 1u8;
        let row2: &[(HIDCodes, u8)] = &[
            (HIDCodes::Tab, 0),
            (HIDCodes::Q, 1),
            (HIDCodes::W, 2),
            (HIDCodes::E, 3),
            (HIDCodes::R, 4),
            (HIDCodes::T, 5),
            (HIDCodes::Y, 6),
            (HIDCodes::U, 7),
            (HIDCodes::I, 8),
            (HIDCodes::O, 9),
            (HIDCodes::P, 10),
            (HIDCodes::BracketLeft, 11),
            (HIDCodes::BracketRight, 12),
            (HIDCodes::Backslash, 13),
        ];
        for (hid, c) in row2 {
            m.insert(
                hid.clone(),
                KeyLoc {
                    midi_row: r_midi,
                    midi_col: *c,
                    led_row: r_led,
                    led_col: *c,
                },
            );
        }

        // Row 3 (physical RGB row 3) -> MIDI row 2
        let r_led = 3u8;
        let r_midi = 2u8;
        let row3: &[(HIDCodes, u8)] = &[
            (HIDCodes::CapsLock, 0),
            (HIDCodes::A, 1),
            (HIDCodes::S, 2),
            (HIDCodes::D, 3),
            (HIDCodes::F, 4),
            (HIDCodes::G, 5),
            (HIDCodes::H, 6),
            (HIDCodes::J, 7),
            (HIDCodes::K, 8),
            (HIDCodes::L, 9),
            (HIDCodes::Semicolon, 10),
            (HIDCodes::Quote, 11),
            (HIDCodes::Enter, 12),
        ];
        for (hid, c) in row3 {
            m.insert(
                hid.clone(),
                KeyLoc {
                    midi_row: r_midi,
                    midi_col: *c,
                    led_row: r_led,
                    led_col: *c,
                },
            );
        }

        // Row 4 (physical RGB row 4) -> MIDI row 3
        let r_led = 4u8;
        let r_midi = 3u8;
        // Wide keys create holes in the LED grid columns. This is a best-effort ANSI guess.
        let row4: &[(HIDCodes, u8, u8)] = &[
            (HIDCodes::LeftShift, 0, 0),
            (HIDCodes::Z, 1, 2),
            (HIDCodes::X, 2, 3),
            (HIDCodes::C, 3, 4),
            (HIDCodes::V, 4, 5),
            (HIDCodes::B, 5, 6),
            (HIDCodes::N, 6, 7),
            (HIDCodes::M, 7, 8),
            (HIDCodes::Comma, 8, 9),
            (HIDCodes::Period, 9, 10),
            (HIDCodes::Slash, 10, 11),
            (HIDCodes::RightShift, 11, 13),
        ];
        for (hid, midi_c, led_c) in row4 {
            m.insert(
                hid.clone(),
                KeyLoc {
                    midi_row: r_midi,
                    midi_col: *midi_c,
                    led_row: r_led,
                    led_col: *led_c,
                },
            );
        }

        // Wide Enter key tends to be the last LED column.
        if let Some(loc) = m.get_mut(&HIDCodes::Enter) {
            loc.led_col = 13;
        }

        Self { loc_by_hid: m }
    }

    pub fn apply_overrides(&mut self, overrides: &[(String, u8, u8, u8, u8)]) -> Result<()> {
        for (hid_name, midi_row, midi_col, led_row, led_col) in overrides {
            let hid = parse_hid_name(hid_name)?;
            self.loc_by_hid.insert(
                hid,
                KeyLoc {
                    midi_row: *midi_row,
                    midi_col: *midi_col,
                    led_row: *led_row,
                    led_col: *led_col,
                },
            );
        }
        Ok(())
    }

    pub fn loc_for(&self, hid: HIDCodes) -> Option<KeyLoc> {
        self.loc_by_hid.get(&hid).copied()
    }
}

pub fn parse_hid_name(name: &str) -> Result<HIDCodes> {
    // Accept enum variant names from wooting_analog_wrapper::HIDCodes
    // e.g. "Esc", "RightControl", "Fn".
    for code in 0u16..=255u16 {
        if let Some(h) = HIDCodes::from_u8(code as u8) {
            let n = format!("{:?}", h);
            if n.eq_ignore_ascii_case(name) {
                return Ok(h);
            }
        }
    }
    anyhow::bail!("Unknown HID key name: {name}")
}

pub fn rotate_4x14(loc: KeyLoc, rotation_deg: u16) -> Result<KeyLoc> {
    match rotation_deg {
        0 => Ok(loc),
        180 => {
            if loc.midi_row >= 4 || loc.midi_col >= 14 {
                anyhow::bail!("KeyLoc out of 4x14 bounds");
            }
            Ok(KeyLoc {
                midi_row: 3 - loc.midi_row,
                midi_col: 13 - loc.midi_col,
                ..loc
            })
        }
        _ => anyhow::bail!("Unsupported rotation_deg {rotation_deg}; use 0 or 180"),
    }
}

pub fn mirror_cols_4x14(mut loc: KeyLoc, mirror: bool) -> Result<KeyLoc> {
    if !mirror {
        return Ok(loc);
    }
    if loc.midi_row >= 4 || loc.midi_col >= 14 {
        anyhow::bail!("KeyLoc out of 4x14 bounds");
    }
    loc.midi_col = 13 - loc.midi_col;
    Ok(loc)
}
