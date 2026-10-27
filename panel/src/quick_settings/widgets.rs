//! Building blocks of the quick settings menu, modelled on GNOME Shell's
//! QuickToggle / QuickMenuToggle / QuickSlider and their submenus.

use gtk::prelude::*;
use gtk::{glib, pango};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// Makes sure only one submenu is open at a time.
#[derive(Default)]
pub struct MenuHost {
    open: RefCell<Option<gtk::Revealer>>,
}

impl MenuHost {
    pub fn toggle(&self, revealer: &gtk::Revealer) {
        let was_open = revealer.reveals_child();
        self.close_all();
        if !was_open {
            revealer.set_reveal_child(true);
            self.open.replace(Some(revealer.clone()));
        }
    }

    pub fn close_all(&self) {
        if let Some(revealer) = self.open.take() {
            revealer.set_reveal_child(false);
        }
    }
}

fn menu_revealer(menu: &QuickMenu) -> gtk::Revealer {
    gtk::Revealer::builder()
        .child(&menu.root)
        .transition_type(gtk::RevealerTransitionType::SlideDown)
        .transition_duration(200)
        .build()
}

fn arrow_button(name: &str) -> gtk::Button {
    gtk::Button::builder()
        .name(name)
        .icon_name("go-next-symbolic")
        .css_classes(["arrow"])
        .build()
}

/// A pill toggle: icon, title and optional subtitle. With a menu it splits
/// into the main button and an arrow that opens the submenu below its row.
pub struct QuickToggle {
    pub root: gtk::Box,
    pub button: gtk::Button,
    icon: gtk::Image,
    subtitle: gtk::Label,
    pub menu: Option<QuickMenu>,
    pub revealer: Option<gtk::Revealer>,
}

impl QuickToggle {
    pub fn new(name: &str, icon: &str, title: &str) -> Rc<Self> {
        Self::build(name, icon, title, None, None)
    }

    pub fn with_menu(name: &str, icon: &str, title: &str, host: &Rc<MenuHost>) -> Rc<Self> {
        Self::build(name, icon, title, Some(QuickMenu::new(icon, title)), Some(host))
    }

    fn build(name: &str, icon: &str, title: &str, menu: Option<QuickMenu>, host: Option<&Rc<MenuHost>>) -> Rc<Self> {
        let icon_image = gtk::Image::from_icon_name(icon);
        let labels = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .valign(gtk::Align::Center)
            .hexpand(true)
            .build();
        labels.append(
            &gtk::Label::builder()
                .label(title)
                .xalign(0.0)
                .ellipsize(pango::EllipsizeMode::End)
                .css_classes(["title"])
                .build(),
        );
        let subtitle = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(pango::EllipsizeMode::End)
            .visible(false)
            .css_classes(["subtitle"])
            .build();
        labels.append(&subtitle);

        let content = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        content.append(&icon_image);
        content.append(&labels);

        let button = gtk::Button::builder()
            .name(name)
            .child(&content)
            .hexpand(true)
            .css_classes(["main"])
            .build();
        let root = gtk::Box::builder().css_classes(["quick-toggle"]).hexpand(true).build();
        root.append(&button);

        let revealer = menu.as_ref().map(menu_revealer);
        if let (Some(revealer), Some(host)) = (&revealer, host) {
            root.add_css_class("has-menu");
            let arrow = arrow_button(&format!("{name}-arrow"));
            arrow.connect_clicked(glib::clone!(
                #[weak]
                revealer,
                #[strong]
                host,
                move |_| host.toggle(&revealer)
            ));
            root.append(&arrow);
        }

        Rc::new(Self {
            root,
            button,
            icon: icon_image,
            subtitle,
            menu,
            revealer,
        })
    }

    pub fn set_checked(&self, checked: bool) {
        if checked {
            self.root.add_css_class("checked");
        } else {
            self.root.remove_css_class("checked");
        }
    }

    pub fn is_checked(&self) -> bool {
        self.root.has_css_class("checked")
    }

    pub fn set_icon(&self, icon: &str) {
        self.icon.set_icon_name(Some(icon));
        if let Some(menu) = &self.menu {
            menu.set_icon(icon);
        }
    }

    pub fn set_subtitle(&self, subtitle: Option<&str>) {
        self.subtitle.set_label(subtitle.unwrap_or_default());
        self.subtitle.set_visible(subtitle.is_some_and(|s| !s.is_empty()));
    }

    pub fn set_visible(&self, visible: bool) {
        self.root.set_visible(visible);
        if !visible {
            if let Some(revealer) = &self.revealer {
                revealer.set_reveal_child(false);
            }
        }
    }

    pub fn connect_clicked(&self, f: impl Fn() + 'static) {
        self.button.connect_clicked(move |_| f());
    }

    /// Runs `f` each time the submenu opens (e.g. to start a Wi-Fi scan).
    pub fn connect_menu_opened(&self, f: impl Fn() + 'static) {
        if let Some(revealer) = &self.revealer {
            revealer.connect_reveal_child_notify(move |r| {
                if r.reveals_child() {
                    f();
                }
            });
        }
    }
}

/// A submenu: header with icon, title and subtitle, a list of items, and an
/// optional footer entry (usually "… Settings").
pub struct QuickMenu {
    pub root: gtk::Box,
    icon: gtk::Image,
    subtitle: gtk::Label,
    items: gtk::Box,
    footer: gtk::Box,
}

impl QuickMenu {
    pub fn new(icon: &str, title: &str) -> Self {
        let root = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .css_classes(["quick-menu"])
            .build();

        let header = gtk::Box::builder().spacing(12).css_classes(["header"]).build();
        let icon_image = gtk::Image::builder()
            .icon_name(icon)
            .valign(gtk::Align::Center)
            .css_classes(["header-icon"])
            .build();
        let labels = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .valign(gtk::Align::Center)
            .build();
        labels.append(
            &gtk::Label::builder()
                .label(title)
                .xalign(0.0)
                .css_classes(["header-title"])
                .build(),
        );
        let subtitle = gtk::Label::builder()
            .xalign(0.0)
            .visible(false)
            .css_classes(["header-subtitle"])
            .build();
        labels.append(&subtitle);
        header.append(&icon_image);
        header.append(&labels);
        root.append(&header);

        let items = gtk::Box::new(gtk::Orientation::Vertical, 2);
        root.append(&items);
        let footer = gtk::Box::new(gtk::Orientation::Vertical, 2);
        root.append(&footer);

        Self {
            root,
            icon: icon_image,
            subtitle,
            items,
            footer,
        }
    }

    pub fn set_icon(&self, icon: &str) {
        self.icon.set_icon_name(Some(icon));
    }

    pub fn set_subtitle(&self, subtitle: Option<&str>) {
        self.subtitle.set_label(subtitle.unwrap_or_default());
        self.subtitle.set_visible(subtitle.is_some_and(|s| !s.is_empty()));
    }

    pub fn clear(&self) {
        while let Some(child) = self.items.first_child() {
            self.items.remove(&child);
        }
    }

    pub fn add(&self, item: &impl IsA<gtk::Widget>) {
        self.items.append(item);
    }

    pub fn add_placeholder(&self, text: &str) {
        self.items.append(
            &gtk::Label::builder()
                .label(text)
                .xalign(0.0)
                .css_classes(["placeholder"])
                .build(),
        );
    }

    pub fn add_footer(&self, name: &str, label: &str, action: impl Fn() + 'static) {
        if self.footer.first_child().is_none() {
            self.footer.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        }
        let item = MenuItem::new(name, label);
        item.connect_clicked(move |_| action());
        self.footer.append(&item);
    }
}

/// A flat row in a submenu: optional icon, label, optional trailing icon
/// (e.g. a lock) and a check mark for the active entry.
pub struct MenuItem;

impl MenuItem {
    pub fn new(name: &str, label: &str) -> gtk::Button {
        Self::build(name, None, label, None, false)
    }

    pub fn build(name: &str, icon: Option<&str>, label: &str, trailing: Option<&str>, checked: bool) -> gtk::Button {
        let content = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        if let Some(icon) = icon {
            content.append(&gtk::Image::from_icon_name(icon));
        }
        content.append(
            &gtk::Label::builder()
                .label(label)
                .xalign(0.0)
                .hexpand(true)
                .ellipsize(pango::EllipsizeMode::End)
                .build(),
        );
        if let Some(trailing) = trailing {
            content.append(
                &gtk::Image::builder()
                    .icon_name(trailing)
                    .css_classes(["dim-label"])
                    .build(),
            );
        }
        content.append(
            &gtk::Image::builder()
                .icon_name("object-select-symbolic")
                .opacity(if checked { 1.0 } else { 0.0 })
                .build(),
        );
        gtk::Button::builder()
            .name(name)
            .child(&content)
            .css_classes(["menu-item"])
            .build()
    }
}

/// Icon button + slider (+ optional menu arrow), e.g. the volume row.
pub struct QuickSlider {
    pub root: gtk::Box,
    pub icon_button: gtk::Button,
    pub scale: gtk::Scale,
    pub menu: Option<QuickMenu>,
    updating: Cell<bool>,
}

impl QuickSlider {
    pub fn new(name: &str, icon: &str, menu: Option<(&str, &Rc<MenuHost>)>) -> Rc<Self> {
        let row = gtk::Box::builder().spacing(6).css_classes(["quick-slider"]).build();
        let icon_button = gtk::Button::builder()
            .name(format!("{name}-icon"))
            .icon_name(icon)
            .valign(gtk::Align::Center)
            .css_classes(["flat", "circular", "slider-icon"])
            .build();
        let scale = gtk::Scale::builder()
            .name(name)
            .orientation(gtk::Orientation::Horizontal)
            .adjustment(&gtk::Adjustment::new(0.0, 0.0, 1.0, 0.01, 0.1, 0.0))
            .hexpand(true)
            .build();
        row.append(&icon_button);
        row.append(&scale);

        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.append(&row);

        let menu = menu.map(|(title, host)| {
            let menu = QuickMenu::new(icon, title);
            let revealer = menu_revealer(&menu);
            let arrow = arrow_button(&format!("{name}-arrow"));
            arrow.set_valign(gtk::Align::Center);
            arrow.add_css_class("circular");
            arrow.connect_clicked(glib::clone!(
                #[weak]
                revealer,
                #[strong]
                host,
                move |_| host.toggle(&revealer)
            ));
            row.append(&arrow);
            root.append(&revealer);
            menu
        });

        Rc::new(Self {
            root,
            icon_button,
            scale,
            menu,
            updating: Cell::new(false),
        })
    }

    pub fn set_range(&self, max: f64) {
        self.scale.adjustment().set_upper(max);
    }

    /// Sets the value without triggering `connect_changed` handlers.
    pub fn set_value(&self, value: f64) {
        self.updating.set(true);
        self.scale.set_value(value);
        self.updating.set(false);
    }

    pub fn set_icon(&self, icon: &str) {
        self.icon_button.set_icon_name(icon);
    }

    /// Only fires for user changes, not for `set_value`.
    pub fn connect_changed(self: &Rc<Self>, f: impl Fn(f64) + 'static) {
        let this = Rc::downgrade(self);
        self.scale.connect_value_changed(move |scale| {
            if this.upgrade().is_some_and(|s| !s.updating.get()) {
                f(scale.value());
            }
        });
    }
}

/// Two-column grid of toggles that reflows when toggles are shown/hidden,
/// with each toggle's submenu revealed directly below its row.
pub struct ToggleGrid {
    pub root: gtk::Box,
    toggles: RefCell<Vec<Rc<QuickToggle>>>,
    pending: Cell<bool>,
}

impl ToggleGrid {
    pub fn new() -> Rc<Self> {
        Rc::new(Self {
            // No box spacing: collapsed submenu revealers are children too
            // and would add gaps. Rows get their own top margin instead.
            root: gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .css_classes(["toggle-grid"])
                .build(),
            toggles: RefCell::default(),
            pending: Cell::new(false),
        })
    }

    pub fn add(self: &Rc<Self>, toggle: Rc<QuickToggle>) {
        let weak = Rc::downgrade(self);
        toggle.root.connect_visible_notify(move |_| {
            if let Some(grid) = weak.upgrade() {
                grid.queue_relayout();
            }
        });
        self.toggles.borrow_mut().push(toggle);
        self.queue_relayout();
    }

    fn queue_relayout(self: &Rc<Self>) {
        if self.pending.replace(true) {
            return;
        }
        let weak = Rc::downgrade(self);
        glib::idle_add_local_once(move || {
            if let Some(grid) = weak.upgrade() {
                grid.pending.set(false);
                grid.relayout();
            }
        });
    }

    fn relayout(&self) {
        fn detach(widget: &impl IsA<gtk::Widget>) {
            if let Some(parent) = widget.parent().and_downcast::<gtk::Box>() {
                parent.remove(widget);
            }
        }

        let toggles = self.toggles.borrow();
        for toggle in toggles.iter() {
            detach(&toggle.root);
            if let Some(revealer) = &toggle.revealer {
                detach(revealer);
            }
        }
        while let Some(row) = self.root.first_child() {
            self.root.remove(&row);
        }

        let visible: Vec<_> = toggles.iter().filter(|t| t.root.is_visible()).collect();
        for (index, pair) in visible.chunks(2).enumerate() {
            let row = gtk::Box::builder()
                .spacing(12)
                .homogeneous(true)
                .margin_top(if index == 0 { 0 } else { 12 })
                .build();
            for toggle in pair {
                row.append(&toggle.root);
            }
            if pair.len() == 1 {
                // Keep a lone toggle at half width, like GNOME.
                row.append(&gtk::Box::new(gtk::Orientation::Horizontal, 0));
            }
            self.root.append(&row);
            for revealer in pair.iter().filter_map(|t| t.revealer.as_ref()) {
                self.root.append(revealer);
            }
        }
    }
}

pub fn status_icon() -> gtk::Image {
    gtk::Image::builder().visible(false).build()
}

/// GNOME's Wi-Fi/cellular strength buckets.
pub fn signal_icon(prefix: &str, strength: u8) -> String {
    let level = match strength {
        80.. => "excellent",
        55.. => "good",
        30.. => "ok",
        5.. => "weak",
        _ => "none",
    };
    format!("{prefix}-signal-{level}-symbolic")
}
