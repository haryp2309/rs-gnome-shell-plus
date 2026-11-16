//! GNOME Shell's Alt-Tab pop-up. The compositor owns the keyboard side
//! (keybindings, the grab, the modifier release, activating the window; see
//! compositor/src/shellplus-switcher.c) and tells us what to draw through
//! the SwitcherShow / SwitcherSelect / SwitcherThumbnails / SwitcherHide
//! signals.
//!
//! Like GNOME Shell's app switcher, applications are big icons with their
//! names below; Down (or Alt+`) shows the selected app's windows as
//! thumbnails in a second pop-up under it. Like its window switcher,
//! windows are thumbnails with the app icon over their corner and the
//! selected window's title below the row. Window contents come from the
//! overview's preview interface (`overview::client`), by window id.

use crate::ipc::ShellClient;
use crate::overlay;
use crate::overview::client::Client as PreviewClient;
use adw::prelude::*;
use gtk::{gdk, gio};
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
/// Window preview sizes in the window switcher (GNOME's
/// WINDOW_PREVIEW_SIZE is 128), largest first.
const PREVIEW_SIZES: [i32; 6] = [128, 96, 64, 48, 32, 24];
/// The app thumbnails' preview box (GNOME's ThumbnailSwitcher scales
/// windows into about this).
const THUMBNAIL_WIDTH: i32 = 200;
const THUMBNAIL_HEIGHT: i32 = 140;
/// Horizontal space an item needs besides its icon (padding + spacing),
/// and what the pop-up needs besides its items (padding, margin, gaps).
const ITEM_EXTRA: i32 = 2 * 12 + 8;
const POPUP_EXTRA: i32 = 2 * 12 + 2 * 12 + 2 * 48;
/// The pop-up's CSS margin + padding (style.css .switcher-popup) and the
/// item row's spacing, for lining the thumbnails up under their app.
const POPUP_INSET: i32 = 12 + 12;
const LIST_SPACING: i32 = 8;
const FALLBACK_ICON: &str = "application-x-executable";

#[derive(Clone, Debug)]
struct Entry {
    app_id: String,
    title: String,
    n_windows: i32,
    /// (window id, title), most recently used first.
    windows: Vec<(u64, String)>,
}

struct Switcher {
    client: Rc<ShellClient>,
    previews: Rc<PreviewClient>,
    window: gtk::ApplicationWindow,
    popup: gtk::Box,
    list: gtk::Box,
    title: gtk::Label,
    /// Above the pop-up, as tall as the thumbnails below it, so that the
    /// (centered) window keeps the app icons where they were.
    spacer: gtk::Box,
    thumbnails_popup: gtk::Box,
    thumbnails_list: gtk::Box,
    items: RefCell<Vec<gtk::Box>>,
    thumbnails: RefCell<Vec<gtk::Box>>,
    entries: RefCell<Vec<Entry>>,
    windows_mode: Cell<bool>,
    /// The item whose thumbnails are shown, or -1.
    thumbnails_item: Cell<i32>,
    /// Bumped whenever the pop-up's contents are replaced, so late preview
    /// replies for an older pop-up are dropped.
    generation: Cell<u64>,
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
        .halign(gtk::Align::Center)
        .css_classes(["switcher-popup"])
        .build();
    let list = gtk::Box::builder()
        .spacing(LIST_SPACING)
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

    let thumbnails_list = gtk::Box::builder().spacing(LIST_SPACING).build();
    let thumbnails_popup = gtk::Box::builder()
        .name("switcher-thumbnails")
        .halign(gtk::Align::Start)
        .visible(false)
        .css_classes(["switcher-popup", "switcher-thumbnails"])
        .build();
    thumbnails_popup.append(&thumbnails_list);
    let spacer = gtk::Box::builder().visible(false).build();
    let heights = gtk::SizeGroup::new(gtk::SizeGroupMode::Vertical);
    heights.add_widget(&spacer);
    heights.add_widget(&thumbnails_popup);

    let column = gtk::Box::builder()
        .name("switcher-column")
        .orientation(gtk::Orientation::Vertical)
        .build();
    column.append(&spacer);
    column.append(&popup);
    column.append(&thumbnails_popup);
    window.set_child(Some(&column));

    let switcher = Rc::new(Switcher {
        client: client.clone(),
        previews: PreviewClient::new(|_| {}),
        window,
        popup,
        list,
        title,
        spacer,
        thumbnails_popup,
        thumbnails_list,
        items: RefCell::default(),
        thumbnails: RefCell::default(),
        entries: RefCell::default(),
        windows_mode: Cell::new(false),
        thumbnails_item: Cell::new(-1),
        generation: Cell::new(0),
    });

    for signal in ["SwitcherShow", "SwitcherSelect", "SwitcherThumbnails", "SwitcherHide"] {
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
                    type Item = (String, String, i32, Vec<(u64, String)>);
                    if let Some((mode, items, selected)) =
                        params.get::<(String, Vec<Item>, i32)>()
                    {
                        let entries = items
                            .into_iter()
                            .map(|(app_id, title, n_windows, windows)| Entry {
                                app_id,
                                title,
                                n_windows,
                                windows,
                            })
                            .collect();
                        switcher.show(mode == "windows", entries, selected);
                    }
                }
                "SwitcherSelect" => {
                    if let Some((selected, window)) = params.get::<(i32, i32)>() {
                        switcher.select(selected, window);
                    }
                }
                "SwitcherThumbnails" => {
                    if let Some((item,)) = params.get::<(i32,)>() {
                        switcher.show_thumbnails(item);
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

/// A fixed-size box that shows a window's contents, scaled to fit.
fn preview_picture(name: String, width: i32, height: i32) -> gtk::Picture {
    gtk::Picture::builder()
        .name(name)
        .width_request(width)
        .height_request(height)
        .can_shrink(true)
        .content_fit(gtk::ContentFit::Contain)
        .css_classes(["switcher-preview"])
        .build()
}

fn set_selected(widget: &impl IsA<gtk::Widget>, class: &str, on: bool) {
    if on {
        widget.add_css_class(class);
    } else {
        widget.remove_css_class(class);
    }
}

impl Switcher {
    /// The largest of `sizes` at which `n` items fit across the monitor.
    fn fitting_size(&self, sizes: &[i32], n: usize) -> i32 {
        let width = self
            .client
            .state()
            .monitor
            .map(|(_, _, width, _)| width)
            .unwrap_or(1280);
        let n = n.max(1) as i32;
        sizes
            .iter()
            .copied()
            .find(|size| n * (size + ITEM_EXTRA) + POPUP_EXTRA <= width)
            .unwrap_or(sizes[sizes.len() - 1])
    }

    /// Fetches window `id`'s contents into `picture` (asynchronously; the
    /// reply is dropped if the pop-up changed meanwhile).
    fn load_preview(self: &Rc<Self>, picture: &gtk::Picture, id: u64, width: i32, height: i32) {
        let scale = self.window.scale_factor().max(1);
        let generation = self.generation.get();
        let picture = picture.downgrade();
        let switcher = Rc::downgrade(self);
        self.previews.get_preview(id, width * scale, height * scale, move |texture| {
            let (Some(switcher), Some(picture), Some(texture)) =
                (switcher.upgrade(), picture.upgrade(), texture)
            else {
                return;
            };
            if switcher.generation.get() == generation {
                picture.set_paintable(Some(texture.upcast_ref::<gdk::Paintable>()));
            }
        });
    }

    fn show(self: &Rc<Self>, windows_mode: bool, entries: Vec<Entry>, selected: i32) {
        self.generation.set(self.generation.get() + 1);
        self.clear_thumbnails();
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }

        let size = if windows_mode {
            self.fitting_size(&PREVIEW_SIZES, entries.len())
        } else {
            self.fitting_size(&ICON_SIZES, entries.len())
        };
        let items: Vec<gtk::Box> = entries
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                let info = app_info(&entry.app_id);
                let item = gtk::Box::builder()
                    .name(format!("switcher-item-{index}"))
                    .orientation(gtk::Orientation::Vertical)
                    .spacing(6)
                    .css_classes(["switcher-item"])
                    .build();

                if windows_mode {
                    // GNOME's WindowIcon in its default "both" mode: the
                    // window's preview with the app icon over its
                    // bottom-right corner.
                    item.add_css_class("window");
                    let overlay = gtk::Overlay::new();
                    let picture = preview_picture(format!("switcher-preview-{index}"), size, size);
                    if let Some((id, _)) = entry.windows.first() {
                        self.load_preview(&picture, *id, size, size);
                    }
                    overlay.set_child(Some(&picture));
                    let icon = icon_image(&entry.app_id, info.as_ref(), (size * 3 / 8).max(16));
                    icon.set_halign(gtk::Align::End);
                    icon.set_valign(gtk::Align::End);
                    icon.add_css_class("switcher-window-icon");
                    overlay.add_overlay(&icon);
                    item.append(&overlay);
                } else {
                    item.append(&icon_image(&entry.app_id, info.as_ref(), size));
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
        // Wide enough for a useful title, but no wider than the item row.
        self.title
            .set_width_request((entries.len() as i32 * (size + ITEM_EXTRA)).max(240));
        self.items.replace(items);
        self.entries.replace(entries);
        self.select(selected, -1);

        // Size the window to the new content before it is mapped.
        self.window.set_default_size(-1, -1);
        self.window.present();
    }

    fn clear_thumbnails(&self) {
        while let Some(child) = self.thumbnails_list.first_child() {
            self.thumbnails_list.remove(&child);
        }
        self.thumbnails.borrow_mut().clear();
        self.thumbnails_item.set(-1);
        self.thumbnails_popup.set_visible(false);
        self.spacer.set_visible(false);
    }

    /// GNOME's ThumbnailSwitcher: the windows of item `index` in a second
    /// pop-up under it, each with its title below. -1 hides them.
    fn show_thumbnails(self: &Rc<Self>, index: i32) {
        self.clear_thumbnails();
        let entries = self.entries.borrow();
        let Some(entry) = usize::try_from(index).ok().and_then(|i| entries.get(i)) else {
            return;
        };

        let thumbnails: Vec<gtk::Box> = entry
            .windows
            .iter()
            .enumerate()
            .map(|(j, (id, title))| {
                let thumbnail = gtk::Box::builder()
                    .name(format!("switcher-thumbnail-{j}"))
                    .orientation(gtk::Orientation::Vertical)
                    .spacing(6)
                    .css_classes(["switcher-item", "thumbnail"])
                    .build();
                let picture = preview_picture(
                    format!("switcher-thumbnail-preview-{j}"),
                    THUMBNAIL_WIDTH,
                    THUMBNAIL_HEIGHT,
                );
                self.load_preview(&picture, *id, THUMBNAIL_WIDTH, THUMBNAIL_HEIGHT);
                thumbnail.append(&picture);
                thumbnail.append(
                    &gtk::Label::builder()
                        .name(format!("switcher-thumbnail-title-{j}"))
                        .label(title)
                        .ellipsize(gtk::pango::EllipsizeMode::End)
                        .max_width_chars(1)
                        .width_request(THUMBNAIL_WIDTH)
                        .css_classes(["switcher-thumbnail-title"])
                        .build(),
                );
                self.thumbnails_list.append(&thumbnail);
                thumbnail
            })
            .collect();
        drop(entries);
        self.thumbnails.replace(thumbnails);
        self.thumbnails_item.set(index);
        // (Hidden widgets measure as 0.)
        self.thumbnails_popup.set_visible(true);
        self.spacer.set_visible(true);
        self.place_thumbnails(index);
        self.window.set_default_size(-1, -1);
    }

    /// Lines the thumbnails' pop-up up under item `index`, as far as it
    /// fits under the main pop-up. Computed from natural sizes, which the
    /// layout uses as well (the window isn't resizable, so it takes its
    /// natural size): the window is as wide as the main pop-up unless the
    /// thumbnails are wider, in which case they start at its left edge.
    fn place_thumbnails(&self, index: i32) {
        let natural = |widget: &gtk::Widget| widget.measure(gtk::Orientation::Horizontal, -1).1;
        let popup_width = natural(self.popup.upcast_ref());
        let list_width = natural(self.list.upcast_ref());
        let thumbnails_width = natural(self.thumbnails_popup.upcast_ref())
            - self.thumbnails_popup.margin_start();

        let items = self.items.borrow();
        let mut x = POPUP_INSET + (popup_width - 2 * POPUP_INSET - list_width) / 2;
        let mut center = popup_width / 2;
        for (i, item) in items.iter().enumerate() {
            let width = natural(item.upcast_ref());
            if i as i32 == index {
                center = x + width / 2;
                break;
            }
            x += width + LIST_SPACING;
        }
        let max = (popup_width - thumbnails_width).max(0);
        self.thumbnails_popup
            .set_margin_start((center - thumbnails_width / 2).clamp(0, max));
    }

    fn select(&self, selected: i32, window: i32) {
        // While a thumbnail is selected, its app is only outlined (GNOME's
        // highlight(index, justOutline)).
        for (i, item) in self.items.borrow().iter().enumerate() {
            let on = i as i32 == selected;
            set_selected(item, "selected", on && window < 0);
            set_selected(item, "outlined", on && window >= 0);
        }
        let in_thumbnails = selected == self.thumbnails_item.get();
        for (j, thumbnail) in self.thumbnails.borrow().iter().enumerate() {
            set_selected(thumbnail, "selected", in_thumbnails && j as i32 == window);
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
        self.generation.set(self.generation.get() + 1);
        self.window.set_visible(false);
        self.clear_thumbnails();
        self.items.borrow_mut().clear();
        self.entries.borrow_mut().clear();
    }
}
