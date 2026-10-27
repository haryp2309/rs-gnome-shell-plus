//! The notification card shared by banners and the date menu list,
//! modelled on GNOME Shell's NotificationMessage: a header (app, time,
//! close button), the icon, summary and body, and the action buttons.
//!
//! The card is a button: clicking it runs the default action.

use super::model::{relative_time, Notification};
use super::Notifications;
use gtk::prelude::*;
use gtk::{glib, pango};
use std::cell::Cell;
use std::rc::{Rc, Weak};

const IMAGE_SIZE: i32 = 40;
const MAX_ACTIONS: usize = 3;

pub enum Style {
    /// Header shows the app icon and name (banners).
    WithApp,
    /// Header shows only the time; the group header names the app.
    InGroup,
}

pub struct Card {
    pub button: gtk::Button,
    content: gtk::Box,
    name: String,
    id: Rc<Cell<u32>>,
    owner: Weak<Notifications>,
}

impl Card {
    /// `name` is the card's widget name; its close and action buttons are
    /// named `<name>-close` and `<name>-action-<n>`.
    pub fn new(name: &str, owner: Weak<Notifications>) -> Rc<Self> {
        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let button = gtk::Button::builder()
            .name(name)
            .child(&content)
            .css_classes(["notification-card"])
            .build();
        let id = Rc::new(Cell::new(0));
        button.connect_clicked(glib::clone!(
            #[strong]
            id,
            #[strong]
            owner,
            move |_| {
                if let Some(owner) = owner.upgrade() {
                    owner.activate(id.get());
                }
            }
        ));
        Rc::new(Self { button, content, name: name.to_owned(), id, owner })
    }

    /// (Re)fills the card with `notification`.
    pub fn set(&self, notification: &Notification, style: Style) {
        while let Some(child) = self.content.first_child() {
            self.content.remove(&child);
        }
        let id = notification.id;
        self.content.append(&self.header(notification, style));
        self.content.append(&body(notification));
        if !notification.actions.is_empty() {
            self.content.append(&self.actions(notification));
        }

        self.id.set(id);
    }

    fn header(&self, notification: &Notification, style: Style) -> gtk::Box {
        let header = gtk::Box::builder().spacing(6).css_classes(["header"]).build();
        if let Style::WithApp = style {
            let icon = gtk::Image::builder().pixel_size(16).css_classes(["app-icon"]).build();
            match &notification.app.icon {
                Some(gicon) => icon.set_from_gicon(gicon),
                None => icon.set_icon_name(Some("application-x-executable-symbolic")),
            }
            header.append(&icon);
            header.append(
                &gtk::Label::builder()
                    .label(&notification.app.name)
                    .ellipsize(pango::EllipsizeMode::End)
                    .max_width_chars(24)
                    .css_classes(["app-name"])
                    .build(),
            );
        }
        header.append(
            &gtk::Label::builder()
                .label(relative_time(&notification.time))
                .xalign(0.0)
                .hexpand(true)
                .css_classes(["time"])
                .build(),
        );

        let close = gtk::Button::builder()
            .name(format!("{}-close", self.name))
            .icon_name("window-close-symbolic")
            .valign(gtk::Align::Center)
            .tooltip_text("Close")
            .css_classes(["close", "circular", "flat"])
            .build();
        let (owner, id) = (self.owner.clone(), notification.id);
        close.connect_clicked(move |_| {
            if let Some(owner) = owner.upgrade() {
                owner.dismiss(id);
            }
        });
        header.append(&close);
        header
    }

    fn actions(&self, notification: &Notification) -> gtk::Box {
        let row = gtk::Box::builder()
            .spacing(6)
            .homogeneous(true)
            .css_classes(["actions"])
            .build();
        for (index, action) in notification.actions.iter().take(MAX_ACTIONS).enumerate() {
            let button = gtk::Button::builder()
                .name(format!("{}-action-{index}", self.name))
                .label(&action.label)
                .hexpand(true)
                .build();
            if let Some(label) = button.child().and_downcast::<gtk::Label>() {
                label.set_ellipsize(pango::EllipsizeMode::End);
            }
            let (owner, id) = (self.owner.clone(), notification.id);
            button.connect_clicked(move |_| {
                if let Some(owner) = owner.upgrade() {
                    owner.invoke_action(id, index);
                }
            });
            row.append(&button);
        }
        row
    }
}

/// The icon next to the summary and body.
fn body(notification: &Notification) -> gtk::Box {
    let row = gtk::Box::builder().spacing(12).css_classes(["content"]).build();
    if let Some(image) = &notification.image {
        row.append(
            &gtk::Image::builder()
                .gicon(image)
                .pixel_size(IMAGE_SIZE)
                .valign(gtk::Align::Start)
                .css_classes(["image"])
                .build(),
        );
    }

    let text = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .hexpand(true)
        .valign(gtk::Align::Center)
        .build();
    text.append(
        &gtk::Label::builder()
            .label(&notification.summary)
            .xalign(0.0)
            .ellipsize(pango::EllipsizeMode::End)
            .max_width_chars(1)
            .css_classes(["summary"])
            .build(),
    );
    if !notification.body.is_empty() {
        text.append(
            &gtk::Label::builder()
                .label(&notification.body)
                .use_markup(true)
                .xalign(0.0)
                .wrap(true)
                .wrap_mode(pango::WrapMode::WordChar)
                .lines(3)
                .ellipsize(pango::EllipsizeMode::End)
                .max_width_chars(1)
                .css_classes(["body"])
                .build(),
        );
    }
    row.append(&text);
    row
}
