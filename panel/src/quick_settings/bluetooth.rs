//! Bluetooth toggle, its device submenu and the panel indicator, driven by
//! BlueZ (`org.bluez`) on the system bus, like GNOME Shell's
//! `status/bluetooth.js`.

use super::widgets::{MenuItem, QuickToggle};
use super::Ctx;
use crate::system::{self, Props};
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

const BLUEZ: &str = "org.bluez";
const ADAPTER: &str = "org.bluez.Adapter1";
const DEVICE: &str = "org.bluez.Device1";
const ICON_ON: &str = "bluetooth-active-symbolic";
const ICON_OFF: &str = "bluetooth-disabled-symbolic";

pub fn build(ctx: &Rc<Ctx>) -> Vec<Rc<QuickToggle>> {
    let Some(conn) = system::system_bus() else {
        return Vec::new();
    };
    let toggle = QuickToggle::with_menu("qs-bluetooth", ICON_ON, "Bluetooth", &ctx.host);
    toggle.set_visible(false);

    let section = Rc::new(Section {
        conn: conn.clone(),
        ctx: ctx.clone(),
        toggle: toggle.clone(),
        state: RefCell::default(),
        pending: RefCell::default(),
        generation: Cell::new(0),
    });

    let weak = Rc::downgrade(&section);
    toggle.connect_clicked(move || {
        if let Some(section) = weak.upgrade() {
            section.toggle_power();
        }
    });
    if let Some(menu) = &toggle.menu {
        menu.add_footer("qs-bt-settings", "Bluetooth Settings", glib::clone!(
            #[strong(rename_to = popover)]
            ctx.popover,
            move || {
                popover.popdown();
                system::open_settings("bluetooth");
            }
        ));
    }

    // The service watch owns the section for the lifetime of the panel.
    system::watch_service(&conn, BLUEZ, move || section.refresh());
    vec![toggle]
}

#[derive(Clone)]
struct Device {
    path: String,
    alias: String,
    icon: String,
    connected: bool,
}

#[derive(Default)]
struct State {
    /// Object path of the first adapter, if any.
    adapter: Option<String>,
    powered: bool,
    /// Paired devices, connected first, then by name.
    devices: Vec<Device>,
}

impl State {
    /// Parses a `GetManagedObjects` reply.
    fn from_objects(reply: &glib::Variant) -> Self {
        let mut state = Self::default();
        // a{oa{sa{sv}}}: object path -> interface -> properties.
        for object in reply.child_value(0).iter() {
            let path = object.child_value(0).str().unwrap_or_default().to_owned();
            let interfaces = object.child_value(1);
            if let Some(adapter) = interface(&interfaces, ADAPTER) {
                if state.adapter.is_none() {
                    state.powered = adapter.get("Powered").unwrap_or(false);
                    state.adapter = Some(path.clone());
                }
            }
            if let Some(device) = interface(&interfaces, DEVICE) {
                if !device.get("Paired").unwrap_or(false) {
                    continue;
                }
                let mut alias = device.string("Alias");
                if alias.is_empty() {
                    alias = device.string("Address");
                }
                state.devices.push(Device {
                    path,
                    alias,
                    icon: device.string("Icon"),
                    connected: device.get("Connected").unwrap_or(false),
                });
            }
        }
        state.devices.sort_by(|a, b| {
            b.connected
                .cmp(&a.connected)
                .then_with(|| a.alias.to_lowercase().cmp(&b.alias.to_lowercase()))
        });
        state
    }

    fn connected(&self) -> impl Iterator<Item = &Device> {
        self.devices.iter().filter(|d| d.connected)
    }
}

/// A Connect/Disconnect call in flight: `true` while connecting.
type Pending = HashMap<String, bool>;

struct Section {
    conn: gio::DBusConnection,
    ctx: Rc<Ctx>,
    toggle: Rc<QuickToggle>,
    state: RefCell<State>,
    pending: RefCell<Pending>,
    /// Bumped per refresh so out-of-order replies are dropped.
    generation: Cell<u64>,
}

impl Section {
    /// Re-reads BlueZ's objects without blocking the panel. Only the
    /// latest reply is applied; a timed-out read keeps the previous state.
    fn refresh(self: &Rc<Self>) {
        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        let weak = Rc::downgrade(self);
        self.conn.call(
            Some(BLUEZ),
            "/",
            "org.freedesktop.DBus.ObjectManager",
            "GetManagedObjects",
            None,
            None,
            gio::DBusCallFlags::NONE,
            5000,
            gio::Cancellable::NONE,
            move |result| {
                let Some(section) = weak.upgrade().filter(|s| s.generation.get() == generation) else {
                    return;
                };
                let state = match result {
                    Ok(reply) => State::from_objects(&reply),
                    Err(err) if err.matches(gio::IOErrorEnum::TimedOut) => return,
                    // No BlueZ (or it went away): no adapter, toggle hidden.
                    Err(_) => State::default(),
                };
                section.state.replace(state);
                section.sync();
            },
        );
    }

    /// Pushes the current state into the toggle, menu and indicator.
    fn sync(self: &Rc<Self>) {
        let state = self.state.borrow();
        let connected: Vec<&Device> = state.connected().collect();

        self.toggle.set_visible(state.adapter.is_some());
        self.toggle.set_checked(state.powered);
        self.toggle.set_icon(if state.powered { ICON_ON } else { ICON_OFF });
        let subtitle = match connected.as_slice() {
            _ if !state.powered => None,
            [] => None,
            [device] => Some(device.alias.clone()),
            many => Some(format!("{} Connected", many.len())),
        };
        self.toggle.set_subtitle(subtitle.as_deref());

        let indicator = &self.ctx.indicators.bluetooth;
        indicator.set_icon_name(Some(ICON_ON));
        indicator.set_visible(state.powered && !connected.is_empty());

        self.sync_menu(&state);
    }

    fn sync_menu(self: &Rc<Self>, state: &State) {
        let Some(menu) = &self.toggle.menu else {
            return;
        };
        menu.clear();
        if !state.powered {
            menu.set_subtitle(Some("Off"));
            return;
        }

        let pending = self.pending.borrow();
        let busy = state
            .devices
            .iter()
            .find_map(|d| pending.get(&d.path).map(|connecting| (d, *connecting)));
        let subtitle = busy.map(|(device, connecting)| {
            if connecting {
                format!("Connecting to {}…", device.alias)
            } else {
                format!("Disconnecting {}…", device.alias)
            }
        });
        menu.set_subtitle(subtitle.as_deref());

        if state.devices.is_empty() {
            menu.add_placeholder("No paired devices");
            return;
        }
        for (n, device) in state.devices.iter().enumerate() {
            let item = MenuItem::build(
                &format!("qs-bt-device-{n}"),
                Some(&device_icon(&device.icon)),
                &device.alias,
                None,
                device.connected,
            );
            if pending.contains_key(&device.path) {
                add_spinner(&item);
            }
            let weak = Rc::downgrade(self);
            let device = device.clone();
            item.connect_clicked(move |_| {
                if let Some(section) = weak.upgrade() {
                    section.toggle_connection(&device);
                }
            });
            menu.add(&item);
        }
    }

    fn toggle_power(&self) {
        let state = self.state.borrow();
        if let Some(adapter) = &state.adapter {
            system::set_prop(&self.conn, BLUEZ, adapter, ADAPTER, "Powered", (!state.powered).to_variant());
        }
    }

    fn toggle_connection(self: &Rc<Self>, device: &Device) {
        if self.pending.borrow().contains_key(&device.path) {
            return;
        }
        let connect = !device.connected;
        self.pending.borrow_mut().insert(device.path.clone(), connect);
        self.sync();

        let weak = Rc::downgrade(self);
        let path = device.path.clone();
        self.conn.call(
            Some(BLUEZ),
            &device.path,
            DEVICE,
            if connect { "Connect" } else { "Disconnect" },
            None,
            None,
            gio::DBusCallFlags::NONE,
            // BlueZ can take a while to reach a device; GNOME waits too.
            60_000,
            gio::Cancellable::NONE,
            move |result| {
                if let Err(err) = result {
                    glib::g_warning!("shellplus", "Bluetooth {} failed: {err}", if connect { "Connect" } else { "Disconnect" });
                }
                if let Some(section) = weak.upgrade() {
                    section.pending.borrow_mut().remove(&path);
                    section.refresh();
                }
            },
        );
    }
}

/// The properties of `iface` in an `a{sa{sv}}` interfaces dictionary.
/// (`system::lookup` would try to unbox the `a{sv}` as a `v`.)
fn interface(interfaces: &glib::Variant, iface: &str) -> Option<Props> {
    interfaces
        .iter()
        .find(|entry| entry.child_value(0).str() == Some(iface))
        .map(|entry| Props(entry.child_value(1)))
}

/// Shows a spinner in front of the item's check mark while GNOME would.
fn add_spinner(item: &gtk::Button) {
    let Some(content) = item.child().and_downcast::<gtk::Box>() else {
        return;
    };
    let check = content.last_child();
    let spinner = adw::Spinner::new();
    content.insert_child_after(&spinner, check.and_then(|c| c.prev_sibling()).as_ref());
}

/// BlueZ device icon names (e.g. "audio-headphones") as symbolic icons,
/// falling back to the generic Bluetooth icon when the theme lacks one.
fn device_icon(icon: &str) -> String {
    let name = format!("{icon}-symbolic");
    let exists = !icon.is_empty()
        && gdk::Display::default().is_some_and(|display| gtk::IconTheme::for_display(&display).has_icon(&name));
    if exists {
        name
    } else {
        ICON_ON.to_owned()
    }
}
