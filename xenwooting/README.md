# xenwooting

Headless Wooting + MTS-ESP + RGB daemon.

This project:

- Hosts the MTS-ESP shared-memory master via `libMTS.so`.
- Reads key events from the Wooting Analog SDK (HID usage codes).
- Maps keys onto a 4x14 (56 key) MIDI grid per device.
- Looks up `Key/Chan/Col` in `.wtn` files (same syntax as `.ltn`).
- Outputs multi-channel MIDI notes (16 channels * 128 notes).
- Drives Wooting RGB for base colors + press highlighting.

## Build

```bash
cd xenwooting
cargo build --release
```

## Minimal MTS test

```bash
./target/release/mts_demo
ipcs -m
```

## Run

First run writes a default config to `~/.config/xenwooting/config.toml`.

```bash
./target/release/xenwooting --print-devices
./target/release/xenwooting
```

To see per-note computed pitch/frequency (based on current EDO + pitch_offset):

```bash
./target/release/xenwooting --log-midi
```

Useful debug mode (prints HID down/up events only):

```bash
./target/release/xenwooting --dump-hid
```

Edit `~/.config/xenwooting/config.toml`:

- Set `boards[].device_id` to bind each keyboard deterministically.
- Set `layouts[].wtn_path` to your `.wtn` files.
- Fix any HID-to-matrix mistakes with `hid_overrides`.

## Install As System Service

```bash
./contrib/install-systemd.sh
```

This will:

- install `/usr/local/bin/xenwooting`
- install and enable `xenwooting.service`
- disable `tun.service` (xenassist) and `wooting-analog-midi-headless.service` if present

`hid_overrides` entries use the Debug name of `HIDCodes` from the Wooting SDK, e.g. `Escape`, `N1`, `BracketLeft`, `RightCtrl`.
