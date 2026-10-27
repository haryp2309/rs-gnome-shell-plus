# Build + headless smoke-test environment. Works on Apple Silicon (aarch64)
# and x86_64 hosts. See README.md ("Testing on a Mac").
FROM fedora:44

RUN dnf -y install \
      gcc meson ninja-build pkgconf-pkg-config cargo rust \
      mutter mutter-devel gtk4-devel libadwaita-devel gsettings-desktop-schemas-devel \
      dbus-daemon dbus-tools mesa-dri-drivers \
      pipewire wireplumber pipewire-gstreamer \
      gstreamer1-plugins-base gstreamer1-plugins-good \
      python3-gobject gtk4 libadwaita \
      adwaita-icon-theme gnome-backgrounds jxl-pixbuf-loader glycin-loaders \
      procps-ng \
    && dnf clean all

ENV CARGO_TARGET_DIR=/target
WORKDIR /src
