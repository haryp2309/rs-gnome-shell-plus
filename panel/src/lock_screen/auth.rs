//! Checking the user's password, like GNOME Shell's unlock dialog:
//!
//! - When GDM runs, through its reauthentication channel:
//!   `org.gnome.DisplayManager.Manager.OpenReauthenticationChannel(user)`
//!   gives a peer-to-peer D-Bus address with GDM's `UserVerifier`, which
//!   runs the `gdm-password` PAM conversation in GDM's worker (with GDM's
//!   PAM configuration: fingerprint, smartcard…).
//! - Otherwise with PAM directly, service `gnome-shell-plus` (see pam.rs).
//!
//! The choice is made at every [`Verifier::start`]. Either way, the caller
//! sees the same conversation: info and problem messages, queries to
//! answer, then success or failure.

use super::pam;
use crate::system;
use gtk::prelude::*;
use gtk::{gio, glib};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};
use std::sync::mpsc;

const GDM: &str = "org.gnome.DisplayManager";
const GDM_MANAGER_PATH: &str = "/org/gnome/DisplayManager/Manager";
const GDM_MANAGER: &str = "org.gnome.DisplayManager.Manager";
const GDM_SESSION_PATH: &str = "/org/gnome/DisplayManager/Session";
const GDM_VERIFIER: &str = "org.gnome.DisplayManager.UserVerifier";
/// The PAM service GNOME Shell reauthenticates with.
const GDM_SERVICE: &str = "gdm-password";

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    Info(String),
    Problem(String),
    /// Answer with [`Verifier::answer`].
    Query { prompt: String, secret: bool },
    /// The user is who they say they are.
    Complete,
    Failed,
}

enum Backend {
    Gdm { conn: gio::DBusConnection, _proxy: gio::DBusProxy },
    Pam { answers: mpsc::Sender<String>, id: u64 },
}

pub struct Verifier {
    user: String,
    on_event: Box<dyn Fn(Event)>,
    backend: RefCell<Option<Backend>>,
    done: Cell<bool>,
}

thread_local! {
    /// PAM threads post their messages to the main thread by id.
    static PAM_SESSIONS: RefCell<HashMap<u64, Weak<Verifier>>> = RefCell::default();
    static NEXT_ID: Cell<u64> = const { Cell::new(1) };
}

impl Verifier {
    /// Starts verifying `user`; `on_event` gets the conversation.
    pub fn start(user: &str, on_event: impl Fn(Event) + 'static) -> Rc<Self> {
        let verifier = Rc::new(Self {
            user: user.to_owned(),
            on_event: Box::new(on_event),
            backend: RefCell::default(),
            done: Cell::new(false),
        });
        let weak = Rc::downgrade(&verifier);
        glib::spawn_future_local(async move {
            let conn = match weak.upgrade() {
                Some(verifier) => open_gdm_channel(&verifier.user).await,
                None => return,
            };
            let Some(verifier) = weak.upgrade() else { return };
            if verifier.done.get() {
                return;
            }
            match conn {
                Some(conn) => verifier.start_gdm(conn),
                None => verifier.start_pam(),
            }
        });
        verifier
    }

    pub fn answer(&self, text: &str) {
        match &*self.backend.borrow() {
            Some(Backend::Gdm { conn, .. }) => {
                conn.call(
                    None,
                    GDM_SESSION_PATH,
                    GDM_VERIFIER,
                    "AnswerQuery",
                    Some(&(GDM_SERVICE, text).to_variant()),
                    None,
                    gio::DBusCallFlags::NONE,
                    -1,
                    gio::Cancellable::NONE,
                    |result| {
                        if let Err(err) = result {
                            glib::g_warning!("shellplus", "AnswerQuery failed: {err}");
                        }
                    },
                );
            }
            Some(Backend::Pam { answers, .. }) => {
                let _ = answers.send(text.to_owned());
            }
            None => {}
        }
    }

    /// Stops the conversation; no more events are delivered.
    pub fn cancel(&self) {
        if self.done.replace(true) {
            return;
        }
        match self.backend.take() {
            Some(Backend::Gdm { conn, .. }) => {
                let closing = conn.clone();
                conn.call(
                    None,
                    GDM_SESSION_PATH,
                    GDM_VERIFIER,
                    "Cancel",
                    None,
                    None,
                    gio::DBusCallFlags::NONE,
                    1000,
                    gio::Cancellable::NONE,
                    move |_| closing.close(gio::Cancellable::NONE, |_| {}),
                );
            }
            Some(Backend::Pam { id, .. }) => {
                // Dropping the answer channel ends the PAM conversation.
                PAM_SESSIONS.with(|s| s.borrow_mut().remove(&id));
            }
            None => {}
        }
    }

    fn emit(&self, event: Event) {
        if self.done.get() {
            return;
        }
        if matches!(event, Event::Complete | Event::Failed) {
            self.done.set(true);
        }
        (self.on_event)(event);
        if self.done.get() {
            self.cancel_backend_quietly();
        }
    }

    fn cancel_backend_quietly(&self) {
        match self.backend.take() {
            Some(Backend::Gdm { conn, .. }) => conn.close(gio::Cancellable::NONE, |_| {}),
            Some(Backend::Pam { id, .. }) => {
                PAM_SESSIONS.with(|s| s.borrow_mut().remove(&id));
            }
            None => {}
        }
    }

    // --- GDM -------------------------------------------------------------

    fn start_gdm(self: &Rc<Self>, conn: gio::DBusConnection) {
        glib::g_message!("shellplus", "Unlock: verifying with GDM's reauthentication channel");
        // A proxy, since the peer-to-peer signals have no sender (which the
        // plain signal_subscribe binding can't take).
        let proxy = match gio::DBusProxy::new_sync(
            &conn,
            gio::DBusProxyFlags::DO_NOT_LOAD_PROPERTIES | gio::DBusProxyFlags::DO_NOT_AUTO_START,
            None,
            None,
            GDM_SESSION_PATH,
            GDM_VERIFIER,
            gio::Cancellable::NONE,
        ) {
            Ok(proxy) => proxy,
            Err(err) => {
                glib::g_warning!("shellplus", "GDM verifier unavailable: {err}");
                self.emit(Event::Failed);
                return;
            }
        };
        let weak = Rc::downgrade(self);
        proxy.connect_local("g-signal", false, move |args| {
            let verifier = weak.upgrade()?;
            let signal = args.get(2)?.get::<String>().ok()?;
            let params = args.get(3)?.get::<glib::Variant>().ok()?;
            let arg = |i: usize| {
                (params.n_children() > i)
                    .then(|| params.child_value(i).str().map(str::to_owned))
                    .flatten()
                    .unwrap_or_default()
            };
            // Only our service's conversation counts.
            if signal != "Reset" && arg(0) != GDM_SERVICE {
                return None;
            }
            match signal.as_str() {
                "Info" => verifier.emit(Event::Info(arg(1))),
                "Problem" => verifier.emit(Event::Problem(arg(1))),
                "InfoQuery" => verifier.emit(Event::Query { prompt: arg(1), secret: false }),
                "SecretInfoQuery" => verifier.emit(Event::Query { prompt: arg(1), secret: true }),
                "VerificationComplete" => verifier.emit(Event::Complete),
                "ServiceUnavailable" => {
                    verifier.emit(Event::Problem(arg(1)));
                    verifier.emit(Event::Failed)
                }
                "VerificationFailed" | "ConversationStopped" | "Reset" => verifier.emit(Event::Failed),
                _ => {}
            }
            None
        });
        self.backend.replace(Some(Backend::Gdm { conn: conn.clone(), _proxy: proxy }));

        let weak = Rc::downgrade(self);
        conn.call(
            None,
            GDM_SESSION_PATH,
            GDM_VERIFIER,
            "BeginVerificationForUser",
            Some(&(GDM_SERVICE, self.user.as_str()).to_variant()),
            None,
            gio::DBusCallFlags::NONE,
            -1,
            gio::Cancellable::NONE,
            move |result| {
                if let Err(err) = result {
                    glib::g_warning!("shellplus", "GDM verification failed to start: {err}");
                    if let Some(verifier) = weak.upgrade() {
                        verifier.emit(Event::Failed);
                    }
                }
            },
        );
    }

    // --- PAM -------------------------------------------------------------

    fn start_pam(self: &Rc<Self>) {
        glib::g_message!("shellplus", "Unlock: verifying with PAM service {}", pam::SERVICE);
        let id = NEXT_ID.with(|n| {
            let id = n.get();
            n.set(id + 1);
            id
        });
        PAM_SESSIONS.with(|s| s.borrow_mut().insert(id, Rc::downgrade(self)));
        let answers = pam::authenticate(&self.user, move |message| {
            // On the PAM thread: hand over to the main loop.
            glib::MainContext::default().invoke(move || {
                let Some(verifier) = PAM_SESSIONS.with(|s| s.borrow().get(&id).and_then(Weak::upgrade)) else {
                    return;
                };
                match message {
                    pam::Message::Info(text) => verifier.emit(Event::Info(text)),
                    pam::Message::Problem(text) => verifier.emit(Event::Problem(text)),
                    pam::Message::Query { prompt, secret } => verifier.emit(Event::Query { prompt, secret }),
                    pam::Message::Done(Ok(())) => verifier.emit(Event::Complete),
                    pam::Message::Done(Err(error)) => {
                        glib::g_message!("shellplus", "Unlock: PAM said {error}");
                        verifier.emit(Event::Failed)
                    }
                }
            });
        });
        self.backend.replace(Some(Backend::Pam { answers, id }));
    }
}

impl Drop for Verifier {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// GDM's reauthentication channel for `user`, if GDM is running.
async fn open_gdm_channel(user: &str) -> Option<gio::DBusConnection> {
    let system = system::system_bus()?;
    let has_owner = system
        .call_future(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "NameHasOwner",
            Some(&(GDM,).to_variant()),
            Some(glib::VariantTy::new("(b)").unwrap()),
            gio::DBusCallFlags::NONE,
            2000,
        )
        .await
        .ok()
        .and_then(|reply| reply.child_value(0).get::<bool>())
        .unwrap_or(false);
    if !has_owner {
        return None;
    }

    let reply = system
        .call_future(
            Some(GDM),
            GDM_MANAGER_PATH,
            GDM_MANAGER,
            "OpenReauthenticationChannel",
            Some(&(user,).to_variant()),
            Some(glib::VariantTy::new("(s)").unwrap()),
            gio::DBusCallFlags::NONE,
            5000,
        )
        .await;
    let address = match reply {
        Ok(reply) => reply.child_value(0).str()?.to_owned(),
        Err(err) => {
            glib::g_message!("shellplus", "No GDM reauthentication channel ({err}); using PAM");
            return None;
        }
    };
    match gio::DBusConnection::for_address_future(
        &address,
        gio::DBusConnectionFlags::AUTHENTICATION_CLIENT,
        None::<&gio::DBusAuthObserver>,
    )
    .await
    {
        Ok(conn) => Some(conn),
        Err(err) => {
            glib::g_warning!("shellplus", "Can't connect to GDM's reauthentication channel: {err}");
            None
        }
    }
}
