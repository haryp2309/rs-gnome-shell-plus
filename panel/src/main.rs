mod clock;
mod debug;
mod ipc;
mod panel;
mod quick_settings;
mod system;
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
        panel::build(app).present();
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
