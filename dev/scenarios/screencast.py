"""Screen recording: the screenshot UI's video mode (Screen and Selection),
the red top-bar indicator (stopped with a real click), the
show-screen-recording-ui toggle (relayed as the "screencast" ShowUI mode),
the "Screencast recorded" notification (the UI is opened with the quick
settings button, which uses the private ShowUI; the shortcut through a
debug hook running the same code as the compositor's "screencast" ShowUI), org.gnome.Shell.Screencast with
its caller policy, and locking stopping a recording.

Recordings are decoded with GStreamer (Python Gst) to count frames, check
their size and see the test window change colour.

The caller policy is the real one (no SHELLPLUS_SCREENSHOT_ALLOW_ALL): a
plain python3 caller is refused, a root-owned copy of python3 named
/usr/libexec/xdg-desktop-portal-gnome is allowed.
"""
import glob
import os
import shutil
import subprocess
import sys
import time

sys.path.insert(0, "/src/dev")
from driver import Session  # noqa: E402

import gi  # noqa: E402

gi.require_version("Gst", "1.0")
from gi.repository import Gio, GLib, Gst  # noqa: E402

Gst.init(None)

OUT = sys.argv[1]
CASTS = os.path.expanduser("~/Videos/Screencasts")
NAME, PATH, IFACE = "org.gnome.Shell.Screencast", "/org/gnome/Shell/Screencast", "org.gnome.Shell.Screencast"
RETURN = 0xff0d

s = Session(OUT)
bus = s.bus
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


def wait_for(predicate, timeout=10.0):
    end = time.time() + timeout
    while time.time() < end:
        pump(0.1)
        if predicate():
            return True
    return predicate()


def names():
    return set(s.panel("ListNames", settle=0)[0])


def ui_open():
    return "screenshot-ui" in names()


def recording():
    state = s.inspect("screencast-indicator")
    return bool(state and state["mapped"])


def click_widget(name):
    x, y, w, h = s.inspect(name)["bounds"]
    s.click(int(x + w / 2), int(y + h / 2), settle=0.5)


def open_ui():
    """The quick settings button, which calls the private ShowUI."""
    s.panel("CloseMenus")
    s.panel("Activate", "qs-button")
    s.panel("Activate", "qs-screenshot")


def shortcut():
    """What show-screen-recording-ui (Ctrl+Shift+Alt+R) does."""
    s.panel("Activate", "screenshot-screencast-shortcut")


def casts():
    return set(glob.glob(os.path.join(CASTS, "Screencast From *.webm")))


def analyse(path, spot=None):
    """(frames, width, height, distinct colours at `spot`) of a recording."""
    pipeline = Gst.parse_launch(
        "filesrc name=src ! matroskademux ! decodebin ! videoconvert ! "
        "video/x-raw,format=RGB ! appsink name=sink sync=false")
    pipeline.get_by_name("src").set_property("location", path)
    sink = pipeline.get_by_name("sink")
    pipeline.set_state(Gst.State.PLAYING)
    frames, size, colours = 0, (0, 0), set()
    while True:
        sample = sink.emit("try-pull-sample", 5 * Gst.SECOND)
        if sample is None:
            break
        frames += 1
        st = sample.get_caps().get_structure(0)
        size = (st.get_value("width"), st.get_value("height"))
        if spot:
            buf = sample.get_buffer()
            ok, info = buf.map(Gst.MapFlags.READ)
            if ok:
                stride = (size[0] * 3 + 3) & ~3
                i = spot[1] * stride + spot[0] * 3
                colours.add(tuple(c // 32 for c in info.data[i:i + 3]))
                buf.unmap(info)
    pipeline.set_state(Gst.State.NULL)
    return frames, size[0], size[1], colours


def is_webm(path):
    with open(path, "rb") as f:
        head = f.read(64)
    return head[:4] == b"\x1a\x45\xdf\xa3" and b"webm" in head


def windows():
    return bus.call_sync("org.gnome.ShellPlus", "/org/gnome/ShellPlus", "org.gnome.ShellPlus.Overview",
                         "ListWindows", None, GLib.VariantType("(a(ta{sv}))"),
                         Gio.DBusCallFlags.NONE, 10000, None).unpack()[0]


# A window cycling through colours (red, green, blue) every 300 ms.
BLINK_WINDOW = """
import gi
gi.require_version("Gtk", "4.0")
from gi.repository import Gdk, GLib, Gtk
app = Gtk.Application(application_id="org.example.Blink")
COLOURS = ["c0", "c1", "c2"]
def activate(app):
    css = Gtk.CssProvider()
    css.load_from_string("window.c0 { background: rgb(220,30,30); } window.c1 { background: rgb(30,200,30); }"
                         " window.c2 { background: rgb(30,30,220); }")
    Gtk.StyleContext.add_provider_for_display(Gdk.Display.get_default(), css, 800)
    w = Gtk.ApplicationWindow(application=app, title="Blink", decorated=False,
                              default_width=400, default_height=300)
    state = [0]
    w.add_css_class("c0")
    def tick():
        w.remove_css_class(COLOURS[state[0] % 3])
        state[0] += 1
        w.add_css_class(COLOURS[state[0] % 3])
        return True
    GLib.timeout_add(300, tick)
    w.present()
app.connect("activate", activate)
app.run([])
"""

# Real input first (the first event can be lost while the device appears).
s.move_to(640, 600)
s.click(640, 600)
s.key(0xffe1)

s.apps.append(subprocess.Popen([sys.executable, "-c", BLINK_WINDOW]))
time.sleep(3)
listed = [p for _, p in windows() if p["title"] == "Blink"]
check(len(listed) == 1, "blinking window listed")
wx, wy, ww, wh = listed[0]["rect"] if listed else (440, 250, 400, 300)
centre = (wx + ww // 2, wy + wh // 2)

check(not recording(), "no indicator before recording")

# --- Screen recording from the UI, stopped with the indicator ------------

open_ui()
check(wait_for(ui_open), "ShowUI opens the screenshot UI")
time.sleep(0.5)
cast = s.inspect("screenshot-cast")
check(bool(cast and cast["sensitive"]), "the video switch is enabled")
click_widget("screenshot-cast")
check("cast" in s.inspect("screenshot-capture")["classes"], "capture button is red in video mode")
check(not s.inspect("screenshot-mode-window")["sensitive"], "Window mode is off in video mode")
click_widget("screenshot-mode-screen")
s.shot("01-ui-video")
before = casts()
click_widget("screenshot-capture")
check(wait_for(lambda: not ui_open()), "UI closes when recording starts")
check(wait_for(recording), "the recording indicator appears")
time.sleep(2.2)
s.shot("02-indicator")
label = (s.inspect("screencast-time") or {}).get("text", "")
check(label in ("00:02", "00:03", "00:04"), f"elapsed time shown: {label!r}")
classes = s.inspect("screencast-indicator")["classes"]
check("screen-recording-indicator" in classes, f"indicator classes {classes}")
time.sleep(1)
click_widget("screencast-indicator")
check(wait_for(lambda: not recording()), "clicking the indicator stops the recording")
new = casts() - before
check(len(new) == 1, f"one screencast saved: {new}")
if new:
    path = new.pop()
    check(is_webm(path), "the file is WebM")
    frames, w, h, colours = analyse(path, centre)
    print(f"screen recording: {frames} frames {w}x{h}, colours {colours}", flush=True)
    check(frames > 1, f"more than one frame: {frames}")
    check((w, h) == (1280, 800), f"screen recording is 1280x800: {w}x{h}")
    check(len(colours) >= 2, f"the window changes colour in the video: {colours}")
check(wait_for(lambda: "notification-banner" in names()), "notification shown")
check("notification-banner-action-0" in names(), "notification has the Show in Files action")
s.shot("03-notification")
s.panel("CloseMenus")

# --- Selection, through the show-screen-recording-ui path ----------------

before = casts()
shortcut()
check(wait_for(ui_open), "the screencast shortcut opens the UI")
time.sleep(0.5)
check(s.inspect("screenshot-cast")["classes"] is not None and
      "cast" in s.inspect("screenshot-capture")["classes"], "it opens in video mode")
s.move_to(200, 150)
s.button(True)
for _ in range(8):
    s.move_by(40, 30)
time.sleep(0.1)
s.button(False)
time.sleep(0.3)
s.shot("04-ui-selection")
s.key(RETURN)
check(wait_for(recording), "selection recording started")
time.sleep(2)
shortcut()
check(wait_for(lambda: not recording()), "the shortcut stops the recording")
time.sleep(0.5)
check(not ui_open(), "and doesn't open the UI")
new = casts() - before
check(len(new) == 1, f"selection screencast saved: {new}")
if new:
    frames, w, h, _ = analyse(new.pop())
    print(f"selection recording: {frames} frames {w}x{h}", flush=True)
    check((w, h) == (320, 240) and frames > 1, f"selection recording is 320x240 with frames: {w}x{h}, {frames}")
s.panel("CloseMenus")

# --- org.gnome.Shell.Screencast ------------------------------------------

owner = bus.call_sync("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus",
                      "NameHasOwner", GLib.Variant("(s)", (NAME,)), None,
                      Gio.DBusCallFlags.NONE, 1000, None).unpack()[0]
check(owner, "org.gnome.Shell.Screencast is owned")

try:
    bus.call_sync(NAME, PATH, IFACE, "Screencast", GLib.Variant("(sa{sv})", ("/tmp/denied", {})),
                  None, Gio.DBusCallFlags.NONE, 10000, None)
    check(False, "python3 caller refused")
except GLib.Error as e:
    check("not allowed" in e.message and not os.path.exists("/tmp/denied.webm"),
          f"python3 caller refused: {e.message}")
check(not recording(), "nothing recording after the refusal")

API_CLIENT = """
import sys, time
from gi.repository import Gio, GLib
bus = Gio.bus_get_sync(Gio.BusType.SESSION)
def call(method, args):
    return bus.call_sync("org.gnome.Shell.Screencast", "/org/gnome/Shell/Screencast",
                         "org.gnome.Shell.Screencast", method, args, None,
                         Gio.DBusCallFlags.NONE, 20000, None).unpack()
try:
    print("supported", bus.call_sync("org.gnome.Shell.Screencast", "/org/gnome/Shell/Screencast",
          "org.freedesktop.DBus.Properties", "Get",
          GLib.Variant("(ss)", ("org.gnome.Shell.Screencast", "ScreencastSupported")),
          None, Gio.DBusCallFlags.NONE, 5000, None).unpack()[0])
    try:
        call("ScreencastArea", GLib.Variant("(iiiisa{sv})", (1200, 700, 200, 200, "/tmp/bad", {})))
        print("bad-area accepted")
    except GLib.Error as e:
        print("bad-area", e.message)
    print("start", call("ScreencastArea", GLib.Variant("(iiiisa{sv})",
          (100, 100, 400, 200, "/tmp/api-%d", {"draw-cursor": GLib.Variant("b", False)}))))
    print("second", call("Screencast", GLib.Variant("(sa{sv})", ("/tmp/api-second", {}))))
    time.sleep(2.5)
    print("stop", call("StopScreencast", None))
    print("stop-again", call("StopScreencast", None))
except GLib.Error as e:
    print("error", e.message)
"""
portal = "/usr/libexec/xdg-desktop-portal-gnome"
shutil.copy(os.path.realpath(sys.executable), portal)
try:
    res = subprocess.run([portal, "-I", "-c", API_CLIENT], capture_output=True, text=True, timeout=60)
finally:
    os.remove(portal)
print(res.stdout, res.stderr[-300:], flush=True)
lines = dict(line.split(" ", 1) for line in res.stdout.splitlines() if " " in line)
check(lines.get("supported") == "True", f"ScreencastSupported: {lines.get('supported')}")
check("Invalid params" in lines.get("bad-area", ""), f"off-screen area refused: {lines.get('bad-area')}")
start = lines.get("start", "")
date = time.strftime("%Y-%m-%d")
check(start.startswith("(True, '/tmp/api-") and f"{date}.webm" in start, f"ScreencastArea started: {start}")
check(lines.get("second") == "(False, '')", f"a second recording is refused: {lines.get('second')}")
check(lines.get("stop") == "(True,)", f"StopScreencast: {lines.get('stop')}")
check(lines.get("stop-again") == "(False,)", f"StopScreencast with nothing running: {lines.get('stop-again')}")
api = f"/tmp/api-{date}.webm"
if os.path.exists(api):
    frames, w, h, _ = analyse(api)
    print(f"API recording: {frames} frames {w}x{h}", flush=True)
    check(is_webm(api) and (w, h) == (400, 200) and frames > 1, f"API recording 400x200: {w}x{h}, {frames}")
else:
    check(False, "API recording saved")
check(wait_for(lambda: not recording()), "indicator gone after the API recording")

# --- Locking stops the recording ----------------------------------------

before = casts()
open_ui()
check(wait_for(ui_open), "UI open for the lock test")
time.sleep(0.5)
click_widget("screenshot-cast")
click_widget("screenshot-mode-screen")
click_widget("screenshot-capture")
check(wait_for(recording), "recording before locking")
time.sleep(2)
bus.call_sync("org.gnome.ScreenSaver", "/org/gnome/ScreenSaver", "org.gnome.ScreenSaver", "Lock",
              None, None, Gio.DBusCallFlags.NONE, 10000, None)
check(wait_for(lambda: s.state().get("locked", False)), "locked")
check(wait_for(lambda: not recording()), "locking stops the recording")
time.sleep(1)
new = casts() - before
if new:
    frames, w, h, _ = analyse(new.pop())
    check(frames > 1, f"the stopped recording is complete: {frames} frames {w}x{h}")
else:
    check(False, "the recording stopped by locking was saved")
try:
    r = bus.call_sync(NAME, PATH, IFACE, "Screencast", GLib.Variant("(sa{sv})", ("/tmp/locked", {})),
                      None, Gio.DBusCallFlags.NONE, 10000, None).unpack()
    check(r == (False, ""), f"no recording while locked: {r}")
except GLib.Error as e:
    check(True, f"no recording while locked: {e.message}")
s.shot("05-locked")

for app in s.apps:
    app.terminate()
print("FAILURES:", ", ".join(failures) if failures else "none")
