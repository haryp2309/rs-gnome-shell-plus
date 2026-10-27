//! GNOME Shell's Alt-Tab pop-up. The compositor owns the keyboard side
//! (keybindings, the grab, the modifier release, activating the window; see
//! compositor/src/shellplus-switcher.c) and tells us what to draw through
//! the SwitcherShow / SwitcherSelect / SwitcherHide signals.
//!
//! Like GNOME Shell's app switcher, applications are big icons with their
//! names below; windows are icons with the selected window's title below
//! the row.

use crate::ipc::ShellClient;
use crate::overlay;
use adw::prelude::*;
use gtk::gio;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// The compositor recognizes the pop-up by this title.
pub const TITLE: &str = "Switcher";

// The private compositor interface (see ipc.rs).
const BUS_NAME: &str = "org.gnome.ShellPlus";
const PATH: &str = "/org/gnome/ShellPlus";
const IFACE: &str = "org.gnome.ShellPlus.Panel";

/// GNOME Shell's icon sizes, largest first; the biggest that fits is used.
const ICON_SIZES: [i32; 6] = [96, 64, 48, 32, 24, 16];
/// Horizontal space an item needs besides its icon (padding + spacing),
/// and what the pop-up needs besides its items (padding, margin, gaps).
const ITEM_EXTRA: i32 = 2 * 12 + 8;
const POPUP_EXTRA: i32 = 2 * 12 + 2 * 12 + 2 * 48;
const FALLBACK_ICON: &str = "application-x-executable";

#[derive(Clone, Debug)]
struct Entry {
    app_id: String,
    title: String,
    n_windows: i32,
}

struct Switcher {
    client: Rc<ShellClient>,
    window: gtk::ApplicationWindow,
    list: gtk::Box,
    title: gtk::Label,
    items: RefCell<Vec<gtk::Box>>,
    entries: RefCell<Vec<Entry>>,
    windows_mode: Cell<bool>,
}

pub fn install(app: &adw::Application, client: &Rc<ShellClient>) {
    let Ok(connection) = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) else {
        return;
    };

    let window = overlay::window(app, TITLE);
    overlay::set_click_through(&window);

    let popup = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(10)
        .css_classes(["switcher-popup"])
        .build();
    let list = gtk::Box::builder()
        .spacing(8)
        .halign(gtk::Align::Center)
        .build();
    let title = gtk::Label::builder()
        .css_classes(["switcher-title"])
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .max_width_chars(1)
        .visible(false)
        .build();
    popup.append(&list);
    popup.append(&title);
    window.set_child(Some(&popup));

    let switcher = Rc::new(Switcher {
        client: client.clone(),
        window,
        list,
        title,
        items: RefCell::default(),
        entries: RefCell::default(),
        windows_mode: Cell::new(false),
    });

    for signal in ["SwitcherShow", "SwitcherSelect", "SwitcherHide"] {
        let switcher = switcher.clone();
        #[allow(deprecated)]
        connection.signal_subscribe(
            Some(BUS_NAME),
            Some(IFACE),
            Some(signal),
            Some(PATH),
            None,
            gio::DBusSignalFlags::NONE,
            move |_, _, _, _, signal, params| match signal {
                "SwitcherShow" => {
                    if let Some((mode, items, selected)) =
                        params.get::<(String, Vec<(String, String, i32)>, i32)>()
                    {
                        let entries = items
                            .into_iter()
                            .map(|(app_id, title, n_windows)| Entry {
                                app_id,
                                title,
                                n_windows,
                            })
                            .collect();
                        switcher.show(mode == "windows", entries, selected);
                    }
                }
                "SwitcherSelect" => {
                    if let Some((selected,)) = params.get::<(i32,)>() {
                        switcher.select(selected);
                    }
                }
                _ => switcher.hide(),
            },
        );
    }
}

/// The installed app with desktop file id `<app_id>.desktop` (matched
/// case-insensitively, as app ids and desktop file names often differ in
/// case). `gio::DesktopAppInfo` lives in the gio-unix crate, so go through
/// the AppInfo interface; GLib caches the desktop file index.
fn app_info(app_id: &str) -> Option<gio::AppInfo> {
    if app_id.is_empty() {
        return None;
    }
    let desktop_id = format!("{app_id}.desktop").to_lowercase();
    gio::AppInfo::all()
        .into_iter()
        .find(|info| info.id().is_some_and(|id| id.to_lowercase() == desktop_id))
}

fn icon_image(app_id: &str, info: Option<&gio::AppInfo>, size: i32) -> gtk::Image {
    let image = gtk::Image::builder().pixel_size(size).build();
    if let Some(icon) = info.and_then(|info| info.icon()) {
        image.set_from_gicon(&icon);
        return image;
    }
    // Apps without a .desktop file often still install an icon named after
    // their id.
    let has_icon = !app_id.is_empty()
        && gtk::gdk::Display::default()
            .map(|display| gtk::IconTheme::for_display(&display).has_icon(app_id))
            .unwrap_or(false);
    image.set_icon_name(Some(if has_icon { app_id } else { FALLBACK_ICON }));
    image
}

impl Switcher {
    /// The largest icon size at which all items fit across the monitor.
    fn icon_size(&self, n: usize) -> i32 {
        let width = self
            .client
            .state()
            .monitor
            .map(|(_, _, width, _)| width)
            .unwrap_or(1280);
        let n = n.max(1) as i32;
        ICON_SIZES
            .into_iter()
            .find(|size| n * (size + ITEM_EXTRA) + POPUP_EXTRA <= width)
            .unwrap_or(ICON_SIZES[ICON_SIZES.len() - 1])
    }

    fn show(&self, windows_mode: bool, entries: Vec<Entry>, selected: i32) {
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }

        let size = self.icon_size(entries.len());
        let items: Vec<gtk::Box> = entries
            .iter()
            .map(|entry| {
                let info = app_info(&entry.app_id);
                let item = gtk::Box::builder()
                    .orientation(gtk::Orientation::Vertical)
                    .spacing(6)
                    .css_classes(["switcher-item"])
                    .build();
                if windows_mode {
                    item.add_css_class("window");
                }
                item.append(&icon_image(&entry.app_id, info.as_ref(), size));

                if !windows_mode {
                    let name = info
                        .as_ref()
                        .map(|info| info.name().to_string())
                        .filter(|name| !name.is_empty())
                        .unwrap_or_else(|| entry.title.clone());
                    // As wide as the icon; long names are ellipsized.
                    let label = gtk::Label::builder()
                        .label(name)
                        .css_classes(["switcher-item-label"])
                        .ellipsize(gtk::pango::EllipsizeMode::End)
                        .max_width_chars(1)
                        .width_request(size.max(64))
                        .build();
                    item.append(&label);

                    // GNOME marks apps with several windows with an arrow.
                    let arrow = gtk::Image::builder()
                        .icon_name("pan-down-symbolic")
                        .pixel_size(12)
                        .css_classes(["switcher-arrow"])
                        .opacity(if entry.n_windows > 1 { 1.0 } else { 0.0 })
                        .build();
                    item.append(&arrow);
                }

                self.list.append(&item);
                item
            })
            .collect();

        self.windows_mode.set(windows_mode);
        self.title.set_visible(windows_mode);
        // Wide enough for a useful title, but no wider than the icon row.
        self.title
            .set_width_request((entries.len() as i32 * (size + ITEM_EXTRA)).max(240));
        self.items.replace(items);
        self.entries.replace(entries);
        self.select(selected);

        // Size the window to the new content before it is mapped.
        self.window.set_default_size(-1, -1);
        self.window.present();
    }

    fn select(&self, selected: i32) {
        for (i, item) in self.items.borrow().iter().enumerate() {
            if i as i32 == selected {
                item.add_css_class("selected");
            } else {
                item.remove_css_class("selected");
            }
        }
        if self.windows_mode.get() {
            let entries = self.entries.borrow();
            let title = usize::try_from(selected)
                .ok()
                .and_then(|i| entries.get(i))
                .map(|entry| entry.title.as_str())
                .unwrap_or_default();
            self.title.set_label(title);
        }
    }

    fn hide(&self) {
        self.window.set_visible(false);
        self.items.borrow_mut().clear();
        self.entries.borrow_mut().clear();
    }
}
