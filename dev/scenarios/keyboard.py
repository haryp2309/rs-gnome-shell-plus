"""Keyboard layouts: the panel indicator and menu, Super+Space switching,
and the keymap actually changing what keys type."""
import os
import subprocess
import sys
import tempfile
import time

sys.path.insert(0, "/src/dev")
from driver import Session  # noqa: E402

from gi.repository import GLib  # noqa: E402

SCHEMA = "org.gnome.desktop.input-sources"
KEY_Y = 21  # evdev keycode; types "z" on a German layout
KEY_LEFTMETA = 125
KEY_LEFTSHIFT = 42
KEY_SPACE = 57

TEXT_WINDOW = r'''
import sys
import gi
gi.require_version("Gtk", "4.0")
from gi.repository import Gtk

def on_activate(app):
    window = Gtk.ApplicationWindow(application=app, title="Typing")
    entry = Gtk.Entry()
    def changed(e):
        with open(sys.argv[1], "w") as f:
            f.write(e.get_text())
    entry.connect("changed", changed)
    window.set_child(entry)
    window.maximize()
    window.present()

app = Gtk.Application(application_id="org.gnome.ShellPlus.Typing")
app.connect("activate", on_activate)
app.run([])
'''

s = Session(sys.argv[1])
failures = []


def check(ok, what):
    print(("PASS " if ok else "FAIL ") + what, flush=True)
    if not ok:
        failures.append(what)


def sources():
    state = s.state()
    return state.get("input-sources", []), state.get("input-source", -1)


def wait_for(predicate, timeout=5.0):
    deadline = time.time() + timeout
    while time.time() < deadline:
        if predicate():
            return True
        time.sleep(0.1)
    return predicate()


def keycodes(*codes, settle=0.6):
    """Presses the keys in order, then releases them in reverse."""
    rd_name, rd, _ = s._remote_desktop()
    iface = rd_name + ".Session"
    for code in codes:
        s._call(rd_name, rd, iface, "NotifyKeyboardKeycode", GLib.Variant("(ub)", (code, True)))
        time.sleep(0.05)
    for code in reversed(codes):
        s._call(rd_name, rd, iface, "NotifyKeyboardKeycode", GLib.Variant("(ub)", (code, False)))
        time.sleep(0.05)
    time.sleep(settle)


# One layout: no indicator.
s.gsettings(SCHEMA, "sources", "[('xkb', 'us')]")
s.gsettings(SCHEMA, "mru-sources", "@a(ss) []")
check(len(sources()[0]) == 1, "one source by default")
s.shot("00-single-layout")

# Three layouts.
s.gsettings(SCHEMA, "sources", "[('xkb', 'us'), ('xkb', 'de+nodeadkeys'), ('xkb', 'fr')]")
check(wait_for(lambda: len(sources()[0]) == 3), "three sources published")
listed, current = sources()
print("sources:", listed, "current:", current)
check([short for _, short, _ in listed] == ["en", "de", "fr"], "short names en/de/fr")
check(listed[1][2] == "German (no dead keys)", "display name from the XKB registry")
check(current == 0, "first source active")
s.shot("01-indicator")

s.panel("Activate", "keyboard-button")
s.shot("02-menu")

# Pick German from the menu.
s.panel("Activate", "keyboard-source-1")
check(wait_for(lambda: sources()[1] == 1), "menu switches to German")
mru = subprocess.run(["gsettings", "get", SCHEMA, "mru-sources"],
                     capture_output=True, text=True).stdout
check(mru.startswith("[('xkb', 'de+nodeadkeys')"), "mru-sources updated: " + mru.strip())
s.shot("03-german")

# The keymap is really applied: the Y key types "z" on German.
text_file = tempfile.mktemp()
window = subprocess.Popen([sys.executable, "-c", TEXT_WINDOW, text_file],
                          stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
time.sleep(3)
keycodes(KEY_LEFTSHIFT)  # the first injected key can get lost


def typed():
    try:
        with open(text_file) as f:
            return f.read()
    except FileNotFoundError:
        return ""


keycodes(KEY_Y)
check(wait_for(lambda: typed().endswith("z")), f"German layout types z (got {typed()!r})")

# Super+Space goes back to the previous layout (US).
keycodes(KEY_LEFTMETA, KEY_SPACE)
check(wait_for(lambda: sources()[1] == 0), "Super+Space switches back to US")
keycodes(KEY_Y)
check(wait_for(lambda: typed().endswith("y")), f"US layout types y (got {typed()!r})")
s.shot("04-super-space")

# Super+Space again toggles between the two most recent.
keycodes(KEY_LEFTMETA, KEY_SPACE)
check(wait_for(lambda: sources()[1] == 1), "Super+Space toggles to German again")

# Super+Shift+Space goes to the least recently used (French).
keycodes(KEY_LEFTMETA, KEY_LEFTSHIFT, KEY_SPACE)
check(wait_for(lambda: sources()[1] == 2), "Super+Shift+Space switches to French")
keycodes(KEY_Y)
check(wait_for(lambda: typed().endswith("y")), "French layout types y")
window.terminate()
time.sleep(1)
s.shot("05-french")

# Sources that share a short name get numbered.
s.gsettings(SCHEMA, "sources", "[('xkb', 'us'), ('xkb', 'us+intl'), ('ibus', 'anthy')]")
check(wait_for(lambda: len(sources()[0]) == 2), "IBus source skipped")
listed, current = sources()
check([short for _, short, _ in listed] == ["en₁", "en₂"], f"duplicates numbered: {listed}")
check(current == 0, "current falls back to an existing source")
s.shot("06-numbered")

# Back to one layout hides the indicator again.
s.gsettings(SCHEMA, "sources", "[('xkb', 'us')]")
s.shot("07-hidden-again")

os.unlink(text_file) if os.path.exists(text_file) else None
print("FAILURES:", ", ".join(failures) if failures else "none")
