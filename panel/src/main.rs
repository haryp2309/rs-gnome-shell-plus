mod app_grid;
mod calendar;
mod clock;
mod debug;
mod ipc;
mod keyboard;
mod media;
mod network_agent;
mod notifications;
mod osd;
mod overlay;
mod overview;
mod panel;
mod polkit_agent;
mod quick_settings;
mod switcher;
mod system;
mod system_dialog;
mod wallpaper;
mod workspaces;

use adw::prelude::*;
use gtk::{gdk, gio, glib};

const APP_ID: &str = "org.gnome.ShellPlus.Panel";

fn main() -> glib::ExitCode {
    let app = adw::Application::builder()
        .application_id(APP_ID)
        // Don't depend on owning a bus name; the panel must come up even
        // without a session bus (e.g. when previewed outside the compositor).
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();

    app.connect_startup(|_| load_css());
    app.connect_activate(|app| {
        let client = ipc::ShellClient::new();
        let notifications = notifications::Notifications::install(app);
        osd::install(app, &client);
        switcher::install(app, &client);
        let overview = overview::install(app, &client);
        let on_activities: std::rc::Rc<dyn Fn()> = {
            let overview = overview.clone();
            std::rc::Rc::new(move || overview.toggle())
        };
        let top_bar = panel::build(app, &client, &notifications, on_activities);
        overview.connect_active(glib::clone!(
            #[strong]
            top_bar,
            move |active| top_bar.set_overview(active)
        ));
        top_bar.window.present();
        network_agent::install(app);
        polkit_agent::install(app);
        debug::install();
    });
    app.run()
}

fn load_css() {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(include_str!("style.css"));
    gtk::style_context_add_provider_for_display(
        &gdk::Display::default().expect("no display"),
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}
