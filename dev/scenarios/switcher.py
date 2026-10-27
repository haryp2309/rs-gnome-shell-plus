"""Alt-Tab: a quick tap switches to the previous window, holding Alt and
pressing Tab walks the list with the pop-up on screen, Escape cancels,
Alt+Shift+Tab goes backwards, and switch-windows shows window titles.
Screenshots: 02 (applications pop-up), 05 (windows), 06 (app with two
windows)."""
import os
import subprocess
import sys
import tempfile
import time

sys.path.insert(0, "/src/dev")
from driver import Session  # noqa: E402

import gi  # noqa: E402

gi.require_version("GdkPixbuf", "2.0")
from gi.repository import GdkPixbuf, GLib  # noqa: E402

KEY_ESC = 1
KEY_TAB = 15
KEY_LEFTSHIFT = 42
KEY_LEFTALT = 56
KEY_LEFTMETA = 125
KEYBINDINGS = "org.gnome.desktop.wm.keybindings"

s = Session(sys.argv[1])
failures = []
focus_file = tempfile.mktemp()


def check(ok, what):
    print(("PASS " if ok else "FAIL ") + what, flush=True)
    if not ok:
        failures.append(what)


def key(code, pressed):
    rd_name, rd, _ = s._remote_desktop()
    s._call(rd_name, rd, rd_name + ".Session", "NotifyKeyboardKeycode",
            GLib.Variant("(ub)", (code, pressed)))
    time.sleep(0.05)


def tap(*codes, settle=0.6):
    """Presses the keys in order, then releases them in reverse."""
    for code in codes:
        key(code, True)
    for code in reversed(codes):
        key(code, False)
    time.sleep(settle)


def focused():
    try:
        with open(focus_file) as f:
            lines = f.read().split()
        return lines[-1] if lines else ""
    except FileNotFoundError:
        return ""


def wait_focus(title, timeout=3.0):
    deadline = time.time() + timeout
    while time.time() < deadline and focused() != title:
        time.sleep(0.05)
    return focused()


def popup_fraction(path):
    """Share of the screen's central area covered by the pop-up's dark gray."""
    pixbuf = GdkPixbuf.Pixbuf.new_from_file(path)
    w, h, n = pixbuf.get_width(), pixbuf.get_height(), pixbuf.get_n_channels()
    stride, data = pixbuf.get_rowstride(), pixbuf.get_pixels()
    dark = total = 0
    for y in range(h // 2 - 90, h // 2 + 90, 3):
        for x in range(w // 2 - 200, w // 2 + 200, 3):
            r, g, b = data[y * stride + x * n: y * stride + x * n + 3]
            total += 1
            if max(r, g, b) < 75 and max(r, g, b) - min(r, g, b) < 12:
                dark += 1
    return dark / total


# Two apps with .desktop files (name + icon), one without (fallback icon).
apps_dir = os.path.expanduser("~/.local/share/applications")
os.makedirs(apps_dir, exist_ok=True)
for app_id, name, icon in [("org.example.Alpha", "Alpha Demo", "gtk3-demo"),
                           ("org.example.Beta", "Beta Widgets", "gtk3-widget-factory")]:
    with open(os.path.join(apps_dir, app_id + ".desktop"), "w") as f:
        f.write(f"[Desktop Entry]\nType=Application\nName={name}\nIcon={icon}\n"
                "Exec=true\n")

tap(KEY_LEFTSHIFT)  # starts the virtual keyboard; without one nothing has focus

windows = []
for app_id, title in [("org.example.Alpha", "Alpha"), ("org.example.Beta", "Beta"),
                      ("org.example.Gamma", "Gamma")]:
    windows.append(subprocess.Popen(
        [sys.executable, "/src/dev/switcher-window.py", app_id, title, focus_file],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL))
    time.sleep(2.5)
    check(wait_focus(title) == title, f"{title} focused when opened (log {open(focus_file).read().split() if os.path.exists(focus_file) else []})")

s.shot("00-three-windows")
baseline = popup_fraction(os.path.join(s.out_dir, "00-three-windows.png"))

# Most recently used first: Gamma, Beta, Alpha.
tap(KEY_LEFTALT, KEY_TAB, settle=0.1)
check(wait_focus("Beta") == "Beta", f"Alt+Tab tap switches to Beta (got {focused()!r})")
time.sleep(0.5)
tap(KEY_LEFTALT, KEY_TAB, settle=0.1)
check(wait_focus("Gamma") == "Gamma", f"second tap switches back to Gamma (got {focused()!r})")
time.sleep(0.5)
s.shot("01-after-taps")
check(popup_fraction(os.path.join(s.out_dir, "01-after-taps.png")) < baseline + 0.2,
      "no pop-up left on screen after taps")

# Hold Alt, Tab twice: Gamma, Beta, [Alpha].
key(KEY_LEFTALT, True)
tap(KEY_TAB, settle=0.1)
tap(KEY_TAB, settle=0.7)
held = s.shot("02-holding-alt")
fraction = popup_fraction(held)
print(f"pop-up coverage: {fraction:.2f} (baseline {baseline:.2f})")
check(fraction > baseline + 0.3, "pop-up visible while Alt is held")
check(focused() == "Gamma", "nothing switches while Alt is held")
key(KEY_LEFTALT, False)
check(wait_focus("Alpha") == "Alpha", f"releasing Alt activates the third window (got {focused()!r})")
time.sleep(0.5)
s.shot("03-released")
check(popup_fraction(os.path.join(s.out_dir, "03-released.png")) < baseline + 0.2,
      "pop-up hidden after release")

# Escape cancels: Alpha stays focused. MRU is now Alpha, Gamma, Beta.
key(KEY_LEFTALT, True)
tap(KEY_TAB, settle=0.5)
tap(KEY_ESC, settle=0.3)
s.shot("04-escaped")
key(KEY_LEFTALT, False)
time.sleep(0.8)
check(focused() == "Alpha", f"Escape cancels (focus {focused()!r})")
check(popup_fraction(os.path.join(s.out_dir, "04-escaped.png")) < baseline + 0.2,
      "Escape hides the pop-up")

# Alt+Shift+Tab goes backwards, to the least recently used (Beta).
tap(KEY_LEFTALT, KEY_LEFTSHIFT, KEY_TAB, settle=0.1)
check(wait_focus("Beta") == "Beta", f"Alt+Shift+Tab switches to Beta (got {focused()!r})")
time.sleep(0.5)

# Holding Alt: Tab then Shift+Tab ends back at the first choice. MRU: Beta, Alpha, Gamma.
key(KEY_LEFTALT, True)
tap(KEY_TAB, settle=0.1)
tap(KEY_TAB, settle=0.1)
tap(KEY_LEFTSHIFT, KEY_TAB, settle=0.4)
key(KEY_LEFTALT, False)
check(wait_focus("Alpha") == "Alpha", f"Tab, Tab, Shift+Tab selects the second (got {focused()!r})")
time.sleep(0.5)

# switch-windows (window titles), bound to Alt+Tab like GNOME Tweaks does.
s.gsettings(KEYBINDINGS, "switch-applications", "['<Super>Tab']")
s.gsettings(KEYBINDINGS, "switch-windows", "['<Alt>Tab']")
key(KEY_LEFTALT, True)
tap(KEY_TAB, settle=0.7)
s.shot("05-windows-mode")
key(KEY_LEFTALT, False)
check(wait_focus("Beta") == "Beta", f"switch-windows switches to Beta (got {focused()!r})")
time.sleep(0.5)

# Super+Tab still switches applications.
tap(KEY_LEFTMETA, KEY_TAB, settle=0.1)
check(wait_focus("Alpha") == "Alpha", f"Super+Tab switches applications (got {focused()!r})")
subprocess.run(["gsettings", "reset", KEYBINDINGS, "switch-applications"], check=True)
subprocess.run(["gsettings", "reset", KEYBINDINGS, "switch-windows"], check=True)

# A second Alpha window (the running Alpha app opens another one when
# activated again): the app shows once, with GNOME's multi-window arrow.
windows.append(subprocess.Popen(
    [sys.executable, "/src/dev/switcher-window.py", "org.example.Alpha", "Alpha", focus_file],
    stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL))
time.sleep(2.5)
key(KEY_LEFTALT, True)
tap(KEY_TAB, settle=0.7)
s.shot("06-app-with-two-windows")
tap(KEY_ESC, settle=0.2)
key(KEY_LEFTALT, False)
time.sleep(0.5)

for window in windows:
    window.terminate()
if os.path.exists(focus_file):
    os.unlink(focus_file)
print("FAILURES:", ", ".join(failures) if failures else "none")
