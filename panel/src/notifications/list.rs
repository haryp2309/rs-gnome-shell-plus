//! The date menu's notification column: notifications grouped per app
//! (newest first, like GNOME 46+), the empty state, and the Do Not
//! Disturb switch and Clear button at the bottom.
//!
//! Like GNOME 46+, an app with several notifications is a collapsed stack:
//! its newest notification with the others peeking out below it, and their
//! count in the group header. Clicking the stack expands it; the header's
//! collapse button folds it back. Groups fold again when the menu closes.

use super::card::{Card, Style};
use super::model::Notification;
use super::Notifications;
use crate::system;
use gtk::prelude::*;
use gtk::{glib, pango};
use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

const MAX_HEIGHT: i32 = 400;
/// At most this many cards peek out under a collapsed stack.
const MAX_PEEKS: usize = 2;
const EXPAND_TIME_MS: u32 = 200;

/// The app keys (`App::key`) of the expanded groups.
type Expanded = Rc<RefCell<HashSet<String>>>;

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

    let expanded = Expanded::default();
    let refresh = glib::clone!(
        #[weak]
        owner,
        #[strong]
        expanded,
        #[weak]
        groups,
        #[weak]
        stack,
        #[weak]
        clear,
        move || {
            let listed = owner.listed();
            fill(&owner, &groups, &listed, &expanded);
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
        move |_| {
            owner.set_menu_open(false);
            expanded.borrow_mut().clear();
        }
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
fn fill(owner: &Rc<Notifications>, groups: &gtk::Box, listed: &[Rc<Notification>], expanded: &Expanded) {
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
        groups.append(&group(owner, groups, &members, expanded));
    }
}

/// A card in the list; clicking it runs the default action and closes the
/// menu, unless `on_click` handles the click.
fn list_card(
    owner: &Rc<Notifications>,
    list: &gtk::Box,
    notification: &Notification,
    on_click: Option<Rc<dyn Fn() -> bool>>,
) -> gtk::Button {
    let card = Card::new(&format!("notification-{}", notification.id), Rc::downgrade(owner));
    card.set(notification, Style::InGroup);
    let list = list.downgrade();
    card.set_click_handler(Some(Rc::new(move || {
        if on_click.as_ref().is_some_and(|handler| handler()) {
            return true;
        }
        // Running the default action also closes the menu. (The card is
        // gone from the list by then, so look up the menu from the list.)
        if let Some(popover) = list
            .upgrade()
            .and_then(|list| list.ancestor(gtk::Popover::static_type()))
            .and_downcast::<gtk::Popover>()
        {
            popover.popdown();
        }
        false
    })));
    card.button.clone()
}

fn group(
    owner: &Rc<Notifications>,
    list: &gtk::Box,
    members: &[&Rc<Notification>],
    expanded: &Expanded,
) -> gtk::Box {
    let key = members[0].app.key().to_owned();
    let group = gtk::Box::builder()
        .name(format!("notification-group-{key}"))
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .css_classes(["notification-group"])
        .build();
    let header = group_header(members[0]);
    group.append(&header);

    if members.len() == 1 {
        group.append(&list_card(owner, list, members[0], None));
        return group;
    }

    // "3" while collapsed, a collapse button while expanded.
    let count = gtk::Label::builder()
        .name(format!("notification-group-count-{key}"))
        .label(members.len().to_string())
        .valign(gtk::Align::Center)
        .css_classes(["notification-count"])
        .build();
    let collapse = gtk::Button::builder()
        .name(format!("notification-group-collapse-{key}"))
        .icon_name("go-up-symbolic")
        .valign(gtk::Align::Center)
        .tooltip_text("Show Less")
        .css_classes(["collapse", "circular", "flat"])
        .build();
    header.append(&count);
    header.append(&collapse);

    // The newest card on top of the stack, the next ones peeking out under
    // it while collapsed, and the rest of the cards revealed when expanded.
    let stack = gtk::Box::builder()
        .name(format!("notification-stack-{key}"))
        .orientation(gtk::Orientation::Vertical)
        .css_classes(["notification-stack"])
        .build();
    let peeks = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .css_classes(["notification-stack-peeks"])
        .build();
    for depth in 1..=MAX_PEEKS.min(members.len() - 1) {
        peeks.append(
            &gtk::Box::builder()
                .css_classes(["notification-stack-peek", &format!("depth-{depth}")])
                .build(),
        );
    }
    let rest = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .margin_top(6)
        .build();
    let revealer = gtk::Revealer::builder()
        .name(format!("notification-group-rest-{key}"))
        .transition_type(gtk::RevealerTransitionType::SlideDown)
        .transition_duration(EXPAND_TIME_MS)
        .child(&rest)
        .build();

    // Shows the group expanded or collapsed. The revealer animates only
    // when the user toggles it, not when the list is rebuilt.
    let apply = Rc::new(glib::clone!(
        #[weak]
        count,
        #[weak]
        collapse,
        #[weak]
        peeks,
        #[weak]
        stack,
        #[weak]
        revealer,
        move |open: bool, animate: bool| {
            count.set_visible(!open);
            collapse.set_visible(open);
            peeks.set_visible(!open);
            if open {
                stack.remove_css_class("collapsed");
            } else {
                stack.add_css_class("collapsed");
            }
            revealer.set_transition_duration(if animate { EXPAND_TIME_MS } else { 0 });
            revealer.set_reveal_child(open);
        }
    ));
    let set_open = {
        let (expanded, key, apply) = (expanded.clone(), key.clone(), apply.clone());
        Rc::new(move |open: bool| {
            if open {
                expanded.borrow_mut().insert(key.clone());
            } else {
                expanded.borrow_mut().remove(&key);
            }
            apply(open, true);
        })
    };

    // Clicking the collapsed stack expands it instead of running the top
    // notification's default action.
    let expand: Rc<dyn Fn() -> bool> = {
        let (expanded, key, set_open) = (expanded.clone(), key.clone(), set_open.clone());
        Rc::new(move || {
            if expanded.borrow().contains(&key) {
                return false;
            }
            set_open(true);
            true
        })
    };
    stack.append(&list_card(owner, list, members[0], Some(expand.clone())));
    stack.append(&peeks);
    // The peeking cards are part of the stack too.
    let click = gtk::GestureClick::new();
    click.connect_released(move |gesture, _, _, _| {
        if expand() {
            gesture.set_state(gtk::EventSequenceState::Claimed);
        }
    });
    peeks.add_controller(click);

    for notification in &members[1..] {
        rest.append(&list_card(owner, list, notification, None));
    }
    collapse.connect_clicked(move |_| set_open(false));

    let body = gtk::Box::builder().orientation(gtk::Orientation::Vertical).build();
    body.append(&stack);
    body.append(&revealer);
    group.append(&body);
    apply(expanded.borrow().contains(&key), false);
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
