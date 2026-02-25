#!/bin/bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

echo "Building xenwooting..."
(cd "$ROOT_DIR" && cargo build --release)

echo "Installing binary to /usr/local/bin/xenwooting"
sudo install -m 0755 "$ROOT_DIR/target/release/xenwooting" /usr/local/bin/xenwooting

echo "Installing systemd unit"
sudo install -m 0644 "$ROOT_DIR/contrib/systemd/xenwooting.service" /etc/systemd/system/xenwooting.service

echo "Reloading systemd"
sudo systemctl daemon-reload

echo "Disabling old xenassist master (if present)"
sudo systemctl disable --now tun.service 2>/dev/null || true

echo "Disabling old analog-midi daemon (if present)"
sudo systemctl disable --now wooting-analog-midi-headless.service 2>/dev/null || true

echo "Enabling xenwooting"
sudo systemctl enable --now xenwooting.service

echo "Done. Status:"
systemctl --no-pager status xenwooting.service || true
