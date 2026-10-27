"""Volume/brightness OSD (org.gnome.Shell.ShowOSD) and media-key grabs
(org.gnome.Shell.GrabAccelerator) as used by gnome-settings-daemon."""
import sys
import threading
import time

sys.path.insert(0, "/src/dev")
from driver import Session  # noqa: E402

from gi.repository import Gio, GLib  # noqa: E402

XF86AudioRaiseVolume = 0x1008FF13

s = Session(sys.argv[1])
bus = s.bus
failures = []


def check(ok, what):
    print(("PASS " if ok else "FAIL ") + what, flush=True)
    if not ok:
        failures.append(what)


def show_osd(params):
    bus.call_sync("org.gnome.Shell", "/org/gnome/Shell", "org.gnome.Shell", "ShowOSD",
                  GLib.Variant("(a{sv})", (params,)), None, Gio.DBusCallFlags.NONE, -1, None)


def shot_while_showing(name, params):
    """The OSD hides after 1.5s and a screenshot can take longer, so keep
    re-requesting it (like holding a volume key) while the shot is taken."""
    stop = threading.Event()

    def keep_showing():
        while not stop.is_set():
            show_osd(params)
            time.sleep(0.3)

    thread = threading.Thread(target=keep_showing, daemon=True)
    thread.start()
    time.sleep(0.6)
    s.shot(name)
    stop.set()
    thread.join()


shot_while_showing("01-volume", {
    "icon": GLib.Variant("s", "audio-volume-medium-symbolic"),
    "level": GLib.Variant("d", 0.55), "max_level": GLib.Variant("d", 1.0)})
shot_while_showing("02-overamplified", {
    "icon": GLib.Variant("s", "audio-volume-overamplified-symbolic"),
    "level": GLib.Variant("d", 1.3), "max_level": GLib.Variant("d", 1.5)})
shot_while_showing("03-brightness-label", {
    "icon": GLib.Variant("s", "display-brightness-symbolic"),
    "label": GLib.Variant("s", "Built-in display"), "level": GLib.Variant("d", 0.8)})
shot_while_showing("04-icon-only", {
    "icon": GLib.Variant("s", "airplane-mode-symbolic"),
    "label": GLib.Variant("s", "Airplane Mode On")})

time.sleep(2.5)
s.shot("05-hidden")

# Media keys: grab like gnome-settings-daemon does and press the key.
grabber = Gio.bus_get_sync(Gio.BusType.SESSION)
reply = grabber.call_sync("org.gnome.Shell", "/org/gnome/Shell", "org.gnome.Shell",
                          "GrabAccelerators",
                          GLib.Variant("(a(suu))", ([("XF86AudioRaiseVolume", 0, 0)],)),
                          GLib.VariantType("(au)"), Gio.DBusCallFlags.NONE, -1, None)
action = reply[0][0]
check(action > 0, f"GrabAccelerators returned an action ({action})")

activated = []
loop = GLib.MainLoop()
grabber.signal_subscribe("org.gnome.Shell", "org.gnome.Shell", "AcceleratorActivated",
                         "/org/gnome/Shell", None, Gio.DBusSignalFlags.NONE,
                         lambda *a: (activated.append(a[5].unpack()), loop.quit()))
s.key(XF86AudioRaiseVolume)
GLib.timeout_add_seconds(3, loop.quit)
loop.run()
check(bool(activated) and activated[0][0] == action, f"AcceleratorActivated for the key ({activated})")

ok = grabber.call_sync("org.gnome.Shell", "/org/gnome/Shell", "org.gnome.Shell",
                       "UngrabAccelerators", GLib.Variant("(au)", ([action],)),
                       GLib.VariantType("(b)"), Gio.DBusCallFlags.NONE, -1, None)[0]
check(ok, "UngrabAccelerators succeeded")

print("FAILURES:", failures or "none")
