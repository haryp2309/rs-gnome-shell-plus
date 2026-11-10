//! The screenshot UI window: a frozen capture of the primary monitor with
//! GNOME Shell's selection rectangle, window picker and bottom toolbar
//! (mode switcher, capture button, show-pointer toggle, close button).
//! Also the bare variants for org.gnome.Shell.Screenshot's SelectArea
//! (drag a rectangle) and PickColor (click a pixel).
//!
//! The photo/video switch picks screen recording: Selection and Screen then
//! record (crate::screencast) instead of capturing, the capture button
//! turns red, and the frozen image is hidden so the live screen shows.
//!
//! The compositor recognizes the window by its title (`Screenshot`),
//! stretches it over the whole primary monitor above everything, and
//! focuses it.

use super::client::Image;
use super::icons;
use crate::overview::client::WindowInfo;
use crate::overview::layout::{window_picker_slots, Rect};
use adw::prelude::*;
use gtk::{cairo, gdk, glib};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

pub const TITLE: &str = "Screenshot";

/// Handles: drawn radius and how far from a corner a press grabs it.
const HANDLE_RADIUS: f64 = 6.0;
const HANDLE_HIT: f64 = 16.0;
/// Room left for the toolbar below the window thumbnails.
const TOOLBAR_SPACE: f64 = 190.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Selection,
    Screen,
    Window,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// The full UI (Print, the quick settings button, InteractiveScreenshot).
    Screenshot,
    /// org.gnome.Shell.Screenshot.SelectArea: drag a rectangle.
    SelectArea,
    /// org.gnome.Shell.Screenshot.PickColor: click a pixel.
    PickColor,
}

/// Stage-coordinate rectangle (x, y, width, height).
pub type StageRect = (i32, i32, i32, i32);

pub enum Outcome {
    Cancelled,
    Captured { image: Image, rect: StageRect },
    /// Video mode: record this area, with or without the pointer.
    Record { rect: StageRect, cursor: bool },
    Area(StageRect),
    Color((f64, f64, f64)),
}

pub struct WindowShot {
    pub info: WindowInfo,
    pub image: Image,
}

pub struct Params {
    pub kind: Kind,
    pub mode: Mode,
    /// The whole stage without and with the pointer, at `scale`.
    pub plain: Image,
    pub with_pointer: Option<Image>,
    pub scale: f64,
    /// The primary monitor in stage coordinates.
    pub monitor: (i32, i32, i32, i32),
    pub windows: Vec<WindowShot>,
    /// Monitor-relative selection to start with.
    pub selection: Rect,
    /// Start in video (screen recording) mode.
    pub video: bool,
    /// Whether the video mode may be picked at all.
    pub video_allowed: bool,
}

#[derive(Clone, Copy)]
enum Drag {
    /// A new rectangle from this corner.
    New(f64, f64),
    /// Moving the selection; the press was this far into it.
    Move(f64, f64),
    /// Resizing; the opposite corner stays put.
    Corner(f64, f64),
}

pub struct Ui {
    window: gtk::ApplicationWindow,
    area: gtk::DrawingArea,
    fixed: gtk::Fixed,
    toolbar: gtk::Widget,
    mode_buttons: Vec<(Mode, gtk::ToggleButton)>,
    pointer: gtk::ToggleButton,
    capture: gtk::Button,
    cast: gtk::ToggleButton,
    window_buttons: RefCell<Vec<gtk::ToggleButton>>,

    kind: Kind,
    plain: Image,
    with_pointer: Option<Image>,
    plain_surface: Option<cairo::ImageSurface>,
    pointer_surface: Option<cairo::ImageSurface>,
    scale: f64,
    monitor: (i32, i32, i32, i32),
    windows: Vec<WindowShot>,

    mode: Cell<Mode>,
    video: Cell<bool>,
    selection: Cell<Rect>,
    /// SelectArea: whether a rectangle has been dragged yet.
    dragged: Cell<bool>,
    drag: Cell<Option<Drag>>,
    drag_start: Cell<(f64, f64)>,
    on_done: RefCell<Option<Box<dyn FnOnce(&Ui, Outcome)>>>,
}

impl Ui {
    pub fn new(app: &adw::Application, params: Params, on_done: impl FnOnce(&Ui, Outcome) + 'static) -> Rc<Self> {
        icons::install();
        let (mx, my, mw, mh) = params.monitor;

        let area = gtk::DrawingArea::builder()
            .name("screenshot-area")
            .width_request(mw)
            .height_request(mh)
            .build();
        let fixed = gtk::Fixed::builder().name("screenshot-windows").visible(false).build();
        let Toolbar { root: toolbar, mode_buttons, capture, pointer, close, cast } = toolbar();

        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&area));
        overlay.add_overlay(&fixed);
        overlay.add_overlay(&toolbar);

        let window = gtk::ApplicationWindow::builder()
            .application(app)
            .name("screenshot-ui")
            .title(TITLE)
            .decorated(false)
            .css_classes(["screenshot-ui"])
            .child(&overlay)
            .build();

        let plain_surface = params.plain.surface();
        let pointer_surface = params.with_pointer.as_ref().and_then(Image::surface);
        let ui = Rc::new(Self {
            window,
            area,
            fixed,
            toolbar: toolbar.upcast(),
            mode_buttons,
            pointer,
            capture: capture.clone(),
            cast: cast.clone(),
            window_buttons: RefCell::default(),
            kind: params.kind,
            plain: params.plain,
            with_pointer: params.with_pointer,
            plain_surface,
            pointer_surface,
            scale: params.scale,
            monitor: (mx, my, mw, mh),
            windows: params.windows,
            mode: Cell::new(params.mode),
            video: Cell::new(false),
            selection: Cell::new(params.selection),
            dragged: Cell::new(false),
            drag: Cell::new(None),
            drag_start: Cell::new((0.0, 0.0)),
            on_done: RefCell::new(Some(Box::new(on_done))),
        });

        ui.toolbar.set_visible(ui.kind == Kind::Screenshot);
        ui.build_window_picker();
        ui.connect(&capture, &close);
        ui.connect_shot_cast(params.video_allowed);
        ui.set_mode(params.mode);
        if params.video && params.video_allowed {
            ui.cast.set_active(true);
        }
        ui
    }

    pub fn present(&self) {
        self.window.present();
        self.area.grab_focus();
    }

    fn finish(&self, outcome: Outcome) {
        // The handler runs while the window still has the focus (the
        // clipboard needs that), then the window goes.
        if let Some(on_done) = self.on_done.take() {
            on_done(self, outcome);
        }
        self.window.destroy();
    }

    fn connect(self: &Rc<Self>, capture: &gtk::Button, close: &gtk::Button) {
        let weak = Rc::downgrade(self);
        self.area.set_draw_func(glib::clone!(
            #[strong]
            weak,
            move |_, cr, _, _| {
                if let Some(ui) = weak.upgrade() {
                    ui.draw(cr);
                }
            }
        ));

        capture.connect_clicked(glib::clone!(
            #[strong]
            weak,
            move |_| {
                if let Some(ui) = weak.upgrade() {
                    ui.capture();
                }
            }
        ));
        close.connect_clicked(glib::clone!(
            #[strong]
            weak,
            move |_| {
                if let Some(ui) = weak.upgrade() {
                    ui.finish(Outcome::Cancelled);
                }
            }
        ));
        for (mode, button) in &self.mode_buttons {
            let mode = *mode;
            button.connect_toggled(glib::clone!(
                #[strong]
                weak,
                move |button| {
                    if let (true, Some(ui)) = (button.is_active(), weak.upgrade()) {
                        ui.set_mode(mode);
                    }
                }
            ));
        }
        self.pointer.connect_toggled(glib::clone!(
            #[strong]
            weak,
            move |_| {
                if let Some(ui) = weak.upgrade() {
                    ui.area.queue_draw();
                }
            }
        ));

        let drag = gtk::GestureDrag::new();
        drag.connect_drag_begin(glib::clone!(
            #[strong]
            weak,
            move |_, x, y| {
                if let Some(ui) = weak.upgrade() {
                    ui.drag_begin(x, y);
                }
            }
        ));
        drag.connect_drag_update(glib::clone!(
            #[strong]
            weak,
            move |_, dx, dy| {
                if let Some(ui) = weak.upgrade() {
                    ui.drag_update(dx, dy);
                }
            }
        ));
        drag.connect_drag_end(glib::clone!(
            #[strong]
            weak,
            move |_, dx, dy| {
                if let Some(ui) = weak.upgrade() {
                    ui.drag_update(dx, dy);
                    ui.drag_end();
                }
            }
        ));
        self.area.add_controller(drag);

        let motion = gtk::EventControllerMotion::new();
        motion.connect_motion(glib::clone!(
            #[strong]
            weak,
            move |_, x, y| {
                if let Some(ui) = weak.upgrade() {
                    ui.update_cursor(x, y);
                }
            }
        ));
        self.area.add_controller(motion);

        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        keys.connect_key_pressed(glib::clone!(
            #[strong]
            weak,
            move |_, key, _, _| {
                let Some(ui) = weak.upgrade() else { return glib::Propagation::Proceed };
                ui.key_pressed(key)
            }
        ));
        self.window.add_controller(keys);

        self.window.connect_close_request(glib::clone!(
            #[strong]
            weak,
            move |_| {
                if let Some(ui) = weak.upgrade() {
                    ui.finish(Outcome::Cancelled);
                }
                glib::Propagation::Stop
            }
        ));
    }

    /// The photo/video switch: exactly one of the two is active.
    fn connect_shot_cast(self: &Rc<Self>, allowed: bool) {
        self.cast.set_sensitive(allowed);
        if !allowed {
            self.cast.set_tooltip_text(Some("Screen recording is not available"));
        }
        let weak = Rc::downgrade(self);
        self.cast.connect_toggled(glib::clone!(
            #[strong]
            weak,
            move |cast| {
                if let Some(ui) = weak.upgrade() {
                    ui.set_video(cast.is_active());
                }
            }
        ));
    }

    fn set_video(&self, video: bool) {
        if self.video.replace(video) == video {
            return;
        }
        if video {
            self.capture.add_css_class("cast");
            self.capture.set_tooltip_text(Some("Record"));
        } else {
            self.capture.remove_css_class("cast");
            self.capture.set_tooltip_text(Some("Capture"));
        }
        // GNOME can't record a window: that mode is off in video mode.
        let window_ok = !video && !self.windows.is_empty();
        for (m, button) in &self.mode_buttons {
            if *m == Mode::Window {
                button.set_sensitive(window_ok);
            }
        }
        if video && self.mode.get() == Mode::Window {
            self.activate_mode(Mode::Selection);
        }
        self.set_mode(self.mode.get());
    }

    fn key_pressed(&self, key: gdk::Key) -> glib::Propagation {
        let screenshot = self.kind == Kind::Screenshot;
        match key {
            gdk::Key::Escape => self.finish(Outcome::Cancelled),
            gdk::Key::Return | gdk::Key::KP_Enter | gdk::Key::space if screenshot => self.capture(),
            gdk::Key::s | gdk::Key::S if screenshot => self.activate_mode(Mode::Selection),
            gdk::Key::c | gdk::Key::C if screenshot => self.activate_mode(Mode::Screen),
            gdk::Key::w | gdk::Key::W if screenshot && !self.video.get() => self.activate_mode(Mode::Window),
            gdk::Key::v | gdk::Key::V if screenshot && self.cast.is_sensitive() => {
                self.cast.set_active(!self.cast.is_active())
            }
            gdk::Key::p | gdk::Key::P if screenshot => self.pointer.set_active(!self.pointer.is_active()),
            _ => return glib::Propagation::Proceed,
        }
        glib::Propagation::Stop
    }

    fn activate_mode(&self, mode: Mode) {
        if let Some((_, button)) = self.mode_buttons.iter().find(|(m, _)| *m == mode) {
            button.set_active(true);
        }
    }

    fn set_mode(&self, mode: Mode) {
        let mode = if mode == Mode::Window && (self.windows.is_empty() || self.video.get()) {
            Mode::Selection
        } else {
            mode
        };
        self.mode.set(mode);
        for (m, button) in &self.mode_buttons {
            if *m == mode && !button.is_active() {
                button.set_active(true);
            }
        }
        self.fixed.set_visible(self.kind == Kind::Screenshot && mode == Mode::Window);
        self.pointer.set_sensitive(mode != Mode::Window);
        self.area.set_cursor_from_name(Some(match (self.kind, mode) {
            (Kind::Screenshot, Mode::Selection) | (Kind::SelectArea, _) | (Kind::PickColor, _) => "crosshair",
            _ => "default",
        }));
        self.area.queue_draw();
    }

    fn monitor_size(&self) -> (f64, f64) {
        (self.monitor.2 as f64, self.monitor.3 as f64)
    }

    // --- Window picker ----------------------------------------------------

    fn build_window_picker(self: &Rc<Self>) {
        if self.windows.is_empty() {
            self.mode_buttons
                .iter()
                .filter(|(m, _)| *m == Mode::Window)
                .for_each(|(_, b)| b.set_sensitive(false));
            return;
        }
        let (mw, mh) = self.monitor_size();
        let (mx, my) = (self.monitor.0 as f64, self.monitor.1 as f64);
        let rects: Vec<Rect> = self
            .windows
            .iter()
            .map(|w| Rect::new(w.info.rect.0 as f64 - mx, w.info.rect.1 as f64 - my, w.info.rect.2 as f64, w.info.rect.3 as f64))
            .collect();
        let area = Rect::new(48.0, 64.0, (mw - 96.0).max(1.0), (mh - 64.0 - TOOLBAR_SPACE).max(1.0));
        let slots = window_picker_slots(&rects, &area, mh);

        let mut buttons: Vec<gtk::ToggleButton> = Vec::new();
        let weak = Rc::downgrade(self);
        for (i, (shot, slot)) in self.windows.iter().zip(slots).enumerate() {
            let picture = gtk::Picture::for_paintable(&shot.image.texture());
            picture.set_content_fit(gtk::ContentFit::Contain);
            picture.set_can_shrink(true);
            let button = gtk::ToggleButton::builder()
                .name(format!("screenshot-window-{i}"))
                .tooltip_text(shot.info.title.as_str())
                .css_classes(["screenshot-ui-window"])
                .child(&picture)
                .build();
            // The border sits outside the window image.
            let pad = 6.0;
            button.set_size_request((slot.w + 2.0 * pad) as i32, (slot.h + 2.0 * pad) as i32);
            // Exactly one is selected (a group of one would let a click
            // unselect it).
            button.connect_toggled(glib::clone!(
                #[strong]
                weak,
                move |button| {
                    let Some(ui) = weak.upgrade() else { return };
                    let buttons = ui.window_buttons.borrow();
                    if button.is_active() {
                        buttons.iter().filter(|b| *b != button).for_each(|b| b.set_active(false));
                    } else if !buttons.iter().any(|b| b.is_active()) {
                        button.set_active(true);
                    }
                }
            ));
            let double = gtk::GestureClick::new();
            double.connect_pressed(glib::clone!(
                #[strong]
                weak,
                move |_, n, _, _| {
                    if n == 2 {
                        if let Some(ui) = weak.upgrade() {
                            ui.capture();
                        }
                    }
                }
            ));
            button.add_controller(double);
            self.fixed.put(&button, slot.x - pad, slot.y - pad);
            buttons.push(button);
        }
        // GNOME selects the window that had the focus (else the topmost).
        let focused = self.windows.iter().position(|w| w.info.focused).unwrap_or(self.windows.len() - 1);
        self.window_buttons.replace(buttons);
        let selected = self.window_buttons.borrow()[focused].clone();
        selected.set_active(true);
    }

    fn selected_window(&self) -> Option<&WindowShot> {
        let buttons = self.window_buttons.borrow();
        let index = buttons.iter().position(|b| b.is_active())?;
        self.windows.get(index)
    }

    // --- Selection --------------------------------------------------------

    fn corners(r: &Rect) -> [(f64, f64); 4] {
        [(r.x, r.y), (r.x + r.w, r.y), (r.x, r.y + r.h), (r.x + r.w, r.y + r.h)]
    }

    fn hit_corner(&self, x: f64, y: f64) -> Option<usize> {
        let r = self.selection.get();
        Self::corners(&r)
            .iter()
            .position(|(cx, cy)| (x - cx).abs() <= HANDLE_HIT && (y - cy).abs() <= HANDLE_HIT)
    }

    fn update_cursor(&self, x: f64, y: f64) {
        if self.drag.get().is_some() || self.kind != Kind::Screenshot || self.mode.get() != Mode::Selection {
            return;
        }
        let name = match self.hit_corner(x, y) {
            Some(0) => "nw-resize",
            Some(1) => "ne-resize",
            Some(2) => "sw-resize",
            Some(_) => "se-resize",
            None if self.selection.get().contains(x, y) => "move",
            None => "crosshair",
        };
        self.area.set_cursor_from_name(Some(name));
    }

    fn drag_begin(&self, x: f64, y: f64) {
        self.drag_start.set((x, y));
        match (self.kind, self.mode.get()) {
            (Kind::PickColor, _) => {
                let px = ((self.monitor.0 as f64 + x) * self.scale).floor() as i32;
                let py = ((self.monitor.1 as f64 + y) * self.scale).floor() as i32;
                let color = self.plain.pixel(px, py);
                self.finish(Outcome::Color(color));
            }
            (Kind::SelectArea, _) => self.drag.set(Some(Drag::New(x, y))),
            (Kind::Screenshot, Mode::Selection) => {
                let r = self.selection.get();
                let drag = if let Some(i) = self.hit_corner(x, y) {
                    let opposite = Self::corners(&r)[3 - i];
                    Drag::Corner(opposite.0, opposite.1)
                } else if r.contains(x, y) {
                    Drag::Move(x - r.x, y - r.y)
                } else {
                    Drag::New(x, y)
                };
                self.drag.set(Some(drag));
            }
            _ => {}
        }
    }

    fn drag_update(&self, dx: f64, dy: f64) {
        let Some(drag) = self.drag.get() else { return };
        let (mw, mh) = self.monitor_size();
        let (sx, sy) = self.drag_start.get();
        let (x, y) = ((sx + dx).clamp(0.0, mw), (sy + dy).clamp(0.0, mh));
        let rect = match drag {
            Drag::New(ax, ay) | Drag::Corner(ax, ay) => {
                Rect::new(ax.min(x).round(), ay.min(y).round(), (ax - x).abs().round(), (ay - y).abs().round())
            }
            Drag::Move(ox, oy) => {
                let r = self.selection.get();
                Rect::new(
                    (sx + dx - ox).clamp(0.0, mw - r.w).round(),
                    (sy + dy - oy).clamp(0.0, mh - r.h).round(),
                    r.w,
                    r.h,
                )
            }
        };
        if let Drag::New(..) = drag {
            if rect.w < 1.0 && rect.h < 1.0 && !self.dragged.get() && self.kind == Kind::Screenshot {
                // A click, not a drag (yet): keep the old selection.
                return;
            }
        }
        self.selection.set(rect);
        self.dragged.set(true);
        self.area.queue_draw();
    }

    fn drag_end(&self) {
        let Some(drag) = self.drag.take() else { return };
        let r = self.selection.get();
        match (self.kind, drag) {
            (Kind::SelectArea, _) => {
                if r.w >= 1.0 && r.h >= 1.0 && self.dragged.get() {
                    let rect = self.stage_rect(&r);
                    self.finish(Outcome::Area(rect));
                }
            }
            (Kind::Screenshot, _) if r.w < 1.0 || r.h < 1.0 => {
                // Too small to capture: GNOME keeps a minimal selection.
                let (mw, mh) = self.monitor_size();
                self.selection.set(Rect::new(r.x.min(mw - 1.0), r.y.min(mh - 1.0), r.w.max(1.0), r.h.max(1.0)));
                self.area.queue_draw();
            }
            _ => {}
        }
    }

    pub fn selection(&self) -> Rect {
        self.selection.get()
    }

    fn stage_rect(&self, r: &Rect) -> StageRect {
        (
            self.monitor.0 + r.x.round() as i32,
            self.monitor.1 + r.y.round() as i32,
            r.w.round().max(1.0) as i32,
            r.h.round().max(1.0) as i32,
        )
    }

    // --- Capture ----------------------------------------------------------

    fn capture(&self) {
        if self.kind != Kind::Screenshot {
            return;
        }
        let (mw, mh) = self.monitor_size();
        let rect = match self.mode.get() {
            Mode::Window => {
                let Some(shot) = self.selected_window() else {
                    glib::g_warning!("shellplus", "screenshot: no window selected");
                    return;
                };
                let image = shot.image.clone();
                let rect = shot.info.rect;
                self.finish(Outcome::Captured { image, rect });
                return;
            }
            Mode::Screen => Rect::new(0.0, 0.0, mw, mh),
            Mode::Selection => self.selection.get(),
        };
        if self.video.get() {
            let rect = self.stage_rect(&rect);
            self.finish(Outcome::Record { rect, cursor: self.pointer.is_active() });
            return;
        }
        let frame = match (&self.with_pointer, self.pointer.is_active()) {
            (Some(with_pointer), true) => with_pointer,
            _ => &self.plain,
        };
        let stage = self.stage_rect(&rect);
        let s = self.scale;
        let x = (stage.0 as f64 * s).round() as i32;
        let y = (stage.1 as f64 * s).round() as i32;
        let image = frame.crop(x, y, (stage.2 as f64 * s).round() as i32, (stage.3 as f64 * s).round() as i32);
        self.finish(Outcome::Captured { image, rect: stage });
    }

    // --- Drawing ----------------------------------------------------------

    fn draw(&self, cr: &cairo::Context) {
        let (mw, mh) = self.monitor_size();
        let surface = match (&self.pointer_surface, self.pointer.is_active() && self.mode.get() != Mode::Window) {
            // Video mode shows the live screen (the window is transparent).
            _ if self.video.get() => None,
            (Some(with_pointer), true) => Some(with_pointer),
            _ => self.plain_surface.as_ref(),
        };
        if let Some(surface) = surface {
            cr.save().ok();
            cr.scale(1.0 / self.scale, 1.0 / self.scale);
            cr.set_source_surface(surface, -self.monitor.0 as f64 * self.scale, -self.monitor.1 as f64 * self.scale)
                .ok();
            cr.source().set_filter(cairo::Filter::Good);
            cr.paint().ok();
            cr.restore().ok();
        }

        let dim = |cr: &cairo::Context, hole: Option<Rect>, alpha: f64| {
            cr.set_fill_rule(cairo::FillRule::EvenOdd);
            cr.rectangle(0.0, 0.0, mw, mh);
            if let Some(r) = hole {
                cr.rectangle(r.x, r.y, r.w, r.h);
            }
            cr.set_source_rgba(0.0, 0.0, 0.0, alpha);
            cr.fill().ok();
        };

        match (self.kind, self.mode.get()) {
            (Kind::Screenshot, Mode::Selection) => {
                let r = self.selection.get();
                dim(cr, Some(r), 0.5);
                self.draw_selection(cr, &r, true);
            }
            (Kind::Screenshot, Mode::Window) => dim(cr, None, 0.6),
            (Kind::Screenshot, Mode::Screen) => {
                // A thin outline marks the captured monitor.
                cr.set_source_rgba(1.0, 1.0, 1.0, 0.6);
                cr.set_line_width(2.0);
                cr.rectangle(1.0, 1.0, mw - 2.0, mh - 2.0);
                cr.stroke().ok();
            }
            (Kind::SelectArea, _) => {
                if self.dragged.get() {
                    let r = self.selection.get();
                    dim(cr, Some(r), 0.4);
                    self.draw_selection(cr, &r, false);
                } else {
                    dim(cr, None, 0.25);
                }
            }
            (Kind::PickColor, _) => {}
        }
    }

    fn draw_selection(&self, cr: &cairo::Context, r: &Rect, handles: bool) {
        cr.set_source_rgba(1.0, 1.0, 1.0, 1.0);
        cr.set_line_width(2.0);
        cr.rectangle(r.x - 1.0, r.y - 1.0, r.w + 2.0, r.h + 2.0);
        cr.stroke().ok();
        if !handles {
            return;
        }
        for (x, y) in Self::corners(r) {
            cr.new_sub_path();
            cr.arc(x, y + 1.0, HANDLE_RADIUS + 1.0, 0.0, std::f64::consts::TAU);
            cr.set_source_rgba(0.0, 0.0, 0.0, 0.25);
            cr.fill().ok();
            cr.arc(x, y, HANDLE_RADIUS, 0.0, std::f64::consts::TAU);
            cr.set_source_rgba(1.0, 1.0, 1.0, 1.0);
            cr.fill().ok();
        }
    }
}

struct Toolbar {
    root: gtk::Box,
    mode_buttons: Vec<(Mode, gtk::ToggleButton)>,
    capture: gtk::Button,
    pointer: gtk::ToggleButton,
    close: gtk::Button,
    cast: gtk::ToggleButton,
}

/// GNOME Shell's screenshot panel: mode buttons on top; the pointer
/// toggle, the round capture button and the screenshot / screencast
/// switch below; a round close button on the top-right corner.
fn toolbar() -> Toolbar {
    let modes = gtk::Box::builder()
        .spacing(12)
        .halign(gtk::Align::Center)
        .homogeneous(true)
        .css_classes(["screenshot-ui-type-buttons"])
        .build();
    let mut mode_buttons: Vec<(Mode, gtk::ToggleButton)> = Vec::new();
    for (mode, name, icon, label) in [
        (Mode::Selection, "selection", icons::AREA, "Selection"),
        (Mode::Screen, "screen", icons::DISPLAY, "Screen"),
        (Mode::Window, "window", icons::WINDOW, "Window"),
    ] {
        let content = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(6).build();
        content.append(&gtk::Image::builder().icon_name(icon).pixel_size(24).build());
        content.append(&gtk::Label::new(Some(label)));
        let button = gtk::ToggleButton::builder()
            .name(format!("screenshot-mode-{name}"))
            .css_classes(["screenshot-ui-type-button"])
            .child(&content)
            .build();
        if let Some((_, first)) = mode_buttons.first() {
            button.set_group(Some(first));
        }
        modes.append(&button);
        mode_buttons.push((mode, button));
    }

    let pointer = gtk::ToggleButton::builder()
        .name("screenshot-pointer")
        .icon_name(icons::POINTER)
        .tooltip_text("Show Pointer")
        .valign(gtk::Align::Center)
        .halign(gtk::Align::Start)
        .css_classes(["screenshot-ui-show-pointer-button"])
        .build();

    let capture = gtk::Button::builder()
        .name("screenshot-capture")
        .tooltip_text("Capture")
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .css_classes(["screenshot-ui-capture-button"])
        .child(&gtk::Box::builder().css_classes(["screenshot-ui-capture-button-circle"]).build())
        .build();

    // Screenshot / screencast.
    let shot_cast = gtk::Box::builder()
        .halign(gtk::Align::End)
        .valign(gtk::Align::Center)
        .css_classes(["screenshot-ui-shot-cast-container"])
        .build();
    let shot = gtk::ToggleButton::builder()
        .name("screenshot-shot")
        .icon_name("camera-photo-symbolic")
        .tooltip_text("Screenshot")
        .active(true)
        .can_focus(false)
        .css_classes(["screenshot-ui-shot-cast-button"])
        .build();
    let cast = gtk::ToggleButton::builder()
        .name("screenshot-cast")
        .icon_name("camera-web-symbolic")
        .tooltip_text("Screencast")
        .can_focus(false)
        .group(&shot)
        .css_classes(["screenshot-ui-shot-cast-button"])
        .build();
    shot_cast.append(&shot);
    shot_cast.append(&cast);

    let bottom = gtk::CenterBox::builder().css_classes(["screenshot-ui-bottom-row"]).build();
    bottom.set_start_widget(Some(&pointer));
    bottom.set_center_widget(Some(&capture));
    bottom.set_end_widget(Some(&shot_cast));

    let panel = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(18)
        // Room for the close button on the top-right corner.
        .margin_top(18)
        .margin_start(18)
        .margin_end(18)
        .css_classes(["screenshot-ui-panel"])
        .build();
    panel.append(&modes);
    panel.append(&bottom);

    let close = gtk::Button::builder()
        .name("screenshot-close")
        .icon_name("window-close-symbolic")
        .tooltip_text("Close")
        .halign(gtk::Align::End)
        .valign(gtk::Align::Start)
        .css_classes(["screenshot-ui-close-button"])
        .build();

    let corner = gtk::Overlay::new();
    corner.set_child(Some(&panel));
    corner.add_overlay(&close);

    let root = gtk::Box::builder()
        .name("screenshot-toolbar")
        .halign(gtk::Align::Center)
        .valign(gtk::Align::End)
        .margin_bottom(28)
        .build();
    root.append(&corner);
    Toolbar { root, mode_buttons, capture, pointer, close, cast }
}
