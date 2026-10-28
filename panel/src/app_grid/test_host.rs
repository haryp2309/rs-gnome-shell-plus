//! Test-only host for the dash, grid and search (PanelDebug.ShowAppsTest,
//! with SHELLPLUS_PANEL_DEBUG=1) until the overview embeds them: a plain
//! dark window with a search entry, the grid or the search results, and the
//! dash at the bottom.
//!
//! For dev/scenarios/app_grid.py it also has `test-running-entry` (comma
//! separated app ids → Dash::set_running), `test-activate-first`
//! (SearchResults::activate_first) and `test-events`, a label counting the
//! callbacks: `show-apps=N launched=N raised=<ids> first=<bool>`.

use super::{connect_launched, AppGrid, Dash, SearchResults};
use gtk::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

#[derive(Default)]
struct Events {
    show_apps: u32,
    launched: u32,
    raised: Vec<String>,
    first: Option<bool>,
}

impl Events {
    fn text(&self) -> String {
        format!(
            "show-apps={} launched={} raised={} first={}",
            self.show_apps,
            self.launched,
            self.raised.join(","),
            self.first.map(|b| b.to_string()).unwrap_or_default()
        )
    }
}

pub fn open() {
    let events = Rc::new(RefCell::new(Events::default()));
    let label = gtk::Label::builder().name("test-events").css_classes(["test-host-events"]).build();
    let update = {
        let (events, label) = (events.clone(), label.clone());
        Rc::new(move |f: &dyn Fn(&mut Events)| {
            f(&mut events.borrow_mut());
            label.set_label(&events.borrow().text());
        })
    };
    update(&|_| {});

    let grid = AppGrid::new();
    let search = SearchResults::new();
    // Like the overview: "Show Apps" shows the grid and checks the button.
    let dash_cell: Rc<RefCell<std::rc::Weak<Dash>>> = Rc::default();
    let u = update.clone();
    let d = dash_cell.clone();
    let dash = Dash::new(move || {
        u(&|e| e.show_apps += 1);
        if let Some(dash) = d.borrow().upgrade() {
            dash.set_show_apps_checked(true);
        }
    });
    *dash_cell.borrow_mut() = Rc::downgrade(&dash);
    let u = update.clone();
    dash.set_activate_running(move |id| {
        let id = id.to_owned();
        u(&move |e| e.raised.push(id.clone()))
    });
    let u = update.clone();
    connect_launched(move || u(&|e| e.launched += 1));

    let entry = gtk::SearchEntry::builder()
        .name("test-search-entry")
        .placeholder_text("Type to search")
        .halign(gtk::Align::Center)
        .width_request(320)
        .css_classes(["test-host-search"])
        .build();
    let stack = gtk::Stack::builder().vexpand(true).hexpand(true).build();
    stack.add_named(&grid.widget(), Some("grid"));
    stack.add_named(&search.widget(), Some("search"));
    {
        let (search, stack, grid) = (search.clone(), stack.clone(), grid.clone());
        entry.connect_search_changed(move |entry| {
            let text = entry.text();
            search.set_query(&text);
            if text.trim().is_empty() {
                grid.reset();
                stack.set_visible_child_name("grid");
            } else {
                stack.set_visible_child_name("search");
            }
        });
    }
    let activate_first = {
        let (search, update) = (search.clone(), update.clone());
        move || {
            let ok = search.activate_first();
            update(&move |e| e.first = Some(ok));
        }
    };
    let a = activate_first.clone();
    entry.connect_activate(move |_| a());

    let running = gtk::Entry::builder().name("test-running-entry").width_chars(24).build();
    {
        let dash = dash.clone();
        running.connect_changed(move |entry| {
            let ids: Vec<String> =
                entry.text().split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned).collect();
            dash.set_running(&ids);
        });
    }
    let first_button = gtk::Button::builder().name("test-activate-first").label("Activate first").build();
    first_button.connect_clicked(move |_| activate_first());

    let controls = gtk::Box::builder().spacing(6).css_classes(["test-host-controls"]).build();
    controls.append(&running);
    controls.append(&first_button);
    controls.append(&label);

    let column = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(12).build();
    column.append(&controls);
    column.append(&entry);
    column.append(&stack);
    column.append(&dash.widget());

    let window = gtk::Window::builder()
        .title("Apps")
        .decorated(false)
        .default_width(1280)
        .default_height(768)
        .child(&column)
        .css_classes(["shellplus-apps-test"])
        .build();
    window.maximize();
    let keep = (grid, search, dash);
    window.connect_close_request(move |_| {
        let _ = &keep;
        gtk::glib::Propagation::Proceed
    });
    window.present();
    entry.grab_focus();
}
