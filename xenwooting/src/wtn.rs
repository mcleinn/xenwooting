use anyhow::{Context, Result};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Copy, Default)]
pub struct WtnCell {
    pub key: u8,
    /// Stored as file value (1-16). Convert to 0-15 for MIDI.
    pub chan_1based: u8,
    pub col_rgb: (u8, u8, u8),
}

#[derive(Debug, Clone)]
pub struct Wtn {
    pub boards: HashMap<u8, Vec<WtnCell>>, // board -> 56 cells
}

impl Wtn {
    pub fn load(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path)
            .with_context(|| format!("Failed to read wtn file: {}", path.display()))?;
        parse_wtn(&text)
    }

    pub fn cell(&self, board: u8, index_0_55: usize) -> Option<WtnCell> {
        self.boards
            .get(&board)
            .and_then(|v| v.get(index_0_55))
            .copied()
    }
}

fn parse_hex_color_last6(s: &str) -> Result<(u8, u8, u8)> {
    let mut s = s.trim();
    if s.starts_with('#') {
        s = &s[1..];
    }
    if s.len() < 6 {
        anyhow::bail!("Color too short");
    }
    let s = &s[s.len() - 6..];
    let r = u8::from_str_radix(&s[0..2], 16).context("Invalid color R")?;
    let g = u8::from_str_radix(&s[2..4], 16).context("Invalid color G")?;
    let b = u8::from_str_radix(&s[4..6], 16).context("Invalid color B")?;
    Ok((r, g, b))
}

pub fn parse_wtn(text: &str) -> Result<Wtn> {
    let mut boards: HashMap<u8, Vec<WtnCell>> = HashMap::new();
    let mut current_board: Option<u8> = None;

    // Temp storage per board and index
    let mut tmp: HashMap<(u8, usize), WtnCell> = HashMap::new();

    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            let name = &line[1..line.len() - 1];
            if let Some(rest) = name.strip_prefix("Board") {
                let b: u8 = rest
                    .parse()
                    .with_context(|| format!("Bad board header: {line}"))?;
                current_board = Some(b);
                continue;
            }
            anyhow::bail!("Unknown section header: {line}");
        }

        let b = current_board.context("Key/value before any [BoardX] header")?;
        let (k, v) = line
            .split_once('=')
            .with_context(|| format!("Bad line (expected key=value): {line}"))?;
        let k = k.trim();
        let v = v.trim();

        let (field, idx_str) = k
            .split_once('_')
            .with_context(|| format!("Bad key (expected Field_N): {k}"))?;
        let idx: usize = idx_str.parse().with_context(|| format!("Bad index: {k}"))?;
        if idx >= 56 {
            // For xenwooting we only care about 56 playable keys.
            continue;
        }
        let cell = tmp.entry((b, idx)).or_insert_with(WtnCell::default);
        match field {
            "Key" => {
                cell.key = v
                    .parse()
                    .with_context(|| format!("Bad Key value: {line}"))?;
            }
            "Chan" => {
                let cv: u8 = v
                    .parse()
                    .with_context(|| format!("Bad Chan value: {line}"))?;
                cell.chan_1based = cv;
            }
            "Col" => {
                cell.col_rgb =
                    parse_hex_color_last6(v).with_context(|| format!("Bad Col: {line}"))?;
            }
            _ => {
                // Ignore unknown fields.
            }
        }
    }

    // Build dense vectors per board
    for ((b, idx), cell) in tmp.into_iter() {
        let v = boards
            .entry(b)
            .or_insert_with(|| vec![WtnCell::default(); 56]);
        v[idx] = cell;
    }

    Ok(Wtn { boards })
}
