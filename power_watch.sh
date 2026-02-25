#!/usr/bin/env bash
set -euo pipefail

INTERVAL_SEC="${INTERVAL_SEC:-2}"

if ! command -v vcgencmd >/dev/null 2>&1; then
  echo "ERROR: vcgencmd not found (this script is intended for Raspberry Pi OS)" >&2
  exit 1
fi

echo "== Raspberry Pi Power/USB Watch =="
echo "Date: $(date)"
echo "Host: $(hostname)"
echo "Interval: ${INTERVAL_SEC}s"
echo

decode_throttled() {
  # See: https://www.raspberrypi.com/documentation/computers/os.html#vcgencmd
  # Bit meanings:
  # 0 under-voltage now
  # 1 arm frequency capped now
  # 2 currently throttled
  # 3 soft temperature limit now
  # 16 under-voltage has occurred
  # 17 arm frequency capped has occurred
  # 18 throttling has occurred
  # 19 soft temperature limit has occurred
  local hex="$1"
  local v=$((hex))
  local -a out=()

  (( v & 0x1 )) && out+=("UNDER_VOLTAGE_NOW")
  (( v & 0x2 )) && out+=("FREQ_CAPPED_NOW")
  (( v & 0x4 )) && out+=("THROTTLED_NOW")
  (( v & 0x8 )) && out+=("SOFT_TEMP_LIMIT_NOW")

  (( v & 0x10000 )) && out+=("UNDER_VOLTAGE_PAST")
  (( v & 0x20000 )) && out+=("FREQ_CAPPED_PAST")
  (( v & 0x40000 )) && out+=("THROTTLED_PAST")
  (( v & 0x80000 )) && out+=("SOFT_TEMP_LIMIT_PAST")

  if ((${#out[@]} == 0)); then
    echo "OK"
  else
    (IFS=","; echo "${out[*]}")
  fi
}

echo "-- Current USB Wooting devices (bMaxPower) --"
for p in /sys/bus/usb/devices/*/product; do
  if grep -qi wooting "$p" 2>/dev/null; then
    d=${p%/product}
    echo "${d##*/}: $(cat "$d/manufacturer" 2>/dev/null) $(cat "$d/product" 2>/dev/null) bMaxPower=$(cat "$d/bMaxPower" 2>/dev/null || echo "?")"
  fi
done
echo

echo "Watching... (Ctrl-C to stop)"
echo

while true; do
  ts="$(date +"%Y-%m-%d %H:%M:%S")"
  thr="$(vcgencmd get_throttled | awk -F= '{print $2}')"
  core_v="$(vcgencmd measure_volts core 2>/dev/null | awk -F= '{print $2}' || true)"
  temp="$(vcgencmd measure_temp 2>/dev/null | awk -F= '{print $2}' || true)"

  # Bash base conversion: 0x... is OK in arithmetic context
  status="$(decode_throttled "$thr")"
  printf "%s throttled=%s status=%s core=%s temp=%s\n" "$ts" "$thr" "$status" "${core_v:-?}" "${temp:-?}"

  sleep "$INTERVAL_SEC"
done
