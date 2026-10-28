"""The Activities overview (panel/src/overview/): Super opens the window
picker with live previews of solid-colored test windows; hover chrome and
the close button; clicking a preview activates it; Escape returns focus;
workspace thumbnails; dragging a preview to another workspace; type to
search; the app grid; keyboard navigation; the Activities button and the
hot corner; mid-animation frames; light and dark style.

Screenshots: 01 (light), 02 (dark), 04 (hover), 10-12 (opening/closing
mid-animation), 14 (search), 15 (app grid), 16 (other workspace).
"""
import os
import subprocess
import sys
import tempfile
import time

sys.path.insert(0, "/src/dev")
from driver import Session  # noqa: E402

import gi  # noqa: E402

gi.require_version("GdkPixbuf", "2.0")
from gi.repository import GdkPixbuf, Gio, GLib  # noqa: E402

NAME, PATH, IFACE = "org.gnome.ShellPlus", "/org/gnome/ShellPlus", "org.gnome.ShellPlus.Overview"
KEY_LEFTMETA = 125
XK_Escape, XK_Return, XK_Right, XK_Left = 0xff1b, 0xff0d, 0xff53, 0xff51
INTERFACE = "org.gnome.desktop.interface"

TEST_WINDOW = r'''
import sys
import time
import gi
gi.require_version("Gtk", "4.0")
from gi.repository import Gdk, Gio, GLib, Gtk

app_id, title, color, focus_file, live = sys.argv[1:6]
css = Gtk.CssProvider()

HUES = ["#c01c28", "#e66100", "#f5c211", "#2ec27e", "#1c71d8", "#813d9c"]
css.load_from_string("label.big { font-size: 40px; color: white; } "
                     + " ".join("window.c%d { background: %s; }" % (i, c) for i, c in enumerate(HUES))
                     + " window.test { background: %s; }" % color)

def on_active(window, _pspec):
    if window.is_active():
        with open(focus_file, "a") as f:
            f.write(title + "\n")

def on_activate(app):
    Gtk.StyleContext.add_provider_for_display(Gdk.Display.get_default(), css, 800)
    window = Gtk.ApplicationWindow(application=app, title=title, default_width=560, default_height=380)
    window.add_css_class("test")
    label = Gtk.Label(label=title, css_classes=["big"])
    window.set_child(label)
    window.connect("notify::is-active", on_active)
    if live == "1":
        start = time.time()
        def tick():
            t = time.time() - start
            # A new color every 200ms.
            window.set_css_classes(["c%d" % (int(t * 5) % len(HUES))])
            label.set_label("%s %d" % (title, int(t * 5)))
            return True
        GLib.timeout_add(200, tick)
    window.present()

app = Gtk.Application(application_id=app_id, flags=Gio.ApplicationFlags.NON_UNIQUE)
app.connect("activate", on_activate)
app.run([])
'''

s = Session(sys.argv[1])

# A few apps for the app grid (the image's own .desktop files are all
# NoDisplay).
APPS_DIR = os.path.expanduser("~/.local/share/applications")
os.makedirs(APPS_DIR, exist_ok=True)
for app_id, name, icon in [("org.example.Files", "Files", "folder"),
                           ("org.example.Terminal", "Terminal", "utilities-terminal"),
                           ("org.example.Calculator", "Calculator", "accessories-calculator"),
                           ("org.example.Music", "Music", "audio-x-generic")]:
    with open(os.path.join(APPS_DIR, app_id + ".desktop"), "w") as f:
        f.write(f"[Desktop Entry]\nType=Application\nName={name}\nIcon={icon}\nExec=true\n")
time.sleep(1)
bus = s.bus
failures = []
focus_file = tempfile.mktemp()
script = tempfile.mktemp(suffix=".py")
with open(script, "w") as f:
    f.write(TEST_WINDOW)


def check(ok, what):
    print(("PASS " if ok else "FAIL ") + what, flush=True)
    if not ok:
        failures.append(what)


def call(method, args=None, reply=None):
    return bus.call_sync(NAME, PATH, IFACE, method, args,
                         GLib.VariantType(reply) if reply else None,
                         Gio.DBusCallFlags.NONE, 5000, None)


def windows():
    return {props["title"]: (wid, props)
            for wid, props in call("ListWindows", reply="(a(ta{sv}))")[0]}


def focused():
    return next((t for t, (_, p) in windows().items() if p["focused"]), None)


def wait_until(predicate, timeout=4.0):
    deadline = time.time() + timeout
    while time.time() < deadline:
        if predicate():
            return True
        time.sleep(0.05)
    return predicate()


def overview_shown():
    state = s.inspect("overview")
    return bool(state and state["mapped"])


def overview_classes():
    state = s.inspect("overview")
    return state["classes"] if state else []


def super_key():
    rd_name, rd, _ = s._remote_desktop()
    for pressed in (True, False):
        s._call(rd_name, rd, rd_name + ".Session", "NotifyKeyboardKeycode",
                GLib.Variant("(ub)", (KEY_LEFTMETA, pressed)))
        time.sleep(0.05)


def open_overview(settle=0.6):
    super_key()
    ok = wait_until(overview_shown)
    time.sleep(settle)
    return ok


def close_overview():
    s.key(XK_Escape, settle=0.1)
    if s.inspect("overview-search") and s.inspect("overview-search")["text"]:
        s.key(XK_Escape, settle=0.1)
    return wait_until(lambda: not overview_shown())


def bounds(name):
    state = s.inspect(name)
    return state["bounds"] if state and state["mapped"] else None


def center(b):
    return b[0] + b[2] / 2, b[1] + b[3] / 2


def mean_color(path, b, region=(0.1, 0.55, 0.35, 0.85)):
    """Mean RGB over part of the box b (fractions x0, y0, x1, y1)."""
    pixbuf = GdkPixbuf.Pixbuf.new_from_file(path)
    n, stride, data = pixbuf.get_n_channels(), pixbuf.get_rowstride(), pixbuf.get_pixels()
    x0, y0 = int(b[0] + b[2] * region[0]), int(b[1] + b[3] * region[1])
    x1, y1 = int(b[0] + b[2] * region[2]), int(b[1] + b[3] * region[3])
    total, count = [0, 0, 0], 0
    for y in range(max(y0, 0), min(y1, pixbuf.get_height()), 2):
        for x in range(max(x0, 0), min(x1, pixbuf.get_width()), 2):
            for c in range(3):
                total[c] += data[y * stride + x * n + c]
            count += 1
    return tuple(t // max(count, 1) for t in total)


def pixel(path, x, y):
    pixbuf = GdkPixbuf.Pixbuf.new_from_file(path)
    n, stride, data = pixbuf.get_n_channels(), pixbuf.get_rowstride(), pixbuf.get_pixels()
    return tuple(data[int(y) * stride + int(x) * n: int(y) * stride + int(x) * n + 3])


def frame_difference(path_a, path_b, skip=()):
    """Mean absolute difference per channel, leaving out the skip rects."""
    a, b = (GdkPixbuf.Pixbuf.new_from_file(p) for p in (path_a, path_b))
    n, stride = a.get_n_channels(), a.get_rowstride()
    da, db = a.get_pixels(), b.get_pixels()
    total = count = 0
    for y in range(0, a.get_height(), 4):
        for x in range(0, a.get_width(), 4):
            if any(rx <= x < rx + rw and ry <= y < ry + rh for rx, ry, rw, rh in skip):
                continue
            i = y * stride + x * n
            total += sum(abs(da[i + c] - db[i + c]) for c in range(3)) / 3
            count += 1
    return total / max(count, 1)


def open_window(app_id, title, color, live=False):
    s.apps.append(subprocess.Popen([sys.executable, script, app_id, title, color, focus_file,
                                    "1" if live else "0"],
                                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL))
    wait_until(lambda: title in windows(), 8)
    time.sleep(0.8)


open_window("org.example.Red", "Red", "#c01c28")
open_window("org.example.Blue", "Blue", "#1c71d8")
open_window("org.example.Live", "Live", "#2ec27e", live=True)
time.sleep(1)
s.shot("00-desktop")
check(focused() == "Live", f"Live focused at start: {focused()}")

# --- Super opens the window picker -----------------------------------------
check(open_overview(), "Super opens the overview")
check("windows-mode" in overview_classes(), f"window picker: {overview_classes()}")
path = s.shot("01-overview-light")
ids = {title: wid for title, (wid, _) in windows().items()}
for title, expect in (("Red", "r"), ("Blue", "b")):
    b = bounds(f"overview-window-{ids[title]}")
    if not b:
        check(False, f"{title} preview shown")
        continue
    r, g, bl = mean_color(path, b)
    ok = (r > 140 and g < 90 and bl < 90) if expect == "r" else (bl > 150 and r < 90)
    check(ok, f"{title} preview shows the window's contents: {(r, g, bl)} in {b}")
    check(b[2] < 560 and b[3] < 380, f"{title} preview is scaled down: {b}")
# The top bar is transparent over the dark overview background.
r, g, b = pixel(path, 400, 4)
check(max(r, g, b) < 60, f"top bar transparent over the overview background: {(r, g, b)}")
search = s.inspect("overview-search")
check(search and search["mapped"], "search entry shown")
thumb = bounds("overview-thumbnail-0")
check(thumb is not None and thumb[2] < 100, f"workspace thumbnails shown: {thumb}")

# --- Live preview updates -----------------------------------------------------
live = bounds(f"overview-window-{ids['Live']}")
before = mean_color(s.shot("02a-live"), live)
time.sleep(0.7)
after = mean_color(s.shot("02b-live"), live)
check(sum(abs(a - b) for a, b in zip(before, after)) > 30, f"live preview updates: {before} -> {after}")

# --- Hover chrome, close button ------------------------------------------------
blue = bounds(f"overview-window-{ids['Blue']}")
s.move_to(*center(blue), settle=0.6)
s.shot("04-hover")
close = s.inspect(f"overview-close-{ids['Blue']}")
check(close and close["mapped"] and close["opacity"] > 0.9, f"close button on hover: {close and close['opacity']}")
title = s.inspect(f"overview-title-{ids['Blue']}")
check(title and title["mapped"] and title["text"] == "Blue", "title shown on hover")
red_close = s.inspect(f"overview-close-{ids['Red']}")
check(not red_close["mapped"], "no close button on the other previews")
s.click(*center(close["bounds"]), settle=1.5)
check("Blue" not in windows(), "close button closes the window")
check(overview_shown(), "overview stays open after closing a window")
s.shot("05-after-close")

# --- Clicking a preview activates it ------------------------------------------
red = bounds(f"overview-window-{ids['Red']}")
s.click(*center(red), settle=0.2)
check(wait_until(lambda: not overview_shown()), "clicking a preview closes the overview")
check(wait_until(lambda: focused() == "Red"), f"clicking a preview activates it: {focused()}")

# --- Escape closes, focus returns ---------------------------------------------
check(open_overview(), "reopen")
s.key(XK_Escape, settle=0.1)
check(wait_until(lambda: not overview_shown()), "Escape closes the overview")
time.sleep(0.5)
check(focused() == "Red", f"focus returns to the previous window: {focused()}")
with open(focus_file) as f:
    check(f.read().split()[-1] == "Red", "the previous window is active again")

# --- Keyboard navigation -----------------------------------------------------
check(open_overview(), "reopen for keyboard navigation")
s.key(XK_Right, settle=0.3)
selected = [t for t in ("Red", "Live")
            if (s.inspect(f"overview-close-{ids[t]}") or {}).get("mapped")]
check(len(selected) == 1, f"an arrow key selects one preview: {selected}")
other = "Live" if selected == ["Red"] else "Red"
s.key(XK_Right if other == "Live" else XK_Left, settle=0.3)
check((s.inspect(f"overview-close-{ids[other]}") or {}).get("mapped"), f"arrow moves to {other}")
s.key(XK_Return, settle=0.2)
check(wait_until(lambda: not overview_shown()), "Enter closes the overview")
check(wait_until(lambda: focused() == other), f"Enter activates the selected window: {focused()}")

# --- Workspace thumbnails ---------------------------------------------------------
check(open_overview(), "reopen for thumbnails")
s.click(*center(bounds("overview-thumbnail-1")), settle=1.0)
check(s.state()["active-workspace"] == 1, "clicking a thumbnail switches workspace")
check(overview_shown(), "overview stays open")
s.shot("16-other-workspace")
s.click(*center(bounds("overview-thumbnail-0")), settle=1.0)
check(s.state()["active-workspace"] == 0, "and back")

# --- Drag a preview onto a thumbnail -----------------------------------------
red = bounds(f"overview-window-{ids['Red']}")
target = center(bounds("overview-thumbnail-2"))
x, y = center(red)
s.move_to(x, y, settle=0.3)
s.button(True)
steps = 20
for i in range(steps):
    s.move_by((target[0] - x) / steps, (target[1] - y) / steps, settle=0.03)
time.sleep(0.3)
s.shot("06-drag")
s.button(False)
check(wait_until(lambda: windows()["Red"][1]["workspace"] == 2),
      f"dragging a preview to a thumbnail moves the window: {windows()['Red'][1]['workspace']}")
call("MoveWindowToWorkspace", GLib.Variant("(ti)", (ids["Red"], 0)))
time.sleep(0.8)
check(close_overview(), "close after drag")

# --- Type to search ----------------------------------------------------------
check(open_overview(), "reopen for search")
for keysym in (ord("f"), ord("i")):
    s.key(keysym, settle=0.1)
time.sleep(0.6)
search = s.inspect("overview-search")
check(search["text"] == "fi", f"typing goes to the search entry: {search['text']!r}")
check("search-mode" in overview_classes(), f"search results shown: {overview_classes()}")
results = s.inspect("search-results")
check(results is None or results["mapped"], "SearchResults mapped")
s.shot("14-search")
s.key(XK_Escape, settle=0.5)
check(s.inspect("overview-search")["text"] == "" and overview_shown(), "Escape clears the search")
check("windows-mode" in overview_classes(), "back to the window picker")

# --- App grid ------------------------------------------------------------------
s.panel("Activate", "overview-show-apps", settle=0.8)
check("apps-mode" in overview_classes(), f"apps mode: {overview_classes()}")
grid = s.inspect("app-grid")
check(grid is None or grid["mapped"], "AppGrid shown")
names = s.panel("ListNames")[0]
check("grid-app-org.example.Calculator" in names, "installed apps appear in the overview's app grid")
s.shot("15-app-grid")
s.key(XK_Escape, settle=0.6)
check("windows-mode" in overview_classes() and overview_shown(), "Escape returns from the app grid")
check(close_overview(), "Escape closes")

# --- Activities button and hot corner -------------------------------------------
activities = s.inspect("activities")
s.click(*center(activities["bounds"]), settle=0.2)
check(wait_until(overview_shown), "Activities button opens the overview")
time.sleep(0.5)
s.click(*center(activities["bounds"]), settle=0.2)
check(wait_until(lambda: not overview_shown()), "Activities button closes it")

s.move_to(300, 300, settle=0.3)
s.move_to(0, 0, settle=0.2)
check(wait_until(overview_shown), "hot corner opens the overview")
time.sleep(0.6)
s.move_to(300, 300, settle=0.3)
s.move_to(0, 0, settle=0.2)
check(wait_until(lambda: not overview_shown()), "hot corner closes it")
time.sleep(0.5)
s.gsettings(INTERFACE, "enable-hot-corners", "false")
s.move_to(300, 300, settle=0.3)
s.move_to(0, 0, settle=1.0)
check(not overview_shown(), "no hot corner with enable-hot-corners off")
s.gsettings(INTERFACE, "enable-hot-corners", "true")
s.move_to(600, 600, settle=0.3)

# --- Clicking empty space closes -----------------------------------------------
check(open_overview(), "reopen for an empty click")
card = bounds("overview-workspace-0")
s.click(card[0] + 20, card[1] + card[3] - 20, settle=0.2)
check(wait_until(lambda: not overview_shown()), "clicking the workspace background closes")
time.sleep(0.5)

# --- The first frame looks like the desktop -----------------------------------
desktop = s.shot("08-desktop-before")
s.panel("SetValue", "overview-slowdown", 400.0, settle=0)
super_key()
wait_until(overview_shown)
first = s.shot("09-first-frames")
s.panel("SetValue", "overview-slowdown", 1.0, settle=0)
live_rect = windows()["Live"][1]["rect"]
diff = frame_difference(desktop, first, skip=[(0, 0, 1280, 40), live_rect])
check(diff < 8, f"the overview starts out looking like the desktop: mean difference {diff:.1f}")
s.key(XK_Escape, settle=0.1)
wait_until(lambda: not overview_shown())
time.sleep(0.5)

# --- Mid-animation frames --------------------------------------------------------
s.panel("SetValue", "overview-slowdown", 12.0, settle=0)
super_key()
time.sleep(0.5)
s.shot("10-opening-early")
time.sleep(0.6)
s.shot("11-opening-late")
time.sleep(3.0)
s.key(XK_Escape, settle=0.3)
s.shot("12-closing")
wait_until(lambda: not overview_shown(), 6)
s.panel("SetValue", "overview-slowdown", 1.0, settle=0)
time.sleep(0.5)

# --- Dark style -------------------------------------------------------------------
s.gsettings(INTERFACE, "color-scheme", "prefer-dark")
check(open_overview(), "opens in dark style")
s.shot("02-overview-dark")
check(close_overview(), "closes in dark style")
s.gsettings(INTERFACE, "color-scheme", "default")

for app in s.apps:
    app.terminate()
print("FAILURES:", ", ".join(failures) if failures else "none")
