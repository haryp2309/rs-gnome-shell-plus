//! Screen recording, as in GNOME Shell 42+: the screenshot UI's video mode
//! records the screen or a selection to
//! ~/Videos/Screencasts/"Screencast From YYYY-MM-DD HH-MM-SS.webm"; while
//! it runs, a red pill in the top bar shows the elapsed time and stops it
//! when clicked (so does show-screen-recording-ui, Ctrl+Shift+Alt+R, which
//! the compositor relays as the "screencast" ShowUI mode). A finished
//! recording is announced with "Screencast recorded" / Show in Files.
//!
//! Also org.gnome.Shell.Screencast for other programs (service.rs). One
//! recording runs at a time, and locking the screen stops it.
//!
//! recorder.rs drives Mutter's ScreenCast API and the gst-launch encoder.

mod recorder;
mod service;

use crate::ipc::ShellClient;
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use recorder::{Encoder, Recorder, Spec};
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::sync::Once;
use std::time::Instant;

/// Who started a recording.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Owner {
    /// The screenshot UI (or the shortcut).
    Ui,
    /// An org.gnome.Shell.Screencast client, by unique bus name.
    Bus(String),
}

struct Active {
    recorder: Rc<Recorder>,
    owner: Owner,
    /// Set once frames flow.
    since: Option<Instant>,
    unwatch: Option<Box<dyn FnOnce()>>,
    waiters: Vec<Box<dyn FnOnce(bool)>>,
}

pub struct Screencasts {
    this: Weak<Screencasts>,
    shell: Rc<ShellClient>,
    conn: Option<gio::DBusConnection>,
    indicator: gtk::Button,
    time: gtk::Label,
    active: RefCell<Option<Active>>,
    ticker: RefCell<Option<glib::SourceId>>,
    encoder: Rc<Cell<Encoder>>,
}

thread_local! {
    static INSTANCE: RefCell<Option<Rc<Screencasts>>> = const { RefCell::new(None) };
}

fn instance() -> Option<Rc<Screencasts>> {
    INSTANCE.with(|i| i.borrow().clone())
}

pub fn install(shell: &Rc<ShellClient>) {
    load_css();
    let conn = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE).ok();
    let (indicator, time) = build_indicator();
    let screencasts = Rc::new_cyclic(|this: &Weak<Screencasts>| Screencasts {
        this: this.clone(),
        shell: shell.clone(),
        conn: conn.clone(),
        indicator,
        time,
        active: RefCell::default(),
        ticker: RefCell::default(),
        encoder: Rc::new(Cell::new(Encoder::Vp8)),
    });
    screencasts.indicator.connect_clicked(glib::clone!(
        #[weak]
        screencasts,
        move |_| screencasts.stop()
    ));
    shell.connect_state(glib::clone!(
        #[weak]
        screencasts,
        move |state| {
            if state.locked {
                screencasts.stop();
            }
        }
    ));
    pick_encoder(screencasts.encoder.clone());
    if let Some(conn) = &conn {
        service::export(conn, Rc::downgrade(&screencasts));
    }
    INSTANCE.with(|i| i.replace(Some(screencasts)));
}

/// The top bar's recording indicator (hidden while not recording).
pub fn indicator() -> gtk::Widget {
    instance().map(|s| s.indicator.clone().upcast()).unwrap_or_else(|| gtk::Box::new(gtk::Orientation::Horizontal, 0).upcast())
}

pub fn is_recording() -> bool {
    instance().is_some_and(|s| s.active.borrow().is_some())
}

/// Stops the recording, whoever started it (the indicator, the shortcut,
/// locking).
pub fn stop() {
    if let Some(s) = instance() {
        s.stop();
    }
}

/// Starts recording `rect` (stage coordinates) for the screenshot UI.
pub fn start_ui(rect: (i32, i32, i32, i32), cursor: bool) {
    let Some(s) = instance() else { return };
    let path = match new_file() {
        Ok(path) => path,
        Err(err) => {
            glib::g_warning!("shellplus", "Could not create the screencast file: {err}");
            return;
        }
    };
    s.start(rect, cursor, 30, path, Owner::Ui, |_| {});
}

impl Screencasts {
    /// The primary monitor, which is what the UI's Screen mode records.
    fn screen_rect(&self) -> (i32, i32, i32, i32) {
        self.shell.state().monitor.unwrap_or((0, 0, 1920, 1080))
    }

    fn start(
        &self,
        area: (i32, i32, i32, i32),
        cursor: bool,
        framerate: i32,
        path: PathBuf,
        owner: Owner,
        on_started: impl FnOnce(Result<(), String>) + 'static,
    ) {
        let refused = if self.conn.is_none() {
            Some("no session bus")
        } else if self.shell.state().locked {
            Some("the screen is locked")
        } else if self.active.borrow().is_some() {
            Some("already recording")
        } else {
            None
        };
        let (Some(conn), None) = (&self.conn, refused) else {
            let why = refused.unwrap_or_default().to_owned();
            glib::idle_add_local_once(move || on_started(Err(why)));
            return;
        };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }

        let spec = Spec { area, cursor, framerate, path, encoder: self.encoder.get() };
        let started = self.this.clone();
        let stopped = self.this.clone();
        let recorder = Recorder::start(
            conn,
            spec,
            move |result| {
                let Some(this) = started.upgrade() else { return };
                match &result {
                    Ok(()) => this.started(),
                    Err(err) => this.ended(Err(err.clone())),
                }
                on_started(result);
            },
            move |result| {
                if let Some(this) = stopped.upgrade() {
                    this.ended(result);
                }
            },
        );

        // A client's recording ends with the client.
        let unwatch = match &owner {
            Owner::Bus(name) => {
                let this = self.this.clone();
                let name = name.clone();
                let id = gio::bus_watch_name_on_connection(
                    conn,
                    &name.clone(),
                    gio::BusNameWatcherFlags::NONE,
                    |_, _, _| {},
                    move |_, _| {
                        if let Some(this) = this.upgrade() {
                            this.stop_for(&name, |_| {});
                        }
                    },
                );
                Some(Box::new(move || gio::bus_unwatch_name(id)) as Box<dyn FnOnce()>)
            }
            Owner::Ui => None,
        };
        self.active.replace(Some(Active { recorder, owner, since: None, unwatch, waiters: Vec::new() }));
    }

    fn started(&self) {
        if let Some(active) = self.active.borrow_mut().as_mut() {
            active.since = Some(Instant::now());
        }
        self.update_time();
        self.indicator.set_visible(true);
        let this = self.this.clone();
        let id = glib::timeout_add_seconds_local(1, move || {
            let Some(this) = this.upgrade() else { return glib::ControlFlow::Break };
            this.update_time();
            glib::ControlFlow::Continue
        });
        if let Some(old) = self.ticker.replace(Some(id)) {
            old.remove();
        }
    }

    fn ended(&self, result: Result<PathBuf, String>) {
        let Some(active) = self.active.take() else { return };
        if let Some(unwatch) = active.unwatch {
            unwatch();
        }
        if let Some(id) = self.ticker.take() {
            id.remove();
        }
        self.indicator.set_visible(false);
        let ok = result.is_ok();
        match result {
            Ok(path) => {
                if active.owner == Owner::Ui {
                    notify(&path);
                }
            }
            Err(err) => {
                if active.since.is_some() {
                    if let Some(conn) = &self.conn {
                        service::emit_error(conn, "RecordingFailed", &err);
                    }
                }
            }
        }
        for waiter in active.waiters {
            waiter(ok);
        }
    }

    fn stop(&self) {
        let recorder = self.active.borrow().as_ref().map(|a| a.recorder.clone());
        if let Some(recorder) = recorder {
            recorder.stop();
        }
    }

    /// StopScreencast: stops `sender`'s recording; `done(stopped)` once
    /// the file is complete.
    fn stop_for(&self, sender: &str, done: impl FnOnce(bool) + 'static) {
        let mut active = self.active.borrow_mut();
        match active.as_mut() {
            Some(a) if a.owner == Owner::Bus(sender.to_owned()) => {
                a.waiters.push(Box::new(done));
                let recorder = a.recorder.clone();
                drop(active);
                recorder.stop();
            }
            _ => {
                drop(active);
                glib::idle_add_local_once(move || done(false));
            }
        }
    }

    fn update_time(&self) {
        let secs = self
            .active
            .borrow()
            .as_ref()
            .and_then(|a| a.since)
            .map(|since| since.elapsed().as_secs())
            .unwrap_or(0);
        self.time.set_label(&format_elapsed(secs));
        if let Some(active) = self.active.borrow().as_ref() {
            let name = active.recorder.path().file_name().map(|n| n.to_string_lossy().into_owned());
            self.indicator.set_tooltip_text(Some(&format!("Stop recording{}", name.map(|n| format!(" ({n})")).unwrap_or_default())));
        }
    }
}

/// mm:ss, or h:mm:ss after an hour.
fn format_elapsed(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

/// GNOME's red pill: a stop icon and the elapsed time.
fn build_indicator() -> (gtk::Button, gtk::Label) {
    let content = gtk::Box::builder().spacing(6).build();
    content.append(&gtk::Image::builder().icon_name("media-playback-stop-symbolic").build());
    let time = gtk::Label::builder().name("screencast-time").label("00:00").build();
    content.append(&time);
    let button = gtk::Button::builder()
        .name("screencast-indicator")
        .child(&content)
        .visible(false)
        .valign(gtk::Align::Fill)
        .css_classes(["panel-button", "screen-recording-indicator"])
        .build();
    (button, time)
}

/// ~/Videos/Screencasts (the XDG videos directory, else ~/Videos).
fn screencasts_dir() -> PathBuf {
    let home = glib::home_dir();
    let videos = glib::user_special_dir(glib::UserDirectory::Videos)
        .filter(|dir| !dir.as_os_str().is_empty() && *dir != home)
        .unwrap_or_else(|| home.join("Videos"));
    videos.join("Screencasts")
}

/// A new "Screencast From YYYY-MM-DD HH-MM-SS.webm" (adding -1, -2, … if
/// taken).
fn new_file() -> std::io::Result<PathBuf> {
    let dir = screencasts_dir();
    std::fs::create_dir_all(&dir)?;
    let stamp = glib::DateTime::now_local()
        .and_then(|now| now.format("%Y-%m-%d %H-%M-%S"))
        .map(|s| s.to_string())
        .unwrap_or_default();
    let base = format!("Screencast From {stamp}");
    for i in 0..1000 {
        let name = if i == 0 { format!("{base}.webm") } else { format!("{base}-{i}.webm") };
        let path = dir.join(name);
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(_) => return Ok(path),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
    }
    Err(std::io::Error::other("too many screencasts with the same name"))
}

fn notify(path: &Path) {
    crate::screenshot::save::post(
        path,
        "camera-web-symbolic",
        "Screencast recorded",
        "Click here to view the video.",
        false,
    );
}

/// VP9 when GStreamer has vp9enc (GNOME's preference), else VP8.
/// SHELLPLUS_SCREENCAST_ENCODER=vp8|vp9 overrides it.
fn pick_encoder(encoder: Rc<Cell<Encoder>>) {
    match std::env::var("SHELLPLUS_SCREENCAST_ENCODER").as_deref() {
        Ok("vp8") => return encoder.set(Encoder::Vp8),
        Ok("vp9") => return encoder.set(Encoder::Vp9),
        _ => {}
    }
    let argv = [std::ffi::OsStr::new("gst-inspect-1.0"), "--exists".as_ref(), "vp9enc".as_ref()];
    let Ok(process) = gio::Subprocess::newv(&argv, gio::SubprocessFlags::STDOUT_SILENCE | gio::SubprocessFlags::STDERR_SILENCE)
    else {
        return;
    };
    let probe = process.clone();
    process.wait_async(gio::Cancellable::NONE, move |_| {
        if probe.has_exited() && probe.exit_status() == 0 {
            encoder.set(Encoder::Vp9);
        }
    });
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
