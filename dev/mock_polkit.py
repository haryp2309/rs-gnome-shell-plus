"""python-dbusmock template: dbusmock's polkitd plus authentication agents,
used by dev/mocks.py.

Records the agents that RegisterAuthenticationAgent(WithOptions) (by their
unique bus name, like polkitd), and the AuthenticationAgentResponse(2)
calls the helper (dev/fake_polkit_helper.py) makes on success.

Scenarios drive the agent through polkitd's own bus name (the panel only
accepts calls from it) with a test interface on the Authority object,
org.gnome.ShellPlus.MockAuthority:

    GetAgents() -> s                JSON list of {sender, subject, locale, path, options}
    BeginAuthentication(s action_id, s message, a{ss} details,
                        a(sa{sv}) identities) -> u id
                                    calls the latest agent's BeginAuthentication
    CancelAuthentication(u id)      calls the agent's CancelAuthentication
    GetCookie(u id) -> s
    GetReply(u id) -> s             JSON, "" while pending: {"ok": true,
                                    "authorized": <helper responded first>}
                                    or {"error": name}
    GetResponses() -> s             JSON list of {uid, cookie, identity}
    Restart()                       drops the bus name and the agents and takes
                                    the name again a moment later, like a
                                    polkitd restart
"""
import json
import uuid

import dbus
import dbus.service
from dbusmock.templates import polkitd
from dbusmock.templates.polkitd import *  # noqa: F401,F403 (BUS_NAME, MAIN_OBJ, CheckAuthorization, ...)
from gi.repository import GLib

BUS_NAME = polkitd.BUS_NAME
MAIN_OBJ = polkitd.MAIN_OBJ
MAIN_IFACE = polkitd.MAIN_IFACE
SYSTEM_BUS = True

TEST_IFACE = "org.gnome.ShellPlus.MockAuthority"
AGENT_IFACE = "org.freedesktop.PolicyKit1.AuthenticationAgent"


def plain(value):
    """dbus-python values -> JSON-friendly ones."""
    if isinstance(value, dict):
        return {str(k): plain(v) for k, v in value.items()}
    if isinstance(value, (list, tuple)):
        return [plain(v) for v in value]
    if isinstance(value, bool):
        return bool(value)
    if isinstance(value, int):
        return int(value)
    if isinstance(value, str):
        return str(value)
    return value


def load(mock, parameters):
    polkitd.load(mock, parameters)
    mock.agents = []  # registration order
    mock.auths = {}  # id -> {"cookie", "agent", "reply"}
    mock.responses = []
    mock.next_id = 1

    def name_owner_changed(name, _old, new):
        if not new:
            mock.agents = [a for a in mock.agents if a["sender"] != name]

    mock.connection.add_signal_receiver(name_owner_changed, "NameOwnerChanged", "org.freedesktop.DBus",
                                        "org.freedesktop.DBus", "/org/freedesktop/DBus")


def register(self, sender, subject, locale, object_path, options):
    entry = {"sender": str(sender), "subject": plain(subject), "locale": str(locale),
             "path": str(object_path), "options": plain(options)}
    print(f"agent registered: {entry}", flush=True)
    self.agents = [a for a in self.agents if a["sender"] != sender] + [entry]


@dbus.service.method(MAIN_IFACE, in_signature="(sa{sv})ss", sender_keyword="sender")
def RegisterAuthenticationAgent(self, subject, locale, object_path, sender=None):
    register(self, sender, subject, locale, object_path, {})


@dbus.service.method(MAIN_IFACE, in_signature="(sa{sv})ssa{sv}", sender_keyword="sender")
def RegisterAuthenticationAgentWithOptions(self, subject, locale, object_path, options, sender=None):
    register(self, sender, subject, locale, object_path, options)


@dbus.service.method(MAIN_IFACE, in_signature="(sa{sv})s", sender_keyword="sender")
def UnregisterAuthenticationAgent(self, _subject, object_path, sender=None):
    self.agents = [a for a in self.agents if not (a["sender"] == sender and a["path"] == object_path)]


@dbus.service.method(MAIN_IFACE, in_signature="us(sa{sv})")
def AuthenticationAgentResponse2(self, uid, cookie, identity):
    print(f"AuthenticationAgentResponse2 uid={uid} cookie={cookie}", flush=True)
    self.responses.append({"uid": int(uid), "cookie": str(cookie), "identity": plain(identity)})


@dbus.service.method(MAIN_IFACE, in_signature="s(sa{sv})")
def AuthenticationAgentResponse(self, cookie, identity):
    self.responses.append({"uid": None, "cookie": str(cookie), "identity": plain(identity)})


# --- test interface ------------------------------------------------------

@dbus.service.method(TEST_IFACE, in_signature="", out_signature="s")
def GetAgents(self):
    return json.dumps(self.agents)


@dbus.service.method(TEST_IFACE, in_signature="ssa{ss}a(sa{sv})", out_signature="u")
def BeginAuthentication(self, action_id, message, details, identities):
    auth_id = self.next_id
    self.next_id += 1
    cookie = f"{auth_id}-{uuid.uuid4().hex}"
    auth = {"cookie": cookie, "agent": None, "reply": None}
    self.auths[auth_id] = auth
    if not self.agents:
        auth["reply"] = {"error": "no-agent"}
        return auth_id
    agent = self.agents[-1]
    auth["agent"] = agent

    def on_reply():
        authorized = any(r["cookie"] == cookie for r in self.responses)
        auth["reply"] = {"ok": True, "authorized": authorized}
        print(f"BeginAuthentication {auth_id}: done (authorized={authorized})", flush=True)

    def on_error(error):
        auth["reply"] = {"error": error.get_dbus_name(), "message": error.get_dbus_message()}
        print(f"BeginAuthentication {auth_id}: {error.get_dbus_name()}", flush=True)

    self.connection.call_async(agent["sender"], agent["path"], AGENT_IFACE, "BeginAuthentication",
                               "sssa{ss}sa(sa{sv})",
                               (action_id, message, "", details, cookie, identities),
                               on_reply, on_error, timeout=3600)
    return auth_id


@dbus.service.method(TEST_IFACE, in_signature="u", out_signature="")
def CancelAuthentication(self, auth_id):
    auth = self.auths[auth_id]
    agent = auth["agent"]
    if agent:
        self.connection.call_async(agent["sender"], agent["path"], AGENT_IFACE, "CancelAuthentication",
                                   "s", (auth["cookie"],), None, None)


@dbus.service.method(TEST_IFACE, in_signature="u", out_signature="s")
def GetCookie(self, auth_id):
    return self.auths[auth_id]["cookie"]


@dbus.service.method(TEST_IFACE, in_signature="u", out_signature="s")
def GetReply(self, auth_id):
    reply = self.auths[auth_id]["reply"]
    return json.dumps(reply) if reply is not None else ""


@dbus.service.method(TEST_IFACE, in_signature="", out_signature="s")
def GetResponses(self):
    return json.dumps(self.responses)


@dbus.service.method(TEST_IFACE, in_signature="", out_signature="")
def Restart(self):
    print("restarting polkitd", flush=True)
    self.agents = []
    self.connection.release_name(BUS_NAME)

    def take_name():
        self.connection.request_name(BUS_NAME)
        return False

    GLib.timeout_add(500, take_name)
