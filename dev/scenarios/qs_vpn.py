"""VPN and Hotspot quick settings, against the mocked NetworkManager
(dev/mocks.py has an OpenVPN and a WireGuard profile, WireGuard used most
recently, and a Wi-Fi device that can be an access point;
dev/mock_networkmanager.py brings VPNs up through ACTIVATING and stores
hotspot settings as given). Also checks the secret agent's VPN password
prompt."""
import json
import sys
import time

import dbus
import dbusmock

sys.path.insert(0, "/src/dev")
from driver import Session  # noqa: E402

NM = "org.freedesktop.NetworkManager"
NM_PATH = "/org/freedesktop/NetworkManager"
ACTIVE = NM + ".Connection.Active"
DEVICE = NM + ".Device"
WIRELESS = NM + ".Device.Wireless"
WIFI = NM_PATH + "/Devices/mock_WiFi"

bus = dbus.SystemBus()
s = Session(sys.argv[1])
failures = []


def check(ok, what):
    print(("PASS " if ok else "FAIL ") + what, flush=True)
    if not ok:
        failures.append(what)


def wait_for(predicate, timeout=5.0):
    deadline = time.time() + timeout
    while time.time() < deadline:
        if predicate():
            return True
        time.sleep(0.1)
    return predicate()


def obj(path):
    return bus.get_object(NM, path)


def prop(path, iface, name):
    return obj(path).Get(iface, name, dbus_interface=dbus.PROPERTIES_IFACE)


def mock(path):
    return dbus.Interface(obj(path), dbusmock.MOCK_IFACE)


def active_connections():
    """[(Id, Type, State)] of NetworkManager's active connections."""
    return [(str(prop(p, ACTIVE, "Id")), str(prop(p, ACTIVE, "Type")), int(prop(p, ACTIVE, "State")))
            for p in prop(NM_PATH, NM, "ActiveConnections")]


def active_vpns():
    return [(i, st) for i, t, st in active_connections() if t in ("vpn", "wireguard")]


def inspect(name):
    return s.inspect(name)


def shown(name):
    state = inspect(name)
    return bool(state and state["mapped"])


def text(name):
    state = inspect(name)
    return str(state["text"]) if state and state["visible"] else None


# --- VPN -----------------------------------------------------------------

s.panel("Activate", "qs-button")
s.shot("01-menu")
check(shown("qs-vpn"), "VPN toggle shown")
check(shown("qs-hotspot"), "Hotspot toggle shown")
check(text("qs-vpn-subtitle") is None, "VPN subtitle empty while off")
check(not shown("vpn-indicator"), "no VPN panel icon while off")

s.panel("Activate", "qs-vpn-arrow")
labels = [text(f"qs-vpn-item-{i}-label") for i in range(3)]
print("VPN menu:", labels)
check(labels == ["Home WireGuard", "Work VPN", None], "VPN menu lists both profiles")
check(shown("qs-vpn-settings"), "VPN Settings item")
s.shot("02-vpn-menu")

# On: the most recently used profile (WireGuard).
s.panel("Activate", "qs-vpn", settle=0)
check(wait_for(lambda: active_vpns() == [("Home WireGuard", 2)]), "toggle on activates Home WireGuard")
print("active:", active_connections())
check(wait_for(lambda: text("qs-vpn-subtitle") == "Home WireGuard"), "subtitle names the active VPN")
check(wait_for(lambda: shown("vpn-indicator")), "VPN panel icon shown")
s.shot("03-vpn-on")

# Off: deactivates.
s.panel("Activate", "qs-vpn")
check(wait_for(lambda: active_vpns() == []), "toggle off deactivates")
check(wait_for(lambda: text("qs-vpn-subtitle") is None), "subtitle cleared")
check(wait_for(lambda: not shown("vpn-indicator")), "VPN panel icon hidden")
check(prop(WIFI, DEVICE, "State") == 100, "Wi-Fi stays connected")

# On again, then pick the other VPN in the menu: switches over.
s.panel("Activate", "qs-vpn")
wait_for(lambda: active_vpns() == [("Home WireGuard", 2)])
s.panel("Activate", "qs-vpn-item-1")
check(wait_for(lambda: active_vpns() == [("Work VPN", 2)]), "picking Work VPN switches to it")
check(wait_for(lambda: text("qs-vpn-subtitle") == "Work VPN"), "subtitle follows the switch")
s.shot("04-vpn-switched")
s.panel("CloseMenus")
s.shot("05-panel-vpn")

# Picking the active one switches it off.
s.panel("Activate", "qs-button")
s.panel("Activate", "qs-vpn-arrow")
s.panel("Activate", "qs-vpn-item-1")
check(wait_for(lambda: active_vpns() == []), "picking the active VPN switches it off")
s.panel("Activate", "qs-vpn-arrow")  # close

# --- Hotspot -------------------------------------------------------------

nm = mock(NM_PATH)
nm.ClearCalls()
s.panel("Activate", "qs-hotspot")
check(wait_for(lambda: len(nm.GetMethodCalls("AddAndActivateConnection")) == 1),
      "hotspot on calls AddAndActivateConnection")
calls = nm.GetMethodCalls("AddAndActivateConnection")
settings, device, ap = calls[0][1] if calls else ({}, "", "")
plain = {k: {kk: (bytes(vv).decode() if isinstance(vv, bytes) or (isinstance(vv, dbus.Array) and vv.signature == "y") else
                  [str(x) for x in vv] if isinstance(vv, dbus.Array) else str(vv))
             for kk, vv in v.items()} for k, v in settings.items()}
print("hotspot settings:", json.dumps(plain, indent=1))
wireless = plain.get("802-11-wireless", {})
security = plain.get("802-11-wireless-security", {})
ssid = wireless.get("ssid", "")
check(str(device) == WIFI and str(ap) == "/", "activated on the Wi-Fi device")
check(wireless.get("mode") == "ap" and len(ssid) > 0, f"AP mode with SSID {ssid!r}")
check(security.get("key-mgmt") == "wpa-psk" and len(security.get("psk", "")) == 8, "WPA2 PSK with a password")
check(plain.get("ipv4", {}).get("method") == "shared", "ipv4 shared")
check(plain.get("connection", {}).get("id") == "Hotspot", "profile named Hotspot")
check(wait_for(lambda: ("Hotspot", "802-11-wireless", 2) in active_connections()), "hotspot active")
check(wait_for(lambda: text("qs-hotspot-subtitle") == ssid), "Hotspot subtitle shows the SSID")
check(wait_for(lambda: text("qs-wifi-subtitle") == "Hotspot Active"), "Wi-Fi toggle shows the hotspot")
s.shot("06-hotspot-on")
s.panel("Activate", "qs-wifi-arrow")
s.shot("07-wifi-menu-hotspot")
s.panel("Activate", "qs-wifi-arrow")

s.panel("Activate", "qs-hotspot")
check(wait_for(lambda: all(i != "Hotspot" for i, _, _ in active_connections())), "hotspot off deactivates")
check(wait_for(lambda: text("qs-hotspot-subtitle") is None), "Hotspot subtitle cleared")

# Again: reuses the saved profile.
nm.ClearCalls()
s.panel("Activate", "qs-hotspot")
check(wait_for(lambda: ("Hotspot", "802-11-wireless", 2) in active_connections()), "hotspot on again")
check(len(nm.GetMethodCalls("AddAndActivateConnection")) == 0
      and len(nm.GetMethodCalls("ActivateConnection")) == 1, "second time activates the saved profile")
s.panel("Activate", "qs-hotspot")
check(wait_for(lambda: all(i != "Hotspot" for i, _, _ in active_connections())), "hotspot off again")

# Without AP support there's no toggle.
mock(WIFI).UpdateProperties(WIRELESS, {"WirelessCapabilities": dbus.UInt32(0x3f)})
check(wait_for(lambda: not shown("qs-hotspot")), "no Hotspot toggle without AP support")
mock(WIFI).UpdateProperties(WIRELESS, {"WirelessCapabilities": dbus.UInt32(0xff)})
check(wait_for(lambda: shown("qs-hotspot")), "Hotspot toggle back with AP support")
s.panel("CloseMenus")

# --- VPN password prompt (secret agent) ----------------------------------

manager = dbus.Interface(obj(NM_PATH + "/AgentManager"), "org.gnome.ShellPlus.MockAgentManager")
connection = dbus.Dictionary({
    "connection": dbus.Dictionary({"id": "Work VPN", "type": "vpn"}, signature="sv"),
    "vpn": dbus.Dictionary({"service-type": "org.freedesktop.NetworkManager.openvpn",
                            "data": dbus.Dictionary({"username": "alice"}, signature="ss")},
                           signature="sv"),
}, signature="sa{sv}")
request = manager.RequestSecrets(connection, dbus.ObjectPath(NM_PATH + "/Settings/vpn"), "vpn",
                                 dbus.Array(["password", "x-vpn-message:Enter your password"], signature="s"),
                                 dbus.UInt32(1))
check(wait_for(lambda: shown("network-secrets")), "VPN password dialog shown")
s.shot("08-vpn-password")
s.panel("SetText", "network-secrets-entry", "correct horse")
s.panel("Activate", "network-secrets-connect")
reply = {}
wait_for(lambda: bool(reply.update(json.loads(manager.GetReply(request) or "{}")) or reply))
print("reply:", reply)
check(reply.get("secrets") == {"vpn": {"secrets": {"password": "correct horse"}}}, "VPN password returned")

print("FAILURES:", ", ".join(failures) if failures else "none")
