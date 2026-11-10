"""IBus input methods: an IBus source next to an XKB layout, the indicator,
Super+Space, real typing through the m17n Chinese tonal pinyin engine
(m17n:zh:tonepy: "ni3" offers 你泥拟…, digits pick one), the candidate
pop-up (keys and clicks), the lock screen's password entry with an engine
active, and XKB typing once ibus-daemon is gone."""
import os
import shutil
import subprocess
import sys
import tempfile
import time

sys.path.insert(0, "/src/dev")
from driver import Session  # noqa: E402

from gi.repository import GLib  # noqa: E402

SCHEMA = "org.gnome.desktop.input-sources"
ENGINE = "m17n:zh:tonepy"
IM = ("org.gnome.ShellPlus", "/org/gnome/ShellPlus/InputMethod",
      "org.gnome.ShellPlus.InputMethod")
KEY_LEFTMETA, KEY_LEFTSHIFT, KEY_SPACE, KEY_ENTER = 125, 42, 57, 28
KEYS = {c: k for c, k in zip("1234567890", range(2, 12))}
KEYS.update({c: k for c, k in zip("qwertyuiop", range(16, 26))})
KEYS.update({c: k for c, k in zip("asdfghjkl", range(30, 39))})
KEYS.update({c: k for c, k in zip("zxcvbnm", range(44, 51))})

TEXT_WINDOW = r'''
import sys
import gi
gi.require_version("Gtk", "4.0")
from gi.repository import Gtk

def on_activate(app):
    window = Gtk.ApplicationWindow(application=app, title="Typing")
    window.set_default_size(700, 300)
    box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, margin_top=120,
                  margin_start=200, margin_end=200)
    entry = Gtk.Entry()
    def changed(e):
        with open(sys.argv[1], "w") as f:
            f.write(e.get_text())
    entry.connect("changed", changed)
    box.append(entry)
    window.set_child(box)
    window.present()

app = Gtk.Application(application_id="org.gnome.ShellPlus.IBusTyping")
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


def popup():
    return s._call(*IM, "GetPopup", reply="(a{sv})")[0]


def wait_for(predicate, timeout=5.0):
    deadline = time.time() + timeout
    while time.time() < deadline:
        if predicate():
            return True
        time.sleep(0.1)
    return predicate()


def keycodes(*codes, settle=0.3):
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


def type_text(text):
    for c in text:
        keycodes(KEYS[c], settle=0.15)
    time.sleep(0.5)


text_file = tempfile.mktemp()


def typed():
    try:
        with open(text_file) as f:
            return f.read()
    except FileNotFoundError:
        return ""


def candidate_mapped(i=0):
    state = s.inspect(f"im-candidate-{i}")
    return bool(state and state.get("mapped"))


# An XKB layout and an IBus engine. The daemon may still be starting.
s.gsettings(SCHEMA, "mru-sources", "@a(ss) []")
s.gsettings(SCHEMA, "sources", f"[('xkb', 'us'), ('ibus', '{ENGINE}')]")
check(wait_for(lambda: len(sources()[0]) == 2, timeout=20), "IBus source listed")
listed, current = sources()
print("sources:", listed, "current:", current)
check(len(listed) == 2 and listed[1][0] == ENGINE, "engine id")
check(len(listed) == 2 and listed[1][1] not in ("", "en"), f"engine short name {listed[1][1]!r}")
check(len(listed) == 2 and "Chinese" in listed[1][2], f"display name {listed[1][2]!r}")
check(current == 0, "XKB source active first")

window = subprocess.Popen([sys.executable, "-c", TEXT_WINDOW, text_file],
                          stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
time.sleep(3)
keycodes(KEY_LEFTSHIFT)  # the first injected key can get lost

# Plain typing on the XKB source.
type_text("ab")
check(wait_for(lambda: typed() == "ab"), f"XKB source types plain text (got {typed()!r})")

# Super+Space switches to the IBus engine; the indicator shows it.
keycodes(KEY_LEFTMETA, KEY_SPACE)
check(wait_for(lambda: sources()[1] == 1), "Super+Space switches to the IBus source")
time.sleep(1)  # SetGlobalEngine is asynchronous
s.shot("01-indicator")
s.panel("Activate", "keyboard-button")
s.shot("02-menu")
s.panel("CloseMenus")

# Preedit and the candidate pop-up.
type_text("ni")
check(typed() == "ab", f"preedit isn't committed yet (got {typed()!r})")
s.shot("03-preedit")
type_text("3")
check(wait_for(lambda: popup()["visible"]), "pop-up visible after ni3")
p = popup()
print("popup:", {k: p[k] for k in ("candidates", "selected", "horizontal", "page", "pages",
                                   "auxiliary", "cursor")})
check(len(p["candidates"]) > 1 and p["candidates"][0][1] == "你", "candidates start with 你")
check(p["selected"] == 0, "first candidate selected")
check(wait_for(candidate_mapped), "panel shows the candidates")
s.shot("04-candidates")

# Pick the second candidate with its number key.
type_text("2")
check(wait_for(lambda: typed() == "ab泥"), f"key 2 commits 泥 (got {typed()!r})")
check(wait_for(lambda: not popup()["visible"]), "pop-up hidden after commit")
check(wait_for(lambda: not candidate_mapped()), "panel pop-up window hidden")

# Space commits the current candidate too.
type_text("ni3")
keycodes(KEY_SPACE)
check(wait_for(lambda: typed() == "ab泥你"), f"space commits 你 (got {typed()!r})")

# Back to the XKB layout: plain text again, no pop-up.
keycodes(KEY_LEFTMETA, KEY_SPACE)
check(wait_for(lambda: sources()[1] == 0), "Super+Space switches back to XKB")
time.sleep(0.5)
type_text("ni3")
check(wait_for(lambda: typed() == "ab泥你ni3"), f"XKB types plain text (got {typed()!r})")
check(not popup()["visible"], "no pop-up on XKB")
s.shot("07-xkb")

# The engine is used again after switching back.
keycodes(KEY_LEFTMETA, KEY_SPACE)
check(wait_for(lambda: sources()[1] == 1), "back on the IBus source")
time.sleep(1)
type_text("ni3")
check(wait_for(lambda: popup()["visible"]), "pop-up again")
type_text("1")
check(wait_for(lambda: typed() == "ab泥你ni3你"), f"key 1 commits 你 (got {typed()!r})")

# Clicking a candidate. ibus-m17n ignores clicks (it has no
# candidate_clicked; the same happens in GNOME Shell), so this uses
# ibus-libpinyin, whose pop-up is horizontal.
s.gsettings(SCHEMA, "sources", "[('xkb', 'us'), ('ibus', 'libpinyin')]")
s.gsettings(SCHEMA, "mru-sources", "[('ibus', 'libpinyin'), ('xkb', 'us')]")
check(wait_for(lambda: sources()[1] == 1 and sources()[0][1][0] == "libpinyin", timeout=10),
      "switched to libpinyin")
time.sleep(1)
before = typed()
type_text("hao")
check(wait_for(lambda: popup()["visible"] and len(popup()["candidates"]) > 1),
      "libpinyin pop-up visible after hao")
check(wait_for(lambda: candidate_mapped(1)), "panel shows the libpinyin candidates")
time.sleep(0.5)
p = popup()
print("popup:", {k: p[k] for k in ("candidates", "selected", "horizontal", "page", "pages",
                                   "auxiliary", "preedit", "cursor")})
wanted = p["candidates"][1][1]
cx, cy, cw, ch = p["cursor"]
bounds = s.inspect("im-candidate-1")["bounds"]
print("cursor:", (cx, cy, cw, ch), "candidate bounds:", bounds)
s.shot("09-libpinyin")
# The constraint puts the window 4px below the cursor, left-aligned
# (nothing is clamped this far from the screen edges).
s.click(cx + bounds[0] + bounds[2] / 2, cy + ch + 4 + bounds[1] + bounds[3] / 2)
check(wait_for(lambda: typed() == before + wanted),
      f"clicking the second candidate commits {wanted} (got {typed()!r})")
check(wait_for(lambda: not popup()["visible"]), "pop-up hidden after the click")
s.shot("10-clicked")

# The lock screen's password entry with an IBus engine active: GNOME Shell
# turns IBus off for password fields, so the password is typed as is.
subprocess.run(["chpasswd"], input="root:hunter2\n", text=True, check=True)
shutil.copy("/src/data/pam/gnome-shell-plus", "/etc/pam.d/gnome-shell-plus")
SS = ("org.gnome.ScreenSaver", "/org/gnome/ScreenSaver", "org.gnome.ScreenSaver")
s._call(*SS, "Lock")
check(wait_for(lambda: (s.inspect("lock-screen") or {}).get("mapped"), 8), "locked")
time.sleep(1)
keycodes(KEY_ENTER)
check(wait_for(lambda: (s.inspect("lock-password") or {}).get("sensitive") and
               (s.inspect("lock-password") or {}).get("mapped")), "password prompt shown")
check(sources()[1] == 1, "the IBus source is still active")
type_text("hunter2")
check(not popup()["visible"], "no candidate pop-up for the password")
s.shot("11-lock-password")
keycodes(KEY_ENTER)
check(wait_for(lambda: not s._call(*SS, "GetActive", reply="(b)")[0], 8),
      "the password typed with IBus active unlocks")
time.sleep(1)

# Without ibus-daemon, the IBus source goes away and XKB keeps working.
subprocess.run(["pkill", "-x", "ibus-daemon"], check=False)
check(wait_for(lambda: len(sources()[0]) == 1, timeout=10), "IBus source dropped without the daemon")
time.sleep(1)
type_text("xy")
check(wait_for(lambda: typed().endswith("xy")), f"XKB types without ibus-daemon (got {typed()!r})")
s.shot("08-no-daemon")

window.terminate()
s.gsettings(SCHEMA, "sources", "[('xkb', 'us')]")
if os.path.exists(text_file):
    os.unlink(text_file)
print("FAILURES:", ", ".join(failures) if failures else "none")
