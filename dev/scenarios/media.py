"""Media player (MPRIS) controls in the date menu.

Runs a fake org.mpris.MediaPlayer2 player in a subprocess (its own bus
connection and main loop, so the panel's calls never wait on us), presses
its buttons through the panel and checks the player got the calls and the
card followed, then makes the player vanish.
"""
import os
import struct
import subprocess
import sys
import tempfile
import time
import zlib

sys.path.insert(0, "/src/dev")
from driver import Session  # noqa: E402
from gi.repository import GLib  # noqa: E402

PLAYER_ID = "org.example.Music"

PLAYER = r'''
import sys
import warnings
from gi.repository import Gio, GLib

warnings.simplefilter("ignore", DeprecationWarning)

art, log_path = sys.argv[1], sys.argv[2]
XML = """<node>
<interface name="org.mpris.MediaPlayer2">
  <method name="Raise"/><method name="Quit"/>
  <property name="CanRaise" type="b" access="read"/>
  <property name="CanQuit" type="b" access="read"/>
  <property name="HasTrackList" type="b" access="read"/>
  <property name="Identity" type="s" access="read"/>
  <property name="DesktopEntry" type="s" access="read"/>
  <property name="SupportedUriSchemes" type="as" access="read"/>
  <property name="SupportedMimeTypes" type="as" access="read"/>
</interface>
<interface name="org.mpris.MediaPlayer2.Player">
  <method name="Next"/><method name="Previous"/><method name="Pause"/>
  <method name="PlayPause"/><method name="Stop"/><method name="Play"/>
  <property name="PlaybackStatus" type="s" access="read"/>
  <property name="Metadata" type="a{sv}" access="read"/>
  <property name="CanGoNext" type="b" access="read"/>
  <property name="CanGoPrevious" type="b" access="read"/>
  <property name="CanPlay" type="b" access="read"/>
  <property name="CanPause" type="b" access="read"/>
  <property name="CanControl" type="b" access="read"/>
</interface></node>"""
TRACKS = [
    {"mpris:trackid": GLib.Variant("o", "/org/example/Music/track/1"),
     "xesam:title": GLib.Variant("s", "Clair de Lune"),
     "xesam:artist": GLib.Variant("as", ["Claude Debussy"]),
     "xesam:album": GLib.Variant("s", "Suite bergamasque"),
     "mpris:artUrl": GLib.Variant("s", "file://" + art)},
    {"mpris:trackid": GLib.Variant("o", "/org/example/Music/track/2"),
     "xesam:title": GLib.Variant("s", "Gymnopédie No. 1"),
     "xesam:artist": GLib.Variant("as", ["Erik Satie"])},
]
state = {"track": 0, "status": "Paused"}
PATH = "/org/mpris/MediaPlayer2"
PLAYER = "org.mpris.MediaPlayer2.Player"

def props():
    track = state["track"]
    return {
        "CanRaise": GLib.Variant("b", True), "CanQuit": GLib.Variant("b", False),
        "HasTrackList": GLib.Variant("b", False), "Identity": GLib.Variant("s", "Music"),
        "DesktopEntry": GLib.Variant("s", "org.example.Music"),
        "SupportedUriSchemes": GLib.Variant("as", []), "SupportedMimeTypes": GLib.Variant("as", []),
        "PlaybackStatus": GLib.Variant("s", state["status"]),
        "Metadata": GLib.Variant("a{sv}", TRACKS[track]),
        "CanGoNext": GLib.Variant("b", track + 1 < len(TRACKS)),
        "CanGoPrevious": GLib.Variant("b", track > 0),
        "CanPlay": GLib.Variant("b", True), "CanPause": GLib.Variant("b", True),
        "CanControl": GLib.Variant("b", True),
    }

def changed(conn, names):
    values = props()
    conn.emit_signal(None, PATH, "org.freedesktop.DBus.Properties", "PropertiesChanged",
                     GLib.Variant("(sa{sv}as)", (PLAYER, {n: values[n] for n in names}, [])))

def on_call(conn, _sender, _path, _iface, method, _params, invocation):
    with open(log_path, "a") as log:
        log.write(method + "\n")
    if method == "PlayPause":
        state["status"] = "Paused" if state["status"] == "Playing" else "Playing"
        changed(conn, ["PlaybackStatus"])
    elif method == "Next" and state["track"] + 1 < len(TRACKS):
        state["track"] += 1
        changed(conn, ["Metadata", "CanGoNext", "CanGoPrevious"])
    elif method == "Previous" and state["track"] > 0:
        state["track"] -= 1
        changed(conn, ["Metadata", "CanGoNext", "CanGoPrevious"])
    invocation.return_value(None)

def on_get(_conn, _sender, _path, _iface, name):
    return props()[name]

conn = Gio.bus_get_sync(Gio.BusType.SESSION)
node = Gio.DBusNodeInfo.new_for_xml(XML)
for iface in node.interfaces:
    conn.register_object(PATH, iface, on_call, on_get, None)
Gio.bus_own_name_on_connection(conn, "org.mpris.MediaPlayer2.examplemusic",
                               Gio.BusNameOwnerFlags.NONE,
                               lambda *_: print("player ready", flush=True), None)
GLib.MainLoop().run()
'''

s = Session(sys.argv[1])
failures = []


def check(condition, what):
    print(("ok   " if condition else "FAIL ") + what, flush=True)
    if not condition:
        failures.append(what)


def wait_for(predicate, timeout=5.0):
    end = time.time() + timeout
    while time.time() < end:
        if predicate():
            return True
        time.sleep(0.1)
    return False


def names():
    return set(s.panel("ListNames", settle=0)[0])


def calls():
    with open(log_path) as f:
        return f.read().split()


def png(path, size=96):
    """A gradient PNG standing in for album art."""
    rows = b"".join(b"\0" + bytes(v for x in range(size)
                                  for v in (230 - x, 80 + y, 120 + x // 2))
                    for y in range(size))

    def chunk(kind, data):
        return (struct.pack(">I", len(data)) + kind + data
                + struct.pack(">I", zlib.crc32(kind + data) & 0xffffffff))
    with open(path, "wb") as f:
        f.write(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 2, 0, 0, 0))
                + chunk(b"IDAT", zlib.compress(rows)) + chunk(b"IEND", b""))


tmp = tempfile.mkdtemp()
art = os.path.join(tmp, "cover.png")
png(art)
log_path = os.path.join(tmp, "calls.log")
open(log_path, "w").close()
script = os.path.join(tmp, "player.py")
with open(script, "w") as f:
    f.write(PLAYER)

apps = os.path.expanduser("~/.local/share/applications")
os.makedirs(apps, exist_ok=True)
with open(f"{apps}/{PLAYER_ID}.desktop", "w") as f:
    f.write("[Desktop Entry]\nType=Application\nName=Music\nIcon=audio-x-generic\nExec=true\n")
time.sleep(1.5)

# --- no players -------------------------------------------------------------

s.panel("Activate", "clock-button", settle=1.0)
check("media-card" not in names(), "no card without players")
s.shot("01-no-players")
s.panel("CloseMenus")

# --- a player appears -------------------------------------------------------

player = subprocess.Popen([sys.executable, script, art, log_path], stdout=subprocess.PIPE, text=True)
check(player.stdout.readline().strip() == "player ready", "fake player owns its name")
check(wait_for(lambda: "media-card" in names()), "card appears for the player")
check(wait_for(lambda: {"media-previous", "media-play-pause", "media-next", "media-art",
                        "media-status-paused"} <= names()),
      "card has controls, album art and paused state")

s.panel("Activate", "clock-button", settle=1.0)
s.shot("02-date-menu")

s.panel("Activate", "media-play-pause", settle=0)
check(wait_for(lambda: "PlayPause" in calls()), "play/pause button calls PlayPause")
check(wait_for(lambda: "media-status-playing" in names()), "card shows playing state")
s.shot("03-playing")

s.panel("Activate", "media-next", settle=0)
check(wait_for(lambda: "Next" in calls()), "next button calls Next")
check(wait_for(lambda: "media-art-fallback" in names()), "card follows the new track (no art)")
s.shot("04-next-track")

s.panel("Activate", "media-card", settle=0)
check(wait_for(lambda: "Raise" in calls()), "clicking the card calls Raise")
s.panel("CloseMenus")

# Dark style
s.gsettings("org.gnome.desktop.interface", "color-scheme", "prefer-dark")
s.panel("Activate", "clock-button", settle=1.0)
s.shot("05-date-menu-dark")
s.panel("CloseMenus")
s.gsettings("org.gnome.desktop.interface", "color-scheme", "default")

# --- the player vanishes ----------------------------------------------------

player.kill()
player.wait()
check(wait_for(lambda: "media-card" not in names()), "card goes with the player")
s.panel("Activate", "clock-button", settle=1.0)
s.shot("06-player-gone")
s.panel("CloseMenus")

print("calls:", calls(), flush=True)
print("FAILURES:", failures if failures else "none", flush=True)
