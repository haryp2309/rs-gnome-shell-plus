"""Fake system services for the headless test session, using python-dbusmock.

Gives the panel something realistic to show: a Wi-Fi card with a few access
points (one connected), an unplugged Ethernet port, NetworkManager's secret
agent manager (mock_networkmanager.py), a Bluetooth adapter with paired
devices, a discharging battery, power profiles, airplane mode and polkitd's
Authority with authentication agent registration (mock_polkit.py).

Runs until killed; prints "mocks ready" once everything is populated.
"""
import os
import time

import dbus
import dbusmock
from dbusmock import BusType, SpawnedMock

NM = "org.freedesktop.NetworkManager"
NM_PATH = "/org/freedesktop/NetworkManager"
BLUEZ_DEVICE = "org.bluez.Device1"
PSK = 0x100  # NM_802_11_AP_SEC_KEY_MGMT_PSK


def spawn(template, bus=BusType.SYSTEM, parameters=None):
    return SpawnedMock.spawn_with_template(template, parameters or {}, bustype=bus)


def setup_network():
    # dbusmock's NetworkManager plus a secret-agent manager.
    nm = spawn(os.path.join(os.path.dirname(os.path.abspath(__file__)), "mock_networkmanager.py"))
    mock = nm.obj

    wifi = mock.AddWiFiDevice("mock_WiFi", "wlan0", 100)  # 100 = activated
    # (name, ssid, bssid, mode=infra, freq, rate, strength, security)
    home = mock.AddAccessPoint(wifi, "ap_home", "Home Network", "AA:BB:CC:00:00:01", 2, 5180, 54000, 82, PSK)
    mock.AddAccessPoint(wifi, "ap_cafe", "Café Guest", "AA:BB:CC:00:00:02", 2, 2437, 54000, 61, 0)
    mock.AddAccessPoint(wifi, "ap_office", "Office", "AA:BB:CC:00:00:03", 2, 2412, 54000, 44, PSK)
    mock.AddAccessPoint(wifi, "ap_neighbour", "Neighbour 5G", "AA:BB:CC:00:00:04", 2, 5745, 54000, 23, PSK)

    conn = mock.AddWiFiConnection(wifi, "conn_home", "Home Network", "wpa-psk")
    mock.AddActiveConnection([wifi], conn, home, "active_home", 2)  # 2 = activated
    mock.AddEthernetDevice("mock_Ethernet", "eth0", 30)  # 30 = disconnected

    mock.SetGlobalConnectionState(70)
    mock.SetConnectivity(4)
    # The template only answers GetAllAccessPoints and leaves the
    # AccessPoints property (which the panel, like libnm, reads) empty.
    wifi_obj = dbus.SystemBus().get_object(NM, wifi)
    aps = wifi_obj.GetAllAccessPoints(dbus_interface=NM + ".Device.Wireless")
    wifi_obj.UpdateProperties(NM + ".Device.Wireless",
                              {"AccessPoints": dbus.Array(aps, signature="o")},
                              dbus_interface=dbusmock.MOCK_IFACE)

    # The template doesn't define PrimaryConnectionType; add it.
    dbus.SystemBus().get_object(NM, NM_PATH).AddProperty(
        NM, "PrimaryConnectionType", dbus.String("802-11-wireless"),
        dbus_interface=dbusmock.MOCK_IFACE)
    return nm


def setup_bluetooth():
    bluez = spawn("bluez5")
    bluez.obj.AddAdapter("hci0", "my-computer")
    bus = dbus.SystemBus()
    for address, alias, icon, connected in [
        ("11:22:33:44:55:01", "WH-1000XM5", "audio-headphones", True),
        ("11:22:33:44:55:02", "MX Master 3S", "input-mouse", False),
        ("11:22:33:44:55:03", "Pixel 9", "phone", False),
    ]:
        path = bluez.obj.AddDevice("hci0", address, alias)
        device = bus.get_object("org.bluez", path)
        device.UpdateProperties(BLUEZ_DEVICE, {
            "Paired": True,
            "Trusted": True,
            "Connected": connected,
            "Icon": icon,
        }, dbus_interface=dbusmock.MOCK_IFACE)
        # The template's Connect/Disconnect don't update the stored
        # Connected property that GetManagedObjects returns.
        for method, value in (("Connect", True), ("Disconnect", False)):
            device.AddMethod(BLUEZ_DEVICE, method, "", "",
                             "self.UpdateProperties('org.bluez.Device1', "
                             f"{{'Connected': dbus.Boolean({value})}})",
                             dbus_interface=dbusmock.MOCK_IFACE)
    return bluez


def setup_polkit():
    return spawn(os.path.join(os.path.dirname(os.path.abspath(__file__)), "mock_polkit.py"))


def setup_power():
    upower = spawn("upower", parameters={"DaemonVersion": "1.90", "OnBattery": True})
    # type=battery, state=discharging, 76 %, ..., is_present, icon, warning level
    upower.obj.SetupDisplayDevice(2, 2, 76.0, 38.0, 50.0, 9.5, 14400, 0, True,
                                  "battery-good-symbolic", 1)
    profiles = spawn("upower_power_profiles_daemon")
    return upower, profiles


def setup_rfkill():
    rfkill = spawn("gsd_rfkill", bus=BusType.SESSION)
    rfkill.obj.UpdateProperties("org.gnome.SettingsDaemon.Rfkill", {
        "HasAirplaneMode": True,
        "ShouldShowAirplaneMode": True,
        "AirplaneMode": False,
    }, dbus_interface=dbusmock.MOCK_IFACE)
    return rfkill


def main():
    mocks = [setup_network(), setup_bluetooth(), *setup_power(), setup_rfkill(), setup_polkit()]
    print("mocks ready", flush=True)
    try:
        while True:
            time.sleep(3600)
    finally:
        for m in mocks:
            m.terminate()


if __name__ == "__main__":
    main()
