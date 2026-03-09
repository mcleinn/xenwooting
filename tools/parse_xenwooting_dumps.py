#!/usr/bin/env python3
"""Parse xenwooting journalctl dumps into per-dump CSV + text.

It looks for complete blocks:
  DBG_RING_BEGIN ...
  DBG <t_ms>ms <event>
  ...
  DBG_RING_END

By default it runs:
  journalctl -u xenwooting.service --no-pager

Outputs one CSV per dump named after the dump wall timestamp.
Also writes a text file per dump with non-numeric / unparsed lines.
"""

from __future__ import annotations

import argparse
import csv
import os
import re
import subprocess
import sys
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Dict, Iterable, List, Optional, Tuple


RE_MSG = re.compile(r"\] (?P<msg>DBG(?:_RING_BEGIN|_RING_END)?\b.*)$")
RE_ISO = re.compile(r"\[(?P<iso>\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z)")

RE_DBG_LINE = re.compile(r"^DBG\s+(?P<t_ms>\d+)ms\s+(?P<body>.*)$")

RE_EDGE = re.compile(
    r"^EDGE\s+(?P<kind>down|up|update)\s+dev=(?P<dev>\d+)\s+hid=(?P<hid>[^\s]+)\s+analog=(?P<analog>\d+(?:\.\d+)?)$"
)

RE_NOTEON_TICK = re.compile(
    r"^NOTEON_TICK\s+dev=(?P<dev>\d+)\s+hid=(?P<hid>[^\s]+)\s+age_ms=(?P<age_ms>\d+)\s+playing=(?P<playing>true|false)\s+pressed=(?P<pressed>true|false)\s+peak=(?P<peak>\d+(?:\.\d+)?)\s+last=(?P<last>\d+(?:\.\d+)?)\s+peak_speed=(?P<peak_speed>\d+(?:\.\d+)?)$"
)

RE_TAP_NOTE = re.compile(
    r"^TAP_NOTE\s+dev=(?P<dev>\d+)\s+hid=(?P<hid>[^\s]+)\s+ch=(?P<ch>\d+)\s+note=(?P<note>\d+)\s+vel=(?P<vel>\d+)\s+age_ms=(?P<age_ms>\d+)\s+peak=(?P<peak>\d+(?:\.\d+)?)\s+peak_speed=(?P<peak_speed>\d+(?:\.\d+)?)$"
)

RE_MIDI_NOTEON_OK = re.compile(
    r"^MIDI\s+noteon\s+ok(?:\s+\(tap\))?\s+dev=(?P<dev>\d+)\s+hid=(?P<hid>[^\s]+)\s+ch=(?P<ch>\d+)\s+note=(?P<note>\d+)\s+vel=(?P<vel>\d+)$"
)

RE_MIDI_NOTEOFF_OK = re.compile(
    r"^MIDI\s+noteoff\s+ok(?:\s+\(fallback\))?\s+dev=(?P<dev>\d+)\s+hid=(?P<hid>[^\s]+)\s+ch=(?P<ch>\d+)\s+note=(?P<note>\d+)$"
)

RE_MIDI_NOTEOFF_SCHED = re.compile(
    r"^MIDI\s+noteoff\s+ok\s+\(scheduled\)\s+ch=(?P<ch>\d+)\s+note=(?P<note>\d+)$"
)

RE_POLL_RAPID_RELEASE = re.compile(
    r"^POLL\s+rapid_release\s+device_id=(?P<dev>\d+)\s+hid=(?P<hid>[^\s]+)\s+peak=(?P<peak>\d+(?:\.\d+)?)\s+analog=(?P<analog>\d+(?:\.\d+)?)\s+delta=(?P<delta>\d+(?:\.\d+)?)\s+thr_down=(?P<thr_down>\d+(?:\.\d+)?)\s+thr_up=(?P<thr_up>\d+(?:\.\d+)?)$"
)


def safe_slug(s: str) -> str:
    return re.sub(r"[^A-Za-z0-9_.-]+", "_", s).strip("_")


def parse_kv(s: str) -> Dict[str, str]:
    out: Dict[str, str] = {}
    for tok in s.split():
        if "=" not in tok:
            continue
        k, v = tok.split("=", 1)
        out[k] = v
    return out


def parse_iso_from_line(line: str) -> Optional[str]:
    m = RE_ISO.search(line)
    if not m:
        return None
    return m.group("iso")


@dataclass
class DumpMeta:
    wall_iso: str
    reason: str
    dumps_counter: str
    rgb_drop_critical: str
    cpu_u_ms: str
    cpu_s_ms: str


@dataclass
class Dump:
    meta: DumpMeta
    raw_lines: List[str]
    rows: List[Dict[str, str]]
    unparsed: List[str]
    cfg_lines: List[str]


def extract_dbg_message(line: str) -> Optional[str]:
    m = RE_MSG.search(line)
    if not m:
        return None
    return m.group("msg")


def parse_event(
    body: str,
    dump_meta: DumpMeta,
    t_ms: str,
) -> Tuple[Optional[Dict[str, str]], Optional[str]]:
    # Returns: (csv_row, unparsed_line_if_any)
    base = {
        "dump_wall_iso": dump_meta.wall_iso,
        "dump_reason": dump_meta.reason,
        "dump_counter": dump_meta.dumps_counter,
        "dump_rgb_drop_critical": dump_meta.rgb_drop_critical,
        "dump_cpu_u_ms": dump_meta.cpu_u_ms,
        "dump_cpu_s_ms": dump_meta.cpu_s_ms,
        "t_ms": t_ms,
    }

    m = RE_EDGE.match(body)
    if m:
        row = dict(base)
        row.update(
            {
                "event": "EDGE",
                "kind": m.group("kind"),
                "device_id": m.group("dev"),
                "hid": m.group("hid"),
                "analog": m.group("analog"),
            }
        )
        return row, None

    m = RE_NOTEON_TICK.match(body)
    if m:
        row = dict(base)
        row.update(
            {
                "event": "NOTEON_TICK",
                "device_id": m.group("dev"),
                "hid": m.group("hid"),
                "age_ms": m.group("age_ms"),
                "playing": m.group("playing"),
                "pressed": m.group("pressed"),
                "peak": m.group("peak"),
                "last": m.group("last"),
                "peak_speed": m.group("peak_speed"),
            }
        )
        return row, None

    m = RE_TAP_NOTE.match(body)
    if m:
        row = dict(base)
        row.update(
            {
                "event": "TAP_NOTE",
                "device_id": m.group("dev"),
                "hid": m.group("hid"),
                "ch": m.group("ch"),
                "note": m.group("note"),
                "vel": m.group("vel"),
                "age_ms": m.group("age_ms"),
                "peak": m.group("peak"),
                "peak_speed": m.group("peak_speed"),
            }
        )
        return row, None

    m = RE_MIDI_NOTEON_OK.match(body)
    if m:
        row = dict(base)
        row.update(
            {
                "event": "MIDI_NOTEON_OK",
                "device_id": m.group("dev"),
                "hid": m.group("hid"),
                "ch": m.group("ch"),
                "note": m.group("note"),
                "vel": m.group("vel"),
            }
        )
        return row, None

    m = RE_MIDI_NOTEOFF_OK.match(body)
    if m:
        row = dict(base)
        row.update(
            {
                "event": "MIDI_NOTEOFF_OK",
                "device_id": m.group("dev"),
                "hid": m.group("hid"),
                "ch": m.group("ch"),
                "note": m.group("note"),
            }
        )
        return row, None

    m = RE_MIDI_NOTEOFF_SCHED.match(body)
    if m:
        row = dict(base)
        row.update(
            {
                "event": "MIDI_NOTEOFF_SCHEDULED_OK",
                "ch": m.group("ch"),
                "note": m.group("note"),
            }
        )
        return row, None

    m = RE_POLL_RAPID_RELEASE.match(body)
    if m:
        row = dict(base)
        row.update(
            {
                "event": "POLL_RAPID_RELEASE",
                "device_id": m.group("dev"),
                "hid": m.group("hid"),
                "peak": m.group("peak"),
                "analog": m.group("analog"),
                "delta": m.group("delta"),
                "thr_down": m.group("thr_down"),
                "thr_up": m.group("thr_up"),
            }
        )
        return row, None

    # Not a numeric row; keep for text.
    return None, body


def parse_dumps(lines: Iterable[str]) -> List[Dump]:
    dumps: List[Dump] = []
    current_meta: Optional[DumpMeta] = None
    raw_lines: List[str] = []
    rows: List[Dict[str, str]] = []
    unparsed: List[str] = []
    cfg_lines: List[str] = []

    for line in lines:
        msg = extract_dbg_message(line)
        if not msg:
            continue

        if msg.startswith("DBG_RING_BEGIN"):
            # Start a new dump block.
            current_meta = None
            raw_lines = [line.rstrip("\n")]
            rows = []
            unparsed = []
            cfg_lines = []

            wall_iso = parse_iso_from_line(line) or datetime.now(timezone.utc).strftime(
                "%Y-%m-%dT%H:%M:%SZ"
            )
            kv = parse_kv(msg[len("DBG_RING_BEGIN") :].strip())
            current_meta = DumpMeta(
                wall_iso=wall_iso,
                reason=kv.get("reason", ""),
                dumps_counter=kv.get("dumps", ""),
                rgb_drop_critical=kv.get("rgb_drop_critical", ""),
                cpu_u_ms=kv.get("cpu_u_ms", ""),
                cpu_s_ms=kv.get("cpu_s_ms", ""),
            )
            continue

        if msg.startswith("DBG_RING_END"):
            if current_meta is not None:
                raw_lines.append(line.rstrip("\n"))
                dumps.append(
                    Dump(
                        meta=current_meta,
                        raw_lines=list(raw_lines),
                        rows=list(rows),
                        unparsed=list(unparsed),
                        cfg_lines=list(cfg_lines),
                    )
                )
            current_meta = None
            raw_lines = []
            rows = []
            unparsed = []
            cfg_lines = []
            continue

        # Inside a dump.
        if current_meta is None:
            continue

        raw_lines.append(line.rstrip("\n"))
        m = RE_DBG_LINE.match(msg)
        if not m:
            # Unexpected; keep it.
            unparsed.append(msg)
            continue

        t_ms = m.group("t_ms")
        body = m.group("body")
        row, extra = parse_event(body, current_meta, t_ms)
        if row is not None:
            rows.append(row)
        if extra is not None:
            unparsed.append(extra)
            if extra.startswith("CFG "):
                cfg_lines.append(extra)

    return dumps


def run_journalctl(unit: str, since: Optional[str], until: Optional[str]) -> List[str]:
    cmd = ["journalctl", "-u", unit, "--no-pager"]
    if since:
        cmd += ["--since", since]
    if until:
        cmd += ["--until", until]
    proc = subprocess.run(cmd, check=False, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    if proc.returncode != 0:
        raise RuntimeError(proc.stderr.strip() or f"journalctl failed with code {proc.returncode}")
    return proc.stdout.splitlines()


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--unit", default="xenwooting.service", help="systemd unit (default: xenwooting.service)")
    ap.add_argument("--since", default=None, help='journalctl --since (e.g. "1 hour ago")')
    ap.add_argument("--until", default=None, help='journalctl --until')
    ap.add_argument("--input", default=None, help="Read from a file instead of journalctl")
    ap.add_argument("--out", default="dump_csv", help="Output directory")
    args = ap.parse_args()

    out_dir = Path(args.out)
    out_dir.mkdir(parents=True, exist_ok=True)

    if args.input:
        text = Path(args.input).read_text(encoding="utf-8", errors="replace").splitlines()
    else:
        text = run_journalctl(args.unit, args.since, args.until)

    dumps = parse_dumps(text)
    if not dumps:
        print("No complete DBG_RING dumps found.", file=sys.stderr)
        return 2

    # Determine CSV columns.
    cols: List[str] = []
    for d in dumps:
        for r in d.rows:
            for k in r.keys():
                if k not in cols:
                    cols.append(k)

    # Stable-ish ordering.
    preferred = [
        "dump_wall_iso",
        "dump_reason",
        "dump_counter",
        "dump_rgb_drop_critical",
        "dump_cpu_u_ms",
        "dump_cpu_s_ms",
        "t_ms",
        "event",
        "kind",
        "device_id",
        "hid",
        "analog",
        "age_ms",
        "peak",
        "last",
        "peak_speed",
        "pressed",
        "playing",
        "ch",
        "note",
        "vel",
        "delta",
        "thr_down",
        "thr_up",
    ]
    cols_sorted = [c for c in preferred if c in cols] + [c for c in cols if c not in preferred]

    seen_bases: Dict[str, int] = {}

    for d in dumps:
        ts_slug = safe_slug(d.meta.wall_iso.replace(":", "-"))
        reason_slug = safe_slug(d.meta.reason) or "dump"
        ctr_slug = safe_slug(d.meta.dumps_counter) or "0"
        base = f"{ts_slug}_{reason_slug}_{ctr_slug}"
        n = seen_bases.get(base, 0)
        seen_bases[base] = n + 1
        if n:
            base = f"{base}_{n}"
        csv_path = out_dir / f"{base}.csv"
        txt_path = out_dir / f"{base}.txt"

        with csv_path.open("w", newline="", encoding="utf-8") as f:
            w = csv.DictWriter(f, fieldnames=cols_sorted)
            w.writeheader()
            for r in d.rows:
                w.writerow(r)

        with txt_path.open("w", encoding="utf-8") as f:
            f.write(f"wall_iso={d.meta.wall_iso}\n")
            f.write(f"reason={d.meta.reason}\n")
            f.write(f"dump_counter={d.meta.dumps_counter}\n")
            f.write(f"rgb_drop_critical={d.meta.rgb_drop_critical}\n")
            f.write(f"cpu_u_ms={d.meta.cpu_u_ms} cpu_s_ms={d.meta.cpu_s_ms}\n")
            if d.cfg_lines:
                f.write("cfg_lines=\n")
                for ln in d.cfg_lines:
                    f.write(f"  {ln}\n")
            f.write("\n-- RAW DUMP --\n")
            for ln in d.raw_lines:
                f.write(ln + "\n")
            f.write("\n-- UNPARSED DBG BODIES --\n")
            for ln in d.unparsed:
                f.write(ln + "\n")

        print(f"Wrote {csv_path} and {txt_path}")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
