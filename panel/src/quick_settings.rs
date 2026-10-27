//! System status icons and the quick settings menu.
//!
//! Each section talks to the same services GNOME Shell does (NetworkManager,
//! UPower, power-profiles-daemon, logind, GSettings) and hides itself when
//! its service isn't available.

use crate::ipc::ShellClient;
use crate::system;
use adw::prelude::*;
use gtk::{glib, pango};
use std::cell::Cell;
use std::rc::Rc;

pub fn build(client: Rc<ShellClient>) -> gtk::MenuButton {
    let icons = gtk::Box::builder()
        .spacing(8)
        .css_classes(["status-icons"])
        .build();
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .width_request(360)
        .css_classes(["quick-settings"])
        .build();
    let popover = gtk::Popover::builder()
        .child(&content)
        .has_arrow(false)
        .css_classes(["panel-menu"])
        .build();
    let button = gtk::MenuButton::builder()
        .child(&icons)
        .popover(&popover)
        .css_classes(["panel-button"])
        .build();

    // Top row: battery level, then Settings and Power buttons.
    let top = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let battery_label = gtk::Label::builder()
        .xalign(0.0)
        .hexpand(true)
        .css_classes(["battery-level"])
        .build();
    let settings_button = gtk::Button::builder()
        .icon_name("emblem-system-symbolic")
        .tooltip_text("Settings")
        .css_classes(["circular", "quick-action"])
        .build();
    let power_button = gtk::ToggleButton::builder()
        .icon_name("system-shutdown-symbolic")
        .tooltip_text("Power Off / Log Out")
        .css_classes(["circular", "quick-action"])
        .build();
    top.append(&battery_label);
    top.append(&settings_button);
    top.append(&power_button);
    content.append(&top);

    let revealer = gtk::Revealer::builder()
        .child(&power_menu(&popover, client))
        .build();
    power_button
        .bind_property("active", &revealer, "reveal-child")
        .sync_create()
        .build();
    content.append(&revealer);

    popover.connect_closed(glib::clone!(
        #[weak]
        power_button,
        move |_| power_button.set_active(false)
    ));
    settings_button.connect_clicked(glib::clone!(
        #[weak]
        popover,
        move |_| {
            popover.popdown();
            system::spawn(&["gnome-control-center"]);
        }
    ));

    // Panel icons are appended in GNOME's order: network, volume, battery.
    let wifi = setup_network(&icons);
    setup_volume(&icons, &content, &popover);
    setup_battery(&icons, &battery_label);

    let toggles: Vec<gtk::ToggleButton> = [wifi, power_saver_toggle(), night_light_toggle(), dark_style_toggle()]
        .into_iter()
        .flatten()
        .collect();
    let grid = gtk::Grid::builder()
        .column_spacing(12)
        .row_spacing(12)
        .column_homogeneous(true)
        .build();
    for (i, toggle) in toggles.iter().enumerate() {
        grid.attach(toggle, (i % 2) as i32, (i / 2) as i32, 1, 1);
    }
    content.append(&grid);

    button
}

fn quick_toggle(icon: &str, title: &str) -> gtk::ToggleButton {
    let child = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    child.append(&gtk::Image::from_icon_name(icon));
    child.append(
        &gtk::Label::builder()
            .label(title)
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(pango::EllipsizeMode::End)
            .build(),
    );
    gtk::ToggleButton::builder()
        .child(&child)
        .css_classes(["quick-toggle"])
        .build()
}

/// Wires a toggle to an external boolean state. `read` returns the current
/// state; `write` is only called when the user's click disagrees with it, so
/// syncing the toggle from the outside never writes back.
fn sync_toggle(
    toggle: &gtk::ToggleButton,
    read: impl Fn() -> bool + 'static,
    write: impl Fn(bool) + 'static,
) -> impl Fn() + Clone + 'static {
    let read = Rc::new(read);
    let sync = glib::clone!(
        #[weak]
        toggle,
        #[strong]
        read,
        move || toggle.set_active(read())
    );
    toggle.connect_toggled(move |toggle| {
        if toggle.is_active() != read() {
            write(toggle.is_active());
        }
    });
    sync();
    sync
}

fn setup_network(icons: &gtk::Box) -> Option<gtk::ToggleButton> {
    let proxy = system::system_proxy(
        "org.freedesktop.NetworkManager",
        "/org/freedesktop/NetworkManager",
        "org.freedesktop.NetworkManager",
    )?;
    let icon = gtk::Image::new();
    icons.append(&icon);

    let toggle = quick_toggle("network-wireless-symbolic", "Wi-Fi");
    let sync_wifi = sync_toggle(
        &toggle,
        glib::clone!(
            #[strong]
            proxy,
            move || system::prop::<bool>(&proxy, "WirelessEnabled").unwrap_or(false)
        ),
        glib::clone!(
            #[strong]
            proxy,
            move |enabled| system::set_prop(&proxy, "WirelessEnabled", enabled.to_variant())
        ),
    );

    let update = glib::clone!(
        #[strong]
        proxy,
        move || {
            // NMState: 70 = connected (global), 40 = connecting.
            let state = system::prop::<u32>(&proxy, "State").unwrap_or(0);
            let kind = system::prop::<String>(&proxy, "PrimaryConnectionType").unwrap_or_default();
            let wifi_enabled = system::prop::<bool>(&proxy, "WirelessEnabled").unwrap_or(false);
            let name = match (state, kind.as_str()) {
                (60.., "802-11-wireless") => "network-wireless-signal-excellent-symbolic",
                (60.., "vpn" | "wireguard") => "network-vpn-symbolic",
                (60.., _) => "network-wired-symbolic",
                (40.., _) => "network-wired-acquiring-symbolic",
                _ if wifi_enabled => "network-wireless-offline-symbolic",
                _ => "network-offline-symbolic",
            };
            icon.set_icon_name(Some(name));
            sync_wifi();
        }
    );
    update();
    system::on_properties_changed(&proxy, update);

    Some(toggle)
}

fn volume_icon_name(volume: f64, muted: bool) -> &'static str {
    match volume {
        _ if muted || volume <= 0.0 => "audio-volume-muted-symbolic",
        v if v < 0.33 => "audio-volume-low-symbolic",
        v if v < 0.66 => "audio-volume-medium-symbolic",
        _ => "audio-volume-high-symbolic",
    }
}

/// Volume via PipeWire's `wpctl`. Polled, since there's no cheap change
/// notification without linking libpulse/libpipewire (planned).
fn setup_volume(icons: &gtk::Box, content: &gtk::Box, popover: &gtk::Popover) {
    const SINK: &str = "@DEFAULT_AUDIO_SINK@";

    if glib::find_program_in_path("wpctl").is_none() {
        return;
    }

    let icon = gtk::Image::from_icon_name("audio-volume-high-symbolic");
    icons.append(&icon);

    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let mute_button = gtk::Button::builder()
        .icon_name("audio-volume-high-symbolic")
        .tooltip_text("Mute")
        .css_classes(["flat", "circular"])
        .build();
    let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 1.0, 0.01);
    scale.set_hexpand(true);
    row.append(&mute_button);
    row.append(&scale);
    content.append(&row);

    let set_icons = glib::clone!(
        #[weak]
        icon,
        #[weak]
        mute_button,
        move |volume: f64, muted: bool| {
            let name = volume_icon_name(volume, muted);
            icon.set_icon_name(Some(name));
            mute_button.set_icon_name(name);
        }
    );

    let updating = Rc::new(Cell::new(false));
    let refresh = Rc::new(glib::clone!(
        #[weak]
        scale,
        #[strong]
        updating,
        #[strong]
        set_icons,
        move || {
            system::run_output(&["wpctl", "get-volume", SINK], glib::clone!(
                #[strong]
                updating,
                #[strong]
                set_icons,
                move |out| {
                    // "Volume: 0.40" or "Volume: 0.40 [MUTED]"
                    let Some(volume) = out.split_whitespace().nth(1).and_then(|v| v.parse::<f64>().ok()) else {
                        return;
                    };
                    let muted = out.contains("MUTED");
                    updating.set(true);
                    scale.set_value(volume);
                    updating.set(false);
                    set_icons(volume, muted);
                }
            ));
        }
    ));

    // Coalesce slider drags into at most one wpctl call per 60ms.
    let pending = Rc::new(Cell::new(false));
    scale.connect_value_changed(move |scale| {
        if updating.get() {
            return;
        }
        set_icons(scale.value(), false);
        if pending.replace(true) {
            return;
        }
        glib::timeout_add_local_once(
            std::time::Duration::from_millis(60),
            glib::clone!(
                #[weak]
                scale,
                #[strong]
                pending,
                move || {
                    pending.set(false);
                    let value = format!("{:.2}", scale.value());
                    system::spawn(&["wpctl", "set-mute", SINK, "0"]);
                    system::spawn(&["wpctl", "set-volume", SINK, &value]);
                }
            ),
        );
    });

    mute_button.connect_clicked(glib::clone!(
        #[strong]
        refresh,
        move |_| {
            system::spawn(&["wpctl", "set-mute", SINK, "toggle"]);
            let refresh = refresh.clone();
            glib::timeout_add_local_once(std::time::Duration::from_millis(100), move || refresh());
        }
    ));

    refresh();
    popover.connect_show(glib::clone!(
        #[strong]
        refresh,
        move |_| refresh()
    ));
    glib::timeout_add_seconds_local(5, move || {
        refresh();
        glib::ControlFlow::Continue
    });
}

fn setup_battery(icons: &gtk::Box, menu_label: &gtk::Label) {
    let Some(proxy) = system::system_proxy(
        "org.freedesktop.UPower",
        "/org/freedesktop/UPower/devices/DisplayDevice",
        "org.freedesktop.UPower.Device",
    ) else {
        return;
    };
    let interface = system::settings("org.gnome.desktop.interface");

    let icon = gtk::Image::new();
    let percentage = gtk::Label::new(None);
    icons.append(&icon);
    icons.append(&percentage);

    let update = Rc::new(glib::clone!(
        #[strong]
        proxy,
        #[strong]
        interface,
        #[weak]
        menu_label,
        move || {
            let present = system::prop::<bool>(&proxy, "IsPresent").unwrap_or(false);
            let level = system::prop::<f64>(&proxy, "Percentage").unwrap_or(0.0);
            let icon_name = system::prop::<String>(&proxy, "IconName").unwrap_or_default();
            let show_percentage = interface
                .as_ref()
                .is_some_and(|s| s.boolean("show-battery-percentage"));
            let text = format!("{level:.0}\u{202F}%");

            icon.set_visible(present);
            icon.set_icon_name(Some(&icon_name));
            percentage.set_visible(present && show_percentage);
            percentage.set_label(&text);
            menu_label.set_visible(present);
            menu_label.set_label(&text);
        }
    ));
    update();
    system::on_properties_changed(&proxy, glib::clone!(
        #[strong]
        update,
        move || update()
    ));
    if let Some(interface) = &interface {
        interface.connect_changed(Some("show-battery-percentage"), move |_, _| update());
    }
}

fn power_saver_toggle() -> Option<gtk::ToggleButton> {
    let proxy = system::system_proxy(
        "org.freedesktop.UPower.PowerProfiles",
        "/org/freedesktop/UPower/PowerProfiles",
        "org.freedesktop.UPower.PowerProfiles",
    )
    .or_else(|| {
        system::system_proxy("net.hadess.PowerProfiles", "/net/hadess/PowerProfiles", "net.hadess.PowerProfiles")
    })?;

    let toggle = quick_toggle("power-profile-power-saver-symbolic", "Power Saver");
    let sync = sync_toggle(
        &toggle,
        glib::clone!(
            #[strong]
            proxy,
            move || system::prop::<String>(&proxy, "ActiveProfile").as_deref() == Some("power-saver")
        ),
        glib::clone!(
            #[strong]
            proxy,
            move |on| {
                let profile = if on { "power-saver" } else { "balanced" };
                system::set_prop(&proxy, "ActiveProfile", profile.to_variant());
            }
        ),
    );
    system::on_properties_changed(&proxy, sync);
    Some(toggle)
}

fn night_light_toggle() -> Option<gtk::ToggleButton> {
    let settings = system::settings("org.gnome.settings-daemon.plugins.color")?;
    let toggle = quick_toggle("night-light-symbolic", "Night Light");
    settings.bind("night-light-enabled", &toggle, "active").build();
    toggle.connect_destroy(move |_| {
        let _ = &settings;
    });
    Some(toggle)
}

fn dark_style_toggle() -> Option<gtk::ToggleButton> {
    let settings = system::settings("org.gnome.desktop.interface")?;
    let toggle = quick_toggle("dark-mode-symbolic", "Dark Style");
    let sync = sync_toggle(
        &toggle,
        glib::clone!(
            #[strong]
            settings,
            move || settings.string("color-scheme") == "prefer-dark"
        ),
        glib::clone!(
            #[strong]
            settings,
            move |dark| {
                let _ = settings.set_string("color-scheme", if dark { "prefer-dark" } else { "default" });
            }
        ),
    );
    settings.connect_changed(Some("color-scheme"), move |_, _| sync());
    Some(toggle)
}

fn power_menu(popover: &gtk::Popover, client: Rc<ShellClient>) -> gtk::Box {
    let menu = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .css_classes(["power-menu"])
        .build();

    let item = |label: &str, action: Box<dyn Fn()>| {
        let button = gtk::Button::builder()
            .child(&gtk::Label::builder().label(label).xalign(0.0).build())
            .css_classes(["flat"])
            .build();
        button.connect_clicked(glib::clone!(
            #[weak]
            popover,
            move |_| {
                popover.popdown();
                action();
            }
        ));
        menu.append(&button);
    };

    item("Suspend", Box::new(|| logind("Suspend")));
    item(
        "Restart…",
        Box::new(|| confirm("Restart?", "The system will restart.", "Restart", || logind("Reboot"))),
    );
    item(
        "Power Off…",
        Box::new(|| confirm("Power Off?", "The system will power off.", "Power Off", || logind("PowerOff"))),
    );
    item("Log Out", Box::new(move || log_out(&client)));

    menu
}

fn logind(method: &str) {
    if let Some(proxy) = system::system_proxy(
        "org.freedesktop.login1",
        "/org/freedesktop/login1",
        "org.freedesktop.login1.Manager",
    ) {
        // interactive = true lets polkit prompt if needed.
        system::call(&proxy, method, Some((true,).to_variant()));
    }
}

fn log_out(client: &ShellClient) {
    match system::session_proxy("org.gnome.SessionManager", "/org/gnome/SessionManager", "org.gnome.SessionManager") {
        // Mode 1 = no confirmation dialog.
        Some(proxy) => system::call(&proxy, "Logout", Some((1u32,).to_variant())),
        // Not running under gnome-session: ending the compositor ends the session.
        None => client.log_out(),
    }
}

fn confirm(heading: &str, body: &str, action_label: &str, action: impl Fn() + 'static) {
    let dialog = adw::AlertDialog::new(Some(heading), Some(body));
    dialog.add_responses(&[("cancel", "Cancel"), ("confirm", action_label)]);
    dialog.set_response_appearance("confirm", adw::ResponseAppearance::Destructive);
    dialog.set_close_response("cancel");
    dialog.connect_response(Some("confirm"), move |_, _| action());
    // The panel window is too short to host the dialog; let it make its own.
    dialog.present(None::<&gtk::Widget>);
}
