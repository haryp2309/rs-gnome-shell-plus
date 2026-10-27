"""Grab one frame of the primary monitor via Mutter's ScreenCast D-Bus API.

Usage: screenshot.py OUTPUT.png  (needs pipewire + wireplumber running)
"""
import glob
import shutil
import subprocess
import sys
import tempfile

import gi

gi.require_version("Gio", "2.0")
from gi.repository import Gio, GLib  # noqa: E402

NAME = "org.gnome.Mutter.ScreenCast"
bus = Gio.bus_get_sync(Gio.BusType.SESSION)


def call(path, iface, method, args, reply=None):
    return bus.call_sync(NAME, path, iface, method, args,
                         GLib.VariantType(reply) if reply else None,
                         Gio.DBusCallFlags.NONE, -1, None)


session = call("/org/gnome/Mutter/ScreenCast", NAME, "CreateSession",
               GLib.Variant("(a{sv})", ({},)), "(o)")[0]
stream = call(session, NAME + ".Session", "RecordMonitor",
              GLib.Variant("(sa{sv})", ("", {})), "(o)")[0]

loop = GLib.MainLoop()
node = {}


def on_stream_added(_conn, _sender, _path, _iface, _signal, params):
    node["id"] = params[0]
    loop.quit()


bus.signal_subscribe(NAME, NAME + ".Stream", "PipeWireStreamAdded", stream,
                     None, Gio.DBusSignalFlags.NONE, on_stream_added)
call(session, NAME + ".Session", "Start", None)
GLib.timeout_add_seconds(10, loop.quit)
loop.run()

if "id" not in node:
    sys.exit("ScreenCast stream never appeared")

tmp = tempfile.mkdtemp()
# Grab a few frames and keep the last one; the first can be incomplete.
subprocess.run(["timeout", "10", "gst-launch-1.0", "-q",
                "pipewiresrc", f"path={node['id']}", "num-buffers=3", "!",
                "videoconvert", "!", "pngenc", "snapshot=false", "!",
                "multifilesink", f"location={tmp}/frame%d.png"],
               check=False)
frames = sorted(glob.glob(f"{tmp}/frame*.png"))
call(session, NAME + ".Session", "Stop", None)

if not frames:
    sys.exit("No frames captured")
shutil.move(frames[-1], sys.argv[1])
print("saved", sys.argv[1])
