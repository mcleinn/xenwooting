# Implementation Details

This file documents a few non-obvious implementation choices in this repo.

## Dual Keyboard Mapping (Analog device_id -> WTN board -> RGB device index)

Goal:

- Treat each physical Wooting keyboard as a stable identity.
- Ensure MIDI note/channel and LED behavior for a given `.wtn` cell always end up on the
  same physical key.
- Make the assignment stable across unplug/replug and regardless of USB plug order.

### Terms

- **Analog device_id**: `u64` identifier returned by the Wooting *Analog* SDK
  (`get_connected_devices_info`). This is stable per physical keyboard.
- **WTN board**: logical board index used in `.wtn` files (`[Board0]`, `[Board1]`, ...),
  used for mapping note/channel/color.
- **RGB device index**: `u8` index used by the Wooting *RGB* SDK
  (`wooting_usb_select_device(index)`), used for sending LED updates.

### Why we cannot rely on enumeration order

The analog SDK and RGB SDK can enumerate devices in different orders. Even if they match
at boot, unplugging and replugging can change the order. If we route LEDs based on a
hard-coded `wtn_board -> rgb_index` mapping (or discovery order), you can see:

- Pressing a key on one keyboard highlights the same key position on the *other* keyboard.
- Base LED colors appear swapped between the two physical boards.

The analog path uses `device_id` to associate events with a specific keyboard. The RGB
path uses an opaque `device_index` without a stable serial/ID in the API, so an explicit
mapping layer is required.

### Robust solution implemented

We compute a runtime map:

`analog_device_id (u64) -> rgb_device_index (u8)`

and use it for **all LED output** (base paint, press highlight, control-bar feedback).

Implementation notes:

1. The Wooting analog SDK `device_id` is derived from `(vid, pid, serial)` using a Rust
   `DefaultHasher` (see Wooting's `generate_device_id`).
2. On Linux, we can read the HID serial string from sysfs via:
   `/sys/bus/hid/devices/*/uevent` keys:
   - `HID_UNIQ=<serial>`
   - `HID_ID=...:<vid>:<pid>`
   - `HID_PHYS=...` (used only as a stable sort key)
3. We re-create the analog `device_id` by hashing those values the same way.
4. We then assign RGB device indices by sorting discovered HID devices by `HID_PHYS`
   (stable for a given physical USB topology) and pairing in that order with RGB
   indices `0..rgb_count-1`.
5. The resulting map is refreshed when the set of connected configured devices changes
   (hotplug) and when RGB device count changes.

Code references:

- `xenwooting/src/bin/xenwooting.rs`
  - `compute_rgb_index_by_device_id(...)`
  - `analog_device_id_from_serial(...)`
  - `rgb_index_by_device_id` map used for:
    - base paint (`paint_base`)
    - key highlight
    - control bar LED feedback

### Base LED painting must use HID->KeyLoc mapping

Separate but related issue:

- Some ANSI keys are wider (Enter, Shift) which creates "holes" in the LED column grid.
- If base painting assumes `led_col == midi_col` for a synthetic 4x14 grid, rows will be
  shifted/misaligned.

Fix:

- Base painting iterates the HID->`KeyLoc` map (same source of truth as highlighting),
  and uses `KeyLoc.led_row/led_col` for physical LEDs.
- Rotation/mirroring is applied only for `.wtn` lookup (`midi_row/midi_col`).

Code references:

- `xenwooting/src/hidmap.rs`: `HidMap::all_locs()`
- `xenwooting/src/bin/xenwooting.rs`: `paint_base` uses `hid_map.all_locs()`
