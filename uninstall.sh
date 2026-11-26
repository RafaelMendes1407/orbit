#!/usr/bin/env bash
set -eu
"$HOME/.local/bin/orbit" --quit 2>/dev/null || true
rm -f "$HOME/.local/bin/orbit" \
      "$HOME/.local/share/applications/io.github.orbit.desktop" \
      "$HOME/.config/autostart/io.github.orbit.desktop"
echo "Orbit removido."
