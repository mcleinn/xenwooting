use crate::rgb::Rgb;
use crossbeam_channel::{Receiver, TrySendError};
use log::{info, warn};
use std::collections::HashMap;
use std::thread;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy)]
pub struct RgbKey {
    pub device_index: u8,
    pub row: u8,
    pub col: u8,
    pub rgb: (u8, u8, u8),
}

#[derive(Debug, Clone, Copy)]
pub enum RgbCmd {
    SetKey(RgbKey),
}

pub fn spawn_rgb_worker(rx: Receiver<RgbCmd>) {
    thread::spawn(move || {
        // Lazily initialize. If the SDK hangs, it only hangs this worker thread.
        let mut per_device: HashMap<u8, Rgb> = HashMap::new();
        let mut dirty: HashMap<u8, bool> = HashMap::new();
        let mut warned_init = false;

        let flush_every = Duration::from_millis(16);
        let mut last_flush = Instant::now();

        loop {
            // Periodic flush.
            if last_flush.elapsed() >= flush_every {
                for (dev_idx, is_dirty) in dirty.iter_mut() {
                    if !*is_dirty {
                        continue;
                    }
                    if let Some(dev) = per_device.get(dev_idx) {
                        let _ = dev.array_update_keyboard();
                    }
                    *is_dirty = false;
                }
                last_flush = Instant::now();
            }

            let cmd = match rx.recv_timeout(Duration::from_millis(5)) {
                Ok(c) => c,
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => continue,
                Err(_) => return,
            };

            match cmd {
                RgbCmd::SetKey(k) => {
                    if !per_device.contains_key(&k.device_index) {
                        let n = Rgb::device_count();
                        if n == 0 {
                            if !warned_init {
                                warn!("RGB: no devices detected");
                                warned_init = true;
                            }
                            continue;
                        }
                        if k.device_index >= n {
                            warn!(
                                "RGB: device_index {} out of range (0..{})",
                                k.device_index,
                                n.saturating_sub(1)
                            );
                            continue;
                        }
                        match Rgb::select(k.device_index) {
                            Ok(dev) => {
                                info!("RGB: selected device index {}", k.device_index);
                                per_device.insert(k.device_index, dev);
                                dirty.insert(k.device_index, false);
                            }
                            Err(e) => {
                                warn!("RGB: failed to select device {}: {:#}", k.device_index, e);
                                continue;
                            }
                        }
                    }

                    if let Some(dev) = per_device.get(&k.device_index) {
                        // Update the backing array; flush is periodic.
                        if dev.array_set_single(k.row, k.col, k.rgb).is_ok() {
                            *dirty.entry(k.device_index).or_insert(false) = true;
                        }
                    }
                }
            }
        }
    });
}

pub fn try_send_drop(tx: &crossbeam_channel::Sender<RgbCmd>, cmd: RgbCmd) {
    match tx.try_send(cmd) {
        Ok(()) => {}
        Err(TrySendError::Full(_)) => {
            // Drop if we're behind.
        }
        Err(TrySendError::Disconnected(_)) => {}
    }
}
