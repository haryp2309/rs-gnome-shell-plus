//! Simple toggles: Night Light, Dark Style (GSettings) and Airplane Mode
//! (gnome-settings-daemon's rfkill service).

use super::widgets::QuickToggle;
use super::Ctx;
use crate::system;
use gtk::glib;
use gtk::prelude::*;
use std::rc::Rc;

pub fn build(ctx: &Rc<Ctx>) -> Vec<Rc<QuickToggle>> {
    [night_light(), dark_style(), airplane_mode(ctx)]
        .into_iter()
        .flatten()
        .collect()
}

fn night_light() -> Option<Rc<QuickToggle>> {
    let settings = system::settings("org.gnome.settings-daemon.plugins.color")?;
    let toggle = QuickToggle::new("qs-night-light", "night-light-symbolic", "Night Light");

    let sync = glib::clone!(
        #[weak]
        toggle,
        #[strong]
        settings,
        move || toggle.set_checked(settings.boolean("night-light-enabled"))
    );
    sync();
    settings.connect_changed(Some("night-light-enabled"), move |_, _| sync());
    toggle.connect_clicked(glib::clone!(
        #[weak]
        toggle,
        move || {
            let _ = settings.set_boolean("night-light-enabled", !toggle.is_checked());
        }
    ));
    Some(toggle)
}

fn dark_style() -> Option<Rc<QuickToggle>> {
    let settings = system::settings("org.gnome.desktop.interface")?;
    let toggle = QuickToggle::new("qs-dark-style", "weather-clear-night-symbolic", "Dark Style");

    let sync = glib::clone!(
        #[weak]
        toggle,
        #[strong]
        settings,
        move || toggle.set_checked(settings.string("color-scheme") == "prefer-dark")
    );
    sync();
    settings.connect_changed(Some("color-scheme"), move |_, _| sync());
    toggle.connect_clicked(glib::clone!(
        #[weak]
        toggle,
        move || {
            let scheme = if toggle.is_checked() { "default" } else { "prefer-dark" };
            let _ = settings.set_string("color-scheme", scheme);
        }
    ));
    Some(toggle)
}

fn airplane_mode(ctx: &Rc<Ctx>) -> Option<Rc<QuickToggle>> {
    const NAME: &str = "org.gnome.SettingsDaemon.Rfkill";
    const PATH: &str = "/org/gnome/SettingsDaemon/Rfkill";

    let conn = system::session_bus()?;
    let toggle = QuickToggle::new("qs-airplane", "airplane-mode-symbolic", "Airplane Mode");
    toggle.set_visible(false);

    let refresh = glib::clone!(
        #[weak]
        toggle,
        #[strong]
        conn,
        #[strong]
        ctx,
        move || {
            let props = system::get_all(&conn, NAME, PATH, NAME);
            let show = props
                .as_ref()
                .is_some_and(|p| p.get::<bool>("ShouldShowAirplaneMode").unwrap_or(false));
            let on = props
                .as_ref()
                .is_some_and(|p| p.get::<bool>("AirplaneMode").unwrap_or(false));
            toggle.set_visible(show);
            toggle.set_checked(on);
            ctx.indicators.airplane.set_icon_name(Some("airplane-mode-symbolic"));
            ctx.indicators.airplane.set_visible(show && on);
        }
    );
    system::watch_service(&conn, NAME, refresh);

    toggle.connect_clicked(glib::clone!(
        #[weak]
        toggle,
        move || {
            system::set_prop(&conn, NAME, PATH, NAME, "AirplaneMode", (!toggle.is_checked()).to_variant());
        }
    ));
    Some(toggle)
}
