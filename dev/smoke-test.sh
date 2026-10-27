#!/bin/bash
# Boots gnome-shell-plus headless inside the dev container and saves
# screenshots of the translucent and the solid (maximized window) panel.
#
#   make smoke        # from the host; results land in dev/out/
set -euo pipefail

OUT=${1:-/src/dev/out}
BUILD=${BUILD:-/tmp/build}

if [ -z "${DBUS_SESSION_BUS_ADDRESS:-}" ]; then
  export XDG_RUNTIME_DIR=$(mktemp -d)
  exec dbus-run-session -- "$0" "$@"
fi

mkdir -p "$OUT"
rm -f "$OUT"/*.png "$OUT"/*.log

meson setup "$BUILD" /src/compositor >/dev/null
ninja -C "$BUILD" >/dev/null
cargo build --quiet --manifest-path /src/panel/Cargo.toml

pipewire >/dev/null 2>&1 &
wireplumber >/dev/null 2>&1 &
sleep 1

export WAYLAND_DISPLAY=wayland-smoke
export GSK_RENDERER=gl  # no Vulkan device in the container
SHELLPLUS_PANEL=$CARGO_TARGET_DIR/debug/gnome-shell-plus-panel \
  "$BUILD/gnome-shell-plus" --wayland --headless --no-x11 \
  --wayland-display "$WAYLAND_DISPLAY" --virtual-monitor 1280x800 \
  >"$OUT/compositor.log" 2>&1 &
SHELL_PID=$!
trap 'kill $SHELL_PID 2>/dev/null || true' EXIT

for _ in $(seq 100); do
  gdbus call --session -d org.gnome.ShellPlus -o /org/gnome/ShellPlus \
    -m org.gnome.ShellPlus.Panel.GetState >/dev/null 2>&1 && break
  sleep 0.1
done
sleep 3

echo "== translucent state"
gdbus call --session -d org.gnome.ShellPlus -o /org/gnome/ShellPlus -m org.gnome.ShellPlus.Panel.GetState
python3 /src/dev/screenshot.py "$OUT/translucent.png"

python3 /src/dev/maximized-window.py >"$OUT/app.log" 2>&1 &
sleep 3

echo "== solid state (maximized window)"
gdbus call --session -d org.gnome.ShellPlus -o /org/gnome/ShellPlus -m org.gnome.ShellPlus.Panel.GetState
python3 /src/dev/screenshot.py "$OUT/solid.png"

echo "Screenshots in $OUT"
