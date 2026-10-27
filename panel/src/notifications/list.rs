//! The date menu's notification column: notifications grouped per app
//! (newest first, like GNOME 46+), the empty state, and the Do Not
//! Disturb switch and Clear button at the bottom.

use super::card::{Card, Style};
use super::model::Notification;
use super::Notifications;
use crate::system;
use gtk::prelude::*;
use gtk::{glib, pango};
use std::rc::Rc;

const MAX_HEIGHT: i32 = 400;

pub fn build(owner: &Rc<Notifications>) -> gtk::Widget {
    let column = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .width_request(360)
        .css_classes(["notifications"])
        .build();

    let groups = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(18)
        .css_classes(["notification-list"])
        .build();
    let scrolled = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(true)
        .max_content_height(MAX_HEIGHT)
        .vexpand(true)
        .child(&groups)
        .build();
    let stack = gtk::Stack::builder()
        .vexpand(true)
        .transition_type(gtk::StackTransitionType::Crossfade)
        .build();
    stack.add_named(&empty_state(), Some("empty"));
    stack.add_named(&scrolled, Some("list"));
    column.append(&stack);

    let clear = gtk::Button::builder()
        .name("notification-clear")
        .label("Clear")
        .valign(gtk::Align::Center)
        .css_classes(["clear-button"])
        .build();
    clear.connect_clicked(glib::clone!(
        #[weak]
        owner,
        move |_| owner.clear()
    ));
    column.append(&bottom_row(&clear));

    let refresh = glib::clone!(
        #[weak]
        owner,
        #[weak]
        groups,
        #[weak]
        stack,
        #[weak]
        clear,
        move || {
            let listed = owner.listed();
            fill(&owner, &groups, &listed);
            stack.set_visible_child_name(if listed.is_empty() { "empty" } else { "list" });
            clear.set_sensitive(!listed.is_empty());
        }
    );
    refresh();
    owner.connect_changed(refresh.clone());

    // Opening the menu marks everything seen and refreshes the time stamps.
    column.connect_map(glib::clone!(
        #[weak]
        owner,
        move |_| {
            owner.set_menu_open(true);
            refresh();
        }
    ));
    column.connect_unmap(glib::clone!(
        #[weak]
        owner,
        move |_| owner.set_menu_open(false)
    ));
    column.upcast()
}

fn empty_state() -> adw::StatusPage {
    adw::StatusPage::builder()
        .icon_name("preferences-system-notifications-symbolic")
        .title("No Notifications")
        .vexpand(true)
        .css_classes(["compact"])
        .build()
}

/// "Do Not Disturb [switch]" on the left, "Clear" on the right.
fn bottom_row(clear: &gtk::Button) -> gtk::Box {
    let row = gtk::Box::builder().spacing(12).css_classes(["notification-bottom-row"]).build();
    row.append(&gtk::Label::builder().label("Do Not Disturb").xalign(0.0).build());
    let dnd = gtk::Switch::builder()
        .name("notification-dnd")
        .valign(gtk::Align::Center)
        .hexpand(true)
        .halign(gtk::Align::Start)
        .build();
    if let Some(settings) = system::settings(super::SCHEMA) {
        settings.bind("show-banners", &dnd, "active").invert_boolean().build();
        // The binding lives as long as the settings object.
        dnd.connect_destroy(move |_| {
            let _ = &settings;
        });
    }
    row.append(&dnd);
    row.append(clear);
    row
}

/// Rebuilds the groups: one per app, ordered by their newest notification.
fn fill(owner: &Rc<Notifications>, groups: &gtk::Box, listed: &[Rc<Notification>]) {
    while let Some(child) = groups.first_child() {
        groups.remove(&child);
    }
    let mut keys: Vec<&str> = Vec::new();
    for notification in listed {
        if !keys.contains(&notification.app.key()) {
            keys.push(notification.app.key());
        }
    }
    for key in keys {
        let members: Vec<&Rc<Notification>> = listed.iter().filter(|n| n.app.key() == key).collect();
        groups.append(&group(owner, groups, &members));
    }
}

fn group(owner: &Rc<Notifications>, list: &gtk::Box, members: &[&Rc<Notification>]) -> gtk::Box {
    let group = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .css_classes(["notification-group"])
        .build();
    group.append(&group_header(members[0]));
    for notification in members {
        let card = Card::new(&format!("notification-{}", notification.id), Rc::downgrade(owner));
        card.set(notification, Style::InGroup);
        // Running the default action also closes the menu. (The card is
        // gone from the list by then, so look up the menu from the list.)
        card.button.connect_clicked(glib::clone!(
            #[weak]
            list,
            move |_| {
                if let Some(popover) = list.ancestor(gtk::Popover::static_type()).and_downcast::<gtk::Popover>() {
                    popover.popdown();
                }
            }
        ));
        group.append(&card.button);
    }
    group
}

fn group_header(notification: &Notification) -> gtk::Box {
    let header = gtk::Box::builder().spacing(8).css_classes(["notification-group-header"]).build();
    let icon = gtk::Image::builder().pixel_size(16).build();
    match &notification.app.icon {
        Some(gicon) => icon.set_from_gicon(gicon),
        None => icon.set_icon_name(Some("application-x-executable-symbolic")),
    }
    header.append(&icon);
    header.append(
        &gtk::Label::builder()
            .label(&notification.app.name)
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(pango::EllipsizeMode::End)
            .css_classes(["app-name"])
            .build(),
    );
    header
}
