"""The gnome-session side: org.gnome.SessionManager.EndSessionDialog (the
log out / restart / power off prompt gnome-session asks the shell for),
the power menu going through gnome-session when it runs, and the
compositor's systemd readiness notification.

gnome-session is a dbusmock here. The dialog is called the way
gnome-session does, on org.gnome.Shell (the compositor), which relays to the
panel and re-emits the panel's signals."""
import os
import re
import subprocess
import sys
import time

import dbus
import dbusmock
from dbusmock import BusType, SpawnedMock
from gi.repository import Gio, GLib

sys.path.insert(0, "/src/dev")
from driver import Session  # noqa: E402

SHELL = "org.gnome.Shell"
DIALOG_PATH = "/org/gnome/SessionManager/EndSessionDialog"
DIALOG = "org.gnome.SessionManager.EndSessionDialog"
SM = "org.gnome.SessionManager"
SM_PATH = "/org/gnome/SessionManager"
INHIBITOR = "org.gnome.SessionManager.Inhibitor"
KEY_ESC, KEY_LEFTSHIFT = 1, 42  # evdev keycodes

failures = []


def check(cond, what):
    print(("ok  " if cond else "FAIL"), what, flush=True)
    if not cond:
        failures.append(what)


out = sys.argv[1]
s = Session(out)

# --- systemd readiness ----------------------------------------------------

log = open(os.path.join(out, "notify.log")).read() if os.path.exists(os.path.join(out, "notify.log")) else ""
print(log.strip())
ready = [line for line in log.splitlines() if "READY=1" in line]
check(len(ready) == 1, "compositor sent READY=1 to NOTIFY_SOCKET exactly once")
compositor_log = open(os.path.join(out, "compositor.log")).read()
check("Notified systemd that the session is ready" in compositor_log, "readiness logged")
check("didn't show up" not in compositor_log, "READY=1 came from the panel mapping, not the fallback")
panels = subprocess.run(["pgrep", "-f", "gnome-shell-plus-panel"], capture_output=True, text=True).stdout.split()
leaked = any(b"NOTIFY_SOCKET=" in open(f"/proc/{pid}/environ", "rb").read() for pid in panels)
check(panels and not leaked, "the panel doesn't inherit NOTIFY_SOCKET")

# --- mocks ------------------------------------------------------------------

# logind (power menu items, other users' sessions) and gnome-session.
logind = SpawnedMock.spawn_with_template("logind", {}, bustype=BusType.SYSTEM)
logind.obj.AddSession("c2", "seat0", dbus.UInt32(1001), "alice", True,
                      dbus_interface=dbusmock.MOCK_IFACE)

sm = SpawnedMock.spawn_for_name(SM, SM_PATH, SM, BusType.SESSION)
sm.obj.AddMethods(SM, [
    ("Logout", "u", "", ""),
    ("Reboot", "", "", ""),
    ("Shutdown", "", "", ""),
], dbus_interface=dbusmock.MOCK_IFACE)


def add_inhibitor(n, app_id, flags, reason):
    path = f"{SM_PATH}/Inhibitor{n}"
    sm.obj.AddObject(path, INHIBITOR, {}, [
        ("GetAppId", "", "s", f"ret = {app_id!r}"),
        ("GetFlags", "", "u", f"ret = dbus.UInt32({flags})"),
        ("GetReason", "", "s", f"ret = {reason!r}"),
    ], dbus_interface=dbusmock.MOCK_IFACE)
    return path


inhibitors = [
    # An app with a desktop file inhibiting logout: listed.
    add_inhibitor(1, "gtk3-widget-factory", 1, "2 documents have unsaved changes"),
    # A service without a desktop file: left out, like GNOME.
    add_inhibitor(2, "gsd-power", 1, "Power management"),
    # An app inhibiting only suspend: left out.
    add_inhibitor(3, "gtk3-demo", 4, "Playing a video"),
]
time.sleep(0.5)

# --- signal capture ---------------------------------------------------------

signals = []
s.bus.signal_subscribe(SHELL, DIALOG, None, DIALOG_PATH, None, 0,
                       lambda *args: signals.append(args[4]))
context = GLib.MainContext.default()


def pump(seconds=0.5):
    end = time.time() + seconds
    while time.time() < end:
        while context.pending():
            context.iteration(False)
        time.sleep(0.05)


def take_signals(seconds=0.5):
    pump(seconds)
    got = list(signals)
    signals.clear()
    return got


def open_dialog(kind, seconds=60, paths=()):
    s.bus.call_sync(SHELL, DIALOG_PATH, DIALOG, "Open",
                    GLib.Variant("(uuuao)", (kind, 0, seconds, list(paths))),
                    None, 0, 10000, None)
    time.sleep(1.0)


def press(code):
    """A real key press (by keycode, as dev/scenarios/polkit.py does)."""
    rd_name, rd, _ = s._remote_desktop()
    for pressed in (True, False):
        s._call(rd_name, rd, rd_name + ".Session", "NotifyKeyboardKeycode",
                GLib.Variant("(ub)", (code, pressed)))
        time.sleep(0.05)


def escape():
    press(KEY_ESC)


# The headless session has no keyboard until Mutter's remote desktop
# creates a virtual one on the first key event; a window focused before that
# never hears about it. So press a harmless key first.
press(KEY_LEFTSHIFT)
time.sleep(0.5)


def wait_for(predicate, timeout=5.0):
    deadline = time.time() + timeout
    while time.time() < deadline:
        if predicate():
            return True
        time.sleep(0.2)
    return predicate()


def names():
    return s.panel("ListNames", settle=0)[0]


def text(name):
    state = s.inspect(name)
    return state.get("text") if state else None


def sm_calls():
    return [(c[1], list(c[2])) for c in sm.obj.GetCalls(dbus_interface=dbusmock.MOCK_IFACE)]


# --- log out, with inhibitors, confirmed --------------------------------------

open_dialog(0, 60, inhibitors)
check("end-session-dialog" in names(), "Open(logout) shows the dialog")
heading = text("end-session-dialog-heading") or ""
body = text("end-session-dialog-body") or ""
print("heading:", heading, "| body:", body)
check(heading.startswith("Log Out"), "logout heading")
# GNOME counts down in steps of 10 seconds until the last 10.
check(re.search(r"logged out automatically in (60|50) seconds", body), "logout countdown text")
s.shot("01-logout-inhibitors")
check(text("end-session-apps-0-title") == "Widget Factory", "inhibiting app listed by name")
check(text("end-session-apps-0-description") == "2 documents have unsaved changes", "inhibitor reason shown")
check("end-session-apps-1" not in names(), "service and suspend-only inhibitors left out")
check(not (s.inspect("end-session-sessions") or {}).get("visible", True), "no other-users list on logout")
check(text("end-session-confirm") in (None, "Log Out") , "confirm button")
s.panel("Activate", "end-session-confirm")
got = take_signals()
check(got == ["ConfirmedLogout"], f"Log Out emits ConfirmedLogout only ({got})")
check("end-session-dialog" not in names(), "dialog gone after confirming")

# --- restart, cancelled with Escape -------------------------------------------

open_dialog(2)
check(re.search(r"restart automatically in (60|50) seconds", text("end-session-dialog-body") or ""),
      "restart text")
check(text("end-session-sessions-0-title") == "alice", "other logged-in user listed for restart")
s.shot("02-restart")
escape()
check(wait_for(lambda: "end-session-dialog" not in names()), "dialog gone after Escape")
got = take_signals()
check(got == ["Canceled", "Closed"], f"Escape emits Canceled + Closed ({got})")

# --- power off, Cancel button -------------------------------------------------

open_dialog(1)
s.shot("03-power-off")
check(text("end-session-dialog-heading") == "Power Off", "power off heading")
s.panel("Activate", "end-session-cancel")
got = take_signals()
check(got == ["Canceled", "Closed"], f"Cancel emits Canceled + Closed ({got})")

# --- restart, confirmed -------------------------------------------------------

open_dialog(2)
s.panel("Activate", "end-session-confirm")
got = take_signals()
check(got == ["ConfirmedReboot"], f"Restart emits ConfirmedReboot ({got})")

# --- countdown confirms on its own ---------------------------------------------

open_dialog(1, seconds=4)
body = text("end-session-dialog-body") or ""
check("in 3 seconds" in body or "in 4 seconds" in body, f"short countdown text ({body})")
s.shot("04-countdown")
got = take_signals(4.5)
check(got == ["ConfirmedShutdown"], f"countdown ends in ConfirmedShutdown ({got})")
check("end-session-dialog" not in names(), "dialog gone after the countdown")

# --- Close(), unknown types ------------------------------------------------------

open_dialog(0)
s.bus.call_sync(SHELL, DIALOG_PATH, DIALOG, "Close", None, None, 0, 10000, None)
got = take_signals()
check(got == ["Closed"], f"Close() emits Closed ({got})")
check("end-session-dialog" not in names(), "Close() closes the dialog")

try:
    open_dialog(7)
    check(False, "unknown type refused")
except GLib.Error as err:
    check(Gio.DBusError.get_remote_error(err) == "org.gnome.Shell.ModalDialog.Error.UnknownType",
          f"unknown type refused with GNOME's error ({err.message})")
check("end-session-dialog" not in names(), "nothing shown for an unknown type")

# --- the power menu goes through gnome-session --------------------------------

sm.obj.ClearCalls(dbus_interface=dbusmock.MOCK_IFACE)
for item, expected in [("qs-log-out", ("Logout", [0])), ("qs-restart", ("Reboot", [])),
                       ("qs-power-off", ("Shutdown", []))]:
    s.panel("Activate", "qs-button")
    s.panel("Activate", "qs-power")
    s.panel("Activate", item, settle=1.0)
    calls = sm_calls()
    check(expected in calls, f"{item} calls gnome-session {expected[0]}{tuple(expected[1])} ({calls})")
    check(not any(n.startswith("qs-confirm") for n in names()), f"{item}: no panel-side confirmation")
    sm.obj.ClearCalls(dbus_interface=dbusmock.MOCK_IFACE)
check("end-session-dialog" not in names(), "no dialog without gnome-session asking")

# Without gnome-session the panel's own dialog is back.
sm.terminate()
time.sleep(0.5)
s.panel("Activate", "qs-button")
s.panel("Activate", "qs-power")
s.panel("Activate", "qs-restart", settle=1.0)
check("qs-confirm-restart" in names(), "without gnome-session Restart shows the panel's dialog")
escape()
check(wait_for(lambda: "qs-confirm-restart" not in names()), "panel dialog dismissed")
calls = [c[1] for c in logind.obj.GetCalls(dbus_interface=dbusmock.MOCK_IFACE)]
check("Reboot" not in calls, "nothing rebooted")

logind.terminate()
print("FAILURES:", failures if failures else "none")
