//! GNOME Shell's modal system dialog: a centered card on a dimmed
//! full-screen backdrop, with a heading, a description, some content
//! (entries) and a row of buttons. Used for prompts that come from system
//! services (NetworkManager secrets; later polkit and the keyring).
//!
//! The compositor recognizes the window by its title (`System Dialog`, see
//! compositor/src/shellplus-plugin.c), stretches it over the primary
//! monitor, keeps it above everything including the top bar, and gives it
//! keyboard focus. Escape cancels.

use adw::prelude::*;
use gtk::{gdk, glib};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Once;

pub const TITLE: &str = "System Dialog";

const FADE_MS: u32 = 150;
const CARD_WIDTH: i32 = 400;

pub struct SystemDialog {
    window: gtk::ApplicationWindow,
    heading: gtk::Label,
    body: gtk::Label,
    content: gtk::Box,
    buttons: gtk::Box,
    on_cancel: RefCell<Option<Box<dyn Fn()>>>,
    closed: Cell<bool>,
}

impl SystemDialog {
    /// A new, not yet shown dialog. `name` is the widget name of its window
    /// (for the debug interface); children are named `<name>-heading` etc.
    pub fn new(app: &adw::Application, name: &str) -> Rc<Self> {
        load_css();

        let heading = gtk::Label::builder()
            .name(format!("{name}-heading"))
            .css_classes(["heading"])
            .justify(gtk::Justification::Center)
            .wrap(true)
            .build();
        let body = gtk::Label::builder()
            .name(format!("{name}-body"))
            .css_classes(["body"])
            .justify(gtk::Justification::Center)
            .wrap(true)
            .max_width_chars(30)
            .build();
        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .css_classes(["fields"])
            .build();
        let text = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .css_classes(["message"])
            .build();
        text.append(&heading);
        text.append(&body);
        text.append(&content);

        let buttons = gtk::Box::builder()
            .spacing(12)
            .homogeneous(true)
            .css_classes(["buttons"])
            .build();

        let card = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .width_request(CARD_WIDTH)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .vexpand(true)
            .css_classes(["system-dialog-card"])
            .build();
        card.append(&text);
        card.append(&buttons);

        // The dimmed backdrop: the whole window, which the compositor puts
        // over the primary monitor. Its minimum size is the monitor's, or
        // GTK would shrink the window to the work area (configure bounds)
        // on the next relayout.
        let backdrop = gtk::Box::builder().orientation(gtk::Orientation::Vertical).build();
        backdrop.append(&card);
        fit_to_monitor(&backdrop, first_monitor().as_ref());

        let window = gtk::ApplicationWindow::builder()
            .application(app)
            .name(name)
            .title(TITLE)
            .decorated(false)
            .css_classes(["system-dialog"])
            .child(&backdrop)
            .build();
        window.connect_realize(glib::clone!(
            #[weak]
            backdrop,
            move |window| {
                if let Some(surface) = window.surface() {
                    surface.connect_enter_monitor(glib::clone!(
                        #[weak]
                        backdrop,
                        move |_, monitor| fit_to_monitor(&backdrop, Some(monitor))
                    ));
                }
            }
        ));

        let dialog = Rc::new(Self {
            window,
            heading,
            body,
            content,
            buttons,
            on_cancel: RefCell::default(),
            closed: Cell::new(false),
        });

        // Escape anywhere cancels (captured before entries see it).
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        keys.connect_key_pressed(glib::clone!(
            #[weak]
            dialog,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, key, _, _| {
                if key == gdk::Key::Escape {
                    dialog.cancel();
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            }
        ));
        dialog.window.add_controller(keys);

        // Closing the window some other way (e.g. Alt+F4) also cancels.
        dialog.window.connect_close_request(glib::clone!(
            #[weak]
            dialog,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_| {
                dialog.cancel();
                glib::Propagation::Stop
            }
        ));
        dialog
    }

    pub fn set_heading(&self, text: &str) {
        self.heading.set_label(text);
    }

    pub fn set_body(&self, text: &str) {
        self.body.set_label(text);
        self.body.set_visible(!text.is_empty());
    }

    /// Adds a widget (an entry, a check box…) below the description.
    pub fn add_content(&self, widget: &impl IsA<gtk::Widget>) {
        self.content.append(widget);
    }

    /// Adds a button to the bottom row. The `default` one is highlighted
    /// like GNOME's default dialog button.
    pub fn add_button(&self, name: &str, label: &str, default: bool) -> gtk::Button {
        let button = gtk::Button::builder()
            .name(name)
            .label(label)
            .use_underline(true)
            .build();
        if default {
            button.add_css_class("default");
            self.window.set_default_widget(Some(&button));
        }
        self.buttons.append(&button);
        button
    }

    /// Called when the user dismisses the dialog (Escape or closing the
    /// window); not called for [`close`](Self::close).
    pub fn connect_cancel(&self, f: impl Fn() + 'static) {
        self.on_cancel.replace(Some(Box::new(f)));
    }

    pub fn present(&self, focus: Option<&impl IsA<gtk::Widget>>) {
        let child = self.window.child().expect("backdrop");
        child.set_opacity(0.0);
        self.window.present();
        if let Some(widget) = focus {
            widget.grab_focus();
        }
        let fade = adw::TimedAnimation::new(
            &child,
            0.0,
            1.0,
            FADE_MS,
            adw::PropertyAnimationTarget::new(&child, "opacity"),
        );
        fade.play();
    }

    /// Runs the cancel handler, then closes.
    pub fn cancel(&self) {
        if self.closed.get() {
            return;
        }
        let handler = self.on_cancel.take();
        self.close();
        if let Some(handler) = handler {
            handler();
        }
    }

    /// Closes the dialog for good.
    pub fn close(&self) {
        if self.closed.replace(true) {
            return;
        }
        self.on_cancel.take();
        self.window.destroy();
    }
}

/// A first guess at the primary monitor, until the window enters one.
fn first_monitor() -> Option<gdk::Monitor> {
    gdk::Display::default()?.monitors().item(0)?.downcast().ok()
}

fn fit_to_monitor(backdrop: &gtk::Box, monitor: Option<&gdk::Monitor>) {
    if let Some(monitor) = monitor {
        let geometry = monitor.geometry();
        backdrop.set_size_request(geometry.width(), geometry.height());
    }
}

fn load_css() {
    static LOADED: Once = Once::new();
    LOADED.call_once(|| {
        let Some(display) = gdk::Display::default() else { return };
        let provider = gtk::CssProvider::new();
        provider.load_from_string(include_str!("system_dialog.css"));
        gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
    });
}
