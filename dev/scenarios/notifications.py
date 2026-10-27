"""Notification server, banners, the date menu list and Do Not Disturb.

Sends notifications with notify-send (org.freedesktop.Notifications) and
gdbus (org.gtk.Notifications), checks the D-Bus signals and the widgets the
panel shows, and takes screenshots along the way.
"""
import os
import subprocess
import sys
import time

sys.path.insert(0, "/src/dev")
from driver import Session  # noqa: E402
from gi.repository import Gio, GLib  # noqa: E402

FDO = "org.freedesktop.Notifications"
FDO_PATH = "/org/freedesktop/Notifications"
APP_SCHEMA = "org.gnome.desktop.notifications.application"
CALENDAR = "org.example.Calendar"

s = Session(sys.argv[1])
failures = []


def check(condition, what):
    print(("ok   " if condition else "FAIL ") + what, flush=True)
    if not condition:
        failures.append(what)


def pump(seconds):
    """Waits while dispatching D-Bus signals and method calls to us."""
    context = GLib.MainContext.default()
    end = time.time() + seconds
    while time.time() < end:
        while context.iteration(False):
            pass
        time.sleep(0.02)


def wait_for(predicate, timeout=6.0):
    end = time.time() + timeout
    while time.time() < end:
        pump(0.1)
        if predicate():
            return True
    return False


def names():
    return set(s.panel("ListNames", settle=0)[0])


def banner_showing():
    return "notification-banner" in names()


def listed(nid):
    return f"notification-{nid}" in names()


def listed_ids():
    return [int(n[13:]) for n in names() if n.startswith("notification-") and n[13:].isdigit()]


def notify(*args):
    out = subprocess.run(["notify-send", "-p", *args], check=True,
                         capture_output=True, text=True).stdout
    pump(0.3)
    return int(out.split()[0])


def notify_waiting(*args):
    """notify-send with actions blocks until one is chosen; returns (proc, id)."""
    proc = subprocess.Popen(["notify-send", "-p", *args], stdout=subprocess.PIPE, text=True)
    nid = int(proc.stdout.readline())
    pump(0.3)
    return proc, nid


def notify_raw(summary, body, actions=(), hints=None, timeout=-1, app="Raw"):
    """Notify over D-Bus directly, for hints notify-send can't send."""
    args = GLib.Variant("(susssasa{sv}i)", (app, 0, "", summary, body, list(actions), hints or {}, timeout))
    nid = s._call(FDO, FDO_PATH, FDO, "Notify", args, "(u)").unpack()[0]
    pump(0.3)
    return nid


def move_pointer(x, y):
    """Moves the real pointer to stage coordinates without clicking."""
    rd_name, rd, _ = s._remote_desktop()
    for dx, dy in ((-10000.0, -10000.0), (float(x), float(y))):
        s._call(rd_name, rd, rd_name + ".Session", "NotifyPointerMotionRelative",
                GLib.Variant("(dd)", (dx, dy)))
    pump(0.3)


def activate(name, settle=0.6):
    s.panel("Activate", name, settle=0)
    pump(settle)


# --- signal recording -------------------------------------------------------

closed, invoked, app_calls = [], [], []


def on_signal(_conn, _sender, _path, _iface, signal, params):
    (closed if signal == "NotificationClosed" else invoked).append(tuple(params.unpack()))


s.bus.signal_subscribe(None, FDO, None, FDO_PATH, None, Gio.DBusSignalFlags.NONE, on_signal)


def closed_with(nid, reason):
    return wait_for(lambda: (nid, reason) in closed)


# A fake GApplication for org.gtk.Notifications activation.
APP_XML = """<node><interface name="org.freedesktop.Application">
  <method name="Activate"><arg type="a{sv}" direction="in"/></method>
  <method name="ActivateAction"><arg type="s" direction="in"/><arg type="av" direction="in"/>
    <arg type="a{sv}" direction="in"/></method>
  <method name="Open"><arg type="as" direction="in"/><arg type="a{sv}" direction="in"/></method>
</interface></node>"""


def on_app_call(_conn, _sender, _path, _iface, method, params, invocation):
    app_calls.append((method, params.unpack()))
    invocation.return_value(None)


node = Gio.DBusNodeInfo.new_for_xml(APP_XML)
s.bus.register_object("/" + CALENDAR.replace(".", "/"), node.interfaces[0], on_app_call)
Gio.bus_own_name_on_connection(s.bus, CALENDAR, Gio.BusNameOwnerFlags.NONE, None, None)

# Desktop files so app names and icons resolve.
apps = os.path.expanduser("~/.local/share/applications")
os.makedirs(apps, exist_ok=True)
for app_id, name, icon in [(CALENDAR, "Calendar", "x-office-calendar"),
                           ("org.example.Files", "Files", "system-file-manager"),
                           ("org.example.Muted", "Muted App", "dialog-error")]:
    with open(f"{apps}/{app_id}.desktop", "w") as f:
        f.write(f"[Desktop Entry]\nType=Application\nName={name}\nIcon={icon}\nExec=true\n")
pump(1.5)

# --- server basics ----------------------------------------------------------

info = s._call(FDO, FDO_PATH, FDO, "GetServerInformation", reply="(ssss)").unpack()
check(info[0] == "GNOME Shell Plus" and info[3] == "1.2", f"server information {info}")
caps = s._call(FDO, FDO_PATH, FDO, "GetCapabilities", reply="(as)").unpack()[0]
check({"actions", "body", "body-markup", "persistence"} <= set(caps), f"capabilities {caps}")

# --- banners ----------------------------------------------------------------

first = notify("-a", "Files", "-h", "string:desktop-entry:org.example.Files", "-i", "folder",
               "Download complete",
               "<b>report-2026.pdf</b> was saved to Downloads. It is a rather long body that "
               "should wrap onto a second line &amp; <span>escape</span> unknown tags.")
# Screenshots wait for the next damaged frame, so take this one while the
# banner is still sliding in rather than after it settled.
s.shot("01-banner")
check(wait_for(banner_showing), "banner shows")
check(wait_for(lambda: not banner_showing(), timeout=6), "banner times out after ~4s")
check(listed(first), "timed-out notification stays in the list")
check(not any(c[0] == first for c in closed), "no NotificationClosed for a timed-out banner")

transient = notify("-e", "Transient", "Gone with its banner")
check(closed_with(transient, 1), "transient notification expires (reason 1) with its banner")
check(not listed(transient), "transient notification isn't listed")

short = notify("-t", "1200", "Short timeout", "expire_timeout 1200ms")
check(wait_for(banner_showing, 2), "short-timeout banner shows")
check(wait_for(lambda: not banner_showing(), timeout=2.5), "expire_timeout shortens the banner")

critical = notify("-u", "critical", "-i", "battery-caution-symbolic",
                  "Battery critically low", "Connect the charger now. 4% remaining.")
check(wait_for(banner_showing), "critical banner shows")
pump(0.3)
s.shot("02-banner-critical")
pump(5)
check(banner_showing(), "critical banner doesn't time out")
activate("notification-banner-close")
check(closed_with(critical, 2), "close button dismisses (reason 2)")
check(wait_for(lambda: not banner_showing(), 2), "banner hides after close")

# Screenshots are slow; long timeouts keep these banners up meanwhile.
proc, chat = notify_waiting("-a", "Chat", "-t", "20000", "-i", "avatar-default-symbolic",
                            "-A", "default=Open", "-A", "reply=Reply", "-A", "read=Mark as Read",
                            "Alice", "Are we still on for lunch tomorrow?")
check(wait_for(banner_showing), "banner with actions shows")
pump(0.3)
s.shot("03-banner-actions")
activate("notification-banner-action-0")
check(wait_for(lambda: (chat, "reply") in invoked), "action button emits ActionInvoked(reply)")
check(closed_with(chat, 2), "invoking an action dismisses (reason 2)")
try:
    out = proc.communicate(timeout=3)[0]
except subprocess.TimeoutExpired:
    proc.kill()
    out = ""
check("reply" in out, f"notify-send saw the action ({out.strip()!r})")

proc, opened = notify_waiting("-t", "20000", "-A", "default=Open", "Click me",
                              "The card runs the default action")
check(wait_for(banner_showing), "clickable banner shows")
pump(0.5)
s.click(640, 95)  # a real pointer click on the banner's text
check(wait_for(lambda: (opened, "default") in invoked), "clicking the card emits ActionInvoked(default)")
s.click(1100, 700)  # move the pointer off the banner: hovering pauses banners
check(closed_with(opened, 2), "clicking the card dismisses it")
proc.kill()

progress = notify("-a", "Updater", "-t", "20000", "Installing updates",
                  "Downloading 12 packages. This takes a while, the body is long enough to need "
                  "a few lines of text in the banner.")
check(wait_for(banner_showing), "progress banner shows")
replaced = notify("-a", "Updater", "-t", "20000", "-r", str(progress), "Installing updates",
                  "<i>90%</i> done")
check(replaced == progress, f"replaces_id keeps the id ({replaced} == {progress})")
pump(0.5)
check(banner_showing(), "replaced notification stays on screen")
s.shot("04-banner-replaced")
pump(1)
s._call(FDO, FDO_PATH, FDO, "CloseNotification", GLib.Variant("(u)", (progress,)))
check(closed_with(progress, 3), "CloseNotification closes (reason 3)")
check(wait_for(lambda: not banner_showing(), 2), "closed notification's banner hides")
check(not listed(progress), "closed notification leaves the list")

# Hovering pauses the timeout; leaving lets the banner go shortly after.
hover = notify("-t", "1500", "Hover me", "The pointer keeps me up")
check(wait_for(banner_showing, 2), "hover banner shows")
move_pointer(640, 90)
pump(3)
check(banner_showing(), "hovered banner doesn't time out")
move_pointer(1100, 700)
check(wait_for(lambda: not banner_showing(), 3), "banner goes after the pointer leaves")

# image-data pixels, link markup, and a resident notification.
size = 48
pixels = bytes(v for y in range(size) for x in range(size)
               for v in (40 + 4 * x, 90 + 3 * y, 220, 255))
image = GLib.Variant("(iiibiiay)", (size, size, size * 4, True, 8, 4, pixels))
resident = notify_raw("Photo shared", 'Open <a href="https://gnome.org">gnome.org</a> to see '
                      "<b>the album</b>", actions=["view", "View"], app="Photos",
                      hints={"image-data": image, "resident": GLib.Variant("b", True)},
                      timeout=20000)
check(wait_for(banner_showing), "image-data banner shows")
s.shot("04b-banner-image-data")
activate("notification-banner-action-0")
check(wait_for(lambda: (resident, "view") in invoked), "resident: action invoked")
pump(0.5)
check(listed(resident) and (resident, 2) not in closed, "resident: stays after its action")
wait_for(lambda: not banner_showing(), 2)

# --- org.gtk.Notifications --------------------------------------------------

subprocess.run(["gdbus", "call", "--session", "--dest", "org.gtk.Notifications",
                "--object-path", "/org/gtk/Notifications",
                "--method", "org.gtk.Notifications.AddNotification", CALENDAR, "standup",
                "{'title': <'Team standup in 5 minutes'>, 'body': <'Meeting room 2 & video call'>,"
                " 'priority': <'urgent'>, 'icon': <('themed', <['x-office-calendar-symbolic']>)>,"
                " 'default-action': <'app.open-event'>, 'default-action-target': <'standup'>,"
                " 'buttons': <[{'label': <'Snooze'>, 'action': <'app.snooze'>, 'target': <'5'>},"
                "              {'label': <'Join'>, 'action': <'app.join'>}]>}"],
               check=True, stdout=subprocess.DEVNULL)
check(wait_for(banner_showing), "GNotification banner shows")
pump(0.3)
s.shot("05-banner-gtk")
activate("notification-banner-action-0")
check(wait_for(lambda: ("ActivateAction", ("snooze", ["5"], {})) in app_calls),
      f"GNotification button activates app.snooze ({app_calls})")

subprocess.run(["gdbus", "call", "--session", "--dest", "org.gtk.Notifications",
                "--object-path", "/org/gtk/Notifications",
                "--method", "org.gtk.Notifications.AddNotification", CALENDAR, "review",
                "{'title': <'Design review'>, 'body': <'Tomorrow at 10:00'>, 'priority': <'low'>}"],
               check=True, stdout=subprocess.DEVNULL)
wait_for(banner_showing)
wait_for(lambda: not banner_showing(), timeout=6)

# More notifications for the grouped list.
notify("-a", "Files", "-h", "string:desktop-entry:org.example.Files", "Copy finished",
       "42 files copied to Backup")
wait_for(lambda: not banner_showing(), timeout=6)
s.shot("06-indicator-dot")

# --- the date menu ----------------------------------------------------------

activate("clock-button", settle=1.0)
s.shot("07-date-menu")
activate(f"notification-{first}")
check(not listed(first), "clicking a listed notification removes it")
check(closed_with(first, 2), "clicking a listed notification dismisses it (reason 2)")
s.panel("CloseMenus")
pump(0.5)
s.shot("08-indicator-seen")
check(len(listed_ids()) >= 3, f"remaining notifications listed ({listed_ids()})")

# --- per-app settings -------------------------------------------------------

muted_path = f"{APP_SCHEMA}:/org/gnome/desktop/notifications/application/org-example-muted/"
subprocess.run(["gsettings", "set", muted_path, "enable", "false"], check=True)
pump(0.5)
muted = notify("-h", "string:desktop-entry:org.example.Muted", "Muted", "Should be dropped")
pump(0.5)
check(not banner_showing() and not listed(muted), "per-app enable=false drops notifications")
children = subprocess.run(["gsettings", "get", "org.gnome.desktop.notifications", "application-children"],
                          capture_output=True, text=True).stdout
check("org-example-files" in children and "org-example-calendar" in children,
      f"apps registered in application-children {children.strip()}")
app_id = subprocess.run(["gsettings", "get",
                         f"{APP_SCHEMA}:/org/gnome/desktop/notifications/application/org-example-files/",
                         "application-id"], capture_output=True, text=True).stdout
check("org.example.Files.desktop" in app_id, f"application-id set ({app_id.strip()})")

quiet_path = f"{APP_SCHEMA}:/org/gnome/desktop/notifications/application/org-example-calendar/"
subprocess.run(["gsettings", "set", quiet_path, "show-banners", "false"], check=True)
pump(0.5)
subprocess.run(["gdbus", "call", "--session", "--dest", "org.gtk.Notifications",
                "--object-path", "/org/gtk/Notifications",
                "--method", "org.gtk.Notifications.AddNotification", CALENDAR, "quiet",
                "{'title': <'No banner for this one'>}"], check=True, stdout=subprocess.DEVNULL)
pump(0.8)
check(not banner_showing(), "per-app show-banners=false: no banner")
check(any(nid > muted for nid in listed_ids()),
      "per-app show-banners=false: still listed")
subprocess.run(["gdbus", "call", "--session", "--dest", "org.gtk.Notifications",
                "--object-path", "/org/gtk/Notifications",
                "--method", "org.gtk.Notifications.RemoveNotification", CALENDAR, "quiet"],
               check=True, stdout=subprocess.DEVNULL)
subprocess.run(["gsettings", "reset", quiet_path, "show-banners"], check=True)

# --- Do Not Disturb ---------------------------------------------------------

s.gsettings("org.gnome.desktop.notifications", "show-banners", "false")
quiet = notify("-a", "Mail", "New message", "From Bob: weekend plans")
pump(0.6)
check(not banner_showing(), "Do Not Disturb: no banner")
check(listed(quiet), "Do Not Disturb: notification still listed")
s.shot("09-dnd")
urgent = notify("-u", "critical", "Critical in Do Not Disturb", "Critical banners still show")
check(wait_for(banner_showing), "Do Not Disturb: critical banner still shows")
activate("notification-banner-close")
check(closed_with(urgent, 2), "critical banner dismissed")
wait_for(lambda: not banner_showing(), 2)
activate("clock-button", settle=1.0)
s.shot("10-date-menu-dnd")
activate("notification-dnd")  # switch Do Not Disturb off again
pump(0.5)
dnd = subprocess.run(["gsettings", "get", "org.gnome.desktop.notifications", "show-banners"],
                     capture_output=True, text=True).stdout.strip()
check(dnd == "true", f"Do Not Disturb switch writes show-banners ({dnd})")

# --- Clear ------------------------------------------------------------------

activate("notification-clear")
check(not listed_ids(), "Clear empties the list")
check(closed_with(quiet, 2), "Clear emits NotificationClosed(reason 2)")
s.shot("11-cleared")
s.panel("CloseMenus")

# Dark style
s.gsettings("org.gnome.desktop.interface", "color-scheme", "prefer-dark")
pump(1)
proc, _ = notify_waiting("-a", "Chat", "-t", "20000", "-i", "avatar-default-symbolic",
                         "-A", "reply=Reply", "Alice", "Dark style banner")
s.shot("12-banner-dark")
activate("notification-banner-action-0")
proc.kill()
notify("-a", "Mail", "Second", "Something else in the list")
wait_for(lambda: not banner_showing(), 6)
activate("clock-button", settle=1.0)
s.shot("13-date-menu-dark")
s.panel("CloseMenus")

print("FAILURES:", failures if failures else "none", flush=True)
