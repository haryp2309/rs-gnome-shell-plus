//! Center clock and the date menu (notifications placeholder + calendar).
//! Honors the org.gnome.desktop.interface clock-* keys like GNOME Shell.

use crate::system;
use adw::prelude::*;
use gtk::{gio, glib};

pub fn build() -> gtk::MenuButton {
    let label = gtk::Label::new(None);
    let button = gtk::MenuButton::builder()
        .child(&label)
        .popover(&build_popover())
        .css_classes(["panel-button", "clock"])
        .build();

    let settings = system::settings("org.gnome.desktop.interface");
    let update = move || {
        let text = format_clock(settings.as_ref());
        if label.label() != text {
            label.set_label(&text);
        }
    };
    update();
    glib::timeout_add_seconds_local(1, move || {
        update();
        glib::ControlFlow::Continue
    });

    button
}

fn format_clock(settings: Option<&gio::Settings>) -> String {
    let Ok(now) = glib::DateTime::now_local() else {
        return String::new();
    };
    let flag = |key: &str, default: bool| settings.map(|s| s.boolean(key)).unwrap_or(default);
    let twelve_hour = settings.is_some_and(|s| s.string("clock-format") == "12h");
    let seconds = flag("clock-show-seconds", false);

    let mut date = Vec::new();
    if flag("clock-show-weekday", false) {
        date.push("%a");
    }
    if flag("clock-show-date", true) {
        date.push("%b %-d");
    }
    let time = match (twelve_hour, seconds) {
        (false, false) => "%R",
        (false, true) => "%T",
        (true, false) => "%-l:%M %p",
        (true, true) => "%-l:%M:%S %p",
    };
    let format = if date.is_empty() {
        time.to_owned()
    } else {
        format!("{}  {}", date.join(" "), time)
    };
    now.format(&format).map(|s| s.to_string()).unwrap_or_default()
}

fn build_popover() -> gtk::Popover {
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    content.add_css_class("date-menu");

    // Notifications column. Notification support comes later; for now this
    // mirrors the empty state and the Do Not Disturb switch.
    let notifications = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .width_request(360)
        .css_classes(["notifications"])
        .build();
    let empty = adw::StatusPage::builder()
        .icon_name("preferences-system-notifications-symbolic")
        .title("No Notifications")
        .vexpand(true)
        .css_classes(["compact"])
        .build();
    notifications.append(&empty);

    let dnd_row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    dnd_row.append(
        &gtk::Label::builder()
            .label("Do Not Disturb")
            .xalign(0.0)
            .hexpand(true)
            .build(),
    );
    let dnd = gtk::Switch::builder().valign(gtk::Align::Center).build();
    if let Some(settings) = system::settings("org.gnome.desktop.notifications") {
        settings
            .bind("show-banners", &dnd, "active")
            .invert_boolean()
            .build();
        dnd.connect_destroy(move |_| {
            let _ = &settings;
        });
    }
    dnd_row.append(&dnd);
    notifications.append(&dnd_row);

    content.append(&notifications);
    content.append(&gtk::Separator::new(gtk::Orientation::Vertical));

    let calendar_column = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .css_classes(["calendar-column"])
        .build();
    let weekday = gtk::Label::builder().xalign(0.0).css_classes(["dim-label"]).build();
    let date = gtk::Label::builder().xalign(0.0).css_classes(["title-2"]).build();
    let calendar = gtk::Calendar::new();
    calendar_column.append(&weekday);
    calendar_column.append(&date);
    calendar_column.append(&calendar);
    content.append(&calendar_column);

    let popover = gtk::Popover::builder()
        .child(&content)
        .has_arrow(false)
        .css_classes(["panel-menu"])
        .build();

    // Jump back to today every time the menu opens.
    popover.connect_show(move |_| {
        if let Ok(now) = glib::DateTime::now_local() {
            weekday.set_label(&now.format("%A").unwrap_or_default());
            date.set_label(&now.format("%B %-d %Y").unwrap_or_default());
            calendar.select_day(&now);
        }
    });

    popover
}
