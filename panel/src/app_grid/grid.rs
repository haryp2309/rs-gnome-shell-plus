//! The app grid: pages of app and folder icons in an adw::Carousel (mouse
//! wheel, touchpad and touch swipes change page), page dots below, and the
//! folder popup.

use super::apps::{self, App};
use super::size_bin::SizeBin;
use super::tile::{self, Options};
use crate::system;
use adw::prelude::*;
use gtk::{gdk, gio, glib, pango};
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

const ICON_SIZE: i32 = 96;
const TILE_WIDTH: i32 = 144;
const TILE_HEIGHT: i32 = 140;
const SPACING: i32 = 12;
const PAGE_PADDING_X: i32 = 64;
const PAGE_PADDING_Y: i32 = 0;
const MAX_COLUMNS: i32 = 8;
const MAX_ROWS: i32 = 4;
const FOLDER_COLUMNS: usize = 3;

const FOLDERS_SCHEMA: &str = "org.gnome.desktop.app-folders";
const FOLDER_SCHEMA: &str = "org.gnome.desktop.app-folders.folder";
const FOLDERS_PATH: &str = "/org/gnome/desktop/app-folders/folders/";

#[derive(Clone)]
struct Folder {
    id: String,
    name: String,
    apps: Vec<App>,
}

#[derive(Clone)]
enum Item {
    App(App),
    Folder(Folder),
}

impl Item {
    /// The id app-picker-layout uses.
    fn layout_id(&self) -> String {
        match self {
            Item::App(app) => app.desktop_id(),
            Item::Folder(folder) => folder.id.clone(),
        }
    }
}

pub struct AppGrid {
    this: Weak<AppGrid>,
    root: gtk::Overlay,
    carousel: adw::Carousel,
    dots: gtk::Box,
    /// Each page's items (the carousel's pages, in order).
    pages: RefCell<Vec<Vec<Item>>>,
    /// (columns, rows) per page.
    shape: Cell<(i32, i32)>,
    current: Cell<u32>,
    folder_overlays: RefCell<Vec<gtk::Widget>>,
    folder_settings: RefCell<Vec<gio::Settings>>,
    folder_rebuild: RefCell<Option<Rc<dyn Fn()>>>,
    app_folders: Option<gio::Settings>,
    shell: Option<gio::Settings>,
    _monitor: gio::AppInfoMonitor,
}

impl AppGrid {
    pub fn new() -> Rc<Self> {
        super::load_css();
        let carousel = adw::Carousel::builder()
            .name("app-grid-carousel")
            .allow_scroll_wheel(true)
            .allow_mouse_drag(true)
            .allow_long_swipes(false)
            .hexpand(true)
            .vexpand(true)
            .build();
        let dots = gtk::Box::builder()
            .name("grid-page-dots")
            .css_classes(["page-dots"])
            .halign(gtk::Align::Center)
            .build();
        let column = gtk::Box::builder().orientation(gtk::Orientation::Vertical).build();
        let root = gtk::Overlay::builder()
            .name("app-grid")
            .css_classes(["app-grid"])
            .child(&column)
            .hexpand(true)
            .vexpand(true)
            .build();

        let grid = Rc::new_cyclic(|this: &Weak<AppGrid>| {
            let weak = this.clone();
            let sizer = SizeBin::new(&carousel, move |w, h| {
                if let Some(grid) = weak.upgrade() {
                    grid.resized(w, h);
                }
            });
            sizer.set_hexpand(true);
            sizer.set_vexpand(true);
            column.append(&sizer);
            column.append(&dots);
            Self {
                this: this.clone(),
                root,
                carousel,
                dots,
                pages: RefCell::default(),
                shape: Cell::new((0, 0)),
                current: Cell::new(0),
                folder_overlays: RefCell::default(),
                folder_settings: RefCell::default(),
                folder_rebuild: RefCell::default(),
                app_folders: system::settings(FOLDERS_SCHEMA),
                shell: system::settings("org.gnome.shell")
                    .filter(|s| s.settings_schema().is_some_and(|schema| schema.has_key("app-picker-layout"))),
                _monitor: gio::AppInfoMonitor::get(),
            }
        });

        let weak = Rc::downgrade(&grid);
        grid.carousel.connect_page_changed(move |_, index| {
            if let Some(grid) = weak.upgrade() {
                grid.current.set(index);
                grid.update_dots();
            }
        });

        let weak = Rc::downgrade(&grid);
        let rebuild = system::debounced(move || {
            if let Some(grid) = weak.upgrade() {
                grid.rebuild();
            }
        });
        let r = rebuild.clone();
        grid._monitor.connect_changed(move |_| r());
        if let Some(settings) = &grid.app_folders {
            let r = rebuild.clone();
            settings.connect_changed(None, move |_, _| r());
        }
        if let Some(settings) = &grid.shell {
            let r = rebuild.clone();
            settings.connect_changed(Some("app-picker-layout"), move |_, _| r());
        }
        grid.folder_rebuild.replace(Some(Rc::new(rebuild)));

        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(&grid);
        keys.connect_key_pressed(move |_, key, _, _| {
            let Some(grid) = weak.upgrade() else { return glib::Propagation::Proceed };
            match key {
                gdk::Key::Escape if !grid.folder_overlays.borrow().is_empty() => {
                    grid.close_folder();
                    glib::Propagation::Stop
                }
                gdk::Key::Page_Down => {
                    grid.go_to_page(grid.current.get() + 1, true);
                    glib::Propagation::Stop
                }
                gdk::Key::Page_Up => {
                    grid.go_to_page(grid.current.get().saturating_sub(1), true);
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            }
        });
        grid.root.add_controller(keys);

        grid.rebuild();
        grid
    }

    pub fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }

    /// Back to the first page, with folders closed.
    pub fn reset(&self) {
        self.close_folder();
        self.go_to_page(0, false);
    }

    pub fn go_to_page(&self, index: u32, animate: bool) {
        let n = self.carousel.n_pages();
        if n == 0 {
            return;
        }
        let index = index.min(n - 1);
        self.carousel.scroll_to(&self.carousel.nth_page(index), animate);
        self.current.set(index);
        self.update_dots();
    }

    fn resized(&self, width: i32, height: i32) {
        let fit = |space: i32, padding: i32, tile: i32, max: i32| {
            ((space - 2 * padding + SPACING) / (tile + SPACING)).clamp(1, max)
        };
        let shape = (
            fit(width, PAGE_PADDING_X, TILE_WIDTH, MAX_COLUMNS),
            fit(height, PAGE_PADDING_Y, TILE_HEIGHT, MAX_ROWS),
        );
        if shape != self.shape.get() {
            self.shape.set(shape);
            self.rebuild();
        }
    }

    /// Re-reads the apps, folders and layout, and lays out the pages.
    fn rebuild(&self) {
        let (columns, rows) = self.shape.get();
        if columns == 0 {
            // Not allocated yet; `resized` will call us.
            return;
        }
        let per_page = (columns * rows) as usize;
        let items = self.items();

        // app-picker-layout's pages, then everything else alphabetically.
        let layout = self.picker_layout();
        let mut remaining = items;
        let mut layout_pages: Vec<Vec<Item>> = Vec::new();
        for page in layout {
            let page: Vec<Item> = page
                .iter()
                .filter_map(|id| {
                    let index = remaining.iter().position(|item| item.layout_id().eq_ignore_ascii_case(id))?;
                    Some(remaining.remove(index))
                })
                .collect();
            if !page.is_empty() {
                layout_pages.push(page);
            }
        }
        if let Some(last) = layout_pages.last_mut() {
            last.append(&mut remaining);
        } else {
            layout_pages.push(remaining);
        }
        let pages: Vec<Vec<Item>> = layout_pages
            .into_iter()
            .flat_map(|page| page.chunks(per_page).map(<[Item]>::to_vec).collect::<Vec<_>>())
            .filter(|page| !page.is_empty())
            .collect();

        while self.carousel.n_pages() > 0 {
            self.carousel.remove(&self.carousel.nth_page(0));
        }
        for (n, page) in pages.iter().enumerate() {
            self.carousel.append(&self.page_widget(n, page, columns as usize));
        }
        *self.pages.borrow_mut() = pages;

        while let Some(child) = self.dots.first_child() {
            self.dots.remove(&child);
        }
        for n in 0..self.carousel.n_pages() {
            let dot = gtk::Button::builder()
                .name(format!("grid-page-dot-{n}"))
                .css_classes(["page-dot", "flat"])
                .child(&gtk::Box::builder().css_classes(["dot"]).halign(gtk::Align::Center).valign(gtk::Align::Center).build())
                .build();
            let carousel = self.carousel.clone();
            dot.connect_clicked(move |_| {
                if n < carousel.n_pages() {
                    carousel.scroll_to(&carousel.nth_page(n), true);
                }
            });
            self.dots.append(&dot);
        }
        self.dots.set_opacity(if self.carousel.n_pages() > 1 { 1.0 } else { 0.0 });
        let current = self.current.get();
        self.go_to_page(current, false);
    }

    fn page_widget(&self, n: usize, items: &[Item], columns: usize) -> gtk::Widget {
        let grid = gtk::Grid::builder()
            .name(format!("grid-page-{n}"))
            .css_classes(["app-grid-page"])
            .column_spacing(SPACING)
            .row_spacing(SPACING)
            .column_homogeneous(true)
            .row_homogeneous(true)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .hexpand(true)
            .vexpand(true)
            .build();
        for (i, item) in items.iter().enumerate() {
            let widget = match item {
                Item::App(app) => self.app_tile(app).upcast(),
                Item::Folder(folder) => self.folder_tile(folder),
            };
            grid.attach(&widget, (i % columns) as i32, (i / columns) as i32, 1, 1);
        }
        grid.upcast()
    }

    fn app_tile(&self, app: &App) -> gtk::Overlay {
        let options = Options {
            prefix: "grid",
            icon_size: ICON_SIZE,
            label: true,
            css_class: "app-grid-item",
            menu: true,
            drag: true,
        };
        let tile = tile::app_tile(app, &format!("grid-app-{}", app.id), &options);
        tile.button.set_size_request(TILE_WIDTH, TILE_HEIGHT);
        tile.dot.set_visible(apps::running_id(app).is_some());
        tile.root
    }

    fn folder_tile(&self, folder: &Folder) -> gtk::Widget {
        let preview = gtk::Grid::builder()
            .row_spacing(4)
            .column_spacing(4)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .row_homogeneous(true)
            .column_homogeneous(true)
            .build();
        for (i, app) in folder.apps.iter().take(4).enumerate() {
            let image = gtk::Image::builder().pixel_size(36).build();
            match app.icon() {
                Some(icon) => image.set_from_gicon(&icon),
                None => image.set_icon_name(Some("application-x-executable")),
            }
            preview.attach(&image, (i % 2) as i32, (i / 2) as i32, 1, 1);
        }
        let frame = gtk::Box::builder().css_classes(["folder-icon"]).halign(gtk::Align::Center).build();
        frame.set_size_request(ICON_SIZE, ICON_SIZE);
        preview.set_hexpand(true);
        frame.append(&preview);
        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(8)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .build();
        content.append(&frame);
        content.append(
            &gtk::Label::builder()
                .label(&folder.name)
                .ellipsize(pango::EllipsizeMode::End)
                .max_width_chars(12)
                .css_classes(["app-name"])
                .build(),
        );
        let button = gtk::Button::builder()
            .name(format!("grid-folder-{}", folder.id))
            .child(&content)
            .css_classes(["app-grid-item", "app-folder"])
            .build();
        button.set_size_request(TILE_WIDTH, TILE_HEIGHT);
        let folder_id = folder.id.clone();
        let this = self.this.clone();
        button.connect_clicked(move |_| {
            if let Some(grid) = this.upgrade() {
                grid.open_folder(&folder_id);
            }
        });
        button.upcast()
    }

    /// Opens the folder popup: a dimmed backdrop (click it, or press
    /// Escape, to close) and a card with the folder's name and apps.
    pub fn open_folder(&self, folder_id: &str) {
        let Some(folder) = self
            .pages
            .borrow()
            .iter()
            .flatten()
            .find_map(|item| match item {
                Item::Folder(f) if f.id == folder_id => Some(f.clone()),
                _ => None,
            })
        else {
            return;
        };
        self.close_folder();

        let backdrop = gtk::Box::builder()
            .name("grid-folder-backdrop")
            .css_classes(["app-folder-backdrop"])
            .hexpand(true)
            .vexpand(true)
            .build();
        let click = gtk::GestureClick::new();
        let this = self.this.clone();
        click.connect_released(move |_, _, _, _| {
            if let Some(grid) = this.upgrade() {
                grid.close_folder();
            }
        });
        backdrop.add_controller(click);

        let card = gtk::Box::builder()
            .name("grid-folder-dialog")
            .orientation(gtk::Orientation::Vertical)
            .spacing(18)
            .css_classes(["app-folder-dialog"])
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .build();
        card.append(
            &gtk::Label::builder()
                .name("grid-folder-title")
                .label(&folder.name)
                .css_classes(["folder-name"])
                .build(),
        );
        let grid = gtk::Grid::builder()
            .column_spacing(SPACING)
            .row_spacing(SPACING)
            .column_homogeneous(true)
            .halign(gtk::Align::Center)
            .build();
        let columns = FOLDER_COLUMNS.min(folder.apps.len().max(1));
        for (i, app) in folder.apps.iter().enumerate() {
            let tile = self.app_tile(app);
            let this = self.this.clone();
            if let Some(button) = tile.child().and_downcast::<gtk::Button>() {
                button.connect_clicked(move |_| {
                    if let Some(grid) = this.upgrade() {
                        // After the click handler that launches.
                        glib::idle_add_local_once(move || grid.close_folder());
                    }
                });
            }
            grid.attach(&tile, (i % columns) as i32, (i / columns) as i32, 1, 1);
        }
        let rows = folder.apps.len().div_ceil(columns);
        if rows > 3 {
            let scrolled = gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .min_content_height(3 * (TILE_HEIGHT + SPACING))
                .propagate_natural_width(true)
                .child(&grid)
                .build();
            card.append(&scrolled);
        } else {
            card.append(&grid);
        }

        // Clicks on the card must not reach the backdrop.
        let swallow = gtk::GestureClick::new();
        swallow.connect_released(|g, _, _, _| {
            g.set_state(gtk::EventSequenceState::Claimed);
        });
        card.add_controller(swallow);

        self.root.add_overlay(&backdrop);
        self.root.add_overlay(&card);
        *self.folder_overlays.borrow_mut() = vec![backdrop.upcast(), card.upcast()];
    }

    pub fn close_folder(&self) {
        for widget in self.folder_overlays.take() {
            self.root.remove_overlay(&widget);
        }
    }

    fn update_dots(&self) {
        let current = self.current.get();
        let mut child = self.dots.first_child();
        let mut n = 0;
        while let Some(dot) = child {
            if n == current {
                dot.add_css_class("active");
            } else {
                dot.remove_css_class("active");
            }
            n += 1;
            child = dot.next_sibling();
        }
    }

    /// Top-level items: folders (with at least one app) and the apps that
    /// aren't in any folder, alphabetically.
    fn items(&self) -> Vec<Item> {
        let apps = apps::visible_apps();
        let folders = self.folders(&apps);
        let in_folder = |app: &App| folders.iter().any(|f| f.apps.iter().any(|a| a.id == app.id));
        let mut items: Vec<Item> = apps.into_iter().filter(|app| !in_folder(app)).map(Item::App).collect();
        items.extend(folders.into_iter().map(Item::Folder));
        items.sort_by_cached_key(|item| match item {
            Item::App(app) => app.name.to_lowercase(),
            Item::Folder(folder) => folder.name.to_lowercase(),
        });
        items
    }

    fn folders(&self, apps: &[App]) -> Vec<Folder> {
        let Some(settings) = &self.app_folders else { return Vec::new() };
        let has_folder_schema = gio::SettingsSchemaSource::default()
            .and_then(|source| source.lookup(FOLDER_SCHEMA, true))
            .is_some();
        if !has_folder_schema {
            return Vec::new();
        }
        let rebuild = self.folder_rebuild.borrow().clone();
        let mut all_settings = Vec::new();
        let mut folders = Vec::new();
        for id in settings.strv("folder-children") {
            let folder = gio::Settings::with_path(FOLDER_SCHEMA, &format!("{FOLDERS_PATH}{id}/"));
            if let Some(rebuild) = &rebuild {
                let rebuild = rebuild.clone();
                folder.connect_changed(None, move |_, _| rebuild());
            }
            let listed: Vec<String> = folder.strv("apps").iter().map(|s| s.to_lowercase()).collect();
            let excluded: Vec<String> = folder.strv("excluded-apps").iter().map(|s| s.to_lowercase()).collect();
            let categories: Vec<String> = folder.strv("categories").iter().map(|s| s.to_string()).collect();
            let folder_apps: Vec<App> = apps
                .iter()
                .filter(|app| {
                    let id = app.desktop_id().to_lowercase();
                    !excluded.contains(&id)
                        && (listed.contains(&id)
                            || (!categories.is_empty() && app.categories().iter().any(|c| categories.contains(c))))
                })
                .cloned()
                .collect();
            let name = folder_name(&folder, id.as_str());
            all_settings.push(folder);
            if !folder_apps.is_empty() {
                folders.push(Folder { id: id.to_string(), name, apps: folder_apps });
            }
        }
        *self.folder_settings.borrow_mut() = all_settings;
        folders
    }

    /// org.gnome.shell app-picker-layout: per page, the item ids by position.
    fn picker_layout(&self) -> Vec<Vec<String>> {
        let Some(settings) = &self.shell else { return Vec::new() };
        let layout = settings.value("app-picker-layout");
        layout
            .iter()
            .map(|page| {
                let mut entries: Vec<(i32, String)> = page
                    .iter()
                    .filter_map(|entry| {
                        let id = entry.child_value(0).str()?.to_owned();
                        let props = entry.child_value(1);
                        let props = if props.is::<glib::Variant>() { props.as_variant()? } else { props };
                        let position = system::lookup(&props, "position")?.get::<i32>()?;
                        Some((position, id))
                    })
                    .collect();
                entries.sort();
                entries.into_iter().map(|(_, id)| id).collect()
            })
            .collect()
    }
}

/// A folder's display name: `name`, or the Name of the .desktop-directories
/// entry it refers to.
fn folder_name(folder: &gio::Settings, id: &str) -> String {
    let name = folder.string("name").to_string();
    if name.ends_with(".directory") {
        let dirs = std::iter::once(glib::user_data_dir()).chain(glib::system_data_dirs());
        for dir in dirs {
            let file = glib::KeyFile::new();
            if file
                .load_from_file(dir.join("desktop-directories").join(&name), glib::KeyFileFlags::NONE)
                .is_ok()
            {
                if let Ok(n) = file.locale_string("Desktop Entry", "Name", None) {
                    return n.to_string();
                }
            }
        }
        return name.trim_end_matches(".directory").to_owned();
    }
    if name.is_empty() {
        id.to_owned()
    } else {
        name
    }
}
