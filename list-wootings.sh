#!/usr/bin/env bash
set -euo pipefail

BIN="${BIN:-wooting-analog-midi-headless}"

echo "== Wooting Device Check =="
echo "Date: $(date)"
echo "User: $(id -un) (uid=$(id -u))"
echo "Groups: $(id -nG)"
echo

if ! command -v "$BIN" >/dev/null 2>&1; then
  echo "ERROR: '$BIN' not found in PATH"
  echo "Hint: if installed via this project, it should be at /usr/local/bin/wooting-analog-midi-headless"
  exit 1
fi

echo "Binary: $(command -v "$BIN")"
echo

if [[ "$(id -u)" -eq 0 ]]; then
  echo "WARNING: running as root; this script is intended for your normal user"
  echo
fi

echo "-- Device list (current user) --"
"$BIN" --list-devices || true
echo

echo "-- hidraw permissions (root-only is a common issue) --"
ls -la /dev/hidraw* 2>/dev/null || echo "No /dev/hidraw* devices found"
echo

echo "-- ALSA MIDI ports --"
if command -v aconnect >/dev/null 2>&1; then
  aconnect -l || true
else
  echo "aconnect not installed"
fi
