"""Wi-Fi password prompt: the panel's NetworkManager secret agent and its
system dialog, against the mocked NetworkManager (dev/mock_networkmanager.py).

Requests come from NetworkManager's bus name (the only caller the agent
accepts), either through the mock's test interface or, at the end, from
connecting to a new secured network in the Wi-Fi menu. Keys are typed with
real RemoteDesktop key events, so the dialog must have keyboard focus.
"""
import json
import sys
import time

import dbus

sys.path.insert(0, "/src/dev")
from driver import Session  # noqa: E402

from gi.repository import GLib  # noqa: E402

NM = "org.freedesktop.NetworkManager"
NM_PATH = "/org/freedesktop/NetworkManager"
WIFI = NM_PATH + "/Devices/mock_WiFi"
ERRORS = "org.freedesktop.NetworkManager.SecretAgent."
SECURITY = "802-11-wireless-security"

bus = dbus.SystemBus()
manager = dbus.Interface(bus.get_object(NM, NM_PATH + "/AgentManager"),
                         "org.gnome.ShellPlus.MockAgentManager")

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
        if c.isupper():
            keycodes(KEY_LEFTSHIFT, KEYCODES[c.lower()])
        else:
            keycodes(KEYCODES[c])
    time.sleep(0.3)


# --- panel and agent -----------------------------------------------------

def inspect(name):
    """The widget's state from the panel's debug interface, or None."""
    try:
        return s.panel("Inspect", name, settle=0)[0]
    except GLib.Error:
        return None


def dialog_shown():
    state = inspect("network-secrets")
    return bool(state and state["mapped"])


def entry_text(name="network-secrets-entry"):
    return (inspect(name) or {}).get("text")


def connect_sensitive():
    return (inspect("network-secrets-connect") or {}).get("sensitive")


def wifi_connection(ssid, key_mgmt="wpa-psk", security=None, extra=None):
    connection = {
        "connection": dbus.Dictionary({"id": ssid, "type": "802-11-wireless",
                                       "uuid": "c0ffee00-0000-4000-8000-%012d" % len(ssid)},
                                      signature="sv"),
        "802-11-wireless": dbus.Dictionary({"ssid": dbus.ByteArray(ssid.encode()),
                                            "mode": "infrastructure",
                                            "security": SECURITY}, signature="sv"),
        SECURITY: dbus.Dictionary({"key-mgmt": key_mgmt, **(security or {})}, signature="sv"),
    }
    connection.update(extra or {})
    return dbus.Dictionary(connection, signature="sa{sv}")


def request(connection, setting=SECURITY, flags=1, path=NM_PATH + "/Settings/test"):
    return manager.RequestSecrets(connection, dbus.ObjectPath(path), setting,
                                  dbus.Array([], signature="s"), dbus.UInt32(flags))


def reply(request_id, timeout=5.0):
    result = {}

    def done():
        text = manager.GetReply(dbus.UInt32(request_id))
        if text:
            result.update(json.loads(text))
        return bool(text)

    wait_for(done, timeout)
    return result or None


# --- registration --------------------------------------------------------

check(wait_for(lambda: bool(manager.GetAgents()), 10), "agent registered with AgentManager")
agents = dict(manager.GetAgents())
print("agents:", agents)
check(list(agents.values()) == ["org.gnome.ShellPlus.NetworkAgent"], "agent identifier")

# Only NetworkManager may call the agent.
try:
    bus.call_blocking(next(iter(agents)), "/org/freedesktop/NetworkManager/SecretAgent",
                      "org.freedesktop.NetworkManager.SecretAgent", "CancelGetSecrets", "os",
                      (dbus.ObjectPath("/"), SECURITY))
    check(False, "calls from other clients are rejected")
except dbus.DBusException as err:
    check(err.get_dbus_name() == ERRORS + "PermissionDenied",
          f"calls from other clients are rejected ({err.get_dbus_name()})")

# --- WPA-PSK: short key, valid key, Enter --------------------------------

first = request(wifi_connection("Office"))
check(wait_for(dialog_shown), "GetSecrets shows the dialog")
time.sleep(0.5)
s.shot("01-dialog")
check(inspect("network-secrets-heading")["text"] == "Authentication required", "heading")
check(inspect("network-secrets-body")["text"] ==
      "Passwords or encryption keys are required to access the wireless network “Office”.",
      "description names the SSID")
check(connect_sensitive() is False, "Connect disabled while empty")

keycodes(KEY_LEFTSHIFT)  # the first injected key can get lost
type_text("short")
check(wait_for(lambda: entry_text() == "short"), f"typed into the entry (got {entry_text()!r})")
check(connect_sensitive() is False, "Connect disabled for a 5-character key")
keycodes(KEY_ENTER)
time.sleep(0.8)
check(manager.GetReply(dbus.UInt32(first)) == "" and dialog_shown(), "Enter ignored while the key is invalid")
s.shot("02-too-short")

type_text("Battery9")
check(wait_for(lambda: entry_text() == "shortBattery9"), f"typed more (got {entry_text()!r})")
check(connect_sensitive() is True, "Connect enabled for a 13-character key")
s.shot("03-valid")

keycodes(KEY_ENTER)
result = reply(first)
print("reply:", result)
check(result == {"secrets": {SECURITY: {"psk": "shortBattery9"}}}, "Enter returns the psk")
check(wait_for(lambda: not dialog_shown()), "dialog closes after Connect")

# --- Escape --------------------------------------------------------------

second = request(wifi_connection("Neighbour 5G", "sae"))
check(wait_for(dialog_shown), "second request shows the dialog")
time.sleep(0.4)
type_text("secret")
keycodes(KEY_ESC)
result = reply(second)
print("reply:", result)
check(result == {"error": ERRORS + "UserCanceled"}, "Escape returns UserCanceled")
check(wait_for(lambda: not dialog_shown()), "dialog closes on Escape")

# --- CancelGetSecrets ----------------------------------------------------

path = NM_PATH + "/Settings/cancel_me"
third = request(wifi_connection("Office"), path=path)
check(wait_for(dialog_shown), "third request shows the dialog")
manager.CancelSecrets(dbus.ObjectPath(path), SECURITY)
check(wait_for(lambda: not dialog_shown()), "CancelGetSecrets closes the dialog")
result = reply(third)
print("reply:", result)
check(result == {"error": ERRORS + "AgentCanceled"}, "cancelled request returns AgentCanceled")

# --- no interaction allowed / unsupported --------------------------------

result = reply(request(wifi_connection("Office"), flags=0))
check(result == {"error": ERRORS + "NoSecrets"}, f"no interaction allowed -> NoSecrets ({result})")
check(not dialog_shown(), "no dialog without interaction")

tls = wifi_connection("Corp", "wpa-eap", extra={
    "802-1x": dbus.Dictionary({"eap": dbus.Array(["tls"], signature="s")}, signature="sv")})
result = reply(request(tls, setting="802-1x"))
check(result == {"error": ERRORS + "NoSecrets"}, f"EAP-TLS rejected cleanly ({result})")

# --- 64 hex digits -------------------------------------------------------

hex_request = request(wifi_connection("Office"))
check(wait_for(dialog_shown), "hex request shows the dialog")
time.sleep(0.4)
type_text("0123456789abcdef" * 4)
check(wait_for(lambda: len(entry_text() or "") == 64), "typed 64 characters")
check(connect_sensitive() is True, "Connect enabled for 64 hex digits")
keycodes(KEY_BACKSPACE)
type_text("g")
check(connect_sensitive() is False, "Connect disabled for 64 characters that aren't hex")
keycodes(KEY_ESC)
reply(hex_request)

# --- WEP -----------------------------------------------------------------

wep = request(wifi_connection("Old Router", "none", {"wep-key-type": dbus.UInt32(1)}))
check(wait_for(dialog_shown), "WEP request shows the dialog")
time.sleep(0.4)
type_text("abcd")
check(connect_sensitive() is False, "WEP: 4 characters invalid")
type_text("e")
check(connect_sensitive() is True, "WEP: 5 characters valid")
s.shot("04-wep")
keycodes(KEY_ENTER)
result = reply(wep)
check(result == {"secrets": {SECURITY: {"wep-key0": "abcde"}}}, f"WEP returns wep-key0 ({result})")

# --- 802.1X (PEAP): username + password ----------------------------------

peap = wifi_connection("Corp", "wpa-eap", extra={
    "802-1x": dbus.Dictionary({"eap": dbus.Array(["peap"], signature="s"),
                               "identity": "alice"}, signature="sv")})
eap = request(peap, setting="802-1x")
check(wait_for(dialog_shown), "802.1X request shows the dialog")
time.sleep(0.4)
check(entry_text("network-secrets-identity") == "alice", "username prefilled")
type_text("wonderland")
s.shot("05-8021x")
keycodes(KEY_ENTER)
result = reply(eap)
check(result == {"secrets": {"802-1x": {"identity": "alice", "password": "wonderland"}}},
      f"802.1X returns identity and password ({result})")

# --- queued requests -----------------------------------------------------

queued = [request(wifi_connection("Office")), request(wifi_connection("Neighbour 5G"))]
check(wait_for(dialog_shown), "first of two requests shown")
time.sleep(0.4)
check("“Office”" in inspect("network-secrets-body")["text"], "first request first")
keycodes(KEY_ESC)
check(wait_for(lambda: dialog_shown() and "Neighbour" in inspect("network-secrets-body")["text"]),
      "second request shown after the first")
time.sleep(0.4)
type_text("password1")
keycodes(KEY_ENTER)
results = [reply(r) for r in queued]
check(results == [{"error": ERRORS + "UserCanceled"},
                  {"secrets": {SECURITY: {"psk": "password1"}}}], f"queued replies ({results})")

# --- dark style ----------------------------------------------------------

s.gsettings("org.gnome.desktop.interface", "color-scheme", "prefer-dark")
dark = request(wifi_connection("Office"))
check(wait_for(dialog_shown), "dark: dialog shown")
time.sleep(0.4)
type_text("short")
s.shot("06-dark-invalid")
type_text("Battery9")
s.shot("07-dark-valid")
keycodes(KEY_ESC)
reply(dark)
s.gsettings("org.gnome.desktop.interface", "color-scheme", "default")

# --- end to end: a new secured network from the Wi-Fi menu ---------------

s.panel("Activate", "qs-button")
s.panel("Activate", "qs-wifi-arrow")
# Rows: Home Network (connected), Café Guest, Office, Neighbour 5G.
s.panel("Activate", "qs-wifi-net-2")
check(wait_for(dialog_shown), "connecting to a new secured network asks for the password")
s.panel("CloseMenus")
time.sleep(0.4)
check("“Office”" in inspect("network-secrets-body")["text"], "for the network that was clicked")
device = bus.get_object(NM, WIFI)
state = device.Get(NM + ".Device", "State", dbus_interface=dbus.PROPERTIES_IFACE)
check(state == 60, f"device waits in NEED_AUTH ({state})")
s.shot("08-from-menu")
type_text("officepass")
keycodes(KEY_ENTER)
check(wait_for(lambda: device.Get(NM + ".Device", "State", dbus_interface=dbus.PROPERTIES_IFACE) == 100),
      "device activated after the password")
active = device.Get(NM + ".Device", "ActiveConnection", dbus_interface=dbus.PROPERTIES_IFACE)
active_id = bus.get_object(NM, active).Get(NM + ".Connection.Active", "Id",
                                           dbus_interface=dbus.PROPERTIES_IFACE)
check(active_id == "Office", f"connected to Office ({active_id})")
time.sleep(0.5)
s.panel("Activate", "qs-button")
s.panel("Activate", "qs-wifi-arrow")
s.shot("09-connected")
s.panel("CloseMenus")

print("FAILURES:", ", ".join(failures) if failures else "none")
