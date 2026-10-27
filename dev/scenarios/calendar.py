"""Calendar events in the date menu.

Stands in for gnome-shell-calendar-server: a fake
org.gnome.Shell.CalendarServer (same interface as GNOME Shell's
data/dbus-interfaces/org.gnome.Shell.CalendarServer.xml) in a Python
subprocess that the bus starts on demand through a .service file, as it
would start the real one. It has a few events today and on another day
(one all-day, one multi-day), plus a small control interface for the test
(remove an event, quit).

Checks that the server is missing quietly at first, gets activated by the
panel, that the list and the marked days follow, that selecting another day
or month works, that EventsRemoved updates the list, that clicking opens the
calendar app, and that everything hides when the server vanishes.
"""
import json
import os
import sys
import tempfile
import time

sys.path.insert(0, "/src/dev")
from driver import Session  # noqa: E402
from gi.repository import Gio, GLib  # noqa: E402

SERVER = r'''
import json
import sys
import warnings
from gi.repository import Gio, GLib

warnings.simplefilter("ignore", DeprecationWarning)

log_path, events_path = sys.argv[1], sys.argv[2]
with open(events_path) as f:
    events = json.load(f)  # [id, summary, start, end]

XML = """<node>
<interface name="org.gnome.Shell.CalendarServer">
  <method name="SetTimeRange">
    <arg type="x" name="since" direction="in"/>
    <arg type="x" name="until" direction="in"/>
    <arg type="b" name="force_reload" direction="in"/>
  </method>
  <signal name="EventsAddedOrUpdated"><arg type="a(ssxxa{sv})" name="events"/></signal>
  <signal name="EventsRemoved"><arg type="as" name="ids"/></signal>
  <signal name="ClientDisappeared"><arg type="s" name="source_uid"/></signal>
  <property name="HasCalendars" type="b" access="read"/>
</interface>
<interface name="org.example.FakeCalendarControl">
  <method name="Remove"><arg type="as" name="ids" direction="in"/></method>
  <method name="Quit"/>
</interface></node>"""
PATH = "/org/gnome/Shell/CalendarServer"
IFACE = "org.gnome.Shell.CalendarServer"
loop = GLib.MainLoop()

def log(line):
    with open(log_path, "a") as f:
        f.write(line + "\n")

def emit(conn, signal, signature, value):
    conn.emit_signal(None, PATH, IFACE, signal, GLib.Variant(signature, (value,)))

def on_call(conn, _sender, _path, iface, method, params, invocation):
    if method == "SetTimeRange":
        since, until, force = params.unpack()
        log(f"SetTimeRange {since} {until} {force}")
        invocation.return_value(None)
        # Like the real server: events in the range, after the reply.
        hits = [(i, s, a, b, {}) for i, s, a, b in events
                if (since <= a < until) or (a <= since and b - 1 > since)]
        GLib.idle_add(lambda: emit(conn, "EventsAddedOrUpdated", "(a(ssxxa{sv}))", hits) and False)
    elif method == "Remove":
        (ids,) = params.unpack()
        log(f"Remove {ids!r}")
        events[:] = [e for e in events if not any(e[0].startswith(i) for i in ids)]
        emit(conn, "EventsRemoved", "(as)", ids)
        invocation.return_value(None)
    elif method == "Quit":
        log("Quit")
        invocation.return_value(None)
        GLib.timeout_add(100, loop.quit)

def on_get(_conn, _sender, _path, _iface, name):
    return GLib.Variant("b", True)

conn = Gio.bus_get_sync(Gio.BusType.SESSION)
node = Gio.DBusNodeInfo.new_for_xml(XML)
for iface in node.interfaces:
    conn.register_object(PATH, iface, on_call, on_get, None)
Gio.bus_own_name_on_connection(conn, "org.gnome.Shell.CalendarServer", Gio.BusNameOwnerFlags.NONE,
                               lambda *_: log("started"), None)
loop.run()
'''

s = Session(sys.argv[1])
bus = Gio.bus_get_sync(Gio.BusType.SESSION)
failures = []


def check(condition, what):
    print(("ok   " if condition else "FAIL ") + what, flush=True)
    if not condition:
        failures.append(what)


def wait_for(predicate, timeout=5.0):
    end = time.time() + timeout
    while time.time() < end:
        try:
            if predicate():
                return True
        except GLib.Error:
            pass
        time.sleep(0.1)
    return False


def inspect(name):
    return s.panel("Inspect", name, settle=0)[0]


def shown():
    try:
        return inspect("calendar-events")["mapped"]
    except GLib.Error:
        return False


def listing():
    """[(summary, time)] of the shown events, in order."""
    names = set(s.panel("ListNames", settle=0)[0])
    out = []
    n = 0
    while f"calendar-event-{n}" in names:
        out.append((inspect(f"calendar-event-{n}")["text"], inspect(f"calendar-event-time-{n}")["text"]))
        n += 1
    return out


def marked():
    return list(inspect("date-menu-calendar")["marked"])


def select(t):
    s.panel("SetValue", "date-menu-calendar", float(t), settle=0)


def log_lines():
    with open(log_path) as f:
        return f.read().splitlines()


def control(method, args=None):
    bus.call_sync("org.gnome.Shell.CalendarServer", "/org/gnome/Shell/CalendarServer",
                  "org.example.FakeCalendarControl", method, args, None,
                  Gio.DBusCallFlags.NO_AUTO_START, 5000, None)


def at(day, hour=0, minute=0, month_offset=0):
    """Local Unix time on `day` of this month (or a later one)."""
    now = time.localtime()
    month = now.tm_mon - 1 + month_offset
    return int(time.mktime((now.tm_year + month // 12, month % 12 + 1, day, hour, minute, 0, 0, 0, -1)))


def open_menu():
    s.panel("CloseMenus", settle=0.3)
    s.panel("Activate", "clock-button", settle=1.0)


# --- events -----------------------------------------------------------------

today = time.localtime().tm_mday
other = today - 5 if today > 15 else today + 5
empty_day = other + 3  # neither today nor the trip
D = 24 * 3600
EVENTS = [
    ["src1\nstandup\n", "Team standup", at(today, 9, 30), at(today, 9, 45)],
    ["src1\nlunch\n", "Lunch with Sam", at(today, 12), at(today, 13)],
    ["src1\nrelease\n", "Release day", at(today), at(today) + D],
    ["src1\ntrip\n", "Road trip", at(other), at(other + 3)],
    ["src1\ndentist\n", "Dentist", at(other, 15), at(other, 16)],
]
EXPECTED_MARKS = sorted({today, other, other + 1, other + 2})

tmp = tempfile.mkdtemp()
log_path = os.path.join(tmp, "server.log")
open(log_path, "w").close()
events_path = os.path.join(tmp, "events.json")
with open(events_path, "w") as f:
    json.dump(EVENTS, f)
script = os.path.join(tmp, "calendar_server.py")
with open(script, "w") as f:
    f.write(SERVER)
launched = os.path.join(tmp, "calendar-launched")

apps = os.path.expanduser("~/.local/share/applications")
os.makedirs(apps, exist_ok=True)
with open(f"{apps}/org.gnome.Calendar.desktop", "w") as f:
    f.write(f"[Desktop Entry]\nType=Application\nName=Calendar\nExec=touch {launched}\n")
services = os.path.expanduser("~/.local/share/dbus-1/services")
service_file = f"{services}/org.gnome.Shell.CalendarServer.service"


def reload_bus():
    bus.call_sync("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus",
                  "ReloadConfig", None, None, Gio.DBusCallFlags.NONE, 5000, None)
    time.sleep(0.5)


time.sleep(1.5)

# --- no calendar server installed --------------------------------------------

open_menu()
check(not shown(), "no events section without a calendar server")
check(marked() == [], "no marked days without a calendar server")
s.shot("01-no-server")

# --- the server is installed: opening the menu starts it ---------------------

os.makedirs(services, exist_ok=True)
with open(service_file, "w") as f:
    f.write(f"[D-BUS Service]\nName=org.gnome.Shell.CalendarServer\n"
            f"Exec={sys.executable} {script} {log_path} {events_path}\n")
reload_bus()

open_menu()
check(wait_for(lambda: "started" in log_lines()), "opening the menu activates the calendar server")
ranges = [l for l in log_lines() if l.startswith("SetTimeRange")]
month_range = f"SetTimeRange {at(1)} {at(1, month_offset=1)} True"
check(month_range in ranges, f"asks for this month ({ranges})")
check(wait_for(shown), "events section shows today's events")
items = listing()
print("today:", items, flush=True)
check([i[0] for i in items] == ["Release day", "Team standup", "Lunch with Sam"],
      "all-day event first, then by start time")
check([i[1] for i in items] == ["All Day", "09:30 – 09:45", "12:00 – 13:00"], "event times")
check(inspect("calendar-events-title")["text"] == "Today", "title says Today")
check(wait_for(lambda: marked() == EXPECTED_MARKS), f"marked days {marked()} == {EXPECTED_MARKS}")
s.shot("02-today")

# --- another day -------------------------------------------------------------

select(at(other, 12))
check(wait_for(lambda: [i[0] for i in listing()] == ["Road trip", "Dentist"]),
      "selecting another day switches the list")
items = listing()
print(f"day {other}:", items, flush=True)
trip = f"{time.strftime('%m/%d', time.localtime(at(other)))} – " \
       f"{time.strftime('%m/%d', time.localtime(at(other + 2)))}"
check(items and items[0][1] == trip, f"multi-day event shows its dates ({trip})")
check(inspect("calendar-events-title")["text"] == time.strftime("%B %-d", time.localtime(at(other))),
      "title shows the selected date")
s.shot("03-other-day")

select(at(other + 1, 12))
check(wait_for(lambda: [i[0] for i in listing()] == ["Road trip"]), "multi-day event on its middle day")

select(at(empty_day, 12))
check(wait_for(lambda: not shown()), "section hides on a day without events")
s.shot("04-empty-day")

# --- another month -----------------------------------------------------------

select(at(1, 12, month_offset=1))
next_range = f"SetTimeRange {at(1, month_offset=1)} {at(1, month_offset=2)} True"
check(wait_for(lambda: next_range in log_lines()), "changing month asks for the new range")
check(wait_for(lambda: marked() == []), "next month has no marked days")
select(at(today, 12))
check(wait_for(lambda: log_lines().count(month_range) >= 2), "back to this month asks for it again")
check(wait_for(lambda: marked() == EXPECTED_MARKS), "marks are back")
check(wait_for(lambda: len(listing()) == 3), "today's events are back")

# --- EventsRemoved -------------------------------------------------------------

control("Remove", GLib.Variant("(as)", (["src1\nlunch\n"],)))
check(wait_for(lambda: [i[0] for i in listing()] == ["Release day", "Team standup"]),
      "EventsRemoved drops the event from the list")
s.shot("05-removed")

# Dark style
s.gsettings("org.gnome.desktop.interface", "color-scheme", "prefer-dark")
open_menu()
check(wait_for(shown), "events shown again on reopening")
s.shot("06-dark")
s.gsettings("org.gnome.desktop.interface", "color-scheme", "default")

# --- clicking opens GNOME Calendar ---------------------------------------------

s.panel("Activate", "calendar-events", settle=0)
check(wait_for(lambda: os.path.exists(launched)), "clicking the events opens the calendar app")
check(wait_for(lambda: not inspect("date-menu-calendar")["mapped"]), "and closes the menu")

# --- the server vanishes ---------------------------------------------------------

open_menu()
check(wait_for(shown), "events shown before the server quits")
os.remove(service_file)
reload_bus()
control("Quit")
check(wait_for(lambda: not shown()), "section hides when the server vanishes")
check(wait_for(lambda: marked() == []), "marks cleared when the server vanishes")
s.shot("07-server-gone")
open_menu()
check(not shown() and marked() == [], "stays hidden on reopening without a server")
s.panel("CloseMenus")

print("server log:", log_lines(), flush=True)
print("FAILURES:", failures if failures else "none", flush=True)
