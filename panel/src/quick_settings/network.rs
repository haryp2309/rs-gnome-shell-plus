//! Network: the Wi-Fi and Wired toggles and the panel's network icon, all
//! read from NetworkManager over D-Bus (like GNOME Shell's status/network.js,
//! minus modem menus). VPN and hotspot live in vpn.rs and hotspot.rs and
//! use the profile/active-connection helpers at the bottom of this file.

use super::widgets::{signal_icon, MenuItem, QuickToggle};
use super::Ctx;
use crate::system::{self, Props};
use gtk::prelude::*;
use gtk::{gio, glib};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

pub(super) const NM: &str = "org.freedesktop.NetworkManager";
pub(super) const NM_PATH: &str = "/org/freedesktop/NetworkManager";
const SETTINGS_PATH: &str = "/org/freedesktop/NetworkManager/Settings";
const SETTINGS: &str = "org.freedesktop.NetworkManager.Settings";
const SETTINGS_CONNECTION: &str = "org.freedesktop.NetworkManager.Settings.Connection";
pub(super) const DEVICE: &str = "org.freedesktop.NetworkManager.Device";
pub(super) const WIRELESS: &str = "org.freedesktop.NetworkManager.Device.Wireless";
const ACCESS_POINT: &str = "org.freedesktop.NetworkManager.AccessPoint";
pub(super) const ACTIVE_CONNECTION: &str = "org.freedesktop.NetworkManager.Connection.Active";

// NMDeviceType
const TYPE_ETHERNET: u32 = 1;
pub(super) const TYPE_WIFI: u32 = 2;
// NMDeviceState
const STATE_UNAVAILABLE: u32 = 20;
const STATE_PREPARE: u32 = 40;
const STATE_ACTIVATED: u32 = 100;
// NMActiveConnectionState
pub(super) const ACTIVE_ACTIVATING: u32 = 1;
pub(super) const ACTIVE_ACTIVATED: u32 = 2;
// NMState / NMConnectivityState
const NM_STATE_CONNECTING: u32 = 40;
const CONNECTIVITY_PORTAL: u32 = 2;
const CONNECTIVITY_LIMITED: u32 = 3;

const MAX_NETWORKS: usize = 8;
// NM80211ApSecurityFlags
const AP_SEC_KEY_MGMT_802_1X: u32 = 0x200;

/// One visible network: all access points sharing an SSID.
#[derive(Clone, PartialEq)]
struct Network {
    ssid: String,
    /// The strongest access point, used for activation.
    ap: String,
    strength: u8,
    secure: bool,
    /// WPA/WPA2 Enterprise (802.1X), which needs more than a password.
    enterprise: bool,
    active: bool,
}

struct Device {
    path: String,
    state: u32,
    /// `Id` of the active connection.
    connection: Option<String>,
    /// `SpecificObject` of the active connection (the AP for Wi-Fi).
    specific_object: Option<String>,
    /// The saved profile (`Connection`) of the active connection.
    profile: Option<String>,
}

struct Wifi {
    device: Device,
    enabled: bool,
    networks: Vec<Network>,
    /// SSID of the hotspot this device is running (active connection in
    /// `802-11-wireless.mode` "ap"), if any.
    hotspot: Option<String>,
}

impl Wifi {
    fn active(&self) -> Option<&Network> {
        self.networks.first().filter(|n| n.active)
    }

    fn icon(&self) -> String {
        if !self.enabled {
            "network-wireless-disabled-symbolic".into()
        } else if self.hotspot.is_some() && self.device.state >= STATE_PREPARE {
            "network-wireless-hotspot-symbolic".into()
        } else if self.device.state < STATE_PREPARE {
            "network-wireless-offline-symbolic".into()
        } else if self.device.state < STATE_ACTIVATED {
            "network-wireless-acquiring-symbolic".into()
        } else {
            signal_icon("network-wireless", self.active().map_or(0, |n| n.strength))
        }
    }
}

struct State {
    primary_type: String,
    nm_state: u32,
    connectivity: u32,
    wifi: Option<Wifi>,
    wired: Option<Device>,
}

impl State {
    fn read(conn: &gio::DBusConnection) -> Option<Self> {
        let nm = system::get_all(conn, NM, NM_PATH, NM)?;
        let mut state = Self {
            primary_type: nm.string("PrimaryConnectionType"),
            nm_state: nm.get("State").unwrap_or(0),
            connectivity: nm.get("Connectivity").unwrap_or(0),
            wifi: None,
            wired: None,
        };
        // Like GNOME's single-device case, the first device of each kind wins.
        for path in nm.paths("Devices") {
            let Some(props) = system::get_all(conn, NM, &path, DEVICE) else { continue };
            match props.get::<u32>("DeviceType") {
                Some(TYPE_WIFI) if state.wifi.is_none() => {
                    let device = read_device(conn, path, &props);
                    let enabled = nm.get("WirelessEnabled").unwrap_or(false);
                    let hotspot = device
                        .profile
                        .as_deref()
                        .filter(|_| device.state >= STATE_PREPARE)
                        .and_then(|p| connection_settings(conn, p))
                        .and_then(|settings| hotspot_ssid(&settings));
                    let networks = if enabled {
                        read_networks(conn, &device, hotspot.as_deref())
                    } else {
                        Vec::new()
                    };
                    state.wifi = Some(Wifi {
                        device,
                        enabled,
                        networks,
                        hotspot,
                    });
                }
                Some(TYPE_ETHERNET) if state.wired.is_none() => {
                    state.wired = Some(read_device(conn, path, &props));
                }
                _ => {}
            }
        }
        Some(state)
    }

    /// The panel icon, following the primary connection like GNOME Shell.
    fn indicator_icon(&self) -> String {
        let limited = matches!(self.connectivity, CONNECTIVITY_PORTAL | CONNECTIVITY_LIMITED);
        let wifi = self.wifi.as_ref().filter(|w| w.enabled);
        let wifi_activated = wifi.filter(|w| w.device.state == STATE_ACTIVATED);
        let wired_activated = self.wired.as_ref().filter(|d| d.state == STATE_ACTIVATED);
        let activating = |d: &Device| (STATE_PREPARE..STATE_ACTIVATED).contains(&d.state);

        match (self.primary_type.as_str(), wifi_activated, wired_activated) {
            // VPNs have their own indicator (vpn.rs); like GNOME, show the
            // device they run over here.
            ("vpn" | "wireguard", _, Some(_)) => return "network-wired-symbolic".into(),
            ("vpn" | "wireguard", Some(w), None) => return w.icon(),
            ("802-11-wireless", Some(_), _) if limited => return "network-wireless-no-route-symbolic".into(),
            ("802-11-wireless", Some(w), _) => return w.icon(),
            ("802-3-ethernet", _, Some(_)) if limited => return "network-wired-no-route-symbolic".into(),
            ("802-3-ethernet", _, Some(_)) => return "network-wired-symbolic".into(),
            _ => {}
        }
        if let Some(w) = wifi_activated.filter(|w| w.hotspot.is_some()) {
            return w.icon();
        }
        if wifi.is_some_and(|w| activating(&w.device)) {
            "network-wireless-acquiring-symbolic".into()
        } else if self.wired.as_ref().is_some_and(activating) || self.nm_state == NM_STATE_CONNECTING {
            "network-wired-acquiring-symbolic".into()
        } else if self.wifi.is_none() && self.wired.is_some() {
            "network-wired-disconnected-symbolic".into()
        } else {
            "network-offline-symbolic".into()
        }
    }
}

fn read_device(conn: &gio::DBusConnection, path: String, props: &Props) -> Device {
    let active = props
        .path("ActiveConnection")
        .and_then(|p| system::get_all(conn, NM, &p, ACTIVE_CONNECTION));
    Device {
        path,
        state: props.get("State").unwrap_or(0),
        connection: active.as_ref().map(|a| a.string("Id")).filter(|id| !id.is_empty()),
        specific_object: active.as_ref().and_then(|a| a.path("SpecificObject")),
        profile: active.as_ref().and_then(|a| a.path("Connection")),
    }
}

/// Visible networks, deduplicated by SSID (hidden ones skipped), the
/// connected one first, then by strength. While the device is a hotspot,
/// nothing is connected and the hotspot's own SSID isn't listed.
fn read_networks(conn: &gio::DBusConnection, device: &Device, hotspot: Option<&str>) -> Vec<Network> {
    let Some(wireless) = system::get_all(conn, NM, &device.path, WIRELESS) else {
        return Vec::new();
    };
    let active_ap = wireless
        .path("ActiveAccessPoint")
        .or_else(|| device.specific_object.clone())
        .filter(|_| device.state >= STATE_PREPARE && hotspot.is_none());

    let mut networks: Vec<Network> = Vec::new();
    for path in wireless.paths("AccessPoints") {
        let Some(ap) = system::get_all(conn, NM, &path, ACCESS_POINT) else { continue };
        let ssid = ap.get::<Vec<u8>>("Ssid").unwrap_or_default();
        let ssid = String::from_utf8_lossy(&ssid).trim_end_matches('\0').to_string();
        if ssid.is_empty() || hotspot == Some(ssid.as_str()) {
            continue;
        }
        let flags = |key| ap.get::<u32>(key).unwrap_or(0);
        let network = Network {
            strength: ap.get("Strength").unwrap_or(0),
            secure: flags("Flags") & 0x1 != 0 || flags("WpaFlags") != 0 || flags("RsnFlags") != 0,
            enterprise: (flags("WpaFlags") | flags("RsnFlags")) & AP_SEC_KEY_MGMT_802_1X != 0,
            active: active_ap.as_deref() == Some(path.as_str()),
            ap: path,
            ssid,
        };
        match networks.iter_mut().find(|n| n.ssid == network.ssid) {
            Some(known) => {
                known.active |= network.active;
                known.secure |= network.secure;
                known.enterprise |= network.enterprise;
                if network.strength > known.strength {
                    known.strength = network.strength;
                    known.ap = network.ap;
                }
            }
            None => networks.push(network),
        }
    }
    networks.sort_by(|a, b| b.active.cmp(&a.active).then(b.strength.cmp(&a.strength)));
    networks.truncate(MAX_NETWORKS);
    networks
}

struct Section {
    ctx: Rc<Ctx>,
    conn: gio::DBusConnection,
    wifi: Rc<QuickToggle>,
    wired: Rc<QuickToggle>,
    state: RefCell<Option<State>>,
    /// What the Wi-Fi menu currently lists, to rebuild it only on change.
    listed: RefCell<Option<Vec<Network>>>,
}

pub fn build(ctx: &Rc<Ctx>) -> Vec<Rc<QuickToggle>> {
    let Some(conn) = system::system_bus() else { return Vec::new() };
    let wifi = QuickToggle::with_menu("qs-wifi", "network-wireless-symbolic", "Wi-Fi", &ctx.host);
    let wired = QuickToggle::new("qs-wired", "network-wired-symbolic", "Wired");
    wifi.set_visible(false);
    wired.set_visible(false);

    let section = Rc::new(Section {
        ctx: ctx.clone(),
        conn: conn.clone(),
        wifi: wifi.clone(),
        wired: wired.clone(),
        state: RefCell::default(),
        listed: RefCell::default(),
    });

    if let Some(menu) = &wifi.menu {
        menu.add_footer("qs-wifi-settings", "All Networks", || system::open_settings("wifi"));
    }
    wifi.connect_clicked(glib::clone!(
        #[weak]
        section,
        move || section.toggle_wifi()
    ));
    wifi.connect_menu_opened(glib::clone!(
        #[weak]
        section,
        move || section.request_scan()
    ));
    wired.connect_clicked(glib::clone!(
        #[weak]
        section,
        move || section.toggle_wired()
    ));
    system::watch_service(
        &conn,
        NM,
        glib::clone!(
            #[weak]
            section,
            move || section.refresh()
        ),
    );

    // The toggles' handlers only hold weak references; keep the section
    // alive as long as the Wi-Fi toggle's widget.
    wifi.root.connect_destroy(move |_| {
        let _ = &section;
    });
    vec![wifi, wired]
}

impl Section {
    fn refresh(self: &Rc<Self>) {
        let state = State::read(&self.conn);
        let indicator = &self.ctx.indicators.network;
        indicator.set_visible(state.is_some());

        match state.as_ref().and_then(|s| s.wifi.as_ref()) {
            Some(wifi) => {
                self.wifi.set_visible(true);
                self.wifi.set_checked(wifi.enabled);
                self.wifi.set_icon(&wifi.icon());
                let subtitle = match &wifi.hotspot {
                    Some(_) => Some("Hotspot Active"),
                    None => wifi.active().map(|n| n.ssid.as_str()),
                };
                self.wifi.set_subtitle(subtitle);
                self.update_wifi_menu(wifi);
            }
            None => self.wifi.set_visible(false),
        }

        match state.as_ref().and_then(|s| s.wired.as_ref()) {
            Some(wired) if wired.state > STATE_UNAVAILABLE => {
                let activating = (STATE_PREPARE..STATE_ACTIVATED).contains(&wired.state);
                self.wired.set_visible(true);
                self.wired.set_checked(wired.state == STATE_ACTIVATED);
                self.wired.set_icon(match wired.state {
                    STATE_ACTIVATED => "network-wired-symbolic",
                    _ if activating => "network-wired-acquiring-symbolic",
                    _ => "network-wired-disconnected-symbolic",
                });
                let connected = wired.state == STATE_ACTIVATED || activating;
                self.wired.set_subtitle(wired.connection.as_deref().filter(|_| connected));
            }
            _ => self.wired.set_visible(false),
        }

        if let Some(state) = &state {
            indicator.set_icon_name(Some(&state.indicator_icon()));
        }
        self.state.replace(state);
    }

    fn update_wifi_menu(self: &Rc<Self>, wifi: &Wifi) {
        let Some(menu) = &self.wifi.menu else { return };
        let networks = Some(wifi.networks.clone()).filter(|_| wifi.enabled);
        if *self.listed.borrow() == networks {
            return;
        }
        menu.clear();
        match &networks {
            // GNOME shows only the header and footer while Wi-Fi is off.
            None => {}
            Some(list) if list.is_empty() => menu.add_placeholder("No Networks"),
            Some(list) => {
                for (i, network) in list.iter().enumerate() {
                    let item = MenuItem::build(
                        &format!("qs-wifi-net-{i}"),
                        Some(&signal_icon("network-wireless", network.strength)),
                        &network.ssid,
                        network.secure.then_some("network-wireless-encrypted-symbolic"),
                        network.active,
                    );
                    let network = network.clone();
                    let device = wifi.device.path.clone();
                    item.connect_clicked(glib::clone!(
                        #[weak(rename_to = section)]
                        self,
                        move |_| section.connect_to(&device, &network)
                    ));
                    menu.add(&item);
                }
            }
        }
        self.listed.replace(networks);
    }

    fn wifi_device(&self) -> Option<String> {
        let state = self.state.borrow();
        let wifi = state.as_ref()?.wifi.as_ref()?;
        Some(wifi.device.path.clone()).filter(|_| wifi.enabled)
    }

    fn toggle_wifi(&self) {
        let enabled = !self.wifi.is_checked();
        system::set_prop(&self.conn, NM, NM_PATH, NM, "WirelessEnabled", enabled.to_variant());
    }

    fn request_scan(&self) {
        if let Some(device) = self.wifi_device() {
            let options = HashMap::<String, glib::Variant>::new().to_variant();
            system::call(&self.conn, NM, &device, WIRELESS, "RequestScan", Some((options,).to_variant()));
        }
    }

    fn toggle_wired(&self) {
        let state = self.state.borrow();
        let Some(wired) = state.as_ref().and_then(|s| s.wired.as_ref()) else { return };
        if self.wired.is_checked() {
            system::call(&self.conn, NM, &wired.path, DEVICE, "Disconnect", None);
        } else {
            // "/" lets NetworkManager pick the best connection for the device.
            self.activate("/", &wired.path, "/");
        }
    }

    fn connect_to(&self, device: &str, network: &Network) {
        if network.active {
            return;
        }
        if let Some(saved) = self.saved_connection(&network.ssid) {
            self.activate(&saved, device, &network.ap);
        } else if !network.enterprise {
            // Like GNOME Shell: let NetworkManager fill in the settings from
            // the access point; for a secured one it then asks our secret
            // agent (network_agent.rs) for the password.
            let settings = HashMap::<String, HashMap<String, glib::Variant>>::new().to_variant();
            let args = glib::Variant::tuple_from_iter([
                settings,
                system::object_path(device),
                system::object_path(&network.ap),
            ]);
            system::call(&self.conn, NM, NM_PATH, NM, "AddAndActivateConnection", Some(args));
        } else {
            // Enterprise networks need certificates and EAP settings.
            system::open_settings("wifi");
        }
    }

    fn activate(&self, connection: &str, device: &str, specific_object: &str) {
        activate(&self.conn, connection, device, specific_object);
    }

    /// A saved (client, not hotspot) connection whose
    /// `802-11-wireless.ssid` is `ssid`.
    fn saved_connection(&self, ssid: &str) -> Option<String> {
        profiles(&self.conn)
            .into_iter()
            .find(|profile| {
                profile.setting("802-11-wireless").is_some_and(|wireless| {
                    wireless.get::<Vec<u8>>("ssid").as_deref() == Some(ssid.as_bytes())
                        && wireless.string("mode") != "ap"
                })
            })
            .map(|profile| profile.path)
    }
}

// --- helpers for the other NetworkManager sections (vpn.rs, hotspot.rs) ---

/// A saved connection profile (Settings.Connection).
pub(super) struct Profile {
    pub path: String,
    /// `connection.id`
    pub id: String,
    /// `connection.type`, e.g. "vpn", "wireguard", "802-11-wireless".
    pub kind: String,
    /// `connection.timestamp`: when it was last activated (0 = never).
    pub timestamp: u64,
    /// The whole `a{sa{sv}}` settings.
    pub settings: glib::Variant,
}

impl Profile {
    pub fn setting(&self, name: &str) -> Option<Props> {
        setting(&self.settings, name)
    }
}

/// One setting (`a{sv}`) of an `a{sa{sv}}` connection.
pub(super) fn setting(settings: &glib::Variant, name: &str) -> Option<Props> {
    // The outer values aren't boxed, so no system::lookup here (it would
    // hit a GLib critical on non-`v` values).
    settings
        .iter()
        .find(|entry| entry.child_value(0).str() == Some(name))
        .map(|entry| Props(entry.child_value(1)))
}

/// `Settings.Connection.GetSettings` of a profile, as `a{sa{sv}}`.
pub(super) fn connection_settings(conn: &gio::DBusConnection, path: &str) -> Option<glib::Variant> {
    system::call_sync(conn, NM, path, SETTINGS_CONNECTION, "GetSettings", None).map(|reply| reply.child_value(0))
}

/// The SSID of a Wi-Fi profile in access point (hotspot) mode.
pub(super) fn hotspot_ssid(settings: &glib::Variant) -> Option<String> {
    let wireless = setting(settings, "802-11-wireless")?;
    (wireless.string("mode") == "ap").then(|| {
        let ssid = wireless.get::<Vec<u8>>("ssid").unwrap_or_default();
        String::from_utf8_lossy(&ssid).into_owned()
    })
}

/// All saved profiles.
pub(super) fn profiles(conn: &gio::DBusConnection) -> Vec<Profile> {
    let Some(reply) = system::call_sync(conn, NM, SETTINGS_PATH, SETTINGS, "ListConnections", None) else {
        return Vec::new();
    };
    system::paths(&reply.child_value(0))
        .into_iter()
        .filter_map(|path| {
            let settings = connection_settings(conn, &path)?;
            let base = setting(&settings, "connection")?;
            Some(Profile {
                id: base.string("id"),
                kind: base.string("type"),
                timestamp: base.get("timestamp").unwrap_or(0),
                path,
                settings,
            })
        })
        .collect()
}

/// An entry of NetworkManager's `ActiveConnections`.
pub(super) struct ActiveConnection {
    pub path: String,
    /// The profile it was activated from.
    pub profile: String,
    pub id: String,
    pub kind: String,
    /// NMActiveConnectionState
    pub state: u32,
}

pub(super) fn active_connections(conn: &gio::DBusConnection) -> Vec<ActiveConnection> {
    let Some(nm) = system::get_all(conn, NM, NM_PATH, NM) else { return Vec::new() };
    nm.paths("ActiveConnections")
        .into_iter()
        .filter_map(|path| {
            let props = system::get_all(conn, NM, &path, ACTIVE_CONNECTION)?;
            Some(ActiveConnection {
                profile: props.path("Connection").unwrap_or_default(),
                id: props.string("Id"),
                kind: props.string("Type"),
                state: props.get("State").unwrap_or(0),
                path,
            })
        })
        .collect()
}

pub(super) fn activate(conn: &gio::DBusConnection, profile: &str, device: &str, specific_object: &str) {
    let args = glib::Variant::tuple_from_iter([
        system::object_path(profile),
        system::object_path(device),
        system::object_path(specific_object),
    ]);
    system::call(conn, NM, NM_PATH, NM, "ActivateConnection", Some(args));
}

pub(super) fn deactivate(conn: &gio::DBusConnection, active: &str) {
    let args = glib::Variant::tuple_from_iter([system::object_path(active)]);
    system::call(conn, NM, NM_PATH, NM, "DeactivateConnection", Some(args));
}

/// Like [`system::watch_service`] for NetworkManager, minus access point
/// chatter (strength updates during scans). The callback's argument says
/// whether saved profiles may have changed since the last call, so sections
/// can keep them cached instead of re-reading every profile each time.
pub(super) fn watch(conn: &gio::DBusConnection, callback: impl Fn(bool) + 'static) {
    let dirty = Rc::new(Cell::new(true));
    let refresh = system::debounced(glib::clone!(
        #[strong]
        dirty,
        move || callback(dirty.replace(false))
    ));

    #[allow(deprecated)]
    conn.signal_subscribe(
        Some(NM),
        None,
        None,
        None,
        None,
        gio::DBusSignalFlags::NONE,
        glib::clone!(
            #[strong]
            dirty,
            #[strong]
            refresh,
            move |_, _, path, iface, _, _| {
                // dbusmock's MethodCalled signals: our own calls.
                if path.contains("/AccessPoint/") || iface.starts_with("org.freedesktop.DBus.Mock") {
                    return;
                }
                if path.starts_with(SETTINGS_PATH) || iface.starts_with(SETTINGS) {
                    dirty.set(true);
                }
                refresh();
            }
        ),
    );
    let on_change = move || {
        dirty.set(true);
        refresh();
    };
    let on_vanish = on_change.clone();
    gio::bus_watch_name_on_connection(
        conn,
        NM,
        gio::BusNameWatcherFlags::NONE,
        move |_, _, _| on_change(),
        move |_, _| on_vanish(),
    );
}
