#!/usr/bin/env python3

import argparse
import subprocess
import time
from typing import Any, List, Optional, Tuple

import rtmidi


def list_input_ports(midi_in: Any) -> List[str]:
    ports = midi_in.get_ports()
    return list(ports) if ports else []


def pick_input_port(midi_in: Any, needle: str) -> Tuple[int, str]:
    ports = list_input_ports(midi_in)
    if not ports:
        raise SystemExit("No ALSA MIDI input ports found")

    n = needle.lower()
    for i, p in enumerate(ports):
        if n in p.lower():
            return i, p

    msg = ["Could not find a matching MIDI input port.", "Available ports:"]
    msg.extend([f"  {i}: {p}" for i, p in enumerate(ports)])
    raise SystemExit("\n".join(msg))


def msg_channel(status: int) -> Optional[int]:
    if status < 0x80:
        return None
    if status >= 0xF0:
        return None
    return status & 0x0F


def fmt_bytes(msg_bytes: List[int], show_raw: bool) -> str:
    if not msg_bytes:
        return "[-] empty"

    status = msg_bytes[0]
    ch = msg_channel(status)
    prefix = f"[{ch}]" if ch is not None else "[-]"

    # Realtime single-byte
    if status >= 0xF8:
        s = f"{prefix} realtime 0x{status:02X}"
        return s

    # SysEx
    if status == 0xF0:
        preview = " ".join(f"{b:02X}" for b in msg_bytes[:32])
        if len(msg_bytes) > 32:
            preview += " ..."
        s = f"{prefix} sysex len={len(msg_bytes)} bytes={preview}"
        return s

    mt = status & 0xF0
    d1 = msg_bytes[1] if len(msg_bytes) > 1 else None
    d2 = msg_bytes[2] if len(msg_bytes) > 2 else None

    if mt == 0x90:
        if d2 == 0:
            s = f"{prefix} note_off note={d1} vel=0"
        else:
            s = f"{prefix} note_on  note={d1} vel={d2}"
    elif mt == 0x80:
        s = f"{prefix} note_off note={d1} vel={d2}"
    elif mt == 0xA0:
        s = f"{prefix} polytouch note={d1} pressure={d2}"
    elif mt == 0xD0:
        s = f"{prefix} aftertouch pressure={d1}"
    elif mt == 0xB0:
        s = f"{prefix} cc cc={d1} val={d2}"
    elif mt == 0xE0:
        # 14-bit, center 8192
        lsb = d1 or 0
        msb = d2 or 0
        val = (msb << 7) | lsb
        s = f"{prefix} pitchwheel value={val - 8192}"
    elif mt == 0xC0:
        s = f"{prefix} program_change program={d1}"
    else:
        s = f"{prefix} unknown status=0x{status:02X}"

    if show_raw:
        s += " raw=" + " ".join(f"{b:02X}" for b in msg_bytes)
    return s


def main() -> int:
    ap = argparse.ArgumentParser(
        description=(
            "Monitor MIDI coming from the Wooting headless daemon. "
            "Prints channel as a 0-based prefix like [0] / [1]."
        )
    )
    ap.add_argument(
        "--port",
        default="Wooting Analog MIDI",
        help="Substring to match the ALSA MIDI input port name (default: %(default)s)",
    )
    ap.add_argument(
        "--dump-alsa-out",
        action="store_true",
        help=(
            "Dump events coming FROM an ALSA sequencer output port using aseqdump. "
            "Useful for xenwooting's output port (which is not an ALSA input port)."
        ),
    )
    ap.add_argument(
        "--list",
        action="store_true",
        help="List available ALSA MIDI input ports and exit",
    )
    ap.add_argument(
        "--raw",
        action="store_true",
        help="Also print raw MIDI bytes as hex",
    )
    args = ap.parse_args()

    if args.dump_alsa_out:
        # aseqdump can subscribe to an ALSA output port and print the events.
        # We resolve the port id via `aconnect -l` by substring.
        needle = args.port.lower()

        def scan_once() -> list[tuple[int, int, str]]:
            try:
                out = subprocess.check_output(["aconnect", "-l"], text=True)
            except Exception as e:
                raise SystemExit(f"Failed to run aconnect -l: {e}")

            cur_client = None
            cur_client_name = None
            found: list[tuple[int, int, str]] = []
            for line in out.splitlines():
                line = line.rstrip("\n")
                if line.startswith("client "):
                    try:
                        left, rest = line.split(":", 1)
                        cur_client = int(left.split()[1])
                        cur_client_name = rest.split("'", 2)[1] if "'" in rest else rest.strip()
                    except Exception:
                        cur_client = None
                        cur_client_name = None
                    continue

                if cur_client is None:
                    continue
                s = line.strip()
                if not s or not s[0].isdigit():
                    continue
                try:
                    port_num = int(s.split()[0])
                except Exception:
                    continue
                port_name = s.split("'", 2)[1] if "'" in s else s
                display = f"{cur_client_name}:{port_name} {cur_client}:{port_num}"
                if needle in display.lower():
                    found.append((cur_client, port_num, display))
            return found

        matches: list[tuple[int, int, str]] = []
        deadline = time.time() + 5.0
        while time.time() < deadline and not matches:
            matches = scan_once()
            if matches:
                break
            time.sleep(0.1)

        if not matches:
            raise SystemExit(
                "Could not find a matching ALSA port in `aconnect -l` output after 5s. "
                "Try running the target app first, then re-run this command."
            )

        client, port, display = matches[0]
        print(f"aseqdump -p {client}:{port}  ({display})", flush=True)
        # This blocks until Ctrl-C.
        subprocess.run(["aseqdump", "-p", f"{client}:{port}"])
        return 0

    midi_in = rtmidi.MidiIn()  # type: ignore[attr-defined]
    # python-rtmidi uses "active_sense" (not "sensing").
    # Keep all message types so we can see aftertouch, etc.
    try:
        midi_in.ignore_types(sysex=False, timing=False, active_sense=False)
    except TypeError:
        # Older/alternate signatures
        midi_in.ignore_types(False, False, False)

    if args.list:
        ports = list_input_ports(midi_in)
        if not ports:
            print("No ALSA MIDI input ports found")
            return 1
        for i, p in enumerate(ports):
            print(f"{i}: {p}")
        return 0

    idx, name = pick_input_port(midi_in, args.port)
    midi_in.open_port(idx)

    print(f"Opened input port {idx}: {name}", flush=True)
    print("Ctrl-C to stop.", flush=True)
    print(
        "Tip: with your config, keyboard #1 should be [0] and keyboard #2 should be [1].",
        flush=True,
    )
    print("", flush=True)

    try:
        while True:
            msg = midi_in.get_message()
            if msg:
                msg_bytes, _dt = msg
                ts = time.strftime("%H:%M:%S")
                print(f"{ts} {fmt_bytes(msg_bytes, args.raw)}", flush=True)
            else:
                time.sleep(0.001)
    except KeyboardInterrupt:
        return 0
    finally:
        try:
            midi_in.close_port()
        except Exception:
            pass


if __name__ == "__main__":
    raise SystemExit(main())
