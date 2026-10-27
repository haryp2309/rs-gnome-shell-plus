//! Small helpers for GSettings, D-Bus services and subprocesses.
//! Everything here degrades to `None` / no-op when the service is missing,
//! so the panel still runs in minimal sessions.

use gtk::prelude::*;
use gtk::{gio, glib};
use std::cell::Cell;
use std::ffi::OsStr;
use std::rc::Rc;
use std::time::Duration;

/// Returns the settings object only if the schema is installed, instead of
/// aborting like `gio::Settings::new` does.
pub fn settings(schema: &str) -> Option<gio::Settings> {
    gio::SettingsSchemaSource::default()?.lookup(schema, true)?;
    Some(gio::Settings::new(schema))
}

pub fn system_bus() -> Option<gio::DBusConnection> {
    gio::bus_get_sync(gio::BusType::System, gio::Cancellable::NONE).ok()
}

pub fn session_bus() -> Option<gio::DBusConnection> {
    gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE).ok()
}

/// `org.freedesktop.DBus.Properties.GetAll`, returned as an `a{sv}` variant
/// to use with [`Props`].
pub fn get_all(conn: &gio::DBusConnection, name: &str, path: &str, iface: &str) -> Option<Props> {
    let reply = call_sync(
        conn,
        name,
        path,
        "org.freedesktop.DBus.Properties",
        "GetAll",
        Some(&(iface,).to_variant()),
    )?;
    Some(Props(reply.child_value(0)))
}

/// Typed access to an `a{sv}` property dictionary.
pub struct Props(pub glib::Variant);

impl Props {
    pub fn value(&self, key: &str) -> Option<glib::Variant> {
        lookup(&self.0, key)
    }

    pub fn get<T: glib::variant::FromVariant>(&self, key: &str) -> Option<T> {
        self.value(key)?.get()
    }

    pub fn string(&self, key: &str) -> String {
        self.value(key)
            .and_then(|v| v.str().map(str::to_owned))
            .unwrap_or_default()
    }

    /// Object path, with NetworkManager's "/" meaning "none".
    pub fn path(&self, key: &str) -> Option<String> {
        Some(self.string(key)).filter(|p| !p.is_empty() && p != "/")
    }

    pub fn paths(&self, key: &str) -> Vec<String> {
        self.value(key).map(|v| paths(&v)).unwrap_or_default()
    }
}

/// Looks up `key` in an `a{sv}` (or `a{s*}`) dictionary, unboxing `v` values.
pub fn lookup(dict: &glib::Variant, key: &str) -> Option<glib::Variant> {
    dict.iter().find_map(|entry| {
        (entry.child_value(0).str() == Some(key)).then(|| {
            let value = entry.child_value(1);
            // as_variant() doesn't check the type, so only unbox real `v`s.
            if value.is::<glib::Variant>() {
                value.as_variant().unwrap_or(value)
            } else {
                value
            }
        })
    })
}

/// The strings of an `ao` / `as` variant.
pub fn paths(variant: &glib::Variant) -> Vec<String> {
    variant
        .iter()
        .filter_map(|v| v.str().map(str::to_owned))
        .collect()
}

pub fn object_path(path: &str) -> glib::Variant {
    glib::variant::ObjectPath::try_from(path.to_owned())
        .map(|p| p.to_variant())
        .unwrap_or_else(|_| glib::variant::ObjectPath::try_from("/".to_owned()).unwrap().to_variant())
}

pub fn call_sync(
    conn: &gio::DBusConnection,
    name: &str,
    path: &str,
    iface: &str,
    method: &str,
    args: Option<&glib::Variant>,
) -> Option<glib::Variant> {
    conn.call_sync(
        Some(name),
        path,
        iface,
        method,
        args,
        None,
        gio::DBusCallFlags::NONE,
        2000,
        gio::Cancellable::NONE,
    )
    .ok()
}

pub fn call(
    conn: &gio::DBusConnection,
    name: &str,
    path: &str,
    iface: &str,
    method: &str,
    args: Option<glib::Variant>,
) {
    let method_name = method.to_owned();
    conn.call(
        Some(name),
        path,
        iface,
        method,
        args.as_ref(),
        None,
        gio::DBusCallFlags::ALLOW_INTERACTIVE_AUTHORIZATION,
        -1,
        gio::Cancellable::NONE,
        move |result| {
            if let Err(err) = result {
                glib::g_warning!("shellplus", "{method_name} failed: {err}");
            }
        },
    );
}

pub fn set_prop(conn: &gio::DBusConnection, name: &str, path: &str, iface: &str, prop: &str, value: glib::Variant) {
    call(
        conn,
        name,
        path,
        "org.freedesktop.DBus.Properties",
        "Set",
        Some((iface, prop, value).to_variant()),
    );
}

/// Calls `callback` (debounced) whenever `name` emits any signal, appears
/// or disappears, and once right away. Good enough for re-reading a
/// service's whole state on change.
pub fn watch_service(conn: &gio::DBusConnection, name: &str, callback: impl Fn() + 'static) {
    let callback = debounced(callback);

    #[allow(deprecated)]
    conn.signal_subscribe(
        Some(name),
        None,
        None,
        None,
        None,
        gio::DBusSignalFlags::NONE,
        glib::clone!(
            #[strong]
            callback,
            move |_, _, _, _, _, _| callback()
        ),
    );
    let on_change = callback.clone();
    gio::bus_watch_name_on_connection(
        conn,
        name,
        gio::BusNameWatcherFlags::NONE,
        glib::clone!(
            #[strong]
            on_change,
            move |_, _, _| on_change()
        ),
        move |_, _| on_change(),
    );
    callback();
}

/// Coalesces bursts of calls (e.g. a dozen PropertiesChanged signals) into
/// one call shortly after.
pub fn debounced(f: impl Fn() + 'static) -> impl Fn() + Clone + 'static {
    let f = Rc::new(f);
    let pending = Rc::new(Cell::new(false));
    move || {
        if pending.replace(true) {
            return;
        }
        let f = f.clone();
        let pending = pending.clone();
        glib::timeout_add_local_once(Duration::from_millis(100), move || {
            pending.set(false);
            f();
        });
    }
}

pub fn spawn(argv: &[&str]) {
    let argv: Vec<&OsStr> = argv.iter().map(OsStr::new).collect();
    if let Err(err) = gio::Subprocess::newv(&argv, gio::SubprocessFlags::NONE) {
        glib::g_warning!("shellplus", "Failed to run {:?}: {err}", argv[0]);
    }
}

pub fn open_settings(panel: &str) {
    spawn(&["gnome-control-center", panel]);
}
