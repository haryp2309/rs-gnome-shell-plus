"""Power and system section of quick settings: top row, power submenu with
its confirmation dialog, Power Mode toggle, battery indicator and the
(hidden, no backlight) brightness slider."""
import subprocess
import sys
import time

import dbus
import dbusmock
from dbusmock import BusType, SpawnedMock

sys.path.insert(0, "/src/dev")
from driver import Session  # noqa: E402

PROFILES = "org.freedesktop.UPower.PowerProfiles"


def check(cond, what):
    print(("ok  " if cond else "FAIL"), what, flush=True)
    if not cond:
        failures.append(what)


failures = []
s = Session(sys.argv[1])
system = dbus.SystemBus()

# logind and GDM, so the power submenu has something to talk to.
logind = SpawnedMock.spawn_with_template("logind", {}, bustype=BusType.SYSTEM)
gdm = SpawnedMock.spawn_for_name("org.gnome.DisplayManager",
                                 "/org/gnome/DisplayManager/LocalDisplayFactory",
                                 "org.gnome.DisplayManager.LocalDisplayFactory",
                                 BusType.SYSTEM)
gdm.obj.AddMethod("org.gnome.DisplayManager.LocalDisplayFactory",
                  "CreateTransientDisplay", "", "o", 'ret = "/org/gnome/DisplayManager/Display1"',
                  dbus_interface=dbusmock.MOCK_IFACE)
time.sleep(0.5)


def names():
    return s.panel("ListNames", settle=0)[0]


s.panel("Activate", "qs-button")
s.shot("01-menu")
check("qs-battery" in names(), "battery button exists")
check("qs-brightness" in names(), "brightness slider built (hidden without backlight)")
check(dict(s.state()).get("brightness", -1) < 0, "no backlight in the container")

# Power submenu.
s.panel("Activate", "qs-power")
s.shot("02-power-menu")

s.panel("Activate", "qs-suspend")
calls = [c[1] for c in logind.obj.GetCalls(dbus_interface=dbusmock.MOCK_IFACE)]
check("Suspend" in calls, "Suspend called on logind")

s.panel("Activate", "qs-button")
s.panel("Activate", "qs-power")
s.panel("Activate", "qs-restart", settle=1.0)
check("qs-confirm-restart" in names(), "restart confirmation dialog shown")
s.shot("03-restart-dialog")
s.click(556, 468)  # Cancel
check("qs-confirm-restart" not in names(), "Cancel dismisses the dialog")
calls = [c[1] for c in logind.obj.GetCalls(dbus_interface=dbusmock.MOCK_IFACE)]
check("Reboot" not in calls, "cancelled restart did not reboot")

# Confirming Power Off reaches (mocked) logind.
s.panel("Activate", "qs-button")
s.panel("Activate", "qs-power")
s.panel("Activate", "qs-power-off", settle=1.0)
check("qs-confirm-power-off" in names(), "power off confirmation dialog shown")
s.shot("03b-power-off-dialog")
s.click(724, 468)  # Power Off
check("qs-confirm-power-off" not in names(), "confirming closes the dialog")
calls = [c[1] for c in logind.obj.GetCalls(dbus_interface=dbusmock.MOCK_IFACE)]
check("PowerOff" in calls, "PowerOff called on (mocked) logind")

s.panel("Activate", "qs-button")
s.panel("Activate", "qs-power")
s.panel("Activate", "qs-switch-user")
calls = [c[1] for c in gdm.obj.GetCalls(dbus_interface=dbusmock.MOCK_IFACE)]
check("CreateTransientDisplay" in calls, "Switch User asks GDM for a transient display")

# Power Mode.
s.panel("Activate", "qs-button")
s.panel("Activate", "qs-power-mode-arrow")
s.shot("04-power-mode-menu")
s.panel("Activate", "qs-profile-power-saver")
active = system.get_object(PROFILES, "/org/freedesktop/UPower/PowerProfiles").Get(
    PROFILES, "ActiveProfile", dbus_interface="org.freedesktop.DBus.Properties")
check(active == "power-saver", f"ActiveProfile is power-saver ({active})")
s.shot("05-power-saver")

# Main part of the toggle goes back to balanced.
s.panel("Activate", "qs-power-mode")
active = system.get_object(PROFILES, "/org/freedesktop/UPower/PowerProfiles").Get(
    PROFILES, "ActiveProfile", dbus_interface="org.freedesktop.DBus.Properties")
check(active == "balanced", f"toggle click returns to balanced ({active})")
s.shot("06-balanced")
s.panel("CloseMenus")

# Battery percentage in the panel.
s.shot("07-panel")
s.gsettings("org.gnome.desktop.interface", "show-battery-percentage", "true")
s.shot("08-panel-percentage")

logind.terminate()
gdm.terminate()
print("FAILURES:", failures if failures else "none")
