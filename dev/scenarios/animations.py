"""Window animations: map, destroy, modal dialogs, minimize/unminimize and
the workspace switch slide, with and without enable-animations.

Uses an undecorated, solid-colored GTK4 test window so screenshots can be
checked by color, and the compositor's g_debug log (G_MESSAGES_DEBUG=shellplus)
for the actor state each effect ends in. dev/session.sh slows animations
down 10x for this scenario (SHELLPLUS_SLOW_ANIMATIONS) so frames captured
while recording show them mid-way.
"""
import glob
import os
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import time

sys.path.insert(0, "/src/dev")
from driver import SCREENCAST, Session  # noqa: E402

import gi  # noqa: E402

gi.require_version("GdkPixbuf", "2.0")
from gi.repository import GdkPixbuf, Gio, GLib  # noqa: E402

SLOW = float(os.environ.get("SHELLPLUS_SLOW_ANIMATIONS") or 1)
MAGENTA = (255, 0, 255)
GREEN = (0, 255, 0)
KEY_LEFTALT = 56
KEY_TAB = 15

TEST_WINDOW = r'''
import sys
import gi
gi.require_version("Gtk", "4.0")
from gi.repository import Gtk, Gdk, GLib

title = sys.argv[1]
css = Gtk.CssProvider()
css.load_from_string(
    "window.main { background: #ff00ff; } window.dialog { background: #00ff00; }")
Gtk.StyleContext.add_provider_for_display(Gdk.Display.get_default(), css, 800)
state = {}

def command(source, condition):
    line = sys.stdin.readline().strip()
    window = state["window"]
    if line == "minimize":
        window.minimize()
    elif line == "present":
        window.present()
    elif line == "dialog":
        dialog = Gtk.Window(title=title + " Dialog", transient_for=window, modal=True,
                            decorated=False, default_width=200, default_height=120)
        dialog.add_css_class("dialog")
        dialog.present()
        state["dialog"] = dialog
    elif line == "close-dialog":
        state.pop("dialog").destroy()
    elif line == "close" or not line:
        window.destroy()
        return False
    return True

def on_activate(app):
    window = Gtk.ApplicationWindow(application=app, title=title, decorated=False,
                                   default_width=400, default_height=300)
    window.add_css_class("main")
    window.present()
    state["window"] = window
    GLib.io_add_watch(sys.stdin, GLib.IO_IN | GLib.IO_HUP, command)

app = Gtk.Application(application_id="org.gnome.ShellPlus.Anim" + title.split()[-1])
app.connect("activate", on_activate)
app.run([])
'''

s = Session(sys.argv[1])
out = sys.argv[1]
log_path = os.path.join(out, "compositor.log")
failures = []


def check(ok, what):
    print(("ok   " if ok else "FAIL ") + what, flush=True)
    if not ok:
        failures.append(what)
    return ok


# --- compositor log ---------------------------------------------------------

log_pos = 0


def log_mark():
    global log_pos
    log_pos = os.path.getsize(log_path)


def log_since():
    with open(log_path, errors="replace") as f:
        f.seek(log_pos)
        return f.read()


def wait_log(pattern, timeout=None):
    """Waits for a regex in the compositor log since the last log_mark()."""
    timeout = timeout or 3 + 2 * SLOW
    deadline = time.time() + timeout
    while time.time() < deadline:
        m = re.search(pattern, log_since())
        if m:
            return m
        time.sleep(0.1)
    return None


def state_of(match):
    """Parses 'scale=.. opacity=.. translation=.. rect=x,y,wxh' from a log line."""
    line = match.group(0) if hasattr(match, "group") else match
    nums = lambda key: [float(v) for v in re.search(key + r"=([-\d.,x]+)", line)
                        .group(1).replace("x", ",").split(",")]
    return {"scale": nums("scale"), "opacity": nums("opacity")[0],
            "translation": nums("translation"), "rect": nums("rect")}


def settled(line_match, what):
    st = state_of(line_match)
    check(st["scale"] == [1.0, 1.0] and st["opacity"] == 255 and st["translation"] == [0, 0],
          f"{what}: ends at scale 1, opacity 255, no translation ({st})")
    return st


# --- screenshots ------------------------------------------------------------

def find_color(path, color, step=2, tolerance=60, faded=False):
    """Bounding box (x0, y0, x1, y1) and pixel count of @color in a PNG.

    With faded, also matches the color half-blended into the (blue)
    wallpaper, i.e. a window mid-fade."""
    pb = GdkPixbuf.Pixbuf.new_from_file(path)
    data = pb.get_pixels()
    n, stride = pb.get_n_channels(), pb.get_rowstride()
    r0, g0, b0 = color
    x0 = y0 = 10 ** 9
    x1 = y1 = -1
    count = 0
    for y in range(0, pb.get_height(), step):
        row = y * stride
        for x in range(0, pb.get_width(), step):
            i = row + x * n
            r, g, b = data[i], data[i + 1], data[i + 2]
            if faded and color == MAGENTA:
                hit = r > 100 and r - g > 60
            elif faded and color == GREEN:
                hit = g > 120 and g - r > 60 and g - b > -40
            else:
                hit = (abs(r - r0) < tolerance and abs(g - g0) < tolerance
                       and abs(b - b0) < tolerance)
            if hit:
                count += 1
                x0, y0, x1, y1 = min(x0, x), min(y0, y), max(x1, x), max(y1, y)
    if not count:
        return None, 0
    return (x0, y0, x1 + step, y1 + step), count


def bbox_matches(bbox, rect, slack=4):
    x, y, w, h = rect
    return bbox is not None and all(abs(a - b) <= slack for a, b in
                                    zip(bbox, (x, y, x + w, y + h)))


def record(action, seconds):
    """Runs action() while recording the screen; returns the frame paths."""
    tmp = tempfile.mkdtemp()
    session = s._call(SCREENCAST, "/org/gnome/Mutter/ScreenCast", SCREENCAST,
                      "CreateSession", GLib.Variant("(a{sv})", ({},)), "(o)")[0]
    stream = s._call(SCREENCAST, session, SCREENCAST + ".Session", "RecordMonitor",
                     GLib.Variant("(sa{sv})", ("", {})), "(o)")[0]
    loop = GLib.MainLoop()
    node = {}

    def on_added(_c, _s, _p, _i, _n, params):
        node["id"] = params[0]
        loop.quit()

    sub = s.bus.signal_subscribe(SCREENCAST, SCREENCAST + ".Stream", "PipeWireStreamAdded",
                                 stream, None, Gio.DBusSignalFlags.NONE, on_added)
    s._call(SCREENCAST, session, SCREENCAST + ".Session", "Start")
    GLib.timeout_add_seconds(10, loop.quit)
    loop.run()
    s.bus.signal_unsubscribe(sub)
    gst = subprocess.Popen(["gst-launch-1.0", "-q", "pipewiresrc", f"path={node.get('id', 0)}",
                            "!", "queue", "max-size-buffers=0", "max-size-time=0",
                            "max-size-bytes=0", "!", "videoconvert", "!",
                            "pngenc", "compression-level=1", "snapshot=false", "!",
                            "multifilesink", f"location={tmp}/frame%04d.png"])
    time.sleep(1.0)
    action()
    time.sleep(seconds)
    gst.send_signal(signal.SIGINT)
    try:
        gst.wait(10)
    except subprocess.TimeoutExpired:
        gst.kill()
    s._call(SCREENCAST, session, SCREENCAST + ".Session", "Stop")
    return sorted(glob.glob(f"{tmp}/frame*.png"))


def save_mid(frames, name, color, predicate):
    """Saves the first frame whose (bbox, count) of @color satisfies predicate."""
    for frame in frames:
        bbox, count = find_color(frame, color, step=4, faded=True)
        if predicate(bbox, count):
            path = os.path.join(out, f"{name}.png")
            shutil.copy(frame, path)
            print("saved", path, "bbox", bbox, flush=True)
            return bbox
    return None


# --- test windows -----------------------------------------------------------

class TestWindow:
    def __init__(self, title):
        self.title = title
        self.proc = subprocess.Popen([sys.executable, "-c", TEST_WINDOW, title],
                                     stdin=subprocess.PIPE, text=True,
                                     stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

    def send(self, command):
        self.proc.stdin.write(command + "\n")
        self.proc.stdin.flush()


def keycodes(*codes):
    rd_name, rd, _ = s._remote_desktop()
    iface = rd_name + ".Session"
    for code in codes:
        s._call(rd_name, rd, iface, "NotifyKeyboardKeycode", GLib.Variant("(ub)", (code, True)))
        time.sleep(0.05)
    for code in reversed(codes):
        s._call(rd_name, rd, iface, "NotifyKeyboardKeycode", GLib.Variant("(ub)", (code, False)))
        time.sleep(0.05)


def activate_workspace(index):
    s._call("org.gnome.ShellPlus", "/org/gnome/ShellPlus", "org.gnome.ShellPlus.Panel",
            "ActivateWorkspace", GLib.Variant("(i)", (index,)))


def esc(title):
    return re.escape(title)


def done_line(kind, title):
    return rf"animation: {kind} completed '{esc(title)}'[^\n]*"


# ---------------------------------------------------------------------------

print("slow factor:", SLOW, flush=True)
s.gsettings("org.gnome.desktop.interface", "enable-animations", "true")
keycodes(KEY_LEFTALT)  # the first injected key can get lost; also starts RD
time.sleep(0.5)

# --- map --------------------------------------------------------------------
log_mark()
holder = {}
frames = record(lambda: holder.setdefault("a", TestWindow("Anim A")), 1.5 + 0.15 * SLOW)
a = holder["a"]
m = wait_log(done_line("map", "Anim A"))
check(m is not None, "map of a normal window completes")
started = re.search(r"animation: map started 'Anim A'[^\n]*", log_since())
check(started is not None, "map is animated")
if started:
    st = state_of(started)
    check(st["scale"] == [0.9, 0.9] and st["opacity"] == 0,
          f"map starts at scale 0.9, transparent ({st})")
rect = settled(m, "map")["rect"] if m else (0, 0, 0, 0)
shot = s.shot("01-mapped")
bbox, _ = find_color(shot, MAGENTA)
check(bbox_matches(bbox, rect), f"mapped window fully drawn at its place: {bbox} vs {rect}")
full_w = bbox[2] - bbox[0] if bbox else 0
mid = save_mid(frames, "01-map-mid", MAGENTA,
               lambda b, n: n and full_w * 0.6 < b[2] - b[0] < full_w - 8)
check(mid is not None, f"captured the map mid-animation ({len(frames)} frames)")

# --- modal dialog -----------------------------------------------------------
log_mark()
frames = record(lambda: a.send("dialog"), 1.0 + 0.1 * SLOW)
m = wait_log(done_line("map", "Anim A Dialog"))
check(m is not None, "map of a modal dialog completes")
started = re.search(r"animation: map started 'Anim A Dialog'[^\n]*", log_since())
check(started is not None and "pivot=0.50,0.00" in started.group(0)
      and "scale=1.00,0.00" in started.group(0),
      f"modal dialog unfolds from its top edge ({started and started.group(0)})")
if m:
    drect = settled(m, "dialog map")["rect"]
    bbox, _ = find_color(s.shot("02-dialog"), GREEN)
    check(bbox_matches(bbox, drect), f"dialog drawn at its place: {bbox} vs {drect}")
save_mid(frames, "02-dialog-mid", GREEN,
         lambda b, n: n and b[3] - b[1] < (drect[3] if m else 0) - 8)

log_mark()
a.send("close-dialog")
check(wait_log(done_line("destroy", "Anim A Dialog")) is not None, "dialog destroy completes")
time.sleep(0.5)
bbox, _ = find_color(s.shot("03-dialog-closed"), GREEN)
check(bbox is None, "dialog gone after closing")

# --- minimize / unminimize --------------------------------------------------
log_mark()
frames = record(lambda: a.send("minimize"), 1.0 + 0.4 * SLOW)
m = wait_log(done_line("minimize", "Anim A"))
check(m is not None, "minimize completes")
if m:
    settled(m, "minimize")
bbox, _ = find_color(s.shot("04-minimized"), MAGENTA)
check(bbox is None, "minimized window is hidden")
# Mid-way the window has shrunk and moved toward the bottom center.
mid = save_mid(frames, "04-minimize-mid", MAGENTA,
               lambda b, n: n and b[2] - b[0] < full_w * 0.7 and b[1] > rect[1] + 20)
check(mid is not None, "captured the minimize mid-animation, heading down")

log_mark()
# Mutter's built-in Alt+Tab (no switcher UI) activates, and so unminimizes,
# the window.
frames = record(lambda: keycodes(KEY_LEFTALT, KEY_TAB), 1.0 + 0.4 * SLOW)
m = wait_log(done_line("unminimize", "Anim A"))
check(m is not None, "unminimize completes")
if m:
    settled(m, "unminimize")
bbox, _ = find_color(s.shot("05-unminimized"), MAGENTA)
check(bbox_matches(bbox, rect), f"unminimized window back at its place: {bbox} vs {rect}")
save_mid(frames, "05-unminimize-mid", MAGENTA,
         lambda b, n: n and b[2] - b[0] < full_w * 0.7)

# --- workspace switch -------------------------------------------------------
log_mark()
frames = record(lambda: activate_workspace(1), 1.0 + 0.25 * SLOW)
check(wait_log(r"animation: switch-workspace completed") is not None,
      "switch to workspace 2 completes")
moved = re.findall(r"switch-workspace moves '([^']*)'", log_since())
check(moved == ["Anim A"], f"only the test window slides, not the panel's: {moved}")
check(s.state()["active-workspace"] == 1, "workspace 2 active")
bbox, _ = find_color(s.shot("06-workspace-2"), MAGENTA)
check(bbox is None, "window of workspace 1 not shown on workspace 2")
mid = save_mid(frames, "06-switch-mid", MAGENTA,
               lambda b, n: n and b[0] < rect[0] - 40)
check(mid is not None, "captured the workspace slide mid-way (window moving left)")

log_mark()
frames = record(lambda: activate_workspace(0), 1.0 + 0.25 * SLOW)
check(wait_log(r"animation: switch-workspace completed") is not None,
      "switch back to workspace 1 completes")
bbox, _ = find_color(s.shot("07-workspace-1"), MAGENTA)
check(bbox_matches(bbox, rect), f"window back in place after switching back: {bbox} vs {rect}")
save_mid(frames, "07-switch-back-mid", MAGENTA,
         lambda b, n: n and b[0] > rect[0] + 40)

# --- destroy ----------------------------------------------------------------
log_mark()
frames = record(lambda: a.send("close"), 1.0 + 0.15 * SLOW)
m = wait_log(done_line("destroy", "Anim A"))
check(m is not None, "destroy completes")
time.sleep(0.5)
bbox, _ = find_color(s.shot("08-closed"), MAGENTA)
check(bbox is None, "closed window removed")
save_mid(frames, "08-destroy-mid", MAGENTA,
         lambda b, n: n and b[2] - b[0] < full_w - 8)
a.proc.wait(5)

# --- destroy while mapping (interrupted effect) -----------------------------
log_mark()
c = TestWindow("Anim C")
time.sleep(0.3 + 0.05 * SLOW)  # mid-way through the map animation
c.send("close")
m = wait_log(rf"animation: map (completed|interrupted|not animated) '{esc('Anim C')}'")
check(m is not None, "map of a window closed while mapping is completed")
check(wait_log(r"animation: destroy (completed|not animated) 'Anim C'") is not None,
      "window closed while mapping is destroyed")
time.sleep(0.5)
bbox, _ = find_color(s.shot("09-closed-while-mapping"), MAGENTA)
check(bbox is None, "window closed while mapping is gone")

# --- enable-animations = false ----------------------------------------------
s.gsettings("org.gnome.desktop.interface", "enable-animations", "false")
log_mark()
b = TestWindow("Anim B")
check(wait_log(r"animation: map not animated 'Anim B'") is not None, "map not animated when disabled")
time.sleep(1.0)
bbox_b, _ = find_color(s.shot("10-noanim-mapped"), MAGENTA)
check(bbox_b is not None, f"window visible right away without animations: {bbox_b}")
b.send("minimize")
check(wait_log(r"animation: minimize not animated 'Anim B'") is not None, "minimize not animated")
time.sleep(0.5)
check(find_color(s.shot("11-noanim-minimized"), MAGENTA)[0] is None, "minimized without animation")
keycodes(KEY_LEFTALT, KEY_TAB)
check(wait_log(r"animation: unminimize not animated 'Anim B'") is not None, "unminimize not animated")
time.sleep(0.5)
check(find_color(s.shot("12-noanim-unminimized"), MAGENTA)[0] == bbox_b,
      "unminimized without animation, same place")
activate_workspace(1)
check(wait_log(r"switch-workspace 0 -> 1 not animated") is not None, "switch not animated")
time.sleep(0.5)
check(find_color(s.shot("13-noanim-workspace-2"), MAGENTA)[0] is None, "switched without animation")
activate_workspace(0)
check(wait_log(r"switch-workspace 1 -> 0 not animated") is not None, "switch back not animated")
b.send("close")
check(wait_log(r"animation: destroy not animated 'Anim B'") is not None, "destroy not animated")
time.sleep(0.5)
check(find_color(s.shot("14-noanim-closed"), MAGENTA)[0] is None, "closed without animation")
check("started" not in "".join(l for l in log_since().splitlines() if "animation:" in l),
      "nothing animated with enable-animations off")
s.gsettings("org.gnome.desktop.interface", "enable-animations", "true")

# --- whole log --------------------------------------------------------------
log_pos = 0
log = log_since()
panel_lines = [l for l in log.splitlines() if "animation:" in l and
               any(t in l for t in ("'Top Bar'", "'Notification Banner'", "'On-Screen Display'"))
               and "not animated" not in l]
check(not panel_lines, f"panel windows never animate: {panel_lines[:3]}")
bad = [l for l in log.splitlines() if re.search(
    r"accounting|did not finish|CRITICAL|assertion", l)]
check(not bad, f"no effect accounting errors or criticals: {bad[:5]}")

print("FAILURES:", ", ".join(failures) if failures else "none")
