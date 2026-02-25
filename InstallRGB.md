# InstallRGB (Raspberry Pi 5 / 60HE v2)

## Examples

All examples assume:

- RGB SDK installed to `/usr/local/lib/libwooting-rgb-sdk.so.0`
- Script available at `/home/patch/wooting-xen/wooting_rgb.py`
- Two keyboards connected

List detected Wooting RGB devices (device indices are 0-based):

```bash
python3 /home/patch/wooting-xen/wooting_rgb.py --list
```

Show meta information for devices 0 and 1:

```bash
python3 /home/patch/wooting-xen/wooting_rgb.py --devices 0,1 --info
```

Set all keys on both keyboards to solid red:

```bash
python3 /home/patch/wooting-xen/wooting_rgb.py --devices 0,1 --all --rgb 255 0 0
```

Set all keys on keyboard 0 to a hex color:

```bash
python3 /home/patch/wooting-xen/wooting_rgb.py --device 0 --all --hex 00FFAA
```

Set a single key (matrix row/col) on both keyboards (direct mode):

```bash
python3 /home/patch/wooting-xen/wooting_rgb.py --devices 0,1 --key 0 0 --hex FF00FF --direct
```

Reset keyboards back to their profile/original colors:

```bash
python3 /home/patch/wooting-xen/wooting_rgb.py --devices 0,1 --reset
```

Effects (Ctrl-C to stop). Available effects:

- `scanner`
- `rainbow`
- `breathe`

Run a red scanner effect on both keyboards for 10 seconds:

```bash
python3 /home/patch/wooting-xen/wooting_rgb.py --devices 0,1 --effect scanner --hex FF0000 --seconds 10
```

Run a rainbow effect on keyboard 0 indefinitely:

```bash
python3 /home/patch/wooting-xen/wooting_rgb.py --device 0 --effect rainbow --hex FF0000
```

Run a breathing effect (blue) on keyboard 1 at 60 fps:

```bash
python3 /home/patch/wooting-xen/wooting_rgb.py --device 1 --effect breathe --hex 0080FF --fps 60
```

Restore profile colors after an effect completes:

```bash
python3 /home/patch/wooting-xen/wooting_rgb.py --devices 0,1 --effect scanner --hex FF0000 --seconds 5 --restore
```

This document captures what it took to get the Wooting RGB SDK working on a headless Raspberry Pi 5 with (at least) two Wooting 60HE v2 keyboards attached.

It is written as "lessons learned" so the next person doesn't repeat the same dead ends.

## What Worked

- Build and install the Wooting RGB SDK from source.
- On Debian/Raspberry Pi OS, prefer linking against the SDK's bundled `hidapi` submodule.
- For 60HE v2 on the multi-report interface, ensure the SDK uses the correct output report ID/size.
- Use a small Python wrapper (`ctypes`) to select devices and set colors.

## Key Lessons Learned

### 1) System `hidapi` may be too old / missing symbols

When linking against Debian's `libhidapi-*`, the RGB SDK failed to load at runtime:

- `undefined symbol: hid_get_report_descriptor`

The upstream Wooting RGB SDK expects `hid_get_report_descriptor()` (provided by newer hidapi implementations). Debian's packaged `hidapi` (0.13.x on Bookworm) does not export this symbol.

Fix:

- Compile the RGB SDK against the repo's bundled `hidapi` submodule instead of `pkg-config hidapi-hidraw`.

### 2) Enumeration can block on feature-response reads

The upstream SDK tries to determine layout by sending a feature request and waiting for a response. On this setup that could block/hang.

Fix applied:

- Avoid layout detection during enumeration (set layout to `LAYOUT_UNKNOWN`). Layout is not required for basic per-key RGB.

### 3) Strict response-size checks can disconnect devices

The SDK's `wooting_usb_send_feature()` expects a full response buffer for each feature report. On the 60HE v2 multi-report interface we observed the device returning 0 bytes for the init command response, leading to "disconnecting.." logic.

Fix applied:

- Treat feature send failures as fatal, but do not require a full response buffer for every feature command.

### 4) Multi-report output report ID must match the report length

This was the main "why don't colors change" issue.

On 60HE v2, the HID report descriptor for the RGB interface exposes output reports:

- ID 4: 510 bytes
- ID 5: 1022 bytes
- ID 6: 2046 bytes

The SDK was sending a ~2046 byte payload but using report ID `5`. The device would accept writes but not apply them as expected.

Fix applied:

- In `wooting_usb_send_buffer_v3()`, set `report_buffer[0] = 6` when writing a 2046-byte report.

After this change, effects and full-matrix updates applied reliably to both keyboards.

### 5) Don't reset colors immediately in your control script

`wooting_rgb_close()` resets colors back to the keyboard's original/profile colors before closing handles.

If a test script calls `close()` at the end of every command, you will see a brief flash (or nothing) and then the keyboard returns to its profile colors, which looks like "the script didn't work".

Fix applied in the Python helper:

- Only call `wooting_rgb_close()` when an explicit `--restore` flag is used.

## Install Steps (Pi)

Dependencies:

```bash
sudo apt update
sudo apt install -y libusb-1.0-0-dev libhidapi-dev pkg-config make gcc
```

Clone and build:

```bash
git clone --recursive https://github.com/WootingKb/wooting-rgb-sdk.git
cd wooting-rgb-sdk/linux
make
sudo make install
sudo ldconfig
```

Note: on this Pi we adjusted the build to compile against bundled `hidapi` and patched the SDK for 60HE v2 multi-report correctness.

## Usage

We use a Python `ctypes` wrapper in `/home/patch/wooting-xen/wooting_rgb.py`:

- `--list` to enumerate devices
- `--device` / `--devices` to target one or many keyboards
- `--all` and `--key` to set colors
- `--effect` for simple effects
- `--restore` to reset back to profile colors on exit

Examples:

```bash
python3 /home/patch/wooting-xen/wooting_rgb.py --list
python3 /home/patch/wooting-xen/wooting_rgb.py --devices 0,1 --all --rgb 255 0 0
python3 /home/patch/wooting-xen/wooting_rgb.py --devices 0,1 --effect scanner --hex FF0000
```
