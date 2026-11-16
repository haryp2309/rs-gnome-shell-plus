"""The on-screen keyboard (panel/src/osk/, compositor/src/shellplus-osk.c).

With screen-keyboard-enabled on: focusing a GTK4 entry slides the keyboard
up; real pointer clicks on its keys type into the entry (letters, shift,
caps lock, the symbols page, space, Backspace, Enter, a long-press accent);
the window whose entry would be covered moves up; a number entry gets the
number pad; focusing a button hides the keyboard; a maximized window ends
above it; another D-Bus client can't type through it; the lock screen's
password is typed on the lock screen's keyboard and unlocks; and turning
the setting off hides it.
"""
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time

sys.path.insert(0, "/src/dev")
from driver import Session  # noqa: E402

from gi.repository import Gio, GLib  # noqa: E402

A11Y = "org.gnome.desktop.a11y.applications"
OSK = ("org.gnome.ShellPlus", "/org/gnome/ShellPlus/Osk", "org.gnome.ShellPlus.Osk")
SS = ("org.gnome.ScreenSaver", "/org/gnome/ScreenSaver", "org.gnome.ScreenSaver")
KEY_ENTER, KEY_LEFTSHIFT = 28, 42
PASSWORD = "hunter2"

# A test window: a button at the top, a number entry, and a text entry at
# the bottom (so the keyboard would cover it). It focuses whatever the
# command file names and writes its state to the state file.
APP = r'''
import json, sys
import gi
gi.require_version("Gtk", "4.0")
from gi.repository import Gtk, GLib

state_file, command_file, maximized = sys.argv[1], sys.argv[2], sys.argv[3] == "1"
state = {"text": "", "number": "", "activated": 0, "height": 0, "focus": ""}
last_command = [None]

def save():
    with open(state_file + ".tmp", "w") as f:
        json.dump(state, f)
    import os
    os.replace(state_file + ".tmp", state_file)

def on_activate(app):
    window = Gtk.ApplicationWindow(application=app, title="OSK Test")
    window.set_default_size(640, 560)
    box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=12,
                  margin_top=12, margin_bottom=12, margin_start=12, margin_end=12)
    button = Gtk.Button(label="Not a text field")
    number = Gtk.Entry(input_purpose=Gtk.InputPurpose.NUMBER, placeholder_text="Number")
    spacer = Gtk.Box(vexpand=True)
    text = Gtk.Entry(placeholder_text="Text")
    for w in (button, number, spacer, text):
        box.append(w)
    window.set_child(box)
    widgets = {"button": button, "number": number, "text": text}

    def changed(entry, key):
        state[key] = entry.get_text()
        save()
    text.connect("changed", changed, "text")
    number.connect("changed", changed, "number")
    def activated(entry):
        state["activated"] += 1
        save()
    text.connect("activate", activated)

    def poll():
        state["height"] = window.get_height()
        try:
            with open(command_file) as f:
                command = f.read().strip()
        except FileNotFoundError:
            command = ""
        if command and command != last_command[0]:
            last_command[0] = command
            widgets[command.split()[0]].grab_focus()
            state["focus"] = command
        save()
        return True
    GLib.timeout_add(100, poll)
    if maximized:
        window.maximize()
    window.present()

app = Gtk.Application(application_id=sys.argv[4])
app.connect("activate", on_activate)
app.run([])
'''

s = Session(sys.argv[1])
failures = []
tmp = tempfile.mkdtemp()


def check(ok, what):
    print(("PASS " if ok else "FAIL ") + what, flush=True)
    if not ok:
        failures.append(what)


def wait_for(predicate, timeout=6.0):
    deadline = time.time() + timeout
    while time.time() < deadline:
        if predicate():
            return True
        time.sleep(0.1)
    return predicate()


class App:
    count = 0

    def __init__(self, maximized=False):
        App.count += 1
        self.state_file = os.path.join(tmp, f"state{App.count}")
        self.command_file = os.path.join(tmp, f"command{App.count}")
        self.proc = subprocess.Popen(
            [sys.executable, "-c", APP, self.state_file, self.command_file,
             "1" if maximized else "0", f"org.gnome.ShellPlus.OskTest{App.count}"],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

    def state(self):
        try:
            with open(self.state_file) as f:
                return json.load(f)
        except (FileNotFoundError, json.JSONDecodeError):
            return {}

    def focus(self, name, n=[0]):
        n[0] += 1  # a new command each time
        with open(self.command_file, "w") as f:
            f.write(f"{name} {n[0]}")
        time.sleep(0.8)

    def close(self):
        self.proc.terminate()
        self.proc.wait()


def focus_info():
    return s._call(*OSK, "GetFocus", reply="(a{sv})")[0]


def mapped(name):
    return bool((s.inspect(name) or {}).get("mapped"))


def osk_top():
    """Screen y of the overlay keyboard (it sits at the bottom)."""
    height = s.state()["monitor"][3]
    return height - s.inspect("osk")["bounds"][3]


def click_key(name, top=None):
    """A real pointer click in the middle of a key."""
    state = s.inspect(name)
    if not state or not state.get("mapped"):
        check(False, f"key {name} shown")
        return
    x, y, w, h = state["bounds"]
    if top is None:
        top = osk_top()
    s.click(x + w / 2, top + y + h / 2, settle=0.35)


def keycodes(*codes, settle=0.4):
    rd_name, rd, _ = s._remote_desktop()
    iface = rd_name + ".Session"
    for code in codes:
        s._call(rd_name, rd, iface, "NotifyKeyboardKeycode", GLib.Variant("(ub)", (code, True)))
        time.sleep(0.05)
    for code in reversed(codes):
        s._call(rd_name, rd, iface, "NotifyKeyboardKeycode", GLib.Variant("(ub)", (code, False)))
        time.sleep(0.05)
    time.sleep(settle)


# --- off by default --------------------------------------------------------
s.gsettings(A11Y, "screen-keyboard-enabled", "false")
app = App()
time.sleep(3)
s._remote_desktop()
app.focus("text")
check(focus_info()["focused"], "the compositor sees the focused entry")
check(not mapped("osk"), "no keyboard while screen-keyboard-enabled is off")

# --- shows on focus ----------------------------------------------------------
s.gsettings(A11Y, "screen-keyboard-enabled", "true")
check(wait_for(lambda: mapped("osk")), "keyboard shows for the focused entry")
time.sleep(1)
s.shot("01-shown")
info = focus_info()
check(info["purpose"] == "normal", f"purpose normal (got {info['purpose']})")
top = osk_top()
height = s.state()["monitor"][3]
check(abs((height - top) - height / 3) <= 2, f"a third of the screen tall (top {top})")
check(mapped("osk-key-default-q") and mapped("osk-key-default-to-shift") and
      mapped("osk-key-default-enter") and mapped("osk-key-default-delete") and
      mapped("osk-key-default-space") and mapped("osk-key-default-hide") and
      mapped("osk-key-default-to-opt"), "US layout keys")
check(not mapped("osk-key-default-language"), "no layout key with one input source")

# --- typing ------------------------------------------------------------------
for k in "hi":
    click_key(f"osk-key-default-{k}")
check(wait_for(lambda: app.state().get("text") == "hi"), f"letters (got {app.state().get('text')!r})")
cx, cy, cw, ch = focus_info()["cursor"]
check(cy + ch <= top, f"the window moved up: cursor {cy}+{ch} above the keyboard at {top}")

click_key("osk-key-default-to-shift")
check(mapped("osk-key-shift-H"), "shift shows capitals")
s.shot("02-shift")
click_key("osk-key-shift-H")
check(wait_for(lambda: mapped("osk-key-default-i")), "shift is latched for one key")
click_key("osk-key-default-i")
check(wait_for(lambda: app.state().get("text") == "hiHi"), f"shift (got {app.state().get('text')!r})")

# Double tap: caps lock.
shift = s.inspect("osk-key-default-to-shift")["bounds"]
s.move_to(shift[0] + shift[2] / 2, top + shift[1] + shift[3] / 2)
for _ in range(2):
    s.button(True)
    s.button(False)
time.sleep(0.5)
check(mapped("osk-key-shift-A"), "double tap locks caps")
s.shot("03-caps-lock")
click_key("osk-key-shift-A")
click_key("osk-key-shift-B")
check(wait_for(lambda: app.state().get("text") == "hiHiAB"), f"caps lock (got {app.state().get('text')!r})")
click_key("osk-key-shift-to-default")
check(wait_for(lambda: mapped("osk-key-default-a")), "shift again unlocks caps")

click_key("osk-key-default-to-opt")
check(mapped("osk-key-opt-1"), "symbols page")
s.shot("04-symbols")
click_key("osk-key-opt-1")
click_key("osk-key-opt-@")
check(mapped("osk-key-opt-1"), "symbols page stays")
click_key("osk-key-opt-to-opt-shift")
check(mapped("osk-key-opt-shift-€"), "more symbols page")
click_key("osk-key-opt-shift-€")
click_key("osk-key-opt-shift-to-default")
click_key("osk-key-default-space")
check(wait_for(lambda: app.state().get("text") == "hiHiAB1@€ "),
      f"symbols and space (got {app.state().get('text')!r})")

click_key("osk-key-default-delete")
click_key("osk-key-default-delete")
check(wait_for(lambda: app.state().get("text") == "hiHiAB1@"), f"backspace (got {app.state().get('text')!r})")
click_key("osk-key-default-enter")
check(wait_for(lambda: app.state().get("activated") == 1), "enter activates the entry")

# Long press: accented variants.
e = s.inspect("osk-key-default-e")
x, y, w, h = e["bounds"]
s.move_to(x + w / 2, top + y + h / 2)
s.button(True)
time.sleep(0.9)
s.button(False)
time.sleep(0.3)
check(wait_for(lambda: mapped("osk-subkeys")), "long press shows the variants")
s.shot("05-variants")
variants = [s.inspect(f"osk-variant-{i}") for i in range(6)]
labels = [v for v in variants if v and v.get("mapped")]
v1 = s.inspect("osk-variant-1")["bounds"]
s.click(v1[0] + v1[2] / 2, top + v1[1] + v1[3] / 2, settle=0.4)
check(wait_for(lambda: app.state().get("text") == "hiHiAB1@é"), f"variant typed (got {app.state().get('text')!r})")
check(not mapped("osk-subkeys"), "variants close")

# --- number pad -----------------------------------------------------------
app.focus("number")
check(wait_for(lambda: focus_info()["purpose"] == "number"), "number purpose")
check(wait_for(lambda: mapped("osk-key-default-7") and not mapped("osk-key-default-q")),
      "number pad for a number entry")
time.sleep(0.5)
s.shot("06-number-pad")
top = osk_top()
for k in "42":
    click_key(f"osk-key-default-{k}", top)
check(wait_for(lambda: app.state().get("number") == "42"), f"number pad types (got {app.state().get('number')!r})")

# --- hide on unfocus --------------------------------------------------------
app.focus("button")
check(wait_for(lambda: not focus_info()["focused"]), "button focus: no text field")
check(wait_for(lambda: not mapped("osk")), "keyboard hides when the entry loses focus")
s.shot("07-hidden")

# The hide key hides it until the focus moves.
app.focus("text")
check(wait_for(lambda: mapped("osk-key-default-q")), "keyboard back on the entry")
time.sleep(0.5)
click_key("osk-key-default-hide")
check(wait_for(lambda: not mapped("osk")), "hide key")
app.close()

# --- maximized windows end above it ------------------------------------------
max_app = App(maximized=True)
time.sleep(3)
max_app.focus("text")
check(wait_for(lambda: mapped("osk")), "keyboard for the maximized window")
time.sleep(1.5)
s.shot("08-maximized")
monitor_h = s.state()["monitor"][3]
osk_h = s.inspect("osk")["bounds"][3]
bar_h = (s.inspect("panel") or s.inspect("top-bar") or {}).get("bounds", (0, 0, 0, 32))[3]
win_h = max_app.state().get("height", 0)
print("maximized height", win_h, "keyboard", osk_h, "bar", bar_h)
check(0 < win_h <= monitor_h - osk_h - bar_h + 1, f"maximized window shrank above the keyboard ({win_h})")
max_app.focus("button")
check(wait_for(lambda: not mapped("osk")), "hidden again")
time.sleep(1)
win_h2 = max_app.state().get("height", 0)
check(win_h2 > win_h, f"maximized window grows back ({win_h2})")
max_app.close()

# --- only the panel may type -------------------------------------------------
for method, args in (("CommitText", GLib.Variant("(s)", ("x",))),
                     ("Keyval", GLib.Variant("(ub)", (0xff0d, True)))):
    try:
        s._call(*OSK, method, args)
        check(False, f"{method} from another client refused")
    except GLib.Error as err:
        check("AccessDenied" in err.message or "Only the panel" in err.message,
              f"{method} from another client refused ({err.message})")

# --- the lock screen -------------------------------------------------------
subprocess.run(["chpasswd"], input=f"root:{PASSWORD}\n", text=True, check=True)
shutil.copy("/src/data/pam/gnome-shell-plus", "/etc/pam.d/gnome-shell-plus")
s._call(*SS, "Lock")
check(wait_for(lambda: mapped("lock-screen"), 8), "locked")
time.sleep(1)
keycodes(KEY_LEFTSHIFT)
keycodes(KEY_ENTER)
check(wait_for(lambda: mapped("lock-password") and (s.inspect("lock-password") or {}).get("sensitive")),
      "password prompt")
check(wait_for(lambda: mapped("lock-osk-key-default-q")), "lock screen keyboard shows")
check(not mapped("osk"), "the overlay keyboard stays hidden while locked")
time.sleep(0.5)
s.shot("09-lock-screen")
for c in PASSWORD:
    if c.isdigit():
        click_key("lock-osk-key-default-to-opt", top=0)
        click_key(f"lock-osk-key-opt-{c}", top=0)
        click_key("lock-osk-key-opt-to-default", top=0)
    else:
        click_key(f"lock-osk-key-default-{c}", top=0)
typed = (s.inspect("lock-password") or {}).get("text")
check(typed == PASSWORD, f"password typed on the lock screen keyboard ({len(typed or '')} chars)")
s.shot("10-lock-typed")
click_key("lock-osk-key-default-enter", top=0)
check(wait_for(lambda: not s._call(*SS, "GetActive", reply="(b)")[0], 10),
      "the lock screen keyboard's Enter unlocks")
time.sleep(1)

# --- turning it off --------------------------------------------------------
app = App()
time.sleep(3)
app.focus("text")
check(wait_for(lambda: mapped("osk")), "keyboard after unlocking")

# Several input sources: the layout key, which switches to the next one.
SOURCES = "org.gnome.desktop.input-sources"
s.gsettings(SOURCES, "mru-sources", "@a(ss) []")
s.gsettings(SOURCES, "sources", "[('xkb', 'us'), ('xkb', 'de+nodeadkeys')]")
check(wait_for(lambda: mapped("osk-key-default-language")), "layout key with two input sources")
time.sleep(0.5)
click_key("osk-key-default-language")
check(wait_for(lambda: s.state().get("input-source") == 1), "layout key switches the input source")
def qwertz():
    z, t = s.inspect("osk-key-default-z"), s.inspect("osk-key-default-t")
    return bool(z and t and z.get("mapped") and z["bounds"][1] == t["bounds"][1])


check(wait_for(qwertz), "German (QWERTZ) keyboard layout")
time.sleep(0.5)
s.shot("11-german")
s.gsettings(SOURCES, "sources", "[('xkb', 'us')]")
check(wait_for(lambda: mapped("osk-key-default-q") and not mapped("osk-key-default-language")),
      "back to one layout")
s.gsettings(A11Y, "screen-keyboard-enabled", "false")
check(wait_for(lambda: not mapped("osk")), "turning the setting off hides it")
app.close()

shutil.rmtree(tmp, ignore_errors=True)
print("FAILURES:", ", ".join(failures) if failures else "none")
