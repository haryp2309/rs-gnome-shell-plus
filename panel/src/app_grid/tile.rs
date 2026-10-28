//! An app icon button, shared by the dash, the grid and the search results:
//! the icon (and name), the running dot, the right-click app menu and a drag
//! source carrying the desktop file id.

use super::apps::{self, App};
use super::favorites;
use gtk::prelude::*;
use gtk::{gdk, gio, glib, pango};

pub struct Tile {
    pub root: gtk::Overlay,
    pub button: gtk::Button,
    pub image: gtk::Image,
    pub dot: gtk::Box,
}

pub struct Options<'a> {
    /// "dash", "grid" or "search": the widget name prefix.
    pub prefix: &'a str,
    pub icon_size: i32,
    pub label: bool,
    pub css_class: &'a str,
    pub menu: bool,
    pub drag: bool,
}

/// A tile for an installed app. Clicking it activates the app.
pub fn app_tile(app: &App, name: &str, options: &Options) -> Tile {
    let tile = build(
        app.icon(),
        &app.id,
        &app.name,
        name,
        options,
    );
    let app_for_click = app.clone();
    tile.button.connect_clicked(move |_| apps::activate(&app_for_click));
    if options.menu {
        attach_menu(&tile, app, options.prefix);
    }
    if options.drag {
        attach_drag(&tile, app);
    }
    tile
}

/// A tile for a running app that has no desktop file.
pub fn window_tile(app_id: &str, options: &Options) -> Tile {
    let has_icon = gdk::Display::default()
        .map(|display| gtk::IconTheme::for_display(&display).has_icon(app_id))
        .unwrap_or(false);
    let icon: gio::Icon =
        gio::ThemedIcon::new(if has_icon { app_id } else { "application-x-executable" }).upcast();
    let tile = build(Some(icon), app_id, app_id, &format!("{}-app-{app_id}", options.prefix), options);
    let id = app_id.to_owned();
    tile.button.connect_clicked(move |_| apps::activate_running_id(&id));
    tile
}

fn build(icon: Option<gio::Icon>, _id: &str, label: &str, name: &str, options: &Options) -> Tile {
    let image = gtk::Image::builder().pixel_size(options.icon_size).css_classes(["icon-dropshadow"]).build();
    match icon {
        Some(icon) => image.set_from_gicon(&icon),
        None => image.set_icon_name(Some("application-x-executable")),
    }
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .build();
    content.append(&image);
    if options.label {
        content.append(
            &gtk::Label::builder()
                .name(format!("{name}-label"))
                .label(label)
                .ellipsize(pango::EllipsizeMode::End)
                .max_width_chars(12)
                .justify(gtk::Justification::Center)
                .css_classes(["app-name"])
                .build(),
        );
    }
    let button = gtk::Button::builder()
        .name(name)
        .child(&content)
        .css_classes([options.css_class])
        .build();
    if !options.label {
        button.set_tooltip_text(Some(label));
    }
    let dot = gtk::Box::builder()
        .name(name.replacen("-app-", "-dot-", 1))
        .css_classes(["app-running-dot"])
        .halign(gtk::Align::Center)
        .valign(gtk::Align::End)
        .can_target(false)
        .visible(false)
        .build();
    let root = gtk::Overlay::builder().child(&button).build();
    root.add_overlay(&dot);
    Tile { root, button, image, dot }
}

fn attach_drag(tile: &Tile, app: &App) {
    let source = gtk::DragSource::builder().actions(gdk::DragAction::COPY | gdk::DragAction::MOVE).build();
    let desktop_id = app.desktop_id();
    source.connect_prepare(move |_, _, _| Some(gdk::ContentProvider::for_value(&desktop_id.to_value())));
    let image = tile.image.clone();
    source.connect_drag_begin(move |source, _| {
        let paintable = gtk::WidgetPaintable::new(Some(&image));
        source.set_icon(Some(&paintable), image.width() / 2, image.height() / 2);
    });
    tile.button.add_controller(source);
}

/// GNOME's app menu: New Window, Add to / Remove from Favorites, App
/// Details. It hangs off an invisible menu button at the top of the tile
/// (named `<prefix>-menu-<id>`, so tests can open it), and opens on
/// right-click or long-press.
fn attach_menu(tile: &Tile, app: &App, prefix: &str) {
    let base = format!("{prefix}-menu-{}", app.id);
    let items = gtk::Box::builder().orientation(gtk::Orientation::Vertical).css_classes(["app-menu"]).build();
    let popover = gtk::Popover::builder()
        .child(&items)
        .has_arrow(true)
        .position(if prefix == "dash" { gtk::PositionType::Top } else { gtk::PositionType::Bottom })
        .css_classes(["app-menu-popover"])
        .build();
    let anchor = gtk::MenuButton::builder()
        .name(&base)
        .popover(&popover)
        .css_classes(["app-menu-anchor"])
        .halign(gtk::Align::Center)
        .valign(if prefix == "dash" { gtk::Align::Start } else { gtk::Align::End })
        .can_target(false)
        .can_focus(false)
        .build();
    tile.root.add_overlay(&anchor);

    let item = |label: &str, suffix: &str| {
        let button = gtk::Button::builder()
            .name(format!("{base}-{suffix}"))
            .child(&gtk::Label::builder().name(format!("{base}-{suffix}-label")).label(label).xalign(0.0).build())
            .css_classes(["flat", "app-menu-item"])
            .build();
        items.append(&button);
        button
    };

    if app.has_new_window_action() {
        let new_window = item("New Window", "new-window");
        let (app, popover) = (app.clone(), popover.clone());
        new_window.connect_clicked(move |_| {
            popover.popdown();
            app.launch(Some("new-window"));
            super::emit_launched();
        });
        items.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    }

    let favorite = item("Add to Favorites", "favorite");
    {
        let (app, popover) = (app.clone(), popover.clone());
        favorite.connect_clicked(move |_| {
            popover.popdown();
            let store = favorites::store();
            let id = app.desktop_id();
            // Change the dash after the popover is gone: it may destroy us.
            glib::idle_add_local_once(move || {
                if store.contains(&id) {
                    store.remove(&id);
                } else {
                    store.add(&id, None);
                }
            });
        });
    }
    if apps::software_installed() {
        let details = item("App Details", "details");
        let (app, popover) = (app.clone(), popover.clone());
        details.connect_clicked(move |_| {
            popover.popdown();
            apps::show_details(&app);
            super::emit_launched();
        });
    }

    let desktop_id = app.desktop_id();
    popover.connect_show(move |_| {
        let label = if favorites::store().contains(&desktop_id) { "Remove from Favorites" } else { "Add to Favorites" };
        if let Some(l) = favorite.child().and_downcast::<gtk::Label>() {
            l.set_label(label);
        }
    });

    let click = gtk::GestureClick::builder().button(gdk::BUTTON_SECONDARY).build();
    let a = anchor.clone();
    click.connect_pressed(move |gesture, _, _, _| {
        gesture.set_state(gtk::EventSequenceState::Claimed);
        a.popup();
    });
    tile.button.add_controller(click);
    let press = gtk::GestureLongPress::new();
    let a = anchor.clone();
    press.connect_pressed(move |gesture, _, _| {
        gesture.set_state(gtk::EventSequenceState::Claimed);
        a.popup();
    });
    tile.button.add_controller(press);
}
