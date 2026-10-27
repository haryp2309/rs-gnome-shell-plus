//! Pop-up banners, one at a time, like GNOME Shell's message tray: they
//! slide in below the top bar (the compositor places the window), time
//! out after a few seconds unless hovered or critical, and queue up
//! behind each other with critical ones first.

use super::card::{Card, Style};
use super::model::Notification;
use super::Notifications;
use crate::overlay;
use gtk::prelude::*;
use gtk::glib;
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_millis(4000);
/// How long a banner stays after the pointer leaves it.
const LEAVE_TIMEOUT: Duration = Duration::from_millis(1000);
/// Matches the fade-out transition in style.css.
const HIDE_TIME: Duration = Duration::from_millis(200);
const NAME: &str = "notification-banner";

struct Showing {
    id: u32,
    critical: bool,
    timeout: Duration,
    card: Rc<Card>,
    /// A fresh window per banner: the compositor places each new window,
    /// and it sizes itself to this banner instead of remembering the last.
    window: gtk::ApplicationWindow,
}

pub struct Banner {
    owner: Weak<Notifications>,
    app: adw::Application,
    /// Ids waiting for their turn, critical ones first.
    queue: RefCell<Vec<(u32, bool)>>,
    showing: RefCell<Option<Showing>>,
    timer: RefCell<Option<glib::SourceId>>,
    hovered: Cell<bool>,
    hiding: Cell<bool>,
    /// Held back while the date menu is open.
    blocked: Cell<bool>,
}

impl Banner {
    pub fn new(app: &adw::Application, owner: Weak<Notifications>) -> Rc<Self> {
        Rc::new(Self {
            owner,
            app: app.clone(),
            queue: RefCell::default(),
            showing: RefCell::default(),
            timer: RefCell::default(),
            hovered: Cell::new(false),
            hiding: Cell::new(false),
            blocked: Cell::new(false),
        })
    }

    /// Shows `notification` once its turn comes, or updates it in place if
    /// it's already showing.
    pub fn push(self: &Rc<Self>, notification: &Notification) {
        if self.showing_id() == Some(notification.id) {
            return self.update(notification);
        }
        {
            let mut queue = self.queue.borrow_mut();
            if !queue.iter().any(|(id, _)| *id == notification.id) {
                let at = if notification.critical {
                    queue.iter().take_while(|(_, critical)| *critical).count()
                } else {
                    queue.len()
                };
                queue.insert(at, (notification.id, notification.critical));
            }
        }
        self.show_next();
    }

    /// Takes the notification off screen and out of the queue.
    pub fn remove(self: &Rc<Self>, id: u32) {
        self.queue.borrow_mut().retain(|(queued, _)| *queued != id);
        if self.showing_id() == Some(id) {
            self.hide();
        }
    }

    /// Fades the current banner out, if any.
    pub fn hide(self: &Rc<Self>) {
        if self.showing.borrow().is_none() || self.hiding.replace(true) {
            return;
        }
        self.stop_timer();
        if let Some(showing) = self.showing.borrow().as_ref() {
            showing.card.button.add_css_class("leaving");
        }
        // The CSS transition fades it; finishing on a timer rather than on
        // the animation means a banner that gets no frames (e.g. covered
        // by a menu) still goes away.
        let weak = Rc::downgrade(self);
        glib::timeout_add_local_once(HIDE_TIME, move || {
            if let Some(banner) = weak.upgrade() {
                banner.hidden();
            }
        });
    }

    /// While the date menu is open, banners stay away like in GNOME Shell.
    /// Its list shows them all, so only critical ones stay queued.
    pub fn set_blocked(self: &Rc<Self>, blocked: bool) {
        self.blocked.set(blocked);
        if !blocked {
            return self.show_next();
        }
        self.queue.borrow_mut().retain(|(_, critical)| *critical);
        if self.showing.borrow().is_some() && !self.hiding.get() {
            self.stop_timer();
            self.hidden();
        }
    }

    fn showing_id(&self) -> Option<u32> {
        self.showing.borrow().as_ref().map(|s| s.id)
    }

    fn update(self: &Rc<Self>, notification: &Notification) {
        if let Some(showing) = self.showing.borrow_mut().as_mut() {
            showing.card.set(notification, Style::WithApp);
            showing.critical = notification.critical;
            showing.timeout = timeout_for(notification);
        }
        self.restart_timer(None);
    }

    fn show_next(self: &Rc<Self>) {
        if self.showing.borrow().is_some() || self.hiding.get() || self.blocked.get() {
            return;
        }
        let Some(owner) = self.owner.upgrade() else { return };
        let next = loop {
            if self.queue.borrow().is_empty() {
                return;
            }
            let (id, _) = self.queue.borrow_mut().remove(0);
            if let Some(notification) = owner.get(id) {
                break notification;
            }
        };

        let card = Card::new(NAME, self.owner.clone());
        card.button.add_css_class(NAME);
        card.set(&next, Style::WithApp);
        self.track_hover(&card.button);
        let window = overlay::window(&self.app, overlay::BANNER_TITLE);
        window.set_child(Some(&card.button));
        window.set_visible(true);
        self.showing.replace(Some(Showing {
            id: next.id,
            critical: next.critical,
            timeout: timeout_for(&next),
            card,
            window,
        }));
        self.hovered.set(false);
        self.restart_timer(None);
    }

    fn hidden(self: &Rc<Self>) {
        if let Some(showing) = self.showing.take() {
            showing.window.destroy();
        }
        self.hovered.set(false);
        self.hiding.set(false);
        self.show_next();
    }

    /// Hovering keeps the banner up; it goes shortly after the pointer leaves.
    fn track_hover(self: &Rc<Self>, card: &gtk::Button) {
        let motion = gtk::EventControllerMotion::new();
        let weak = Rc::downgrade(self);
        motion.connect_enter(glib::clone!(
            #[strong]
            weak,
            move |_, _, _| {
                if let Some(banner) = weak.upgrade() {
                    banner.hovered.set(true);
                    banner.stop_timer();
                }
            }
        ));
        motion.connect_leave(move |_| {
            if let Some(banner) = weak.upgrade() {
                banner.hovered.set(false);
                banner.restart_timer(Some(LEAVE_TIMEOUT));
            }
        });
        card.add_controller(motion);
    }

    fn restart_timer(self: &Rc<Self>, timeout: Option<Duration>) {
        self.stop_timer();
        let (id, timeout) = match self.showing.borrow().as_ref() {
            Some(showing) if !showing.critical && !self.hovered.get() => {
                (showing.id, timeout.unwrap_or(showing.timeout))
            }
            _ => return,
        };
        let weak = Rc::downgrade(self);
        let source = glib::timeout_add_local_once(timeout, move || {
            let Some(banner) = weak.upgrade() else { return };
            banner.timer.take();
            if let Some(owner) = banner.owner.upgrade() {
                owner.banner_expired(id);
            }
            banner.hide();
        });
        self.timer.replace(Some(source));
    }

    fn stop_timer(&self) {
        if let Some(source) = self.timer.take() {
            source.remove();
        }
    }
}

fn timeout_for(notification: &Notification) -> Duration {
    notification
        .timeout_ms
        .map(|ms| Duration::from_millis(ms.into()))
        .unwrap_or(TIMEOUT)
}
