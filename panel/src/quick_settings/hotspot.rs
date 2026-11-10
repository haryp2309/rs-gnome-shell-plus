//! The Hotspot toggle: shown when the Wi-Fi device can be an access point.
//! Turning it on brings up the saved hotspot profile, or creates one with
//! GNOME Settings' defaults (SSID = host name, WPA2 with a random password,
//! shared IPv4); the subtitle is the SSID.

use super::network::{
    self, ACTIVE_ACTIVATED, ACTIVE_ACTIVATING, ACTIVE_CONNECTION, DEVICE, NM, NM_PATH, TYPE_WIFI, WIRELESS,
};
use super::widgets::QuickToggle;
use super::Ctx;
use crate::system;
use gtk::prelude::*;
use gtk::{gio, glib};
use std::cell::{Cell, RefCell};
use std::cmp::Reverse;
use std::collections::HashMap;
use std::rc::Rc;

/// NM_WIFI_DEVICE_CAP_AP
const CAP_AP: u32 = 0x40;
const ICON: &str = "network-wireless-hotspot-symbolic";
/// GNOME Settings' password alphabet: no look-alikes (0/O, 1/l/I).
const PASSWORD_CHARS: &[u8] = b"abcdefghijkmnopqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789";

/// The running hotspot.
struct Active {
    path: String,
    ssid: String,
}

struct Section {
    conn: gio::DBusConnection,
    toggle: Rc<QuickToggle>,
    /// The AP-capable Wi-Fi device.
    device: RefCell<Option<String>>,
    wifi_enabled: Cell<bool>,
    /// The most recently used saved hotspot profile.
    profile: RefCell<Option<String>>,
    active: RefCell<Option<Active>>,
    /// Turned on while Wi-Fi was off: start once Wi-Fi is enabled.
    pending: Cell<bool>,
}

pub fn build(_ctx: &Rc<Ctx>) -> Option<Rc<QuickToggle>> {
    let conn = system::system_bus()?;
    let toggle = QuickToggle::new("qs-hotspot", ICON, "Hotspot");
    toggle.set_visible(false);

    let section = Rc::new(Section {
        conn: conn.clone(),
        toggle: toggle.clone(),
        device: RefCell::default(),
        wifi_enabled: Cell::new(false),
        profile: RefCell::default(),
        active: RefCell::default(),
        pending: Cell::new(false),
    });
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

    toggle.root.connect_destroy(move |_| {
        let _ = &section;
    });
    Some(toggle)
}

impl Section {
    fn refresh(&self, profiles_changed: bool) {
        let nm = system::get_all(&self.conn, NM, NM_PATH, NM);
        let device = nm.as_ref().and_then(|nm| {
            nm.paths("Devices").into_iter().find_map(|path| {
                let props = system::get_all(&self.conn, NM, &path, DEVICE)?;
                if props.get::<u32>("DeviceType") != Some(TYPE_WIFI) {
                    return None;
                }
                let wireless = system::get_all(&self.conn, NM, &path, WIRELESS)?;
                let caps = wireless.get::<u32>("WirelessCapabilities").unwrap_or(0);
                (caps & CAP_AP != 0).then_some((path, props))
            })
        });
        let Some((path, props)) = device else {
            self.toggle.set_visible(false);
            self.device.take();
            self.active.take();
            return;
        };
        self.wifi_enabled
            .set(nm.as_ref().is_some_and(|nm| nm.get("WirelessEnabled").unwrap_or(false)));

        if profiles_changed {
            let profile = network::profiles(&self.conn)
                .into_iter()
                .filter(|p| p.kind == "802-11-wireless" && network::hotspot_ssid(&p.settings).is_some())
                .min_by_key(|p| Reverse(p.timestamp))
                .map(|p| p.path);
            self.profile.replace(profile);
        }

        // The device's active connection, if it's an access point.
        let active = props.path("ActiveConnection").and_then(|active_path| {
            let active = system::get_all(&self.conn, NM, &active_path, ACTIVE_CONNECTION)?;
            if !matches!(active.get::<u32>("State"), Some(ACTIVE_ACTIVATING | ACTIVE_ACTIVATED)) {
                return None;
            }
            let settings = network::connection_settings(&self.conn, &active.path("Connection")?)?;
            Some(Active {
                path: active_path,
                ssid: network::hotspot_ssid(&settings)?,
            })
        });

        self.toggle.set_visible(true);
        self.toggle.set_checked(active.is_some());
        self.toggle.set_subtitle(active.as_ref().map(|a| a.ssid.as_str()));
        self.device.replace(Some(path));
        self.active.replace(active);

        if self.wifi_enabled.get() && self.pending.replace(false) {
            self.start();
        }
    }

    fn toggle(&self) {
        if let Some(active) = self.active.borrow().as_ref() {
            network::deactivate(&self.conn, &active.path);
        } else if !self.wifi_enabled.get() {
            self.pending.set(true);
            system::set_prop(&self.conn, NM, NM_PATH, NM, "WirelessEnabled", true.to_variant());
        } else {
            self.start();
        }
    }

    fn start(&self) {
        let Some(device) = self.device.borrow().clone() else { return };
        if let Some(profile) = self.profile.borrow().as_deref() {
            network::activate(&self.conn, profile, &device, "/");
            return;
        }
        let args = glib::Variant::tuple_from_iter([
            new_hotspot(),
            system::object_path(&device),
            system::object_path("/"),
        ]);
        system::call(&self.conn, NM, NM_PATH, NM, "AddAndActivateConnection", Some(args));
    }
}

/// A new hotspot profile with GNOME Settings' defaults.
fn new_hotspot() -> glib::Variant {
    let mut ssid = glib::host_name().to_string().into_bytes();
    ssid.truncate(32);
    let password: String = (0..8)
        .map(|_| PASSWORD_CHARS[glib::random_int_range(0, PASSWORD_CHARS.len() as i32) as usize] as char)
        .collect();
    let strings = |values: &[&str]| values.iter().map(|s| s.to_string()).collect::<Vec<_>>().to_variant();

    let mut settings: HashMap<String, HashMap<String, glib::Variant>> = HashMap::new();
    settings.insert(
        "connection".into(),
        HashMap::from([
            ("id".into(), "Hotspot".to_variant()),
            ("type".into(), "802-11-wireless".to_variant()),
            ("uuid".into(), glib::uuid_string_random().to_string().to_variant()),
            ("autoconnect".into(), false.to_variant()),
        ]),
    );
    settings.insert(
        "802-11-wireless".into(),
        HashMap::from([
            ("ssid".into(), ssid.to_variant()),
            ("mode".into(), "ap".to_variant()),
            ("security".into(), "802-11-wireless-security".to_variant()),
        ]),
    );
    settings.insert(
        "802-11-wireless-security".into(),
        HashMap::from([
            ("key-mgmt".into(), "wpa-psk".to_variant()),
            ("psk".into(), password.to_variant()),
            ("proto".into(), strings(&["rsn"])),
            ("pairwise".into(), strings(&["ccmp"])),
            ("group".into(), strings(&["ccmp"])),
        ]),
    );
    settings.insert(
        "ipv4".into(),
        HashMap::from([("method".into(), "shared".to_variant())]),
    );
    settings.insert(
        "ipv6".into(),
        HashMap::from([("method".into(), "ignore".to_variant())]),
    );
    settings.to_variant()
}
