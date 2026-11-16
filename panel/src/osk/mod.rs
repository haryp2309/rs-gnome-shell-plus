//! GNOME Shell's on-screen keyboard (js/ui/keyboard.js).
//!
//! It shows when org.gnome.desktop.a11y.applications screen-keyboard-enabled
//! is on and a text field has the input focus, and hides when the focus
//! leaves (or with its hide key, until the focus moves). The compositor
//! (compositor/src/shellplus-osk.c) says where the focus is and what the
//! field is for (org.gnome.ShellPlus.Osk), puts our window titled "Screen
//! Keyboard" full width at the bottom of the primary monitor without ever
//! focusing it, slides it in, keeps maximized windows above it and moves a
//! window up if the keyboard would cover its text cursor. Keys go back to
//! it: text is committed through its input method, Enter, Backspace and the
//! like are real key events. Only this process may send them.
//!
//! The layouts are GNOME Shell's (layouts/README): the active XKB layout's,
//! or a number pad, email or URL keyboard for such fields.
//!
//! On the lock screen this window stays hidden under the shield; the lock
//! screen embeds its own keyboard ([`lock_keyboard`]) that types straight
//! into its password entry.

mod icons;
mod json;
mod keyboard;
mod layout;

use crate::ipc::{ShellClient, ShellState};
use crate::system;
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use keyboard::{Keyboard, Output};
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::sync::Once;
use std::time::Duration;

/// The compositor recognizes the keyboard window by this title.
pub const TITLE: &str = "Screen Keyboard";

const BUS_NAME: &str = "org.gnome.ShellPlus";
const PATH: &str = "/org/gnome/ShellPlus/Osk";
const IFACE: &str = "org.gnome.ShellPlus.Osk";
const A11Y_APPS: &str = "org.gnome.desktop.a11y.applications";
const ENABLED_KEY: &str = "screen-keyboard-enabled";
/// How long a BackSpace held on the lock screen keyboard waits to repeat,
/// and then how often it repeats.
const REPEAT_DELAY: Duration = Duration::from_millis(500);
const REPEAT_INTERVAL: Duration = Duration::from_millis(50);

#[derive(Debug, Default, Clone, PartialEq)]
struct Focus {
    focused: bool,
    purpose: String,
}

impl Focus {
    fn from_variant(dict: &glib::Variant) -> Self {
        let dict = glib::VariantDict::new(Some(dict));
        Self {
            focused: dict.lookup::<bool>("focused").ok().flatten().unwrap_or(false),
            purpose: dict.lookup::<String>("purpose").ok().flatten().unwrap_or_default(),
        }
    }
}

/// The keyboard's size on a monitor: full width, a third of the height
/// (GNOME Shell's landscape keyboard).
fn keyboard_size(state: &ShellState) -> (i32, i32) {
    let (width, height) = state.monitor.map(|m| (m.2, m.3)).unwrap_or((1280, 800));
    (width, height / 3)
}

/// The layout file for a field: by purpose, else the active input source.
fn layout_for(state: &ShellState, purpose: &str) -> (&'static str, bool) {
    let multiple = state.input_sources.len() > 1;
    if let Some(id) = layout::for_purpose(purpose) {
        return (id, multiple);
    }
    let source = usize::try_from(state.input_source).ok().and_then(|i| state.input_sources.get(i));
    (source.map(|s| layout::for_xkb(&s.id)).unwrap_or("us"), multiple)
}

fn next_source(client: &ShellClient) {
    let state = client.state();
    let n = state.input_sources.len() as i32;
    if n > 1 {
        client.set_input_source((state.input_source.max(0) + 1) % n);
    }
}

fn load_css() {
    static LOADED: Once = Once::new();
    LOADED.call_once(|| {
        let Some(display) = gdk::Display::default() else { return };
        let provider = gtk::CssProvider::new();
        provider.load_from_string(include_str!("osk.css"));
        gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
    });
}

thread_local! {
    /// The lock screen's keyboards, kept on the active layout by
    /// [`install`]'s state callback (a callback can't be added from inside
    /// another, where the lock screen is built).
    static LOCK_KEYBOARDS: RefCell<Vec<Weak<Keyboard>>> = RefCell::default();
}

fn update_lock_keyboards(state: &ShellState) {
    let keyboards: Vec<Rc<Keyboard>> = LOCK_KEYBOARDS.with(|list| {
        let mut list = list.borrow_mut();
        list.retain(|k| k.strong_count() > 0);
        list.iter().filter_map(Weak::upgrade).collect()
    });
    for keyboard in keyboards {
        update_lock_keyboard(&keyboard, state);
    }
}

fn update_lock_keyboard(keyboard: &Keyboard, state: &ShellState) {
    let (width, height) = keyboard_size(state);
    keyboard.set_size(width, height);
    let (id, multiple) = layout_for(state, "password");
    keyboard.set_layout(id, multiple);
}

fn enabled_settings() -> Option<gio::Settings> {
    system::settings(A11Y_APPS).filter(|s| s.settings_schema().is_some_and(|schema| schema.has_key(ENABLED_KEY)))
}

// --- the overlay keyboard -------------------------------------------------

struct Osk {
    this: Weak<Osk>,
    client: Rc<ShellClient>,
    settings: Option<gio::Settings>,
    window: gtk::ApplicationWindow,
    keyboard: RefCell<Option<Rc<Keyboard>>>,
    focus: RefCell<Focus>,
    /// Hidden with its hide key; shown again when the focus moves.
    dismissed: Cell<bool>,
}

/// Sends what the keys do to the compositor.
struct CompositorOutput {
    connection: gio::DBusConnection,
    osk: Weak<Osk>,
}

impl CompositorOutput {
    fn call(&self, method: &'static str, args: glib::Variant) {
        self.connection.call(
            Some(BUS_NAME),
            PATH,
            IFACE,
            method,
            Some(&args),
            None,
            gio::DBusCallFlags::NONE,
            -1,
            gio::Cancellable::NONE,
            move |result| {
                if let Err(err) = result {
                    glib::g_warning!("shellplus", "Screen keyboard {method} failed: {err}");
                }
            },
        );
    }
}

impl Output for CompositorOutput {
    fn commit(&self, text: &str) {
        self.call("CommitText", (text,).to_variant());
    }

    fn keyval(&self, keyval: u32, pressed: bool) {
        self.call("Keyval", (keyval, pressed).to_variant());
    }

    fn hide(&self) {
        if let Some(osk) = self.osk.upgrade() {
            osk.dismissed.set(true);
            osk.sync();
        }
    }

    fn next_source(&self) {
        if let Some(osk) = self.osk.upgrade() {
            next_source(&osk.client);
        }
    }
}

pub fn install(app: &adw::Application, client: &Rc<ShellClient>) {
    let Some(connection) = system::session_bus() else { return };
    load_css();

    let window = crate::overlay::window(app, TITLE);
    window.add_css_class("osk-window");
    let osk = Rc::new_cyclic(|this| Osk {
        this: this.clone(),
        client: client.clone(),
        settings: enabled_settings(),
        window,
        keyboard: RefCell::default(),
        focus: RefCell::default(),
        dismissed: Cell::new(false),
    });
    let output = Rc::new(CompositorOutput { connection: connection.clone(), osk: osk.this.clone() });
    let keyboard = Keyboard::new("osk", output);
    osk.window.set_child(Some(keyboard.widget()));
    osk.keyboard.replace(Some(keyboard));

    if let Some(settings) = &osk.settings {
        let weak = osk.this.clone();
        settings.connect_changed(Some(ENABLED_KEY), move |_, _| {
            if let Some(osk) = weak.upgrade() {
                osk.sync();
            }
        });
    }
    let weak = osk.this.clone();
    client.connect_state(move |state| {
        update_lock_keyboards(state);
        if let Some(osk) = weak.upgrade() {
            osk.sync();
        }
    });

    {
        let weak = osk.this.clone();
        #[allow(deprecated)]
        connection.signal_subscribe(
            Some(BUS_NAME),
            Some(IFACE),
            Some("FocusChanged"),
            Some(PATH),
            None,
            gio::DBusSignalFlags::NONE,
            move |_, _, _, _, _, params| {
                if let Some(osk) = weak.upgrade() {
                    osk.set_focus(Focus::from_variant(&params.child_value(0)));
                }
            },
        );
    }
    let weak = osk.this.clone();
    connection.call(
        Some(BUS_NAME),
        PATH,
        IFACE,
        "GetFocus",
        None,
        Some(glib::VariantTy::new("(a{sv})").unwrap()),
        gio::DBusCallFlags::NONE,
        -1,
        gio::Cancellable::NONE,
        move |result| {
            if let (Ok(reply), Some(osk)) = (result, weak.upgrade()) {
                osk.set_focus(Focus::from_variant(&reply.child_value(0)));
            }
        },
    );

    // The application keeps it alive (callbacks only hold weak references).
    app.connect_shutdown(move |_| drop(osk.keyboard.take()));
}

impl Osk {
    fn set_focus(&self, focus: Focus) {
        // Any focus change brings a dismissed keyboard back.
        self.dismissed.set(false);
        self.focus.replace(focus);
        self.sync();
    }

    fn enabled(&self) -> bool {
        self.settings.as_ref().is_some_and(|s| s.boolean(ENABLED_KEY))
    }

    fn sync(&self) {
        let state = self.client.state();
        let focus = self.focus.borrow().clone();
        let show = self.enabled() && focus.focused && !state.locked && !self.dismissed.get();
        let Some(keyboard) = self.keyboard.borrow().clone() else { return };
        if !show {
            if self.window.is_visible() {
                self.window.set_visible(false);
            }
            return;
        }
        let (width, height) = keyboard_size(&state);
        keyboard.set_size(width, height);
        let (id, multiple) = layout_for(&state, &focus.purpose);
        keyboard.set_layout(id, multiple);
        if !self.window.is_visible() {
            keyboard.reset();
            self.window.present();
        }
    }
}

// --- the lock screen's keyboard -------------------------------------------

/// Types into a GTK entry of this process (the lock screen's password).
struct EntryOutput {
    entry: glib::WeakRef<gtk::Widget>,
    client: Weak<ShellClient>,
    hidden: Rc<Cell<bool>>,
    on_hide: Box<dyn Fn()>,
    repeat: RefCell<Option<glib::SourceId>>,
}

impl EntryOutput {
    fn editable(&self) -> Option<gtk::Editable> {
        self.entry.upgrade().and_then(|w| w.dynamic_cast::<gtk::Editable>().ok())
    }

    fn backspace(&self) {
        let Some(editable) = self.editable() else { return };
        if let Some((start, end)) = editable.selection_bounds() {
            if start != end {
                editable.delete_text(start, end);
                return;
            }
        }
        let position = editable.position();
        if position > 0 {
            editable.delete_text(position - 1, position);
        }
    }

    fn stop_repeat(&self) {
        if let Some(id) = self.repeat.take() {
            id.remove();
        }
    }
}

impl Output for Rc<EntryOutput> {
    fn commit(&self, text: &str) {
        let Some(editable) = self.editable() else { return };
        editable.delete_selection();
        let mut position = editable.position();
        editable.insert_text(text, &mut position);
        editable.set_position(position);
    }

    fn keyval(&self, keyval: u32, pressed: bool) {
        self.stop_repeat();
        if !pressed {
            return;
        }
        let Some(editable) = self.editable() else { return };
        match keyval {
            layout::BACKSPACE => {
                self.backspace();
                // Repeats while held, like a real key.
                let weak = Rc::downgrade(self);
                let id = glib::timeout_add_local_once(REPEAT_DELAY, move || {
                    let Some(output) = weak.upgrade() else { return };
                    let weak = Rc::downgrade(&output);
                    let id = glib::timeout_add_local(REPEAT_INTERVAL, move || match weak.upgrade() {
                        Some(output) => {
                            output.backspace();
                            glib::ControlFlow::Continue
                        }
                        None => glib::ControlFlow::Break,
                    });
                    output.repeat.replace(Some(id));
                });
                self.repeat.replace(Some(id));
            }
            layout::RETURN => {
                editable.emit_by_name::<()>("activate", &[]);
            }
            0xff51 => editable.set_position((editable.position() - 1).max(0)),
            0xff53 => editable.set_position(editable.position() + 1),
            _ => {}
        }
    }

    fn hide(&self) {
        self.hidden.set(true);
        (self.on_hide)();
    }

    fn next_source(&self) {
        if let Some(client) = self.client.upgrade() {
            next_source(&client);
        }
    }
}

/// The lock screen's keyboard: slides up at the bottom of the lock window
/// while screen-keyboard-enabled is on and `entry` has the focus, and types
/// into it (Enter activates it). Put it below the prompt, not over it.
pub fn lock_keyboard(entry: &impl IsA<gtk::Widget>, client: &Rc<ShellClient>) -> gtk::Revealer {
    load_css();
    let entry = entry.as_ref().clone();
    let revealer = gtk::Revealer::builder()
        .name("lock-osk-revealer")
        .transition_type(gtk::RevealerTransitionType::SlideUp)
        .transition_duration(150)
        .valign(gtk::Align::End)
        .can_focus(false)
        .build();
    let hidden = Rc::new(Cell::new(false));
    let has_focus = Rc::new(Cell::new(false));
    let settings = enabled_settings();

    let sync: Rc<dyn Fn()> = {
        let revealer = revealer.downgrade();
        let hidden = hidden.clone();
        let has_focus = has_focus.clone();
        let settings = settings.clone();
        Rc::new(move || {
            let Some(revealer) = revealer.upgrade() else { return };
            let enabled = settings.as_ref().is_some_and(|s| s.boolean(ENABLED_KEY));
            revealer.set_reveal_child(enabled && has_focus.get() && !hidden.get());
        })
    };

    let output = Rc::new(EntryOutput {
        entry: entry.downgrade(),
        client: Rc::downgrade(client),
        hidden: hidden.clone(),
        on_hide: Box::new({
            let sync = sync.clone();
            move || sync()
        }),
        repeat: RefCell::default(),
    });
    let keyboard = Keyboard::new("lock-osk", Rc::new(output));
    update_lock_keyboard(&keyboard, &client.state());
    LOCK_KEYBOARDS.with(|list| list.borrow_mut().push(Rc::downgrade(&keyboard)));
    revealer.set_child(Some(keyboard.widget()));
    // The revealer owns the keyboard from here on.
    unsafe { revealer.set_data("shellplus-osk-keyboard", keyboard) };

    // Focus in the password entry (its text child, really).
    let focus = gtk::EventControllerFocus::new();
    {
        let has_focus = has_focus.clone();
        let hidden = hidden.clone();
        let sync = sync.clone();
        focus.connect_enter(move |_| {
            has_focus.set(true);
            hidden.set(false);
            sync();
        });
    }
    {
        let has_focus = has_focus.clone();
        let sync = sync.clone();
        focus.connect_leave(move |_| {
            has_focus.set(false);
            sync();
        });
    }
    entry.add_controller(focus);
    if let Some(settings) = &settings {
        let sync = sync.clone();
        settings.connect_changed(Some(ENABLED_KEY), move |_, _| sync());
        // The closure holds the settings object; keep it with the revealer.
        unsafe { revealer.set_data("shellplus-osk-settings", settings.clone()) };
    }
    sync();
    revealer
}
