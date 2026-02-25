use anyhow::{Context, Result};
use libc::{c_char, c_double};
use std::ffi::CString;

#[link(name = "MTS")]
extern "C" {
    fn MTS_RegisterMaster();
    fn MTS_DeregisterMaster();
    fn MTS_HasIPC() -> bool;
    fn MTS_Reinitialize();

    fn MTS_SetScaleName(name: *const c_char);
    fn MTS_SetNoteTunings(freqs: *const c_double);
    fn MTS_SetMultiChannel(set: bool, midichannel: i8);
    fn MTS_SetMultiChannelNoteTunings(freqs: *const c_double, midichannel: i8);
    fn MTS_SetMultiChannelNoteTuning(freq: c_double, midinote: i8, midichannel: i8);
}

pub struct MtsMaster {
    registered: bool,
}

impl MtsMaster {
    /// Registers an MTS master.
    ///
    /// This build of libMTS.so does not export `MTS_CanRegisterMaster()`, so we cannot
    /// preflight-check. If you suspect stale shared memory from a crash, pass
    /// `reinitialize_if_ipc=true`.
    pub fn register(reinitialize_if_ipc: bool) -> Result<Self> {
        unsafe {
            if reinitialize_if_ipc && MTS_HasIPC() {
                MTS_Reinitialize();
            }
            MTS_RegisterMaster();
        }
        Ok(Self { registered: true })
    }

    pub fn set_scale_name(&self, name: &str) -> Result<()> {
        let s = CString::new(name).context("Scale name contains NUL")?;
        unsafe { MTS_SetScaleName(s.as_ptr()) };
        Ok(())
    }

    pub fn enable_all_channels(&self) {
        unsafe {
            for ch in 0..16 {
                MTS_SetMultiChannel(true, ch);
            }
        }
    }

    pub fn set_note_tunings(&self, freqs_128: &[f64; 128]) {
        unsafe {
            MTS_SetNoteTunings(freqs_128.as_ptr() as *const c_double);
        }
    }

    pub fn set_multi_channel_note_tuning(&self, ch: u8, note: u8, freq_hz: f64) {
        unsafe {
            MTS_SetMultiChannelNoteTuning(freq_hz as c_double, note as i8, ch as i8);
        }
    }

    pub fn set_multi_channel_note_tunings(&self, ch: u8, freqs_128: &[f64; 128]) {
        unsafe {
            MTS_SetMultiChannelNoteTunings(freqs_128.as_ptr() as *const c_double, ch as i8);
        }
    }
}

impl Drop for MtsMaster {
    fn drop(&mut self) {
        if self.registered {
            unsafe { MTS_DeregisterMaster() };
        }
    }
}
