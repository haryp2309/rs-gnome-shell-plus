//! What a notification is, independent of the D-Bus API it arrived on.

use gtk::prelude::*;
use gtk::{gio, glib};

/// What happens when the user clicks a notification or one of its buttons.
#[derive(Clone, Debug)]
pub enum Activation {
    /// org.freedesktop.Notifications: emit `ActionInvoked` with this key.
    Fdo(String),
    /// org.gtk.Notifications: activate an `app.` action on the sending
    /// application, or just activate the application when `action` is None.
    Gtk { action: Option<String>, target: Option<glib::Variant> },
}

#[derive(Clone, Debug)]
pub struct Action {
    pub label: String,
    pub activation: Activation,
}

/// Which API the notification came from, and its key there.
#[derive(Clone, Debug, PartialEq)]
pub enum Origin {
    Fdo,
    Gtk { app_id: String, id: String },
}

/// The application a notification belongs to. Notifications are grouped by
/// [`App::key`] in the date menu.
#[derive(Clone, Debug)]
pub struct App {
    /// Desktop file id without ".desktop", when known.
    pub id: Option<String>,
    pub name: String,
    pub icon: Option<gio::Icon>,
    pub info: Option<gio::AppInfo>,
}

impl App {
    /// Resolves an application by desktop file id (with or without the
    /// ".desktop" suffix), falling back to `fallback_name`.
    pub fn resolve(desktop_id: Option<&str>, fallback_name: &str) -> Self {
        let id = desktop_id
            .map(|id| id.trim_end_matches(".desktop"))
            .filter(|id| !id.is_empty());
        let info = id.and_then(desktop_app);
        let name = match &info {
            Some(info) => info.name().to_string(),
            None if !fallback_name.is_empty() => fallback_name.to_owned(),
            None => id.unwrap_or("Notification").to_owned(),
        };
        Self {
            id: id.map(str::to_owned),
            name,
            icon: info.as_ref().and_then(|info| info.icon()),
            info,
        }
    }

    pub fn key(&self) -> &str {
        self.id.as_deref().unwrap_or(&self.name)
    }
}

#[derive(Clone, Debug)]
pub struct Notification {
    pub id: u32,
    pub origin: Origin,
    pub app: App,
    pub summary: String,
    /// Safe Pango markup (see [`super::markup`]).
    pub body: String,
    /// The notification's own image, shown next to the text.
    pub image: Option<gio::Icon>,
    pub default_action: Option<Activation>,
    pub actions: Vec<Action>,
    pub critical: bool,
    /// Not kept in the date menu once its banner is gone.
    pub transient: bool,
    /// Stays around after an action is invoked.
    pub resident: bool,
    /// Banner timeout requested by the sender, in milliseconds.
    pub timeout_ms: Option<u32>,
    pub time: glib::DateTime,
}

impl Notification {
    pub fn new(origin: Origin, app: App) -> Self {
        Self {
            id: 0,
            origin,
            app,
            summary: String::new(),
            body: String::new(),
            image: None,
            default_action: None,
            actions: Vec::new(),
            critical: false,
            transient: false,
            resident: false,
            timeout_ms: None,
            time: glib::DateTime::now_local().expect("local time"),
        }
    }
}

/// The installed application with desktop file id `id` (no ".desktop").
/// (`gio::DesktopAppInfo` lives in the gio-unix crate, so go through the
/// AppInfo interface; GLib caches the desktop file index.)
fn desktop_app(id: &str) -> Option<gio::AppInfo> {
    let desktop_id = format!("{id}.desktop");
    gio::AppInfo::all()
        .into_iter()
        .find(|info| info.id().is_some_and(|i| i == desktop_id.as_str()))
}

/// An icon from a notification's `app_icon` or `image-path`: a file path,
/// a file URI or an icon name.
pub fn icon_from_string(icon: &str) -> Option<gio::Icon> {
    if icon.is_empty() {
        None
    } else if icon.starts_with("file://") {
        Some(gio::FileIcon::new(&gio::File::for_uri(icon)).upcast())
    } else if icon.starts_with('/') {
        Some(gio::FileIcon::new(&gio::File::for_path(icon)).upcast())
    } else {
        Some(gio::ThemedIcon::new(icon).upcast())
    }
}

/// GNOME's time stamps: "Just now", "5 minutes ago", "Yesterday", ...
pub fn relative_time(time: &glib::DateTime) -> String {
    let Ok(now) = glib::DateTime::now_local() else {
        return String::new();
    };
    let same_day = |a: &glib::DateTime, b: &glib::DateTime| a.ymd() == b.ymd();
    let minutes = now.difference(time).as_minutes().max(0);
    let ago = |n: i64, unit: &str| format!("{n} {unit}{} ago", if n == 1 { "" } else { "s" });
    let format = |f: &str| time.format(f).map(|s| s.to_string()).unwrap_or_default();
    if minutes == 0 {
        "Just now".to_owned()
    } else if minutes < 60 {
        ago(minutes, "minute")
    } else if same_day(&now, time) {
        ago(minutes / 60, "hour")
    } else if now.add_days(-1).is_ok_and(|yesterday| same_day(&yesterday, time)) {
        "Yesterday".to_owned()
    } else if minutes < 7 * 24 * 60 {
        format("%A")
    } else {
        format("%b %-d")
    }
}
