"""Bluetooth quick toggle, its device menu and the panel indicator."""
import sys

sys.path.insert(0, "/src/dev")
from driver import Session  # noqa: E402

import gi  # noqa: E402

gi.require_version("Gio", "2.0")
from gi.repository import Gio, GLib  # noqa: E402

MOCK = "org.freedesktop.DBus.Mock"
ADAPTER = "/org/bluez/hci0"
DEVICES = {name: f"{ADAPTER}/dev_11_22_33_44_55_0{n}"
           for n, name in enumerate(["WH-1000XM5", "MX Master 3S", "Pixel 9"], 1)}

system = Gio.bus_get_sync(Gio.BusType.SYSTEM)


def bluez(path, iface, method, args=None):
    system.call_sync("org.bluez", path, iface, method, args, None,
                     Gio.DBusCallFlags.NONE, 10000, None)


def set_connect_methods(path, delay=0):
    """The bluez5 template tracks Connected outside the device's properties,
    so GetManagedObjects never sees a change. Replace Connect/Disconnect with
    versions that update the real property (and emit PropertiesChanged)."""
    for method, value in (("Connect", True), ("Disconnect", False)):
        code = (f"import dbus, time; time.sleep({delay}); "
                f"self.UpdateProperties('org.bluez.Device1', {{'Connected': dbus.Boolean({value})}})")
        bluez(path, MOCK, "AddMethod",
              GLib.Variant("(sssss)", ("org.bluez.Device1", method, "", "", code)))


for path in DEVICES.values():
    set_connect_methods(path)

s = Session(sys.argv[1])

# Headphones connected: indicator in the panel, alias as toggle subtitle.
s.panel("Activate", "qs-button")
s.shot("01-menu")
s.panel("Activate", "qs-bluetooth-arrow")
s.shot("02-devices")

# Pixel 9 (third item) connects: "2 Connected", both checked, sorted first.
s.panel("Activate", "qs-bt-device-2")
s.shot("03-two-connected")

# A slow Connect shows its progress while the call is pending. Make the
# mouse's Connect take a few seconds (this blocks the mock meanwhile).
set_connect_methods(DEVICES["MX Master 3S"], delay=8)
s.panel("Activate", "qs-bt-device-2", settle=0.3)  # order: Pixel, WH, MX
# (The item spinner keeps frames coming, so the shot lands mid-call.)
s.shot("04-connecting")
s.panel("ListNames", settle=8)  # let the call finish
s.shot("04b-three-connected")

# Disconnect all: no subtitle, indicator hidden. Order is MX, Pixel, WH.
set_connect_methods(DEVICES["MX Master 3S"])
for _ in range(3):
    s.panel("Activate", "qs-bt-device-0")
s.shot("05-none-connected")

# Power off: disabled icon, unchecked, menu says "Off".
s.panel("Activate", "qs-bluetooth")
s.shot("06-off")

# Power back on, reconnect the headphones from outside: indicator returns.
s.panel("Activate", "qs-bluetooth")
bluez(DEVICES["WH-1000XM5"], "org.bluez.Device1", "Connect")
s.panel("ListNames", settle=0.6)
s.shot("07-on-again")

# Forget all devices: empty state.
for path in DEVICES.values():
    bluez(ADAPTER, "org.bluez.Adapter1", "RemoveDevice", GLib.Variant("(o)", (path,)))
s.panel("ListNames", settle=0.6)
s.shot("08-no-devices")

s.panel("CloseMenus")
s.shot("09-closed")
