"""python-dbusmock template: dbusmock's NetworkManager plus a secret-agent
manager, used by dev/mocks.py.

Adds /org/freedesktop/NetworkManager/AgentManager, which records the
agents that Register (by their unique bus name, like real NetworkManager,
which needs the sender and so can't be an AddMethod snippet), and makes
AddAndActivateConnection on a secured access point ask the agent for the
secrets before the connection comes up, like the real thing.

Scenarios drive the agent through NetworkManager's own bus name (the
panel only accepts calls from it) with a test interface on the same object,
org.gnome.ShellPlus.MockAgentManager:

    GetAgents() -> a{ss}                    unique name -> identifier
    RequestSecrets(a{sa{sv}} connection, o path, s setting, as hints,
                   u flags) -> u id         calls the agent's GetSecrets
    CancelSecrets(o path, s setting)        calls CancelGetSecrets
    GetReply(u id) -> s                     JSON, "" while pending:
                                            {"secrets": {...}} or {"error": name}
"""
import json

import dbus
import dbus.service
import dbusmock
from dbusmock.templates import networkmanager as nm
from dbusmock.templates.networkmanager import *  # noqa: F401,F403 (BUS_NAME, MAIN_OBJ, mock methods)

AGENT_MANAGER_PATH = "/org/freedesktop/NetworkManager/AgentManager"
AGENT_MANAGER_IFACE = "org.freedesktop.NetworkManager.AgentManager"
AGENT_PATH = "/org/freedesktop/NetworkManager/SecretAgent"
AGENT_IFACE = "org.freedesktop.NetworkManager.SecretAgent"
TEST_IFACE = "org.gnome.ShellPlus.MockAgentManager"

FLAG_ALLOW_INTERACTION = 0x1
FLAG_USER_REQUESTED = 0x4
DEVICE_STATE_NEED_AUTH = 60
DEVICE_STATE_DISCONNECTED = 30
DEVICE_STATE_ACTIVATED = 100
ACTIVE_STATE_ACTIVATING = 1
ACTIVE_STATE_ACTIVATED = 2


def plain(value):
    """dbus-python values -> JSON-friendly ones."""
    if isinstance(value, dbus.ByteArray) or (isinstance(value, dbus.Array) and value.signature == "y"):
        return bytes(value).decode("utf-8", "replace")
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
    nm.load(mock, parameters)

    class AgentManager(dbus.service.Object):
        # dbusmock replaces dbus-python's method lookup with one that reads
        # these first, then falls back to the decorated methods below.
        methods = {}
        interface = AGENT_MANAGER_IFACE

        def __init__(self, conn):
            super().__init__(conn, AGENT_MANAGER_PATH)
            self.conn = conn
            self.agents = {}  # unique name -> identifier, in registration order
            self.replies = {}
            self.next_id = 1
            conn.add_signal_receiver(self._name_owner_changed, "NameOwnerChanged",
                                     "org.freedesktop.DBus", "org.freedesktop.DBus",
                                     "/org/freedesktop/DBus")

        def _name_owner_changed(self, name, _old, new):
            if not new:
                self.agents.pop(name, None)

        @dbus.service.method(AGENT_MANAGER_IFACE, in_signature="s", out_signature="",
                             sender_keyword="sender")
        def Register(self, identifier, sender=None):
            print(f"agent registered: {identifier} ({sender})", flush=True)
            self.agents.pop(sender, None)
            self.agents[sender] = identifier

        @dbus.service.method(AGENT_MANAGER_IFACE, in_signature="su", out_signature="",
                             sender_keyword="sender")
        def RegisterWithCapabilities(self, identifier, _capabilities, sender=None):
            self.Register(identifier, sender=sender)

        @dbus.service.method(AGENT_MANAGER_IFACE, in_signature="", out_signature="",
                             sender_keyword="sender")
        def Unregister(self, sender=None):
            self.agents.pop(sender, None)

        # --- test interface ------------------------------------------------

        @dbus.service.method(TEST_IFACE, in_signature="", out_signature="a{ss}")
        def GetAgents(self):
            return self.agents

        @dbus.service.method(TEST_IFACE, in_signature="a{sa{sv}}osasu", out_signature="u")
        def RequestSecrets(self, connection, path, setting, hints, flags):
            return self.request(connection, path, setting, hints, flags)

        @dbus.service.method(TEST_IFACE, in_signature="os", out_signature="")
        def CancelSecrets(self, path, setting):
            agent = self.agent()
            if agent:
                self.conn.call_async(agent, AGENT_PATH, AGENT_IFACE, "CancelGetSecrets", "os",
                                     (path, setting), None, None)

        @dbus.service.method(TEST_IFACE, in_signature="u", out_signature="s")
        def GetReply(self, request_id):
            reply = self.replies.get(request_id)
            return json.dumps(reply) if reply is not None else ""

        # --- helpers ---------------------------------------------------------

        def agent(self):
            # Like NetworkManager, ask the most recently registered agent.
            return next(reversed(self.agents), None)

        def request(self, connection, path, setting, hints, flags, done=None):
            request_id = self.next_id
            self.next_id += 1
            agent = self.agent()
            if not agent:
                self.replies[request_id] = {"error": "no-agent"}
                if done:
                    done(None)
                return request_id

            def on_reply(secrets):
                self.replies[request_id] = {"secrets": plain(secrets)}
                print(f"GetSecrets {request_id}: secrets for {list(secrets)}", flush=True)
                if done:
                    done(secrets)

            def on_error(error):
                self.replies[request_id] = {"error": error.get_dbus_name()}
                print(f"GetSecrets {request_id}: {error.get_dbus_name()}", flush=True)
                if done:
                    done(None)

            self.conn.call_async(agent, AGENT_PATH, AGENT_IFACE, "GetSecrets", "a{sa{sv}}osasu",
                                 (connection, path, setting, hints, dbus.UInt32(flags)),
                                 on_reply, on_error, timeout=600)
            return request_id

    manager = AgentManager(mock.connection)
    mock.agent_manager = manager  # keep it alive

    # AddAndActivateConnection on a secured AP: the device waits in NEED_AUTH
    # while the agent is asked; the reply decides whether it comes up.
    nm_obj = dbusmock.get_object(nm.MANAGER_OBJ)
    original = nm_obj.add_and_activate_connection

    def add_and_activate_connection(self, conn_conf, dev, ap):
        conn_path, active_path = original(self, conn_conf, dev, ap)
        conn_obj = dbusmock.get_object(conn_path)
        settings = conn_obj.settings
        if "802-11-wireless-security" not in settings:
            return (conn_path, active_path)

        device = dbusmock.get_object(dev)
        active = dbusmock.get_object(active_path)
        active.UpdateProperties(nm.ACTIVE_CONNECTION_IFACE, {"State": dbus.UInt32(ACTIVE_STATE_ACTIVATING)})
        device.UpdateProperties(nm.DEVICE_IFACE, {"State": dbus.UInt32(DEVICE_STATE_NEED_AUTH)})

        def done(secrets):
            if secrets and "802-11-wireless-security" in secrets:
                settings["802-11-wireless-security"].update(secrets["802-11-wireless-security"])
                active.UpdateProperties(nm.ACTIVE_CONNECTION_IFACE,
                                        {"State": dbus.UInt32(ACTIVE_STATE_ACTIVATED)})
                device.UpdateProperties(nm.DEVICE_IFACE, {"State": dbus.UInt32(DEVICE_STATE_ACTIVATED)})
            else:
                nm.deactivate_connection(self, active_path)
                device.UpdateProperties(nm.DEVICE_IFACE, {"State": dbus.UInt32(DEVICE_STATE_DISCONNECTED)})

        # Secrets aren't sent to agents; only the settings that describe them.
        request = {k: dbus.Dictionary(v, signature="sv") for k, v in settings.items()}
        manager.request(dbus.Dictionary(request, signature="sa{sv}"), dbus.ObjectPath(conn_path),
                        "802-11-wireless-security", dbus.Array([], signature="s"),
                        FLAG_ALLOW_INTERACTION | FLAG_USER_REQUESTED, done)
        return (conn_path, active_path)

    nm_obj.add_and_activate_connection = add_and_activate_connection
