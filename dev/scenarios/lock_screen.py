"""The lock screen: the compositor's shield and lock state
(compositor/src/shellplus-lock.c) and the panel's lock UI
(panel/src/lock_screen/).

Passwords are checked for real: root gets the password "hunter2" and the
gnome-shell-plus PAM service from data/pam is installed, so the panel's
PAM path (pam_unix) runs unchanged. A fake GDM (system bus manager plus a
peer-to-peer UserVerifier) covers the GDM reauthentication path at the
end. Passwords are typed with real RemoteDesktop key events.

Checks: Lock() shows the curtain and hides a test window; Super, Alt+Tab
and overview requests do nothing; a key shows the prompt; a wrong password
fails and stays locked; Unlock from another D-Bus client and
SetActive(false) are refused; killing the panel leaves the screen black and
the respawned panel shows the lock screen again; the right password
unlocks (ActiveChanged(false)); media keys grabbed for the lock screen
still work, others don't; logind's Lock and PrepareForSleep lock (with a
delay inhibitor); the quick settings Lock button; GDM; idle locking.
"""
import os
import shutil
import subprocess
import sys
import tempfile
import time

import dbus
import dbusmock
from dbusmock import BusType, SpawnedMock

sys.path.insert(0, "/src/dev")
from driver import Session  # noqa: E402

import gi  # noqa: E402

gi.require_version("GdkPixbuf", "2.0")
from gi.repository import GdkPixbuf, Gio, GLib  # noqa: E402

PASSWORD = "hunter2"
GDM_PASSWORD = "gdmsecret"
SORRY = "Sorry, that didn’t work. Please try again."
OUT = sys.argv[1]
LOG = os.path.join(OUT, "compositor.log")
SS = ("org.gnome.ScreenSaver", "/org/gnome/ScreenSaver", "org.gnome.ScreenSaver")
SHELL = ("org.gnome.ShellPlus", "/org/gnome/ShellPlus")

s = Session(OUT)
failures = []


def check(ok, what):
    print(("PASS " if ok else "FAIL ") + what, flush=True)
    if not ok:
        failures.append(what)


def pump():
    ctx = GLib.MainContext.default()
    while ctx.iteration(False):
        pass


def wait_for(predicate, timeout=6.0):
    deadline = time.time() + timeout
    while time.time() < deadline:
        pump()
        if predicate():
            return True
        time.sleep(0.1)
    pump()
    return predicate()


# --- setup: password, PAM service, signals, a test window -----------------

subprocess.run(["chpasswd"], input=f"root:{PASSWORD}\n", text=True, check=True)
shutil.copy("/src/data/pam/gnome-shell-plus", "/etc/pam.d/gnome-shell-plus")

active_changes = []
s.bus.signal_subscribe(None, SS[2], "ActiveChanged", SS[1], None, Gio.DBusSignalFlags.NONE,
                       lambda *a: active_changes.append(a[5][0]))
accelerators = []
s.bus.signal_subscribe(None, "org.gnome.Shell", "AcceleratorActivated", "/org/gnome/Shell", None,
                       Gio.DBusSignalFlags.NONE, lambda *a: accelerators.append(a[5][0]))


def ss(method, args=None, reply=None):
    return s._call(SS[0], SS[1], SS[2], method, args, reply)


def get_active():
    return ss("GetActive", reply="(b)")[0]


def locked_state():
    return bool(s.state().get("locked"))


def lock_shown():
    state = s.inspect("lock-screen")
    return bool(state and state["mapped"])


def prompt_shown():
    state = s.inspect("lock-prompt")
    return bool(state and state["mapped"])


TEST_WINDOW = '''
import sys
import gi
gi.require_version("Gtk", "4.0")
from gi.repository import Gdk, Gio, Gtk
css = Gtk.CssProvider()
css.load_from_string("window.test { background: #ff00ff; }")
def on_activate(app):
    Gtk.StyleContext.add_provider_for_display(Gdk.Display.get_default(), css, 800)
    w = Gtk.ApplicationWindow(application=app, title="Magenta", default_width=600, default_height=400)
    w.add_css_class("test")
    w.present()
app = Gtk.Application(application_id="org.example.Magenta", flags=Gio.ApplicationFlags.NON_UNIQUE)
app.connect("activate", on_activate)
app.run([])
'''
script = tempfile.mktemp(suffix=".py")
with open(script, "w") as f:
    f.write(TEST_WINDOW)
s.apps.append(subprocess.Popen([sys.executable, script], stdout=subprocess.DEVNULL,
                               stderr=subprocess.DEVNULL))


def window_rect():
    windows = s._call("org.gnome.ShellPlus", "/org/gnome/ShellPlus", "org.gnome.ShellPlus.Overview",
                      "ListWindows", reply="(a(ta{sv}))")[0]
    for _, props in windows:
        if props.get("title") == "Magenta":
            return props["rect"]
    return None


wait_for(lambda: (window_rect() or (0, 0, 0, 0))[2] > 100, 8)
time.sleep(1.0)
rect = window_rect()
print("test window at", rect, flush=True)


def pixbuf(path):
    return GdkPixbuf.Pixbuf.new_from_file(path)


def mean_color(path, r):
    pb = pixbuf(path)
    n, stride, data = pb.get_n_channels(), pb.get_rowstride(), pb.get_pixels()
    x0, y0, w, h = r
    total, count = [0, 0, 0], 0
    for y in range(y0 + h // 4, y0 + 3 * h // 4, 4):
        for x in range(x0 + w // 4, x0 + 3 * w // 4, 4):
            for c in range(3):
                total[c] += data[y * stride + x * n + c]
            count += 1
    return tuple(t // max(count, 1) for t in total)


def is_magenta(color):
    r, g, b = color
    return r > 200 and b > 200 and g < 60


def black_fraction(path):
    pb = pixbuf(path)
    n, stride, data = pb.get_n_channels(), pb.get_rowstride(), pb.get_pixels()
    black = total = 0
    for y in range(0, pb.get_height(), 5):
        for x in range(0, pb.get_width(), 5):
            i = y * stride + x * n
            total += 1
            if max(data[i:i + 3]) < 12:
                black += 1
    return black / total


def frame_difference(path_a, path_b):
    a, b = pixbuf(path_a), pixbuf(path_b)
    n, stride = a.get_n_channels(), a.get_rowstride()
    da, db = a.get_pixels(), b.get_pixels()
    total = count = 0
    # Leave out the clock, which may tick between the two frames.
    for y in range(0, a.get_height(), 4):
        for x in range(0, a.get_width(), 4):
            if 400 <= x < 880 and 100 <= y < 400:
                continue
            i = y * stride + x * n
            total += sum(abs(da[i + c] - db[i + c]) for c in range(3)) / 3
            count += 1
    return total / max(count, 1)


def record(action, before=1.0, after=1.0):
    """Records the primary monitor while running action(); returns the frames."""
    session = s._call("org.gnome.Mutter.ScreenCast", "/org/gnome/Mutter/ScreenCast",
                      "org.gnome.Mutter.ScreenCast", "CreateSession",
                      GLib.Variant("(a{sv})", ({},)), "(o)")[0]
    stream = s._call("org.gnome.Mutter.ScreenCast", session, "org.gnome.Mutter.ScreenCast.Session",
                     "RecordMonitor", GLib.Variant("(sa{sv})", ("", {})), "(o)")[0]
    node = {}
    sub = s.bus.signal_subscribe("org.gnome.Mutter.ScreenCast", "org.gnome.Mutter.ScreenCast.Stream",
                                 "PipeWireStreamAdded", stream, None, Gio.DBusSignalFlags.NONE,
                                 lambda *a: node.update(id=a[5][0]))
    s._call("org.gnome.Mutter.ScreenCast", session, "org.gnome.Mutter.ScreenCast.Session", "Start")
    wait_for(lambda: "id" in node, 10)
    s.bus.signal_unsubscribe(sub)
    tmp = tempfile.mkdtemp()
    gst = subprocess.Popen(["gst-launch-1.0", "-q", "pipewiresrc", f"path={node['id']}", "!",
                            "videoconvert", "!", "pngenc", "snapshot=false", "!",
                            "multifilesink", f"location={tmp}/frame%04d.png"])
    time.sleep(before)
    action()
    time.sleep(after)
    gst.terminate()
    gst.wait()
    s._call("org.gnome.Mutter.ScreenCast", session, "org.gnome.Mutter.ScreenCast.Session", "Stop")
    frames = []
    for name in sorted(os.listdir(tmp)):
        try:
            pixbuf(os.path.join(tmp, name))  # skip a half-written last frame
            frames.append(os.path.join(tmp, name))
        except GLib.Error:
            pass
    return frames


path = s.shot("00-desktop")
check(rect is not None and is_magenta(mean_color(path, rect)),
      f"test window visible before locking: {rect and mean_color(path, rect)}")


# --- keyboard -------------------------------------------------------------

KEY_ESC, KEY_ENTER, KEY_TAB, KEY_LEFTALT, KEY_LEFTMETA = 1, 28, 15, 56, 125
KEYCODES = {c: code for code, c in enumerate("1234567890", start=2)}
for row, start in (("qwertyuiop", 16), ("asdfghjkl", 30), ("zxcvbnm", 44)):
    KEYCODES.update({c: code for code, c in enumerate(row, start=start)})


def keycode(code, pressed):
    rd_name, rd, _ = s._remote_desktop()
    s._call(rd_name, rd, rd_name + ".Session", "NotifyKeyboardKeycode",
            GLib.Variant("(ub)", (code, pressed)))
    time.sleep(0.03)


def tap(*codes, settle=0.3):
    for code in codes:
        keycode(code, True)
    for code in reversed(codes):
        keycode(code, False)
    time.sleep(settle)


def type_text(text):
    for c in text:
        tap(KEYCODES[c], settle=0.03)
    time.sleep(0.3)


def message():
    return (s.inspect("lock-message") or {}).get("text", "")


def password_ready():
    state = s.inspect("lock-password")
    return bool(state and state["mapped"] and state["sensitive"])


def unlock_with_password(password=PASSWORD):
    if not prompt_shown():
        tap(KEY_ENTER)
    wait_for(password_ready)
    type_text(password)
    tap(KEY_ENTER)
    return wait_for(lambda: not get_active(), 8)


# --- Lock() ---------------------------------------------------------------

for i in range(2):
    s._call("org.freedesktop.Notifications", "/org/freedesktop/Notifications",
            "org.freedesktop.Notifications", "Notify",
            GLib.Variant("(susssasa{sv}i)", ("Chat", 0, "", f"Secret message {i}", "Body", [], {}, -1)),
            "(u)")
time.sleep(0.5)
ss("Lock")
check(wait_for(lock_shown, 8), "Lock() shows the lock screen")
check(get_active(), "GetActive is true")
check(locked_state(), "compositor state says locked")
check(wait_for(lambda: True in active_changes), "ActiveChanged(true) emitted")
time.sleep(0.6)
curtain = s.shot("01-curtain")
check(rect is not None and not is_magenta(mean_color(curtain, rect)),
      f"test window hidden while locked: {rect and mean_color(curtain, rect)}")
clock = (s.inspect("lock-clock-time") or {}).get("text", "")
check(":" in clock, f"curtain shows the clock: {clock!r}")
listing = s.inspect("lock-notifications") or {}
check(bool(listing.get("mapped")), "curtain shows the notification summary")
active_time = ss("GetActiveTime", reply="(u)")[0]
check(active_time < 30, f"GetActiveTime sane: {active_time}")

# --- Super, Alt+Tab, overview requests -------------------------------------

tap(KEY_LEFTMETA, settle=1.0)
overview = s.inspect("overview")
check(not (overview and overview["mapped"]), "Super doesn't open the overview")
check(not prompt_shown(), "curtain still up (modifier keys alone don't lift it)")
s._call(*SHELL, "org.gnome.ShellPlus.Panel", "ToggleOverview")
time.sleep(1.0)
overview = s.inspect("overview")
check(not (overview and overview["mapped"]), "ToggleOverview does nothing while locked")
try:
    s._call(*SHELL, "org.gnome.ShellPlus.Overview", "GetPreview",
            GLib.Variant("(tii)", (1, 100, 100)))
    check(False, "window previews refused while locked")
except GLib.Error as e:
    check("locked" in e.message, f"window previews refused while locked ({e.message})")

keycode(KEY_LEFTALT, True)
tap(KEY_TAB, settle=0.8)
during = s.shot("02-alt-tab")
keycode(KEY_LEFTALT, False)
time.sleep(0.5)
diff = frame_difference(curtain, during)
check(diff < 3.0, f"Alt+Tab shows nothing over the lock screen (difference {diff:.2f})")
check(get_active(), "still locked after Super / Alt+Tab")
tap(KEY_ESC)  # back to the curtain (Tab raised the prompt, like any key)

# --- Media keys: only grabs meant for the lock screen work -----------------

XF86AudioMute, F12 = 0x1008FF12, 0xFFC9
grabs = s._call("org.gnome.Shell", "/org/gnome/Shell", "org.gnome.Shell", "GrabAccelerators",
                GLib.Variant("(a(suu))", ([("XF86AudioMute", 1 | 4 | 8, 0), ("F12", 1, 0)],)),
                "(au)")[0]
check(all(grabs), f"accelerators grabbed: {grabs}")
s.key(XF86AudioMute, settle=0.5)
s.key(F12, settle=0.5)
pump()
check(grabs[0] in accelerators, "a media key grabbed for the lock screen works while locked")
check(grabs[1] not in accelerators, "a key grabbed for normal mode only is blocked while locked")
s._call("org.gnome.Shell", "/org/gnome/Shell", "org.gnome.Shell", "UngrabAccelerators",
        GLib.Variant("(au)", (grabs,)), "(b)")

# --- A key shows the prompt -------------------------------------------------

tap(KEY_ENTER)
check(wait_for(prompt_shown), "a key press shows the unlock prompt")
check(wait_for(password_ready), "password entry ready")
time.sleep(0.5)
s.shot("03-prompt")
name = (s.inspect("lock-user-name") or {}).get("text", "")
check(bool(name), f"prompt shows the user's name: {name!r}")
check(wait_for(lambda: "Unlock: verifying with PAM" in open(LOG).read(), 2),
      "without GDM the PAM path is used")

# --- Wrong password ----------------------------------------------------------

type_text("wrongpassword")
tap(KEY_ENTER)
check(wait_for(lambda: message() == SORRY, 8), f"wrong password shows GNOME's message: {message()!r}")
check(get_active() and locked_state(), "wrong password: still locked")
time.sleep(0.5)
s.shot("04-wrong-password")
check(wait_for(password_ready), "can try again after a failure")
check((s.inspect("lock-password") or {}).get("text") == "", "entry cleared after a failure")

# --- Unlock from someone else ----------------------------------------------

try:
    s._call(*SHELL, "org.gnome.ShellPlus.Lock", "Unlock")
    check(False, "Unlock from a non-panel client is refused")
except GLib.Error as e:
    check("AccessDenied" in e.message or "panel" in e.message,
          f"Unlock from a non-panel client is refused ({e.message})")
ss("SetActive", GLib.Variant("(b)", (False,)))
time.sleep(0.5)
check(get_active() and locked_state(), "still locked after a foreign Unlock and SetActive(false)")
check(lock_shown(), "lock screen still shown")

# --- The panel dies -----------------------------------------------------------

# Record the screen across the panel's death and respawn: every frame must
# be either all black or the lock screen, never the desktop.
frames = record(lambda: subprocess.run(["pkill", "-9", "-f", "debug/gnome-shell-plus-panel"],
                                       check=False), before=1.5, after=0.8)
black = [f for f in frames if black_fraction(f) > 0.999]
leaked = [f for f in frames if is_magenta(mean_color(f, rect))]
check(len(frames) > 3, f"frames recorded across the panel's death: {len(frames)}")
check(bool(black), f"panel killed while locked: the whole screen goes black ({len(black)} black frames)")
check(not leaked, f"no frame shows the desktop while the panel is gone ({len(leaked)} leaked)")
if black:
    shutil.copy(black[-1], os.path.join(OUT, "05-panel-killed.png"))
check(get_active(), "still locked without the panel")
check(wait_for(lock_shown, 15), "the respawned panel shows the lock screen again")
time.sleep(1.0)
back = s.shot("06-panel-respawned")
check(black_fraction(back) < 0.5, "lock screen drawn again after the respawn")
check(not is_magenta(mean_color(back, rect)), "test window still hidden after the respawn")

# --- The right password --------------------------------------------------------

active_changes.clear()
check(unlock_with_password(), "the right password unlocks")
check(wait_for(lambda: False in active_changes), "ActiveChanged(false) emitted")
check(not locked_state(), "compositor state says unlocked")
check(wait_for(lambda: not lock_shown(), 3), "lock window gone")
time.sleep(0.8)
path = s.shot("07-unlocked")
check(is_magenta(mean_color(path, rect)), f"test window visible again: {mean_color(path, rect)}")

# --- logind -------------------------------------------------------------------

# dbusmock's logind, plus GetSession / GetSessionByPID from the start (the
# compositor looks its session up as soon as logind appears).
LOGIND_TEMPLATE = """
import dbus
from dbusmock.templates.logind import *  # noqa: F401,F403
from dbusmock.templates import logind as base


def load(mock, parameters):
    base.load(mock, parameters)
    mock.AddMethods(MAIN_IFACE, [
        (name, sig, "o", "ret = dbus.ObjectPath('/org/freedesktop/login1/session/c7')")
        for name, sig in (("GetSession", "s"), ("GetSessionByPID", "u"))])
"""
logind_template = tempfile.mktemp(suffix=".py")
with open(logind_template, "w") as f:
    f.write(LOGIND_TEMPLATE)
logind = SpawnedMock.spawn_with_template(logind_template, {}, bustype=BusType.SYSTEM)
logind.obj.AddSession("c7", "seat0", dbus.UInt32(0), "root", True, dbus_interface=dbusmock.MOCK_IFACE)
system = dbus.SystemBus()
session = system.get_object("org.freedesktop.login1", "/org/freedesktop/login1/session/c7")
manager = system.get_object("org.freedesktop.login1", "/org/freedesktop/login1")


def inhibitors():
    return [tuple(str(x) for x in i[:4])
            for i in manager.ListInhibitors(dbus_interface="org.freedesktop.login1.Manager")]


def sleep_inhibited():
    return any(i[0] == "sleep" and i[3] == "delay" for i in inhibitors())


check(wait_for(sleep_inhibited, 5), f"delay inhibitor for sleep taken: {inhibitors()}")
session.EmitSignal("org.freedesktop.login1.Session", "Lock", "", [], dbus_interface=dbusmock.MOCK_IFACE)
check(wait_for(get_active, 5) and wait_for(lock_shown, 8), "logind's Lock signal locks")
s.shot("08-logind-lock")
check(unlock_with_password(), "unlocked again")

manager.EmitSignal("org.freedesktop.login1.Manager", "PrepareForSleep", "b", [True],
                   dbus_interface=dbusmock.MOCK_IFACE)
check(wait_for(get_active, 5), "PrepareForSleep(true) locks")
check(wait_for(lambda: not sleep_inhibited(), 5), "inhibitor released once locked")
manager.EmitSignal("org.freedesktop.login1.Manager", "PrepareForSleep", "b", [False],
                   dbus_interface=dbusmock.MOCK_IFACE)
check(wait_for(sleep_inhibited, 5), "inhibitor taken again after resume")
check(wait_for(lock_shown, 8), "lock screen shown after resume")
check(unlock_with_password(), "unlocked after resume")
logind.terminate()

# --- Quick settings ---------------------------------------------------------------

s.panel("Activate", "qs-button")
lock_button = s.inspect("qs-lock")
check(bool(lock_button and lock_button["mapped"]), "quick settings has a Lock button")
s.shot("09-quick-settings")
s.panel("Activate", "qs-lock")
check(wait_for(get_active, 5) and wait_for(lock_shown, 8), "the quick settings Lock button locks")
check(unlock_with_password(), "unlocked after the quick settings lock")

# --- GDM's reauthentication channel -------------------------------------------------

FAKE_GDM = r'''
import sys
from gi.repository import Gio, GLib
PASSWORD, SOCKET, LOG = sys.argv[1:4]
VERIFIER = """<node><interface name="org.gnome.DisplayManager.UserVerifier">
<method name="BeginVerificationForUser"><arg type="s" direction="in"/><arg type="s" direction="in"/></method>
<method name="AnswerQuery"><arg type="s" direction="in"/><arg type="s" direction="in"/></method>
<method name="Cancel"/>
<signal name="SecretInfoQuery"><arg type="s"/><arg type="s"/></signal>
<signal name="Problem"><arg type="s"/><arg type="s"/></signal>
<signal name="VerificationComplete"><arg type="s"/></signal>
<signal name="VerificationFailed"><arg type="s"/></signal>
</interface></node>"""
MANAGER = """<node><interface name="org.gnome.DisplayManager.Manager">
<method name="OpenReauthenticationChannel"><arg type="s" direction="in"/><arg type="s" direction="out"/></method>
</interface></node>"""
verifier_info = Gio.DBusNodeInfo.new_for_xml(VERIFIER).interfaces[0]
manager_info = Gio.DBusNodeInfo.new_for_xml(MANAGER).interfaces[0]

def log(text):
    with open(LOG, "a") as f:
        f.write(text + "\n")

def on_verifier_call(conn, sender, path, iface, method, params, invocation):
    invocation.return_value(None)
    emit = lambda name, args: conn.emit_signal(None, path, iface, name, args)
    if method == "BeginVerificationForUser":
        log("begin %s %s" % tuple(params.unpack()))
        emit("SecretInfoQuery", GLib.Variant("(ss)", (params[0], "Password:")))
    elif method == "AnswerQuery":
        service, answer = params.unpack()
        if answer == PASSWORD:
            log("complete")
            emit("VerificationComplete", GLib.Variant("(s)", (service,)))
        else:
            log("failed")
            emit("VerificationFailed", GLib.Variant("(s)", (service,)))

connections = []

def on_new_connection(server, conn):
    connections.append(conn)  # or Python drops (and closes) it
    conn.register_object("/org/gnome/DisplayManager/Session", verifier_info, on_verifier_call)
    return True

guid = Gio.dbus_generate_guid()
server = Gio.DBusServer.new_sync("unix:path=" + SOCKET, Gio.DBusServerFlags.AUTHENTICATION_ALLOW_ANONYMOUS,
                                 guid, None, None)
server.connect("new-connection", on_new_connection)
server.start()

def on_manager_call(conn, sender, path, iface, method, params, invocation):
    log("open channel for %s" % params[0])
    invocation.return_value(GLib.Variant("(s)", (server.get_client_address(),)))

system = Gio.bus_get_sync(Gio.BusType.SYSTEM)
system.register_object("/org/gnome/DisplayManager/Manager", manager_info, on_manager_call)
Gio.bus_own_name_on_connection(system, "org.gnome.DisplayManager", 0, None, None)
log("ready")
GLib.MainLoop().run()
'''
gdm_script = tempfile.mktemp(suffix=".py")
gdm_log = tempfile.mktemp()
with open(gdm_script, "w") as f:
    f.write(FAKE_GDM)
gdm = subprocess.Popen([sys.executable, gdm_script, GDM_PASSWORD,
                        os.path.join(os.environ["XDG_RUNTIME_DIR"], "fake-gdm"), gdm_log])


def gdm_log_text():
    try:
        return open(gdm_log).read()
    except FileNotFoundError:
        return ""


wait_for(lambda: "ready" in gdm_log_text(), 5)
ss("Lock")
check(wait_for(lock_shown, 8), "locked for the GDM test")
time.sleep(1.0)  # until the compositor has mapped and focused it
tap(KEY_ENTER)
check(wait_for(lambda: "begin gdm-password root" in gdm_log_text(), 5),
      f"GDM: reauthentication channel opened, gdm-password verification started ({gdm_log_text().split()})")
check(wait_for(password_ready), "GDM: password entry ready")
type_text(PASSWORD)  # the PAM password is the wrong one here
tap(KEY_ENTER)
check(wait_for(lambda: message() == SORRY, 5), f"GDM: wrong password rejected: {message()!r}")
check(get_active(), "GDM: still locked after a wrong password")
check(unlock_with_password(GDM_PASSWORD), "GDM: the right password unlocks")
gdm.terminate()

# --- Idle -----------------------------------------------------------------------------

s.gsettings("org.gnome.desktop.interface", "enable-animations", "false")
s.gsettings("org.gnome.desktop.session", "idle-delay", "uint32 2")
check(wait_for(get_active, 8), "idle-delay locks the screen (lock-enabled, lock-delay 0)")
s.gsettings("org.gnome.desktop.session", "idle-delay", "uint32 0")
check(wait_for(lock_shown, 8), "lock screen shown after idle")
s.shot("10-idle-lock")
check(unlock_with_password(), "unlocked after idle lock")
s.gsettings("org.gnome.desktop.interface", "enable-animations", "true")

for app in s.apps:
    app.terminate()
print("FAILURES: " + (", ".join(failures) if failures else "none"), flush=True)
sys.exit(1 if failures else 0)
