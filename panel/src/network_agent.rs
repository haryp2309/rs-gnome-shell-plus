//! NetworkManager secret agent: asks for Wi-Fi passwords (and basic 802.1X
//! credentials) in a system dialog, like GNOME Shell's networkAgent.js.
//!
//! The panel exports org.freedesktop.NetworkManager.SecretAgent on the
//! system bus and registers it with NetworkManager's AgentManager (again
//! whenever NetworkManager restarts). When a connection needs a secret
//! that isn't stored, NetworkManager calls GetSecrets and the panel shows
//! one request at a time; others wait in a queue.
//!
//! Secrets aren't stored: there's no keyring integration yet, so
//! SaveSecrets/DeleteSecrets succeed without doing anything and
//! NetworkManager keeps system-owned secrets itself.

use crate::system::{self, Props};
use crate::system_dialog::SystemDialog;
use adw::prelude::*;
use gtk::{gio, glib};
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

const NM: &str = "org.freedesktop.NetworkManager";
const AGENT_MANAGER_PATH: &str = "/org/freedesktop/NetworkManager/AgentManager";
const AGENT_MANAGER: &str = "org.freedesktop.NetworkManager.AgentManager";
const AGENT_PATH: &str = "/org/freedesktop/NetworkManager/SecretAgent";
const AGENT: &str = "org.freedesktop.NetworkManager.SecretAgent";
const IDENTIFIER: &str = "org.gnome.ShellPlus.NetworkAgent";

// NMSecretAgentGetSecretsFlags
const FLAG_ALLOW_INTERACTION: u32 = 0x1;
const FLAG_REQUEST_NEW: u32 = 0x2;

// NMSecretAgentError names
const ERROR_PERMISSION_DENIED: &str = "org.freedesktop.NetworkManager.SecretAgent.PermissionDenied";
const ERROR_INVALID_CONNECTION: &str = "org.freedesktop.NetworkManager.SecretAgent.InvalidConnection";
const ERROR_USER_CANCELED: &str = "org.freedesktop.NetworkManager.SecretAgent.UserCanceled";
const ERROR_AGENT_CANCELED: &str = "org.freedesktop.NetworkManager.SecretAgent.AgentCanceled";
const ERROR_NO_SECRETS: &str = "org.freedesktop.NetworkManager.SecretAgent.NoSecrets";

const XML: &str = r#"
<node>
  <interface name="org.freedesktop.NetworkManager.SecretAgent">
    <method name="GetSecrets">
      <arg name="connection" type="a{sa{sv}}" direction="in"/>
      <arg name="connection_path" type="o" direction="in"/>
      <arg name="setting_name" type="s" direction="in"/>
      <arg name="hints" type="as" direction="in"/>
      <arg name="flags" type="u" direction="in"/>
      <arg name="secrets" type="a{sa{sv}}" direction="out"/>
    </method>
    <method name="CancelGetSecrets">
      <arg name="connection_path" type="o" direction="in"/>
      <arg name="setting_name" type="s" direction="in"/>
    </method>
    <method name="SaveSecrets">
      <arg name="connection" type="a{sa{sv}}" direction="in"/>
      <arg name="connection_path" type="o" direction="in"/>
    </method>
    <method name="DeleteSecrets">
      <arg name="connection" type="a{sa{sv}}" direction="in"/>
      <arg name="connection_path" type="o" direction="in"/>
    </method>
  </interface>
</node>"#;

/// What the dialog asks for.
#[derive(Clone, Debug, PartialEq)]
enum Kind {
    /// WPA/WPA2/WPA3 personal: `psk`.
    Psk,
    /// Static WEP: `wep-key0` (`wep-key-type`: 1 = hex/ASCII key, 2 = passphrase).
    Wep { key_type: u32 },
    /// 802.1X with a password-based EAP method: `identity` + `password`.
    Eap,
}

impl Kind {
    fn secret_key(&self) -> &'static str {
        match self {
            Kind::Psk => "psk",
            Kind::Wep { .. } => "wep-key0",
            Kind::Eap => "password",
        }
    }

    fn validate(&self, secret: &str) -> bool {
        let hex = |s: &str| s.chars().all(|c| c.is_ascii_hexdigit());
        let len = secret.chars().count();
        match self {
            // 8–63 characters, or a raw 64-digit hex key.
            Kind::Psk => (8..=63).contains(&len) || (len == 64 && hex(secret)),
            Kind::Wep { key_type: 2 } => (1..=64).contains(&len),
            // 40/104-bit keys: 5/13 ASCII characters or 10/26 hex digits.
            Kind::Wep { .. } => matches!(len, 5 | 13) || (matches!(len, 10 | 26) && hex(secret)),
            Kind::Eap => len > 0,
        }
    }
}

struct Request {
    invocation: gio::DBusMethodInvocation,
    connection_path: String,
    setting_name: String,
    kind: Kind,
    /// Network name (SSID or connection id) for the description.
    network: String,
    wireless: bool,
    /// Already known values to prefill (`identity`, or the old secret).
    identity: String,
    existing: String,
}

impl Request {
    fn matches(&self, connection_path: &str, setting_name: &str) -> bool {
        self.connection_path == connection_path && self.setting_name == setting_name
    }
}

struct Agent {
    app: adw::Application,
    conn: gio::DBusConnection,
    /// Unique name of NetworkManager; only it may call the agent.
    nm_owner: RefCell<Option<String>>,
    queue: RefCell<VecDeque<Request>>,
    /// The request on screen and its dialog. The request is taken out once
    /// it's answered.
    current: RefCell<Option<(Rc<RefCell<Option<Request>>>, Rc<SystemDialog>)>>,
}

pub fn install(app: &adw::Application) {
    let Some(conn) = system::system_bus() else { return };
    let agent = Rc::new(Agent {
        app: app.clone(),
        conn: conn.clone(),
        nm_owner: RefCell::default(),
        queue: RefCell::default(),
        current: RefCell::default(),
    });

    let node = gio::DBusNodeInfo::for_xml(XML).expect("valid introspection XML");
    let iface = node.lookup_interface(AGENT).expect("interface in XML");
    let registration = conn
        .register_object(AGENT_PATH, &iface)
        .method_call(glib::clone!(
            #[strong]
            agent,
            move |_, sender, _, _, method, params, invocation| agent.handle(sender, method, &params, invocation)
        ))
        .build();
    if let Err(err) = registration {
        glib::g_warning!("shellplus", "Can't export the NetworkManager secret agent: {err}");
        return;
    }

    // Register whenever NetworkManager (re)appears.
    gio::bus_watch_name_on_connection(
        &conn,
        NM,
        gio::BusNameWatcherFlags::NONE,
        glib::clone!(
            #[strong]
            agent,
            move |_, _, owner| {
                agent.nm_owner.replace(Some(owner.to_owned()));
                agent.register();
            }
        ),
        move |_, _| {
            agent.nm_owner.take();
            agent.cancel_all(ERROR_AGENT_CANCELED);
        },
    );
}

impl Agent {
    fn register(&self) {
        self.conn.call(
            Some(NM),
            AGENT_MANAGER_PATH,
            AGENT_MANAGER,
            "Register",
            Some(&(IDENTIFIER,).to_variant()),
            None,
            gio::DBusCallFlags::NONE,
            -1,
            gio::Cancellable::NONE,
            move |result| match result {
                Ok(_) => glib::g_message!("shellplus", "Registered NetworkManager secret agent"),
                Err(err) => glib::g_warning!("shellplus", "Can't register the NetworkManager secret agent: {err}"),
            },
        );
    }

    fn handle(
        self: &Rc<Self>,
        sender: Option<&str>,
        method: &str,
        params: &glib::Variant,
        invocation: gio::DBusMethodInvocation,
    ) {
        if sender.is_none() || sender != self.nm_owner.borrow().as_deref() {
            invocation.return_dbus_error(ERROR_PERMISSION_DENIED, "Only NetworkManager may call the secret agent");
            return;
        }
        match method {
            "GetSecrets" => self.get_secrets(params, invocation),
            "CancelGetSecrets" => {
                let path = params.child_value(0).str().unwrap_or_default().to_owned();
                let setting = params.child_value(1).str().unwrap_or_default().to_owned();
                self.cancel_request(&path, &setting);
                invocation.return_value(None);
            }
            // No keyring yet: nothing to save or delete.
            "SaveSecrets" | "DeleteSecrets" => invocation.return_value(None),
            _ => invocation.return_dbus_error("org.freedesktop.DBus.Error.UnknownMethod", method),
        }
    }

    fn get_secrets(self: &Rc<Self>, params: &glib::Variant, invocation: gio::DBusMethodInvocation) {
        let connection = params.child_value(0);
        let connection_path = params.child_value(1).str().unwrap_or_default().to_owned();
        let setting_name = params.child_value(2).str().unwrap_or_default().to_owned();
        let flags = params.child_value(4).get::<u32>().unwrap_or(0);

        if flags & FLAG_ALLOW_INTERACTION == 0 {
            invocation.return_dbus_error(ERROR_NO_SECRETS, "No secrets stored and interaction isn't allowed");
            return;
        }
        let request = match parse_request(&connection, &setting_name) {
            Ok(parsed) => parsed,
            Err((error, message)) => {
                invocation.return_dbus_error(error, &message);
                return;
            }
        };
        let (kind, network, wireless, identity, mut existing) = request;
        if flags & FLAG_REQUEST_NEW != 0 {
            // The stored secret was wrong; don't offer it again.
            existing.clear();
        }
        self.queue.borrow_mut().push_back(Request {
            invocation,
            connection_path,
            setting_name,
            kind,
            network,
            wireless,
            identity,
            existing,
        });
        self.show_next();
    }

    /// NetworkManager no longer needs the secrets (e.g. the activation was
    /// cancelled): drop the request and close its dialog.
    fn cancel_request(self: &Rc<Self>, path: &str, setting: &str) {
        let mut queue = self.queue.borrow_mut();
        let (cancelled, kept): (VecDeque<_>, VecDeque<_>) = queue.drain(..).partition(|r| r.matches(path, setting));
        *queue = kept;
        drop(queue);
        for request in cancelled {
            request.invocation.return_dbus_error(ERROR_AGENT_CANCELED, "Canceled by NetworkManager");
        }

        let current = self.current.borrow().clone();
        if let Some((slot, dialog)) = current {
            let matches = slot.borrow().as_ref().is_some_and(|r| r.matches(path, setting));
            if matches {
                if let Some(request) = slot.take() {
                    request.invocation.return_dbus_error(ERROR_AGENT_CANCELED, "Canceled by NetworkManager");
                }
                dialog.close();
                self.finish();
            }
        }
    }

    fn cancel_all(&self, error: &str) {
        for request in self.queue.borrow_mut().drain(..) {
            request.invocation.return_dbus_error(error, "Canceled");
        }
        if let Some((slot, dialog)) = self.current.take() {
            if let Some(request) = slot.take() {
                request.invocation.return_dbus_error(error, "Canceled");
            }
            dialog.close();
        }
    }

    /// The current dialog is done; show the next queued request, if any.
    fn finish(self: &Rc<Self>) {
        self.current.take();
        self.show_next();
    }

    fn show_next(self: &Rc<Self>) {
        if self.current.borrow().is_some() {
            return;
        }
        let Some(request) = self.queue.borrow_mut().pop_front() else { return };

        let dialog = SystemDialog::new(&self.app, "network-secrets");
        dialog.set_heading("Authentication required");
        let kind_of_network = if request.wireless { "wireless" } else { "wired" };
        dialog.set_body(&format!(
            "Passwords or encryption keys are required to access the {kind_of_network} network “{}”.",
            request.network
        ));

        let identity = (request.kind == Kind::Eap).then(|| {
            let entry = gtk::Entry::builder()
                .name("network-secrets-identity")
                .placeholder_text("Username")
                .text(request.identity.as_str())
                .build();
            dialog.add_content(&entry);
            entry
        });
        let secret = gtk::PasswordEntry::builder()
            .name("network-secrets-entry")
            .placeholder_text(if matches!(request.kind, Kind::Wep { .. }) { "Key" } else { "Password" })
            .show_peek_icon(true)
            .text(request.existing.as_str())
            .build();
        dialog.add_content(&secret);

        let cancel = dialog.add_button("network-secrets-cancel", "Cancel", false);
        let connect = dialog.add_button("network-secrets-connect", "Connect", true);

        let kind = request.kind.clone();
        let slot = Rc::new(RefCell::new(Some(request)));

        let valid = {
            let secret = secret.clone();
            let identity = identity.clone();
            move || {
                kind.validate(&secret.text())
                    && identity.as_ref().is_none_or(|entry| !entry.text().trim().is_empty())
            }
        };
        let update = {
            let valid = valid.clone();
            let connect = connect.clone();
            move || connect.set_sensitive(valid())
        };
        update();
        secret.connect_changed({
            let update = update.clone();
            move |_| update()
        });
        if let Some(entry) = &identity {
            entry.connect_changed({
                let update = update.clone();
                move |_| update()
            });
        }

        let submit = {
            let this = Rc::downgrade(self);
            let slot = slot.clone();
            let dialog = Rc::downgrade(&dialog);
            let secret = secret.clone();
            let identity = identity.clone();
            move || {
                if !valid() {
                    return;
                }
                let (Some(this), Some(dialog)) = (this.upgrade(), dialog.upgrade()) else { return };
                if let Some(request) = slot.take() {
                    let mut values: HashMap<String, glib::Variant> = HashMap::new();
                    values.insert(request.kind.secret_key().into(), secret.text().to_variant());
                    if let Some(entry) = &identity {
                        values.insert("identity".into(), entry.text().trim().to_variant());
                    }
                    let mut secrets: HashMap<String, HashMap<String, glib::Variant>> = HashMap::new();
                    secrets.insert(request.setting_name.clone(), values);
                    request.invocation.return_value(Some(&(secrets,).to_variant()));
                }
                dialog.close();
                this.finish();
            }
        };
        let submit = Rc::new(submit);
        connect.connect_clicked({
            let submit = submit.clone();
            move |_| submit()
        });
        secret.connect_activate({
            let submit = submit.clone();
            move |_| submit()
        });
        if let Some(entry) = &identity {
            // Enter in the username field moves on to the password.
            let secret = secret.clone();
            entry.connect_activate(move |_| {
                secret.grab_focus();
            });
        }

        let user_cancel = {
            let this = Rc::downgrade(self);
            let slot = slot.clone();
            move || {
                if let Some(request) = slot.take() {
                    request.invocation.return_dbus_error(ERROR_USER_CANCELED, "User canceled");
                }
                if let Some(this) = this.upgrade() {
                    this.finish();
                }
            }
        };
        dialog.connect_cancel(user_cancel);
        cancel.connect_clicked(glib::clone!(
            #[weak]
            dialog,
            move |_| dialog.cancel()
        ));

        self.current.replace(Some((slot, dialog.clone())));
        match identity.as_ref().filter(|entry| entry.text().is_empty()) {
            Some(entry) => dialog.present(Some(entry)),
            None => dialog.present(Some(&secret)),
        }
    }
}

type Parsed = (Kind, String, bool, String, String);

/// Works out what to ask for from the connection's settings.
fn parse_request(connection: &glib::Variant, setting_name: &str) -> Result<Parsed, (&'static str, String)> {
    // a{sa{sv}}: outer values aren't boxed, so look them up by hand.
    let setting = |name: &str| {
        connection
            .iter()
            .find(|entry| entry.child_value(0).str() == Some(name))
            .map(|entry| Props(entry.child_value(1)))
    };
    let base = setting("connection").ok_or((ERROR_INVALID_CONNECTION, "No connection setting".to_owned()))?;
    let connection_type = base.string("type");
    let wireless = connection_type == "802-11-wireless";

    let ssid = setting("802-11-wireless")
        .and_then(|s| s.get::<Vec<u8>>("ssid"))
        .map(|ssid| String::from_utf8_lossy(&ssid).into_owned())
        .filter(|ssid| !ssid.is_empty());
    let network = ssid.unwrap_or_else(|| base.string("id"));

    match setting_name {
        "802-11-wireless-security" => {
            let security = setting(setting_name).ok_or((ERROR_INVALID_CONNECTION, "No security setting".to_owned()))?;
            match security.string("key-mgmt").as_str() {
                "wpa-psk" | "sae" => Ok((Kind::Psk, network, wireless, String::new(), security.string("psk"))),
                "none" => {
                    let key_type = security.get::<u32>("wep-key-type").unwrap_or(0);
                    Ok((Kind::Wep { key_type }, network, wireless, String::new(), security.string("wep-key0")))
                }
                other => Err((ERROR_NO_SECRETS, format!("Unsupported key management “{other}”"))),
            }
        }
        "802-1x" => {
            let eap = setting(setting_name).ok_or((ERROR_INVALID_CONNECTION, "No 802-1x setting".to_owned()))?;
            let methods: Vec<String> = eap.get("eap").unwrap_or_default();
            // Certificate-based (TLS) needs files and key passwords; leave
            // that to Settings.
            let password_based = ["peap", "ttls", "pwd", "leap", "md5", "fast"];
            if !methods.iter().any(|m| password_based.contains(&m.as_str())) {
                return Err((ERROR_NO_SECRETS, format!("Unsupported EAP method(s) {methods:?}")));
            }
            Ok((Kind::Eap, network, wireless, eap.string("identity"), eap.string("password")))
        }
        other => Err((ERROR_NO_SECRETS, format!("Can't ask for “{other}” secrets"))),
    }
}

#[cfg(test)]
mod tests {
    use super::Kind;

    #[test]
    fn psk_validation() {
        assert!(!Kind::Psk.validate("short"));
        assert!(Kind::Psk.validate("12345678"));
        assert!(Kind::Psk.validate(&"a".repeat(63)));
        assert!(Kind::Psk.validate(&"a".repeat(64)));
        assert!(!Kind::Psk.validate(&"g".repeat(64)));
        assert!(!Kind::Psk.validate(&"a".repeat(65)));
    }

    #[test]
    fn wep_validation() {
        let key = Kind::Wep { key_type: 1 };
        assert!(key.validate("abcde"));
        assert!(key.validate("0123456789"));
        assert!(!key.validate("012345678g"));
        assert!(!key.validate("abcdef"));
        assert!(Kind::Wep { key_type: 2 }.validate("any passphrase"));
    }
}
