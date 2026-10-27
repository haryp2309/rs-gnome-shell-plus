//! The two notification APIs on the session bus:
//!
//! * `org.freedesktop.Notifications` (libnotify, notify-send, browsers...)
//! * `org.gtk.Notifications` (GLib's GNotification, used by most GNOME apps)
//!
//! Both are parsed into a [`Notification`] and handed to [`Notifications`].

use super::markup;
use super::model::{icon_from_string, Action, Activation, App, Notification, Origin};
use super::Notifications;
use crate::system;
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use std::collections::HashMap;
use std::rc::{Rc, Weak};

const FDO_NAME: &str = "org.freedesktop.Notifications";
const FDO_PATH: &str = "/org/freedesktop/Notifications";
const GTK_NAME: &str = "org.gtk.Notifications";
const GTK_PATH: &str = "/org/gtk/Notifications";

const XML: &str = r#"
<node>
  <interface name="org.freedesktop.Notifications">
    <method name="Notify">
      <arg type="s" direction="in"/><arg type="u" direction="in"/>
      <arg type="s" direction="in"/><arg type="s" direction="in"/>
      <arg type="s" direction="in"/><arg type="as" direction="in"/>
      <arg type="a{sv}" direction="in"/><arg type="i" direction="in"/>
      <arg type="u" direction="out"/>
    </method>
    <method name="CloseNotification"><arg type="u" direction="in"/></method>
    <method name="GetCapabilities"><arg type="as" direction="out"/></method>
    <method name="GetServerInformation">
      <arg type="s" direction="out"/><arg type="s" direction="out"/>
      <arg type="s" direction="out"/><arg type="s" direction="out"/>
    </method>
    <signal name="NotificationClosed"><arg type="u"/><arg type="u"/></signal>
    <signal name="ActionInvoked"><arg type="u"/><arg type="s"/></signal>
  </interface>
  <interface name="org.gtk.Notifications">
    <method name="AddNotification">
      <arg type="s" direction="in"/><arg type="s" direction="in"/>
      <arg type="a{sv}" direction="in"/>
    </method>
    <method name="RemoveNotification">
      <arg type="s" direction="in"/><arg type="s" direction="in"/>
    </method>
  </interface>
</node>"#;

const CAPABILITIES: &[&str] = &["actions", "body", "body-markup", "icon-static", "persistence"];

/// `NotificationClosed` reasons.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CloseReason {
    Expired = 1,
    Dismissed = 2,
    Closed = 3,
}

pub fn install(conn: &gio::DBusConnection, owner: Weak<Notifications>) {
    let node = gio::DBusNodeInfo::for_xml(XML).expect("valid introspection XML");
    for (name, path) in [(FDO_NAME, FDO_PATH), (GTK_NAME, GTK_PATH)] {
        let iface = node.lookup_interface(name).expect("interface in XML");
        let owner = owner.clone();
        let registration = conn
            .register_object(path, &iface)
            .method_call(move |_, sender, _, _, method, params, invocation| {
                let Some(owner) = owner.upgrade() else { return };
                match handle(&owner, sender, method, &params) {
                    Ok(reply) => invocation.return_value(reply.as_ref()),
                    Err(message) => invocation.return_dbus_error("org.freedesktop.DBus.Error.InvalidArgs", &message),
                }
            })
            .build();
        if let Err(err) = registration {
            glib::g_warning!("shellplus", "Can't export {name}: {err}");
            continue;
        }
        gio::bus_own_name_on_connection(
            conn,
            name,
            gio::BusNameOwnerFlags::REPLACE,
            |_, _| {},
            |_, name| glib::g_warning!("shellplus", "Lost the {name} bus name"),
        );
    }
}

fn handle(owner: &Rc<Notifications>, sender: Option<&str>, method: &str, params: &glib::Variant)
          -> Result<Option<glib::Variant>, String> {
    match method {
        "Notify" => {
            let id = owner.add(parse_fdo(owner, params).ok_or("bad arguments")?);
            Ok(Some((id,).to_variant()))
        }
        "CloseNotification" => {
            let (id,) = params.get::<(u32,)>().ok_or("bad arguments")?;
            owner.close(id, Some(CloseReason::Closed));
            Ok(None)
        }
        "GetCapabilities" => Ok(Some((CAPABILITIES,).to_variant())),
        "GetServerInformation" => {
            Ok(Some(("GNOME Shell Plus", "GNOME", env!("CARGO_PKG_VERSION"), "1.2").to_variant()))
        }
        "AddNotification" => {
            let notification = parse_gtk(owner, params).ok_or("bad arguments")?;
            owner.add(notification);
            Ok(None)
        }
        "RemoveNotification" => {
            let (app_id, id) = params.get::<(String, String)>().ok_or("bad arguments")?;
            if let Some(id) = owner.find(&Origin::Gtk { app_id, id }) {
                owner.close(id, None);
            }
            Ok(None)
        }
        _ => Err(format!("unknown method {method} from {}", sender.unwrap_or("?"))),
    }
}

// --- org.freedesktop.Notifications ------------------------------------------

fn parse_fdo(owner: &Notifications, params: &glib::Variant) -> Option<Notification> {
    let string = |i| params.try_child_value(i).and_then(|v| v.str().map(str::to_owned));
    let app_name = string(0)?;
    let replaces_id = params.try_child_value(1)?.get::<u32>()?;
    let app_icon = string(2)?;
    let actions = params.try_child_value(5)?.get::<Vec<String>>()?;
    let hints = params.try_child_value(6)?;
    let expire_timeout = params.try_child_value(7)?.get::<i32>()?;

    let hint = |key: &str| system::lookup(&hints, key);
    let hint_str = |key: &str| hint(key).and_then(|v| v.str().map(str::to_owned));
    let hint_bool = |key: &str| hint(key).and_then(|v| v.get::<bool>()).unwrap_or(false);

    let mut app = App::resolve(hint_str("desktop-entry").as_deref(), &app_name);
    let app_icon = icon_from_string(&app_icon);
    let image = ["image-data", "image_data", "icon_data"]
        .into_iter()
        .find_map(|key| hint(key).and_then(|v| texture_from_image_data(&v)))
        .or_else(|| ["image-path", "image_path"].into_iter().find_map(|key| icon_from_string(&hint_str(key)?)));
    // Without a known app, the app_icon stands in for the app's icon;
    // otherwise it's the notification's own icon.
    let image = if app.info.is_none() {
        app.icon = app_icon;
        image
    } else {
        image.or(app_icon)
    };

    let mut notification = Notification::new(Origin::Fdo, app);
    notification.id = Some(replaces_id).filter(|id| *id != 0 && owner.contains(*id)).unwrap_or(0);
    notification.summary = string(3)?;
    notification.body = markup::sanitize(&string(4)?);
    notification.image = image;
    notification.critical = hint("urgency").and_then(|v| v.get::<u8>()) == Some(2);
    notification.transient = hint_bool("transient");
    notification.resident = hint_bool("resident");
    notification.timeout_ms = u32::try_from(expire_timeout).ok().filter(|ms| *ms > 0);
    for pair in actions.chunks_exact(2) {
        let activation = Activation::Fdo(pair[0].clone());
        if pair[0] == "default" {
            notification.default_action = Some(activation);
        } else if !pair[1].is_empty() {
            notification.actions.push(Action { label: pair[1].clone(), activation });
        }
    }
    Some(notification)
}

/// The `(iiibiiay)` pixel data of the `image-data` hint, as a texture.
fn texture_from_image_data(data: &glib::Variant) -> Option<gio::Icon> {
    let (width, height, stride, has_alpha, bits, channels, pixels) =
        data.get::<(i32, i32, i32, bool, i32, i32, Vec<u8>)>()?;
    let format = match (has_alpha, channels, bits) {
        (true, 4, 8) => gdk::MemoryFormat::R8g8b8a8,
        (false, 3, 8) => gdk::MemoryFormat::R8g8b8,
        _ => return None,
    };
    let stride = usize::try_from(stride).ok()?;
    let needed = stride * usize::try_from(height - 1).ok()? + usize::try_from(width * channels).ok()?;
    if width <= 0 || pixels.len() < needed {
        return None;
    }
    let bytes = glib::Bytes::from_owned(pixels);
    Some(gdk::MemoryTexture::new(width, height, format, &bytes, stride).upcast())
}

pub fn emit_closed(conn: &gio::DBusConnection, id: u32, reason: CloseReason) {
    emit(conn, "NotificationClosed", (id, reason as u32).to_variant());
}

pub fn emit_action_invoked(conn: &gio::DBusConnection, id: u32, key: &str) {
    emit(conn, "ActionInvoked", (id, key).to_variant());
}

fn emit(conn: &gio::DBusConnection, signal: &str, args: glib::Variant) {
    if let Err(err) = conn.emit_signal(None, FDO_PATH, FDO_NAME, signal, Some(&args)) {
        glib::g_warning!("shellplus", "Can't emit {signal}: {err}");
    }
}

// --- org.gtk.Notifications --------------------------------------------------

fn parse_gtk(owner: &Notifications, params: &glib::Variant) -> Option<Notification> {
    let (app_id, id) = (params.try_child_value(0)?.str()?.to_owned(), params.try_child_value(1)?.str()?.to_owned());
    let data = params.try_child_value(2)?;
    let get = |key: &str| system::lookup(&data, key);
    let string = |key: &str| get(key).and_then(|v| v.str().map(str::to_owned)).unwrap_or_default();

    let origin = Origin::Gtk { app_id: app_id.clone(), id };
    let mut notification = Notification::new(origin.clone(), App::resolve(Some(&app_id), &app_id));
    notification.id = owner.find(&origin).unwrap_or(0);
    notification.summary = string("title");
    notification.body = match get("markup-body") {
        Some(markup) => markup::sanitize(markup.str().unwrap_or_default()),
        None => glib::markup_escape_text(&string("body")).to_string(),
    };
    notification.image = get("icon").and_then(|icon| gio::Icon::deserialize(&icon));
    notification.critical = string("priority") == "urgent";
    notification.default_action = Some(gtk_activation(&string("default-action"), get("default-action-target")));
    if let Some(buttons) = get("buttons") {
        notification.actions = buttons.iter().filter_map(|button| {
            let label = system::lookup(&button, "label")?.str()?.to_owned();
            let action = system::lookup(&button, "action")?.str()?.to_owned();
            Some(Action { label, activation: gtk_activation(&action, system::lookup(&button, "target")) })
        }).collect();
    }
    Some(notification)
}

fn gtk_activation(action: &str, target: Option<glib::Variant>) -> Activation {
    Activation::Gtk {
        action: action.strip_prefix("app.").map(str::to_owned),
        target,
    }
}

/// Activates `action` (or the application itself) on the GApplication
/// `app_id` through org.freedesktop.Application.
pub fn activate_gtk(conn: &gio::DBusConnection, app_id: &str, action: Option<&str>, target: Option<&glib::Variant>) {
    let path = format!("/{}", app_id.replace('.', "/").replace('-', "_"));
    let platform_data: HashMap<String, glib::Variant> = HashMap::new();
    let (method, args) = match action {
        Some(action) => {
            let targets: Vec<glib::Variant> = target.into_iter().cloned().collect();
            ("ActivateAction", (action, targets, platform_data).to_variant())
        }
        None => ("Activate", (platform_data,).to_variant()),
    };
    system::call(conn, app_id, &path, "org.freedesktop.Application", method, Some(args));
}
