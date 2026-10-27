"""polkit authentication: the panel's authentication agent and its system
dialog, against the mocked polkitd (dev/mock_polkit.py) and the fake helper
(dev/fake_polkit_helper.py, which accepts "hunter2").

Requests come from polkitd's bus name (the only caller the agent accepts)
through the mock's test interface. Passwords are typed with real
RemoteDesktop key events, so the dialog must have keyboard focus. The
scenario also adds users (alice in wheel, bob), a small AccountsService
mock for alice's real name and avatar, a helper socket like
polkit-agent-helper.socket, and logind, to check re-registration when
polkitd restarts.
"""
import json
import os
import socket
import subprocess
import sys
import threading
import time

import dbus
from dbusmock import BusType, SpawnedMock

sys.path.insert(0, "/src/dev")
from driver import Session  # noqa: E402

import gi  # noqa: E402

gi.require_version("GdkPixbuf", "2.0")
from gi.repository import GdkPixbuf, GLib  # noqa: E402

POLKIT = "org.freedesktop.PolicyKit1"
AUTHORITY_PATH = "/org/freedesktop/PolicyKit1/Authority"
AGENT_PATH = "/org/freedesktop/PolicyKit1/AuthenticationAgent"
AGENT_IFACE = "org.freedesktop.PolicyKit1.AuthenticationAgent"
ERRORS = "org.freedesktop.PolicyKit1.Error."
SORRY = "Sorry, that didn’t work. Please try again."
MESSAGE = "Authentication is required to change the system time."
RUNTIME = os.environ["XDG_RUNTIME_DIR"]
HELPER_LOG = os.path.join(RUNTIME, "fake-polkit-helper.log")
HELPER_SOCKET = os.path.join(RUNTIME, "polkit-agent-helper.socket")

bus = dbus.SystemBus()
authority = dbus.Interface(bus.get_object(POLKIT, AUTHORITY_PATH), "org.gnome.ShellPlus.MockAuthority")

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


# --- keyboard ------------------------------------------------------------

KEY_ESC, KEY_BACKSPACE, KEY_ENTER, KEY_LEFTSHIFT = 1, 14, 28, 42
KEYCODES = {c: code for code, c in enumerate("1234567890", start=2)}
for row, start in (("qwertyuiop", 16), ("asdfghjkl", 30), ("zxcvbnm", 44)):
    KEYCODES.update({c: code for code, c in enumerate(row, start=start)})


def keycodes(*codes, settle=0.05):
    """Presses the keys in order, then releases them in reverse."""
    rd_name, rd, _ = s._remote_desktop()
    iface = rd_name + ".Session"
    for code in codes:
        s._call(rd_name, rd, iface, "NotifyKeyboardKeycode", GLib.Variant("(ub)", (code, True)))
        time.sleep(0.02)
    for code in reversed(codes):
        s._call(rd_name, rd, iface, "NotifyKeyboardKeycode", GLib.Variant("(ub)", (code, False)))
        time.sleep(0.02)
    time.sleep(settle)


def type_text(text):
    for c in text:
        keycodes(KEYCODES[c])
    time.sleep(0.3)


# --- panel, agent and helper ---------------------------------------------

def inspect(name):
    try:
        return s.panel("Inspect", name, settle=0)[0]
    except GLib.Error:
        return None


def dialog_shown():
    state = inspect("polkit")
    return bool(state and state["mapped"])


def text(name):
    return (inspect(name) or {}).get("text")


def ready():
    """The helper asked for the password (the entry then takes the focus;
    a PasswordEntry's focus is on its inner text widget, which has no name)."""
    if (inspect("polkit-password") or {}).get("sensitive"):
        time.sleep(0.2)
        return True
    return False


def user(uid):
    return dbus.Struct(("unix-user", dbus.Dictionary({"uid": dbus.UInt32(uid)}, signature="sv")),
                       signature="sa{sv}")


def group(gid):
    return dbus.Struct(("unix-group", dbus.Dictionary({"gid": dbus.UInt32(gid)}, signature="sv")),
                       signature="sa{sv}")


def begin(identities, message=MESSAGE, action="org.freedesktop.timedate1.set-time"):
    return authority.BeginAuthentication(action, message, dbus.Dictionary({}, signature="ss"),
                                         dbus.Array(identities, signature="(sa{sv})"))


def reply(auth_id, timeout=5.0):
    result = {}

    def done():
        raw = authority.GetReply(dbus.UInt32(auth_id))
        if raw:
            result.update(json.loads(raw))
        return bool(raw)

    wait_for(done, timeout)
    return result or None


def pending(auth_id):
    return authority.GetReply(dbus.UInt32(auth_id)) == ""


def helper_runs():
    try:
        with open(HELPER_LOG) as f:
            return [json.loads(line) for line in f]
    except FileNotFoundError:
        return []


def agents():
    return json.loads(authority.GetAgents())


def uid_of(name):
    return int(subprocess.run(["id", "-u", name], capture_output=True, text=True, check=True).stdout)


# --- users and AccountsService -------------------------------------------

subprocess.run(["useradd", "-M", "-c", "alice gecos", "-G", "wheel", "alice"], check=True)
subprocess.run(["useradd", "-M", "-c", "Bob Builder,,,", "bob"], check=True)
ALICE, BOB = uid_of("alice"), uid_of("bob")

avatar_path = os.path.join(RUNTIME, "alice.png")
pixbuf = GdkPixbuf.Pixbuf.new(GdkPixbuf.Colorspace.RGB, True, 8, 96, 96)
pixbuf.fill(0xe66100ff)
pixbuf.savev(avatar_path, "png", [], [])

accounts = SpawnedMock.spawn_for_name("org.freedesktop.Accounts", "/org/freedesktop/Accounts",
                                      "org.freedesktop.Accounts", BusType.SYSTEM)
accounts.obj.AddMethod(
    "org.freedesktop.Accounts", "FindUserById", "x", "o",
    f"if args[0] != {ALICE}:\n"
    "    raise dbus.exceptions.DBusException('No such user', name='org.freedesktop.Accounts.Error.Failed')\n"
    f"ret = dbus.ObjectPath('/org/freedesktop/Accounts/User{ALICE}')",
    dbus_interface="org.freedesktop.DBus.Mock")
accounts.obj.AddObject(f"/org/freedesktop/Accounts/User{ALICE}", "org.freedesktop.Accounts.User", {
    "UserName": "alice", "RealName": "Alice Liddell", "IconFile": avatar_path,
    "AccountType": dbus.Int32(1), "Uid": dbus.UInt64(ALICE),
}, [], dbus_interface="org.freedesktop.DBus.Mock")

# --- registration --------------------------------------------------------

check(wait_for(lambda: bool(agents()), 10), "agent registered with the Authority")
registered = agents()
print("agents:", registered)
agent = registered[-1]
check(agent["path"] == AGENT_PATH, f"agent object path ({agent['path']})")
check(agent["subject"] == ["unix-session", {"session-id": "shellplus-test"}],
      f"unix-session subject from XDG_SESSION_ID ({agent['subject']})")
check(bool(agent["locale"]), f"locale ({agent['locale']})")

# --- only polkitd may call the agent -------------------------------------

for method, signature, args in [
    ("CancelAuthentication", "s", ("nope",)),
    ("BeginAuthentication", "sssa{ss}sa(sa{sv})",
     ("x", "evil", "", dbus.Dictionary({}, signature="ss"), "cookie", dbus.Array([user(0)], signature="(sa{sv})"))),
]:
    try:
        bus.call_blocking(agent["sender"], AGENT_PATH, AGENT_IFACE, method, signature, args)
        check(False, f"{method} from another client is rejected")
    except dbus.DBusException as err:
        check(err.get_dbus_name() == ERRORS + "NotAuthorized",
              f"{method} from another client is rejected ({err.get_dbus_name()})")
check(not dialog_shown(), "no dialog for a rejected caller")

# --- one identity: dialog, wrong password, right password ----------------

first = begin([user(0)])
check(wait_for(dialog_shown), "BeginAuthentication shows the dialog")
check(wait_for(ready), "password entry ready and focused")
time.sleep(0.4)
s.shot("01-dialog")
check(text("polkit-heading") == "Authentication Required", "heading")
check(text("polkit-body") == MESSAGE, "the action's message")
check(text("polkit-user-name") == "Super User", f"real name from passwd ({text('polkit-user-name')})")
check((inspect("polkit-user-name") or {}).get("visible") is True, "name shown (single identity)")
check(inspect("polkit-user-picker") is None, "no user picker for one identity")
cookie = authority.GetCookie(dbus.UInt32(first))

keycodes(KEY_LEFTSHIFT)  # the first injected key can get lost
type_text("wrongpw")
check(wait_for(lambda: text("polkit-password") == "wrongpw"), f"typed into the entry ({text('polkit-password')!r})")
keycodes(KEY_ENTER)
check(wait_for(lambda: text("polkit-message") == SORRY), f"wrong password shows the error ({text('polkit-message')!r})")
check(dialog_shown() and pending(first), "dialog stays open, request pending")
check(wait_for(ready), "ready for another try")
check(text("polkit-password") == "", "entry cleared for the retry")
time.sleep(0.3)
s.shot("02-wrong-password")
runs = helper_runs()
check(len(runs) == 1 and runs[0]["mode"] == "setuid" and runs[0]["argv"] == ["root"],
      f"helper run with the user name as its only argument ({runs[:1]})")
check(bool(runs) and runs[0]["cookie"] == cookie, "cookie passed on stdin")
check(bool(runs) and runs[0]["answer"] == "wrongpw" and runs[0]["result"] == "failure", "helper got the answer")

# A PAM error message (escaped by the helper) replaces the generic text.
type_text("locked")
keycodes(KEY_ENTER)
locked = "Your account is locked.\nAsk an administrator."
check(wait_for(lambda: text("polkit-message") == locked), f"PAM_ERROR_MSG shown ({text('polkit-message')!r})")
check(wait_for(ready), "ready after the PAM error")

type_text("hunter2")
keycodes(KEY_ENTER)
result = reply(first)
print("reply:", result)
check(result == {"ok": True, "authorized": True}, f"right password: BeginAuthentication returns after the helper "
      f"answered polkitd ({result})")
check(wait_for(lambda: not dialog_shown()), "dialog closes after success")
responses = json.loads(authority.GetResponses())
check(any(r["cookie"] == cookie and r["identity"] == ["unix-user", {"uid": 0}] for r in responses),
      "helper's AuthenticationAgentResponse2 for root")

# --- Escape ----------------------------------------------------------------

second = begin([user(0)])
check(wait_for(dialog_shown) and wait_for(ready), "second request ready")
type_text("abc")
keycodes(KEY_ESC)
result = reply(second)
print("reply:", result)
check(result and result.get("error") == ERRORS + "Cancelled", f"Escape returns Cancelled ({result})")
check(wait_for(lambda: not dialog_shown()), "dialog closes on Escape")
second_cookie = authority.GetCookie(dbus.UInt32(second))
check(wait_for(lambda: any(r["cookie"] == second_cookie and r["result"] in ("terminated", "eof")
                           for r in helper_runs())), "helper stopped on Escape")

# --- CancelAuthentication ---------------------------------------------------

third = begin([user(0)])
check(wait_for(dialog_shown) and wait_for(ready), "third request ready")
authority.CancelAuthentication(dbus.UInt32(third))
check(wait_for(lambda: not dialog_shown()), "CancelAuthentication closes the dialog")
result = reply(third)
check(result and result.get("error") == ERRORS + "Cancelled", f"cancelled request returns Cancelled ({result})")

# --- several identities: user picker ------------------------------------

picked = begin([user(BOB), group(10)])  # wheel: alice
check(wait_for(dialog_shown) and wait_for(ready), "multi-identity request ready")
time.sleep(0.4)
check((inspect("polkit-user-picker") or {}).get("visible") is True, "user picker shown")
check(text("polkit-user-name") == "Alice Liddell",
      f"first admin preselected, real name from AccountsService ({text('polkit-user-name')})")
s.shot("03-user-picker")
type_text("hunter2")
keycodes(KEY_ENTER)
result = reply(picked)
check(result == {"ok": True, "authorized": True}, f"alice authenticated ({result})")
check(helper_runs()[-1]["user"] == "alice", f"helper ran for alice ({helper_runs()[-1]['user']})")
responses = json.loads(authority.GetResponses())
check(responses[-1]["identity"] == ["unix-user", {"uid": ALICE}], f"response for alice ({responses[-1]})")

current = begin([user(BOB), user(0), user(ALICE)])
check(wait_for(dialog_shown) and wait_for(ready), "request including the current user ready")
check(text("polkit-user-name") == "Super User", f"current user preselected ({text('polkit-user-name')})")
keycodes(KEY_ESC)
reply(current)

only_bob = begin([user(BOB)])
check(wait_for(dialog_shown) and wait_for(ready), "bob request ready")
check(text("polkit-user-name") == "Bob Builder", f"gecos real name ({text('polkit-user-name')})")
time.sleep(0.3)
s.shot("04-initials")
keycodes(KEY_ESC)
reply(only_bob)

# --- queued requests -----------------------------------------------------

queued = [begin([user(0)], message="First request"), begin([user(0)], message="Second request")]
check(wait_for(dialog_shown) and wait_for(ready), "first of two requests shown")
check(text("polkit-body") == "First request", "first request first")
keycodes(KEY_ESC)
check(wait_for(lambda: dialog_shown() and text("polkit-body") == "Second request"), "second shown after the first")
check(wait_for(ready), "second request ready")
type_text("hunter2")
keycodes(KEY_ENTER)
results = [reply(r) for r in queued]
check(results[0].get("error") == ERRORS + "Cancelled" and results[1] == {"ok": True, "authorized": True},
      f"queued replies ({results})")

# A queued request can be cancelled before it's shown.
queued = [begin([user(0)], message="Shown"), begin([user(0)], message="Never shown")]
check(wait_for(dialog_shown), "shown request")
authority.CancelAuthentication(dbus.UInt32(queued[1]))
result = reply(queued[1])
check(result and result.get("error") == ERRORS + "Cancelled", f"queued request cancelled ({result})")
keycodes(KEY_ESC)
reply(queued[0])
check(wait_for(lambda: not dialog_shown()), "nothing left on screen")

# --- the socket-activated helper ------------------------------------------

server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
server.bind(HELPER_SOCKET)
server.listen(4)


def serve():
    while True:
        try:
            conn, _ = server.accept()
        except OSError:
            return
        subprocess.Popen(["/src/dev/fake_polkit_helper.py", "--socket-activated"],
                         stdin=conn.fileno(), stdout=conn.fileno())
        conn.close()


threading.Thread(target=serve, daemon=True).start()
via_socket = begin([user(0)])
check(wait_for(dialog_shown) and wait_for(ready), "socket request ready")
type_text("hunter2")
keycodes(KEY_ENTER)
result = reply(via_socket)
run = helper_runs()[-1]
check(result == {"ok": True, "authorized": True}, f"authenticated through the helper socket ({result})")
check(run["mode"] == "socket" and run["user"] == "root" and run["cookie"] == authority.GetCookie(dbus.UInt32(via_socket)),
      f"socket helper got the user name and cookie on its input ({run})")
server.close()
os.unlink(HELPER_SOCKET)

# --- polkitd restarts (now with logind) ----------------------------------

logind = SpawnedMock.spawn_with_template("logind", {}, bustype=BusType.SYSTEM)
logind.obj.AddSession("c7", "seat0", dbus.UInt32(0), "root", True, dbus_interface="org.freedesktop.DBus.Mock")
logind.obj.AddMethod("org.freedesktop.login1.Manager", "GetSessionByPID", "u", "o",
                     "ret = dbus.ObjectPath('/org/freedesktop/login1/session/c7')",
                     dbus_interface="org.freedesktop.DBus.Mock")

during = begin([user(0)])
check(wait_for(dialog_shown), "request before the restart shown")
authority.Restart()
check(wait_for(lambda: not dialog_shown()), "polkitd going away closes the dialog")
check(wait_for(lambda: bool(agents()), 10), "agent registers again after polkitd restarts")
registered = agents()
print("agents after restart:", registered)
check(registered and registered[-1]["subject"] == ["unix-session", {"session-id": "c7"}],
      f"session id from logind GetSessionByPID ({registered and registered[-1]['subject']})")
after = begin([user(0)])
check(wait_for(dialog_shown) and wait_for(ready), "works after the restart")
keycodes(KEY_ESC)
reply(after)
logind.terminate()

# --- dark style ----------------------------------------------------------

s.gsettings("org.gnome.desktop.interface", "color-scheme", "prefer-dark")
dark = begin([user(BOB), group(10)])
check(wait_for(dialog_shown) and wait_for(ready), "dark: dialog shown")
time.sleep(0.4)
s.shot("05-dark")
type_text("wrongpw")
keycodes(KEY_ENTER)
check(wait_for(lambda: text("polkit-message") == SORRY), "dark: error shown")
wait_for(ready)
time.sleep(0.3)
s.shot("06-dark-error")
keycodes(KEY_ESC)
reply(dark)
s.gsettings("org.gnome.desktop.interface", "color-scheme", "default")

accounts.terminate()
print("FAILURES:", ", ".join(failures) if failures else "none")
