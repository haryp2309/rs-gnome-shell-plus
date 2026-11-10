//! One recording: a Mutter screen cast session (org.gnome.Mutter.ScreenCast
//! CreateSession → RecordArea → Start → PipeWireStreamAdded(node)) whose
//! PipeWire stream a `gst-launch-1.0 -e` child encodes to WebM.
//!
//! Why a gst-launch child rather than the gstreamer crate: it's what GNOME
//! does too (the encoding runs outside the shell, in
//! gnome-shell-screencast-service), a stuck or crashing encoder can't take
//! the panel down, and the panel keeps its small dependency list (the
//! gstreamer crates add a large build and link against GStreamer even when
//! nobody records). The cost is coarser feedback: start is detected from
//! gst-launch's "Setting pipeline to PLAYING" line, and SIGINT (-e) ends
//! the stream with an EOS so webmmux writes a complete file.

use gtk::prelude::*;
use gtk::{gio, glib};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::{Rc, Weak};
use std::time::Duration;

const MUTTER: &str = "org.gnome.Mutter.ScreenCast";
const MUTTER_PATH: &str = "/org/gnome/Mutter/ScreenCast";
const SESSION_IFACE: &str = "org.gnome.Mutter.ScreenCast.Session";
const STREAM_IFACE: &str = "org.gnome.Mutter.ScreenCast.Stream";

/// Mutter's cursor-mode: hidden, or drawn into the frames.
const CURSOR_HIDDEN: u32 = 0;
const CURSOR_EMBEDDED: u32 = 1;

const START_TIMEOUT: Duration = Duration::from_secs(10);
const STOP_TIMEOUT: Duration = Duration::from_secs(10);
const SIGINT: i32 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoder {
    Vp9,
    Vp8,
}

pub struct Spec {
    /// Stage coordinates.
    pub area: (i32, i32, i32, i32),
    pub cursor: bool,
    pub framerate: i32,
    pub path: PathBuf,
    pub encoder: Encoder,
}

type Started = Box<dyn FnOnce(Result<(), String>)>;
type Stopped = Box<dyn FnOnce(Result<PathBuf, String>)>;

pub struct Recorder {
    this: Weak<Recorder>,
    conn: gio::DBusConnection,
    spec: Spec,
    session: RefCell<Option<String>>,
    subscriptions: RefCell<Vec<gio::SignalSubscriptionId>>,
    process: RefCell<Option<gio::Subprocess>>,
    playing: Cell<bool>,
    stopping: Cell<bool>,
    done: Cell<bool>,
    on_started: RefCell<Option<Started>>,
    on_stopped: RefCell<Option<Stopped>>,
}

impl Recorder {
    /// Starts recording. Exactly one of the callbacks ends it: `on_started`
    /// with an error, or `on_started(Ok)` and later `on_stopped`. Neither
    /// runs before this returns.
    pub fn start(
        conn: &gio::DBusConnection,
        spec: Spec,
        on_started: impl FnOnce(Result<(), String>) + 'static,
        on_stopped: impl FnOnce(Result<PathBuf, String>) + 'static,
    ) -> Rc<Recorder> {
        let recorder = Rc::new_cyclic(|this| Recorder {
            this: this.clone(),
            conn: conn.clone(),
            spec,
            session: RefCell::default(),
            subscriptions: RefCell::default(),
            process: RefCell::default(),
            playing: Cell::new(false),
            stopping: Cell::new(false),
            done: Cell::new(false),
            on_started: RefCell::new(Some(Box::new(on_started))),
            on_stopped: RefCell::new(Some(Box::new(on_stopped))),
        });
        recorder.create_session();
        let weak = recorder.this.clone();
        glib::timeout_add_local_once(START_TIMEOUT, move || {
            if let Some(this) = weak.upgrade() {
                if !this.playing.get() && !this.done.get() {
                    this.fail("the recording didn't start in time".into());
                }
            }
        });
        recorder
    }

    pub fn path(&self) -> &std::path::Path {
        &self.spec.path
    }

    fn call(
        &self,
        path: &str,
        iface: &str,
        method: &'static str,
        args: Option<glib::Variant>,
        reply: Option<&str>,
        then: impl FnOnce(&Recorder, glib::Variant) + 'static,
    ) {
        let weak = self.this.clone();
        self.conn.call(
            Some(MUTTER),
            path,
            iface,
            method,
            args.as_ref(),
            reply.map(|r| glib::VariantTy::new(r).unwrap()),
            gio::DBusCallFlags::NONE,
            10000,
            gio::Cancellable::NONE,
            move |result| {
                let Some(this) = weak.upgrade() else { return };
                if this.done.get() {
                    return;
                }
                match result {
                    Ok(reply) => then(&this, reply),
                    Err(err) => this.fail(format!("{method} failed: {err}")),
                }
            },
        );
    }

    fn create_session(&self) {
        let props = glib::VariantDict::new(None);
        props.insert("is-recording", true);
        let args = glib::Variant::tuple_from_iter([props.end()]);
        self.call(MUTTER_PATH, MUTTER, "CreateSession", Some(args), Some("(o)"), |this, reply| {
            let Some(session) = reply.child_value(0).str().map(str::to_owned) else { return };
            this.session.replace(Some(session.clone()));
            this.subscribe(&session, SESSION_IFACE, "Closed", |this, _| this.stop());
            this.record_area(&session);
        });
    }

    fn record_area(&self, session: &str) {
        let (x, y, w, h) = self.spec.area;
        let props = glib::VariantDict::new(None);
        props.insert("cursor-mode", if self.spec.cursor { CURSOR_EMBEDDED } else { CURSOR_HIDDEN });
        let args = glib::Variant::tuple_from_iter([
            x.to_variant(),
            y.to_variant(),
            w.to_variant(),
            h.to_variant(),
            props.end(),
        ]);
        let session = session.to_owned();
        self.call(&session.clone(), SESSION_IFACE, "RecordArea", Some(args), Some("(o)"), move |this, reply| {
            let Some(stream) = reply.child_value(0).str().map(str::to_owned) else { return };
            this.subscribe(&stream, STREAM_IFACE, "PipeWireStreamAdded", |this, params| {
                if let Some((node,)) = params.get::<(u32,)>() {
                    this.launch(node);
                }
            });
            this.call(&session, SESSION_IFACE, "Start", None, None, |_, _| {});
        });
    }

    fn subscribe(&self, path: &str, iface: &str, signal: &str, f: impl Fn(&Recorder, &glib::Variant) + 'static) {
        let weak = self.this.clone();
        #[allow(deprecated)]
        let id = self.conn.signal_subscribe(
            Some(MUTTER),
            Some(iface),
            Some(signal),
            Some(path),
            None,
            gio::DBusSignalFlags::NONE,
            move |_, _, _, _, _, params| {
                if let Some(this) = weak.upgrade() {
                    if !this.done.get() {
                        f(&this, params);
                    }
                }
            },
        );
        self.subscriptions.borrow_mut().push(id);
    }

    /// The gst-launch pipeline, as GNOME Shell's software encoding ones.
    fn pipeline(&self, node: u32) -> Vec<String> {
        let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2).clamp(1, 8);
        let framerate = self.spec.framerate.clamp(1, 120);
        let encoder = match self.spec.encoder {
            Encoder::Vp9 => format!(
                "vp9enc min-quantizer=10 max-quantizer=50 cq-level=13 cpu-used=8 deadline=1 \
                 static-threshold=100 keyframe-mode=disabled buffer-size=20000 row-mt=true threads={threads}"
            ),
            Encoder::Vp8 => format!(
                "vp8enc cpu-used=16 max-quantizer=17 deadline=1 keyframe-mode=disabled \
                 static-threshold=1000 buffer-size=20000 threads={threads}"
            ),
        };
        let mut args: Vec<String> = vec!["gst-launch-1.0".into(), "-e".into()];
        let text = format!(
            "pipewiresrc path={node} do-timestamp=true keepalive-time=1000 resend-last=true ! \
             video/x-raw,max-framerate={framerate}/1 ! \
             videoconvert chroma-mode=none dither=none matrix-mode=output-only n-threads={threads} ! \
             queue ! {encoder} ! queue ! webmmux"
        );
        args.extend(text.split_whitespace().map(str::to_owned));
        args.push("!".into());
        args.push("filesink".into());
        // One argv element: gst-launch escapes the spaces in it.
        args.push(format!("location={}", self.spec.path.display()));
        args
    }

    fn launch(&self, node: u32) {
        if self.process.borrow().is_some() {
            return;
        }
        let argv = self.pipeline(node);
        glib::g_debug!("shellplus", "screencast: {}", argv.join(" "));
        let argv: Vec<&std::ffi::OsStr> = argv.iter().map(std::ffi::OsStr::new).collect();
        let process = match gio::Subprocess::newv(
            &argv,
            gio::SubprocessFlags::STDOUT_PIPE | gio::SubprocessFlags::STDERR_MERGE,
        ) {
            Ok(process) => process,
            Err(err) => {
                self.fail(format!("could not run gst-launch-1.0: {err}"));
                return;
            }
        };
        if let Some(stdout) = process.stdout_pipe() {
            self.read_output(gio::DataInputStream::new(&stdout));
        }
        let weak = self.this.clone();
        process.wait_async(gio::Cancellable::NONE, move |_| {
            if let Some(this) = weak.upgrade() {
                this.exited();
            }
        });
        self.process.replace(Some(process));
    }

    /// gst-launch's progress lines: PLAYING means frames flow. Errors are
    /// logged.
    fn read_output(&self, stream: gio::DataInputStream) {
        let weak = self.this.clone();
        let next = stream.clone();
        stream.read_line_utf8_async(glib::Priority::DEFAULT, gio::Cancellable::NONE, move |result| {
            let Ok(Some(line)) = result else { return };
            if let Some(this) = weak.upgrade() {
                if line.contains("ERROR") || line.contains("WARNING") {
                    glib::g_warning!("shellplus", "screencast: {line}");
                }
                if line.contains("Setting pipeline to PLAYING") && !this.playing.replace(true) {
                    if let Some(started) = this.on_started.take() {
                        started(Ok(()));
                    }
                }
                this.read_output(next);
            }
        });
    }

    /// Ends the recording: EOS through SIGINT, then the Mutter session.
    pub fn stop(&self) {
        if self.done.get() || self.stopping.replace(true) {
            return;
        }
        let process = self.process.borrow().clone();
        match process {
            Some(process) if self.playing.get() => {
                process.send_signal(SIGINT);
                let weak = self.this.clone();
                glib::timeout_add_local_once(STOP_TIMEOUT, move || {
                    if let Some(this) = weak.upgrade() {
                        if !this.done.get() {
                            glib::g_warning!("shellplus", "screencast: the encoder didn't stop; killing it");
                            if let Some(process) = this.process.borrow().as_ref() {
                                process.force_exit();
                            }
                        }
                    }
                });
            }
            _ => self.fail("stopped before the recording started".into()),
        }
    }

    fn exited(&self) {
        if self.done.get() {
            return;
        }
        let ok = self
            .process
            .borrow()
            .as_ref()
            .is_some_and(|p| p.has_exited() && p.exit_status() == 0);
        let written = std::fs::metadata(&self.spec.path).map(|m| m.len() > 0).unwrap_or(false);
        if self.playing.get() && written && (ok || self.stopping.get()) {
            self.finish(Ok(self.spec.path.clone()));
        } else {
            self.fail("the encoder stopped unexpectedly".into());
        }
    }

    fn fail(&self, message: String) {
        if self.done.get() {
            return;
        }
        glib::g_warning!("shellplus", "screencast: {message}");
        if let Some(process) = self.process.borrow().as_ref() {
            if !process.has_exited() {
                process.force_exit();
            }
        }
        self.finish(Err(message));
    }

    fn finish(&self, result: Result<PathBuf, String>) {
        if self.done.replace(true) {
            return;
        }
        for id in self.subscriptions.take() {
            #[allow(deprecated)]
            self.conn.signal_unsubscribe(id);
        }
        if let Some(session) = self.session.take() {
            self.conn.call(
                Some(MUTTER),
                &session,
                SESSION_IFACE,
                "Stop",
                None,
                None,
                gio::DBusCallFlags::NONE,
                -1,
                gio::Cancellable::NONE,
                |_| {},
            );
        }
        // Never call back into the owner from inside its own call.
        let started = self.on_started.take();
        let stopped = self.on_stopped.take();
        glib::idle_add_local_once(move || match (started, result) {
            (Some(started), Ok(_)) => started(Err("the recording didn't start".into())),
            (Some(started), Err(err)) => started(Err(err)),
            (None, result) => {
                if let Some(stopped) = stopped {
                    stopped(result);
                }
            }
        });
    }
}
