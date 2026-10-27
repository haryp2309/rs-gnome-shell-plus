#!/bin/bash
# One-time setup inside a Fedora 44 VM (see README.md, "Testing on a Mac").
# Installs build deps, builds, and installs the "GNOME Shell Plus" session.
#
# With this repo shared into the VM via UTM's shared directory (VirtFS):
#   sudo mkdir -p /mnt/share
#   sudo mount -t 9p -o trans=virtio,version=9p2000.L share /mnt/share
#   git clone /mnt/share ~/gnome-shell-plus
#   ~/gnome-shell-plus/dev/vm-setup.sh
#
# Later, to pick up changes made on the Mac:
#   cd ~/gnome-shell-plus && git pull && make && sudo make install PREFIX=/usr
set -euo pipefail
cd "$(dirname "$0")/.."

sudo dnf install -y gcc meson ninja-build cargo \
  mutter-devel gtk4-devel libadwaita-devel gsettings-desktop-schemas-devel \
  mutter-devkit

make
sudo make install PREFIX=/usr

cat <<'EOF'

Installed. Two ways to try it:

  * Log out, click the gear on the login screen, pick "GNOME Shell Plus".

  * Or, from the repo directory, in a window inside this session:
      SHELLPLUS_PANEL=$PWD/build/panel/release/gnome-shell-plus-panel \
        dbus-run-session -- build/compositor/gnome-shell-plus \
        --wayland --devkit --wayland-display shellplus-dev
    and from another terminal:
      WAYLAND_DISPLAY=shellplus-dev gnome-text-editor
EOF
