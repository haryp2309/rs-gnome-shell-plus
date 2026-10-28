"""The compositor's overview interface (org.gnome.ShellPlus.Overview):
window list, previews over shared memory, window actions, damage and
ShowOverview requests. No panel UI involved."""
import mmap
import os
import subprocess
import sys
import time

sys.path.insert(0, "/src/dev")
from driver import Session  # noqa: E402

from gi.repository import Gio, GLib  # noqa: E402

NAME, PATH, IFACE = "org.gnome.ShellPlus", "/org/gnome/ShellPlus", "org.gnome.ShellPlus.Overview"
KEY_LEFTMETA = 125

s = Session(sys.argv[1])
bus = s.bus
failures = []
signals = []


def check(ok, what):
    print(("PASS " if ok else "FAIL ") + what, flush=True)
    if not ok:
        failures.append(what)


def call(method, args=None, reply=None):
    return bus.call_sync(NAME, PATH, IFACE, method, args,
                         GLib.VariantType(reply) if reply else None,
                         Gio.DBusCallFlags.NONE, 5000, None)


def pump(seconds):
    ctx = GLib.MainContext.default()
    end = time.time() + seconds
    while time.time() < end:
        ctx.iteration(False)
        time.sleep(0.01)


def windows():
    return {wid: props for wid, props in call("ListWindows", reply="(a(ta{sv}))")[0]}


bus.signal_subscribe(NAME, IFACE, None, PATH, None, Gio.DBusSignalFlags.NONE,
                     lambda *a: signals.append((a[4], a[5].unpack())))

check(windows() == {}, "no windows listed at start (panel windows left out)")

s.open_maximized_window()
pump(1)
listed = windows()
check(len(listed) == 1, f"one window listed: {listed}")
wid, props = next(iter(listed.items()))
check(props["title"] == "Maximized", "title")
check(props["app-id"] == "org.gnome.ShellPlus.SmokeTest", "app id")
check(props["workspace"] == 0 and props["focused"] and not props["minimized"], "workspace/focus/minimized")
x, y, w, h = props["rect"]
check(w > 1000 and h > 600, f"frame rect {props['rect']}")
check(any(sig == "WindowsChanged" for sig, _ in signals), "WindowsChanged sent")

# Preview through a memfd.
reply, fds = bus.call_with_unix_fd_list_sync(NAME, PATH, IFACE, "GetPreview",
                                             GLib.Variant("(tii)", (wid, 320, 320)),
                                             GLib.VariantType("(hiii)"),
                                             Gio.DBusCallFlags.NONE, 5000, None, None)
_, pw, ph, stride = reply.unpack()
fd = fds.get(0)
data = mmap.mmap(fd, stride * ph, prot=mmap.PROT_READ)
check(pw == 320 and 0 < ph <= 320, f"preview fits 320x320: {pw}x{ph}")
# The header bar of a light Adwaita window is near-white; the pixel is BGRA.
b, g, r, a = data[stride * 2 + 40 * 4: stride * 2 + 40 * 4 + 4]
check(a == 255 and r > 200 and g > 200 and b > 200, f"preview has window contents: {(r, g, b, a)}")
data.close()
os.close(fd)

# Damage only while the overview is active.
signals.clear()
call("SetOverviewActive", GLib.Variant("(b)", (True,)))
# A second window (another app id, since GApplication is single-instance).
s.apps.append(subprocess.Popen([sys.executable, "/src/dev/switcher-window.py",
                                "org.example.Second", "Second", "/dev/null"]))
pump(3)
damaged = [ids for sig, ids in signals if sig == "WindowsDamaged"]
check(bool(damaged), "WindowsDamaged sent while active")
call("SetOverviewActive", GLib.Variant("(b)", (False,)))
pump(0.5)
signals.clear()
pump(1.5)
check(not any(sig == "WindowsDamaged" for sig, _ in signals), "no damage signals when inactive")

# Actions.
listed = windows()
check(len(listed) == 2, "two windows")
first = min(listed)
call("ActivateWindow", GLib.Variant("(t)", (first,)))
pump(0.8)
check(windows()[first]["focused"], "ActivateWindow focuses")
call("MoveWindowToWorkspace", GLib.Variant("(ti)", (first, 1)))
pump(0.8)
check(windows()[first]["workspace"] == 1, "MoveWindowToWorkspace")
call("CloseWindow", GLib.Variant("(t)", (first,)))
pump(1.5)
check(first not in windows(), "CloseWindow closes")
try:
    call("ActivateWindow", GLib.Variant("(t)", (12345678,)))
    check(False, "unknown id rejected")
except GLib.Error:
    check(True, "unknown id rejected")

# Triggers: Super key and the panel interface's ToggleOverview.
signals.clear()
rd_name, rd, _ = s._remote_desktop()
for pressed in (True, False):
    s._call(rd_name, rd, rd_name + ".Session", "NotifyKeyboardKeycode",
            GLib.Variant("(ub)", (KEY_LEFTMETA, pressed)))
    time.sleep(0.05)
pump(0.8)
check(("ShowOverview", ("toggle",)) in signals, f"Super sends ShowOverview: {signals}")
signals.clear()
bus.call_sync(NAME, PATH, "org.gnome.ShellPlus.Panel", "ToggleOverview", None, None,
              Gio.DBusCallFlags.NONE, 5000, None)
pump(0.5)
check(("ShowOverview", ("toggle",)) in signals, "ToggleOverview sends ShowOverview")

for app in s.apps:
    app.terminate()
print("FAILURES:", ", ".join(failures) if failures else "none")
