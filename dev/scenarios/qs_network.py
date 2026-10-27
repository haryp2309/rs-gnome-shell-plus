"""Network section of quick settings: Wi-Fi menu, connecting, Wi-Fi off,
and the Wired toggle, against the mocked NetworkManager (dev/mocks.py)."""
import sys
import time

import dbus
import dbusmock

sys.path.insert(0, "/src/dev")
from driver import Session  # noqa: E402

NM = "org.freedesktop.NetworkManager"
NM_PATH = "/org/freedesktop/NetworkManager"
DEVICE = NM + ".Device"
WIFI = NM_PATH + "/Devices/mock_WiFi"
ETH = NM_PATH + "/Devices/mock_Ethernet"

bus = dbus.SystemBus()


def obj(path):
    return bus.get_object(NM, path)


def mock(path):
    return dbus.Interface(obj(path), dbusmock.MOCK_IFACE)


def prop(path, iface, name):
    return obj(path).Get(iface, name, dbus_interface=dbus.PROPERTIES_IFACE)


# The template keeps access points out of the device's AccessPoints property
# (it only answers GetAllAccessPoints); fill it in like real NetworkManager.
aps = obj(WIFI).GetAllAccessPoints(dbus_interface=NM + ".Device.Wireless")
mock(WIFI).UpdateProperties(NM + ".Device.Wireless",
                            {"AccessPoints": dbus.Array(aps, signature="o")})
# A duplicate (weaker) "Office" AP and a hidden one: both must not add rows.
mock(NM_PATH).AddAccessPoint(WIFI, "ap_office2", "Office", "AA:BB:CC:00:00:05", 2, 5180, 54000, 30, 0x100)
mock(NM_PATH).AddAccessPoint(WIFI, "ap_hidden", "", "AA:BB:CC:00:00:06", 2, 5180, 54000, 90, 0x100)
aps = obj(WIFI).GetAllAccessPoints(dbus_interface=NM + ".Device.Wireless")
mock(WIFI).UpdateProperties(NM + ".Device.Wireless",
                            {"AccessPoints": dbus.Array(aps, signature="o")})

# Wired: a saved profile, ActivateConnection("/") picking it like NM does,
# and Device.Disconnect (neither exists in the template).
wired_conn = dbus.Interface(obj(NM_PATH + "/Settings"), NM + ".Settings").AddConnection(dbus.Dictionary({
    "connection": dbus.Dictionary({"id": "Wired connection 1", "type": "802-3-ethernet",
                                   "autoconnect": dbus.Boolean(False)}, signature="sv"),
}, signature="sa{sv}"))
mock(ETH).UpdateProperties(DEVICE, {"AvailableConnections": dbus.Array([wired_conn], signature="o")})
mock(NM_PATH).AddMethod(NM, "ActivateConnection", "ooo", "o", """
conn = args[0]
if conn == '/':
    conn = objects[args[1]].Get('org.freedesktop.NetworkManager.Device', 'AvailableConnections')[0]
ret = self.activate_connection(self, conn, args[1], args[2])
""")
mock(ETH).AddMethod(DEVICE, "Disconnect", "", "", """
nm = objects['/org/freedesktop/NetworkManager']
active = self.Get('org.freedesktop.NetworkManager.Device', 'ActiveConnection')
nm.RemoveActiveConnection(self.__dbus_object_path__, active)
""")

s = Session(sys.argv[1])

s.panel("Activate", "qs-button")
s.shot("01-menu")
s.panel("Activate", "qs-wifi-arrow")
s.shot("02-wifi-menu")
print("RequestScan calls:", len(mock(WIFI).GetMethodCalls("RequestScan")))

# Connect to the open "Café Guest" network (second row): AddAndActivate.
s.panel("Activate", "qs-wifi-net-1")
time.sleep(0.5)
active = prop(WIFI, DEVICE, "ActiveConnection")
print("wifi active connection:", active,
      "->", prop(active, NM + ".Connection.Active", "Id"))
s.shot("03-wifi-cafe")

# Back to the saved "Home Network": ActivateConnection with the saved profile.
s.panel("Activate", "qs-wifi-net-1")
time.sleep(0.5)
active = prop(WIFI, DEVICE, "ActiveConnection")
print("wifi active connection:", active,
      "->", prop(active, NM + ".Connection.Active", "Id"))
s.shot("04-wifi-home")

# Wi-Fi off: toggle unchecked, list emptied, panel icon offline.
s.panel("Activate", "qs-wifi")
print("WirelessEnabled:", prop(NM_PATH, NM, "WirelessEnabled"))
s.shot("05-wifi-off")
s.panel("CloseMenus")
s.shot("06-panel-wifi-off")

# Wired: plug in (connect via the toggle) and make it primary.
s.panel("Activate", "qs-button")
s.panel("Activate", "qs-wired")
print("eth0 state:", prop(ETH, DEVICE, "State"))
mock(NM_PATH).SetProperty(NM_PATH, NM, "PrimaryConnectionType", dbus.String("802-3-ethernet"))
time.sleep(0.5)
s.shot("07-wired-on")
s.panel("CloseMenus")
s.shot("08-panel-wired")

# Wired off again, Wi-Fi back on.
s.panel("Activate", "qs-button")
s.panel("Activate", "qs-wired")
print("eth0 state:", prop(ETH, DEVICE, "State"))
mock(NM_PATH).SetProperty(NM_PATH, NM, "PrimaryConnectionType", dbus.String("802-11-wireless"))
s.panel("Activate", "qs-wifi")
print("WirelessEnabled:", prop(NM_PATH, NM, "WirelessEnabled"))
time.sleep(0.5)
s.shot("09-wired-off-wifi-on")
