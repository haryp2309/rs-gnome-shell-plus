//! GNOME Shell's dash, app grid and app search, as widgets for the
//! Activities overview to embed.
//!
//! - [`Dash`]: favourites (org.gnome.shell `favorite-apps`, else our own
//!   key file, else GNOME's defaults that are installed), then running apps
//!   that aren't favourites, then the "Show Apps" button. Running apps get a
//!   dot; right-click opens the app menu; favourites reorder by drag and drop
//!   (and apps dragged in from the grid become favourites).
//! - [`AppGrid`]: every app that should show, in pages (adw::Carousel) with
//!   page dots, ordered by org.gnome.shell `app-picker-layout` when
//!   available, otherwise alphabetically, plus the folders from
//!   org.gnome.desktop.app-folders, which open in a popup.
//! - [`SearchResults`]: one row of large icons for the apps matching a query
//!   (name, generic name, keywords, executable), or "No Results".
//!
//! Clicking an app anywhere raises it when it is running and the overview
//! has given us a way to (see [`Dash::set_activate_running`]); otherwise it
//! launches it with a GdkAppLaunchContext. Either way the
//! [`connect_launched`] callbacks run afterwards, so the overview can close.
//!
//! Widget names (for dev/driver.py; `<id>` is the desktop file id without
//! ".desktop", or a running app id that has no desktop file):
//! `dash-app-<id>`, `dash-dot-<id>` (visible while running),
//! `dash-menu-<id>` (opens the app menu) with items `dash-menu-<id>-new-window`,
//! `-favorite`, `-details`, `dash-separator`, `dash-show-apps`;
//! `app-grid-carousel`, `grid-page-<n>`, `grid-page-dot-<n>` (class `active`
//! on the current page), `grid-app-<id>`, `grid-menu-<id>…` (as for the
//! dash), `grid-folder-<id>`, `grid-folder-dialog`, `grid-folder-title`,
//! `grid-folder-backdrop`; `search-result-<n>`, `search-no-results`.

mod apps;
mod dash;
mod favorites;
mod grid;
mod search;
mod size_bin;
pub mod test_host;
mod tile;

pub use dash::Dash;
pub use grid::AppGrid;
pub use search::SearchResults;

use gtk::gdk;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

thread_local! {
    static LAUNCHED: RefCell<Vec<Rc<dyn Fn()>>> = RefCell::default();
    static CSS_LOADED: Cell<bool> = const { Cell::new(false) };
}

/// Runs `f` after any app is launched (or its window raised) from the dash,
/// the grid or the search results.
pub fn connect_launched(f: impl Fn() + 'static) {
    LAUNCHED.with(|l| l.borrow_mut().push(Rc::new(f)));
}

fn emit_launched() {
    // Clone first: a callback may register another one.
    let callbacks: Vec<_> = LAUNCHED.with(|l| l.borrow().clone());
    for f in callbacks {
        f();
    }
}

fn load_css() {
    if CSS_LOADED.with(|c| c.replace(true)) {
        return;
    }
    let Some(display) = gdk::Display::default() else { return };
    let provider = gtk::CssProvider::new();
    provider.load_from_string(include_str!("style.css"));
    gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
}
