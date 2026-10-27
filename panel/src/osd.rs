//! The volume/brightness pop-up (GNOME Shell's OSD): an icon, an optional
//! label and an optional level bar in a pill near the bottom of the
//! screen. gnome-settings-daemon requests it through org.gnome.Shell.ShowOSD,
//! which the compositor relays (see ipc::OsdRequest).

use crate::ipc::{OsdRequest, ShellClient};
use crate::overlay;
use adw::prelude::*;
use gtk::glib;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

const HIDE_TIMEOUT: Duration = Duration::from_millis(1500);
const FADE_MS: u32 = 200;

struct Osd {
    window: gtk::ApplicationWindow,
    icon: gtk::Image,
    label: gtk::Label,
    level: gtk::LevelBar,
    fade: adw::TimedAnimation,
    hide_timeout: RefCell<Option<glib::SourceId>>,
}

pub fn install(app: &adw::Application, client: &Rc<ShellClient>) {
    let window = overlay::window(app, overlay::OSD_TITLE);
    overlay::set_click_through(&window);

    let card = gtk::Box::builder()
        .spacing(12)
        .css_classes(["osd-card"])
        .build();
    let icon = gtk::Image::builder().pixel_size(24).build();
    let label = gtk::Label::builder().css_classes(["osd-label"]).build();
    let level = gtk::LevelBar::builder()
        .width_request(200)
        .valign(gtk::Align::Center)
        .build();
    // Drop the default low/high/full color offsets; GNOME's bar is one color.
    for name in ["low", "high", "full"] {
        level.remove_offset_value(Some(name));
    }
    card.append(&icon);
    card.append(&label);
    card.append(&level);
    window.set_child(Some(&card));

    let fade = adw::TimedAnimation::new(
        &card,
        0.0,
        1.0,
        FADE_MS,
        adw::PropertyAnimationTarget::new(&card, "opacity"),
    );

    let osd = Rc::new(Osd {
        window,
        icon,
        label,
        level,
        fade,
        hide_timeout: RefCell::default(),
    });

    // Once faded out, unmap the window so it doesn't linger on screen.
    osd.fade.connect_done(glib::clone!(
        #[weak(rename_to = window)]
        osd.window,
        move |fade| {
            if fade.value_to() == 0.0 {
                window.set_visible(false);
            }
        }
    ));

    client.connect_osd(move |request| osd.show(request));
}

impl Osd {
    fn show(self: &Rc<Self>, request: &OsdRequest) {
        self.icon
            .set_icon_name(Some(request.icon.as_deref().unwrap_or("dialog-information-symbolic")));

        self.label.set_label(request.label.as_deref().unwrap_or_default());
        self.label.set_visible(request.label.is_some());

        // max_level > 1 means over-amplification (e.g. volume up to 150 %).
        let max = request.max_level.max(1.0);
        self.level.set_max_value(max);
        self.level.set_value(request.level.unwrap_or(0.0).clamp(0.0, max));
        self.level.set_visible(request.level.is_some());

        self.window.present();
        self.animate_to(1.0);

        if let Some(source) = self.hide_timeout.take() {
            source.remove();
        }
        let this = Rc::downgrade(self);
        let source = glib::timeout_add_local_once(HIDE_TIMEOUT, move || {
            if let Some(this) = this.upgrade() {
                this.hide_timeout.take();
                this.animate_to(0.0);
            }
        });
        self.hide_timeout.replace(Some(source));
    }

    fn animate_to(&self, opacity: f64) {
        if self.fade.value_to() == opacity && self.fade.state() == adw::AnimationState::Playing {
            return;
        }
        let current = self.window.child().map(|c| c.opacity()).unwrap_or(0.0);
        self.fade.set_value_from(current);
        self.fade.set_value_to(opacity);
        self.fade.play();
    }
}
