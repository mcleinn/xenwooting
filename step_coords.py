#!/usr/bin/env python3

import argparse
import os
import sys
import termios
import tty
from typing import Optional, Tuple

from wooting_rgb import WootingRgb


def parse_devices(devices: Optional[str], device: int) -> list[int]:
    if devices is None:
        return [device]
    out: list[int] = []
    for part in devices.split(","):
        part = part.strip()
        if part:
            out.append(int(part))
    seen: set[int] = set()
    uniq: list[int] = []
    for d in out:
        if d not in seen:
            uniq.append(d)
            seen.add(d)
    return uniq


def parse_hex_color(s: str) -> Tuple[int, int, int]:
    s = s.strip().lstrip("#")
    if len(s) != 6:
        raise SystemExit("--hex must be RRGGBB")
    return (int(s[0:2], 16), int(s[2:4], 16), int(s[4:6], 16))


def read_key_from_tty() -> str:
    fd = os.open("/dev/tty", os.O_RDONLY)
    try:
        old = termios.tcgetattr(fd)
        try:
            tty.setraw(fd)
            b = os.read(fd, 1)
            return b.decode("utf-8", errors="ignore")
        finally:
            termios.tcsetattr(fd, termios.TCSADRAIN, old)
    finally:
        os.close(fd)


def main() -> int:
    ap = argparse.ArgumentParser(description="Step through Wooting RGB (row,col) and light one cell.")
    ap.add_argument("--device", type=int, default=0, help="RGB device index (0-based)")
    ap.add_argument("--devices", type=str, default=None, help="Comma-separated device indices")
    ap.add_argument("--hex", type=str, default="FF00FF", help="Highlight color RRGGBB")
    ap.add_argument("--rows", type=int, default=None, help="Override row count")
    ap.add_argument("--cols", type=int, default=None, help="Override col count")
    ap.add_argument("--advance", choices=["any", "space", "enter"], default="enter")
    args = ap.parse_args()

    sdk = WootingRgb()
    count = sdk.device_count()
    if count <= 0:
        raise SystemExit("No RGB devices found")

    devs = parse_devices(args.devices, args.device)
    for d in devs:
        if d < 0 or d >= count:
            raise SystemExit(f"Device {d} out of range (0..{count-1})")

    # Determine a common rows/cols to iterate.
    rows = args.rows
    cols = args.cols
    if rows is None or cols is None:
        r_min = None
        c_min = None
        for d in devs:
            sdk.select_device(d)
            info = sdk.get_device_info(d)
            r = int(info.max_rows)
            c = int(info.max_columns)
            r_min = r if r_min is None else min(r_min, r)
            c_min = c if c_min is None else min(c_min, c)
        if rows is None:
            rows = int(r_min or 6)
        if cols is None:
            cols = int(c_min or 14)

    rgb = parse_hex_color(args.hex)
    row = 0
    col = 0
    prev = None

    def set_key(r: int, c: int, color: Tuple[int, int, int]) -> None:
        for d in devs:
            sdk.select_device(d)
            sdk.set_key(r, c, color, direct=True)

    def reset_key(r: int, c: int) -> None:
        for d in devs:
            sdk.select_device(d)
            sdk.reset_key(r, c)

    sys.stdout.write("q=quit\n")
    sys.stdout.flush()

    try:
        while True:
            if prev is not None:
                reset_key(prev[0], prev[1])
            set_key(row, col, rgb)
            prev = (row, col)
            sys.stdout.write(f"(row,col)=({row},{col})\n")
            sys.stdout.flush()

            while True:
                ch = read_key_from_tty()
                if ch in ("q", "Q"):
                    return 0
                if args.advance == "any":
                    break
                if args.advance == "space" and ch == " ":
                    break
                if args.advance == "enter" and ch in ("\r", "\n"):
                    break

            col += 1
            if col >= int(cols):
                col = 0
                row += 1
                if row >= int(rows):
                    row = 0
    finally:
        if prev is not None:
            try:
                reset_key(prev[0], prev[1])
            except Exception:
                pass


if __name__ == "__main__":
    raise SystemExit(main())
