#!/usr/bin/env bash
# Compila e instala o Orbit para o usuário atual (sem root).
set -euo pipefail
cd "$(dirname "$0")"

FEATURES=()
if ! pkg-config --exists gtk4-layer-shell-0; then
  echo "⚠  gtk4-layer-shell não encontrado: o Orbit vai abrir como janela em tela cheia"
  echo "   (não fica por cima de jogos em tela cheia). Para o overlay de verdade, instale:"
  echo "     Arch/CachyOS: sudo pacman -S gtk4-layer-shell"
  echo "     Fedora:       sudo dnf install gtk4-layer-shell-devel"
  echo "     Debian/Ubuntu: sudo apt install libgtk4-layer-shell-dev"
  echo "   (GNOME não suporta layer-shell; lá a janela em tela cheia é o esperado.)"
  FEATURES=(--no-default-features)
fi

cargo build --release "${FEATURES[@]}"
BIN="$HOME/.local/bin/orbit"
install -Dm755 target/release/orbit "$BIN"
install -Dm644 data/io.github.orbit.desktop "$HOME/.local/share/applications/io.github.orbit.desktop"

# Inicia junto com a sessão, escondido.
mkdir -p "$HOME/.config/autostart"
sed -e "s|^Exec=.*|Exec=$BIN --daemon|" -e "s|^Name=.*|Name=Orbit (segundo plano)|" \
  data/io.github.orbit.desktop > "$HOME/.config/autostart/io.github.orbit.desktop"
echo "X-GNOME-Autostart-enabled=true" >> "$HOME/.config/autostart/io.github.orbit.desktop"

# Reinicia a instância em execução.
"$BIN" --quit 2>/dev/null || true
sleep 0.3
setsid -f "$BIN" --daemon >/dev/null 2>&1

echo "✓ Orbit instalado em $BIN e rodando em segundo plano."
echo "  Aperte o botão Guide do controle (ou rode 'orbit') para abrir."
