//! gnome-session's end-session prompt: `org.gnome.SessionManager.EndSessionDialog`
//! (GNOME Shell's js/ui/endSessionDialog.js).
//!
//! Under gnome-session, Log Out / Restart / Power Off ask gnome-session,
//! which calls Open(type, timestamp, seconds_to_stay_open,
//! inhibitor_object_paths) on `/org/gnome/SessionManager/EndSessionDialog`
//! of whoever owns org.gnome.Shell. That's the compositor; it forwards the
//! calls to this object (on `org.gnome.ShellPlus.EndSessionDialog`) and
//! re-emits its signals (compositor/src/shellplus-end-session.c).
//!
//! The dialog counts down and then confirms on its own, lists the apps
//! inhibiting logout (with their reasons) and, for restart/power off, the
//! other users who are logged in. Confirming emits ConfirmedLogout /
//! ConfirmedReboot / ConfirmedShutdown; Cancel or Escape emits Canceled
//! and Closed; Close() closes it and emits Closed.

use crate::system;
use crate::system_dialog::SystemDialog;
use adw::prelude::*;
use gtk::{gio, glib};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

const NAME: &str = "org.gnome.ShellPlus.EndSessionDialog";
const PATH: &str = "/org/gnome/SessionManager/EndSessionDialog";
const IFACE: &str = "org.gnome.SessionManager.EndSessionDialog";
const XML: &str = r#"
<node>
  <interface name="org.gnome.SessionManager.EndSessionDialog">
    <method name="Open">
      <arg type="u" name="type" direction="in"/>
      <arg type="u" name="timestamp" direction="in"/>
      <arg type="u" name="seconds_to_stay_open" direction="in"/>
      <arg type="ao" name="inhibitor_object_paths" direction="in"/>
    </method>
    <method name="Close"/>
    <signal name="ConfirmedLogout"/>
    <signal name="ConfirmedReboot"/>
    <signal name="ConfirmedShutdown"/>
    <signal name="Canceled"/>
    <signal name="Closed"/>
  </interface>
</node>"#;

const SESSION_MANAGER: &str = "org.gnome.SessionManager";
const INHIBITOR: &str = "org.gnome.SessionManager.Inhibitor";
/// GSM_INHIBITOR_FLAG_LOGOUT
const INHIBIT_LOGOUT: u32 = 1;

const LOGIND: &str = "org.freedesktop.login1";
const ACCOUNTS: &str = "org.freedesktop.Accounts";

const MAX_USERS_IN_SESSION_DIALOG: usize = 5;
const ICON_SIZE: i32 = 32;

/// The dialog types gnome-session asks for (GsmShellEndSessionDialogType),
/// plus GNOME Shell's own "restart and install updates".
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Logout,
    Shutdown,
    Restart,
    UpdateRestart,
}

impl Kind {
    fn from_u32(value: u32) -> Option<Self> {
        Some(match value {
            0 => Self::Logout,
            1 => Self::Shutdown,
            2 => Self::Restart,
            3 => Self::UpdateRestart,
            _ => return None,
        })
    }

    fn subject(self, user: Option<&str>) -> String {
        match (self, user) {
            (Self::Logout, Some(user)) => format!("Log Out {user}"),
            (Self::Logout, None) => "Log Out".into(),
            (Self::Shutdown, _) => "Power Off".into(),
            (Self::Restart, _) => "Restart".into(),
            (Self::UpdateRestart, _) => "Restart & Install Updates".into(),
        }
    }

    fn description(self, user: Option<&str>, seconds: u32) -> String {
        let unit = if seconds == 1 { "second" } else { "seconds" };
        match (self, user) {
            (Self::Logout, Some(user)) => format!("{user} will be logged out automatically in {seconds} {unit}"),
            (Self::Logout, None) => format!("You will be logged out automatically in {seconds} {unit}"),
            (Self::Shutdown, _) => format!("The system will power off automatically in {seconds} {unit}"),
            (Self::Restart, _) => format!("The system will restart automatically in {seconds} {unit}"),
            (Self::UpdateRestart, _) => {
                format!("The system will automatically restart and install updates in {seconds} {unit}")
            }
        }
    }

    fn confirm_label(self) -> &'static str {
        match self {
            Self::Logout => "Log Out",
            Self::Shutdown => "Power Off",
            Self::Restart => "Restart",
            Self::UpdateRestart => "Restart & Install",
        }
    }

    fn signal(self) -> &'static str {
        match self {
            Self::Logout => "ConfirmedLogout",
            Self::Shutdown => "ConfirmedShutdown",
            Self::Restart | Self::UpdateRestart => "ConfirmedReboot",
        }
    }

    fn shows_other_sessions(self) -> bool {
        self != Self::Logout
    }
}

/// GNOME's countdown: in steps of 10 seconds, then every second for the
/// last 10 (`_roundSecondsToInterval`).
fn rounded_seconds(total: u32, left: f64, interval: u32) -> u32 {
    let time = left.ceil().max(0.0) as u32;
    if time <= interval {
        return time;
    }
    let time = time + interval - 1;
    if time > total {
        total
    } else {
        time - time % interval
    }
}

struct Open {
    dialog: Rc<SystemDialog>,
    kind: Kind,
    timer: Option<glib::SourceId>,
}

struct EndSession {
    app: adw::Application,
    conn: gio::DBusConnection,
    current: RefCell<Option<Open>>,
}

pub fn install(app: &adw::Application) {
    let Some(conn) = system::session_bus() else { return };
    let this = Rc::new(EndSession { app: app.clone(), conn: conn.clone(), current: RefCell::default() });

    let node = gio::DBusNodeInfo::for_xml(XML).expect("valid introspection XML");
    let iface = node.lookup_interface(IFACE).expect("interface in XML");
    let registration = conn
        .register_object(PATH, &iface)
        .method_call(glib::clone!(
            #[strong]
            this,
            move |_, _, _, _, method, params, invocation| match method {
                "Open" => this.open(&params, invocation),
                "Close" => {
                    this.close_with_signal();
                    invocation.return_value(None);
                }
                _ => invocation.return_error(gio::DBusError::UnknownMethod, method),
            }
        ))
        .build();
    if let Err(err) = registration {
        glib::g_warning!("shellplus", "Can't export the end session dialog: {err}");
        return;
    }
    gio::bus_own_name_on_connection(
        &conn,
        NAME,
        gio::BusNameOwnerFlags::REPLACE,
        |_, _| {},
        |_, name| glib::g_warning!("shellplus", "Could not own {name}; gnome-session can't show its log out dialog"),
    );
}

impl EndSession {
    fn emit(&self, signal: &str) {
        if let Err(err) = self.conn.emit_signal(None, PATH, IFACE, signal, None) {
            glib::g_warning!("shellplus", "Could not emit {signal}: {err}");
        }
    }

    /// Takes the open dialog (if any) down without emitting anything.
    fn dismiss(&self) -> Option<Kind> {
        let open = self.current.take()?;
        if let Some(timer) = open.timer {
            timer.remove();
        }
        open.dialog.close();
        Some(open.kind)
    }

    fn close_with_signal(&self) {
        self.dismiss();
        self.emit("Closed");
    }

    fn cancel(&self) {
        self.dismiss();
        self.emit("Canceled");
        self.emit("Closed");
    }

    /// Confirming closes the dialog without Closed, like GNOME.
    fn confirm(&self) {
        if let Some(kind) = self.dismiss() {
            self.emit(kind.signal());
        }
    }

    fn open(self: &Rc<Self>, params: &glib::Variant, invocation: gio::DBusMethodInvocation) {
        let Some((kind, _timestamp, seconds, inhibitors)) = params.get::<(u32, u32, u32, Vec<glib::variant::ObjectPath>)>()
        else {
            invocation.return_error(gio::DBusError::InvalidArgs, "Bad arguments");
            return;
        };
        let Some(kind) = Kind::from_u32(kind) else {
            invocation.return_dbus_error(
                "org.gnome.Shell.ModalDialog.Error.UnknownType",
                "Unknown dialog type requested",
            );
            return;
        };
        // A second Open replaces the first, as GNOME reuses its dialog.
        self.dismiss();

        let user = real_name();
        let dialog = SystemDialog::new(&self.app, "end-session-dialog");
        dialog.set_heading(&kind.subject(user.as_deref()));
        dialog.set_body(&kind.description(user.as_deref(), seconds));

        let apps = Section::new("end-session-apps", "Some applications are busy or have unsaved work");
        let sessions = Section::new("end-session-sessions", "Other users are logged in");
        dialog.add_content(&apps.root);
        dialog.add_content(&sessions.root);

        let cancel = dialog.add_button("end-session-cancel", "Cancel", false);
        let confirm = dialog.add_button("end-session-confirm", kind.confirm_label(), false);
        let weak = Rc::downgrade(self);
        cancel.connect_clicked(glib::clone!(
            #[strong]
            weak,
            move |_| {
                if let Some(this) = weak.upgrade() {
                    this.cancel();
                }
            }
        ));
        confirm.connect_clicked(glib::clone!(
            #[strong]
            weak,
            move |_| {
                if let Some(this) = weak.upgrade() {
                    this.confirm();
                }
            }
        ));
        // Escape or closing the window.
        dialog.connect_cancel(glib::clone!(
            #[strong]
            weak,
            move || {
                if let Some(this) = weak.upgrade() {
                    this.cancel();
                }
            }
        ));

        let timer = self.start_timer(&dialog, kind, user, seconds);
        dialog.present(Some(&cancel));
        self.current.replace(Some(Open { dialog: dialog.clone(), kind, timer: Some(timer) }));
        invocation.return_value(None);

        // After replying: the inhibitors live in gnome-session, which may be
        // waiting for this reply.
        load_inhibitors(&self.conn, &inhibitors, &apps);
        if kind.shows_other_sessions() {
            glib::idle_add_local_once(move || load_sessions(&sessions));
        }
    }

    fn start_timer(self: &Rc<Self>, dialog: &Rc<SystemDialog>, kind: Kind, user: Option<String>, total: u32) -> glib::SourceId {
        let start = Instant::now();
        let weak = Rc::downgrade(self);
        let dialog = Rc::downgrade(dialog);
        glib::timeout_add_local(Duration::from_secs(1), move || {
            let (Some(this), Some(dialog)) = (weak.upgrade(), dialog.upgrade()) else {
                return glib::ControlFlow::Break;
            };
            let left = f64::from(total) - start.elapsed().as_secs_f64();
            if left > 0.0 {
                dialog.set_body(&kind.description(user.as_deref(), rounded_seconds(total, left, 10)));
                return glib::ControlFlow::Continue;
            }
            // The source is ending; don't let dismiss() remove it.
            if let Some(open) = this.current.borrow_mut().as_mut() {
                open.timer = None;
            }
            this.confirm();
            glib::ControlFlow::Break
        })
    }
}

/// A titled list in the dialog ("Some applications are busy…"), hidden
/// while empty.
struct Section {
    root: gtk::Box,
    list: gtk::Box,
    name: String,
    count: std::cell::Cell<usize>,
}

impl Section {
    fn new(name: &str, title: &str) -> Rc<Self> {
        let root = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .name(name)
            .visible(false)
            .css_classes(["end-session-list"])
            .build();
        let heading = gtk::Label::builder()
            .label(title)
            .wrap(true)
            .xalign(0.0)
            .css_classes(["list-title"])
            .build();
        let list = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(6).build();
        root.append(&heading);
        root.append(&list);
        Rc::new(Self { root, list, name: name.to_owned(), count: Default::default() })
    }

    fn add(&self, icon: &gio::Icon, title: &str, description: Option<&str>) {
        let index = self.count.get();
        self.count.set(index + 1);
        let row = gtk::Box::builder()
            .spacing(12)
            .name(format!("{}-{index}", self.name))
            .css_classes(["end-session-item"])
            .build();
        let image = gtk::Image::from_gicon(icon);
        image.set_pixel_size(ICON_SIZE);
        row.append(&image);
        let text = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .valign(gtk::Align::Center)
            .build();
        text.append(
            &gtk::Label::builder()
                .label(title)
                .name(format!("{}-{index}-title", self.name))
                .xalign(0.0)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .css_classes(["title"])
                .build(),
        );
        if let Some(description) = description.filter(|d| !d.is_empty()) {
            text.append(
                &gtk::Label::builder()
                    .label(description)
                    .name(format!("{}-{index}-description", self.name))
                    .xalign(0.0)
                    .wrap(true)
                    .css_classes(["dim-label", "caption"])
                    .build(),
            );
        }
        row.append(&text);
        self.list.append(&row);
        self.root.set_visible(true);
    }
}

/// Asks gnome-session about each inhibitor and lists the apps that inhibit
/// logout. Like GNOME, services without an app (no desktop file) and
/// inhibitors of other things (suspend, idle) are left out.
fn load_inhibitors(conn: &gio::DBusConnection, paths: &[glib::variant::ObjectPath], section: &Rc<Section>) {
    for path in paths {
        let path = path.as_str().to_owned();
        // Strong: nothing else keeps the Section (only its widgets live on).
        let section = section.clone();
        let conn2 = conn.clone();
        call(conn, SESSION_MANAGER, &path.clone(), INHIBITOR, "GetAppId", move |reply| {
            let Some(app_id) = reply.and_then(|r| r.child_value(0).str().map(str::to_owned)) else {
                glib::g_message!("shellplus", "gnome-session gave us a dead inhibitor: {path}");
                return;
            };
            let Some(app) = find_app(&app_id) else { return };
            let conn3 = conn2.clone();
            call(&conn2, SESSION_MANAGER, &path.clone(), INHIBITOR, "GetFlags", move |reply| {
                let flags = reply.and_then(|r| r.child_value(0).get::<u32>()).unwrap_or(0);
                if flags & INHIBIT_LOGOUT == 0 {
                    return;
                }
                call(&conn3, SESSION_MANAGER, &path, INHIBITOR, "GetReason", move |reply| {
                    let reason = reply.and_then(|r| r.child_value(0).str().map(str::to_owned));
                    let icon = app
                        .icon()
                        .unwrap_or_else(|| gio::ThemedIcon::new("application-x-executable").upcast());
                    section.add(&icon, &app.name(), reason.as_deref());
                });
            });
        });
    }
}

/// The app for an inhibitor's app id, which is a desktop file name with or
/// without ".desktop" (GNOME's `lookup_heuristic_basename`, simplified: the
/// exact id, else a vendor-prefixed one like `org.example-<id>`). Goes
/// through `gio::AppInfo` as `DesktopAppInfo` is in the gio-unix crate.
fn find_app(app_id: &str) -> Option<gio::AppInfo> {
    let id = if app_id.ends_with(".desktop") { app_id.to_owned() } else { format!("{app_id}.desktop") };
    let id = id.rsplit('/').next().unwrap_or_default().to_owned();
    let apps = gio::AppInfo::all();
    apps.iter()
        .find(|info| info.id().is_some_and(|i| i == id.as_str()))
        .or_else(|| apps.iter().find(|info| info.id().is_some_and(|i| i.ends_with(&format!("-{id}")))))
        .cloned()
}

fn call(
    conn: &gio::DBusConnection,
    name: &str,
    path: &str,
    iface: &str,
    method: &str,
    f: impl FnOnce(Option<glib::Variant>) + 'static,
) {
    conn.call(
        Some(name),
        path,
        iface,
        method,
        None,
        None,
        gio::DBusCallFlags::NONE,
        5000,
        gio::Cancellable::NONE,
        move |result| f(result.ok()),
    );
}

/// Other users' sessions from logind, as GNOME lists them before restart
/// or power off.
fn load_sessions(section: &Section) {
    let Some(bus) = system::system_bus() else { return };
    let own_id = std::env::var("XDG_SESSION_ID").unwrap_or_default();
    let Some(reply) = system::call_sync(&bus, LOGIND, "/org/freedesktop/login1", "org.freedesktop.login1.Manager", "ListSessions", None)
    else {
        return;
    };
    let mut shown = 0;
    for session in reply.child_value(0).iter() {
        let Some((_, _, user_name, _, path)) = session.get::<(String, u32, String, String, glib::variant::ObjectPath)>()
        else {
            continue;
        };
        let Some(props) = system::get_all(&bus, LOGIND, path.as_str(), "org.freedesktop.login1.Session") else {
            continue;
        };
        if props.string("Class") != "user" || props.string("State") == "closing" || props.string("Id") == own_id {
            continue;
        }
        let display_name = user_real_name(&bus, &user_name).unwrap_or_else(|| user_name.clone());
        let label = if props.get::<bool>("Remote").unwrap_or(false) {
            format!("{display_name} (remote)")
        } else if props.string("Type") == "tty" {
            format!("{display_name} (console)")
        } else {
            user_name
        };
        section.add(&gio::ThemedIcon::new("avatar-default-symbolic").upcast(), &label, None);
        shown += 1;
        if shown == MAX_USERS_IN_SESSION_DIALOG {
            break;
        }
    }
}

fn user_real_name(bus: &gio::DBusConnection, user: &str) -> Option<String> {
    let path = system::call_sync(
        bus,
        ACCOUNTS,
        "/org/freedesktop/Accounts",
        ACCOUNTS,
        "FindUserByName",
        Some(&(user,).to_variant()),
    )?
    .child_value(0)
    .str()?
    .to_owned();
    Some(system::get_all(bus, ACCOUNTS, &path, "org.freedesktop.Accounts.User")?.string("RealName"))
        .filter(|name| !name.is_empty())
}

/// The user's real name (AccountsService, else passwd), if they have one.
fn real_name() -> Option<String> {
    let user = glib::user_name().to_string_lossy().into_owned();
    system::system_bus()
        .and_then(|bus| user_real_name(&bus, &user))
        .or_else(|| {
            let name = glib::real_name().to_string_lossy().into_owned();
            Some(name).filter(|n| !n.is_empty() && n != "Unknown")
        })
}

#[cfg(test)]
mod tests {
    use super::rounded_seconds;

    #[test]
    fn countdown_rounds_like_gnome() {
        assert_eq!(rounded_seconds(60, 60.0, 10), 60);
        assert_eq!(rounded_seconds(60, 59.0, 10), 60);
        assert_eq!(rounded_seconds(60, 50.5, 10), 60);
        assert_eq!(rounded_seconds(60, 50.0, 10), 50);
        assert_eq!(rounded_seconds(60, 41.0, 10), 50);
        assert_eq!(rounded_seconds(60, 10.0, 10), 10);
        assert_eq!(rounded_seconds(60, 3.2, 10), 4);
    }
}
