//! Windows that float over the desktop: notification banners and the
//! volume/brightness OSD. The compositor recognizes them by title
//! (compositor/src/shellplus-plugin.c), makes them unfocusable
//! notification windows on every workspace, and places them.

use gtk::prelude::*;
use gtk::{cairo, glib};

pub const BANNER_TITLE: &str = "Notification Banner";
pub const OSD_TITLE: &str = "On-Screen Display";

/// A transparent, undecorated window that sizes itself to its content.
pub fn window(app: &adw::Application, title: &str) -> gtk::ApplicationWindow {
    gtk::ApplicationWindow::builder()
        .application(app)
        .title(title)
        .decorated(false)
        .resizable(false)
        .css_classes(["shellplus-overlay"])
        .build()
}

/// Lets pointer input pass through the window to whatever is below it.
pub fn set_click_through(window: &gtk::ApplicationWindow) {
    let apply = |window: &gtk::ApplicationWindow| {
        if let Some(surface) = window.surface() {
            surface.set_input_region(Some(&cairo::Region::create()));
        }
    };
    if window.is_realized() {
        apply(window);
    }
    window.connect_realize(glib::clone!(
        #[weak]
        window,
        move |_| apply(&window)
    ));
}
