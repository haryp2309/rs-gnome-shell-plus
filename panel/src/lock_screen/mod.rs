//! GNOME Shell's lock screen (GNOME 46–50 look): the blurred wallpaper with
//! the curtain (big clock, date, notification counts, a hint), and on any
//! key, click or swipe up the unlock prompt (avatar, name, password).
//!
//! The compositor owns the lock (compositor/src/shellplus-lock.c). It
//! publishes `locked` in its state; we then show a window titled
//! "Lock Screen", which it puts alone above a black shield over every
//! monitor. If this process dies, the screen stays black and the
//! compositor restarts us, and we show the lock screen again from the
//! state. Once the user's password checks out (auth.rs: GDM's
//! reauthentication channel, else PAM), we ask the compositor to Unlock;
//! it only accepts that from this process.

mod auth;
mod pam;
mod status_bar;

use crate::ipc::ShellClient;
use crate::notifications::Notifications;
use crate::quick_settings::Indicators;
use crate::system;
use adw::prelude::*;
use auth::{Event, Verifier};
use status_bar::StatusBar;
use gtk::{gdk, gio, glib};
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::sync::Once;
use std::time::Duration;

/// The compositor recognizes the lock window by this title.
pub const TITLE: &str = "Lock Screen";

const BUS_NAME: &str = "org.gnome.ShellPlus";
const PATH: &str = "/org/gnome/ShellPlus";
const LOCK_IFACE: &str = "org.gnome.ShellPlus.Lock";

const ACCOUNTS: &str = "org.freedesktop.Accounts";
const ACCOUNTS_PATH: &str = "/org/freedesktop/Accounts";
const ACCOUNTS_USER: &str = "org.freedesktop.Accounts.User";

/// The prompt drops back to the curtain after this long without input.
const PROMPT_IDLE: Duration = Duration::from_secs(30);
const FADE_OUT_MS: u32 = 250;
const SHAKE_MS: u32 = 400;
const AVATAR_SIZE: i32 = 128;
const FAILED: &str = "Sorry, that didn’t work. Please try again.";

struct LockScreen {
    app: adw::Application,
    client: Rc<ShellClient>,
    notifications: Rc<Notifications>,
    indicators: Rc<Indicators>,
    on_lock: Box<dyn Fn()>,
    ui: RefCell<Option<Rc<Ui>>>,
}

pub fn install(
    app: &adw::Application,
    client: &Rc<ShellClient>,
    notifications: &Rc<Notifications>,
    indicators: &Rc<Indicators>,
    on_lock: impl Fn() + 'static,
) {
    load_css();
    let lock = Rc::new(LockScreen {
        app: app.clone(),
        client: client.clone(),
        notifications: notifications.clone(),
        indicators: indicators.clone(),
        on_lock: Box::new(on_lock),
        ui: RefCell::default(),
    });
    let weak = Rc::downgrade(&lock);
    client.connect_state(move |state| {
        if let Some(lock) = weak.upgrade() {
            lock.set_locked(state.locked);
            if let Some(ui) = lock.ui.borrow().as_ref() {
                ui.status_bar.update(state);
            }
        }
    });
    // The application keeps the lock screen alive (state callbacks only
    // hold a weak reference).
    app.connect_shutdown(move |_| drop(lock.ui.take()));
}

impl LockScreen {
    fn set_locked(self: &Rc<Self>, locked: bool) {
        let shown = self.ui.borrow().as_ref().is_some_and(|ui| !ui.closing.get());
        if locked && !shown {
            (self.on_lock)();
            close_popovers();
            let ui = Ui::new(self);
            self.ui.replace(Some(ui.clone()));
            ui.present();
        } else if !locked {
            if let Some(ui) = self.ui.take() {
                ui.close();
            }
        }
    }
}

/// Our own menus would keep a popup grab (and the keyboard) while hidden
/// under the shield.
fn close_popovers() {
    fn visit(widget: &gtk::Widget) {
        if let Some(popover) = widget.downcast_ref::<gtk::Popover>() {
            popover.popdown();
        }
        let mut child = widget.first_child();
        while let Some(c) = child {
            visit(&c);
            child = c.next_sibling();
        }
    }
    for window in gtk::Window::list_toplevels() {
        visit(&window);
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Page {
    Curtain,
    Prompt,
}

struct Ui {
    this: Weak<Ui>,
    window: gtk::ApplicationWindow,
    root: gtk::Overlay,
    stack: gtk::Stack,
    time: gtk::Label,
    date: gtk::Label,
    notification_list: gtk::Box,
    prompt_box: gtk::Box,
    password: gtk::PasswordEntry,
    unlock: gtk::Button,
    message: gtk::Label,
    status_bar: StatusBar,
    user: String,

    verifier: RefCell<Option<Rc<Verifier>>>,
    /// The verifier waits for an answer.
    querying: Cell<bool>,
    /// Typed and confirmed before the verifier asked.
    pending: RefCell<Option<String>>,
    /// A problem message is showing for this attempt.
    problem: Cell<bool>,
    /// The user answered a query in this attempt.
    answered: Cell<bool>,
    idle: RefCell<Option<glib::SourceId>>,
    closing: Cell<bool>,
    shake: RefCell<Option<adw::TimedAnimation>>,
}

impl Ui {
    fn new(lock: &Rc<LockScreen>) -> Rc<Self> {
        let user = glib::user_name().to_string_lossy().into_owned();

        // --- the curtain -------------------------------------------------
        let time = gtk::Label::builder().name("lock-clock-time").css_classes(["lock-clock-time"]).build();
        let date = gtk::Label::builder().name("lock-clock-date").css_classes(["lock-clock-date"]).build();
        let notification_list = gtk::Box::builder()
            .name("lock-notifications")
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .halign(gtk::Align::Center)
            .css_classes(["lock-notifications"])
            .build();
        let clock = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .halign(gtk::Align::Center)
            .css_classes(["lock-clock"])
            .build();
        clock.append(&time);
        clock.append(&date);
        let hint = gtk::Label::builder()
            .name("lock-hint")
            .label("Click or press a key to unlock")
            .css_classes(["lock-hint"])
            .valign(gtk::Align::End)
            .vexpand(true)
            .build();
        let curtain = gtk::Box::builder()
            .name("lock-curtain")
            .orientation(gtk::Orientation::Vertical)
            .spacing(36)
            .css_classes(["lock-curtain"])
            .build();
        curtain.append(&clock);
        curtain.append(&notification_list);
        curtain.append(&hint);

        // --- the prompt --------------------------------------------------
        let avatar = adw::Avatar::builder().name("lock-avatar").size(AVATAR_SIZE).show_initials(true).build();
        let name = gtk::Label::builder()
            .name("lock-user-name")
            .css_classes(["lock-user-name"])
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        let (real_name, icon) = user_info(&user);
        avatar.set_text(Some(&real_name));
        avatar.set_custom_image(icon.and_then(|p| gdk::Texture::from_filename(p).ok()).as_ref());
        name.set_label(&real_name);

        let password = gtk::PasswordEntry::builder()
            .name("lock-password")
            .placeholder_text("Password")
            .show_peek_icon(true)
            .hexpand(true)
            .build();
        // No context menu: its popover would sit invisible (under the
        // shield) and hold the keyboard.
        let no_menu = gtk::GestureClick::builder().button(gdk::BUTTON_SECONDARY).build();
        no_menu.set_propagation_phase(gtk::PropagationPhase::Capture);
        no_menu.connect_pressed(|gesture, _, _, _| {
            gesture.set_state(gtk::EventSequenceState::Claimed);
        });
        password.add_controller(no_menu);
        let unlock = gtk::Button::builder()
            .name("lock-unlock")
            .label("Unlock")
            .css_classes(["suggested-action", "lock-unlock"])
            .build();
        let entry_row = gtk::Box::builder().spacing(8).css_classes(["lock-entry-row"]).build();
        entry_row.append(&password);
        entry_row.append(&unlock);
        let message = gtk::Label::builder()
            .name("lock-message")
            .css_classes(["lock-message"])
            .wrap(true)
            .justify(gtk::Justification::Center)
            .max_width_chars(40)
            .build();
        let prompt_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(16)
            .width_request(340)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .vexpand(true)
            .margin_start(24)
            .margin_end(24)
            .build();
        prompt_box.append(&avatar);
        prompt_box.append(&name);
        prompt_box.append(&entry_row);
        prompt_box.append(&message);
        let prompt = gtk::Box::builder()
            .name("lock-prompt")
            .orientation(gtk::Orientation::Vertical)
            .css_classes(["lock-prompt"])
            .build();
        prompt.append(&prompt_box);

        let stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .transition_duration(300)
            .build();
        stack.add_named(&curtain, Some("curtain"));
        stack.add_named(&prompt, Some("prompt"));

        // --- background --------------------------------------------------
        let wallpaper = gtk::Picture::builder()
            .name("lock-wallpaper")
            .content_fit(gtk::ContentFit::Cover)
            .can_shrink(true)
            .build();
        let dim = gtk::Box::builder().css_classes(["lock-dim"]).build();
        let root = gtk::Overlay::builder().child(&wallpaper).build();
        root.add_overlay(&dim);
        root.add_overlay(&stack);
        // GNOME's panel stays up over both the curtain and the prompt.
        let status_bar = StatusBar::new(&lock.indicators);
        status_bar.update(&lock.client.state());
        root.add_overlay(&status_bar.bar);
        root.add_overlay(&status_bar.menu);

        let window = gtk::ApplicationWindow::builder()
            .application(&lock.app)
            .name("lock-screen")
            .title(TITLE)
            .decorated(false)
            .css_classes(["lock-screen"])
            .child(&root)
            .build();

        let monitor = lock.client.state().monitor;
        fit(&root, monitor.map(|m| (m.2, m.3)).or_else(first_monitor_size));
        window.connect_realize(glib::clone!(
            #[weak]
            root,
            move |window| {
                if let Some(surface) = window.surface() {
                    surface.connect_enter_monitor(glib::clone!(
                        #[weak]
                        root,
                        move |_, monitor| {
                            let g = monitor.geometry();
                            fit(&root, Some((g.width(), g.height())));
                        }
                    ));
                }
            }
        ));
        // Blurred and dimmed like GNOME's (rendered at a quarter size).
        if let Some((width, height)) = monitor.map(|m| (m.2, m.3)).or_else(first_monitor_size) {
            let request = crate::wallpaper::Request {
                panel_height: height,
                monitor_width: width,
                monitor_height: height,
                scale: window.scale_factor().max(1),
            };
            crate::wallpaper::render(request, glib::clone!(
                #[weak]
                wallpaper,
                move |texture| wallpaper.set_paintable(texture.as_ref())
            ));
        }

        let ui = Rc::new_cyclic(|this| Self {
            this: this.clone(),
            window,
            root,
            stack,
            time,
            date,
            notification_list,
            prompt_box,
            password,
            unlock,
            message,
            status_bar,
            user,
            verifier: RefCell::default(),
            querying: Cell::new(false),
            pending: RefCell::default(),
            problem: Cell::new(false),
            answered: Cell::new(false),
            idle: RefCell::default(),
            closing: Cell::new(false),
            shake: RefCell::default(),
        });
        ui.connect_input();
        ui.update_clock();
        ui.update_notifications(&lock.notifications);

        let weak = ui.this.clone();
        glib::timeout_add_seconds_local(1, move || match weak.upgrade() {
            Some(ui) if !ui.closing.get() => {
                ui.update_clock();
                glib::ControlFlow::Continue
            }
            _ => glib::ControlFlow::Break,
        });
        let weak = ui.this.clone();
        let notifications = Rc::downgrade(&lock.notifications);
        lock.notifications.connect_changed(move || {
            if let (Some(ui), Some(notifications)) = (weak.upgrade(), notifications.upgrade()) {
                ui.update_notifications(&notifications);
            }
        });
        crate::debug::register("lock-show-prompt", {
            let weak = ui.this.clone();
            move |_| {
                if let Some(ui) = weak.upgrade() {
                    ui.show_prompt();
                }
            }
        });
        ui
    }

    fn connect_input(&self) {
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = self.this.clone();
        keys.connect_key_pressed(move |_, key, _, _| {
            let Some(ui) = weak.upgrade() else { return glib::Propagation::Proceed };
            ui.on_key(key)
        });
        self.window.add_controller(keys);

        let click = gtk::GestureClick::new();
        let weak = self.this.clone();
        click.connect_pressed(move |_, _, x, y| {
            if let Some(ui) = weak.upgrade() {
                ui.touch();
                // Clicks on the accessibility menu (or closing it) leave the
                // curtain down.
                if !ui.status_bar.handle_click(&ui.window, x, y) && ui.page() == Page::Curtain {
                    ui.show_prompt();
                }
            }
        });
        self.window.add_controller(click);

        // Swipe the curtain up.
        let drag = gtk::GestureDrag::new();
        let weak = self.this.clone();
        drag.connect_drag_end(move |_, _, dy| {
            if let Some(ui) = weak.upgrade() {
                if dy < -60.0 && ui.page() == Page::Curtain {
                    ui.show_prompt();
                }
            }
        });
        self.window.add_controller(drag);

        let motion = gtk::EventControllerMotion::new();
        let weak = self.this.clone();
        motion.connect_motion(move |_, _, _| {
            if let Some(ui) = weak.upgrade() {
                ui.touch();
            }
        });
        self.window.add_controller(motion);

        let weak = self.this.clone();
        let submit = move || {
            if let Some(ui) = weak.upgrade() {
                ui.submit();
            }
        };
        self.password.connect_activate({
            let submit = submit.clone();
            move |_| submit()
        });
        self.unlock.connect_clicked(move |_| submit());
        self.password.connect_changed({
            let weak = self.this.clone();
            move |_| {
                if let Some(ui) = weak.upgrade() {
                    ui.touch();
                }
            }
        });

        // Nothing closes the lock screen but unlocking.
        self.window.connect_close_request(|_| glib::Propagation::Stop);
    }

    fn present(&self) {
        self.window.present();
        self.stack.set_visible_child_name("curtain");
        self.root.grab_focus();
    }

    fn page(&self) -> Page {
        if self.stack.visible_child_name().as_deref() == Some("prompt") {
            Page::Prompt
        } else {
            Page::Curtain
        }
    }

    fn on_key(&self, key: gdk::Key) -> glib::Propagation {
        self.touch();
        if key == gdk::Key::Escape && self.status_bar.close_menu() {
            return glib::Propagation::Stop;
        }
        match self.page() {
            Page::Curtain => {
                // Modifiers alone (e.g. Super, Alt+Tab attempts) leave the
                // curtain down.
                if key == gdk::Key::Escape || is_modifier(key) {
                    return glib::Propagation::Stop;
                }
                self.show_prompt();
                // What was typed on the curtain is the start of the password.
                if let Some(c) = key.to_unicode().filter(|c| !c.is_control()) {
                    let text = format!("{}{c}", self.password.text());
                    self.password.set_text(&text);
                    self.password.set_position(-1);
                }
                glib::Propagation::Stop
            }
            Page::Prompt => {
                if key == gdk::Key::Escape {
                    self.show_curtain();
                    return glib::Propagation::Stop;
                }
                glib::Propagation::Proceed
            }
        }
    }

    /// Restarts the prompt's idle timeout.
    fn touch(&self) {
        if let Some(id) = self.idle.take() {
            id.remove();
        }
        if self.page() != Page::Prompt || self.closing.get() {
            return;
        }
        let weak = self.this.clone();
        let id = glib::timeout_add_local_once(PROMPT_IDLE, move || {
            if let Some(ui) = weak.upgrade() {
                ui.idle.take();
                // Not while a password is being checked.
                if ui.page() == Page::Prompt && ui.password.is_sensitive() {
                    ui.show_curtain();
                }
            }
        });
        self.idle.replace(Some(id));
    }

    fn show_prompt(&self) {
        if self.closing.get() {
            return;
        }
        if self.page() != Page::Prompt {
            self.stack.set_visible_child_name("prompt");
        }
        self.password.grab_focus();
        if self.verifier.borrow().is_none() {
            self.start_verification();
        }
        self.touch();
    }

    fn show_curtain(&self) {
        self.stop_verification();
        self.password.set_text("");
        self.pending.take();
        self.set_message("", false);
        self.set_busy(false);
        self.stack.set_visible_child_name("curtain");
        self.root.grab_focus();
        self.touch();
    }

    fn start_verification(&self) {
        self.querying.set(false);
        self.problem.set(false);
        self.answered.set(false);
        let weak = self.this.clone();
        let verifier = Verifier::start(&self.user, move |event| {
            if let Some(ui) = weak.upgrade() {
                ui.on_event(event);
            }
        });
        self.verifier.replace(Some(verifier));
    }

    fn stop_verification(&self) {
        if let Some(verifier) = self.verifier.take() {
            verifier.cancel();
        }
        self.querying.set(false);
    }

    fn submit(&self) {
        if self.page() != Page::Prompt || !self.password.is_sensitive() {
            return;
        }
        let text = self.password.text().to_string();
        if text.is_empty() {
            return;
        }
        self.set_busy(true);
        self.set_message("", false);
        if self.querying.replace(false) {
            if let Some(verifier) = self.verifier.borrow().as_ref() {
                self.answered.set(true);
                verifier.answer(&text);
            }
        } else {
            self.pending.replace(Some(text));
            if self.verifier.borrow().is_none() {
                self.start_verification();
            }
        }
    }

    fn on_event(&self, event: Event) {
        match event {
            Event::Info(text) => self.set_message(&text, false),
            Event::Problem(text) => {
                self.problem.set(!text.is_empty());
                self.set_message(&text, true);
            }
            Event::Query { prompt, secret } => {
                let hint = prompt.trim().trim_end_matches(':').trim();
                // A password prompt keeps GNOME's "Password" hint.
                if !secret || !hint.eq_ignore_ascii_case("password") {
                    if !hint.is_empty() {
                        self.password.set_placeholder_text(Some(hint));
                    }
                }
                if let Some(answer) = self.pending.take() {
                    if let Some(verifier) = self.verifier.borrow().as_ref() {
                        self.answered.set(true);
                        verifier.answer(&answer);
                    }
                } else {
                    self.querying.set(true);
                    self.set_busy(false);
                    if self.page() == Page::Prompt {
                        self.password.grab_focus();
                    }
                }
            }
            Event::Complete => {
                self.verifier.take();
                self.password.set_text("");
                self.request_unlock();
            }
            Event::Failed => {
                self.verifier.take();
                if !self.problem.get() {
                    self.set_message(FAILED, true);
                }
                self.password.set_text("");
                self.pending.take();
                self.set_busy(false);
                self.shake();
                // Try again right away, like GNOME Shell. A verifier that
                // failed by itself (e.g. GDM went away) is only restarted
                // when the user submits again, not in a loop.
                if self.page() == Page::Prompt && self.answered.get() {
                    self.start_verification();
                    self.password.grab_focus();
                }
            }
        }
    }

    fn request_unlock(&self) {
        let Some(conn) = system::session_bus() else { return };
        let weak = self.this.clone();
        conn.call(
            Some(BUS_NAME),
            PATH,
            LOCK_IFACE,
            "Unlock",
            None,
            None,
            gio::DBusCallFlags::NONE,
            -1,
            gio::Cancellable::NONE,
            move |result| {
                if let Err(err) = result {
                    glib::g_warning!("shellplus", "Unlock failed: {err}");
                    if let Some(ui) = weak.upgrade() {
                        ui.set_busy(false);
                        ui.set_message(FAILED, true);
                        ui.start_verification();
                    }
                }
            },
        );
    }

    fn set_busy(&self, busy: bool) {
        self.password.set_sensitive(!busy);
        self.unlock.set_sensitive(!busy);
    }

    fn set_message(&self, text: &str, error: bool) {
        self.message.set_label(text);
        if error {
            self.message.add_css_class("error");
        } else {
            self.message.remove_css_class("error");
        }
    }

    fn shake(&self) {
        let prompt = self.prompt_box.clone();
        let target = adw::CallbackAnimationTarget::new(move |v| {
            let offset = (v * std::f64::consts::PI * 6.0).sin() * 14.0 * (1.0 - v);
            prompt.set_margin_start((24.0 + offset).round() as i32);
            prompt.set_margin_end((24.0 - offset).round() as i32);
        });
        let animation = adw::TimedAnimation::new(&self.prompt_box, 0.0, 1.0, SHAKE_MS, target);
        animation.set_easing(adw::Easing::Linear);
        animation.play();
        self.shake.replace(Some(animation));
    }

    fn update_clock(&self) {
        let Ok(now) = glib::DateTime::now_local() else { return };
        let twelve_hour = system::settings("org.gnome.desktop.interface")
            .is_some_and(|s| s.string("clock-format") == "12h");
        let time = now.format(if twelve_hour { "%-l:%M" } else { "%H:%M" }).map(|s| s.to_string());
        let date = now.format("%A %B %-d").map(|s| s.to_string());
        if let Ok(time) = time {
            if self.time.label() != time.as_str() {
                self.time.set_label(&time);
            }
        }
        if let Ok(date) = date {
            if self.date.label() != date.as_str() {
                self.date.set_label(&date);
            }
        }
    }

    /// Counts per application, no contents (GNOME's default for apps that
    /// don't allow details on the lock screen).
    fn update_notifications(&self, notifications: &Notifications) {
        while let Some(child) = self.notification_list.first_child() {
            self.notification_list.remove(&child);
        }
        let summary = notifications.summary();
        for (name, icon, count) in summary.iter().take(5) {
            let row = gtk::Box::builder().spacing(12).css_classes(["lock-notification"]).build();
            let image = match icon {
                Some(icon) => gtk::Image::from_gicon(icon),
                None => gtk::Image::from_icon_name("preferences-system-notifications-symbolic"),
            };
            image.set_pixel_size(32);
            row.append(&image);
            let text = gtk::Label::builder().label(name.as_str()).xalign(0.0).hexpand(true).build();
            row.append(&text);
            let badge = gtk::Label::builder()
                .label(count.to_string())
                .css_classes(["lock-notification-count"])
                .valign(gtk::Align::Center)
                .build();
            row.append(&badge);
            self.notification_list.append(&row);
        }
        self.notification_list.set_visible(!summary.is_empty());
    }

    /// Unlocked: fade out over the desktop, then go away.
    fn close(&self) {
        if self.closing.replace(true) {
            return;
        }
        self.stop_verification();
        self.pending.take();
        self.password.set_text("");
        if let Some(id) = self.idle.take() {
            id.remove();
        }
        let window = self.window.clone();
        let fade = adw::TimedAnimation::new(
            &self.root,
            1.0,
            0.0,
            FADE_OUT_MS,
            adw::PropertyAnimationTarget::new(&self.root, "opacity"),
        );
        fade.connect_done(move |_| window.destroy());
        fade.play();
    }
}

fn is_modifier(key: gdk::Key) -> bool {
    use gdk::Key;
    [
        Key::Shift_L, Key::Shift_R, Key::Control_L, Key::Control_R, Key::Alt_L, Key::Alt_R,
        Key::Super_L, Key::Super_R, Key::Meta_L, Key::Meta_R, Key::Hyper_L, Key::Hyper_R,
        Key::Caps_Lock, Key::Num_Lock, Key::ISO_Level3_Shift, Key::ISO_Level5_Shift,
        Key::ISO_Next_Group, Key::ISO_Prev_Group, Key::Mode_switch,
    ]
    .contains(&key)
}

/// Real name and avatar from AccountsService, else the passwd entry.
fn user_info(user: &str) -> (String, Option<String>) {
    let fallback = glib::real_name().to_string_lossy().into_owned();
    let fallback = if fallback.is_empty() || fallback == "Unknown" { user.to_owned() } else { fallback };
    let account = system::system_bus().and_then(|conn| {
        let reply = system::call_sync(&conn, ACCOUNTS, ACCOUNTS_PATH, ACCOUNTS, "FindUserByName", Some(&(user,).to_variant()))?;
        let path = reply.child_value(0).str()?.to_owned();
        system::get_all(&conn, ACCOUNTS, &path, ACCOUNTS_USER)
    });
    let real_name = account
        .as_ref()
        .map(|a| a.string("RealName"))
        .filter(|n| !n.is_empty())
        .unwrap_or(fallback);
    let icon = account
        .as_ref()
        .map(|a| a.string("IconFile"))
        .filter(|p| !p.is_empty() && std::path::Path::new(p).is_file());
    (real_name, icon)
}

fn first_monitor_size() -> Option<(i32, i32)> {
    let monitor: gdk::Monitor = gdk::Display::default()?.monitors().item(0)?.downcast().ok()?;
    let g = monitor.geometry();
    Some((g.width(), g.height()))
}

/// The window must be as big as the monitor (the compositor stretches it,
/// but GTK would otherwise shrink it to the work area on relayout).
fn fit(root: &gtk::Overlay, size: Option<(i32, i32)>) {
    if let Some((width, height)) = size {
        root.set_size_request(width, height);
    }
}

fn load_css() {
    static LOADED: Once = Once::new();
    LOADED.call_once(|| {
        let Some(display) = gdk::Display::default() else { return };
        let provider = gtk::CssProvider::new();
        provider.load_from_string(include_str!("lock_screen.css"));
        gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
    });
}
