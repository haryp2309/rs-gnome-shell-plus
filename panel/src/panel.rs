//! The top bar window: blurred-wallpaper backdrop when nothing touches the
//! panel, or a solid fill matching the maximized window's header otherwise.

use crate::ipc::{ShellClient, ShellState};
use crate::notifications::Notifications;
use crate::{clock, keyboard, quick_settings, system, wallpaper, workspaces::Workspaces};
use adw::prelude::*;
use gtk::{gdk, glib};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

const FALLBACK_HEIGHT: i32 = 32;
/// Gap between the bottom of a panel button's pill and its menu. Panel
/// buttons end 3px above the panel's bottom edge, and the panel's strut
/// makes Mutter push popups that start inside it down into the work area,
/// which GTK answers by dropping the popover. 6px clears the panel, like
/// GNOME Shell's menus.
const MENU_OFFSET: i32 = 6;
const TRANSITION_MS: u32 = 250;

pub fn build(
    app: &adw::Application,
    client: &Rc<ShellClient>,
    notifications: &Rc<Notifications>,
) -> gtk::ApplicationWindow {
    let client = client.clone();

    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .title("Top Bar")
        .decorated(false)
        .default_width(1280)
        .css_classes(["shellplus-panel"])
        .build();

    let backdrop = gtk::Picture::builder()
        .can_shrink(true)
        .content_fit(gtk::ContentFit::Fill)
        .css_classes(["panel-backdrop"])
        .build();

    let bar = gtk::CenterBox::builder().css_classes(["panel"]).build();
    let workspaces = Workspaces::new(client.clone());
    bar.set_start_widget(Some(workspaces.widget()));
    bar.set_center_widget(Some(&clock::build(notifications)));
    let end = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    end.append(&keyboard::build(&client));
    end.append(&quick_settings::build(client.clone()));
    bar.set_end_widget(Some(&end));

    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&backdrop));
    overlay.add_overlay(&bar);
    overlay.set_measure_overlay(&bar, true);
    window.set_child(Some(&overlay));

    let refresh_backdrop = backdrop_refresher(&window, &backdrop, &client);
    window.connect_map(glib::clone!(
        #[strong]
        refresh_backdrop,
        move |_| refresh_backdrop()
    ));
    window.connect_scale_factor_notify(glib::clone!(
        #[strong]
        refresh_backdrop,
        move |_| refresh_backdrop()
    ));
    watch_wallpaper_settings(&window, refresh_backdrop.clone());

    // Solid color is injected with its own provider so it can change at
    // runtime; CSS transitions in style.css animate the change.
    let color_css = gtk::CssProvider::new();
    gtk::style_context_add_provider_for_display(
        &WidgetExt::display(&window),
        &color_css,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1,
    );
    let fade = adw::TimedAnimation::new(
        &backdrop,
        1.0,
        1.0,
        TRANSITION_MS,
        adw::PropertyAnimationTarget::new(&backdrop, "opacity"),
    );

    let last_monitor = Cell::new(None);
    client.connect_state(move |state: &ShellState| {
        apply_mode(&bar, &color_css, &fade, state);
        workspaces.update(state.active_workspace, state.n_workspaces);
        if last_monitor.replace(state.monitor) != state.monitor {
            refresh_backdrop();
        }
    });

    window
}

/// A popover for a panel menu (date menu, quick settings, …).
pub fn menu_popover(child: &impl IsA<gtk::Widget>) -> gtk::Popover {
    let popover = gtk::Popover::builder()
        .child(child)
        .has_arrow(false)
        .css_classes(["panel-menu"])
        .build();
    popover.set_offset(0, MENU_OFFSET);
    popover
}

fn apply_mode(bar: &gtk::CenterBox, color_css: &gtk::CssProvider, fade: &adw::TimedAnimation, state: &ShellState) {
    let target_opacity = if state.solid { 0.0 } else { 1.0 };

    if state.solid {
        let c = state.color;
        // Relative luminance (sRGB weights) to pick light or dark text.
        let luminance = 0.2126 * c.red() + 0.7152 * c.green() + 0.0722 * c.blue();
        color_css.load_from_string(&format!(".panel.solid {{ background-color: {}; }}", c.to_str()));
        bar.add_css_class("solid");
        if luminance > 0.55 {
            bar.add_css_class("light");
        } else {
            bar.remove_css_class("light");
        }
    } else {
        bar.remove_css_class("solid");
        bar.remove_css_class("light");
    }

    if fade.value_to() != target_opacity {
        fade.set_value_from(fade.widget().opacity());
        fade.set_value_to(target_opacity);
        fade.play();
    }
}

/// Returns a closure that re-renders the blurred wallpaper strip for the
/// current monitor, panel height and scale. Stale renders are discarded.
fn backdrop_refresher(
    window: &gtk::ApplicationWindow,
    backdrop: &gtk::Picture,
    client: &Rc<ShellClient>,
) -> Rc<dyn Fn()> {
    let generation = Rc::new(Cell::new(0u32));
    let last_request = Rc::new(RefCell::new(None::<wallpaper::Request>));

    let weak_window = window.downgrade();
    let weak_backdrop = backdrop.downgrade();
    let client = Rc::downgrade(client);

    Rc::new(move || {
        let (Some(window), Some(backdrop), Some(client)) =
            (weak_window.upgrade(), weak_backdrop.upgrade(), client.upgrade())
        else {
            return;
        };

        let (monitor_width, monitor_height) = client
            .state()
            .monitor
            .map(|(_, _, w, h)| (w, h))
            .or_else(|| fallback_monitor_size(&window))
            .unwrap_or((1920, 1080));
        let height = window.height();
        let request = wallpaper::Request {
            panel_height: if height > 0 { height } else { FALLBACK_HEIGHT },
            monitor_width,
            monitor_height,
            scale: window.scale_factor(),
        };
        last_request.replace(Some(request));

        let this_generation = generation.get().wrapping_add(1);
        generation.set(this_generation);
        let generation = generation.clone();
        wallpaper::render(request, move |texture| {
            if generation.get() == this_generation {
                backdrop.set_paintable(texture.as_ref());
            }
        });
    })
}

fn fallback_monitor_size(window: &gtk::ApplicationWindow) -> Option<(i32, i32)> {
    let monitor = WidgetExt::display(window)
        .monitors()
        .item(0)
        .and_downcast::<gdk::Monitor>()?;
    let geometry = monitor.geometry();
    Some((geometry.width(), geometry.height()))
}

fn watch_wallpaper_settings(window: &gtk::ApplicationWindow, refresh: Rc<dyn Fn()>) {
    let background = system::settings("org.gnome.desktop.background");
    let interface = system::settings("org.gnome.desktop.interface");

    if let Some(settings) = &background {
        let refresh = refresh.clone();
        settings.connect_changed(None, move |_, _| refresh());
    }
    if let Some(settings) = &interface {
        settings.connect_changed(Some("color-scheme"), move |_, _| refresh());
    }

    // Keep the settings objects (and so their signal handlers) alive for as
    // long as the panel exists.
    window.connect_destroy(move |_| {
        let _ = (&background, &interface);
    });
}
