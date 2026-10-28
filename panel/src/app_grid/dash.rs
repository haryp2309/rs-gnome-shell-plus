//! The dash: favourites, running apps that aren't favourites, and the
//! "Show Apps" button, in GNOME Shell's rounded translucent box.

use super::apps::{self, App};
use super::favorites;
use super::tile::{self, Options};
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

const ICON_SIZE: i32 = 48;
const TILE_SIZE: i32 = 64;

pub struct Dash {
    root: gtk::Box,
    items: gtk::Box,
    show_apps: gtk::ToggleButton,
    on_show_apps: Box<dyn Fn()>,
    /// Set while we change the toggle ourselves.
    updating: Cell<bool>,
    running: RefCell<Vec<String>>,
    /// Favourite tiles by desktop id, in order (for drop positions).
    favorite_tiles: RefCell<Vec<(String, gtk::Widget)>>,
    _monitor: gio::AppInfoMonitor,
}

impl Dash {
    pub fn new(on_show_apps: impl Fn() + 'static) -> Rc<Self> {
        super::load_css();
        let root = gtk::Box::builder()
            .name("dash")
            .css_classes(["dash"])
            .halign(gtk::Align::Center)
            .valign(gtk::Align::End)
            .build();
        let items = gtk::Box::builder().css_classes(["dash-items"]).build();
        let show_apps = gtk::ToggleButton::builder()
            .name("dash-show-apps")
            .css_classes(["dash-item", "show-apps"])
            .tooltip_text("Show Apps")
            .child(
                &gtk::Image::builder()
                    .icon_name("view-app-grid-symbolic")
                    .pixel_size(32)
                    .css_classes(["show-apps-icon"])
                    .build(),
            )
            .build();
        show_apps.set_size_request(TILE_SIZE, TILE_SIZE);
        root.append(&items);
        root.append(&separator("dash-show-apps-separator"));
        root.append(&show_apps);

        let dash = Rc::new(Self {
            root,
            items,
            show_apps,
            on_show_apps: Box::new(on_show_apps),
            updating: Cell::new(false),
            running: RefCell::default(),
            favorite_tiles: RefCell::default(),
            _monitor: gio::AppInfoMonitor::get(),
        });

        let weak = Rc::downgrade(&dash);
        dash.show_apps.connect_toggled(move |_| {
            if let Some(dash) = weak.upgrade() {
                if !dash.updating.get() {
                    (dash.on_show_apps)();
                }
            }
        });
        let weak = Rc::downgrade(&dash);
        favorites::store().connect_changed(move || redisplay_later(&weak));
        let weak = Rc::downgrade(&dash);
        dash._monitor.connect_changed(move |_| redisplay_later(&weak));
        dash.setup_drop();
        dash.redisplay();
        dash
    }

    pub fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }

    /// The app ids (GTK application ids or WM_CLASSes) of the apps that
    /// have windows, in the order to show them.
    pub fn set_running(&self, app_ids: &[String]) {
        apps::set_running(app_ids);
        *self.running.borrow_mut() = app_ids.to_vec();
        self.redisplay();
    }

    /// Checks the "Show Apps" button without calling `on_show_apps`.
    pub fn set_show_apps_checked(&self, checked: bool) {
        self.updating.set(true);
        self.show_apps.set_active(checked);
        self.updating.set(false);
    }

    /// How to raise a running app (called with its app id as passed to
    /// [`Self::set_running`]) instead of launching it again. Applies to the
    /// grid and the search results too. Without it, clicks launch.
    pub fn set_activate_running(&self, f: impl Fn(&str) + 'static) {
        apps::set_activate_running(f);
    }

    fn redisplay(&self) {
        while let Some(child) = self.items.first_child() {
            self.items.remove(&child);
        }
        let options = Options {
            prefix: "dash",
            icon_size: ICON_SIZE,
            label: false,
            css_class: "dash-item",
            menu: true,
            drag: true,
        };
        let mut shown: Vec<String> = Vec::new();
        let mut favorite_tiles = Vec::new();
        for id in favorites::store().get() {
            let Some(app) = apps::lookup(&id) else { continue };
            if shown.contains(&app.id) {
                continue;
            }
            let tile = self.tile(&app, &options);
            self.items.append(&tile);
            favorite_tiles.push((app.desktop_id(), tile));
            shown.push(app.id);
        }
        *self.favorite_tiles.borrow_mut() = favorite_tiles;

        let mut running_tiles = Vec::new();
        for running_id in self.running.borrow().iter() {
            match apps::resolve_running(running_id, &[]) {
                Some(app) if shown.contains(&app.id) => {}
                Some(app) => {
                    running_tiles.push(self.tile(&app, &options));
                    shown.push(app.id);
                }
                None if shown.contains(running_id) => {}
                None => {
                    let tile = tile::window_tile(running_id, &options);
                    tile.button.set_size_request(TILE_SIZE, TILE_SIZE);
                    tile.dot.set_visible(true);
                    running_tiles.push(tile.root.upcast());
                    shown.push(running_id.clone());
                }
            }
        }
        if !self.favorite_tiles.borrow().is_empty() && !running_tiles.is_empty() {
            self.items.append(&separator("dash-separator"));
        }
        for tile in running_tiles {
            self.items.append(&tile);
        }
        // Without any app icons, don't show a lone separator.
        if let Some(sep) = self.root.first_child().and_then(|c| c.next_sibling()) {
            sep.set_visible(!shown.is_empty());
        }
    }

    fn tile(&self, app: &App, options: &Options) -> gtk::Widget {
        let tile = tile::app_tile(app, &format!("dash-app-{}", app.id), options);
        tile.button.set_size_request(TILE_SIZE, TILE_SIZE);
        tile.dot.set_visible(apps::running_id(app).is_some());
        tile.root.upcast()
    }

    /// Dropping an app (from the dash or the grid) makes it a favourite at
    /// the drop position.
    fn setup_drop(self: &Rc<Self>) {
        let target = gtk::DropTarget::new(glib::Type::STRING, gdk::DragAction::COPY | gdk::DragAction::MOVE);
        let weak = Rc::downgrade(self);
        target.connect_drop(move |_, value, x, _| {
            let (Some(dash), Ok(id)) = (weak.upgrade(), value.get::<String>()) else { return false };
            let mut position = 0;
            for (tile_id, widget) in dash.favorite_tiles.borrow().iter() {
                if tile_id.eq_ignore_ascii_case(&id) {
                    continue;
                }
                let Some(bounds) = widget.compute_bounds(&dash.items) else { continue };
                if (bounds.x() + bounds.width() / 2.0) < x as f32 {
                    position += 1;
                }
            }
            glib::idle_add_local_once(move || favorites::store().add(&id, Some(position)));
            true
        });
        self.items.add_controller(target);
    }
}

fn redisplay_later(weak: &Weak<Dash>) {
    let weak = weak.clone();
    glib::idle_add_local_once(move || {
        if let Some(dash) = weak.upgrade() {
            dash.redisplay();
        }
    });
}

fn separator(name: &str) -> gtk::Separator {
    gtk::Separator::builder()
        .name(name)
        .orientation(gtk::Orientation::Vertical)
        .css_classes(["dash-separator"])
        .valign(gtk::Align::Center)
        .build()
}
