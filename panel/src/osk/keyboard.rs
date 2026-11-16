//! The keyboard widget (GNOME Shell's Keyboard/KeyContainer/Key): one grid
//! of keys per level of a layout, GNOME's shift (tap: one capital letter,
//! double tap: caps lock), the symbols pages, and long-press for accented
//! variants. What the keys do goes to an [`Output`]: the compositor for
//! the overlay keyboard, a GTK entry for the lock screen's.

use super::icons;
use super::layout::{self, Action, Key, Layout, Mode};
use gtk::prelude::*;
use gtk::glib;
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

const LONG_PRESS: Duration = Duration::from_millis(500);
const DOUBLE_TAP: Duration = Duration::from_millis(400);
/// Keys are at most this much wider than tall (number pads stay compact).
const MAX_KEY_ASPECT: f64 = 1.6;
const PADDING: i32 = 8;
const SPACING: i32 = 6;

pub trait Output {
    fn commit(&self, text: &str);
    fn keyval(&self, keyval: u32, pressed: bool);
    fn hide(&self);
    fn next_source(&self);
}

pub struct Keyboard {
    this: Weak<Keyboard>,
    prefix: String,
    output: Rc<dyn Output>,
    root: gtk::Overlay,
    stack: gtk::Stack,
    subkeys: gtk::Box,
    layout: RefCell<Option<Layout>>,
    layout_key: RefCell<(String, bool)>,
    size: Cell<(i32, i32)>,
    level: RefCell<String>,
    latched: Cell<bool>,
    caps: Cell<bool>,
    shifted_at: Cell<Option<Instant>>,
    /// The long-press timer of the key being held.
    long_press: RefCell<Option<glib::SourceId>>,
    /// Shift keys, to restyle for latched/caps lock.
    shift_keys: RefCell<Vec<gtk::Image>>,
}

impl Keyboard {
    /// `prefix` names the key widgets (`<prefix>-key-<level>-<key>`) for
    /// the test hooks.
    pub fn new(prefix: &str, output: Rc<dyn Output>) -> Rc<Self> {
        icons::install();
        let stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::None)
            .hhomogeneous(true)
            .vhomogeneous(true)
            .build();
        let body = gtk::Box::builder()
            .css_classes(["osk-keyboard"])
            .can_focus(false)
            .build();
        body.append(&stack);
        stack.set_hexpand(true);
        let subkeys = gtk::Box::builder()
            .name(format!("{prefix}-subkeys"))
            .css_classes(["keyboard-subkeys"])
            .spacing(SPACING)
            .halign(gtk::Align::Start)
            .valign(gtk::Align::Start)
            .visible(false)
            .build();
        let root = gtk::Overlay::builder().name(prefix).child(&body).can_focus(false).build();
        root.add_overlay(&subkeys);

        let keyboard = Rc::new_cyclic(|this| Self {
            this: this.clone(),
            prefix: prefix.to_owned(),
            output,
            root,
            stack,
            subkeys,
            layout: RefCell::default(),
            layout_key: RefCell::new((String::new(), false)),
            size: Cell::new((0, 0)),
            level: RefCell::new("default".into()),
            latched: Cell::new(false),
            caps: Cell::new(false),
            shifted_at: Cell::new(None),
            long_press: RefCell::default(),
            shift_keys: RefCell::default(),
        });

        // A press anywhere but on the variants closes them.
        let close = gtk::GestureClick::new();
        close.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = keyboard.this.clone();
        close.connect_pressed(move |_, _, x, y| {
            if let Some(kb) = weak.upgrade() {
                if kb.subkeys.is_visible() {
                    let inside = kb.root.pick(x, y, gtk::PickFlags::DEFAULT).is_some_and(|w| {
                        w == *kb.subkeys.upcast_ref::<gtk::Widget>() || w.is_ancestor(&kb.subkeys)
                    });
                    if !inside {
                        kb.subkeys.set_visible(false);
                    }
                }
            }
        });
        keyboard.root.add_controller(close);
        keyboard
    }

    pub fn widget(&self) -> &gtk::Overlay {
        &self.root
    }

    /// The keyboard's whole size; keys are laid out to fill it.
    pub fn set_size(&self, width: i32, height: i32) {
        if self.size.replace((width, height)) == (width, height) {
            return;
        }
        self.root.set_size_request(width, height);
        self.rebuild();
    }

    /// Shows `id` (a layout file name), rebuilding it if it changed.
    pub fn set_layout(&self, id: &str, multiple_sources: bool) {
        let key = (id.to_owned(), multiple_sources);
        if *self.layout_key.borrow() == key && self.layout.borrow().is_some() {
            return;
        }
        self.layout_key.replace(key);
        self.layout.replace(Some(layout::load(id).without_unsupported_keys(multiple_sources)));
        self.rebuild();
    }

    /// Back to the first level (when the keyboard is shown again).
    pub fn reset(&self) {
        self.subkeys.set_visible(false);
        self.cancel_long_press();
        self.caps.set(false);
        self.set_level("default");
    }

    fn rebuild(&self) {
        while let Some(child) = self.stack.first_child() {
            self.stack.remove(&child);
        }
        self.shift_keys.borrow_mut().clear();
        {
            let layout = self.layout.borrow();
            let Some(layout) = layout.as_ref() else { return };
            let (width, height) = self.size.get();
            for level in &layout.levels {
                let grid = self.build_level(level, width, height);
                self.stack.add_named(&grid, Some(&level.name));
            }
        }
        let level = self.level.borrow().clone();
        self.set_level(&level);
    }

    fn build_level(&self, level: &layout::Level, width: i32, height: i32) -> gtk::Grid {
        let row_units = |row: &Vec<Key>| row.iter().map(|k| k.width + k.left_offset).sum::<f64>();
        let units = level.rows.iter().map(row_units).fold(0.0, f64::max).max(1.0);
        let rows = level.rows.len().max(1) as i32;
        let key_height = ((height - 2 * PADDING - (rows - 1) * SPACING) / rows).max(1);
        // Half-unit columns.
        let columns = (units * 2.0).round() as i32;
        let max_width = (units * key_height as f64 * MAX_KEY_ASPECT) as i32;
        let grid_width = (width - 2 * PADDING).min(max_width).max(1);

        let grid = gtk::Grid::builder()
            .name(format!("{}-level-{}", self.prefix, level_id(&level.name)))
            .column_homogeneous(true)
            .row_homogeneous(true)
            .column_spacing(SPACING)
            .row_spacing(SPACING)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .width_request(grid_width)
            .height_request(height - 2 * PADDING)
            .css_classes(["key-container"])
            .build();
        // Rows can be shorter than the widest one; they're centered. A key
        // taller than one row (the number pads' Enter) blocks columns in
        // the rows below.
        let mut blocked: Vec<(i32, i32, i32)> = Vec::new(); // (row, column, span)
        let mut names = std::collections::HashSet::new();
        for (r, row) in level.rows.iter().enumerate() {
            let r = r as i32;
            let mut column = ((units - row_units(row)).max(0.0)).round() as i32;
            if row_units(row) + blocked.iter().filter(|b| b.0 == r).map(|b| b.2 as f64 / 2.0).sum::<f64>() >= units - 0.01 {
                column = 0;
            }
            for key in row {
                column += (key.left_offset * 2.0).round() as i32;
                while let Some(b) = blocked.iter().find(|b| b.0 == r && b.1 == column) {
                    column += b.2;
                }
                let span = ((key.width * 2.0).round() as i32).max(1);
                let height_span = (key.height.round() as i32).max(1);
                for extra in 1..height_span {
                    blocked.push((r + extra, column, span));
                }
                let mut name = format!("{}-key-{}-{}", self.prefix, level_id(&level.name), key_id(key));
                let mut n = 2;
                while !names.insert(name.clone()) {
                    name = format!("{}-key-{}-{}-{n}", self.prefix, level_id(&level.name), key_id(key));
                    n += 1;
                }
                let widget = self.build_key(level, key, &name);
                grid.attach(&widget, column, r, span, height_span);
                column += span;
            }
        }
        let _ = columns;
        grid
    }

    fn build_key(&self, level: &layout::Level, key: &Key, name: &str) -> gtk::Widget {
        let widget = gtk::Box::builder()
            .name(name)
            .css_classes(["keyboard-key"])
            .can_focus(false)
            .focus_on_click(false)
            .hexpand(true)
            .vexpand(true)
            .accessible_role(gtk::AccessibleRole::Button)
            .build();
        let (label, icon) = key_face(key);
        if let Some(icon) = icon {
            let image = gtk::Image::builder()
                .icon_name(icon)
                .pixel_size(20)
                .hexpand(true)
                .halign(gtk::Align::Center)
                .valign(gtk::Align::Center)
                .build();
            widget.append(&image);
            if matches!(key.action, Action::LevelSwitch(_)) && icon == icons::SHIFT {
                image.set_widget_name(&format!("{name}-icon"));
                self.shift_keys.borrow_mut().push(image);
                // Shift keys glow while the shift level is up.
                if level.name.contains("shift") {
                    widget.add_css_class("shift-key-uppercase");
                }
            }
        } else {
            let text = label.unwrap_or_default();
            widget.append(
                &gtk::Label::builder()
                    .label(&text)
                    .hexpand(true)
                    .halign(gtk::Align::Center)
                    .valign(gtk::Align::Center)
                    .build(),
            );
        }
        widget.update_property(&[gtk::accessible::Property::Label(&accessible_label(key))]);
        match &key.action {
            Action::Commit(_) if !key.variants.is_empty() => widget.add_css_class("has-variants"),
            Action::Commit(_) => {}
            Action::Keyval(layout::RETURN) => {
                widget.add_css_class("default-key");
                widget.add_css_class("enter-key");
            }
            _ => widget.add_css_class("default-key"),
        }

        let gesture = gtk::GestureClick::new();
        gesture.set_button(gtk::gdk::BUTTON_PRIMARY);
        let weak = self.this.clone();
        let key_pressed = key.clone();
        let pressed_widget = widget.clone();
        gesture.connect_pressed(move |_, _, _, _| {
            if let Some(kb) = weak.upgrade() {
                pressed_widget.add_css_class("active");
                kb.on_press(&key_pressed, &pressed_widget);
            }
        });
        let weak = self.this.clone();
        let key_released = key.clone();
        let released_widget = widget.clone();
        gesture.connect_released(move |gesture, _, _, _| {
            gesture.set_state(gtk::EventSequenceState::Claimed);
            if let Some(kb) = weak.upgrade() {
                released_widget.remove_css_class("active");
                kb.on_release(&key_released);
            }
        });
        let weak = self.this.clone();
        let key_cancelled = key.clone();
        let cancelled_widget = widget.clone();
        gesture.connect_cancel(move |_, _| {
            if let Some(kb) = weak.upgrade() {
                cancelled_widget.remove_css_class("active");
                kb.cancel_long_press();
                if let Some(keyval) = keyval_of(&key_cancelled.action) {
                    kb.output.keyval(keyval, false);
                }
            }
        });
        widget.add_controller(gesture);
        widget.upcast()
    }

    fn on_press(&self, key: &Key, widget: &gtk::Box) {
        self.subkeys.set_visible(false);
        self.cancel_long_press();
        // Real keys go down now and up on release, so the client repeats
        // them while held (BackSpace).
        if let Some(keyval) = keyval_of(&key.action) {
            self.output.keyval(keyval, true);
            return;
        }
        if !key.variants.is_empty() {
            let weak = self.this.clone();
            let key = key.clone();
            let widget = widget.clone();
            let id = glib::timeout_add_local_once(LONG_PRESS, move || {
                if let Some(kb) = weak.upgrade() {
                    kb.long_press.take();
                    kb.show_variants(&key, &widget);
                }
            });
            self.long_press.replace(Some(id));
        }
    }

    fn on_release(&self, key: &Key) {
        let pending = self.long_press.borrow_mut().take();
        match &key.action {
            Action::Commit(text) => {
                // After a long press the variants stay up to pick from.
                if let Some(id) = pending {
                    id.remove();
                    self.commit(text);
                } else if key.variants.is_empty() {
                    self.commit(text);
                }
            }
            Action::Keyval(_) | Action::Delete => {
                if let Some(keyval) = keyval_of(&key.action) {
                    self.output.keyval(keyval, false);
                }
            }
            Action::LevelSwitch(level) => self.switch_level(level),
            Action::Hide => self.output.hide(),
            Action::Language => self.output.next_source(),
            Action::Emoji => {}
        }
    }

    fn cancel_long_press(&self) {
        if let Some(id) = self.long_press.take() {
            id.remove();
        }
    }

    fn commit(&self, text: &str) {
        self.output.commit(text);
        // A latched shift lasts one key.
        if self.latched.get() && !self.caps.get() {
            self.set_level("default");
        }
    }

    /// GNOME's shift: a tap gives one capital, a second tap right after
    /// locks caps, a tap while locked goes back to lower case.
    fn switch_level(&self, target: &str) {
        let current = self.level.borrow().clone();
        let to_default_from_shift = target == "default" && current == "shift";
        if to_default_from_shift && self.latched.get() && !self.caps.get() &&
            self.shifted_at.get().is_some_and(|t| t.elapsed() < DOUBLE_TAP)
        {
            self.caps.set(true);
            self.latched.set(false);
            self.update_shift_keys();
            return;
        }
        self.caps.set(false);
        if target == "shift" {
            self.shifted_at.set(Some(Instant::now()));
        }
        self.set_level(target);
    }

    fn set_level(&self, name: &str) {
        let mode = self
            .layout
            .borrow()
            .as_ref()
            .and_then(|l| l.levels.iter().find(|l| l.name == name).map(|l| l.mode));
        let Some(mode) = mode else {
            if name != "default" {
                self.set_level("default");
            }
            return;
        };
        self.level.replace(name.to_owned());
        self.latched.set(mode == Mode::Latched && !self.caps.get());
        self.stack.set_visible_child_name(name);
        self.update_shift_keys();
    }

    fn update_shift_keys(&self) {
        let icon = if self.caps.get() { icons::CAPS_LOCK } else { icons::SHIFT };
        for image in self.shift_keys.borrow().iter() {
            image.set_icon_name(Some(icon));
            if let Some(parent) = image.parent() {
                if self.caps.get() {
                    parent.add_css_class("shift-key-locked");
                } else {
                    parent.remove_css_class("shift-key-locked");
                }
            }
        }
    }

    /// The accented variants above the key; picking one types it.
    fn show_variants(&self, key: &Key, widget: &gtk::Box) {
        while let Some(child) = self.subkeys.first_child() {
            self.subkeys.remove(&child);
        }
        let Some(bounds) = widget.compute_bounds(&self.root) else { return };
        let (w, h) = (bounds.width() as i32, bounds.height() as i32);
        for (i, variant) in key.variants.iter().enumerate() {
            let text = if self.caps.get() || self.latched.get() { variant.to_uppercase() } else { variant.clone() };
            let item = gtk::Box::builder()
                .name(format!("{}-variant-{i}", self.prefix))
                .css_classes(["keyboard-key"])
                .width_request(w)
                .height_request(h)
                .can_focus(false)
                .accessible_role(gtk::AccessibleRole::Button)
                .build();
            item.append(&gtk::Label::builder().label(&text).hexpand(true).build());
            let click = gtk::GestureClick::new();
            let weak = self.this.clone();
            click.connect_released(move |gesture, _, _, _| {
                gesture.set_state(gtk::EventSequenceState::Claimed);
                if let Some(kb) = weak.upgrade() {
                    kb.subkeys.set_visible(false);
                    kb.commit(&text);
                }
            });
            item.add_controller(click);
            self.subkeys.append(&item);
        }
        let n = key.variants.len() as i32;
        let total = n * w + (n - 1) * SPACING + 2 * PADDING;
        let (root_w, _) = self.size.get();
        let x = (bounds.x() as i32 + w / 2 - total / 2).clamp(0, (root_w - total).max(0));
        let y = (bounds.y() as i32 - h - 2 * PADDING - SPACING).max(0);
        self.subkeys.set_margin_start(x);
        self.subkeys.set_margin_top(y);
        self.subkeys.set_visible(true);
    }
}

fn keyval_of(action: &Action) -> Option<u32> {
    match action {
        Action::Keyval(keyval) => Some(*keyval),
        Action::Delete => Some(layout::BACKSPACE),
        _ => None,
    }
}

/// What a key shows: our icons for GNOME's osk-* ones, else its label.
fn key_face(key: &Key) -> (Option<String>, Option<&'static str>) {
    let icon = match (&key.action, key.icon.as_deref()) {
        (Action::Delete, _) => Some(icons::DELETE),
        (Action::Keyval(layout::RETURN), _) => Some(icons::ENTER),
        (Action::Hide, _) => Some(icons::HIDE),
        (Action::Language, _) => Some(icons::LAYOUT),
        (Action::LevelSwitch(_), Some("osk-shift-symbolic")) => Some(icons::SHIFT),
        _ => None,
    };
    let label = key.label.clone().or_else(|| match &key.action {
        Action::Commit(text) => Some(text.clone()),
        _ => None,
    });
    (label, icon)
}

fn accessible_label(key: &Key) -> String {
    match &key.action {
        Action::Commit(text) if text == " " => "Space".into(),
        Action::Commit(text) => text.clone(),
        Action::Keyval(layout::RETURN) => "Enter".into(),
        Action::Keyval(_) => key.label.clone().unwrap_or_default(),
        Action::Delete => "Backspace".into(),
        Action::LevelSwitch(level) if level == "shift" || level == "default" && key.label.is_none() => "Shift".into(),
        Action::LevelSwitch(_) => key.label.clone().unwrap_or_default(),
        Action::Hide => "Hide keyboard".into(),
        Action::Language => "Keyboard layout".into(),
        Action::Emoji => "Emoji".into(),
    }
}

/// Widget names: `default`, `shift`, `opt`, `opt-shift`.
fn level_id(level: &str) -> String {
    level.replace('+', "-")
}

fn key_id(key: &Key) -> String {
    match &key.action {
        Action::Commit(text) if text == " " => "space".into(),
        Action::Commit(text) => text.clone(),
        Action::Keyval(layout::RETURN) => "enter".into(),
        Action::Keyval(keyval) => format!("{keyval:#x}"),
        Action::Delete => "delete".into(),
        Action::LevelSwitch(level) => format!("to-{}", level_id(level)),
        Action::Hide => "hide".into(),
        Action::Language => "language".into(),
        Action::Emoji => "emoji".into(),
    }
}
