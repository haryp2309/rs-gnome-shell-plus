//! GNOME Shell's screenshot UI (Print): a full-screen overlay over a frozen
//! capture with Selection / Screen / Window modes. A capture is saved to
//! ~/Pictures/Screenshots, copied to the clipboard and announced with a
//! notification.
//!
//! The compositor does the capturing (compositor/src/shellplus-screenshot.c,
//! org.gnome.ShellPlus.Screenshot) and asks for the UI with its ShowUI
//! signal: for the Print key, the quick settings button (which calls the
//! private ShowUI method) and for org.gnome.Shell.Screenshot's InteractiveScreenshot,
//! SelectArea and PickColor, whose answers go back with Finish.

mod client;
mod icons;
mod save;
mod ui;

use crate::ipc::ShellClient;
use crate::overview::client::{Client as WindowsClient, WindowInfo};
use crate::overview::layout::Rect;
use client::{Client, Event, Image};
use gtk::prelude::*;
use gtk::{gdk, glib};
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::sync::Once;
use std::time::Duration;
use ui::{Kind, Mode, Outcome, Params, Ui, WindowShot};

/// Lets the window go before the requester (e.g. gnome-screenshot after
/// SelectArea) captures the screen.
const UNMAP_DELAY: Duration = Duration::from_millis(150);

pub struct Screenshots {
    this: Weak<Screenshots>,
    app: adw::Application,
    shell: Rc<ShellClient>,
    client: Rc<Client>,
    windows: Rc<WindowsClient>,
    ui: RefCell<Option<Rc<Ui>>>,
    busy: Cell<bool>,
    last_selection: Cell<Option<Rect>>,
}

thread_local! {
    /// Lives as long as the panel.
    static INSTANCE: RefCell<Option<Rc<Screenshots>>> = const { RefCell::new(None) };
}

pub fn install(app: &adw::Application, shell: &Rc<ShellClient>) {
    load_css();
    let screenshots = Rc::new_cyclic(|weak: &Weak<Screenshots>| {
        let events = weak.clone();
        Screenshots {
            this: weak.clone(),
            app: app.clone(),
            shell: shell.clone(),
            client: Client::new(move |event| {
                if let Some(screenshots) = events.upgrade() {
                    screenshots.on_event(event);
                }
            }),
            windows: WindowsClient::new(|_| {}),
            ui: RefCell::default(),
            busy: Cell::new(false),
            last_selection: Cell::new(None),
        }
    });
    INSTANCE.with(|instance| instance.replace(Some(screenshots)));
}

impl Screenshots {
    fn on_event(&self, event: Event) {
        match event {
            Event::ShowUi { request, mode } => self.open(request, &mode),
            Event::Taken(path) => save::notify(std::path::Path::new(&path)),
        }
    }

    fn open(&self, request: u32, mode: &str) {
        if self.busy.replace(true) {
            self.client.finish(request, false, glib::VariantDict::new(None));
            return;
        }
        let (kind, mode) = match mode {
            "select-area" => (Kind::SelectArea, Mode::Selection),
            "pick-color" => (Kind::PickColor, Mode::Selection),
            "screen" => (Kind::Screenshot, Mode::Screen),
            "window" => (Kind::Screenshot, Mode::Window),
            _ => (Kind::Screenshot, Mode::Selection),
        };

        let this = self.this.clone();
        self.client.capture_screen(false, move |plain| {
            let Some(this) = this.upgrade() else { return };
            let Some((plain, scale)) = plain else {
                this.fail(request);
                return;
            };
            if kind != Kind::Screenshot {
                this.present(request, kind, mode, plain, scale, None, Vec::new());
                return;
            }
            let weak = this.this.clone();
            this.client.capture_screen(true, move |with_pointer| {
                let Some(this) = weak.upgrade() else { return };
                let with_pointer = with_pointer.map(|(image, _)| image);
                let weak = this.this.clone();
                this.windows.list_windows(move |windows| {
                    let Some(this) = weak.upgrade() else { return };
                    this.capture_windows(windows.unwrap_or_default(), move |this, shots| {
                        this.present(request, kind, mode, plain, scale, with_pointer, shots);
                    });
                });
            });
        });
    }

    fn fail(&self, request: u32) {
        self.busy.set(false);
        self.client.finish(request, false, glib::VariantDict::new(None));
    }

    /// Full-size images of the windows on the active workspace.
    fn capture_windows(&self, windows: Vec<WindowInfo>, done: impl FnOnce(&Screenshots, Vec<WindowShot>) + 'static) {
        let active = self.shell.state().active_workspace;
        let wanted: Vec<WindowInfo> = windows
            .into_iter()
            .filter(|w| !w.minimized && (w.workspace == active || w.workspace < 0))
            .collect();
        let pending = Rc::new(Cell::new(wanted.len()));
        let shots: Rc<RefCell<Vec<Option<WindowShot>>>> =
            Rc::new(RefCell::new((0..wanted.len()).map(|_| None).collect()));
        let done: Rc<RefCell<Option<Box<dyn FnOnce(&Screenshots, Vec<WindowShot>)>>>> =
            Rc::new(RefCell::new(Some(Box::new(done))));
        let finish = {
            let this = self.this.clone();
            let shots = shots.clone();
            let done = done.clone();
            move || {
                let (Some(this), Some(done)) = (this.upgrade(), done.take()) else { return };
                let shots = shots.take().into_iter().flatten().collect();
                done(&this, shots);
            }
        };
        if wanted.is_empty() {
            finish();
            return;
        }
        let finish = Rc::new(finish);
        for (i, info) in wanted.into_iter().enumerate() {
            let (pending, shots, finish) = (pending.clone(), shots.clone(), finish.clone());
            self.client.capture_window(info.id, move |image: Option<Image>| {
                if let Some(image) = image {
                    shots.borrow_mut()[i] = Some(WindowShot { info, image });
                }
                pending.set(pending.get() - 1);
                if pending.get() == 0 {
                    finish();
                }
            });
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn present(
        &self,
        request: u32,
        kind: Kind,
        mode: Mode,
        plain: Image,
        scale: f64,
        with_pointer: Option<Image>,
        windows: Vec<WindowShot>,
    ) {
        let scale = if scale > 0.0 { scale } else { 1.0 };
        let monitor = self.shell.state().monitor.unwrap_or((
            0,
            0,
            (plain.width as f64 / scale) as i32,
            (plain.height as f64 / scale) as i32,
        ));
        let (mw, mh) = (monitor.2 as f64, monitor.3 as f64);
        let selection = match self.last_selection.get() {
            Some(r) if r.x + r.w <= mw && r.y + r.h <= mh => r,
            _ => Rect::new((mw / 4.0).round(), (mh / 4.0).round(), (mw / 2.0).round(), (mh / 2.0).round()),
        };
        let params = Params { kind, mode, plain, with_pointer, scale, monitor, windows, selection };

        let this = self.this.clone();
        let ui = Ui::new(&self.app, params, move |ui, outcome| {
            let Some(this) = this.upgrade() else { return };
            this.done(request, kind, ui, outcome);
        });
        ui.present();
        self.ui.replace(Some(ui));
    }

    fn done(&self, request: u32, kind: Kind, ui: &Ui, outcome: Outcome) {
        if kind == Kind::Screenshot {
            self.last_selection.set(Some(ui.selection()));
        }
        let result = glib::VariantDict::new(None);
        let mut success = true;
        let mut flash = None;
        match outcome {
            Outcome::Cancelled => success = false,
            Outcome::Captured { image, rect } => {
                let texture = image.texture();
                // While the UI still has the keyboard focus.
                save::copy(&texture);
                match save::save_png(&texture) {
                    Ok(path) => {
                        save::notify(&path);
                        result.insert("uri", gtk::gio::File::for_path(&path).uri().as_str());
                    }
                    Err(err) => {
                        glib::g_warning!("shellplus", "Could not save the screenshot: {err}");
                        success = false;
                    }
                }
                flash = Some(rect);
            }
            Outcome::Area(rect) => result.insert("rect", rect),
            Outcome::Color(color) => result.insert("color", color),
        }

        let this = self.this.clone();
        glib::timeout_add_local_once(UNMAP_DELAY, move || {
            let Some(this) = this.upgrade() else { return };
            if let Some(rect) = flash {
                this.client.flash(rect);
            }
            this.client.finish(request, success, result);
            this.ui.take();
            this.busy.set(false);
        });
    }
}

fn load_css() {
    static LOADED: Once = Once::new();
    LOADED.call_once(|| {
        let Some(display) = gdk::Display::default() else { return };
        let provider = gtk::CssProvider::new();
        provider.load_from_string(include_str!("style.css"));
        gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
    });
}
