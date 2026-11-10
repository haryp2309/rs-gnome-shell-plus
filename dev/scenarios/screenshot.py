"""Screenshots: GNOME Shell's org.gnome.Shell.Screenshot API, the caller
allowlist, and the interactive screenshot UI (selection drawn with real
pointer events, Screen and Window modes, Escape), with the saved PNGs,
the clipboard and the notification.

session.sh sets SHELLPLUS_SCREENSHOT_ALLOW_ALL=1 for this scenario. The
allowlist itself is checked against a second, compositor-only session
without it (`screenshot.py --deny-check`, run under its own
dbus-run-session).
"""
import glob
import os
import shutil
import subprocess
import sys
import tempfile
import time

sys.path.insert(0, "/src/dev")

import gi  # noqa: E402

gi.require_version("GdkPixbuf", "2.0")
from gi.repository import GdkPixbuf, Gio, GLib  # noqa: E402

NAME, PATH, IFACE = "org.gnome.Shell.Screenshot", "/org/gnome/Shell/Screenshot", "org.gnome.Shell.Screenshot"
PRIV = ("org.gnome.ShellPlus", "/org/gnome/ShellPlus", "org.gnome.ShellPlus.Screenshot")
COLOUR = (200, 40, 120)
SHOTS = os.path.expanduser("~/Pictures/Screenshots")
ESCAPE, RETURN = 0xff1b, 0xff0d

failures = []


def check(ok, what):
    print(("PASS " if ok else "FAIL ") + what, flush=True)
    if not ok:
        failures.append(what)


def pump(seconds):
    ctx = GLib.MainContext.default()
    end = time.time() + seconds
    while time.time() < end:
        while ctx.iteration(False):
            pass
        time.sleep(0.01)


def png(path):
    pb = GdkPixbuf.Pixbuf.new_from_file(path)
    return pb


def pixel(pb, x, y):
    data = pb.get_pixels()
    i = y * pb.get_rowstride() + x * pb.get_n_channels()
    return tuple(data[i:i + 3])


def near(a, b, tolerance=3):
    return all(abs(p - q) <= tolerance for p, q in zip(a, b))


# --- Allowlist check in a session without SHELLPLUS_SCREENSHOT_ALLOW_ALL ---

CALL_SCREENSHOT = """
import sys
from gi.repository import Gio, GLib
bus = Gio.bus_get_sync(Gio.BusType.SESSION)
try:
    r = bus.call_sync("org.gnome.Shell.Screenshot", "/org/gnome/Shell/Screenshot",
                      "org.gnome.Shell.Screenshot", "Screenshot",
                      GLib.Variant("(bbs)", (False, False, sys.argv[1])), None,
                      Gio.DBusCallFlags.NONE, 5000, None)
    print("ok", r.unpack())
except GLib.Error as e:
    print("error", e.message)
"""


def deny_check():
    build = os.environ.get("BUILD", "/tmp/build")
    env = dict(os.environ)
    env.pop("SHELLPLUS_SCREENSHOT_ALLOW_ALL", None)
    env["SHELLPLUS_PANEL"] = "/bin/true"
    log = open("/tmp/deny-compositor.log", "w")
    compositor = subprocess.Popen([f"{build}/gnome-shell-plus", "--wayland", "--headless", "--no-x11",
                                   "--wayland-display", "wayland-deny", "--virtual-monitor", "640x480"],
                                  env=env, stdout=log, stderr=log)
    bus = Gio.bus_get_sync(Gio.BusType.SESSION)
    for _ in range(100):
        owner = bus.call_sync("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus",
                              "NameHasOwner", GLib.Variant("(s)", (NAME,)), None,
                              Gio.DBusCallFlags.NONE, 1000, None).unpack()[0]
        if owner:
            break
        time.sleep(0.1)
    time.sleep(1)

    def call(method, args, iface=IFACE, name=NAME, path=PATH):
        try:
            bus.call_sync(name, path, iface, method, args, None, Gio.DBusCallFlags.NONE, 5000, None)
            return "ok"
        except GLib.Error as e:
            return e.message

    out = "/tmp/deny-shot.png"
    r = call("Screenshot", GLib.Variant("(bbs)", (False, False, out)))
    check("not allowed" in r and not os.path.exists(out), f"python3 caller refused: {r}")
    r = call("ScreenshotArea", GLib.Variant("(iiiibs)", (0, 0, 10, 10, False, out)))
    check("not allowed" in r, f"ScreenshotArea refused: {r}")
    r = call("CaptureScreen", GLib.Variant("(b)", (False,)), PRIV[2], PRIV[0], PRIV[1])
    check("not allowed" in r, f"private CaptureScreen refused for non-panel: {r}")

    python = os.path.realpath(sys.executable)
    # Right name but not a system binary: refused.
    fake = os.path.join(tempfile.mkdtemp(), "gnome-screenshot")
    shutil.copy(python, fake)
    res = subprocess.run([fake, "-I", "-c", CALL_SCREENSHOT, out], capture_output=True, text=True)
    check("not allowed" in res.stdout, f"/tmp/.../gnome-screenshot refused: {res.stdout.strip()} {res.stderr[-200:]}")
    # A root-owned system binary with an allowed name: accepted.
    portal = "/usr/libexec/xdg-desktop-portal-gnome"
    shutil.copy(python, portal)
    try:
        res = subprocess.run([portal, "-I", "-c", CALL_SCREENSHOT, out], capture_output=True, text=True)
    finally:
        os.remove(portal)
    check(res.stdout.startswith("ok") and os.path.exists(out),
          f"{portal} allowed: {res.stdout.strip()} {res.stderr[-200:]}")
    if os.path.exists(out):
        check(png(out).get_width() == 640, "its screenshot is of the 640x480 session")

    compositor.terminate()
    compositor.wait(5)
    print("FAILURES:", ", ".join(failures) if failures else "none")
    sys.exit(1 if failures else 0)


if sys.argv[1:] == ["--deny-check"]:
    deny_check()

# --- Main session ------------------------------------------------------------

from driver import Session  # noqa: E402

s = Session(sys.argv[1])
bus = s.bus
os.makedirs("/tmp/shots", exist_ok=True)


def call(method, args=None, reply=None, iface=IFACE, name=NAME, path=PATH):
    return bus.call_sync(name, path, iface, method, args,
                         GLib.VariantType(reply) if reply else None,
                         Gio.DBusCallFlags.NONE, 10000, None).unpack()


def private(method, args=None):
    return call(method, args, iface=PRIV[2], name=PRIV[0], path=PRIV[1])


def call_async(method, args=None):
    """Starts a call; returns a dict filled with 'result' or 'error'."""
    box = {}

    def done(conn, res):
        try:
            box["result"] = conn.call_finish(res).unpack()
        except GLib.Error as e:
            box["error"] = e.message
    bus.call(NAME, PATH, IFACE, method, args, None, Gio.DBusCallFlags.NONE, 60000, None, done)
    return box


def names():
    return set(s.panel("ListNames", settle=0)[0])


def wait_for(predicate, timeout=8.0):
    end = time.time() + timeout
    while time.time() < end:
        pump(0.1)
        if predicate():
            return True
    return False


def ui_open():
    return "screenshot-ui" in names()


def drag(x0, y0, x1, y1, steps=8):
    s.move_to(x0, y0)
    s.button(True)
    for i in range(steps):
        s.move_by((x1 - x0) / steps, (y1 - y0) / steps)
    time.sleep(0.1)
    s.button(False)
    time.sleep(0.3)


def click_widget(name):
    x, y, w, h = s.inspect(name)["bounds"]
    s.click(int(x + w / 2), int(y + h / 2), settle=0.5)


def saved():
    return set(glob.glob(os.path.join(SHOTS, "Screenshot From *.png")))


def windows():
    return call("ListWindows", reply="(a(ta{sv}))", iface="org.gnome.ShellPlus.Overview",
                name="org.gnome.ShellPlus", path="/org/gnome/ShellPlus")[0]


COLOUR_WINDOW = """
import gi
gi.require_version("Gtk", "4.0")
from gi.repository import Gdk, Gtk
app = Gtk.Application(application_id="org.example.Colour")
def activate(app):
    css = Gtk.CssProvider()
    css.load_from_string("window.colour { background: rgb(200, 40, 120); }")
    Gtk.StyleContext.add_provider_for_display(Gdk.Display.get_default(), css, 800)
    w = Gtk.ApplicationWindow(application=app, title="Colour", decorated=False,
                              default_width=400, default_height=300)
    w.add_css_class("colour")
    w.present()
app.connect("activate", activate)
app.run([])
"""

CLIPBOARD_READER = """
import sys
import gi
gi.require_version("Gtk", "4.0")
from gi.repository import GLib, Gtk
out = sys.argv[1]
app = Gtk.Application(application_id="org.example.Clipboard")
def activate(app):
    w = Gtk.ApplicationWindow(application=app, title="Clipboard", default_width=200, default_height=120)
    started = []
    def read():
        cb = w.get_clipboard()
        def got(cb, res):
            try:
                t = cb.read_texture_finish(res)
                text = f"{t.get_width()} {t.get_height()} {cb.get_formats().to_string()}"
            except Exception as e:
                text = f"error {e} {cb.get_formats().to_string()}"
            open(out, "w").write(text)
            app.quit()
        cb.read_texture_async(None, got)
        return False
    def on_active(*_):
        if w.is_active() and not started:
            started.append(1)
            GLib.timeout_add(400, read)
    w.connect("notify::is-active", on_active)
    w.present()
app.connect("activate", activate)
app.run([])
"""


def read_clipboard():
    out = "/tmp/clipboard.txt"
    if os.path.exists(out):
        os.remove(out)
    proc = subprocess.Popen([sys.executable, "-c", CLIPBOARD_READER, out],
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        proc.wait(10)
    except subprocess.TimeoutExpired:
        proc.kill()
    return open(out).read() if os.path.exists(out) else ""


# Real input: set up the virtual pointer and keyboard first (the very first
# click or key press can be lost while the device appears).
s.move_to(640, 600)
s.click(640, 600)
s.key(0xffe1)  # Shift

# Bus name.
owner = bus.call_sync("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus",
                      "NameHasOwner", GLib.Variant("(s)", (NAME,)), None,
                      Gio.DBusCallFlags.NONE, 1000, None).unpack()[0]
check(owner, "org.gnome.Shell.Screenshot is owned")

# A solid-coloured 400x300 window.
s.apps.append(subprocess.Popen([sys.executable, "-c", COLOUR_WINDOW]))
time.sleep(3)
listed = [p for _, p in windows() if p["title"] == "Colour"]
check(len(listed) == 1, "colour window listed")
wx, wy, ww, wh = listed[0]["rect"] if listed else (440, 250, 400, 300)
print("colour window at", (wx, wy, ww, wh), flush=True)

# Screenshot: whole stage, absolute path.
ok, used = call("Screenshot", GLib.Variant("(bbs)", (False, True, "/tmp/shots/full.png")))
check(ok and used == "/tmp/shots/full.png", f"Screenshot -> {ok}, {used}")
full = png(used)
check((full.get_width(), full.get_height()) == (1280, 800), f"full size {full.get_width()}x{full.get_height()}")
check(near(pixel(full, wx + ww // 2, wy + wh // 2), COLOUR),
      f"window colour in full shot: {pixel(full, wx + ww // 2, wy + wh // 2)}")
check(not near(pixel(full, 5, 5), COLOUR), "top bar isn't the colour")

# With the cursor.
ok, used = call("Screenshot", GLib.Variant("(bbs)", (True, False, "/tmp/shots/cursor.png")))
check(ok and png(used).get_width() == 1280, "Screenshot with cursor")

# Relative names go to ~/Pictures/Screenshots, never overwriting.
for p in glob.glob(os.path.join(SHOTS, "relative*.png")):
    os.remove(p)
ok, used = call("Screenshot", GLib.Variant("(bbs)", (False, False, "relative")))
check(ok and used == os.path.join(SHOTS, "relative.png") and os.path.exists(used), f"relative -> {used}")
ok, used = call("Screenshot", GLib.Variant("(bbs)", (False, False, "relative.png")))
check(ok and used == os.path.join(SHOTS, "relative-1.png"), f"second relative -> {used}")

# ScreenshotArea crops. Let any earlier capture's flash fade out first.
time.sleep(0.7)
ax, ay = wx + 50, wy + 40
ok, used = call("ScreenshotArea", GLib.Variant("(iiiibs)", (ax - 20, ay, 100, 50, True, "/tmp/shots/area.png")))
area = png(used)
check(ok and (area.get_width(), area.get_height()) == (100, 50), f"area size {area.get_width()}x{area.get_height()}")
check(near(pixel(area, 30, 25), COLOUR) and near(pixel(area, 99, 0), COLOUR), "area inside the window is the colour")
check(near(pixel(area, 0, 0), pixel(full, ax - 20, ay)), "area matches the full shot")
for bad in [(-1, 0, 10, 10), (0, 0, 0, 10), (1200, 700, 200, 200)]:
    try:
        call("ScreenshotArea", GLib.Variant("(iiiibs)", (*bad, False, "/tmp/shots/bad.png")))
        check(False, f"invalid area {bad} rejected")
    except GLib.Error as e:
        check("Invalid params" in e.message, f"invalid area {bad} rejected: {e.message}")

# ScreenshotWindow: the focused (colour) window.
ok, used = call("ScreenshotWindow", GLib.Variant("(bbbs)", (True, False, True, "/tmp/shots/window.png")))
win = png(used) if ok else None
check(win is not None and (win.get_width(), win.get_height()) == (ww, wh),
      f"window shot size {win and (win.get_width(), win.get_height())} == {(ww, wh)}")
if win:
    check(near(pixel(win, 0, 0), COLOUR) and near(pixel(win, ww - 1, wh - 1), COLOUR), "window shot is the colour")

# Empty filename: to the clipboard.
ok, used = call("Screenshot", GLib.Variant("(bbs)", (False, False, "")))
check(ok and used == "", "empty filename -> clipboard")

# FlashArea.
call("FlashArea", GLib.Variant("(iiii)", (100, 100, 300, 200)))
time.sleep(0.1)
s.shot("flash")
try:
    call("FlashArea", GLib.Variant("(iiii)", (0, 0, 0, 0)))
    check(False, "FlashArea rejects an empty area")
except GLib.Error:
    check(True, "FlashArea rejects an empty area")

# PickColor: click inside the colour window.
box = call_async("PickColor")
check(wait_for(ui_open), "PickColor shows the UI")
s.shot("pick-color")
s.click(wx + 100, wy + 100)
check(wait_for(lambda: box), f"PickColor answered: {box}")
color = box.get("result", ({},))[0].get("color")
check(color and near([c * 255 for c in color], COLOUR, 2), f"PickColor colour {color}")
wait_for(lambda: not ui_open())

# SelectArea: a real drag.
box = call_async("SelectArea")
check(wait_for(ui_open), "SelectArea shows the UI")
drag(100, 120, 300, 270)
check(wait_for(lambda: box), f"SelectArea answered: {box}")
check(box.get("result") == (100, 120, 200, 150), f"SelectArea rect {box.get('result')}")
check(wait_for(lambda: not ui_open()), "SelectArea UI gone")

# SelectArea cancelled with Escape.
box = call_async("SelectArea")
check(wait_for(ui_open), "SelectArea UI again")
time.sleep(0.5)  # mapped and focused
s.key(ESCAPE)
check(wait_for(lambda: box) and "cancelled" in box.get("error", ""), f"Escape cancels SelectArea: {box}")

# --- Interactive UI -------------------------------------------------------

# Selection mode: drag a new 320x240 selection, capture with Enter.
before = saved()
private("ShowUI", GLib.Variant("(s)", ("",)))
check(wait_for(ui_open), "ShowUI opens the screenshot UI")
time.sleep(0.5)
s.shot("ui-selection-default")
check(s.inspect("screenshot-mode-selection")["classes"] is not None, "mode buttons present")
drag(200, 150, 520, 390)
s.shot("ui-selection")
s.key(RETURN, settle=1.0)
check(wait_for(lambda: not ui_open()), "UI closes after capture")
new = saved() - before
check(len(new) == 1, f"one file saved: {new}")
if new:
    shot = png(new.pop())
    check((shot.get_width(), shot.get_height()) == (320, 240), f"selection size {shot.get_width()}x{shot.get_height()}")
check(wait_for(lambda: "notification-banner" in names()), "notification shown")
check("notification-banner-action-0" in names(), "notification has the Show in Files action")
s.shot("ui-notification")
clip = read_clipboard()
check(clip.startswith("320 240") and "image/png" in clip, f"clipboard has the image: {clip}")
s.panel("CloseMenus")

# The selection is remembered; Screen mode through the toolbar.
before = saved()
private("ShowUI", GLib.Variant("(s)", ("",)))
check(wait_for(ui_open), "UI open again")
time.sleep(0.5)
s.shot("ui-remembered")
click_widget("screenshot-mode-screen")
s.shot("ui-screen")
click_widget("screenshot-capture")
check(wait_for(lambda: not ui_open()), "UI closes after Screen capture")
new = saved() - before
check(len(new) == 1 and png(next(iter(new))).get_width() == 1280 and png(next(iter(new))).get_height() == 800,
      f"screen capture is 1280x800: {new}")

# Remembered selection: capture straight away with the capture button.
before = saved()
private("ShowUI", GLib.Variant("(s)", ("",)))
check(wait_for(ui_open), "UI open for the remembered selection")
time.sleep(0.5)
click_widget("screenshot-capture")
wait_for(lambda: not ui_open())
new = saved() - before
check(len(new) == 1 and png(next(iter(new))).get_width() == 320, "last selection remembered")

# Window mode: pick the colour window's thumbnail.
before = saved()
private("ShowUI", GLib.Variant("(s)", ("",)))
check(wait_for(ui_open), "UI open for Window mode")
time.sleep(0.5)
click_widget("screenshot-mode-window")
time.sleep(0.3)
s.shot("ui-window")
thumbs = sorted(n for n in names() if n.startswith("screenshot-window-"))
check(len(thumbs) >= 1, f"window thumbnails: {thumbs}")
if thumbs:
    click_widget(thumbs[0])
    click_widget("screenshot-capture")
wait_for(lambda: not ui_open())
new = saved() - before
if new:
    shot = png(next(iter(new)))
    check((shot.get_width(), shot.get_height()) == (ww, wh) and near(pixel(shot, 10, 10), COLOUR),
          f"window capture {shot.get_width()}x{shot.get_height()} of the colour window")
else:
    check(False, "window capture saved")

# Escape cancels without saving.
before = saved()
private("ShowUI", GLib.Variant("(s)", ("window",)))
check(wait_for(ui_open), "UI open (window mode)")
time.sleep(0.5)
s.key(ESCAPE)
check(wait_for(lambda: not ui_open()), "Escape closes the UI")
time.sleep(0.5)
check(saved() == before, "nothing saved after Escape")

# InteractiveScreenshot returns the file's URI.
box = call_async("InteractiveScreenshot")
check(wait_for(ui_open), "InteractiveScreenshot shows the UI")
time.sleep(0.5)
s.key(RETURN)
check(wait_for(lambda: box), f"InteractiveScreenshot answered: {box}")
res = box.get("result", (False, ""))
check(res[0] and res[1].startswith("file://") and os.path.exists(Gio.File.new_for_uri(res[1]).get_path()),
      f"InteractiveScreenshot -> {res}")
box = call_async("InteractiveScreenshot")
wait_for(ui_open)
time.sleep(0.5)
s.key(ESCAPE)
check(wait_for(lambda: box) and box.get("result") == (False, ""), f"cancelled InteractiveScreenshot: {box}")

# Quick settings button.
s.panel("CloseMenus")
s.panel("Activate", "qs-button")
s.shot("quick-settings")
check("qs-screenshot" in names(), "Take Screenshot button in quick settings")
s.panel("Activate", "qs-screenshot")
check(wait_for(ui_open), "Take Screenshot opens the UI")
time.sleep(0.5)
s.key(ESCAPE)
wait_for(lambda: not ui_open())

for app in s.apps:
    app.terminate()

# --- Allowlist, in a separate session without ALLOW_ALL --------------------
res = subprocess.run(["dbus-run-session", "--", sys.executable, os.path.abspath(__file__), "--deny-check"],
                     capture_output=True, text=True, timeout=120)
print(res.stdout, end="", flush=True)
for line in res.stdout.splitlines():
    if line.startswith("FAIL "):
        failures.append("deny-check: " + line[5:])
if res.returncode != 0 and not any(f.startswith("deny-check") for f in failures):
    failures.append(f"deny-check exited {res.returncode}: {res.stderr[-500:]}")

print("FAILURES:", ", ".join(failures) if failures else "none")
