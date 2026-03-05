# xenharm_service

Small localhost-only HTTP service that wraps xenharmlib for note-name rendering.

## Run (manual)

```bash
python3.12 /home/patch/wooting-xen/xenharm_service/server.py --host 127.0.0.1 --port 3199
```

## Install (systemd user service)

```bash
mkdir -p ~/.config/systemd/user
ln -sf /home/patch/wooting-xen/xenharm_service/xenharm.service ~/.config/systemd/user/xenharm.service
systemctl --user daemon-reload
systemctl --user enable --now xenharm.service
```

Health check:

```bash
curl -s http://127.0.0.1:3199/health
```

## API

- `GET /health`
- `POST /v1/note-names` with `{ "edo": 31, "pitches": [0, 1, 2] }`
  - Returns `{ "edo": 31, "results": { "0": {"short": "C0", "unicode": "C\uE2610"}, ... } }`
  - If a pitch has no name (unsupported EDO / error), it is omitted from `results`.

Future endpoints (implemented for parity with xenassist interval operations):

- `POST /v1/scale/rotate` with `{ "edo": 31, "pitches": [0, 10, 18], "direction": 1 }`
- `POST /v1/scale/retune` with `{ "edoFrom": 31, "edoTo": 19, "pitches": [0, 10, 18] }`
