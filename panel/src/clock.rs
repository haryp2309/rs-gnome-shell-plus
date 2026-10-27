//! Center clock and the date menu (media players, notifications, calendar).
//! Honors the org.gnome.desktop.interface clock-* keys like GNOME Shell.

use crate::notifications::Notifications;
use crate::system;
use gtk::prelude::*;
use gtk::{gio, glib};
use std::rc::Rc;

pub fn build(notifications: &Rc<Notifications>) -> gtk::MenuButton {
    let label = gtk::Label::new(None);
    // The clock, plus GNOME's unread-notifications dot / Do Not Disturb icon.
    let content = gtk::Box::builder().spacing(6).build();
    content.append(&label);
    content.append(&notifications.indicator());
    let button = gtk::MenuButton::builder()
        .name("clock-button")
        .child(&content)
        .popover(&build_popover(notifications))
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

fn build_popover(notifications: &Rc<Notifications>) -> gtk::Popover {
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    content.add_css_class("date-menu");

    // Media players above the notification list (or empty state) and the
    // Do Not Disturb switch.
    let messages = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(12).build();
    messages.append(&crate::media::section());
    messages.append(&notifications.date_menu_section());
    content.append(&messages);
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

    let popover = crate::panel::menu_popover(&content);

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
