//! GNOME Shell's Activities overview: the window picker with its
//! workspaces, the workspace thumbnails, the search entry, and the dash /
//! app grid / search results from `app_grid`.
//!
//! The compositor can't be asked to draw this, so the panel does: the
//! compositor lists the windows and streams their contents as previews
//! (compositor/src/shellplus-overview.c, `client.rs`), and this module
//! draws everything in one full-screen window titled "Overview", which the
//! compositor stacks under the top bar and gives the keyboard focus.
//!
//! Everything is placed by hand every frame from a few animated values
//! (see `Overview::geometry`):
//!
//! - `progress`: 0 = the desktop (windows at their real places, the
//!   workspace filling the monitor), 1 = the overview. The very first
//!   frame is drawn at 0, so opening looks like the desktop zooming out.
//! - `apps`: 0 = window picker, 1 = app grid (workspace shrunk to a strip).
//! - `searching`: 0..1 cross-fade to the search results.
//! - `ws_offset`: the (fractional) workspace in the middle.
//! - `relayout`: windows moving to new slots after the list changed.

mod client;
mod layout;
mod preview;
mod stage;

use crate::app_grid;
use crate::ipc::{ShellClient, ShellState};
use crate::{system, wallpaper};
use adw::prelude::*;
use client::{Client, Event, WindowInfo};
use gtk::{gdk, glib, gsk};
use layout::{lerp, Boxes, Rect};
use preview::{Preview, ACTIVE_SIZE_INC, CLOSE_SIZE, ICON_OVERLAP, ICON_SIZE, ICON_TITLE_SPACING};
use stage::{place, Stage};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::{Rc, Weak};

/// The compositor recognizes the overview window by this title.
pub const TITLE: &str = "Overview";

/// overview.js ANIMATION_TIME.
const ANIMATION_MS: f64 = 250.0;
/// Show the overview even if some previews haven't arrived by then.
const LOAD_TIMEOUT_MS: u64 = 600;
/// GNOME's dark overview background ($system_bg_color).
const BACKGROUND: (f32, f32, f32) = (0x22 as f32 / 255.0, 0x22 as f32 / 255.0, 0x26 as f32 / 255.0);
const THUMBNAIL_RADIUS: f32 = 6.0;
/// windowPreview.js WINDOW_DND_SIZE and DRAGGING_WINDOW_OPACITY.
const DND_SIZE: f64 = 256.0;
const DND_OPACITY: f64 = 100.0 / 255.0;

#[derive(Clone, Copy, Debug, PartialEq)]
enum State {
    Hidden,
    /// Waiting for the window list and previews before showing anything.
    Loading,
    Opening,
    Shown,
    Closing,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Mode {
    Windows,
    Apps,
}

/// An animated value.
struct Anim {
    value: Rc<Cell<f64>>,
    anim: adw::TimedAnimation,
    settings: Option<gtk::gio::Settings>,
}

impl Anim {
    fn new(widget: &impl IsA<gtk::Widget>, initial: f64, on_change: impl Fn() + 'static) -> Self {
        let value = Rc::new(Cell::new(initial));
        let target = adw::CallbackAnimationTarget::new(glib::clone!(
            #[strong]
            value,
            move |v| {
                value.set(v);
                on_change();
            }
        ));
        let anim = adw::TimedAnimation::builder()
            .widget(widget)
            .value_from(initial)
            .value_to(initial)
            .duration(ANIMATION_MS as u32)
            .easing(adw::Easing::EaseOutQuad)
            .target(&target)
            .build();
        Self {
            value,
            anim,
            settings: system::settings("org.gnome.desktop.interface"),
        }
    }

    fn get(&self) -> f64 {
        self.value.get()
    }

    fn target(&self) -> f64 {
        self.anim.value_to()
    }

    /// Jumps to `v` without animating (and without running on_change).
    fn set(&self, v: f64) {
        self.anim.pause();
        self.anim.set_value_from(v);
        self.anim.set_value_to(v);
        self.value.set(v);
    }

    fn animate(&self, to: f64, slowdown: f64) {
        self.anim.set_value_from(self.get());
        self.anim.set_value_to(to);
        self.anim.set_duration((ANIMATION_MS * slowdown).round() as u32);
        self.anim.play();
        let enabled = self
            .settings
            .as_ref()
            .map(|s| s.boolean("enable-animations"))
            .unwrap_or(true);
        if !enabled {
            self.anim.skip();
        }
    }
}

/// Per-frame geometry, from the animated values.
struct Geometry {
    boxes: Boxes,
    cards: Vec<Rect>,
    card_radius: f64,
    /// How spread out the windows are (0 = real positions, 1 = picker slots).
    spread: f64,
    thumbnails: Vec<Rect>,
    thumbnails_opacity: f64,
    dash: Rect,
}

pub struct Overview {
    this: Weak<Overview>,
    shell: Rc<ShellClient>,
    client: Rc<Client>,
    window: gtk::ApplicationWindow,
    stage: Stage,
    search: gtk::SearchEntry,
    dash: Rc<app_grid::Dash>,
    grid: Rc<app_grid::AppGrid>,
    results: Rc<app_grid::SearchResults>,
    dash_widget: gtk::Widget,
    grid_widget: gtk::Widget,
    results_widget: gtk::Widget,

    state: Cell<State>,
    mode: Cell<Mode>,
    generation: Cell<u32>,
    progress: Anim,
    apps: Anim,
    searching: Anim,
    ws_offset: Anim,
    relayout: Anim,

    /// Windows on the primary monitor, bottom to top.
    windows: RefCell<Vec<WindowInfo>>,
    /// One per window and workspace it's shown on, bottom to top.
    previews: RefCell<Vec<Rc<Preview>>>,
    textures: RefCell<HashMap<u64, gdk::Texture>>,
    in_flight: RefCell<HashSet<u64>>,
    refetch: RefCell<HashMap<u64, (i32, i32)>>,
    /// Pending damage that arrived while animating.
    damaged: RefCell<HashSet<u64>>,
    /// Hit areas of the workspace cards and thumbnails (for the debug
    /// interface's Inspect; input is handled by the stage).
    cards: RefCell<Vec<gtk::Widget>>,
    thumbs: RefCell<Vec<gtk::Widget>>,
    n_workspaces: Cell<usize>,
    active_workspace: Cell<i32>,
    monitor: Cell<Option<(i32, i32, i32, i32)>>,

    wallpaper: RefCell<Option<(gdk::Texture, (i32, i32, i32))>>,
    wallpaper_generation: Cell<u32>,

    hovered: RefCell<Option<Rc<Preview>>>,
    selected: RefCell<Option<Rc<Preview>>>,
    activated: Cell<Option<u64>>,
    on_active: RefCell<Vec<Box<dyn Fn(bool)>>>,
    slowdown: Cell<f64>,
    hot_settings: RefCell<Vec<gtk::gio::Settings>>,
}

pub fn install(app: &adw::Application, shell: &Rc<ShellClient>) -> Rc<Overview> {
    load_css();
    let overview = Rc::new_cyclic(|weak: &Weak<Overview>| Overview::new(app, shell, weak));
    overview.connect_signals();
    overview
}

impl Overview {
    fn new(app: &adw::Application, shell: &Rc<ShellClient>, weak: &Weak<Overview>) -> Self {
        let stage = Stage::new();
        stage.set_widget_name("overview-stage");
        stage.add_css_class("overview-stage");
        stage.set_focusable(true);
        stage.set_overflow(gtk::Overflow::Hidden);

        let window = gtk::ApplicationWindow::builder()
            .application(app)
            .name("overview")
            .title(TITLE)
            .decorated(false)
            .css_classes(["overview"])
            .child(&stage)
            .build();

        let search = gtk::SearchEntry::builder()
            .name("overview-search")
            .placeholder_text("Type to search")
            .css_classes(["overview-search"])
            .build();
        search.set_parent(&stage);

        let dash = app_grid::Dash::new(glib::clone!(
            #[strong]
            weak,
            move || {
                if let Some(overview) = weak.upgrade() {
                    overview.toggle_apps();
                }
            }
        ));
        let grid = app_grid::AppGrid::new();
        let results = app_grid::SearchResults::new();
        let dash_widget = dash.widget();
        let grid_widget = grid.widget();
        let results_widget = results.widget();
        for widget in [&results_widget, &grid_widget, &dash_widget] {
            widget.set_parent(&stage);
        }
        grid_widget.set_visible(false);
        results_widget.set_visible(false);

        let changed = |weak: &Weak<Overview>| {
            let weak = weak.clone();
            move || {
                if let Some(overview) = weak.upgrade() {
                    overview.update();
                }
            }
        };

        let slowdown = std::env::var("SHELLPLUS_SLOW_ANIMATIONS")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|v| *v > 0.0)
            .unwrap_or(1.0);

        Self {
            this: weak.clone(),
            shell: shell.clone(),
            client: Client::new(glib::clone!(
                #[strong]
                weak,
                move |event| {
                    if let Some(overview) = weak.upgrade() {
                        overview.on_event(event);
                    }
                }
            )),
            progress: Anim::new(&stage, 0.0, changed(weak)),
            apps: Anim::new(&stage, 0.0, changed(weak)),
            searching: Anim::new(&stage, 0.0, changed(weak)),
            ws_offset: Anim::new(&stage, 0.0, changed(weak)),
            relayout: Anim::new(&stage, 1.0, changed(weak)),
            window,
            stage,
            search,
            dash,
            grid,
            results,
            dash_widget,
            grid_widget,
            results_widget,
            state: Cell::new(State::Hidden),
            mode: Cell::new(Mode::Windows),
            generation: Cell::new(0),
            windows: RefCell::default(),
            previews: RefCell::default(),
            textures: RefCell::default(),
            in_flight: RefCell::default(),
            refetch: RefCell::default(),
            damaged: RefCell::default(),
            cards: RefCell::default(),
            thumbs: RefCell::default(),
            n_workspaces: Cell::new(0),
            active_workspace: Cell::new(0),
            monitor: Cell::new(None),
            wallpaper: RefCell::default(),
            wallpaper_generation: Cell::new(0),
            hovered: RefCell::default(),
            selected: RefCell::default(),
            activated: Cell::new(None),
            on_active: RefCell::default(),
            slowdown: Cell::new(slowdown),
            hot_settings: RefCell::default(),
        }
    }

    fn connect_signals(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);

        self.stage.set_allocate(glib::clone!(
            #[strong]
            weak,
            move |w, h| {
                if let Some(overview) = weak.upgrade() {
                    overview.allocate(w as f64, h as f64);
                }
            }
        ));
        self.stage.set_draw_below(glib::clone!(
            #[strong]
            weak,
            move |snapshot, w, h| {
                if let Some(overview) = weak.upgrade() {
                    overview.draw(snapshot, w, h);
                }
            }
        ));

        // Pointer: hover highlight, clicks, drag and drop.
        let motion = gtk::EventControllerMotion::new();
        motion.connect_motion(glib::clone!(
            #[strong]
            weak,
            move |_, x, y| {
                if let Some(overview) = weak.upgrade() {
                    overview.pointer_moved(Some((x, y)));
                }
            }
        ));
        motion.connect_leave(glib::clone!(
            #[strong]
            weak,
            move |_| {
                if let Some(overview) = weak.upgrade() {
                    overview.pointer_moved(None);
                }
            }
        ));
        self.stage.add_controller(motion);

        let click = gtk::GestureClick::builder().button(0).build();
        click.connect_released(glib::clone!(
            #[strong]
            weak,
            move |gesture, _, x, y| {
                if let Some(overview) = weak.upgrade() {
                    overview.clicked(gesture.current_button(), x, y);
                }
            }
        ));
        self.stage.add_controller(click);

        let drop = gtk::DropTarget::new(glib::Type::STRING, gdk::DragAction::MOVE);
        drop.connect_drop(glib::clone!(
            #[strong]
            weak,
            move |_, value, x, y| {
                let Some(overview) = weak.upgrade() else { return false };
                let Some(id) = value.get::<String>().ok().and_then(|s| s.parse::<u64>().ok()) else {
                    return false;
                };
                overview.dropped(id, x, y)
            }
        ));
        self.stage.add_controller(drop);

        // Keyboard: Escape, arrow keys between previews, Enter; typing
        // anywhere goes to the search entry.
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        keys.connect_key_pressed(glib::clone!(
            #[strong]
            weak,
            move |_, key, _, _| match weak.upgrade() {
                Some(overview) => overview.key_pressed(key),
                None => glib::Propagation::Proceed,
            }
        ));
        self.window.add_controller(keys);
        self.search.set_key_capture_widget(Some(&self.window));
        self.search.connect_changed(glib::clone!(
            #[strong]
            weak,
            move |_| {
                if let Some(overview) = weak.upgrade() {
                    overview.search_changed();
                }
            }
        ));
        self.search.connect_activate(glib::clone!(
            #[strong]
            weak,
            move |_| {
                if let Some(overview) = weak.upgrade() {
                    if overview.results.activate_first() {
                        overview.hide(None);
                    }
                }
            }
        ));

        self.window.connect_close_request(glib::clone!(
            #[strong]
            weak,
            move |_| {
                if let Some(overview) = weak.upgrade() {
                    overview.hide(None);
                }
                glib::Propagation::Stop
            }
        ));

        self.progress.anim.connect_done(glib::clone!(
            #[strong]
            weak,
            move |_| {
                if let Some(overview) = weak.upgrade() {
                    overview.progress_done();
                }
            }
        ));

        app_grid::connect_launched(glib::clone!(
            #[strong]
            weak,
            move || {
                if let Some(overview) = weak.upgrade() {
                    overview.hide(None);
                }
            }
        ));

        // A new wallpaper or style: render the cards' wallpaper again.
        let mut settings = Vec::new();
        for (schema, key) in [
            ("org.gnome.desktop.background", None),
            ("org.gnome.desktop.interface", Some("color-scheme")),
        ] {
            if let Some(s) = system::settings(schema) {
                s.connect_changed(
                    key,
                    glib::clone!(
                        #[strong]
                        weak,
                        move |_, _| {
                            if let Some(overview) = weak.upgrade() {
                                overview.invalidate_wallpaper();
                            }
                        }
                    ),
                );
                settings.push(s);
            }
        }
        self.hot_settings.replace(settings);

        self.shell.connect_state(glib::clone!(
            #[strong]
            weak,
            move |state| {
                if let Some(overview) = weak.upgrade() {
                    overview.shell_state_changed(state);
                }
            }
        ));

        crate::debug::register(
            "overview-toggle",
            glib::clone!(
                #[strong]
                weak,
                move |_| {
                    if let Some(overview) = weak.upgrade() {
                        overview.request("toggle");
                    }
                }
            ),
        );
        crate::debug::register(
            "overview-show-apps",
            glib::clone!(
                #[strong]
                weak,
                move |_| {
                    if let Some(overview) = weak.upgrade() {
                        overview.request("apps");
                    }
                }
            ),
        );
        crate::debug::register(
            "overview-slowdown",
            glib::clone!(
                #[strong]
                weak,
                move |value| {
                    if let Some(overview) = weak.upgrade() {
                        overview.slowdown.set(if value > 0.0 { value } else { 1.0 });
                    }
                }
            ),
        );
    }

    // --- Public API ------------------------------------------------------

    /// Toggles the window picker (Activities button, hot corner).
    pub fn toggle(self: &Rc<Self>) {
        self.request("toggle");
    }

    /// `f(true)` when the overview starts opening, `f(false)` when it
    /// starts closing (the top bar turns transparent meanwhile).
    pub fn connect_active(&self, f: impl Fn(bool) + 'static) {
        self.on_active.borrow_mut().push(Box::new(f));
    }

    fn notify_active(&self, active: bool) {
        for f in self.on_active.borrow().iter() {
            f(active);
        }
    }

    /// A ShowOverview request: "toggle", "windows" or "apps".
    fn request(self: &Rc<Self>, mode: &str) {
        let wanted = match mode {
            "apps" => Mode::Apps,
            _ => Mode::Windows,
        };
        match self.state.get() {
            State::Hidden | State::Closing => self.show(wanted),
            State::Loading => self.hide(None),
            State::Opening | State::Shown => {
                if mode == "toggle" || self.mode.get() == wanted {
                    self.hide(None);
                } else {
                    self.set_mode(wanted);
                }
            }
        }
    }

    fn toggle_apps(self: &Rc<Self>) {
        match self.state.get() {
            State::Hidden | State::Closing => self.show(Mode::Apps),
            State::Loading => {}
            _ => self.set_mode(if self.mode.get() == Mode::Apps { Mode::Windows } else { Mode::Apps }),
        }
    }

    // --- Showing and hiding ----------------------------------------------

    fn show(self: &Rc<Self>, mode: Mode) {
        if self.state.get() == State::Closing {
            // Turn around mid-animation.
            self.mode.set(mode);
            self.dash.set_show_apps_checked(mode == Mode::Apps);
            self.apps.animate(if mode == Mode::Apps { 1.0 } else { 0.0 }, self.slowdown.get());
            self.start_open();
            return;
        }
        if self.state.get() != State::Hidden {
            return;
        }

        self.state.set(State::Loading);
        let generation = self.generation.get().wrapping_add(1);
        self.generation.set(generation);
        self.mode.set(mode);
        self.apps.set(if mode == Mode::Apps { 1.0 } else { 0.0 });
        self.searching.set(0.0);
        self.progress.set(0.0);
        self.relayout.set(1.0);
        self.ws_offset.set(self.active_workspace.get() as f64);
        self.activated.set(None);
        self.dash.set_show_apps_checked(mode == Mode::Apps);
        self.update_mode_classes();
        self.client.set_overview_active(true);

        let weak = Rc::downgrade(self);
        self.client.list_windows(move |windows| {
            let Some(overview) = weak.upgrade() else { return };
            if overview.state.get() != State::Loading || overview.generation.get() != generation {
                return;
            }
            overview.set_windows(windows.unwrap_or_default(), false);
            overview.load(generation);
        });
    }

    /// Fetches the previews and the wallpaper, then starts opening.
    fn load(self: &Rc<Self>, generation: u32) {
        let scale = self.scale_factor() as f64;
        let monitor = self.monitor_rect();
        let active = self.active_workspace.get();

        // Windows visible on the desktop are drawn at their real size in
        // the first frames: fetch those at full size, the rest at their
        // slot size.
        let mut sizes: Vec<(u64, i32, i32)> = Vec::new();
        for preview in self.previews.borrow().iter() {
            if sizes.iter().any(|(id, _, _)| *id == preview.id) {
                continue;
            }
            let info = preview.info.borrow();
            let on_desktop = (info.workspace == active || info.workspace < 0) && !info.minimized;
            let (w, h) = if on_desktop {
                (info.rect.2 as f64, info.rect.3 as f64)
            } else {
                let slot = preview.slot.get();
                (slot.w, slot.h)
            };
            let w = w.min(monitor.w);
            let h = h.min(monitor.h);
            sizes.push((preview.id, (w * scale).ceil() as i32, (h * scale).ceil() as i32));
        }

        let remaining = Rc::new(Cell::new(sizes.len() + 1));
        let weak = Rc::downgrade(self);
        let done: Rc<dyn Fn()> = Rc::new(move || {
            remaining.set(remaining.get().saturating_sub(1));
            if remaining.get() == 0 {
                if let Some(overview) = weak.upgrade() {
                    overview.loaded(generation);
                }
            }
        });
        for (id, w, h) in sizes {
            self.fetch(id, w, h, Some(done.clone()));
        }
        self.ensure_wallpaper(Some(Box::new(glib::clone!(
            #[strong]
            done,
            move || done()
        ))));

        let weak = Rc::downgrade(self);
        glib::timeout_add_local_once(std::time::Duration::from_millis(LOAD_TIMEOUT_MS), move || {
            if let Some(overview) = weak.upgrade() {
                overview.loaded(generation);
            }
        });
    }

    fn loaded(self: &Rc<Self>, generation: u32) {
        if self.state.get() == State::Loading && self.generation.get() == generation {
            self.start_open();
        }
    }

    fn start_open(self: &Rc<Self>) {
        let reopening = self.state.get() == State::Closing;
        self.state.set(State::Opening);
        self.activated.set(None);
        self.notify_active(true);
        self.update();

        if reopening {
            self.progress.animate(1.0, self.slowdown.get());
            return;
        }

        self.fit_to_monitor();
        self.window.present();
        self.stage.grab_focus();
        // Draw one frame at progress 0 (the desktop), then animate.
        let weak = Rc::downgrade(self);
        let ticks = Cell::new(0);
        self.stage.add_tick_callback(move |_, _| {
            ticks.set(ticks.get() + 1);
            if ticks.get() < 2 {
                return glib::ControlFlow::Continue;
            }
            if let Some(overview) = weak.upgrade() {
                if overview.state.get() == State::Opening {
                    overview.progress.animate(1.0, overview.slowdown.get());
                }
            }
            glib::ControlFlow::Break
        });
    }

    /// Closes the overview; `activated` is the window that was picked (it
    /// is raised above the others and ends up at its real place).
    fn hide(self: &Rc<Self>, activated: Option<u64>) {
        match self.state.get() {
            State::Opening | State::Shown => {}
            State::Loading => {
                self.state.set(State::Hidden);
                self.client.set_overview_active(false);
                return;
            }
            State::Hidden | State::Closing => return,
        }
        self.state.set(State::Closing);
        self.activated.set(activated);
        self.notify_active(false);
        self.set_hovered(None);
        self.selected.replace(None);
        self.refresh_highlight();
        self.restack();

        // The windows end up at their real size: fetch sharp previews of
        // those that will be visible.
        let scale = self.scale_factor() as f64;
        let active = self.active_workspace.get();
        let wanted: Vec<(u64, i32, i32)> = self
            .windows
            .borrow()
            .iter()
            .filter(|w| (w.workspace == active || w.workspace < 0) && (!w.minimized || Some(w.id) == activated))
            .map(|w| (w.id, (w.rect.2 as f64 * scale) as i32, (w.rect.3 as f64 * scale) as i32))
            .filter(|(id, w, _)| {
                self.textures.borrow().get(id).is_none_or(|t| (t.width() as f64) < *w as f64 * 0.9)
            })
            .collect();
        for (id, w, h) in wanted {
            self.fetch(id, w, h, None);
        }

        if self.searching.get() > 0.0 {
            self.searching.animate(0.0, self.slowdown.get());
        }
        self.progress.animate(0.0, self.slowdown.get());
    }

    fn progress_done(self: &Rc<Self>) {
        match self.state.get() {
            State::Opening if self.progress.get() >= 1.0 => {
                self.state.set(State::Shown);
                self.update();
                // Previews at the size they're shown at now.
                let ids: Vec<u64> = self.previews.borrow().iter().map(|p| p.id).collect();
                let mut damaged = self.damaged.take();
                damaged.extend(ids);
                self.refresh_previews(damaged);
            }
            State::Closing if self.progress.get() <= 0.0 => self.finish_close(),
            _ => {}
        }
    }

    fn finish_close(&self) {
        self.state.set(State::Hidden);
        self.window.set_visible(false);
        self.client.set_overview_active(false);
        self.search.set_text("");
        self.searching.set(0.0);
        self.apps.set(0.0);
        self.mode.set(Mode::Windows);
        self.dash.set_show_apps_checked(false);
        self.grid.reset();
        self.activated.set(None);
        self.damaged.borrow_mut().clear();
        self.update_mode_classes();
    }

    fn set_mode(self: &Rc<Self>, mode: Mode) {
        if self.mode.get() == mode {
            return;
        }
        self.mode.set(mode);
        self.search.set_text("");
        self.dash.set_show_apps_checked(mode == Mode::Apps);
        self.set_hovered(None);
        self.selected.replace(None);
        self.refresh_highlight();
        self.apps.animate(if mode == Mode::Apps { 1.0 } else { 0.0 }, self.slowdown.get());
        self.update_mode_classes();
    }

    fn update_mode_classes(&self) {
        let classes: &[&str] = match (self.mode.get(), self.search.text().is_empty()) {
            (_, false) => &["overview", "search-mode"],
            (Mode::Apps, true) => &["overview", "apps-mode"],
            (Mode::Windows, true) => &["overview", "windows-mode"],
        };
        self.window.set_css_classes(classes);
    }

    // --- Compositor events ---------------------------------------------

    fn on_event(self: &Rc<Self>, event: Event) {
        match event {
            Event::ShowOverview(mode) => self.request(&mode),
            Event::WindowsChanged => {
                if matches!(self.state.get(), State::Opening | State::Shown | State::Closing) {
                    let weak = Rc::downgrade(self);
                    self.client.list_windows(move |windows| {
                        if let (Some(overview), Some(windows)) = (weak.upgrade(), windows) {
                            if overview.state.get() != State::Hidden {
                                overview.set_windows(windows, true);
                            }
                        }
                    });
                }
            }
            Event::WindowsDamaged(ids) => match self.state.get() {
                State::Shown => self.refresh_previews(ids),
                State::Opening => self.damaged.borrow_mut().extend(ids),
                _ => {}
            },
        }
    }

    fn shell_state_changed(self: &Rc<Self>, state: &ShellState) {
        if self.monitor.replace(state.monitor) != state.monitor {
            self.fit_to_monitor();
        }
        let n = state.n_workspaces.max(1) as usize;
        if n != self.n_workspaces.get() {
            self.n_workspaces.set(n);
            self.rebuild_workspaces();
        }
        let active = state.active_workspace.clamp(0, n as i32 - 1);
        if self.active_workspace.replace(active) != active {
            if self.state.get() == State::Hidden {
                self.ws_offset.set(active as f64);
            } else {
                self.ws_offset.animate(active as f64, self.slowdown.get());
            }
            self.selected.replace(None);
            self.refresh_highlight();
        }
        self.update();
    }

    fn rebuild_workspaces(&self) {
        let n = self.n_workspaces.get();
        let make = |kind: &str, i: usize| -> gtk::Widget {
            let widget = gtk::Box::builder()
                .name(format!("overview-{kind}-{i}"))
                .can_target(false)
                .build();
            widget.set_parent(&self.stage);
            widget.upcast()
        };
        for list in [&self.cards, &self.thumbs] {
            for widget in list.borrow_mut().drain(..) {
                widget.unparent();
            }
        }
        *self.cards.borrow_mut() = (0..n).map(|i| make("workspace", i)).collect();
        *self.thumbs.borrow_mut() = (0..n).map(|i| make("thumbnail", i)).collect();

        // Sticky windows have a preview on every workspace.
        if self.state.get() != State::Hidden {
            let windows = self.windows.borrow().clone();
            self.set_windows(windows, true);
        } else {
            self.restack();
        }
    }

    // --- Windows and previews --------------------------------------------

    fn set_windows(&self, windows: Vec<WindowInfo>, animate: bool) {
        let monitor = self.monitor_rect();
        let windows: Vec<WindowInfo> = windows
            .into_iter()
            .filter(|w| {
                let r = Rect::new(w.rect.0 as f64, w.rect.1 as f64, w.rect.2 as f64, w.rect.3 as f64);
                let (cx, cy) = r.center();
                monitor.contains(cx, cy)
            })
            .collect();

        let n = self.n_workspaces.get().max(1) as i32;
        let mut old: HashMap<(u64, i32), Rc<Preview>> = self
            .previews
            .borrow_mut()
            .drain(..)
            .map(|p| ((p.id, p.workspace), p))
            .collect();
        let mut previews = Vec::new();
        for info in &windows {
            let workspaces: Vec<i32> = if info.workspace < 0 {
                (0..n).collect()
            } else {
                vec![info.workspace.min(n - 1)]
            };
            for ws in workspaces {
                let preview = match old.remove(&(info.id, ws)) {
                    Some(preview) => {
                        preview.set_info(info);
                        preview
                    }
                    None => self.create_preview(info, ws),
                };
                previews.push(preview);
            }
        }
        for preview in old.into_values() {
            for cell in [&self.hovered, &self.selected] {
                if cell.borrow().as_ref().is_some_and(|p| Rc::ptr_eq(p, &preview)) {
                    cell.replace(None);
                }
            }
            preview.unparent();
        }
        let ids: HashSet<u64> = windows.iter().map(|w| w.id).collect();
        self.textures.borrow_mut().retain(|id, _| ids.contains(id));

        let mut running: Vec<String> = Vec::new();
        for w in &windows {
            if !w.app_id.is_empty() && !running.contains(&w.app_id) {
                running.push(w.app_id.clone());
            }
        }
        self.dash.set_running(&running);

        self.windows.replace(windows);
        self.previews.replace(previews);
        self.compute_slots(animate);
        self.restack();
        self.update();
    }

    fn create_preview(&self, info: &WindowInfo, workspace: i32) -> Rc<Preview> {
        let preview = Rc::new(Preview::new(info, workspace));
        for widget in preview.widgets() {
            widget.set_parent(&self.stage);
        }
        preview.close.set_can_target(false);
        if let Some(texture) = self.textures.borrow().get(&info.id) {
            preview.picture.set_paintable(Some(texture));
        }

        let client = self.client.clone();
        let id = info.id;
        preview.close.connect_clicked(move |_| client.close_window(id));

        // Drag a preview onto a workspace (thumbnail or card).
        let drag = gtk::DragSource::new();
        drag.set_actions(gdk::DragAction::MOVE);
        let weak = Rc::downgrade(&preview);
        let overview = self.this.clone();
        drag.connect_prepare(move |_, _, _| {
            if !overview.upgrade()?.picking_windows() {
                return None;
            }
            let preview = weak.upgrade()?;
            Some(gdk::ContentProvider::for_value(&preview.id.to_string().to_value()))
        });
        let weak = Rc::downgrade(&preview);
        let overview = self.this.clone();
        drag.connect_drag_begin(move |_, drag| {
            let (Some(preview), Some(overview)) = (weak.upgrade(), overview.upgrade()) else { return };
            // Like windowPreview.js: a translucent copy of the window, at
            // most WINDOW_DND_SIZE big, follows the pointer.
            let (w, h) = (preview.picture.width() as f64, preview.picture.height() as f64);
            let scale = (DND_SIZE / w.max(h).max(1.0)).min(1.0);
            let (w, h) = ((w * scale).round() as i32, (h * scale).round() as i32);
            let icon = gtk::Picture::builder()
                .can_shrink(true)
                .content_fit(gtk::ContentFit::Fill)
                .width_request(w)
                .height_request(h)
                .opacity(DND_OPACITY)
                .build();
            icon.set_paintable(overview.textures.borrow().get(&preview.id));
            gtk::DragIcon::for_drag(drag).set_child(Some(&icon));
            drag.set_hotspot(w / 2, h / 2);
            preview.dragging.set(true);
            overview.update();
        });
        let weak = Rc::downgrade(&preview);
        let overview = self.this.clone();
        drag.connect_drag_end(move |_, _, _| {
            if let Some(preview) = weak.upgrade() {
                preview.dragging.set(false);
            }
            if let Some(overview) = overview.upgrade() {
                overview.update();
            }
        });
        preview.picture.add_controller(drag);
        preview
    }

    /// Picker slots for every preview, relative to the centered card.
    fn compute_slots(&self, animate: bool) {
        let boxes = self.boxes();
        let area = layout::picker_area(&boxes.picker);
        let monitor = self.monitor_rect();
        let relayout = self.relayout.get();
        let visible = self.state.get() != State::Hidden && self.state.get() != State::Loading;
        let previews = self.previews.borrow();

        let mut changed = false;
        for ws in 0..self.n_workspaces.get() as i32 {
            let on_ws: Vec<&Rc<Preview>> = previews.iter().filter(|p| p.workspace == ws).collect();
            let rects: Vec<Rect> = on_ws.iter().map(|p| local_rect(&p.info.borrow(), &monitor)).collect();
            let slots = layout::window_picker_slots(&rects, &area, monitor.h);
            for (preview, slot) in on_ws.into_iter().zip(slots) {
                let current = preview.from_slot.get().lerp(preview.slot.get(), relayout);
                let is_new = preview.slot.get() == Rect::default();
                if animate && visible && !is_new {
                    if current != slot {
                        changed = true;
                    }
                    preview.from_slot.set(current);
                } else {
                    preview.from_slot.set(slot);
                }
                preview.slot.set(slot);
            }
        }
        drop(previews);
        if changed {
            self.relayout.set(0.0);
            self.relayout.animate(1.0, self.slowdown.get());
        } else if !animate {
            self.relayout.set(1.0);
        }
    }

    /// Stacking order of the stage's children.
    fn restack(&self) {
        let previews = self.previews.borrow();
        let activated = self.activated.get();
        for card in self.cards.borrow().iter() {
            self.stage.raise(card);
        }
        for preview in previews.iter().filter(|p| Some(p.id) != activated) {
            self.stage.raise(&preview.picture);
        }
        for preview in previews.iter().filter(|p| Some(p.id) == activated) {
            self.stage.raise(&preview.picture);
        }
        for thumb in self.thumbs.borrow().iter() {
            self.stage.raise(thumb);
        }
        for preview in previews.iter() {
            self.stage.raise(&preview.icon);
            self.stage.raise(&preview.title);
            self.stage.raise(&preview.close);
        }
        self.stage.raise(&self.search);
        self.stage.raise(&self.results_widget);
        self.stage.raise(&self.grid_widget);
        self.stage.raise(&self.dash_widget);
    }

    /// Fetches a preview of `id` at (physical) w x h; `done` runs when the
    /// request finished, successfully or not.
    fn fetch(&self, id: u64, w: i32, h: i32, done: Option<Rc<dyn Fn()>>) {
        if !self.in_flight.borrow_mut().insert(id) {
            self.refetch.borrow_mut().insert(id, (w, h));
            if let Some(done) = done {
                done();
            }
            return;
        }
        let weak = self.stage.downgrade();
        let overview = self.this.clone();
        self.client.get_preview(id, w, h, move |texture| {
            let Some(overview) = overview.upgrade() else { return };
            overview.in_flight.borrow_mut().remove(&id);
            if let Some(texture) = texture {
                overview.set_texture(id, texture);
            }
            if let Some(done) = done {
                done();
            }
            let again = overview.refetch.borrow_mut().remove(&id);
            if let Some((w, h)) = again {
                overview.fetch(id, w, h, None);
            }
            if let Some(stage) = weak.upgrade() {
                stage.queue_draw();
            }
        });
    }

    fn set_texture(&self, id: u64, texture: gdk::Texture) {
        for preview in self.previews.borrow().iter().filter(|p| p.id == id) {
            preview.picture.set_paintable(Some(&texture));
        }
        self.textures.borrow_mut().insert(id, texture);
    }

    /// New previews of the damaged windows that are on screen, at the
    /// size they're shown at.
    fn refresh_previews(&self, ids: impl IntoIterator<Item = u64>) {
        let screen = self.boxes().screen;
        let scale = self.scale_factor() as f64;
        let searching = self.searching.get() >= 1.0;
        let mut sizes: HashMap<u64, (f64, f64)> = HashMap::new();
        {
            let previews = self.previews.borrow();
            for id in ids {
                for preview in previews.iter().filter(|p| p.id == id) {
                    let rect = preview.rect.get();
                    if searching || !rect.intersects(&screen) || rect.w < 1.0 {
                        continue;
                    }
                    let entry = sizes.entry(id).or_insert((0.0, 0.0));
                    entry.0 = entry.0.max(rect.w);
                    entry.1 = entry.1.max(rect.h);
                }
            }
        }
        for (id, (w, h)) in sizes {
            self.fetch(id, (w * scale).ceil() as i32, (h * scale).ceil() as i32, None);
        }
    }

    // --- Wallpaper -------------------------------------------------------

    fn ensure_wallpaper(&self, done: Option<Box<dyn FnOnce()>>) {
        let monitor = self.monitor_rect();
        let key = (monitor.w as i32, monitor.h as i32, self.scale_factor());
        if self.wallpaper.borrow().as_ref().is_some_and(|(_, k)| *k == key) {
            if let Some(done) = done {
                done();
            }
            return;
        }
        let generation = self.wallpaper_generation.get().wrapping_add(1);
        self.wallpaper_generation.set(generation);
        let overview = self.this.clone();
        wallpaper::render_desktop(key.0, key.1, key.2, move |texture| {
            if let Some(overview) = overview.upgrade() {
                if overview.wallpaper_generation.get() == generation {
                    overview.wallpaper.replace(texture.map(|t| (t, key)));
                    overview.stage.queue_draw();
                }
            }
            if let Some(done) = done {
                done();
            }
        });
    }

    fn invalidate_wallpaper(&self) {
        self.wallpaper.replace(None);
        if self.state.get() != State::Hidden {
            self.ensure_wallpaper(None);
        } else {
            self.wallpaper_generation.set(self.wallpaper_generation.get().wrapping_add(1));
        }
    }

    // --- Geometry --------------------------------------------------------

    /// The primary monitor in stage coordinates.
    fn monitor_rect(&self) -> Rect {
        let (x, y, w, h) = self
            .monitor
            .get()
            .or_else(|| {
                let monitor = gdk::Display::default()?.monitors().item(0)?.downcast::<gdk::Monitor>().ok()?;
                let g = monitor.geometry();
                Some((g.x(), g.y(), g.width(), g.height()))
            })
            .unwrap_or((0, 0, 1920, 1080));
        Rect::new(x as f64, y as f64, w as f64, h as f64)
    }

    fn scale_factor(&self) -> i32 {
        if self.window.is_realized() {
            return self.window.scale_factor();
        }
        gdk::Display::default()
            .and_then(|d| d.monitors().item(0))
            .and_then(|m| m.downcast::<gdk::Monitor>().ok())
            .map(|m| m.scale_factor())
            .unwrap_or(1)
    }

    fn fit_to_monitor(&self) {
        let monitor = self.monitor_rect();
        self.stage.set_size_request(monitor.w as i32, monitor.h as i32);
    }

    fn dash_size(&self, width: f64) -> (f64, f64) {
        let (_, nat_w, _, _) = self.dash_widget.measure(gtk::Orientation::Horizontal, -1);
        let w = (nat_w as f64).min(width);
        let (_, nat_h, _, _) = self.dash_widget.measure(gtk::Orientation::Vertical, w as i32);
        (w, nat_h as f64)
    }

    fn boxes(&self) -> Boxes {
        let (w, h) = if self.stage.width() > 0 {
            (self.stage.width() as f64, self.stage.height() as f64)
        } else {
            let m = self.monitor_rect();
            (m.w, m.h)
        };
        Boxes::compute(w, h, self.dash_size(w), self.n_workspaces.get())
    }

    fn geometry(&self, w: f64, h: f64) -> Geometry {
        let n = self.n_workspaces.get();
        let boxes = Boxes::compute(w, h, self.dash_size(w), n);
        let p = self.progress.get();
        let a = self.apps.get();
        let offset = self.ws_offset.get();

        // Above the app grid all workspaces are shown side by side,
        // centered, shrunk if they don't fit.
        let strip_fit = {
            let total = n as f64 * boxes.strip.w + n.saturating_sub(1) as f64 * layout::SPACING;
            (((w - 2.0 * layout::SPACING) / total).min(1.0), total)
        };
        let cards = (0..n)
            .map(|i| {
                let d = i as f64 - offset;
                let picker = boxes
                    .picker
                    .translate(d * (boxes.picker.w + layout::WORKSPACE_SPACING), 0.0)
                    .scale_centered(lerp(1.0, layout::INACTIVE_SCALE, d.abs().min(1.0)));
                let (fit, total) = strip_fit;
                let strip = Rect::new(
                    (w - total * fit) / 2.0 + i as f64 * (boxes.strip.w + layout::SPACING) * fit,
                    boxes.strip.y + boxes.strip.h * (1.0 - fit) / 2.0,
                    boxes.strip.w * fit,
                    boxes.strip.h * fit,
                );
                let full = boxes.screen.translate(d * (boxes.screen.w + layout::WORKSPACE_SPACING), 0.0);
                full.lerp(picker.lerp(strip, a), p)
            })
            .collect();
        let thumbnails = (0..n).map(|i| boxes.thumbnail(i, n)).collect();
        let dash = boxes.dash.translate(0.0, (1.0 - p) * (boxes.dash.h + layout::DASH_BOTTOM_MARGIN));
        let card_radius = p * lerp(layout::CARD_RADIUS, 8.0, a);
        Geometry {
            boxes,
            cards,
            card_radius,
            spread: p * (1.0 - a),
            thumbnails,
            thumbnails_opacity: if n > 1 { p * (1.0 - a) * (1.0 - self.searching.get()) } else { 0.0 },
            dash,
        }
    }

    /// Where the window of `preview` is drawn.
    fn window_rect(&self, geometry: &Geometry, preview: &Preview) -> Option<Rect> {
        let card = *geometry.cards.get(usize::try_from(preview.workspace).ok()?)?;
        let info = preview.info.borrow();
        let natural = local_rect(&info, &self.monitor_rect()).map(&geometry.boxes.screen, &card);
        let slot = preview
            .from_slot
            .get()
            .lerp(preview.slot.get(), self.relayout.get())
            .map(&geometry.boxes.picker, &card);
        let mut rect = natural.lerp(slot, geometry.spread);
        let hover = preview.hover_value();
        if hover > 0.0 {
            let grow = ACTIVE_SIZE_INC * hover;
            rect = rect.inset(-grow, -grow * rect.h / rect.w.max(1.0));
        }
        Some(rect)
    }

    // --- Per-frame updates -----------------------------------------------

    /// Applies the animated values to the widgets' opacity and visibility,
    /// and queues a relayout.
    fn update(&self) {
        let p = self.progress.get();
        let a = self.apps.get();
        let s = self.searching.get();
        let interactive = self.state.get() == State::Shown;

        set_opacity(&self.search, p);
        set_opacity(&self.dash_widget, p);
        set_opacity(&self.grid_widget, p * a * (1.0 - s));
        set_opacity(&self.results_widget, p * s);
        self.grid_widget.set_can_target(interactive || self.state.get() == State::Opening);

        let spread = p * (1.0 - a);
        for preview in self.previews.borrow().iter() {
            let minimized = preview.info.borrow().minimized && self.activated.get() != Some(preview.id);
            let mut opacity = 1.0 - s;
            if minimized {
                opacity *= spread;
            }
            set_opacity(&preview.picture, opacity);
            if preview.dragging.get() {
                // Hidden while dragged, but still mapped: it's the drag source.
                preview.picture.set_opacity(0.0);
            }
            preview.picture.set_can_target(interactive && opacity > 0.0);
            let shown = spread > 0.0 && !preview.dragging.get();
            set_opacity(&preview.icon, (1.0 - s) * if shown { 1.0 } else { 0.0 });
            let hover = preview.hover_value() * (1.0 - s) * spread;
            set_opacity(&preview.title, hover);
            set_opacity(&preview.close, hover);
        }
        self.stage.queue_allocate();
        self.stage.queue_draw();
    }

    fn allocate(&self, w: f64, h: f64) {
        let geometry = self.geometry(w, h);
        let boxes = &geometry.boxes;

        for (card, rect) in self.cards.borrow().iter().zip(&geometry.cards) {
            place(card, rect.x, rect.y, rect.w, rect.h);
        }
        for (thumb, rect) in self.thumbs.borrow().iter().zip(&geometry.thumbnails) {
            place(thumb, rect.x, rect.y, rect.w, rect.h);
        }

        let icon_scale = geometry.spread;
        for preview in self.previews.borrow().iter() {
            let Some(rect) = self.window_rect(&geometry, preview) else {
                preview.rect.set(Rect::default());
                for widget in preview.widgets() {
                    if widget.is_visible() {
                        place(&widget, -1000.0, -1000.0, 0.0, 0.0);
                    }
                }
                continue;
            };
            preview.rect.set(rect);
            place(&preview.picture, rect.x, rect.y, rect.w, rect.h);

            // The icon overlaps the bottom edge; it grows in with the
            // window picker (windowPreview.js _updateIconScale).
            let bottom = rect.y + rect.h;
            let (cx, _) = rect.center();
            let icon = ICON_SIZE * icon_scale;
            if preview.icon.is_visible() {
                let transform = gsk::Transform::new()
                    .translate(&gtk::graphene::Point::new(
                        (cx - icon / 2.0) as f32,
                        (bottom - icon * ICON_OVERLAP) as f32,
                    ))
                    .scale(icon_scale.max(0.001) as f32, icon_scale.max(0.001) as f32);
                preview.icon.allocate(ICON_SIZE as i32, ICON_SIZE as i32, -1, Some(transform));
            }

            if preview.title.is_visible() {
                let (_, nat, _, _) = preview.title.measure(gtk::Orientation::Horizontal, -1);
                let (_, title_h, _, _) = preview.title.measure(gtk::Orientation::Vertical, -1);
                let title_w = (nat as f64).min(rect.w.max(ICON_SIZE * 2.0));
                place(
                    &preview.title,
                    cx - title_w / 2.0,
                    bottom + ICON_SIZE * (1.0 - ICON_OVERLAP) + ICON_TITLE_SPACING,
                    title_w,
                    title_h as f64,
                );
            }
            if preview.close.is_visible() {
                place(
                    &preview.close,
                    rect.x + rect.w - CLOSE_SIZE / 2.0,
                    rect.y - CLOSE_SIZE / 2.0,
                    CLOSE_SIZE,
                    CLOSE_SIZE,
                );
            }
        }

        let (_, search_h, _, _) = self.search.measure(gtk::Orientation::Vertical, boxes.search.w as i32);
        let search_h = (search_h as f64).max(boxes.search.h);
        place(
            &self.search,
            boxes.search.x,
            boxes.search.y + (boxes.search.h - search_h) / 2.0,
            boxes.search.w,
            search_h,
        );
        if self.dash_widget.is_visible() {
            let d = geometry.dash;
            place(&self.dash_widget, d.x, d.y, d.w, d.h);
        }
        if self.grid_widget.is_visible() {
            let g = boxes.grid.translate(0.0, (1.0 - self.progress.get()) * 100.0);
            place(&self.grid_widget, g.x, g.y, g.w, g.h);
        }
        if self.results_widget.is_visible() {
            let r = boxes.results;
            place(&self.results_widget, r.x, r.y, r.w, r.h);
        }
    }

    fn draw(&self, snapshot: &gtk::Snapshot, w: f64, h: f64) {
        let geometry = self.geometry(w, h);
        let screen = geometry.boxes.screen;
        let (r, g, b) = BACKGROUND;
        snapshot.append_color(&gdk::RGBA::new(r, g, b, 1.0), &screen.graphene());

        let wallpaper = self.wallpaper.borrow().as_ref().map(|(t, _)| t.clone());
        let draw_wallpaper = |rect: &Rect| match &wallpaper {
            Some(texture) => snapshot.append_scaled_texture(texture, gsk::ScalingFilter::Trilinear, &rect.graphene()),
            None => snapshot.append_color(&gdk::RGBA::new(0.14, 0.14, 0.16, 1.0), &rect.graphene()),
        };

        // Workspace cards.
        let p = self.progress.get() as f32;
        let cards_opacity = 1.0 - self.searching.get();
        if cards_opacity > 0.0 {
            snapshot.push_opacity(cards_opacity);
            for card in geometry.cards.iter().filter(|c| c.intersects(&screen)) {
                let rounded = gsk::RoundedRect::from_rect(card.graphene(), geometry.card_radius as f32);
                if p > 0.0 {
                    snapshot.append_outset_shadow(
                        &rounded,
                        &gdk::RGBA::new(0.0, 0.0, 0.0, 0.3 * p),
                        0.0,
                        4.0,
                        0.0,
                        16.0,
                    );
                }
                snapshot.push_rounded_clip(&rounded);
                draw_wallpaper(card);
                snapshot.pop();
            }

            // Hover / keyboard highlight around a preview.
            for preview in self.previews.borrow().iter() {
                let hover = preview.hover_value() * geometry.spread;
                if hover <= 0.0 {
                    continue;
                }
                let rect = preview.rect.get().inset(-6.0, -6.0);
                let rounded = gsk::RoundedRect::from_rect(rect.graphene(), 12.0);
                let color = gdk::RGBA::new(1.0, 1.0, 1.0, (0.35 * hover) as f32);
                snapshot.append_border(&rounded, &[4.0; 4], &[color; 4]);
            }
            snapshot.pop();
        }

        // Workspace thumbnails, with the windows at their real places.
        if geometry.thumbnails_opacity > 0.0 {
            snapshot.push_opacity(geometry.thumbnails_opacity);
            let windows = self.windows.borrow();
            let textures = self.textures.borrow();
            let monitor = self.monitor_rect();
            for (i, thumb) in geometry.thumbnails.iter().enumerate() {
                let rounded = gsk::RoundedRect::from_rect(thumb.graphene(), THUMBNAIL_RADIUS);
                snapshot.append_outset_shadow(&rounded, &gdk::RGBA::new(0.0, 0.0, 0.0, 0.25), 0.0, 1.0, 0.0, 4.0);
                snapshot.push_rounded_clip(&rounded);
                draw_wallpaper(thumb);
                for window in windows
                    .iter()
                    .filter(|w| !w.minimized && (w.workspace == i as i32 || w.workspace < 0))
                {
                    let rect = local_rect(window, &monitor).map(&screen, thumb);
                    match textures.get(&window.id) {
                        Some(texture) => {
                            snapshot.append_scaled_texture(texture, gsk::ScalingFilter::Trilinear, &rect.graphene())
                        }
                        None => snapshot.append_color(&gdk::RGBA::new(0.5, 0.5, 0.5, 0.6), &rect.graphene()),
                    }
                }
                snapshot.pop();
            }
            // The active workspace's frame slides between thumbnails.
            let offset = self.ws_offset.get().clamp(0.0, (geometry.thumbnails.len().max(1) - 1) as f64);
            let (lo, hi) = (offset.floor() as usize, offset.ceil() as usize);
            if let (Some(a), Some(b)) = (geometry.thumbnails.get(lo), geometry.thumbnails.get(hi)) {
                let rect = a.lerp(*b, offset - lo as f64).inset(-2.0, -2.0);
                let rounded = gsk::RoundedRect::from_rect(rect.graphene(), THUMBNAIL_RADIUS + 2.0);
                let white = gdk::RGBA::new(1.0, 1.0, 1.0, 1.0);
                snapshot.append_border(&rounded, &[2.0; 4], &[white; 4]);
            }
            snapshot.pop();
        }
    }

    // --- Input -----------------------------------------------------------

    fn preview_at(&self, x: f64, y: f64, with_chrome: bool) -> Option<Rc<Preview>> {
        let previews = self.previews.borrow();
        previews
            .iter()
            .rev()
            .find(|p| {
                let r = p.rect.get();
                let area = if with_chrome {
                    let below = ICON_SIZE * (1.0 - ICON_OVERLAP) + ICON_TITLE_SPACING + 28.0;
                    Rect::new(
                        r.x - CLOSE_SIZE / 2.0,
                        r.y - CLOSE_SIZE / 2.0,
                        r.w + CLOSE_SIZE,
                        r.h + CLOSE_SIZE / 2.0 + below,
                    )
                } else {
                    r
                };
                area.contains(x, y)
            })
            .cloned()
    }

    fn picking_windows(&self) -> bool {
        self.state.get() == State::Shown && self.apps.get() <= 0.0 && self.searching.get() <= 0.0
    }

    fn pointer_moved(&self, position: Option<(f64, f64)>) {
        let hovered = match position {
            Some((x, y)) if self.picking_windows() => self.preview_at(x, y, true),
            _ => None,
        };
        self.set_hovered(hovered);
    }

    fn set_hovered(&self, preview: Option<Rc<Preview>>) {
        let same = match (&*self.hovered.borrow(), &preview) {
            (Some(a), Some(b)) => Rc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        };
        if !same {
            self.hovered.replace(preview);
            self.refresh_highlight();
        }
    }

    /// Shows the hover chrome on the hovered and the keyboard-selected
    /// preview.
    fn refresh_highlight(&self) {
        let hovered = self.hovered.borrow().clone();
        let selected = self.selected.borrow().clone();
        for preview in self.previews.borrow().iter() {
            let on = [&hovered, &selected]
                .iter()
                .any(|p| p.as_ref().is_some_and(|p| Rc::ptr_eq(p, preview)));
            let overview = self.this.clone();
            preview.set_hovered(on, self.slowdown.get(), move || {
                if let Some(overview) = overview.upgrade() {
                    overview.update();
                }
            });
        }
        self.update();
    }

    fn clicked(self: &Rc<Self>, button: u32, x: f64, y: f64) {
        if self.state.get() != State::Shown {
            return;
        }
        let geometry = self.geometry(self.stage.width() as f64, self.stage.height() as f64);
        let boxes = &geometry.boxes;
        let searching = self.searching.get() > 0.0;
        let apps = self.mode.get() == Mode::Apps;

        if boxes.search.contains(x, y) || geometry.dash.contains(x, y) {
            return;
        }
        if searching {
            return;
        }
        if apps && boxes.grid.contains(x, y) {
            return;
        }
        if !apps {
            if let Some(preview) = self.preview_at(x, y, false) {
                match button {
                    2 => self.client.close_window(preview.id),
                    _ => self.activate(&preview),
                }
                return;
            }
            if geometry.thumbnails_opacity > 0.0 {
                if let Some(i) = geometry.thumbnails.iter().position(|t| t.contains(x, y)) {
                    self.shell.activate_workspace(i as i32);
                    return;
                }
            }
        }
        let active = self.active_workspace.get() as usize;
        if let Some(i) = geometry.cards.iter().position(|c| c.contains(x, y)) {
            if i != active {
                self.shell.activate_workspace(i as i32);
                if apps {
                    self.hide(None);
                }
                return;
            }
        }
        // Empty space, or the active workspace's background.
        self.hide(None);
    }

    fn activate(self: &Rc<Self>, preview: &Rc<Preview>) {
        self.client.activate_window(preview.id);
        self.hide(Some(preview.id));
    }

    fn dropped(&self, id: u64, x: f64, y: f64) -> bool {
        if self.state.get() != State::Shown {
            return false;
        }
        let geometry = self.geometry(self.stage.width() as f64, self.stage.height() as f64);
        let target = geometry
            .thumbnails
            .iter()
            .position(|t| geometry.thumbnails_opacity > 0.0 && t.contains(x, y))
            .or_else(|| geometry.cards.iter().position(|c| c.contains(x, y)));
        let Some(target) = target else { return false };
        let current = self.windows.borrow().iter().find(|w| w.id == id).map(|w| w.workspace);
        if current.is_none() || current == Some(target as i32) {
            return false;
        }
        self.client.move_window_to_workspace(id, target as i32);
        true
    }

    fn key_pressed(self: &Rc<Self>, key: gdk::Key) -> glib::Propagation {
        if !matches!(self.state.get(), State::Opening | State::Shown) {
            return glib::Propagation::Proceed;
        }
        match key {
            gdk::Key::Escape => {
                if !self.search.text().is_empty() {
                    self.search.set_text("");
                } else if self.mode.get() == Mode::Apps {
                    self.set_mode(Mode::Windows);
                } else {
                    self.hide(None);
                }
                glib::Propagation::Stop
            }
            gdk::Key::Left | gdk::Key::Right | gdk::Key::Up | gdk::Key::Down | gdk::Key::Tab
            | gdk::Key::ISO_Left_Tab
                if self.search.text().is_empty() && self.mode.get() == Mode::Windows =>
            {
                self.navigate(key);
                glib::Propagation::Stop
            }
            gdk::Key::Return | gdk::Key::KP_Enter | gdk::Key::space
                if self.search.text().is_empty() && self.selected.borrow().is_some() =>
            {
                let selected = self.selected.borrow().clone();
                if let Some(preview) = selected {
                    self.activate(&preview);
                }
                glib::Propagation::Stop
            }
            _ => glib::Propagation::Proceed,
        }
    }

    /// Moves the keyboard selection between the active workspace's
    /// previews, by direction.
    fn navigate(&self, key: gdk::Key) {
        let active = self.active_workspace.get();
        let previews: Vec<Rc<Preview>> = self
            .previews
            .borrow()
            .iter()
            .filter(|p| p.workspace == active)
            .cloned()
            .collect();
        if previews.is_empty() {
            return;
        }
        // Reading order: rows top to bottom, then left to right.
        let mut ordered = previews.clone();
        ordered.sort_by(|a, b| {
            let (sa, sb) = (a.slot.get(), b.slot.get());
            (sa.y + sa.h)
                .total_cmp(&(sb.y + sb.h))
                .then(sa.x.total_cmp(&sb.x))
        });
        let current = self.selected.borrow().clone();
        let next = match current.filter(|c| previews.iter().any(|p| Rc::ptr_eq(p, c))) {
            None => ordered.first().cloned(),
            Some(current) => {
                let index = ordered.iter().position(|p| Rc::ptr_eq(p, &current)).unwrap_or(0);
                match key {
                    gdk::Key::Tab => ordered.get((index + 1) % ordered.len()).cloned(),
                    gdk::Key::ISO_Left_Tab => ordered.get((index + ordered.len() - 1) % ordered.len()).cloned(),
                    _ => {
                        let (dx, dy) = match key {
                            gdk::Key::Left => (-1.0, 0.0),
                            gdk::Key::Right => (1.0, 0.0),
                            gdk::Key::Up => (0.0, -1.0),
                            _ => (0.0, 1.0),
                        };
                        let (cx, cy) = current.slot.get().center();
                        previews
                            .iter()
                            .filter(|p| !Rc::ptr_eq(p, &current))
                            .filter_map(|p| {
                                let (px, py) = p.slot.get().center();
                                let (vx, vy) = (px - cx, py - cy);
                                let along = vx * dx + vy * dy;
                                if along <= 0.0 {
                                    return None;
                                }
                                let across = (vx * dy - vy * dx).abs();
                                Some((along + 2.0 * across, p.clone()))
                            })
                            .min_by(|a, b| a.0.total_cmp(&b.0))
                            .map(|(_, p)| p)
                            .or(Some(current))
                    }
                }
            }
        };
        self.selected.replace(next);
        self.refresh_highlight();
    }

    fn search_changed(&self) {
        let text = self.search.text();
        if text.is_empty() {
            if self.searching.target() != 0.0 {
                self.searching.animate(0.0, self.slowdown.get());
            }
        } else {
            self.results.set_query(&text);
            self.set_hovered(None);
            self.selected.replace(None);
            self.refresh_highlight();
            if self.state.get() == State::Hidden {
                self.searching.set(1.0);
            } else if self.searching.target() != 1.0 {
                self.searching.animate(1.0, self.slowdown.get());
            }
        }
        self.update_mode_classes();
        self.update();
    }
}

/// The window's frame rect relative to the monitor (= overview window).
fn local_rect(info: &WindowInfo, monitor: &Rect) -> Rect {
    Rect::new(
        info.rect.0 as f64 - monitor.x,
        info.rect.1 as f64 - monitor.y,
        info.rect.2.max(1) as f64,
        info.rect.3.max(1) as f64,
    )
}

fn set_opacity(widget: &impl IsA<gtk::Widget>, opacity: f64) {
    let visible = opacity > 0.0;
    if widget.is_visible() != visible {
        widget.set_visible(visible);
    }
    if (widget.opacity() - opacity).abs() > f64::EPSILON {
        widget.set_opacity(opacity);
    }
}

fn load_css() {
    let Some(display) = gdk::Display::default() else { return };
    let provider = gtk::CssProvider::new();
    provider.load_from_string(include_str!("style.css"));
    gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
}
