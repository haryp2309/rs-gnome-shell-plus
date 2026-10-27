//! Notifications, like GNOME Shell's message tray: the notification
//! servers (server.rs), pop-up banners (banner.rs), the list in the date
//! menu (list.rs) and the unseen-notifications dot next to the clock.
//!
//! Honors org.gnome.desktop.notifications: `show-banners` is Do Not
//! Disturb, and the per-application settings decide whether an app may
//! notify at all and whether it gets banners.

mod banner;
mod card;
mod list;
mod markup;
mod model;
mod server;

use crate::system;
use banner::Banner;
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use model::{Activation, App, Notification, Origin};
use server::CloseReason;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

const SCHEMA: &str = "org.gnome.desktop.notifications";
const APP_SCHEMA: &str = "org.gnome.desktop.notifications.application";
const APP_PATH: &str = "/org/gnome/desktop/notifications/application";
/// Older notifications of an app expire beyond this, as in GNOME Shell.
const MAX_PER_APP: usize = 3;

pub struct Notifications {
    conn: Option<gio::DBusConnection>,
    settings: Option<gio::Settings>,
    /// Per-application settings by canonical id.
    policies: RefCell<HashMap<String, gio::Settings>>,
    /// Oldest first.
    items: RefCell<Vec<Rc<Notification>>>,
    next_id: Cell<u32>,
    banner: Rc<Banner>,
    menu_open: Cell<bool>,
    unseen: Cell<bool>,
    listeners: RefCell<Vec<Box<dyn Fn()>>>,
    indicator: Indicator,
}

impl Notifications {
    pub fn install(app: &adw::Application) -> Rc<Self> {
        load_css();
        let this = Rc::new_cyclic(|weak| Self {
            conn: system::session_bus(),
            settings: system::settings(SCHEMA),
            policies: RefCell::default(),
            items: RefCell::default(),
            next_id: Cell::new(1),
            banner: Banner::new(app, weak.clone()),
            menu_open: Cell::new(false),
            unseen: Cell::new(false),
            listeners: RefCell::default(),
            indicator: Indicator::new(),
        });
        if let Some(conn) = &this.conn {
            server::install(conn, Rc::downgrade(&this));
        }
        if let Some(settings) = &this.settings {
            settings.connect_changed(
                Some("show-banners"),
                glib::clone!(
                    #[weak]
                    this,
                    move |_, _| this.update_indicator()
                ),
            );
        }
        this.update_indicator();
        // Widgets and D-Bus handlers only hold weak references; the
        // application keeps the notifications alive for its whole lifetime.
        app.connect_shutdown(glib::clone!(
            #[strong]
            this,
            move |_| this.banner.hide()
        ));
        this
    }

    /// The date menu's left column.
    pub fn date_menu_section(self: &Rc<Self>) -> gtk::Widget {
        list::build(self)
    }

    /// Shown next to the clock: unseen dot, or the Do Not Disturb icon.
    pub fn indicator(self: &Rc<Self>) -> gtk::Widget {
        self.indicator.root.clone().upcast()
    }

    // --- the store -------------------------------------------------------

    fn get(&self, id: u32) -> Option<Rc<Notification>> {
        self.items.borrow().iter().find(|n| n.id == id).cloned()
    }

    fn contains(&self, id: u32) -> bool {
        self.items.borrow().iter().any(|n| n.id == id)
    }

    fn find(&self, origin: &Origin) -> Option<u32> {
        self.items.borrow().iter().find(|n| &n.origin == origin).map(|n| n.id)
    }

    /// Notifications for the date menu: newest first, without transient ones.
    fn listed(&self) -> Vec<Rc<Notification>> {
        self.items.borrow().iter().rev().filter(|n| !n.transient).cloned().collect()
    }

    fn connect_changed(&self, f: impl Fn() + 'static) {
        self.listeners.borrow_mut().push(Box::new(f));
    }

    fn changed(&self) {
        self.update_indicator();
        for listener in self.listeners.borrow().iter() {
            listener();
        }
    }

    fn new_id(&self) -> u32 {
        let id = self.next_id.get();
        self.next_id.set(id.checked_add(1).unwrap_or(1));
        id
    }

    /// Adds a notification, or replaces the one with the same id. Returns
    /// the notification's id.
    fn add(self: &Rc<Self>, mut notification: Notification) -> u32 {
        if notification.id == 0 {
            notification.id = self.new_id();
        }
        let id = notification.id;
        let policy = notification.app.id.as_deref().and_then(|app_id| self.policy(app_id));
        if policy.as_ref().is_some_and(|p| !p.boolean("enable")) {
            return id;
        }
        let app_banners = policy.is_none_or(|p| p.boolean("show-banners"));

        let notification = Rc::new(notification);
        let replaced = {
            let mut items = self.items.borrow_mut();
            match items.iter_mut().find(|n| n.id == id) {
                Some(slot) => {
                    *slot = notification.clone();
                    true
                }
                None => {
                    items.push(notification.clone());
                    false
                }
            }
        };
        if !replaced {
            self.expire_old(&notification.app);
        }
        if !notification.transient && !self.menu_open.get() {
            self.unseen.set(true);
        }

        if notification.critical || (self.banners_enabled() && app_banners) {
            self.banner.push(&notification);
        } else if notification.transient {
            // Nowhere to show it. Close after replying with the id.
            let weak = Rc::downgrade(self);
            glib::idle_add_local_once(move || {
                if let Some(this) = weak.upgrade() {
                    this.close(id, Some(CloseReason::Expired));
                }
            });
        }
        self.changed();
        id
    }

    fn expire_old(&self, app: &App) {
        let old: Vec<u32> = {
            let items = self.items.borrow();
            let same_app: Vec<u32> = items.iter().filter(|n| n.app.key() == app.key()).map(|n| n.id).collect();
            same_app[..same_app.len().saturating_sub(MAX_PER_APP)].to_vec()
        };
        for id in old {
            self.close(id, Some(CloseReason::Expired));
        }
    }

    /// Removes a notification. `reason` is reported to
    /// org.freedesktop.Notifications clients; None closes it silently.
    fn close(&self, id: u32, reason: Option<CloseReason>) {
        let removed = {
            let mut items = self.items.borrow_mut();
            items.iter().position(|n| n.id == id).map(|index| items.remove(index))
        };
        let Some(notification) = removed else { return };
        self.banner.remove(id);
        if let (Some(conn), Some(reason), Origin::Fdo) = (&self.conn, reason, &notification.origin) {
            server::emit_closed(conn, id, reason);
        }
        self.changed();
    }

    // --- user actions ----------------------------------------------------

    /// Clicking a notification: runs its default action and dismisses it.
    fn activate(&self, id: u32) {
        let Some(notification) = self.get(id) else { return };
        match &notification.default_action {
            Some(activation) => self.run(&notification, activation),
            None => launch(&notification.app),
        }
        self.done(&notification);
    }

    fn invoke_action(&self, id: u32, index: usize) {
        let Some(notification) = self.get(id) else { return };
        if let Some(action) = notification.actions.get(index) {
            self.run(&notification, &action.activation);
        }
        self.done(&notification);
    }

    /// The close button.
    fn dismiss(&self, id: u32) {
        self.close(id, Some(CloseReason::Dismissed));
    }

    fn clear(&self) {
        for notification in self.listed() {
            self.dismiss(notification.id);
        }
    }

    /// After an action ran: resident notifications stay, others go.
    fn done(&self, notification: &Notification) {
        if notification.resident {
            self.banner.remove(notification.id);
        } else {
            self.dismiss(notification.id);
        }
    }

    fn run(&self, notification: &Notification, activation: &Activation) {
        let Some(conn) = &self.conn else { return };
        match (activation, &notification.origin) {
            (Activation::Fdo(key), _) => server::emit_action_invoked(conn, notification.id, key),
            (Activation::Gtk { action, target }, Origin::Gtk { app_id, .. }) => {
                server::activate_gtk(conn, app_id, action.as_deref(), target.as_ref())
            }
            (Activation::Gtk { .. }, Origin::Fdo) => {}
        }
    }

    /// The banner timed out: transient notifications are gone with it.
    fn banner_expired(&self, id: u32) {
        if self.get(id).is_some_and(|n| n.transient) {
            self.close(id, Some(CloseReason::Expired));
        }
    }

    /// The date menu opened or closed. While it's open, everything counts
    /// as seen and banners are held back, as in GNOME Shell.
    fn set_menu_open(&self, open: bool) {
        self.menu_open.set(open);
        self.banner.set_blocked(open);
        if open {
            self.unseen.set(false);
            self.update_indicator();
        }
    }

    // --- settings --------------------------------------------------------

    fn banners_enabled(&self) -> bool {
        self.settings.as_ref().is_none_or(|s| s.boolean("show-banners"))
    }

    /// The per-application settings GNOME Settings shows, registering the
    /// app in `application-children` like GNOME Shell does.
    fn policy(&self, app_id: &str) -> Option<gio::Settings> {
        let canonical = canonical_id(app_id);
        if let Some(settings) = self.policies.borrow().get(&canonical) {
            return Some(settings.clone());
        }
        gio::SettingsSchemaSource::default()?.lookup(APP_SCHEMA, true)?;
        let settings = gio::Settings::with_path(APP_SCHEMA, &format!("{APP_PATH}/{canonical}/"));
        let desktop_id = format!("{app_id}.desktop");
        if settings.string("application-id") != desktop_id {
            let _ = settings.set_string("application-id", &desktop_id);
        }
        if let Some(global) = &self.settings {
            let mut children: Vec<String> = global.strv("application-children").iter().map(|s| s.to_string()).collect();
            if !children.contains(&canonical) {
                children.push(canonical.clone());
                let _ = global.set_strv("application-children", children);
            }
        }
        self.policies.borrow_mut().insert(canonical, settings.clone());
        Some(settings)
    }

    fn update_indicator(&self) {
        let dnd = !self.banners_enabled();
        self.indicator.update(self.unseen.get() && !dnd, dnd);
    }
}

/// GNOME's settings path component for an app id: lowercase alphanumerics
/// and single dashes.
fn canonical_id(app_id: &str) -> String {
    let mut out = String::with_capacity(app_id.len());
    for c in app_id.to_lowercase().chars() {
        let c = if c.is_ascii_alphanumeric() || c == '-' { c } else { '-' };
        if !(c == '-' && out.ends_with('-')) {
            out.push(c);
        }
    }
    out
}

/// Clicking a notification without a default action opens its app.
fn launch(app: &App) {
    if let Some(info) = &app.info {
        if let Err(err) = info.launch(&[], gio::AppLaunchContext::NONE) {
            glib::g_warning!("shellplus", "Failed to launch {}: {err}", app.name);
        }
    }
}

/// The unseen-notifications dot, or the Do Not Disturb icon.
struct Indicator {
    root: gtk::Box,
    dot: gtk::Box,
    dnd: gtk::Image,
}

impl Indicator {
    fn new() -> Self {
        let root = gtk::Box::builder()
            .name("notification-indicator")
            .valign(gtk::Align::Center)
            .css_classes(["notification-indicator"])
            .build();
        let dot = gtk::Box::builder()
            .valign(gtk::Align::Center)
            .css_classes(["unseen-dot"])
            .build();
        let dnd = gtk::Image::from_icon_name("notifications-disabled-symbolic");
        root.append(&dot);
        root.append(&dnd);
        Self { root, dot, dnd }
    }

    fn update(&self, unseen: bool, dnd: bool) {
        self.dot.set_visible(unseen);
        self.dnd.set_visible(dnd);
        // Hidden entirely so the clock doesn't keep the spacing for it.
        self.root.set_visible(unseen || dnd);
    }
}

fn load_css() {
    let Some(display) = gdk::Display::default() else { return };
    let provider = gtk::CssProvider::new();
    provider.load_from_string(include_str!("style.css"));
    gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
}

#[cfg(test)]
mod tests {
    #[test]
    fn canonical_ids() {
        assert_eq!(super::canonical_id("org.gnome.Nautilus"), "org-gnome-nautilus");
        assert_eq!(super::canonical_id("my_app..x"), "my-app-x");
    }
}
