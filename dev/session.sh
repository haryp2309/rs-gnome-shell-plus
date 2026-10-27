#!/bin/bash
# Boots a complete headless GNOME Shell Plus session inside the dev
# container and runs a scenario script against it:
#
#   dev/session.sh <scenario> [output-dir]      # e.g. quick_settings
#
# The session has a fake system bus with mocked NetworkManager, BlueZ,
# UPower, power-profiles-daemon and gsd-rfkill (dev/mocks.py), PipeWire with
# fake speakers/headphones/microphone, and the compositor + panel with the
# panel's debug D-Bus interface enabled. Run it via `make shots`.
set -euo pipefail

SCENARIO=${1:?usage: session.sh <scenario> [output-dir]}
OUT=${2:-/src/dev/out/$SCENARIO}
BUILD=${BUILD:-/tmp/build}

if [ -z "${SHELLPLUS_IN_SESSION:-}" ]; then
  export SHELLPLUS_IN_SESSION=1
  export XDG_RUNTIME_DIR=$(mktemp -d)
  exec dbus-run-session -- "$0" "$@"
fi

mkdir -p "$OUT"
rm -f "$OUT"/*.png "$OUT"/*.log

meson setup "$BUILD" /src/compositor >/dev/null
ninja -C "$BUILD" >/dev/null
cargo build --quiet --manifest-path /src/panel/Cargo.toml

# A second bus daemon stands in for the system bus.
export DBUS_SYSTEM_BUS_ADDRESS=unix:path=$XDG_RUNTIME_DIR/system_bus
dbus-daemon --session --address="$DBUS_SYSTEM_BUS_ADDRESS" --fork --nopidfile

python3 /src/dev/mocks.py >"$OUT/mocks.log" 2>&1 &
for _ in $(seq 100); do grep -q "mocks ready" "$OUT/mocks.log" && break; sleep 0.1; done
grep -q "mocks ready" "$OUT/mocks.log" || { cat "$OUT/mocks.log"; exit 1; }

pipewire >/dev/null 2>&1 &
wireplumber >/dev/null 2>&1 &
pipewire-pulse >/dev/null 2>&1 &
for _ in $(seq 100); do pactl info >/dev/null 2>&1 && break; sleep 0.1; done
pactl load-module module-null-sink sink_name=speakers \
  sink_properties=device.description=Speakers >/dev/null
pactl load-module module-null-sink sink_name=headphones \
  sink_properties=device.description=Headphones >/dev/null
pactl load-module module-null-sink sink_name=mic media.class=Audio/Source/Virtual \
  sink_properties=device.description=Microphone >/dev/null
pactl set-default-sink speakers
pactl set-default-source mic
pactl set-sink-volume speakers 62%

export WAYLAND_DISPLAY=wayland-test
export GSK_RENDERER=gl  # no Vulkan device in the container
export SHELLPLUS_PANEL=$CARGO_TARGET_DIR/debug/gnome-shell-plus-panel
export SHELLPLUS_PANEL_DEBUG=1
export MUTTER_DEBUG=${MUTTER_DEBUG:-}
# Compositor debug logs (g_debug) go to compositor.log.
export G_MESSAGES_DEBUG=${G_MESSAGES_DEBUG:-shellplus}
# SHELLPLUS_SLOW_ANIMATIONS=N slows window animations down N times (pass it
# with docker run -e); the animations scenario defaults to 10 so it can
# capture frames mid-animation.
if [ "$SCENARIO" = animations ]; then
  export SHELLPLUS_SLOW_ANIMATIONS=${SHELLPLUS_SLOW_ANIMATIONS:-10}
fi
export SHELLPLUS_SLOW_ANIMATIONS=${SHELLPLUS_SLOW_ANIMATIONS:-}
"$BUILD/gnome-shell-plus" --wayland --headless --no-x11 \
  --wayland-display "$WAYLAND_DISPLAY" --virtual-monitor 1280x800 \
  >"$OUT/compositor.log" 2>&1 &
trap 'kill %- 2>/dev/null || true' EXIT

python3 /src/dev/driver.py wait-ready
python3 "/src/dev/scenarios/$SCENARIO.py" "$OUT"
echo "Screenshots in $OUT"
