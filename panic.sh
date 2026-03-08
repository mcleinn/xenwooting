#!/usr/bin/env bash
set -euo pipefail

# MIDI panic: try to stop all currently-sounding notes.
# Sends:
# - CC64  (Sustain)      = 0
# - CC120 (All Sound Off) = 0
# - CC123 (All Notes Off) = 0
# to all channels on all available MIDI output ports.

python3 - <<'PY'
import mido

outs = sorted(set(mido.get_output_names()))
print('MIDI outputs:')
for n in outs:
    print(' ', n)

for name in outs:
    try:
        out = mido.open_output(name)
    except Exception as e:
        print('Skip:', name, e)
        continue

    for ch in range(16):
        out.send(mido.Message('control_change', channel=ch, control=64, value=0))
        out.send(mido.Message('control_change', channel=ch, control=120, value=0))
        out.send(mido.Message('control_change', channel=ch, control=123, value=0))

    out.close()

print('Panic sent.')
PY
