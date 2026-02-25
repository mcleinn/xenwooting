#!/usr/bin/env python3

import argparse
import ctypes
import os
import sys
import time
from ctypes import c_bool, c_char_p, c_int, c_size_t, c_uint8, c_uint16
from dataclasses import dataclass
from typing import Iterable, Optional, Tuple


def _try_load_lib() -> ctypes.CDLL:
    candidates = [
        "libwooting-rgb-sdk.so.0",
        "libwooting-rgb-sdk.so",
        "/usr/local/lib/libwooting-rgb-sdk.so.0",
        "/usr/local/lib/libwooting-rgb-sdk.so",
    ]
    last_err: Optional[Exception] = None
    for name in candidates:
        try:
            return ctypes.CDLL(name)
        except OSError as e:
            last_err = e
    raise SystemExit(
        "Failed to load Wooting RGB SDK shared library.\n"
        "Tried: {}\n"
        "Last error: {}".format(
            ", ".join(candidates),
            str(last_err) if last_err else "(unknown)",
        )
    )


class WOOTING_USB_META(ctypes.Structure):
    _fields_ = [
        ("connected", c_bool),
        ("model", c_char_p),
        ("max_rows", c_uint8),
        ("max_columns", c_uint8),
        ("led_index_max", c_uint8),
        ("device_type", c_int),
        ("v2_interface", c_bool),
        ("layout", c_int),
        ("uses_small_packets", c_bool),
        ("uses_multi_report", c_bool),
    ]


@dataclass(frozen=True)
class DeviceInfo:
    index: int
    model: str
    connected: bool
    max_rows: int
    max_columns: int
    led_index_max: int
    device_type: int
    layout: int
    v2_interface: bool
    uses_small_packets: bool
    uses_multi_report: bool


class WootingRgb:
    def __init__(self) -> None:
        self.lib = _try_load_lib()
        self._bind()

    def _bind(self) -> None:
        # USB device selection / enumeration
        self.lib.wooting_usb_device_count.argtypes = []
        self.lib.wooting_usb_device_count.restype = c_uint8

        self.lib.wooting_usb_select_device.argtypes = [c_uint8]
        self.lib.wooting_usb_select_device.restype = c_bool

        self.lib.wooting_usb_get_device_meta.argtypes = [c_uint8]
        self.lib.wooting_usb_get_device_meta.restype = ctypes.POINTER(WOOTING_USB_META)

        # Core RGB API
        self.lib.wooting_rgb_kbd_connected.argtypes = []
        self.lib.wooting_rgb_kbd_connected.restype = c_bool

        self.lib.wooting_rgb_reset_rgb.argtypes = []
        self.lib.wooting_rgb_reset_rgb.restype = c_bool

        self.lib.wooting_rgb_close.argtypes = []
        self.lib.wooting_rgb_close.restype = c_bool

        self.lib.wooting_rgb_array_auto_update.argtypes = [c_bool]
        self.lib.wooting_rgb_array_auto_update.restype = None

        self.lib.wooting_rgb_array_set_single.argtypes = [
            c_uint8,
            c_uint8,
            c_uint8,
            c_uint8,
            c_uint8,
        ]
        self.lib.wooting_rgb_array_set_single.restype = c_bool

        self.lib.wooting_rgb_array_update_keyboard.argtypes = []
        self.lib.wooting_rgb_array_update_keyboard.restype = c_bool

        self.lib.wooting_rgb_direct_set_key.argtypes = [
            c_uint8,
            c_uint8,
            c_uint8,
            c_uint8,
            c_uint8,
        ]
        self.lib.wooting_rgb_direct_set_key.restype = c_bool

        self.lib.wooting_rgb_direct_reset_key.argtypes = [c_uint8, c_uint8]
        self.lib.wooting_rgb_direct_reset_key.restype = c_bool

    def device_count(self) -> int:
        # Enumeration happens lazily on first use.
        # Force an initial scan so device_count is meaningful.
        try:
            self.lib.wooting_rgb_kbd_connected()
        except Exception:
            pass
        return int(self.lib.wooting_usb_device_count())

    def select_device(self, index: int) -> None:
        if index < 0 or index > 255:
            raise SystemExit("Device index out of range")
        ok = bool(self.lib.wooting_usb_select_device(c_uint8(index)))
        if not ok:
            raise SystemExit(f"Failed to select device index {index}")

    def get_device_info(self, index: int) -> DeviceInfo:
        p = self.lib.wooting_usb_get_device_meta(c_uint8(index))
        if not p:
            raise SystemExit(f"No device meta for index {index}")
        m = p.contents
        model = (m.model.decode("utf-8", errors="replace") if m.model else "")
        return DeviceInfo(
            index=index,
            model=model,
            connected=bool(m.connected),
            max_rows=int(m.max_rows),
            max_columns=int(m.max_columns),
            led_index_max=int(m.led_index_max),
            device_type=int(m.device_type),
            layout=int(m.layout),
            v2_interface=bool(m.v2_interface),
            uses_small_packets=bool(m.uses_small_packets),
            uses_multi_report=bool(m.uses_multi_report),
        )

    def reset(self) -> None:
        if not bool(self.lib.wooting_rgb_reset_rgb()):
            raise SystemExit("wooting_rgb_reset_rgb failed")

    def close(self) -> None:
        # NOTE: wooting_rgb_close() resets colors back to the keyboard's
        # original/profile colors and then closes handles.
        self.lib.wooting_rgb_close()

    def set_auto_update(self, enabled: bool) -> None:
        self.lib.wooting_rgb_array_auto_update(c_bool(enabled))

    def set_key(self, row: int, col: int, rgb: Tuple[int, int, int], direct: bool) -> None:
        r, g, b = rgb
        if direct:
            ok = bool(
                self.lib.wooting_rgb_direct_set_key(
                    c_uint8(row), c_uint8(col), c_uint8(r), c_uint8(g), c_uint8(b)
                )
            )
        else:
            ok = bool(
                self.lib.wooting_rgb_array_set_single(
                    c_uint8(row), c_uint8(col), c_uint8(r), c_uint8(g), c_uint8(b)
                )
            )
        if not ok:
            raise SystemExit("Failed to set key color")

    def reset_key(self, row: int, col: int) -> None:
        if not bool(self.lib.wooting_rgb_direct_reset_key(c_uint8(row), c_uint8(col))):
            raise SystemExit("Failed to reset key")

    def update(self) -> None:
        if not bool(self.lib.wooting_rgb_array_update_keyboard()):
            raise SystemExit("wooting_rgb_array_update_keyboard failed")


def clamp_u8(v: int) -> int:
    if v < 0:
        return 0
    if v > 255:
        return 255
    return int(v)


def parse_rgb(args: argparse.Namespace) -> Tuple[int, int, int]:
    if args.hex is not None:
        s = args.hex.strip().lstrip("#")
        if len(s) != 6:
            raise SystemExit("--hex must be RRGGBB")
        r = int(s[0:2], 16)
        g = int(s[2:4], 16)
        b = int(s[4:6], 16)
        return (r, g, b)

    if args.rgb is None or len(args.rgb) != 3:
        raise SystemExit("Provide --rgb R G B or --hex RRGGBB")
    return (clamp_u8(args.rgb[0]), clamp_u8(args.rgb[1]), clamp_u8(args.rgb[2]))


def iter_all_cells(rows: int, cols: int) -> Iterable[Tuple[int, int]]:
    for r in range(rows):
        for c in range(cols):
            yield r, c


def hsv_to_rgb(h: float, s: float, v: float) -> Tuple[int, int, int]:
    # h in [0,1)
    i = int(h * 6.0)
    f = h * 6.0 - i
    p = v * (1.0 - s)
    q = v * (1.0 - f * s)
    t = v * (1.0 - (1.0 - f) * s)
    i = i % 6
    if i == 0:
        r, g, b = v, t, p
    elif i == 1:
        r, g, b = q, v, p
    elif i == 2:
        r, g, b = p, v, t
    elif i == 3:
        r, g, b = p, q, v
    elif i == 4:
        r, g, b = t, p, v
    else:
        r, g, b = v, p, q
    return (int(r * 255.0), int(g * 255.0), int(b * 255.0))


def main() -> int:
    ap = argparse.ArgumentParser(
        description="Control Wooting keyboard LEDs via the Wooting RGB SDK (multi-device)."
    )
    ap.add_argument(
        "--device",
        type=int,
        default=0,
        help="Device index (0-based, see --list). Ignored if --devices is provided.",
    )
    ap.add_argument(
        "--devices",
        type=str,
        default=None,
        help="Comma-separated device indices, e.g. 0,1,2 (overrides --device)",
    )
    ap.add_argument(
        "--list",
        action="store_true",
        help="List connected Wooting RGB devices",
    )
    ap.add_argument(
        "--info",
        action="store_true",
        help="Print device info (for --device)",
    )
    ap.add_argument(
        "--auto-update",
        action="store_true",
        help="Auto update after each array set (slow; default is manual update)",
    )

    g = ap.add_mutually_exclusive_group()
    g.add_argument(
        "--rgb",
        type=int,
        nargs=3,
        metavar=("R", "G", "B"),
        help="RGB color components (0-255)",
    )
    g.add_argument(
        "--hex",
        type=str,
        help="RGB color as hex, e.g. FF00AA",
    )

    ap.add_argument(
        "--all",
        action="store_true",
        help="Set all keys to the selected color (requires --rgb/--hex)",
    )
    ap.add_argument(
        "--key",
        type=int,
        nargs=2,
        metavar=("ROW", "COL"),
        help="Set a single key at ROW COL (requires --rgb/--hex)",
    )
    ap.add_argument(
        "--reset",
        action="store_true",
        help="Reset all RGB to original",
    )
    ap.add_argument(
        "--reset-key",
        type=int,
        nargs=2,
        metavar=("ROW", "COL"),
        help="Reset a single key at ROW COL",
    )
    ap.add_argument(
        "--direct",
        action="store_true",
        help="Use direct set key (does not touch the color array)",
    )
    ap.add_argument(
        "--effect",
        choices=["breathe", "rainbow", "scanner"],
        help="Run an effect (Ctrl-C to stop)",
    )
    ap.add_argument(
        "--fps",
        type=float,
        default=30.0,
        help="Effect update rate (default: %(default)s)",
    )
    ap.add_argument(
        "--seconds",
        type=float,
        default=0.0,
        help="Effect duration; 0 means forever (default: %(default)s)",
    )
    ap.add_argument(
        "--restore",
        action="store_true",
        help="Restore original/profile colors on exit (calls wooting_rgb_close)",
    )

    args = ap.parse_args()

    sdk = WootingRgb()
    count = sdk.device_count()

    if args.list:
        if count == 0:
            print("No Wooting RGB devices found")
            return 1
        for i in range(count):
            info = sdk.get_device_info(i)
            print(
                f"{i}: connected={info.connected} model={info.model!r} rows={info.max_rows} cols={info.max_columns}"
            )
        return 0

    if count == 0:
        raise SystemExit("No Wooting RGB devices found (try --list)")

    def parse_devices(s: Optional[str]) -> list[int]:
        if not s:
            return [int(args.device)]
        out: list[int] = []
        for part in s.split(","):
            part = part.strip()
            if not part:
                continue
            out.append(int(part))
        # De-dupe while preserving order
        seen: set[int] = set()
        uniq: list[int] = []
        for d in out:
            if d not in seen:
                uniq.append(d)
                seen.add(d)
        return uniq

    devices = parse_devices(args.devices)
    if not devices:
        raise SystemExit("No devices specified")
    for d in devices:
        if d < 0 or d >= count:
            raise SystemExit(f"Device index {d} out of range (0..{count-1})")

    sdk.set_auto_update(bool(args.auto_update))

    if args.info:
        for d in devices:
            sdk.select_device(d)
            info = sdk.get_device_info(d)
            print(info)

    if args.reset:
        for d in devices:
            sdk.select_device(d)
            sdk.reset()
        return 0

    if args.reset_key is not None:
        row, col = args.reset_key
        for d in devices:
            sdk.select_device(d)
            sdk.reset_key(row, col)
        return 0

    do_color = bool(args.all or args.key is not None or args.effect is not None)
    rgb = parse_rgb(args) if do_color else None

    # Determine matrix size. If multiple devices differ, use each device's
    # reported max rows/cols.
    per_device_rc: dict[int, tuple[int, int]] = {}
    for d in devices:
        sdk.select_device(d)
        info = sdk.get_device_info(d)
        per_device_rc[d] = (max(1, info.max_rows), max(1, info.max_columns))

    if args.all:
        assert rgb is not None
        for d in devices:
            sdk.select_device(d)
            rows, cols = per_device_rc[d]
            for row, col in iter_all_cells(rows, cols):
                sdk.set_key(row, col, rgb, direct=False)
            sdk.update()
        return 0

    if args.key is not None:
        assert rgb is not None
        row, col = args.key
        for d in devices:
            sdk.select_device(d)
            sdk.set_key(row, col, rgb, direct=bool(args.direct))
            if not args.direct:
                sdk.update()
        return 0

    if args.effect is None:
        ap.print_help()
        return 2

    assert rgb is not None

    dt = 1.0 / max(1.0, float(args.fps))
    t0 = time.monotonic()

    def time_left() -> bool:
        if args.seconds <= 0.0:
            return True
        return (time.monotonic() - t0) < args.seconds

    try:
        if args.effect == "breathe":
            base_r, base_g, base_b = rgb
            phase = 0.0
            while time_left():
                # Simple sine-ish triangle wave
                x = abs((phase % 2.0) - 1.0)
                scale = 0.1 + (0.9 * x)
                c = (int(base_r * scale), int(base_g * scale), int(base_b * scale))
                for d in devices:
                    sdk.select_device(d)
                    rows, cols = per_device_rc[d]
                    for row, col in iter_all_cells(rows, cols):
                        sdk.set_key(row, col, c, direct=False)
                    sdk.update()
                phase += dt
                time.sleep(dt)

        elif args.effect == "rainbow":
            phase = 0.0
            while time_left():
                for d in devices:
                    sdk.select_device(d)
                    rows, cols = per_device_rc[d]
                    for row, col in iter_all_cells(rows, cols):
                        h = (phase + (col / max(1, cols))) % 1.0
                        c = hsv_to_rgb(h, 1.0, 0.5)
                        sdk.set_key(row, col, c, direct=False)
                    sdk.update()
                phase = (phase + 0.01) % 1.0
                time.sleep(dt)

        elif args.effect == "scanner":
            off = (0, 0, 0)
            scanner_cols = min((c for (_r, c) in per_device_rc.values()), default=1)
            pos = 0
            direction = 1
            while time_left():
                for d in devices:
                    sdk.select_device(d)
                    rows, cols = per_device_rc[d]
                    for row, col in iter_all_cells(rows, cols):
                        sdk.set_key(row, col, off, direct=False)
                    col = pos
                    for row in range(rows):
                        sdk.set_key(row, col, rgb, direct=False)
                    sdk.update()

                pos += direction
                if pos <= 0:
                    pos = 0
                    direction = 1
                elif pos >= scanner_cols - 1:
                    pos = scanner_cols - 1
                    direction = -1
                time.sleep(dt)

    except KeyboardInterrupt:
        return 0
    finally:
        if args.restore:
            sdk.close()

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
