PREFIX   ?= /usr/local
DESTDIR  ?=
BUILDDIR ?= build
# The lock screen's PAM service (used when GDM isn't running).
PAMDIR   ?= /etc/pam.d
# systemd user units for the gnome-session based session.
SYSTEMDUSERUNITDIR ?= $(PREFIX)/lib/systemd/user

DEV_IMAGE   := gnome-shell-plus-dev
DOCKER_RUN  := docker run --rm -v "$(CURDIR)":/src \
               -v gnome-shell-plus-cargo:/root/.cargo/registry \
               -v gnome-shell-plus-target:/target $(DEV_IMAGE)

PANEL_BIN := $(BUILDDIR)/panel/release/gnome-shell-plus-panel

.PHONY: all compositor panel install uninstall clean dev-image check smoke shots

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
	install -Dm644 data/pam/gnome-shell-plus $(DESTDIR)$(PAMDIR)/gnome-shell-plus
# "GNOME Shell Plus (gnome-session)": the same shell run by gnome-session.
	install -Dm644 data/gnome-shell-plus-gnome-session.desktop $(DESTDIR)$(PREFIX)/share/wayland-sessions/gnome-shell-plus-gnome-session.desktop
	install -Dm644 data/gnome-shell-plus.session $(DESTDIR)$(PREFIX)/share/gnome-session/sessions/gnome-shell-plus.session
	install -d $(DESTDIR)$(SYSTEMDUSERUNITDIR)
	sed 's|@bindir@|$(PREFIX)/bin|g' data/systemd/org.gnome.ShellPlus.service.in > $(DESTDIR)$(SYSTEMDUSERUNITDIR)/org.gnome.ShellPlus.service
	chmod 644 $(DESTDIR)$(SYSTEMDUSERUNITDIR)/org.gnome.ShellPlus.service
	install -Dm644 data/systemd/gnome-shell-plus.session.conf $(DESTDIR)$(SYSTEMDUSERUNITDIR)/gnome-session@gnome-shell-plus.target.d/gnome-shell-plus.session.conf

uninstall:
	rm -f $(DESTDIR)$(PREFIX)/bin/gnome-shell-plus \
	      $(DESTDIR)$(PREFIX)/bin/gnome-shell-plus-panel \
	      $(DESTDIR)$(PREFIX)/bin/gnome-shell-plus-session \
	      $(DESTDIR)$(PREFIX)/share/wayland-sessions/gnome-shell-plus.desktop \
	      $(DESTDIR)$(PAMDIR)/gnome-shell-plus \
	      $(DESTDIR)$(PREFIX)/share/wayland-sessions/gnome-shell-plus-gnome-session.desktop \
	      $(DESTDIR)$(PREFIX)/share/gnome-session/sessions/gnome-shell-plus.session \
	      $(DESTDIR)$(SYSTEMDUSERUNITDIR)/org.gnome.ShellPlus.service \
	      $(DESTDIR)$(SYSTEMDUSERUNITDIR)/gnome-session@gnome-shell-plus.target.d/gnome-shell-plus.session.conf
	-rmdir $(DESTDIR)$(SYSTEMDUSERUNITDIR)/gnome-session@gnome-shell-plus.target.d

clean:
	rm -rf $(BUILDDIR)

# --- Container targets (usable from macOS) ---------------------------------

dev-image:
	docker build -t $(DEV_IMAGE) dev

# Compile both parts against the real Mutter/GTK/libadwaita.
check: dev-image
	$(DOCKER_RUN) sh -c 'meson setup /tmp/build compositor >/dev/null && ninja -C /tmp/build && cargo build --manifest-path panel/Cargo.toml'

# Boot a headless session (mocked services, fake audio) and run a scenario
# from dev/scenarios/, saving screenshots to dev/out/<scenario>/.
SCENARIO ?= quick_settings
shots: dev-image
	$(DOCKER_RUN) dev/session.sh $(SCENARIO)

smoke:
	$(MAKE) shots SCENARIO=smoke
