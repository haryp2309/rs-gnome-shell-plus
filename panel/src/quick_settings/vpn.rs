//! The VPN toggle and its panel icon, like GNOME Shell's NMVpnToggle: shown
//! while NetworkManager has VPN or WireGuard profiles. Turning it on brings
//! up the most recently used one, off takes down every active VPN; the menu
//! has a switch per profile and "VPN Settings".

use super::network::{self, ActiveConnection, ACTIVE_ACTIVATED, ACTIVE_ACTIVATING};
use super::widgets::QuickToggle;
use super::Ctx;
use crate::system;
use gtk::prelude::*;
use gtk::{gio, glib, pango};
use std::cell::RefCell;
use std::cmp::Reverse;
use std::rc::Rc;

/// `connection.type`s the toggle covers.
const TYPES: [&str; 2] = ["vpn", "wireguard"];

#[derive(Clone, PartialEq)]
struct Vpn {
    path: String,
    id: String,
    timestamp: u64,
}

/// What a menu row shows, to rebuild the menu only on change.
#[derive(PartialEq)]
struct Row {
    path: String,
    id: String,
    on: bool,
}

struct Section {
    ctx: Rc<Ctx>,
    conn: gio::DBusConnection,
    toggle: Rc<QuickToggle>,
    /// VPN profiles, sorted by name like GNOME's menu.
    vpns: RefCell<Vec<Vpn>>,
    /// Active or activating VPN connections.
    active: RefCell<Vec<ActiveConnection>>,
    listed: RefCell<Vec<Row>>,
}

pub fn build(ctx: &Rc<Ctx>) -> Option<Rc<QuickToggle>> {
    let conn = system::system_bus()?;
    let toggle = QuickToggle::with_menu("qs-vpn", "network-vpn-symbolic", "VPN", &ctx.host);
    toggle.set_visible(false);

    let section = Rc::new(Section {
        ctx: ctx.clone(),
        conn: conn.clone(),
        toggle: toggle.clone(),
        vpns: RefCell::default(),
        active: RefCell::default(),
        listed: RefCell::default(),
    });

    if let Some(menu) = &toggle.menu {
        menu.add_footer("qs-vpn-settings", "VPN Settings", || system::open_settings("network"));
    }
    toggle.connect_clicked(glib::clone!(
        #[weak]
        section,
        move || section.toggle()
    ));
    network::watch(
        &conn,
        glib::clone!(
            #[weak]
            section,
            move |profiles_changed| section.refresh(profiles_changed)
        ),
    );

    // Handlers only hold weak references; live as long as the toggle.
    toggle.root.connect_destroy(move |_| {
        let _ = &section;
    });
    Some(toggle)
}

impl Section {
    fn refresh(self: &Rc<Self>, profiles_changed: bool) {
        if profiles_changed {
            let mut vpns: Vec<Vpn> = network::profiles(&self.conn)
                .into_iter()
                .filter(|p| TYPES.contains(&p.kind.as_str()))
                .map(|p| Vpn {
                    path: p.path,
                    id: p.id,
                    timestamp: p.timestamp,
                })
                .collect();
            vpns.sort_by(|a, b| a.id.to_lowercase().cmp(&b.id.to_lowercase()));
            self.vpns.replace(vpns);
        }
        let mut active: Vec<ActiveConnection> = network::active_connections(&self.conn)
            .into_iter()
            .filter(|a| TYPES.contains(&a.kind.as_str()) && matches!(a.state, ACTIVE_ACTIVATING | ACTIVE_ACTIVATED))
            .collect();
        // The activated one names the toggle, else the one coming up.
        active.sort_by_key(|a| a.state != ACTIVE_ACTIVATED);

        let shown = !self.vpns.borrow().is_empty();
        self.toggle.set_visible(shown);
        let current = active.first();
        let icon = match current.map(|a| a.state) {
            Some(ACTIVE_ACTIVATED) => "network-vpn-symbolic",
            Some(_) => "network-vpn-acquiring-symbolic",
            None => "network-vpn-disabled-symbolic",
        };
        self.toggle.set_checked(current.is_some());
        self.toggle.set_icon(icon);
        self.toggle.set_subtitle(current.map(|a| a.id.as_str()));
        if let Some(menu) = &self.toggle.menu {
            menu.set_icon("network-vpn-symbolic");
        }

        let indicator = &self.ctx.indicators.vpn;
        indicator.set_icon_name(Some(icon));
        indicator.set_visible(shown && current.is_some());

        self.active.replace(active);
        self.update_menu();
    }

    fn is_on(&self, profile: &str) -> bool {
        self.active.borrow().iter().any(|a| a.profile == profile)
    }

    fn update_menu(self: &Rc<Self>) {
        let Some(menu) = &self.toggle.menu else { return };
        let rows: Vec<Row> = self
            .vpns
            .borrow()
            .iter()
            .map(|vpn| Row {
                path: vpn.path.clone(),
                id: vpn.id.clone(),
                on: self.is_on(&vpn.path),
            })
            .collect();
        if *self.listed.borrow() == rows {
            return;
        }
        menu.clear();
        for (i, row) in rows.iter().enumerate() {
            let item = switch_item(&format!("qs-vpn-item-{i}"), &row.id, row.on);
            let path = row.path.clone();
            item.connect_clicked(glib::clone!(
                #[weak(rename_to = section)]
                self,
                move |_| section.pick(&path)
            ));
            menu.add(&item);
        }
        self.listed.replace(rows);
    }

    fn toggle(&self) {
        if self.toggle.is_checked() {
            for active in self.active.borrow().iter() {
                network::deactivate(&self.conn, &active.path);
            }
        } else if let Some(recent) = self.vpns.borrow().iter().min_by_key(|v| Reverse(v.timestamp)) {
            // The most recently used one (the first listed if none was).
            network::activate(&self.conn, &recent.path, "/", "/");
        }
    }

    /// A menu row: switches that VPN off, or switches over to it.
    fn pick(&self, profile: &str) {
        let active = self.active.borrow();
        if let Some(on) = active.iter().find(|a| a.profile == profile) {
            network::deactivate(&self.conn, &on.path);
            return;
        }
        for other in active.iter() {
            network::deactivate(&self.conn, &other.path);
        }
        network::activate(&self.conn, profile, "/", "/");
    }
}

/// A menu row with a trailing switch, like GNOME's PopupSwitchMenuItem.
/// The whole row is the button; the switch only shows the state.
fn switch_item(name: &str, label: &str, on: bool) -> gtk::Button {
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    content.append(
        &gtk::Label::builder()
            .name(format!("{name}-label"))
            .label(label)
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(pango::EllipsizeMode::End)
            .build(),
    );
    content.append(
        &gtk::Switch::builder()
            .name(format!("{name}-switch"))
            .active(on)
            .valign(gtk::Align::Center)
            .can_target(false)
            .can_focus(false)
            .build(),
    );
    gtk::Button::builder()
        .name(name)
        .child(&content)
        .css_classes(["menu-item"])
        .build()
}
