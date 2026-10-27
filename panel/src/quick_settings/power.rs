//! Power and system: the top row (battery, Settings, power submenu), the
//! brightness slider and the Power Mode toggle. Talks to UPower, logind,
//! gnome-session, GDM and power-profiles-daemon like GNOME Shell does.

use super::widgets::{MenuItem, QuickMenu, QuickSlider, QuickToggle};
use super::Ctx;
use crate::system;
use adw::prelude::*;
use gtk::{gio, glib};
use std::cell::Cell;
use std::rc::Rc;

const UPOWER: &str = "org.freedesktop.UPower";
const DISPLAY_DEVICE: &str = "/org/freedesktop/UPower/devices/DisplayDevice";
const UPOWER_DEVICE: &str = "org.freedesktop.UPower.Device";

const LOGIND: &str = "org.freedesktop.login1";
const LOGIND_PATH: &str = "/org/freedesktop/login1";
const LOGIND_MANAGER: &str = "org.freedesktop.login1.Manager";

const SESSION_MANAGER: &str = "org.gnome.SessionManager";
const DISPLAY_MANAGER: &str = "org.gnome.DisplayManager";

/// Seconds before a Restart / Power Off confirmation acts on its own.
const END_SESSION_TIMEOUT: u32 = 60;

// --- top row --------------------------------------------------------------

pub fn system_row(ctx: &Rc<Ctx>) -> gtk::Box {
    let row = gtk::Box::builder().spacing(8).css_classes(["system-row"]).build();
    row.append(&battery_button(ctx));
    row.append(&gtk::Box::builder().hexpand(true).build());

    // No lock screen or screenshot UI exists yet, so no buttons for them.
    let settings = circular_button("qs-settings", "emblem-system-symbolic", "Settings");
    settings.connect_clicked(glib::clone!(
        #[strong]
        ctx,
        move |_| {
            ctx.popover.popdown();
            system::spawn(&["gnome-control-center"]);
        }
    ));
    row.append(&settings);

    let revealer = power_menu(ctx);
    let power = circular_button("qs-power", "system-shutdown-symbolic", "Power Off / Log Out");
    power.connect_clicked(glib::clone!(
        #[strong]
        ctx,
        #[weak]
        revealer,
        move |_| ctx.host.toggle(&revealer)
    ));
    row.append(&power);

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.append(&row);
    root.append(&revealer);
    root
}

fn circular_button(name: &str, icon: &str, tooltip: &str) -> gtk::Button {
    gtk::Button::builder()
        .name(name)
        .icon_name(icon)
        .tooltip_text(tooltip)
        .valign(gtk::Align::Center)
        .css_classes(["circular"])
        .build()
}

/// GNOME formats percentages with a thin space: "76 %".
fn percentage(value: f64) -> String {
    format!("{}\u{2009}%", value.round() as i64)
}

/// The battery pill in the top row, plus the panel's battery indicator.
fn battery_button(ctx: &Rc<Ctx>) -> gtk::Button {
    let icon = gtk::Image::new();
    let label = gtk::Label::new(None);
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    content.append(&icon);
    content.append(&label);
    let button = gtk::Button::builder()
        .name("qs-battery")
        .child(&content)
        .valign(gtk::Align::Center)
        .visible(false)
        .css_classes(["battery-button"])
        .build();
    button.connect_clicked(glib::clone!(
        #[strong]
        ctx,
        move |_| {
            ctx.popover.popdown();
            system::open_settings("power");
        }
    ));

    let Some(conn) = system::system_bus() else {
        return button;
    };
    let interface = system::settings("org.gnome.desktop.interface");
    let refresh = glib::clone!(
        #[weak]
        button,
        #[strong]
        ctx,
        #[strong]
        conn,
        #[strong]
        interface,
        move || {
            let props = system::get_all(&conn, UPOWER, DISPLAY_DEVICE, UPOWER_DEVICE);
            let present = props.as_ref().is_some_and(|p| p.get::<bool>("IsPresent").unwrap_or(false));
            let (icon_name, text) = match &props {
                Some(p) if present => (p.string("IconName"), percentage(p.get::<f64>("Percentage").unwrap_or(0.0))),
                _ => (String::new(), String::new()),
            };
            let show_percentage = interface
                .as_ref()
                .is_some_and(|s| s.boolean("show-battery-percentage"));

            button.set_visible(present);
            icon.set_icon_name(Some(&icon_name));
            label.set_label(&text);
            let indicators = &ctx.indicators;
            indicators.battery.set_icon_name(Some(&icon_name));
            indicators.battery.set_visible(present);
            indicators.battery_percentage.set_label(&text);
            indicators.battery_percentage.set_visible(present && show_percentage);
        }
    );
    if let Some(interface) = &interface {
        let refresh = refresh.clone();
        interface.connect_changed(Some("show-battery-percentage"), move |_, _| refresh());
    }
    system::watch_service(&conn, UPOWER, refresh);
    button
}

// --- power submenu ----------------------------------------------------------

fn power_menu(ctx: &Rc<Ctx>) -> gtk::Revealer {
    let menu = QuickMenu::new("system-shutdown-symbolic", "Power Off");
    menu.root.set_margin_top(12);
    let revealer = gtk::Revealer::builder()
        .child(&menu.root)
        .transition_type(gtk::RevealerTransitionType::SlideDown)
        .transition_duration(200)
        .build();

    let suspend = MenuItem::new("qs-suspend", "Suspend");
    suspend.connect_clicked(glib::clone!(
        #[strong]
        ctx,
        move |_| {
            ctx.popover.popdown();
            logind("Suspend");
        }
    ));
    let restart = MenuItem::new("qs-restart", "Restart…");
    restart.connect_clicked(glib::clone!(
        #[strong]
        ctx,
        move |_| confirm(&ctx, "qs-confirm-restart", "Restart", "restart", "_Restart", "Reboot")
    ));
    let power_off = MenuItem::new("qs-power-off", "Power Off…");
    power_off.connect_clicked(glib::clone!(
        #[strong]
        ctx,
        move |_| confirm(&ctx, "qs-confirm-power-off", "Power Off", "power off", "_Power Off", "PowerOff")
    ));
    let log_out = MenuItem::new("qs-log-out", "Log Out");
    log_out.connect_clicked(glib::clone!(
        #[strong]
        ctx,
        move |_| {
            ctx.popover.popdown();
            match system::session_bus().filter(|bus| has_owner(bus, SESSION_MANAGER)) {
                // 1 = no confirmation.
                Some(bus) => system::call(
                    &bus,
                    SESSION_MANAGER,
                    "/org/gnome/SessionManager",
                    SESSION_MANAGER,
                    "Logout",
                    Some((1u32,).to_variant()),
                ),
                None => ctx.client.log_out(),
            }
        }
    ));
    let switch_user = MenuItem::new("qs-switch-user", "Switch User…");
    switch_user.connect_clicked(glib::clone!(
        #[strong]
        ctx,
        move |_| {
            ctx.popover.popdown();
            if let Some(bus) = system::system_bus() {
                system::call(
                    &bus,
                    DISPLAY_MANAGER,
                    "/org/gnome/DisplayManager/LocalDisplayFactory",
                    "org.gnome.DisplayManager.LocalDisplayFactory",
                    "CreateTransientDisplay",
                    None,
                );
            }
        }
    ));

    let separator = gtk::Separator::new(gtk::Orientation::Horizontal);
    for item in [suspend.upcast_ref::<gtk::Widget>(), restart.upcast_ref(), power_off.upcast_ref()] {
        menu.add(item);
    }
    menu.add(&separator);
    menu.add(&log_out);
    menu.add(&switch_user);

    // Like GNOME's SystemActions: only offer what logind allows, and Switch
    // User only with GDM. Checked each time the menu opens.
    let sync = glib::clone!(
        #[weak]
        suspend,
        #[weak]
        restart,
        #[weak]
        power_off,
        #[weak]
        separator,
        #[weak]
        switch_user,
        move || {
            let bus = system::system_bus();
            let can = |method: &str| {
                bus.as_ref()
                    .and_then(|bus| system::call_sync(bus, LOGIND, LOGIND_PATH, LOGIND_MANAGER, method, None))
                    .and_then(|reply| reply.child_value(0).str().map(str::to_owned))
                    .is_some_and(|answer| answer == "yes" || answer == "challenge")
            };
            suspend.set_visible(can("CanSuspend"));
            restart.set_visible(can("CanReboot"));
            power_off.set_visible(can("CanPowerOff"));
            separator.set_visible(suspend.is_visible() || restart.is_visible() || power_off.is_visible());
            switch_user.set_visible(bus.as_ref().is_some_and(|bus| has_owner(bus, DISPLAY_MANAGER)));
        }
    );
    sync();
    revealer.connect_reveal_child_notify(move |r| {
        if r.reveals_child() {
            sync();
        }
    });
    revealer
}

fn has_owner(conn: &gio::DBusConnection, name: &str) -> bool {
    system::call_sync(
        conn,
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
        "NameHasOwner",
        Some(&(name,).to_variant()),
    )
    .and_then(|reply| reply.child_value(0).get::<bool>())
    .unwrap_or(false)
}

/// Calls a logind action (`Suspend`, `Reboot`, `PowerOff`), allowing polkit
/// to ask for authorization.
fn logind(method: &str) {
    if let Some(bus) = system::system_bus() {
        system::call(&bus, LOGIND, LOGIND_PATH, LOGIND_MANAGER, method, Some((true,).to_variant()));
    }
}

/// GNOME's end-session dialog: confirm, or act after a countdown.
fn confirm(ctx: &Rc<Ctx>, name: &str, heading: &str, verb: &str, accept: &str, method: &'static str) {
    ctx.popover.popdown();
    let body = {
        let verb = verb.to_owned();
        move |seconds: u32| format!("The system will {verb} automatically in {seconds} seconds.")
    };
    let dialog = adw::AlertDialog::builder()
        .name(name)
        .heading(heading)
        .body(body(END_SESSION_TIMEOUT))
        .close_response("cancel")
        .default_response("confirm")
        .build();
    dialog.add_responses(&[("cancel", "_Cancel"), ("confirm", accept)]);
    if method == "PowerOff" {
        dialog.set_response_appearance("confirm", adw::ResponseAppearance::Destructive);
    }

    let done = Rc::new(Cell::new(false));
    dialog.connect_response(
        None,
        glib::clone!(
            #[strong]
            done,
            move |_, response| {
                done.set(true);
                if response == "confirm" {
                    logind(method);
                }
            }
        ),
    );

    let remaining = Cell::new(END_SESSION_TIMEOUT);
    glib::timeout_add_seconds_local(
        1,
        glib::clone!(
            #[weak]
            dialog,
            #[upgrade_or]
            glib::ControlFlow::Break,
            move || {
                if done.get() {
                    return glib::ControlFlow::Break;
                }
                let left = remaining.get().saturating_sub(1);
                remaining.set(left);
                if left == 0 {
                    done.set(true);
                    dialog.force_close();
                    logind(method);
                    return glib::ControlFlow::Break;
                }
                dialog.set_body(&body(left));
                glib::ControlFlow::Continue
            }
        ),
    );
    dialog.present(None::<&gtk::Widget>);
}

// --- brightness -------------------------------------------------------------

pub fn brightness_slider(ctx: &Rc<Ctx>, sliders: &gtk::Box) {
    let slider = QuickSlider::new("qs-brightness", "display-brightness-symbolic", None);
    slider.root.set_visible(false);
    let client = ctx.client.clone();
    slider.connect_changed(move |value| client.set_brightness(value));

    let weak = Rc::downgrade(&slider);
    ctx.client.connect_state(move |state| {
        if let Some(slider) = weak.upgrade() {
            slider.root.set_visible(state.brightness.is_some());
            if let Some(value) = state.brightness {
                slider.set_value(value);
            }
        }
    });
    sliders.append(&slider.root);
    // The state callback only holds a weak reference; tie the slider's
    // lifetime to its widget.
    slider.root.clone().connect_destroy(move |_| {
        let _ = &slider;
    });
}

// --- power profiles ---------------------------------------------------------

const PROFILE_SERVICES: [(&str, &str); 2] = [
    ("org.freedesktop.UPower.PowerProfiles", "/org/freedesktop/UPower/PowerProfiles"),
    ("net.hadess.PowerProfiles", "/net/hadess/PowerProfiles"),
];

/// Menu order and labels, as in GNOME.
const PROFILES: [(&str, &str); 3] = [
    ("performance", "Performance"),
    ("balanced", "Balanced"),
    ("power-saver", "Power Saver"),
];

fn profile_icon(profile: &str) -> String {
    format!("power-profile-{profile}-symbolic")
}

pub fn profiles_toggle(ctx: &Rc<Ctx>) -> Vec<Rc<QuickToggle>> {
    let Some(conn) = system::system_bus() else {
        return Vec::new();
    };
    // The service's name doubles as its interface name.
    let Some((name, path)) = PROFILE_SERVICES
        .into_iter()
        .find(|(name, path)| system::get_all(&conn, name, path, name).is_some())
    else {
        return Vec::new();
    };

    let toggle = QuickToggle::with_menu("qs-power-mode", &profile_icon("balanced"), "Power Mode", &ctx.host);
    let set_profile = Rc::new(glib::clone!(
        #[strong]
        conn,
        move |profile: &str| system::set_prop(&conn, name, path, name, "ActiveProfile", profile.to_variant())
    ));

    let refresh = glib::clone!(
        #[weak]
        toggle,
        #[strong]
        conn,
        #[strong]
        set_profile,
        move || {
            let Some(props) = system::get_all(&conn, name, path, name) else {
                toggle.set_visible(false);
                return;
            };
            let active = props.string("ActiveProfile");
            let available: Vec<String> = props
                .value("Profiles")
                .map(|list| {
                    list.iter()
                        .filter_map(|dict| system::lookup(&dict, "Profile")?.str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default();

            let label = PROFILES.iter().find(|(id, _)| *id == active).map(|(_, label)| *label);
            toggle.set_visible(true);
            toggle.set_checked(active != "balanced");
            toggle.set_subtitle(label);
            toggle.set_icon(&profile_icon(if label.is_some() { &active } else { "balanced" }));

            let Some(menu) = &toggle.menu else { return };
            menu.clear();
            for (id, label) in PROFILES.iter().filter(|(id, _)| available.iter().any(|a| a == id)) {
                let item = MenuItem::build(&format!("qs-profile-{id}"), Some(&profile_icon(id)), label, None, *id == active);
                let set_profile = set_profile.clone();
                item.connect_clicked(move |_| set_profile(id));
                menu.add(&item);
            }
        }
    );
    system::watch_service(&conn, name, refresh);

    toggle.connect_clicked(glib::clone!(
        #[weak]
        toggle,
        move || set_profile(if toggle.is_checked() { "balanced" } else { "power-saver" })
    ));
    vec![toggle]
}
