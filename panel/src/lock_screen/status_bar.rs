//! The lock screen's top bar, like GNOME Shell's panel in its unlock-dialog
//! mode: no Activities or clock, just the accessibility menu, the keyboard
//! layout and the system status icons. The status icons are live copies of
//! the top bar's (quick_settings::Indicators::mirror), so nothing here
//! watches a service of its own. Clicking them does nothing.
//!
//! The accessibility menu opens inside the lock window: a popover would be
//! a surface of its own, which stays hidden under the shield.

use crate::ipc::ShellState;
use crate::quick_settings::Indicators;
use crate::system;
use gtk::prelude::*;
use std::cell::RefCell;

const A11Y_APPS: &str = "org.gnome.desktop.a11y.applications";
const A11Y_KEYBOARD: &str = "org.gnome.desktop.a11y.keyboard";
const LARGE_TEXT: f64 = 1.25;

pub struct StatusBar {
    /// The bar, to overlay at the top of the lock window.
    pub bar: gtk::CenterBox,
    /// The accessibility menu, to overlay below the bar.
    pub menu: gtk::Revealer,
    a11y: gtk::ToggleButton,
    layout: gtk::Label,
    shown_layout: RefCell<Option<String>>,
}

impl StatusBar {
    pub fn new(indicators: &Indicators) -> Self {
        let a11y = gtk::ToggleButton::builder()
            .name("lock-a11y-button")
            .icon_name("preferences-desktop-accessibility-symbolic")
            .focus_on_click(false)
            .css_classes(["lock-bar-button"])
            .build();
        // No tooltips: like popovers, they'd stay hidden under the shield.
        a11y.update_property(&[gtk::accessible::Property::Label("Accessibility")]);
        let layout = gtk::Label::builder()
            .name("lock-keyboard-layout")
            .css_classes(["lock-bar-item", "input-source"])
            .visible(false)
            .build();
        let status = indicators.mirror();
        status.set_widget_name("lock-status-icons");
        status.add_css_class("lock-bar-item");

        let end = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        end.append(&a11y);
        end.append(&layout);
        end.append(&status);
        let bar = gtk::CenterBox::builder()
            .name("lock-bar")
            .valign(gtk::Align::Start)
            .css_classes(["lock-bar"])
            .end_widget(&end)
            .build();

        let menu = gtk::Revealer::builder()
            .name("lock-a11y-menu")
            .child(&a11y_menu())
            .transition_type(gtk::RevealerTransitionType::Crossfade)
            .transition_duration(150)
            .halign(gtk::Align::End)
            .valign(gtk::Align::Start)
            .build();
        a11y.bind_property("active", &menu, "reveal-child").sync_create().build();
        // Not clickable while hidden.
        menu.bind_property("child-revealed", &menu, "can-target").sync_create().build();

        Self { bar, menu, a11y, layout, shown_layout: RefCell::default() }
    }

    /// The keyboard layout's short name, shown with more than one layout.
    pub fn update(&self, state: &ShellState) {
        let sources = &state.input_sources;
        let current = usize::try_from(state.input_source).ok().and_then(|i| sources.get(i));
        let text = current.filter(|_| sources.len() > 1).map(|s| s.short_name.clone());
        if *self.shown_layout.borrow() == text {
            return;
        }
        self.layout.set_label(text.as_deref().unwrap_or_default());
        let description = current.map(|s| s.display_name.as_str()).unwrap_or_default();
        self.layout.update_property(&[gtk::accessible::Property::Description(description)]);
        self.layout.set_visible(text.is_some());
        self.shown_layout.replace(text);
    }

    /// For a click at (x, y) in `window`: true if the click belongs to the
    /// accessibility menu or its button, or closed the open menu.
    pub fn handle_click(&self, window: &gtk::ApplicationWindow, x: f64, y: f64) -> bool {
        let target = window.pick(x, y, gtk::PickFlags::DEFAULT);
        let inside = |w: &gtk::Widget| target.as_ref().is_some_and(|t| t == w || t.is_ancestor(w));
        inside(self.menu.upcast_ref()) || inside(self.a11y.upcast_ref()) || self.close_menu()
    }

    /// Closes the accessibility menu; false if it wasn't open.
    pub fn close_menu(&self) -> bool {
        let open = self.a11y.is_active();
        self.a11y.set_active(false);
        open
    }
}

/// GNOME's accessibility menu: switches for the a11y GSettings (only those
/// whose schema is installed). A binding keeps its settings object alive.
fn a11y_menu() -> gtk::Box {
    let menu = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .css_classes(["lock-a11y-menu"])
        .build();
    let items: [(&str, &str, &str, &str); 10] = [
        ("high-contrast", "High Contrast", "org.gnome.desktop.a11y.interface", "high-contrast"),
        ("zoom", "Zoom", A11Y_APPS, "screen-magnifier-enabled"),
        ("large-text", "Large Text", "org.gnome.desktop.interface", "text-scaling-factor"),
        ("screen-reader", "Screen Reader", A11Y_APPS, "screen-reader-enabled"),
        ("screen-keyboard", "Screen Keyboard", A11Y_APPS, "screen-keyboard-enabled"),
        ("visual-alerts", "Visual Alerts", "org.gnome.desktop.wm.preferences", "visual-bell"),
        ("sticky-keys", "Sticky Keys", A11Y_KEYBOARD, "stickykeys-enable"),
        ("slow-keys", "Slow Keys", A11Y_KEYBOARD, "slowkeys-enable"),
        ("bounce-keys", "Bounce Keys", A11Y_KEYBOARD, "bouncekeys-enable"),
        ("mouse-keys", "Mouse Keys", A11Y_KEYBOARD, "mousekeys-enable"),
    ];
    for (id, label, schema, key) in items {
        let Some(settings) = system::settings(schema)
            .filter(|s| s.settings_schema().is_some_and(|schema| schema.has_key(key)))
        else {
            continue;
        };
        let switch = gtk::Switch::builder()
            .name(format!("lock-a11y-{id}"))
            .focus_on_click(false)
            .valign(gtk::Align::Center)
            .build();
        if key == "text-scaling-factor" {
            settings
                .bind(key, &switch, "active")
                .mapping(|variant, _| Some((variant.get::<f64>()? > 1.0).to_value()))
                .set_mapping(|value, _| {
                    let large = value.get::<bool>().ok()?;
                    Some((if large { LARGE_TEXT } else { 1.0 }).to_variant())
                })
                .build();
        } else {
            settings.bind(key, &switch, "active").build();
        }
        let row = gtk::Box::builder().spacing(12).css_classes(["lock-a11y-item"]).build();
        row.append(&gtk::Label::builder().label(label).xalign(0.0).hexpand(true).build());
        row.append(&switch);
        menu.append(&row);
    }
    menu
}
