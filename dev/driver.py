"""Helpers for scenario scripts running inside dev/session.sh.

    from driver import Session
    s = Session(out_dir)
    s.panel("Activate", "qs-button")   # open the quick settings menu
    s.shot("quick-settings")           # -> out_dir/quick-settings.png

The panel's debug interface (enabled by SHELLPLUS_PANEL_DEBUG=1) finds
widgets by their GTK widget name; see panel/src/debug.rs.
"""
import glob
import os
import shutil
import subprocess
import sys
import tempfile
import time

import gi

gi.require_version("Gio", "2.0")
from gi.repository import Gio, GLib  # noqa: E402

SCREENCAST = "org.gnome.Mutter.ScreenCast"


class Session:
    def __init__(self, out_dir):
        self.out_dir = out_dir
        self.bus = Gio.bus_get_sync(Gio.BusType.SESSION)
        self.apps = []

    # --- compositor and panel -------------------------------------------

    def _call(self, name, path, iface, method, args=None, reply=None):
        return self.bus.call_sync(name, path, iface, method, args,
                                  GLib.VariantType(reply) if reply else None,
                                  Gio.DBusCallFlags.NONE, 10000, None)

    def state(self):
        return self._call("org.gnome.ShellPlus", "/org/gnome/ShellPlus",
                          "org.gnome.ShellPlus.Panel", "GetState",
                          reply="(a{sv})")[0]

    def panel(self, method, *args, settle=0.6):
        """Calls a PanelDebug method, e.g. panel("Activate", "qs-wifi-arrow")."""
        signature = "".join("d" if isinstance(a, float) else "s" for a in args)
        params = GLib.Variant(f"({signature})", args) if args else None
        result = self._call("org.gnome.ShellPlus.PanelDebug",
                            "/org/gnome/ShellPlus/PanelDebug",
                            "org.gnome.ShellPlus.PanelDebug", method, params)
        time.sleep(settle)
        return result.unpack() if result else None

    def _remote_desktop(self):
        """Lazily starts a Mutter RemoteDesktop session for input injection."""
        if getattr(self, "_rd", None):
            return self._rd
        rd_name = "org.gnome.Mutter.RemoteDesktop"
        rd = self._call(rd_name, "/org/gnome/Mutter/RemoteDesktop", rd_name,
                        "CreateSession", reply="(o)")[0]
        rd_id = self._call(rd_name, rd, "org.freedesktop.DBus.Properties", "Get",
                           GLib.Variant("(ss)", (rd_name + ".Session", "SessionId")),
                           "(v)")[0]
        sc = self._call(SCREENCAST, "/org/gnome/Mutter/ScreenCast", SCREENCAST,
                        "CreateSession",
                        GLib.Variant("(a{sv})", ({"remote-desktop-session-id":
                                                  GLib.Variant("s", rd_id)},)), "(o)")[0]
        stream = self._call(SCREENCAST, sc, SCREENCAST + ".Session", "RecordMonitor",
                            GLib.Variant("(sa{sv})", ("", {})), "(o)")[0]
        self._call(rd_name, rd, rd_name + ".Session", "Start")
        # The virtual pointer needs a moment, and an initial event, before
        # clicks land reliably; otherwise the first click can be lost.
        time.sleep(1.0)
        self._call(rd_name, rd, rd_name + ".Session", "NotifyPointerMotionRelative",
                   GLib.Variant("(dd)", (1.0, 1.0)))
        time.sleep(0.5)
        self._rd = (rd_name, rd, stream)
        return self._rd

    def click(self, x, y, button=0x110, settle=0.8):
        """Real pointer click at stage coordinates (BTN_LEFT by default)."""
        rd_name, rd, stream = self._remote_desktop()
        iface = rd_name + ".Session"
        # Relative motion works without a consumer on the screen-cast stream:
        # pin the pointer to the top-left corner, then move to the target.
        self._call(rd_name, rd, iface, "NotifyPointerMotionRelative",
                   GLib.Variant("(dd)", (-10000.0, -10000.0)))
        self._call(rd_name, rd, iface, "NotifyPointerMotionRelative",
                   GLib.Variant("(dd)", (float(x), float(y))))
        time.sleep(0.1)
        for pressed in (True, False):
            self._call(rd_name, rd, iface, "NotifyPointerButton",
                       GLib.Variant("(ib)", (button, pressed)))
            time.sleep(0.05)
        time.sleep(settle)

    def key(self, keysym, settle=0.5):
        """Presses and releases a key by X keysym (e.g. 0xff1b for Escape)."""
        rd_name, rd, _ = self._remote_desktop()
        for pressed in (True, False):
            self._call(rd_name, rd, rd_name + ".Session", "NotifyKeyboardKeysym",
                       GLib.Variant("(ub)", (keysym, pressed)))
        time.sleep(settle)

    def gsettings(self, schema, key, value):
        subprocess.run(["gsettings", "set", schema, key, value], check=True)
        time.sleep(0.6)

    def open_maximized_window(self, settle=3):
        app = subprocess.Popen([sys.executable, os.path.join(os.path.dirname(__file__),
                                                             "maximized-window.py")],
                               stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        self.apps.append(app)
        time.sleep(settle)
        return app

    # --- screenshots ----------------------------------------------------

    def shot(self, name):
        """Saves one frame of the primary monitor as <out_dir>/<name>.png."""
        path = os.path.join(self.out_dir, f"{name}.png")
        session = self._call(SCREENCAST, "/org/gnome/Mutter/ScreenCast", SCREENCAST,
                             "CreateSession", GLib.Variant("(a{sv})", ({},)), "(o)")[0]
        stream = self._call(SCREENCAST, session, SCREENCAST + ".Session", "RecordMonitor",
                            GLib.Variant("(sa{sv})", ("", {})), "(o)")[0]

        loop = GLib.MainLoop()
        node = {}

        def on_added(_c, _s, _p, _i, _n, params):
            node["id"] = params[0]
            loop.quit()

        sub = self.bus.signal_subscribe(SCREENCAST, SCREENCAST + ".Stream",
                                        "PipeWireStreamAdded", stream, None,
                                        Gio.DBusSignalFlags.NONE, on_added)
        self._call(SCREENCAST, session, SCREENCAST + ".Session", "Start")
        GLib.timeout_add_seconds(10, loop.quit)
        loop.run()
        self.bus.signal_unsubscribe(sub)

        tmp = tempfile.mkdtemp()
        if "id" in node:
            # Keep the last of a few frames; the first can be incomplete.
            subprocess.run(["timeout", "10", "gst-launch-1.0", "-q",
                            "pipewiresrc", f"path={node['id']}", "num-buffers=3", "!",
                            "videoconvert", "!", "pngenc", "snapshot=false", "!",
                            "multifilesink", f"location={tmp}/frame%d.png"], check=False)
        self._call(SCREENCAST, session, SCREENCAST + ".Session", "Stop")

        frames = sorted(glob.glob(f"{tmp}/frame*.png"))
        if not frames:
            raise RuntimeError(f"no frames captured for {name}")
        shutil.move(frames[-1], path)
        print("saved", path, flush=True)
        return path


def wait_ready(timeout=20):
    """Blocks until the compositor and the panel's debug interface respond."""
    s = Session(None)
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            s.state()
            s.panel("ListNames", settle=0)
            time.sleep(2)  # let the first frame and backdrop render
            return
        except GLib.Error:
            time.sleep(0.2)
    raise SystemExit("compositor/panel did not come up; see compositor.log")


if __name__ == "__main__" and sys.argv[1:] == ["wait-ready"]:
    wait_ready()
