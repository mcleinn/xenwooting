#!/usr/bin/env python3.12

import argparse
import json
import re
import threading
from collections import OrderedDict
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


# xenharmlib is installed for python3.12 on this machine.
from xenharmlib import EDOTuning  # type: ignore
from xenharmlib import UpDownNotation  # type: ignore


_NOTATION_LOCK = threading.Lock()
_NOTATION_BY_EDO: dict[int, tuple[EDOTuning, UpDownNotation]] = {}


_CACHE_LOCK = threading.Lock()
_NOTE_CACHE: "OrderedDict[tuple[int, int], dict[str, str] | None]" = OrderedDict()
_NOTE_CACHE_MAX = 8192


# Ported from xenassist/server.py so Bravura glyphs match.
_NOTATION_REPLACEMENTS: dict[str, str] = {
    "^": "\uE272",
    "^^": "\uE2D1",
    "^^^": "\uE2DB",
    "vvv#": "\uE2D7",
    "vv#": "\uE2CD",
    "v#": "\uE2C3",
    "#": "\uE262",
    "^#": "\uE2C8",
    "^^#": "\uE2D2",
    "^^^#": "\uE2DC",
    "vvvx": "\uE2D8",
    "vvx": "\uE2CE",
    "vx": "\uE2C4",
    "x": "\uE263",
    "^x": "\uE2C9",
    "^^x": "\uE2D3",
    "^^^x": "\uE2DD",
    "v": "\uE2C2",
    "vv": "\uE2CC",
    "vvv": "\uE2D6",
    "^^^b": "\uE2DA",
    "^^b": "\uE2D0",
    "^b": "\uE2C6",
    "b": "\uE260",
    "vb": "\uE2C1",
    "vvb": "\uE2CB",
    "vvvb": "\uE2D5",
    "^^^bb": "\uE2D9",
    "^^bb": "\uE2CF",
    "^bb": "\uE2C5",
    "bb": "\uE264",
    "vbb": "\uE2C0",
    "vvbb": "\uE2CA",
    "vvvbb": "\uE2D4",
}


_NOTE_RE = re.compile(r"^([v\^]*)([A-G])([#xb]+)?(-?\d+)?$")


def encode_notation(short_repr: str) -> str:
    m = _NOTE_RE.match(short_repr)
    if not m:
        return short_repr
    prefix, note, suffix, octave = m.groups()
    suffix = suffix or ""
    octave = octave or ""
    key = f"{prefix}{suffix}"
    return f"{note}{_NOTATION_REPLACEMENTS.get(key, '')}{octave}"


def _get_notation(edo: int) -> tuple[EDOTuning, UpDownNotation] | None:
    if edo < 5 or edo > 72:
        return None
    with _NOTATION_LOCK:
        hit = _NOTATION_BY_EDO.get(edo)
        if hit is not None:
            return hit
        try:
            tuning = EDOTuning(edo)
            notation = UpDownNotation(tuning)
        except Exception:
            return None
        _NOTATION_BY_EDO[edo] = (tuning, notation)
        return tuning, notation


def _cache_get(edo: int, pitch: int) -> dict[str, str] | None | object:
    key = (edo, pitch)
    with _CACHE_LOCK:
        if key not in _NOTE_CACHE:
            return _MISSING
        _NOTE_CACHE.move_to_end(key)
        return _NOTE_CACHE[key]


def _cache_set(edo: int, pitch: int, value: dict[str, str] | None) -> None:
    key = (edo, pitch)
    with _CACHE_LOCK:
        _NOTE_CACHE[key] = value
        _NOTE_CACHE.move_to_end(key)
        while len(_NOTE_CACHE) > _NOTE_CACHE_MAX:
            _NOTE_CACHE.popitem(last=False)


_MISSING = object()


def note_name_for_pitch(edo: int, pitch: int) -> dict[str, str] | None:
    cached = _cache_get(edo, pitch)
    if cached is not _MISSING:
        return cached  # may be None

    hit = _get_notation(edo)
    if hit is None:
        _cache_set(edo, pitch, None)
        return None
    tuning, notation = hit
    try:
        ep = tuning.pitch(pitch)
        note = notation.guess_note(ep)
        short = note.short_repr
        out = {"short": short, "unicode": encode_notation(short)}
        _cache_set(edo, pitch, out)
        return out
    except Exception:
        _cache_set(edo, pitch, None)
        return None


def _json_body(handler: BaseHTTPRequestHandler) -> dict:
    n = int(handler.headers.get("Content-Length", "0") or "0")
    raw = handler.rfile.read(n) if n > 0 else b""
    if not raw:
        return {}
    return json.loads(raw.decode("utf-8"))


def _send_json(handler: BaseHTTPRequestHandler, status: int, obj) -> None:
    data = json.dumps(obj, ensure_ascii=False).encode("utf-8")
    handler.send_response(status)
    handler.send_header("Content-Type", "application/json; charset=utf-8")
    handler.send_header("Content-Length", str(len(data)))
    handler.send_header("Cache-Control", "no-store")
    handler.end_headers()
    handler.wfile.write(data)


class Handler(BaseHTTPRequestHandler):
    server_version = "xenharm_service/0.1"

    def log_message(self, fmt: str, *args) -> None:
        # keep quiet under systemd unless something goes wrong
        return

    def do_GET(self) -> None:
        if self.path == "/health":
            _send_json(self, 200, {"ok": True})
            return
        _send_json(self, 404, {"error": "not found"})

    def do_POST(self) -> None:
        if self.path == "/v1/note-names":
            self._post_note_names()
            return
        if self.path == "/v1/scale/rotate":
            self._post_scale_rotate()
            return
        if self.path == "/v1/scale/retune":
            self._post_scale_retune()
            return
        _send_json(self, 404, {"error": "not found"})

    def _post_note_names(self) -> None:
        try:
            body = _json_body(self)
        except Exception:
            _send_json(self, 400, {"error": "invalid json"})
            return

        edo = body.get("edo")
        pitches = body.get("pitches")
        if not isinstance(edo, int) or not isinstance(pitches, list):
            _send_json(self, 400, {"error": "expected { edo: int, pitches: int[] }"})
            return

        results: dict[str, dict[str, str]] = {}
        for p in pitches:
            if not isinstance(p, int):
                continue
            nn = note_name_for_pitch(edo, p)
            if nn is None:
                continue
            results[str(p)] = nn

        _send_json(self, 200, {"edo": edo, "results": results})

    def _post_scale_rotate(self) -> None:
        try:
            body = _json_body(self)
        except Exception:
            _send_json(self, 200, {})
            return

        edo = body.get("edo")
        pitches = body.get("pitches")
        direction = body.get("direction")
        if not isinstance(edo, int) or not isinstance(pitches, list) or not isinstance(direction, int):
            _send_json(self, 200, {})
            return

        try:
            t = EDOTuning(edo)
            scale = t.scale([t.pitch(int(p)) for p in pitches if isinstance(p, int)])
            if direction < 0:
                scale2 = scale.rotated_down()
            elif direction > 0:
                scale2 = scale.rotated_up()
            else:
                scale2 = scale
            out_pitches = [int(x.pitch_index) for x in scale2]
            _send_json(self, 200, {"edo": edo, "pitches": out_pitches})
        except Exception:
            _send_json(self, 200, {})

    def _post_scale_retune(self) -> None:
        try:
            body = _json_body(self)
        except Exception:
            _send_json(self, 200, {})
            return

        edo_from = body.get("edoFrom")
        edo_to = body.get("edoTo")
        pitches = body.get("pitches")
        if not isinstance(edo_from, int) or not isinstance(edo_to, int) or not isinstance(pitches, list):
            _send_json(self, 200, {})
            return

        try:
            t_from = EDOTuning(edo_from)
            t_to = EDOTuning(edo_to)
            scale = t_from.scale([t_from.pitch(int(p)) for p in pitches if isinstance(p, int)])
            scale2 = scale.retune(t_to)
            out_pitches = [int(x.pitch_index) for x in scale2]
            _send_json(self, 200, {"edo": edo_to, "pitches": out_pitches})
        except Exception:
            _send_json(self, 200, {})


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--port", type=int, default=3199)
    args = ap.parse_args()

    httpd = ThreadingHTTPServer((args.host, args.port), Handler)
    httpd.serve_forever()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
