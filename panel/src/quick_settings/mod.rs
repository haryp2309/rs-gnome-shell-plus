//! System status icons and the quick settings menu.
//!
//! Each section talks to the same services GNOME Shell does
//! (NetworkManager, BlueZ, PulseAudio/PipeWire, UPower,
//! power-profiles-daemon, gsd-rfkill, logind, GSettings) and hides itself
//! when its service isn't available.

mod bluetooth;
mod network;
mod power;
mod sound;
mod toggles;
pub mod widgets;

use crate::ipc::ShellClient;
use gtk::glib;
use gtk::prelude::*;
use std::rc::Rc;
use widgets::{status_icon, MenuHost, ToggleGrid};

/// Panel status icons, in GNOME's left-to-right order. Sections show and
/// update the ones they own (all start hidden).
pub struct Indicators {
    pub microphone: gtk::Image,
    pub network: gtk::Image,
    pub bluetooth: gtk::Image,
    pub airplane: gtk::Image,
    pub volume: gtk::Image,
    pub battery: gtk::Image,
    pub battery_percentage: gtk::Label,
}

impl Indicators {
    fn new(container: &gtk::Box) -> Self {
        let this = Self {
            microphone: status_icon(),
            network: status_icon(),
            bluetooth: status_icon(),
            airplane: status_icon(),
            volume: status_icon(),
            battery: status_icon(),
            battery_percentage: gtk::Label::builder().visible(false).build(),
        };
        this.microphone.add_css_class("privacy-indicator");
        for icon in [&this.microphone, &this.network, &this.bluetooth, &this.airplane, &this.volume, &this.battery] {
            container.append(icon);
        }
        container.append(&this.battery_percentage);
        this
    }
}

/// What every section gets to build itself.
pub struct Ctx {
    pub client: Rc<ShellClient>,
    pub host: Rc<MenuHost>,
    pub indicators: Indicators,
    pub popover: gtk::Popover,
}

pub fn build(client: Rc<ShellClient>) -> gtk::MenuButton {
    let icons = gtk::Box::builder().spacing(8).css_classes(["status-icons"]).build();
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .width_request(380)
        .css_classes(["quick-settings"])
        .build();
    let popover = crate::panel::menu_popover(&content);
    let button = gtk::MenuButton::builder()
        .name("qs-button")
        .child(&icons)
        .popover(&popover)
        .css_classes(["panel-button"])
        .build();

    let ctx = Rc::new(Ctx {
        client,
        host: Rc::new(MenuHost::default()),
        indicators: Indicators::new(&icons),
        popover: popover.clone(),
    });
    popover.connect_closed(glib::clone!(
        #[strong]
        ctx,
        move |_| ctx.host.close_all()
    ));

    // Top row: battery, Settings, power submenu.
    content.append(&power::system_row(&ctx));

    // Sliders: output volume, input volume (while recording), brightness.
    let sliders = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .css_classes(["sliders"])
        .build();
    sound::build(&ctx, &sliders);
    power::brightness_slider(&ctx, &sliders);
    content.append(&sliders);

    // Toggle grid, in GNOME's order.
    let grid = ToggleGrid::new();
    let mut toggles = network::build(&ctx);
    toggles.extend(bluetooth::build(&ctx));
    toggles.extend(power::profiles_toggle(&ctx));
    toggles.extend(toggles::build(&ctx));
    for toggle in toggles {
        grid.add(toggle);
    }
    content.append(&grid.root);
    // The grid reflows itself later; keep it alive as long as its widget.
    let root = grid.root.clone();
    root.connect_destroy(move |_| {
        let _ = &grid;
    });

    button
}
