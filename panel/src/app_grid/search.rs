//! App search: GNOME Shell's applications section, a single row of large
//! icons for the best matches, or "No Results".
//!
//! Matching is our own (gio::DesktopAppInfo::search is in gio-unix), close
//! to GLib's desktop file index plus GNOME Shell's ordering: every term must
//! match the name, executable, keywords, generic name or comment; matches at
//! the start of the name rank first, then at the start of a word in the
//! name, then the executable, keywords, generic name, the middle of the
//! name, and the comment. Ties are alphabetical.

use super::apps::{self, App};
use super::size_bin::SizeBin;
use super::tile::{self, Options};
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

const ICON_SIZE: i32 = 96;
const TILE_WIDTH: i32 = 144;
const TILE_HEIGHT: i32 = 140;
const SPACING: i32 = 12;
const SECTION_PADDING: i32 = 2 * 24;

pub struct SearchResults {
    root: gtk::Box,
    section: gtk::Box,
    row: gtk::Box,
    no_results: gtk::Label,
    results: RefCell<Vec<App>>,
    /// How many icons fit across.
    max_items: Cell<usize>,
}

impl SearchResults {
    pub fn new() -> Rc<Self> {
        super::load_css();
        let root = gtk::Box::builder()
            .name("search-results")
            .orientation(gtk::Orientation::Vertical)
            .css_classes(["search-results"])
            .hexpand(true)
            .valign(gtk::Align::Start)
            .build();
        let row = gtk::Box::builder()
            .spacing(SPACING)
            .halign(gtk::Align::Center)
            .css_classes(["search-results-row"])
            .build();
        let section = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .css_classes(["search-section"])
            .visible(false)
            .build();
        let no_results = gtk::Label::builder()
            .name("search-no-results")
            .label("No Results")
            .css_classes(["search-statustext"])
            .halign(gtk::Align::Center)
            .visible(false)
            .build();
        root.append(&no_results);

        let this = Rc::new_cyclic(|weak: &std::rc::Weak<SearchResults>| {
            let weak = weak.clone();
            let sizer = SizeBin::new(&row, move |width, _| {
                if let Some(this) = weak.upgrade() {
                    let fit = ((width - SECTION_PADDING + SPACING) / (TILE_WIDTH + SPACING)).max(1) as usize;
                    if this.max_items.replace(fit) != fit {
                        this.render();
                    }
                }
            });
            sizer.set_hexpand(true);
            section.append(&sizer);
            root.append(&section);
            Self {
                root,
                section,
                row,
                no_results,
                results: RefCell::default(),
                max_items: Cell::new(8),
            }
        });
        this
    }

    pub fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }

    pub fn set_query(&self, text: &str) {
        let terms: Vec<String> = text.split_whitespace().map(|t| t.to_lowercase()).collect();
        *self.results.borrow_mut() = if terms.is_empty() { Vec::new() } else { search(&terms) };
        self.no_results.set_visible(!terms.is_empty() && self.results.borrow().is_empty());
        self.render();
    }

    /// Launches (or raises) the top result; false when there is none.
    pub fn activate_first(&self) -> bool {
        let first = self.results.borrow().first().cloned();
        match first {
            Some(app) => {
                apps::activate(&app);
                true
            }
            None => false,
        }
    }

    fn render(&self) {
        while let Some(child) = self.row.first_child() {
            self.row.remove(&child);
        }
        let options = Options {
            prefix: "search",
            icon_size: ICON_SIZE,
            label: true,
            css_class: "search-result-item",
            menu: true,
            drag: true,
        };
        let results = self.results.borrow();
        for (n, app) in results.iter().take(self.max_items.get()).enumerate() {
            let tile = tile::app_tile(app, &format!("search-result-{n}"), &options);
            tile.button.set_size_request(TILE_WIDTH, TILE_HEIGHT);
            tile.dot.set_visible(apps::running_id(app).is_some());
            if n == 0 {
                tile.button.add_css_class("selected");
            }
            self.row.append(&tile.root);
        }
        self.section.set_visible(!results.is_empty());
    }
}

struct Fields {
    name: String,
    exec: String,
    keywords: Vec<String>,
    generic: String,
    comment: String,
}

fn words(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty())
}

fn score_term(fields: &Fields, term: &str) -> u32 {
    let word_prefix = |text: &str| words(text).any(|w| w.starts_with(term));
    if fields.name.starts_with(term) {
        1000
    } else if word_prefix(&fields.name) {
        900
    } else if fields.exec.starts_with(term) {
        700
    } else if fields.keywords.iter().any(|k| k.starts_with(term) || word_prefix(k)) {
        600
    } else if word_prefix(&fields.generic) {
        500
    } else if fields.name.contains(term) {
        300
    } else if word_prefix(&fields.comment) {
        100
    } else {
        0
    }
}

/// Apps matching every term, best first.
fn search(terms: &[String]) -> Vec<App> {
    let mut scored: Vec<(u32, App)> = apps::visible_apps()
        .into_iter()
        .filter_map(|app| {
            let fields = Fields {
                name: app.name.to_lowercase(),
                exec: app
                    .info
                    .executable()
                    .file_name()
                    .map(|f| f.to_string_lossy().to_lowercase())
                    .unwrap_or_default(),
                keywords: app.keywords().iter().map(|k| k.to_lowercase()).collect(),
                generic: app.generic_name().unwrap_or_default().to_lowercase(),
                comment: app.info.description().map(|d| d.to_lowercase()).unwrap_or_default(),
            };
            let mut total = 0;
            for term in terms {
                let score = score_term(&fields, term);
                if score == 0 {
                    return None;
                }
                total += score;
            }
            Some((total, app))
        })
        .collect();
    // visible_apps() is alphabetical and the sort is stable.
    scored.sort_by(|a, b| b.0.cmp(&a.0));
    scored.into_iter().map(|(_, app)| app).collect()
}
