# Implementation Details

This file documents a few non-obvious implementation choices in this repo.

## Layout Logic (Grids and Mappings)

This project has to reconcile multiple coordinate systems:

- a logical per-board musical grid (what the user edits in `.wtn`)
- physical keys reported by the Analog SDK (HID codes)
- the RGB LED matrix addressed by the RGB SDK (row/col with holes)

When you change anything related to keyboard layout (rotation, ISO vs ANSI, split-backspace,
etc.) you must know which layer you are changing.

### 1) `.wtn` logical grid (musical)

Files: `~/.config/xenwooting/wtn/*.wtn`

Each `[BoardN]` section stores 56 cells as `Key_0..Key_55`, `Chan_0..Chan_55`, `Col_0..Col_55`.

Important semantics:

- The 56 indices represent a *4-row musical grid*.
- Index `i` maps to a logical `(row, col)` as:

  - `row = i / 14` (0..3)
  - `col = i % 14` (0..13)

- **Holes are not part of `.wtn` semantics.**
  Wide keys create holes in the physical LED column grid; `.wtn` indices are treated as
  "first key, second key, ..." within each row from the user's perspective.
  (Implementation: we compact/left-justify rows after applying rotation/mirror.
  See "WTN Compaction" below.)

Cell meanings:

- `Key_i` is the MIDI note number (0..127) used by xenwooting for the pressed key.
- `Chan_i` is the MIDI channel (1..16) (stored 1-based in the file).
- `Col_i` is the base LED color (RRGGBB) used for the idle layout and the base color to
  restore after highlight.

### 2) Physical key identity (Analog SDK)

xenwooting reads per-key analog values from Wooting's Analog SDK.

- Each physical keyboard is identified by an **Analog `device_id`** (`u64`).
- Each key on that keyboard is identified by a **HID usage code** (Wooting uses HID keycodes).

We map:

`(device_id, HID) -> KeyLoc { midi_row, midi_col, led_row, led_col }`

The mapping table lives in `xenwooting/src/hidmap.rs`:

- `HidMap::default_60he_ansi_guess()` provides a best-effort mapping for a 60% ANSI layout.
- `hid_overrides` in config can override any key to a custom `(midi_row, midi_col, led_row, led_col)`.

### 3) MIDI grid (per board)

`KeyLoc.midi_row/midi_col` is the *musical grid coordinate* for a physical key, before
per-board transforms.

This grid is **always 4x14** (rows 0..3, cols 0..13).

Note: for certain physical layouts (ANSI wide keys), not every (row,col) exists as a
physical key. Those missing positions are the "holes".

### 4) RGB LED grid (physical)

The RGB SDK addresses keys via a matrix:

- `led_row` is the physical RGB row (0..5)
- `led_col` is the physical RGB column (0..13 on 60HE)

Wide keys create holes here. Example from `xenwooting/src/hidmap.rs`:

- RightShift is a wide key; it uses `midi_col=11` but `led_col=13`.
- Enter is forced to `led_col=13`.

Highlighting must use the LED grid (physical), not the MIDI grid.

### 5) Per-board logical transforms (rotation/mirror)

Each configured keyboard (`[[boards]]`) can apply transforms for `.wtn` lookup:

- `rotation_deg`: supported values: `0` or `180`
- `mirror_cols`: left/right mirror

These transforms affect only the `.wtn` *lookup* (musical space), not the physical LED
coordinates. In code, the transform helpers operate on `KeyLoc` by changing
`midi_row/midi_col` and leaving `led_row/led_col` unchanged:

- `rotate_4x14()` in `xenwooting/src/hidmap.rs`
- `mirror_cols_4x14()` in `xenwooting/src/hidmap.rs`

This is how we can mount the North keyboard rotated 180 degrees while keeping LED addressing
correct.

### 6) WTN compaction ("ignore holes")

Problem:

- Some physical rows have fewer than 14 keys.
- On unrotated ANSI layouts, missing positions tend to be at the end, so `.wtn` indices
  feel left-justified.
- After rotation, those holes move to the start of the row, which would make `.wtn` index
  0 correspond to a hole.

Desired behavior:

- `.wtn` indices always mean "first key, second key, ..." in that row from the user's
  perspective.
- Holes must never consume `.wtn` indices.

Solution:

- After applying rotation/mirror, compute per-row `min midi_col` among the actually present
  keys.
- Subtract that offset before indexing `.wtn`.

Implementation:

- `compute_compact_col_offsets()` (per board/device)
- `wtn_index_for_loc()`

These are used everywhere we look up `.wtn`:

- MIDI note/channel lookup
- base LED paint lookup
- base color restore after highlight

### 7) Dual keyboard identity and LED routing (robust device_id -> RGB index)

See the next section.

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

## How To Change Layouts (New Keyboard / ISO / Different Key Geometry)

There are three common types of changes:

### A) Change musical mapping (what note/channel/color each key means)

- Edit the `.wtn` file (via webconfigurator or by hand).
- `.wtn` is the authoritative "musical" mapping.

Notes:

- For rotated boards, remember that xenwooting applies rotation/mirror and then compacts rows
  (holes ignored). The configurator mirrors this.

### B) Change board orientation (North vs South physical mounting)

- Update `[[boards]]` entries in `~/.config/xenwooting/config.toml`:
  - North board: `rotation_deg = 180`
  - South board: `rotation_deg = 0`
- Ensure North is assigned to `wtn_board = 0` and South to `wtn_board = 1` (project convention).

This affects `.wtn` lookup only.

### C) Change physical key/LED mapping (ISO layout, split keys, different Wooting model)

This is the part that changes *coordinates*, not notes.

1) Determine how HID codes map to your physical key positions.

2) Update `hid_overrides` in `~/.config/xenwooting/config.toml` to adjust the mapping.
   Each override is:

   - HID name
   - `midi_row`, `midi_col` (musical grid position)
   - `led_row`, `led_col` (physical LED address)

3) If many keys differ from the built-in guess, consider updating
   `HidMap::default_60he_ansi_guess()` in `xenwooting/src/hidmap.rs` or adding a new
   preset map for your model.

Important:

- Holes: if the physical LED grid has holes, set `led_col` appropriately (like RightShift,
  Enter in the ANSI guess). Do not encode holes into `.wtn`.
- Rotation/mirror: do NOT change `led_row/led_col` when applying rotation/mirror; only
  `midi_row/midi_col` changes.

## Hex-Grid Board Geometry + LTN->WTN Mapping (Import / Placement Mode)

The configurator can project Lumatone `.ltn` layouts onto Wooting `.wtn` layouts using explicit
hex-grid geometry tables (not pixel hit-testing).

### Geometry Tables

Files:

- `webconfigurator/web/src/hexgrid/boardGrids.ts`
  - `WTN_GRIDS.{Board0,Board1}`: Wooting boards in a hex-grid **visible-key** index space
    (53 keys per board, indices `0..52`).
  - `LTN_GRIDS.Board0..Board4`: Lumatone boards (56 keys per board, indices `0..55`).

Each key has an integer coordinate `(x, y)` in a hex lattice where neighbors are:

- `(0, -2)`, `(0, +2)`
- `(-1, -1)`, `(-1, +1)`, `(+1, -1)`, `(+1, +1)`

This is a "doubled-y" representation: adjacent columns differ by 2 in `y`.

### Combined Coordinate Space

Mapping uses a single combined coordinate space:

1) LTN: `(BoardN, Key_k) -> (x, y)` via `LTN_GRIDS.BoardN.byKey`.
2) WTN: `(x, y) -> (wtn_board, visible_key_index)` via a combined lookup built from `WTN_GRIDS`.

This step is purely geometric. **Holes/wide-key gaps are not part of the projection**.

### Transform (Translate + 60-degree Rotate)

Placement mode applies an adjustable transform to LTN coordinates before lookup:

```
world = rotate60(src, rot_steps) + (tx, ty)
```

- `rot_steps` is in `0..5` (60-degree steps around the hex axes)
- `(tx, ty)` is a translation in the same `(x, y)` coordinate system

Implementation: `webconfigurator/web/src/hexgrid/project.ts`

Rotation is implemented by converting `(x, y)` to cube coordinates scaled by 2, applying
`(X, Y, Z) -> (-Z, -X, -Y)` per step, then converting back.

### Rotation Pivot (Hover-rotate)

In placement mode, pressing `r` rotates around:

- the currently hovered target WTN key, or
- a fixed anchor coordinate if the mouse is not hovering a key.

Translation is adjusted so the overlay rotates "in place" around that world coordinate.

### Visible-key Mapping vs `.wtn` 56-cell Indexing

WTN geometry tables use **visible** key indices (`0..52`). However `.wtn` files and xenwooting
operate on the 56-cell 4x14 musical grid (`Key_0..Key_55`).

After the geometric hit-test identifies `(wtn_board, visible_key_index)`, the configurator maps
that visible index onto the current internal 0..55 indexing used by the loaded layout arrays.

This final step is intentionally separate so we can later change `.wtn` handling (e.g. eliminate
unused indices) without changing the hex-grid projection logic.

### Overlay / Apply Semantics

- Overlay cells in placement mode render with a red border + red text.
- `Enter` applies the overlay values into the in-memory layout (not saved yet).
- `Esc` aborts placement without applying.
- Missing/incomplete `.ltn` entries are ignored.

Relevant code:

- LTN parsing: `webconfigurator/web/src/ltn/parse.ts`
- Placement + projection + apply: `webconfigurator/web/src/App.tsx`
- Overlay rendering: `webconfigurator/web/src/KeyboardView.tsx`
