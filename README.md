# wooting-xen

Small helper scripts for monitoring MIDI coming from the Wooting headless daemon.

See `InstallRGB.md` for Raspberry Pi + Wooting RGB SDK notes.

## Monitor MIDI

For ALSA *input* ports (e.g. the old `Wooting Analog MIDI` daemon):

The printed prefix is a 0-based MIDI channel, e.g. `[0]`, `[1]`.

Run:

```bash
python3 monitor_wooting_midi.py
```

For ALSA *output* ports (e.g. `XenWooting`), use `aseqdump` mode:

```bash
python3 monitor_wooting_midi.py --dump-alsa-out --port XenWooting
```

List ports:

```bash
python3 monitor_wooting_midi.py --list
```

Show raw bytes:

```bash
python3 monitor_wooting_midi.py --raw
```

Select a different port by substring:

```bash
python3 monitor_wooting_midi.py --port "Wooting"
```

## Power/USB Watch

Prints `vcgencmd get_throttled` + core voltage/temperature periodically. Useful to catch undervoltage or USB power problems.

```bash
./power_watch.sh
```

Change interval:

```bash
INTERVAL_SEC=1 ./power_watch.sh
```

## Wooting RGB Control

Uses the Wooting RGB SDK (`libwooting-rgb-sdk.so`) to set LED colors per device.

List devices:

```bash
python3 wooting_rgb.py --list
```

Set all keys on device 0 to red:

```bash
python3 wooting_rgb.py --device 0 --all --rgb 255 0 0
```

Set all keys on devices 0 and 1:

```bash
python3 wooting_rgb.py --devices 0,1 --all --hex 00FF00
```

Set a single matrix cell (row/col) on device 1:

```bash
python3 wooting_rgb.py --device 1 --key 0 0 --hex 00FFAA
```

Set a single key on multiple devices:

```bash
python3 wooting_rgb.py --devices 0,1 --key 0 0 --hex FF00FF --direct
```

Reset:

```bash
python3 wooting_rgb.py --device 0 --reset
```

By default the script leaves your custom colors active. To restore the original
profile colors on exit (for effects), pass `--restore`.

Effects (Ctrl-C to stop):

```bash
python3 wooting_rgb.py --device 0 --effect rainbow --rgb 255 0 0
python3 wooting_rgb.py --device 0 --effect breathe --hex 00A0FF
python3 wooting_rgb.py --device 0 --effect scanner --hex FF0000
```

## Identify Matrix Coordinates

Step through `(row,col)` positions and light one at a time (press Enter to advance). Useful for building a physical key map.

```bash
./step_coords.py --devices 0,1
```

If Enter doesn't advance in your terminal, use single-key mode (default) or force a specific key:

```bash
./step_coords.py --devices 0,1 --advance any
./step_coords.py --devices 0,1 --advance space
./step_coords.py --devices 0,1 --advance enter
```

If you're running through SSH / tmux / redirected stdin, this script reads from `/dev/tty` so keypresses still work.

Use a different highlight color and reset to profile colors on exit:

```bash
./step_coords.py --devices 0,1 --color FF00FF --reset-on-exit
```

## RGB Power Test (Best Effort)

Runs a quick test: baseline -> full white -> reset, printing Pi PMIC rail readings and looking for undervoltage/USB errors.

```bash
./rgb_power_test.sh
```

Target specific devices / change settle time:

```bash
DEVICES=0,1 SETTLE_SEC=5 ./rgb_power_test.sh
```
