"""The lock screen's top bar (panel/src/lock_screen/status_bar.rs).

Checks: while locked, the bar mirrors the top bar's status icons (network,
Bluetooth, volume, battery), the keyboard layout (with two layouts) and
the accessibility button, on the curtain and on the prompt; battery
percentage follows show-battery-percentage; a UPower change (mocked
battery) and a volume change (pactl) update the icons while locked; the
accessibility menu opens inside the lock window and its switches flip the
a11y GSettings; clicking the status icons doesn't open anything; the
mirrors go away with the lock window and come back on the next lock.
"""
import shutil
import subprocess
import sys
import time

import dbus
import dbusmock

sys.path.insert(0, "/src/dev")
from driver import Session  # noqa: E402

from gi.repository import GLib  # noqa: E402

PASSWORD = "hunter2"
SS = ("org.gnome.ScreenSaver", "/org/gnome/ScreenSaver", "org.gnome.ScreenSaver")
UPOWER_DEVICE = "org.freedesktop.UPower.Device"
DISPLAY_DEVICE = "/org/freedesktop/UPower/devices/DisplayDevice"

s = Session(sys.argv[1])
failures = []


def check(ok, what):
    print(("PASS " if ok else "FAIL ") + what, flush=True)
    if not ok:
        failures.append(what)


def wait_for(predicate, timeout=6.0):
    deadline = time.time() + timeout
    while time.time() < deadline:
        if predicate():
            return True
        time.sleep(0.1)
    return predicate()


def ss(method):
    return s._call(SS[0], SS[1], SS[2], method, None, "(b)" if method == "GetActive" else None)


def mapped(name):
    state = s.inspect(name)
    return bool(state and state["mapped"])


def text(name):
    return (s.inspect(name) or {}).get("text", "")


def names():
    return s.panel("ListNames", settle=0)[0]


KEY_ENTER, KEY_ESC, KEY_LEFTMETA, KEY_SPACE = 28, 1, 125, 57
KEYCODES = {c: code for code, c in enumerate("1234567890", start=2)}
for row, start in (("qwertyuiop", 16), ("asdfghjkl", 30), ("zxcvbnm", 44)):
    KEYCODES.update({c: code for code, c in enumerate(row, start=start)})


def tap(code, settle=0.3):
    rd_name, rd, _ = s._remote_desktop()
    for pressed in (True, False):
        s._call(rd_name, rd, rd_name + ".Session", "NotifyKeyboardKeycode",
                GLib.Variant("(ub)", (code, pressed)))
        time.sleep(0.03)
    time.sleep(settle)


subprocess.run(["chpasswd"], input=f"root:{PASSWORD}\n", text=True, check=True)
shutil.copy("/src/data/pam/gnome-shell-plus", "/etc/pam.d/gnome-shell-plus")
s.gsettings("org.gnome.desktop.input-sources", "sources", "[('xkb', 'us'), ('xkb', 'de')]")
s.gsettings("org.gnome.desktop.interface", "show-battery-percentage", "false")
s.gsettings("org.gnome.desktop.a11y.interface", "high-contrast", "false")

MIRRORED = ["network-indicator", "bluetooth-indicator", "volume-indicator", "battery-indicator"]
for name in MIRRORED:
    check(wait_for(lambda: mapped(name)), f"top bar shows {name}")
check(not mapped("lock-bar"), "no lock bar while unlocked")

# --- lock: the curtain ------------------------------------------------------

ss("Lock")
check(wait_for(lambda: mapped("lock-screen"), 8), "locked")
time.sleep(0.8)
for name in MIRRORED:
    check(mapped("lock-" + name), f"curtain mirrors {name}")
    check(text("lock-" + name) == text(name) and text(name),
          f"same icon as the top bar: {text('lock-' + name)!r} vs {text(name)!r}")
check(not mapped("lock-vpn-indicator"), "no VPN icon (VPN off)")
check(not mapped("lock-microphone-indicator"), "no microphone privacy indicator (not recording)")
check(not mapped("lock-battery-percentage"), "no percentage while show-battery-percentage is off")
check(mapped("lock-keyboard-layout") and text("lock-keyboard-layout") == "en",
      f"keyboard layout shown: {text('lock-keyboard-layout')!r}")
check(mapped("lock-a11y-button"), "accessibility button shown")
bar = (s.inspect("lock-bar") or {}).get("bounds", (0, 0, 0, 0))
check(bar[1] == 0 and bar[3] <= 40 and bar[2] >= 1000, f"bar across the top: {bar}")
status = (s.inspect("lock-status-icons") or {}).get("bounds", (0, 0, 0, 0))
check(status[0] > 1000, f"status icons at the right: {status}")
s.shot("01-curtain")

s.gsettings("org.gnome.desktop.interface", "show-battery-percentage", "true")
check(wait_for(lambda: mapped("lock-battery-percentage")), "percentage shown when enabled")
check(text("lock-battery-percentage").startswith("76"),
      f"percentage text: {text('lock-battery-percentage')!r}")

# A battery change while locked (mocked UPower).
before = text("lock-battery-indicator")
dbus.SystemBus().get_object("org.freedesktop.UPower", DISPLAY_DEVICE).UpdateProperties(
    UPOWER_DEVICE, {"Percentage": dbus.Double(9.0), "IconName": "battery-caution-symbolic"},
    dbus_interface=dbusmock.MOCK_IFACE)
check(wait_for(lambda: text("lock-battery-percentage").startswith("9")),
      f"percentage follows UPower while locked: {text('lock-battery-percentage')!r}")
check(wait_for(lambda: text("lock-battery-indicator") == "battery-caution-symbolic"),
      f"battery icon follows UPower: {before!r} -> {text('lock-battery-indicator')!r}")
s.shot("02-curtain-battery")

# A volume change while locked.
before = text("lock-volume-indicator")
subprocess.run(["pactl", "set-sink-mute", "@DEFAULT_SINK@", "1"], check=True)
check(wait_for(lambda: text("lock-volume-indicator") == "audio-volume-muted-symbolic"),
      f"volume icon follows mute while locked: {before!r} -> {text('lock-volume-indicator')!r}")
subprocess.run(["pactl", "set-sink-mute", "@DEFAULT_SINK@", "0"], check=True)
subprocess.run(["pactl", "set-sink-volume", "@DEFAULT_SINK@", "10%"], check=True)
check(wait_for(lambda: text("lock-volume-indicator") == "audio-volume-low-symbolic"),
      f"volume icon follows the level: {text('lock-volume-indicator')!r}")

# Super+Space switches layouts while locked; the label follows.
def super_space():
    rd_name, rd, _ = s._remote_desktop()
    for code, pressed in ((KEY_LEFTMETA, True), (KEY_SPACE, True), (KEY_SPACE, False), (KEY_LEFTMETA, False)):
        s._call(rd_name, rd, rd_name + ".Session", "NotifyKeyboardKeycode",
                GLib.Variant("(ub)", (code, pressed)))
        time.sleep(0.05)
    time.sleep(0.3)


super_space()
check(wait_for(lambda: text("lock-keyboard-layout") == "de"),
      f"layout label follows Super+Space: {text('lock-keyboard-layout')!r}")
check(not mapped("lock-prompt"), "Super+Space leaves the curtain down")

# Clicking the accessibility button on the curtain opens the menu without
# lifting the curtain; a click elsewhere only closes the menu.
a11y = (s.inspect("lock-a11y-button") or {}).get("bounds", (0, 0, 0, 0))
s.click(int(a11y[0] + a11y[2] / 2), int(a11y[1] + a11y[3] / 2))
check(wait_for(lambda: mapped("lock-a11y-high-contrast")), "accessibility menu opens on the curtain")
check(not mapped("lock-prompt"), "the curtain stays down")
s.shot("02b-curtain-a11y-menu")
s.click(300, 500)
check(wait_for(lambda: not mapped("lock-a11y-high-contrast")), "a click outside closes the menu")
check(not mapped("lock-prompt"), "that click doesn't lift the curtain")

super_space()
check(wait_for(lambda: text("lock-keyboard-layout") == "en"), "back to the US layout")

# Clicking the status icons opens nothing and leaves the curtain to lift.
before_names = set(names())
s.click(int(status[0] + status[2] / 2), 16)
check(mapped("lock-screen"), "still locked after clicking the status icons")
new = set(names()) - before_names
check(not any("qs-" in n for n in new), f"no system menu opened: {sorted(new)}")

# --- the prompt -------------------------------------------------------------

if not mapped("lock-prompt"):
    tap(KEY_ENTER)
check(wait_for(lambda: mapped("lock-prompt")), "prompt shown")
time.sleep(0.5)
for name in MIRRORED + ["battery-percentage"]:
    check(mapped("lock-" + name), f"prompt shows lock-{name}")
check(mapped("lock-keyboard-layout"), "prompt shows the keyboard layout")
s.shot("03-prompt")

# Accessibility menu: opens inside the lock window, flips a GSetting, and
# keeps the password focused.
a11y = (s.inspect("lock-a11y-button") or {}).get("bounds", (0, 0, 0, 0))
s.click(int(a11y[0] + a11y[2] / 2), int(a11y[1] + a11y[3] / 2))
check(wait_for(lambda: mapped("lock-a11y-high-contrast")), "accessibility menu opens")
check(mapped("lock-prompt"), "the prompt stays up under the menu")
s.shot("04-a11y-menu")
hc = (s.inspect("lock-a11y-high-contrast") or {}).get("bounds", (0, 0, 0, 0))
s.click(int(hc[0] + hc[2] / 2), int(hc[1] + hc[3] / 2))
value = subprocess.run(["gsettings", "get", "org.gnome.desktop.a11y.interface", "high-contrast"],
                       capture_output=True, text=True).stdout.strip()
check(value == "true", f"High Contrast switch sets the setting: {value}")
s.shot("05-high-contrast")
s.gsettings("org.gnome.desktop.a11y.interface", "high-contrast", "false")
tap(KEY_ESC)
check(wait_for(lambda: not mapped("lock-a11y-high-contrast")), "Escape closes the menu")
check(mapped("lock-prompt"), "Escape with the menu open keeps the prompt")

# --- unlock and lock again -------------------------------------------------------

check(wait_for(lambda: (s.inspect("lock-password") or {}).get("sensitive")), "password entry ready")
for c in PASSWORD:
    tap(KEYCODES[c], settle=0.03)
tap(KEY_ENTER)
check(wait_for(lambda: not ss("GetActive")[0], 8), "unlocked")
check(wait_for(lambda: "lock-bar" not in names()), "the lock bar goes away with the lock window")

ss("Lock")
check(wait_for(lambda: mapped("lock-screen"), 8), "locked again")
time.sleep(0.6)
check(mapped("lock-battery-indicator") and text("lock-battery-indicator") == "battery-caution-symbolic",
      "the next lock mirrors the current state")
s.shot("06-relocked")

for app in s.apps:
    app.terminate()
print("FAILURES: " + (", ".join(failures) if failures else "none"), flush=True)
sys.exit(1 if failures else 0)
