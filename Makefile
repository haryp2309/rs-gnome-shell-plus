PREFIX   ?= /usr/local
DESTDIR  ?=
BUILDDIR ?= build

DEV_IMAGE   := gnome-shell-plus-dev
DOCKER_RUN  := docker run --rm -v "$(CURDIR)":/src \
               -v gnome-shell-plus-cargo:/root/.cargo/registry \
               -v gnome-shell-plus-target:/target $(DEV_IMAGE)

PANEL_BIN := $(BUILDDIR)/panel/release/gnome-shell-plus-panel

.PHONY: all compositor panel install uninstall clean dev-image check smoke

all: compositor panel

compositor:
	test -d $(BUILDDIR)/compositor || meson setup $(BUILDDIR)/compositor compositor --prefix=$(PREFIX)
	meson compile -C $(BUILDDIR)/compositor

panel:
	cargo build --release --manifest-path panel/Cargo.toml --target-dir $(BUILDDIR)/panel

# Doesn't build, so `make && sudo make install` never compiles as root.
install:
	meson install -C $(BUILDDIR)/compositor --no-rebuild --destdir "$(DESTDIR)"
	install -Dm755 $(PANEL_BIN) $(DESTDIR)$(PREFIX)/bin/gnome-shell-plus-panel
	install -Dm755 data/gnome-shell-plus-session $(DESTDIR)$(PREFIX)/bin/gnome-shell-plus-session
	install -Dm644 data/gnome-shell-plus.desktop $(DESTDIR)$(PREFIX)/share/wayland-sessions/gnome-shell-plus.desktop

uninstall:
	rm -f $(DESTDIR)$(PREFIX)/bin/gnome-shell-plus \
	      $(DESTDIR)$(PREFIX)/bin/gnome-shell-plus-panel \
	      $(DESTDIR)$(PREFIX)/bin/gnome-shell-plus-session \
	      $(DESTDIR)$(PREFIX)/share/wayland-sessions/gnome-shell-plus.desktop

clean:
	rm -rf $(BUILDDIR)

# --- Container targets (usable from macOS) ---------------------------------

dev-image:
	docker build -t $(DEV_IMAGE) dev

# Compile both parts against the real Mutter/GTK/libadwaita.
check: dev-image
	$(DOCKER_RUN) sh -c 'meson setup /tmp/build compositor >/dev/null && ninja -C /tmp/build && cargo build --manifest-path panel/Cargo.toml'

# Boot the compositor headless and screenshot the panel into dev/out/.
smoke: dev-image
	$(DOCKER_RUN) dev/smoke-test.sh
