# XenWTN (wooting-xen)

XenWTN turns multiple Wooting 60HE V2 (analog hall effect keyboards popular with gamers: https://wooting.io/wooting-60he-v2) into a configurable microtonal music controller.
It lets you edit per-key pitch + color layouts in a web UI, then plays them as multi-channel MIDI and provides an MTS-ESP master for compatible synths, such as free Amsynth (multi-channel branch: https://github.com/mcleinn/amsynth_multichannel) or commercial Pianotech synth. 
The project is intended to be installed on a Raspberry Pi with a PiSound soundcard and a recent Patchbox OS (https://community.blokas.io/t/beta-patchbox-os-bookworm-arm64-2024-04-04/5163), but will likely run on any Linux. 

This repository contains three main components:

- `xenwooting/`: Rust daemon that reads analog key events, maps keys to a WTN grid via `.wtn` layout files, outputs MIDI, and optionally drives per-key RGB.
- `webconfigurator/`: Local web app (Node backend + React frontend) to edit `.wtn` layouts, preview changes, and import `.ltn` layouts.
- `xenharm_service/`: Localhost-only Python service that wraps `xenharmlib` to generate readable note names (including Unicode glyph variants).

## For Microtonal Musicians

Features (musician-facing):

- Multiple tunings / layouts: store several `.wtn` layouts (e.g. 12-EDO, 19-EDO, 31-EDO, 53-EDO) and switch between them.
- Two-board support: use one or two keyboards as `Board0` / `Board1` with independent layouts.
- Microtonal pitch via MIDI channel + note: pitch is encoded as `(channel-1)*edo + note + pitch_offset` (16 channels x 128 notes).
- MTS-ESP master: the daemon hosts an MTS-ESP shared-memory master so compatible synths can follow your current tuning.
- Per-key colors: key LEDs show your layout colors; presses can be highlighted.
- Layout import: import `.ltn` Lumatone layout files into `.wtn` layout files and place/rotate them on the target key grid.
- Note labels: optional on-key labels (Unicode note spelling) and tooltips (ASCII spellings).
- Preview mode: audition edits without permanently saving to disk.
- Isomorphic layout check: the editor can flag boards that are not isomorphic (grid direction steps are inconsistent).

## Required Hardware

- An analog keyboard supported by the vendor Analog SDK (commonly used: Wooting 60HE/Two HE class devices).
- Optional: a second compatible keyboard for `Board1`.
- Linux machine (tested in headless setups); ALSA MIDI available.

RGB is optional, but requires the vendor RGB SDK. See `InstallRGB.md`.

## Web App Addresses

When the configurator server is running (default):

- Configurator UI: `http://<host>:3174/wtn/`
- Board geometry/debug page: `http://<host>:3174/wtn/boards`
- Configurator API base: `http://<host>:3174/wtn/api/`

The note-name service runs locally by default:

- XenHarm health: `http://127.0.0.1:3199/health`

Tip: using a reverse proxy (e.g. Caddy) makes this easier to access from phones/tablets on your LAN.

Example Caddyfile snippet:

```caddyfile
xenwtn.local {
  reverse_proxy 127.0.0.1:3174
}
```

Then use: `http://xenwtn.local/wtn/`

## Installation (High Level)

This project is typically installed as three services:

1) Build/install the daemon:

```bash
cd xenwooting
cargo build --release
sudo install -m 0755 target/release/xenwooting /usr/local/bin/xenwooting
```

Systemd service (system service): `xenwooting/contrib/systemd/xenwooting.service`

2) Install the XenHarm note-name service (systemd user service):

```bash
mkdir -p ~/.config/systemd/user
ln -sf "$PWD/xenharm_service/xenharm.service" ~/.config/systemd/user/xenharm.service
systemctl --user daemon-reload
systemctl --user enable --now xenharm.service
```

3) Build and run the configurator:

```bash
npm -C webconfigurator/server install
npm -C webconfigurator/web install
npm -C webconfigurator/web run build
npm -C webconfigurator/server start
```

Environment variables for the configurator server:

- `PORT` (default `3174`)
- `XENWTN_CONFIG_DIR` (default `/home/patch/.config/xenwooting`)
- `XENWTN_CONFIG_TOML` (default `<config_dir>/config.toml`)
- `XENWTN_GEOMETRY_JSON` (default `/home/patch/xenWTN.json`)
- `XENHARM_URL` (default `http://127.0.0.1:3199`)

## API Endpoints

Configurator server (`/wtn/api`):

- `GET /wtn/api/layouts`
- `POST /wtn/api/layouts/add`
- `GET /wtn/api/layout/:id`
- `POST /wtn/api/layout/:id` (save `.wtn` boards)
- `POST /wtn/api/layout/:id/settings`
- `DELETE /wtn/api/layout/:id`
- `POST /wtn/api/preview/enable`
- `POST /wtn/api/preview/update`
- `POST /wtn/api/preview/disable`
- `POST /wtn/api/highlight`
- `POST /wtn/api/note-names` (proxy to xenharm_service)
- `GET /wtn/api/geometry`

XenHarm service (`xenharm_service/server.py`):

- `GET /health`
- `POST /v1/note-names`
- `POST /v1/scale/rotate`
- `POST /v1/scale/retune`

## Developer Docs

- Implementation notes and geometry details: `ImpDetails.md`

## Upstream Projects / Credits

This project depends on and/or is inspired by:

- Wooting Analog SDK and Wooting RGB SDK (Wooting Technologies B.V.)
- MTS-ESP (ODDSound Ltd.)
- xenharmlib (Fabian Vallon)
- `.ltn` layout file format from the Lumatone ecosystem (used for import)

## Utilities

This repo also includes helper scripts (useful for headless setups):

- MIDI monitor: `monitor_wooting_midi.py`
- USB/power watchdog: `power_watch.sh`
- RGB helpers: `wooting_rgb.py`, `step_coords.py`, `rgb_power_test.sh` (see `InstallRGB.md`)
