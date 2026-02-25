#!/usr/bin/env bash
set -euo pipefail

# Best-effort power test for Pi 5 + Wooting keyboards.
#
# Notes:
# - Raspberry Pi 5 exposes PMIC rail volt/current via `vcgencmd pmic_read_adc`,
#   but it does NOT report USB 5V peripheral current. So we cannot directly
#   measure per-keyboard power draw in software.
# - This script therefore reports:
#   * Wooting USB descriptors (bMaxPower)
#   * Pi throttling/undervoltage flags
#   * Pi internal rail power estimate (sum of PMIC rails)
#   * Kernel log power/USB errors during the test

DEVICES="${DEVICES:-0,1}"
SETTLE_SEC="${SETTLE_SEC:-3}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RGBCTL="${RGBCTL:-${SCRIPT_DIR}/wooting_rgb.py}"

if ! command -v vcgencmd >/dev/null 2>&1; then
  echo "ERROR: vcgencmd not found" >&2
  exit 1
fi
if ! command -v python3 >/dev/null 2>&1; then
  echo "ERROR: python3 not found" >&2
  exit 1
fi
if [[ ! -x "$RGBCTL" ]]; then
  echo "ERROR: RGB control script not executable: $RGBCTL" >&2
  echo "Hint: chmod +x $RGBCTL" >&2
  exit 1
fi

# journalctl --since does not accept all ISO-8601 variants (notably timezones
# like "+00:00"), so use a conservative format.
start_ts="$(date '+%Y-%m-%d %H:%M:%S')"

wooting_usb_info() {
  echo "-- Wooting USB descriptor (bMaxPower) --"
  local found=0
  for d in /sys/bus/usb/devices/*; do
    if [[ -f "$d/manufacturer" ]] && grep -qi wooting "$d/manufacturer" 2>/dev/null; then
      found=1
      echo "${d##*/}: $(tr -d '\n' <"$d/manufacturer") / $(tr -d '\n' <"$d/product") bMaxPower=$(tr -d '\n' <"$d/bMaxPower" 2>/dev/null || echo "?")"
    fi
  done
  if [[ "$found" -eq 0 ]]; then
    echo "No Wooting USB devices found in /sys/bus/usb/devices"
  fi
  echo
}

pmic_snapshot() {
  # Print + compute a rough internal rail power sum.
  # We match rails by name prefix, e.g. VDD_CORE_A with VDD_CORE_V.
  local tmp
  tmp="$(mktemp)"
  vcgencmd pmic_read_adc >"$tmp" || true

  echo "-- PMIC rails (vcgencmd pmic_read_adc) --"
  cat "$tmp"
  echo

  awk '
    function trim(s){ sub(/^ +/,"",s); sub(/ +$/,"",s); return s }
    BEGIN { FS="=" }
    {
      # Example: "  VDD_CORE_A current(7)=3.15229000A"
      name=trim($1)
      val=$2
      gsub(/A$/,"",val)
      gsub(/V$/,"",val)
      if (name ~ /_A current\(/) {
        gsub(/_A current\(.*/,"",name)
        amps[name]=val+0
      } else if (name ~ /_V volt\(/) {
        gsub(/_V volt\(.*/,"",name)
        volts[name]=val+0
      }
    }
    END {
      sum=0
      for (k in amps) {
        if (k in volts) {
          sum += amps[k]*volts[k]
        }
      }
      printf("PMIC internal rails power estimate: %.3f W\n", sum)
    }
  ' "$tmp"

  rm -f "$tmp"
}

throttled_snapshot() {
  echo "-- Throttle flags --"
  vcgencmd get_throttled || true
  echo
}

kernel_issues_since_start() {
  echo "-- Kernel power/USB issues during test --"
  if command -v journalctl >/dev/null 2>&1; then
    journalctl -k --since "$start_ts" --no-pager \
      | grep -iE 'over-current|overcurrent|under-voltage|undervoltage|throttl|usb.*reset|usb.*disconnect|usb.*error' \
      || true
  else
    dmesg -T | grep -iE 'over-current|overcurrent|under-voltage|undervoltage|throttl|usb.*reset|usb.*disconnect|usb.*error' || true
  fi
  echo
}

measure_block() {
  local label="$1"
  echo "== $label =="
  echo "Time: $(date --iso-8601=seconds)"
  throttled_snapshot
  wooting_usb_info
  pmic_snapshot
  echo
}

echo "== RGB Power Test =="
echo "Start: $start_ts"
echo "Devices: $DEVICES"
echo "Settle seconds: $SETTLE_SEC"
echo
echo "NOTE: USB peripheral current is not readable in software on Pi 5; bMaxPower is the USB descriptor, not a measurement."
echo

python3 "$RGBCTL" --list || true
echo

measure_block "Baseline (profile colors)"

echo "Setting both keyboards to FULL WHITE (255,255,255) ..."
python3 "$RGBCTL" --devices "$DEVICES" --all --rgb 255 255 255
sleep "$SETTLE_SEC"
measure_block "Full white"

echo "Resetting both keyboards to profile/original colors ..."
python3 "$RGBCTL" --devices "$DEVICES" --reset
sleep "$SETTLE_SEC"
measure_block "After reset"

kernel_issues_since_start

echo "Done."
