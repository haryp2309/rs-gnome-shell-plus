"""Alt-Tab: a quick tap switches to the previous window, holding Alt and
pressing Tab walks the list with the pop-up on screen, Escape cancels,
Alt+Shift+Tab goes backwards, switch-windows shows window previews and
titles, and in the applications pop-up Down / Alt+` show an app's windows
as thumbnails to pick from. Screenshots: 02 (applications pop-up), 05
(windows), 06 (app with two windows), 07 / 08 (an app's thumbnails)."""
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
KEY_GRAVE = 41
KEY_LEFT = 105
KEY_RIGHT = 106
KEY_DOWN = 108
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
            lines = f.read().splitlines()
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


# Window contents, for checking the previews by color.
COLORS = {"Alpha": (255, 0, 0), "Alpha 2": (255, 255, 0), "Beta": (0, 192, 0),
          "Gamma": (0, 255, 255)}


def hex_color(rgb):
    return "#%02x%02x%02x" % rgb


def popup_origin():
    """Screen position of the pop-up window (the compositor centers it)."""
    column = s.inspect("switcher-column")
    _, _, width, height = s.state()["monitor"]
    _, _, w, h = column["bounds"]
    return (width - int(w)) // 2, (height - int(h)) // 2


def color_in(path, name, color, tolerance=40):
    """Share of the pixels in the middle of widget @name (in the pop-up)
    that have @color. The previews are fitted into their box, so only the
    middle is sure to be window contents."""
    info = s.inspect(name)
    if not info or "bounds" not in info:
        return 0.0
    ox, oy = popup_origin()
    x, y, w, h = info["bounds"]
    pixbuf = GdkPixbuf.Pixbuf.new_from_file(path)
    n, stride, data = pixbuf.get_n_channels(), pixbuf.get_rowstride(), pixbuf.get_pixels()
    hits = total = 0
    cx, cy = ox + x + w / 2, oy + y + h / 2 + h * 0.12
    for py in range(int(cy - h * 0.12), int(cy + h * 0.12)):
        for px in range(int(cx - w * 0.2), int(cx + w * 0.2)):
            r, g, b = data[py * stride + px * n: py * stride + px * n + 3]
            total += 1
            if all(abs(a - e) < tolerance for a, e in zip((r, g, b), color)):
                hits += 1
    return hits / max(total, 1)


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
        [sys.executable, "/src/dev/switcher-window.py", app_id, title, focus_file,
         hex_color(COLORS[title]), hex_color(COLORS[title + " 2"]) if title == "Alpha" else ""],
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
tap(KEY_TAB, settle=1.0)
shot = s.shot("05-windows-mode")
# Most recently used first: Alpha, Beta, Gamma.
for i, title in enumerate(["Alpha", "Beta", "Gamma"]):
    share = color_in(shot, f"switcher-preview-{i}", COLORS[title])
    check(share > 0.5, f"window {i} shows {title}'s preview ({share:.2f} of it in its color)")
key(KEY_LEFTALT, False)
check(wait_focus("Beta") == "Beta", f"switch-windows switches to Beta (got {focused()!r})")
time.sleep(0.5)

# Super+Tab still switches applications.
tap(KEY_LEFTMETA, KEY_TAB, settle=0.1)
check(wait_focus("Alpha") == "Alpha", f"Super+Tab switches applications (got {focused()!r})")
subprocess.run(["gsettings", "reset", KEYBINDINGS, "switch-applications"], check=True)
subprocess.run(["gsettings", "reset", KEYBINDINGS, "switch-windows"], check=True)

# A second Alpha window (the running Alpha app opens another one, "Alpha 2",
# when activated again): the app shows once, with GNOME's multi-window arrow.
windows.append(subprocess.Popen(
    [sys.executable, "/src/dev/switcher-window.py", "org.example.Alpha", "Alpha", focus_file],
    stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL))
time.sleep(2.5)
check(wait_focus("Alpha 2") == "Alpha 2", f"second Alpha window opened ({focused()!r})")
key(KEY_LEFTALT, True)
tap(KEY_TAB, settle=0.7)
s.shot("06-app-with-two-windows")
check("switcher-thumbnail-0" not in s.panel("ListNames", settle=0)[0],
      "no thumbnails for an app with one window")
tap(KEY_ESC, settle=0.2)
key(KEY_LEFTALT, False)
time.sleep(0.5)


def thumbnails():
    names = s.panel("ListNames", settle=0)[0]
    return sorted(n for n in names if n.startswith("switcher-thumbnail-") and n[19:].isdigit())


# Down on Alpha (most recently used: Alpha 2, Alpha) shows its windows and
# moves into them; Right selects the second, and releasing Alt activates it.
key(KEY_LEFTALT, True)
tap(KEY_TAB, settle=0.5)
tap(KEY_LEFT, settle=0.2)
tap(KEY_DOWN, settle=0.3)
tap(KEY_RIGHT, settle=1.0)
shot = s.shot("07-app-thumbnails")
check(thumbnails() == ["switcher-thumbnail-0", "switcher-thumbnail-1"],
      f"Down lists Alpha's two windows ({thumbnails()})")
titles = [(s.inspect(f"switcher-thumbnail-title-{j}") or {}).get("text") for j in range(2)]
check(titles == ["Alpha 2", "Alpha"], f"thumbnail titles, most recent first ({titles})")
selected = "selected" in (s.inspect("switcher-thumbnail-1") or {}).get("classes", [])
check(selected, "Right selects the second thumbnail")
for j, title in enumerate(titles):
    if title in COLORS:
        share = color_in(shot, f"switcher-thumbnail-preview-{j}", COLORS[title])
        check(share > 0.5, f"thumbnail {j} shows {title} ({share:.2f})")
thumbs = (s.inspect("switcher-thumbnails") or {}).get("bounds")
item = (s.inspect("switcher-item-0") or {}).get("bounds")
check(thumbs and item and thumbs[1] > item[1] + item[3]
      and thumbs[0] <= item[0] + item[2] / 2 <= thumbs[0] + thumbs[2],
      f"thumbnails under the selected app ({thumbs} / {item})")
key(KEY_LEFTALT, False)
check(wait_focus("Alpha") == "Alpha", f"selecting the second thumbnail activates it ({focused()!r})")
time.sleep(0.5)

# Alt+` (switch-group) does the same from the keyboard binding: now Alpha,
# Alpha 2; ` twice selects the second.
key(KEY_LEFTALT, True)
tap(KEY_TAB, settle=0.5)
tap(KEY_LEFT, settle=0.2)
tap(KEY_GRAVE, settle=0.3)
tap(KEY_GRAVE, settle=1.0)
s.shot("08-app-thumbnails-grave")
check(len(thumbnails()) == 2, f"Alt+` lists Alpha's windows ({thumbnails()})")
title = (s.inspect("switcher-thumbnail-title-1") or {}).get("text")
key(KEY_LEFTALT, False)
check(wait_focus("Alpha 2") == "Alpha 2" and title == "Alpha 2",
      f"Alt+` twice activates the second window ({focused()!r}, {title!r})")
time.sleep(0.5)

for window in windows:
    window.terminate()
if os.path.exists(focus_file):
    os.unlink(focus_file)
print("FAILURES:", ", ".join(failures) if failures else "none")
