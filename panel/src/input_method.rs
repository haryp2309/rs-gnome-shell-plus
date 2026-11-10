//! The IBus candidate pop-up (GNOME Shell's ibusCandidatePopup.js). The
//! compositor is the input method and the IBus panel (see
//! compositor/src/shellplus-input-method.c); it sends what to show over
//! `org.gnome.ShellPlus.InputMethod` and places this window at the text
//! cursor. The window is never focused (taking the focus from the client
//! would reset its input), but clicks on candidates and page buttons work.

use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use std::rc::Rc;

/// The compositor recognizes the pop-up by this title.
pub const TITLE: &str = "Input Method Candidates";

const BUS_NAME: &str = "org.gnome.ShellPlus";
const PATH: &str = "/org/gnome/ShellPlus/InputMethod";
const IFACE: &str = "org.gnome.ShellPlus.InputMethod";

const CSS: &str = r#"
.candidate-popup {
  margin: 4px;
  padding: 6px;
  border-radius: 12px;
  background-color: #2e2e32;
  color: white;
  box-shadow: 0 1px 4px rgba(0, 0, 0, 0.4);
  font-size: 1.1em;
}
.candidate-popup .candidate-text-label {
  padding: 0 6px 2px;
}
.candidate-popup .candidate-text-label.preedit {
  text-decoration: underline;
}
.candidate-popup .candidate-text-label.auxiliary {
  color: rgba(255, 255, 255, 0.7);
}
.candidate-popup button.candidate-box {
  min-height: 0;
  min-width: 0;
  padding: 3px 8px;
  border-radius: 6px;
  background: none;
  color: white;
  font-weight: normal;
}
.candidate-popup button.candidate-box:hover {
  background-color: rgba(255, 255, 255, 0.1);
}
.candidate-popup button.candidate-box.selected {
  background-color: @accent_bg_color;
  color: @accent_fg_color;
}
.candidate-popup .candidate-index {
  padding-right: 6px;
  color: rgba(255, 255, 255, 0.6);
}
.candidate-popup .candidate-box.selected .candidate-index {
  color: inherit;
}
.candidate-popup button.candidate-page-button {
  min-height: 0;
  min-width: 0;
  padding: 2px 6px;
  background: none;
  color: white;
}
"#;

#[derive(Debug, Default, PartialEq)]
struct Popup {
    visible: bool,
    preedit: String,
    auxiliary: String,
    candidates: Vec<(String, String)>,
    selected: i32,
    horizontal: bool,
    page: i32,
    pages: i32,
}

impl Popup {
    fn from_variant(dict: &glib::Variant) -> Self {
        let dict = glib::VariantDict::new(Some(dict));
        let get = |key: &str| dict.lookup_value(key, None);
        Self {
            visible: get("visible").and_then(|v| v.get()).unwrap_or(false),
            preedit: get("preedit").and_then(|v| v.get()).unwrap_or_default(),
            auxiliary: get("auxiliary").and_then(|v| v.get()).unwrap_or_default(),
            candidates: get("candidates").and_then(|v| v.get()).unwrap_or_default(),
            selected: get("selected").and_then(|v| v.get()).unwrap_or(-1),
            horizontal: get("horizontal").and_then(|v| v.get()).unwrap_or(false),
            page: get("page").and_then(|v| v.get()).unwrap_or(0),
            pages: get("pages").and_then(|v| v.get()).unwrap_or(0),
        }
    }
}

struct CandidatePopup {
    connection: gio::DBusConnection,
    window: gtk::ApplicationWindow,
    preedit: gtk::Label,
    auxiliary: gtk::Label,
    candidates: gtk::Box,
    buttons: gtk::Box,
    previous: gtk::Button,
    next: gtk::Button,
}

pub fn install(app: &adw::Application) {
    let Ok(connection) = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) else {
        return;
    };

    let provider = gtk::CssProvider::new();
    provider.load_from_string(CSS);
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }

    let window = crate::overlay::window(app, TITLE);
    let popup = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .css_classes(["candidate-popup"])
        .build();
    let label = |class: &str| {
        gtk::Label::builder()
            .xalign(0.0)
            .css_classes(["candidate-text-label", class])
            .visible(false)
            .build()
    };
    let preedit = label("preedit");
    preedit.set_widget_name("im-preedit");
    let auxiliary = label("auxiliary");
    auxiliary.set_widget_name("im-auxiliary");
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    let candidates = gtk::Box::new(gtk::Orientation::Vertical, 0);
    candidates.set_hexpand(true);
    let buttons = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .valign(gtk::Align::End)
        .visible(false)
        .build();
    let page_button = |name: &str| {
        gtk::Button::builder()
            .name(name)
            .focus_on_click(false)
            .can_focus(false)
            .css_classes(["candidate-page-button"])
            .build()
    };
    let previous = page_button("im-page-up");
    let next = page_button("im-page-down");
    buttons.append(&previous);
    buttons.append(&next);
    row.append(&candidates);
    row.append(&buttons);
    popup.append(&preedit);
    popup.append(&auxiliary);
    popup.append(&row);
    window.set_child(Some(&popup));

    let this = Rc::new(CandidatePopup {
        connection: connection.clone(),
        window,
        preedit,
        auxiliary,
        candidates,
        buttons,
        previous: previous.clone(),
        next: next.clone(),
    });

    previous.connect_clicked(glib::clone!(
        #[weak]
        this,
        move |_| this.call("PageUp", None)
    ));
    next.connect_clicked(glib::clone!(
        #[weak]
        this,
        move |_| this.call("PageDown", None)
    ));

    {
        let this = this.clone();
        #[allow(deprecated)]
        connection.signal_subscribe(
            Some(BUS_NAME),
            Some(IFACE),
            Some("PopupChanged"),
            Some(PATH),
            None,
            gio::DBusSignalFlags::NONE,
            move |_, _, _, _, _, params| {
                this.update(&Popup::from_variant(&params.child_value(0)));
            },
        );
    }
}

impl CandidatePopup {
    fn update(self: &Rc<Self>, popup: &Popup) {
        if !popup.visible {
            self.window.set_visible(false);
            return;
        }

        self.preedit.set_label(&popup.preedit);
        self.preedit.set_visible(!popup.preedit.is_empty());
        self.auxiliary.set_label(&popup.auxiliary);
        self.auxiliary.set_visible(!popup.auxiliary.is_empty());

        let orientation = if popup.horizontal {
            gtk::Orientation::Horizontal
        } else {
            gtk::Orientation::Vertical
        };
        self.candidates.set_orientation(orientation);
        self.buttons.set_orientation(orientation);
        let (up, down) = if popup.horizontal {
            ("go-previous-symbolic", "go-next-symbolic")
        } else {
            ("go-up-symbolic", "go-down-symbolic")
        };
        self.previous.set_icon_name(up);
        self.next.set_icon_name(down);
        self.buttons.set_visible(popup.pages > 1);
        self.previous.set_sensitive(popup.page > 0);
        self.next.set_sensitive(popup.page < popup.pages - 1);

        while let Some(child) = self.candidates.first_child() {
            self.candidates.remove(&child);
        }
        self.candidates.set_visible(!popup.candidates.is_empty());
        for (i, (index, text)) in popup.candidates.iter().enumerate() {
            let content = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            content.append(
                &gtk::Label::builder()
                    .label(index)
                    .css_classes(["candidate-index"])
                    .build(),
            );
            content.append(
                &gtk::Label::builder()
                    .label(text)
                    .xalign(0.0)
                    .css_classes(["candidate-label"])
                    .build(),
            );
            let button = gtk::Button::builder()
                .name(format!("im-candidate-{i}"))
                .child(&content)
                .focus_on_click(false)
                .can_focus(false)
                .css_classes(["candidate-box"])
                .build();
            if i as i32 == popup.selected {
                button.add_css_class("selected");
            }
            button.connect_clicked(glib::clone!(
                #[weak(rename_to = this)]
                self,
                move |_| this.call("CandidateClicked", Some((i as u32,).to_variant()))
            ));
            self.candidates.append(&button);
        }

        // Shrink to the new content; the compositor places the window.
        self.window.set_default_size(-1, -1);
        self.window.present();
    }

    fn call(&self, method: &str, args: Option<glib::Variant>) {
        let method_name = method.to_owned();
        self.connection.call(
            Some(BUS_NAME),
            PATH,
            IFACE,
            method,
            args.as_ref(),
            None,
            gio::DBusCallFlags::NONE,
            -1,
            gio::Cancellable::NONE,
            move |result| {
                if let Err(err) = result {
                    glib::g_warning!("shellplus", "{method_name} failed: {err}");
                }
            },
        );
    }
}
