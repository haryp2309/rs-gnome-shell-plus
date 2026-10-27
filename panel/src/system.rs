//! Small helpers for GSettings, D-Bus system services and subprocesses.
//! Everything here degrades to `None` / no-op when the service is missing,
//! so the panel still runs in minimal sessions.

use gtk::prelude::*;
use gtk::{gio, glib};
use std::ffi::OsStr;

/// Returns the settings object only if the schema is installed, instead of
/// aborting like `gio::Settings::new` does.
pub fn settings(schema: &str) -> Option<gio::Settings> {
    gio::SettingsSchemaSource::default()?.lookup(schema, true)?;
    Some(gio::Settings::new(schema))
}

pub fn system_proxy(name: &str, path: &str, iface: &str) -> Option<gio::DBusProxy> {
    proxy(gio::BusType::System, name, path, iface)
}

pub fn session_proxy(name: &str, path: &str, iface: &str) -> Option<gio::DBusProxy> {
    proxy(gio::BusType::Session, name, path, iface)
}

fn proxy(bus: gio::BusType, name: &str, path: &str, iface: &str) -> Option<gio::DBusProxy> {
    let proxy = gio::DBusProxy::for_bus_sync(
        bus,
        gio::DBusProxyFlags::NONE,
        None::<&gio::DBusInterfaceInfo>,
        name,
        path,
        iface,
        gio::Cancellable::NONE,
    )
    .ok()?;
    proxy.name_owner().is_some().then_some(proxy)
}

pub fn prop<T: glib::variant::FromVariant>(proxy: &gio::DBusProxy, name: &str) -> Option<T> {
    proxy.cached_property(name)?.get::<T>()
}

pub fn on_properties_changed(proxy: &gio::DBusProxy, callback: impl Fn() + 'static) {
    proxy.connect_local("g-properties-changed", false, move |_| {
        callback();
        None
    });
}

pub fn set_prop(proxy: &gio::DBusProxy, name: &str, value: glib::Variant) {
    let iface = proxy.interface_name();
    proxy.connection().call(
        proxy.name().as_deref(),
        &proxy.object_path(),
        "org.freedesktop.DBus.Properties",
        "Set",
        Some(&(iface.as_str(), name, value).to_variant()),
        None,
        gio::DBusCallFlags::NONE,
        -1,
        gio::Cancellable::NONE,
        |result| {
            if let Err(err) = result {
                glib::g_warning!("shellplus", "Failed to set property: {err}");
            }
        },
    );
}

pub fn call(proxy: &gio::DBusProxy, method: &str, args: Option<glib::Variant>) {
    let method_name = method.to_owned();
    proxy.call(
        method,
        args.as_ref(),
        gio::DBusCallFlags::NONE,
        -1,
        gio::Cancellable::NONE,
        move |result| {
            if let Err(err) = result {
                glib::g_warning!("shellplus", "{method_name} failed: {err}");
            }
        },
    );
}

pub fn spawn(argv: &[&str]) {
    let argv: Vec<&OsStr> = argv.iter().map(OsStr::new).collect();
    if let Err(err) = gio::Subprocess::newv(&argv, gio::SubprocessFlags::NONE) {
        glib::g_warning!("shellplus", "Failed to run {:?}: {err}", argv[0]);
    }
}

/// Runs a command and passes its stdout to `callback` once it exits.
pub fn run_output(argv: &[&str], callback: impl FnOnce(String) + 'static) {
    let argv: Vec<&OsStr> = argv.iter().map(OsStr::new).collect();
    let flags = gio::SubprocessFlags::STDOUT_PIPE | gio::SubprocessFlags::STDERR_SILENCE;
    match gio::Subprocess::newv(&argv, flags) {
        Ok(process) => {
            process.communicate_utf8_async(None, gio::Cancellable::NONE, move |result| {
                if let Ok((Some(stdout), _)) = result {
                    callback(stdout.to_string());
                }
            })
        }
        Err(err) => glib::g_warning!("shellplus", "Failed to run {:?}: {err}", argv[0]),
    }
}
