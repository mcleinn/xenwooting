use anyhow::{Context, Result};

#[link(name = "wooting-rgb-sdk")]
extern "C" {
    fn wooting_rgb_kbd_connected() -> bool;
    fn wooting_usb_device_count() -> u8;
    fn wooting_usb_select_device(index: u8) -> bool;
    fn wooting_rgb_array_auto_update(auto_update: bool);
    fn wooting_rgb_array_set_single(row: u8, column: u8, red: u8, green: u8, blue: u8) -> bool;
    fn wooting_rgb_array_update_keyboard() -> bool;
    fn wooting_rgb_direct_set_key(row: u8, column: u8, red: u8, green: u8, blue: u8) -> bool;
}

#[derive(Debug, Clone, Copy)]
pub struct Rgb {
    pub device_index: u8,
}

impl Rgb {
    pub fn ensure_connected() {
        unsafe {
            let _ = wooting_rgb_kbd_connected();
        }
    }

    pub fn device_count() -> u8 {
        Self::ensure_connected();
        unsafe { wooting_usb_device_count() }
    }

    pub fn select(device_index: u8) -> Result<Self> {
        Self::ensure_connected();
        let ok = unsafe { wooting_usb_select_device(device_index) };
        if !ok {
            anyhow::bail!("Failed to select RGB device index {device_index}");
        }
        unsafe {
            // We drive updates manually.
            wooting_rgb_array_auto_update(false);
        }
        Ok(Self { device_index })
    }

    pub fn set_key(&self, row: u8, col: u8, rgb: (u8, u8, u8)) -> Result<()> {
        let (r, g, b) = rgb;
        // Note: The device selection is global inside the SDK; re-select defensively.
        let ok_sel = unsafe { wooting_usb_select_device(self.device_index) };
        if !ok_sel {
            anyhow::bail!("Failed to re-select RGB device index {}", self.device_index);
        }
        let ok = unsafe { wooting_rgb_direct_set_key(row, col, r, g, b) };
        if !ok {
            anyhow::bail!("wooting_rgb_direct_set_key failed");
        }
        Ok(())
    }

    pub fn array_set_single(&self, row: u8, col: u8, rgb: (u8, u8, u8)) -> Result<()> {
        let (r, g, b) = rgb;
        let ok_sel = unsafe { wooting_usb_select_device(self.device_index) };
        if !ok_sel {
            anyhow::bail!("Failed to re-select RGB device index {}", self.device_index);
        }
        let ok = unsafe { wooting_rgb_array_set_single(row, col, r, g, b) };
        if !ok {
            anyhow::bail!("wooting_rgb_array_set_single failed");
        }
        Ok(())
    }

    pub fn array_update_keyboard(&self) -> Result<()> {
        let ok_sel = unsafe { wooting_usb_select_device(self.device_index) };
        if !ok_sel {
            anyhow::bail!("Failed to re-select RGB device index {}", self.device_index);
        }
        let ok = unsafe { wooting_rgb_array_update_keyboard() };
        if !ok {
            anyhow::bail!("wooting_rgb_array_update_keyboard failed");
        }
        Ok(())
    }
}

pub fn parse_hex_rgb(s: &str) -> Result<(u8, u8, u8)> {
    let s = s.trim().trim_start_matches('#');
    if s.len() != 6 {
        anyhow::bail!("Expected RRGGBB");
    }
    let r = u8::from_str_radix(&s[0..2], 16).context("Invalid R")?;
    let g = u8::from_str_radix(&s[2..4], 16).context("Invalid G")?;
    let b = u8::from_str_radix(&s[4..6], 16).context("Invalid B")?;
    Ok((r, g, b))
}
