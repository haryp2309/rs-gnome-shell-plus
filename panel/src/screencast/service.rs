//! org.gnome.Shell.Screencast on /org/gnome/Shell/Screencast, as GNOME
//! Shell exports it (data/dbus-interfaces/org.gnome.Shell.Screencast.xml):
//! Screencast, ScreencastArea, StopScreencast, the ScreencastSupported
//! property and the Error signal.
//!
//! Who may call it: the same rule as org.gnome.Shell.Screenshot in the
//! compositor (executable_allowed() in compositor/src/shellplus-screenshot.c).
//! The sender's pid → /proc/<pid>/exe must have an allowed basename, live
//! under /usr/, still exist, be owned by root and not be group/other
//! writable. SHELLPLUS_SCREENSHOT_ALLOW_ALL=1 turns the check off (tests).
//! The panel checks it itself: it's the one recording, and the compositor
//! has nothing to add but the same /proc lookup.
//!
//! A recording belongs to the client that started it: only that client
//! can stop it, and it stops when that client leaves the bus.
//! `options`: draw-cursor (b, default true) and framerate (i, default 30);
//! `pipeline` is not supported (the encoder is always WebM). A relative
//! file_template goes to the XDG videos directory (else ~), %d and %t
//! become the date and time, and ".webm" is appended when missing.

use super::Screencasts;
use gtk::prelude::*;
use gtk::{gio, glib};
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::rc::Weak;

pub const NAME: &str = "org.gnome.Shell.Screencast";
const PATH: &str = "/org/gnome/Shell/Screencast";

const XML: &str = r#"
<node>
  <interface name="org.gnome.Shell.Screencast">
    <property name="ScreencastSupported" type="b" access="read"/>
    <method name="Screencast">
      <arg type="s" direction="in" name="file_template"/>
      <arg type="a{sv}" direction="in" name="options"/>
      <arg type="b" direction="out" name="success"/>
      <arg type="s" direction="out" name="filename_used"/>
    </method>
    <method name="ScreencastArea">
      <arg type="i" direction="in" name="x"/>
      <arg type="i" direction="in" name="y"/>
      <arg type="i" direction="in" name="width"/>
      <arg type="i" direction="in" name="height"/>
      <arg type="s" direction="in" name="file_template"/>
      <arg type="a{sv}" direction="in" name="options"/>
      <arg type="b" direction="out" name="success"/>
      <arg type="s" direction="out" name="filename_used"/>
    </method>
    <method name="StopScreencast">
      <arg type="b" direction="out" name="success"/>
    </method>
    <signal name="Error">
      <arg type="s" name="name"/>
      <arg type="s" name="message"/>
    </signal>
  </interface>
</node>"#;

const ALLOWED_EXECUTABLES: [&str; 4] =
    ["xdg-desktop-portal-gnome", "xdg-desktop-portal-gtk", "gnome-screenshot", "gsd-media-keys"];

pub fn export(conn: &gio::DBusConnection, screencasts: Weak<Screencasts>) {
    let node = gio::DBusNodeInfo::for_xml(XML).expect("valid introspection XML");
    let iface = node.lookup_interface(NAME).expect("interface in XML");
    let registration = conn
        .register_object(PATH, &iface)
        .method_call(move |conn, sender, _, _, method, params, invocation| {
            let Some(screencasts) = screencasts.upgrade() else {
                invocation.return_error(gio::IOErrorEnum::Failed, "Shutting down");
                return;
            };
            let sender = sender.map(str::to_owned).unwrap_or_default();
            let method = method.to_owned();
            check_sender(&conn, &sender.clone(), move |allowed| {
                if allowed {
                    handle(&screencasts, &sender, &method, &params, invocation);
                } else {
                    glib::g_message!("shellplus", "Refused {method} from {sender}");
                    invocation.return_dbus_error(
                        "org.freedesktop.DBus.Error.AccessDenied",
                        &format!("{method} is not allowed"),
                    );
                }
            });
        })
        // ScreencastSupported is the only property.
        .property(|_, _, _, _, _| true.to_variant())
        .build();
    if let Err(err) = registration {
        glib::g_warning!("shellplus", "Could not export {NAME}: {err}");
        return;
    }
    if allow_all() {
        glib::g_message!("shellplus", "SHELLPLUS_SCREENSHOT_ALLOW_ALL=1: anyone may record the screen");
    }
    gio::bus_own_name_on_connection(
        conn,
        NAME,
        gio::BusNameOwnerFlags::REPLACE,
        |_, _| {},
        |_, name| glib::g_warning!("shellplus", "Could not own {name}; screen recording tools won't work"),
    );
}

pub fn emit_error(conn: &gio::DBusConnection, name: &str, message: &str) {
    let _ = conn.emit_signal(None, PATH, NAME, "Error", Some(&(name, message).to_variant()));
}

fn allow_all() -> bool {
    std::env::var("SHELLPLUS_SCREENSHOT_ALLOW_ALL").as_deref() == Ok("1")
}

/// See the comment at the top.
fn executable_allowed(pid: u32) -> bool {
    let link = format!("/proc/{pid}/exe");
    let Ok(exe) = std::fs::read_link(&link) else { return false };
    let Some(exe) = exe.to_str() else { return false };
    if !exe.starts_with("/usr/") || exe.ends_with(" (deleted)") {
        return false;
    }
    let base = exe.rsplit('/').next().unwrap_or_default();
    if !ALLOWED_EXECUTABLES.contains(&base) {
        return false;
    }
    // Through the link, so it's the very file the process runs.
    match std::fs::metadata(&link) {
        Ok(meta) => meta.uid() == 0 && meta.mode() & 0o022 == 0,
        Err(_) => false,
    }
}

fn check_sender(conn: &gio::DBusConnection, sender: &str, then: impl FnOnce(bool) + 'static) {
    if allow_all() {
        then(true);
        return;
    }
    conn.call(
        Some("org.freedesktop.DBus"),
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
        "GetConnectionUnixProcessID",
        Some(&(sender,).to_variant()),
        Some(glib::VariantTy::new("(u)").unwrap()),
        gio::DBusCallFlags::NONE,
        -1,
        gio::Cancellable::NONE,
        move |result| {
            let pid = result.ok().and_then(|r| r.get::<(u32,)>()).map(|(pid,)| pid).unwrap_or(0);
            then(pid != 0 && executable_allowed(pid));
        },
    );
}

fn handle(
    screencasts: &Screencasts,
    sender: &str,
    method: &str,
    params: &glib::Variant,
    invocation: gio::DBusMethodInvocation,
) {
    let (area, template, options) = match method {
        "Screencast" => {
            let Some((template, options)) = params.get::<(String, glib::VariantDict)>() else {
                return invocation.return_error(gio::IOErrorEnum::InvalidArgument, "Bad arguments");
            };
            (None, template, options)
        }
        "ScreencastArea" => {
            let Some((x, y, w, h, template, options)) =
                params.get::<(i32, i32, i32, i32, String, glib::VariantDict)>()
            else {
                return invocation.return_error(gio::IOErrorEnum::InvalidArgument, "Bad arguments");
            };
            (Some((x, y, w, h)), template, options)
        }
        "StopScreencast" => {
            screencasts.stop_for(sender, move |stopped| invocation.return_value(Some(&(stopped,).to_variant())));
            return;
        }
        _ => return invocation.return_error(gio::IOErrorEnum::NotSupported, &format!("Unknown method {method}")),
    };

    let screen = screencasts.screen_rect();
    let area = match area {
        None => screen,
        Some((x, y, w, h)) => {
            let (sx, sy, sw, sh) = screen;
            if w <= 0 || h <= 0 || x < sx || y < sy || x + w > sx + sw || y + h > sy + sh {
                return invocation.return_error(gio::IOErrorEnum::Cancelled, "Invalid params");
            }
            (x, y, w, h)
        }
    };
    let cursor = options.lookup::<bool>("draw-cursor").ok().flatten().unwrap_or(true);
    let framerate = options.lookup::<i32>("framerate").ok().flatten().unwrap_or(30);
    let Some(path) = file_path(&template) else {
        return invocation.return_value(Some(&(false, "").to_variant()));
    };
    let used = path.to_string_lossy().into_owned();
    screencasts.start(
        area,
        cursor,
        framerate,
        path,
        super::Owner::Bus(sender.to_owned()),
        move |result| match result {
            Ok(()) => invocation.return_value(Some(&(true, used).to_variant())),
            Err(_) => invocation.return_value(Some(&(false, "").to_variant())),
        },
    );
}

/// The file for a template (see the comment at the top).
fn file_path(template: &str) -> Option<PathBuf> {
    let now = glib::DateTime::now_local().ok()?;
    let date = now.format("%Y-%m-%d").ok()?.to_string();
    let time = now.format("%H-%M-%S").ok()?.to_string();
    let mut name = String::new();
    let mut chars = template.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            name.push(c);
            continue;
        }
        match chars.next() {
            Some('d') => name.push_str(&date),
            Some('t') => name.push_str(&time),
            Some('%') => name.push('%'),
            Some(other) => {
                name.push('%');
                name.push(other);
            }
            None => name.push('%'),
        }
    }
    if name.is_empty() {
        return None;
    }
    if !name.ends_with(".webm") {
        name.push_str(".webm");
    }
    let path = PathBuf::from(&name);
    if path.is_absolute() {
        return Some(path);
    }
    let home = glib::home_dir();
    let videos = glib::user_special_dir(glib::UserDirectory::Videos)
        .filter(|dir| !dir.as_os_str().is_empty() && dir.is_dir())
        .unwrap_or(home);
    Some(videos.join(path))
}
