//! polkit authentication agent: asks for a password when an app needs
//! administrator rights, like GNOME Shell's polkitAgent.js.
//!
//! libpolkit-agent-1 has no Rust bindings, so this speaks its D-Bus side
//! directly. The panel exports org.freedesktop.PolicyKit1.AuthenticationAgent
//! on the system bus and registers it for its logind session with polkitd's
//! Authority (again whenever polkitd restarts). polkitd then calls
//! BeginAuthentication, which only returns once the user has authenticated
//! (or with the Cancelled error).
//!
//! The password itself is checked by polkit's helper, as libpolkit-agent-1
//! does it in polkit 127: first through the socket-activated helper
//! (/run/polkit/agent-helper.socket; the agent writes the user name and the
//! cookie), otherwise by running the setuid polkit-agent-helper-1 with the
//! user name as its argument and the cookie on stdin. The helper runs PAM,
//! prints its conversation line by line (`PAM_PROMPT_ECHO_OFF Password: `,
//! `PAM_ERROR_MSG …`, `SUCCESS`/`FAILURE`), reads the answers from us, and
//! tells polkitd itself when it succeeded.
//!
//! For tests, SHELLPLUS_POLKIT_HELPER replaces the helper binary and
//! SHELLPLUS_POLKIT_HELPER_SOCKET the socket path. Setting only the helper
//! skips the socket.

use crate::system::{self, Props};
use crate::system_dialog::SystemDialog;
use adw::prelude::*;
use gtk::{gdk, gio, glib};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::ffi::OsStr;
use std::path::Path;
use std::rc::{Rc, Weak};
use std::sync::Once;
use std::time::Duration;

const POLKIT: &str = "org.freedesktop.PolicyKit1";
const AUTHORITY_PATH: &str = "/org/freedesktop/PolicyKit1/Authority";
const AUTHORITY: &str = "org.freedesktop.PolicyKit1.Authority";
const AGENT_PATH: &str = "/org/freedesktop/PolicyKit1/AuthenticationAgent";
const AGENT: &str = "org.freedesktop.PolicyKit1.AuthenticationAgent";

const ERROR_CANCELLED: &str = "org.freedesktop.PolicyKit1.Error.Cancelled";
const ERROR_FAILED: &str = "org.freedesktop.PolicyKit1.Error.Failed";
const ERROR_NOT_AUTHORIZED: &str = "org.freedesktop.PolicyKit1.Error.NotAuthorized";

const HELPER: &str = "/usr/lib/polkit-1/polkit-agent-helper-1";
const HELPER_SOCKET: &str = "/run/polkit/agent-helper.socket";
const HELPER_ENV: &str = "SHELLPLUS_POLKIT_HELPER";
const HELPER_SOCKET_ENV: &str = "SHELLPLUS_POLKIT_HELPER_SOCKET";

const LOGIND: &str = "org.freedesktop.login1";
const LOGIND_PATH: &str = "/org/freedesktop/login1";
const LOGIND_MANAGER: &str = "org.freedesktop.login1.Manager";
const ACCOUNTS: &str = "org.freedesktop.Accounts";
const ACCOUNTS_PATH: &str = "/org/freedesktop/Accounts";
const ACCOUNTS_USER: &str = "org.freedesktop.Accounts.User";

const FAILED_TEXT: &str = "Sorry, that didn’t work. Please try again.";
/// Pause before retrying when the helper gave up without asking anything
/// (e.g. a locked account), so the dialog doesn't spin.
const RETRY_DELAY: Duration = Duration::from_secs(2);
const AVATAR_SIZE: i32 = 64;
const SIGTERM: i32 = 15;

const XML: &str = r#"
<node>
  <interface name="org.freedesktop.PolicyKit1.AuthenticationAgent">
    <method name="BeginAuthentication">
      <arg type="s" name="action_id" direction="in"/>
      <arg type="s" name="message" direction="in"/>
      <arg type="s" name="icon_name" direction="in"/>
      <arg type="a{ss}" name="details" direction="in"/>
      <arg type="s" name="cookie" direction="in"/>
      <arg type="a(sa{sv})" name="identities" direction="in"/>
    </method>
    <method name="CancelAuthentication">
      <arg type="s" name="cookie" direction="in"/>
    </method>
  </interface>
</node>"#;

const CSS: &str = "
.system-dialog-card .polkit-user-box { margin-top: 8px; margin-bottom: 4px; }
.system-dialog-card .polkit-user { font-weight: bold; }
.system-dialog-card .polkit-user.root { color: @warning_color; }
.system-dialog-card .polkit-message { font-size: smaller; }
";

/// Someone who may authenticate.
#[derive(Clone, Debug)]
struct User {
    uid: u32,
    name: String,
    real_name: String,
    icon: Option<String>,
    admin: bool,
}

struct Request {
    invocation: gio::DBusMethodInvocation,
    message: String,
    cookie: String,
    users: Vec<User>,
}

struct Agent {
    app: adw::Application,
    conn: gio::DBusConnection,
    /// polkitd's unique name; only it may call the agent.
    owner: RefCell<Option<String>>,
    queue: RefCell<VecDeque<Request>>,
    current: RefCell<Option<Rc<Prompt>>>,
}

pub fn install(app: &adw::Application) {
    let Some(conn) = system::system_bus() else { return };
    let agent = Rc::new(Agent {
        app: app.clone(),
        conn: conn.clone(),
        owner: RefCell::default(),
        queue: RefCell::default(),
        current: RefCell::default(),
    });

    let node = gio::DBusNodeInfo::for_xml(XML).expect("valid introspection XML");
    let iface = node.lookup_interface(AGENT).expect("interface in XML");
    let registration = conn
        .register_object(AGENT_PATH, &iface)
        .method_call(glib::clone!(
            #[strong]
            agent,
            move |_, sender, _, _, method, params, invocation| agent.handle(sender, method, &params, invocation)
        ))
        .build();
    if let Err(err) = registration {
        glib::g_warning!("shellplus", "Can't export the polkit authentication agent: {err}");
        return;
    }

    // Register whenever polkitd (re)appears; it's normally bus-activated.
    gio::bus_watch_name_on_connection(
        &conn,
        POLKIT,
        gio::BusNameWatcherFlags::AUTO_START,
        glib::clone!(
            #[strong]
            agent,
            move |_, _, owner| {
                agent.owner.replace(Some(owner.to_owned()));
                agent.register();
            }
        ),
        move |_, _| {
            agent.owner.take();
            agent.cancel_all();
        },
    );
}

impl Agent {
    fn register(&self) {
        let Some(session) = session_id(&self.conn) else {
            glib::g_warning!("shellplus", "Not registering the polkit agent: no logind session");
            return;
        };
        let details: HashMap<String, glib::Variant> = [("session-id".to_owned(), session.to_variant())].into();
        let subject = ("unix-session", details).to_variant();
        let args = glib::Variant::tuple_from_iter([subject, locale().to_variant(), AGENT_PATH.to_variant()]);
        self.conn.call(
            Some(POLKIT),
            AUTHORITY_PATH,
            AUTHORITY,
            "RegisterAuthenticationAgent",
            Some(&args),
            None,
            gio::DBusCallFlags::NONE,
            -1,
            gio::Cancellable::NONE,
            move |result| match result {
                Ok(_) => glib::g_message!("shellplus", "Registered polkit authentication agent for session {session}"),
                Err(err) => glib::g_warning!("shellplus", "Can't register the polkit authentication agent: {err}"),
            },
        );
    }

    fn handle(
        self: &Rc<Self>,
        sender: Option<&str>,
        method: &str,
        params: &glib::Variant,
        invocation: gio::DBusMethodInvocation,
    ) {
        if sender.is_none() || sender != self.owner.borrow().as_deref() {
            invocation.return_dbus_error(ERROR_NOT_AUTHORIZED, "Only polkitd may call the authentication agent");
            return;
        }
        match method {
            "BeginAuthentication" => self.begin(params, invocation),
            "CancelAuthentication" => {
                let cookie = params.child_value(0).str().unwrap_or_default().to_owned();
                if self.cancel(&cookie) {
                    invocation.return_value(None);
                } else {
                    invocation.return_dbus_error(ERROR_FAILED, "No pending authentication with that cookie");
                }
            }
            _ => invocation.return_dbus_error("org.freedesktop.DBus.Error.UnknownMethod", method),
        }
    }

    fn begin(self: &Rc<Self>, params: &glib::Variant, invocation: gio::DBusMethodInvocation) {
        let string = |i| params.child_value(i).str().unwrap_or_default().to_owned();
        let action_id = string(0);
        let message = string(1);
        let cookie = string(4);
        let users = resolve_users(&self.conn, &params.child_value(5));
        glib::g_message!("shellplus", "polkit: authentication for {action_id} ({} identities)", users.len());
        if users.is_empty() {
            invocation.return_dbus_error(ERROR_FAILED, "None of the identities can authenticate");
            return;
        }
        self.queue.borrow_mut().push_back(Request { invocation, message, cookie, users });
        self.show_next();
    }

    /// polkitd no longer needs an answer: drop the request, or close its
    /// dialog. False if there's no such request.
    fn cancel(self: &Rc<Self>, cookie: &str) -> bool {
        let mut queue = self.queue.borrow_mut();
        if let Some(index) = queue.iter().position(|r| r.cookie == cookie) {
            let request = queue.remove(index).expect("index in range");
            drop(queue);
            request.invocation.return_dbus_error(ERROR_CANCELLED, "Authentication was cancelled");
            return true;
        }
        drop(queue);
        let current = self.current.borrow().clone();
        match current {
            Some(prompt) if prompt.cookie == cookie => {
                prompt.finish(Err("Authentication was cancelled"));
                true
            }
            _ => false,
        }
    }

    fn cancel_all(&self) {
        for request in self.queue.borrow_mut().drain(..) {
            request.invocation.return_dbus_error(ERROR_CANCELLED, "polkitd went away");
        }
        let current = self.current.borrow().clone();
        if let Some(prompt) = current {
            prompt.finish(Err("polkitd went away"));
        }
    }

    /// The current dialog is done; show the next queued request, if any.
    fn finish(self: &Rc<Self>) {
        self.current.take();
        self.show_next();
    }

    fn show_next(self: &Rc<Self>) {
        if self.current.borrow().is_some() {
            return;
        }
        let Some(request) = self.queue.borrow_mut().pop_front() else { return };
        let prompt = Prompt::new(self, request);
        self.current.replace(Some(prompt.clone()));
        prompt.dialog.present(None::<&gtk::Widget>);
        prompt.start_session();
    }
}

/// A connection to the helper for one authentication attempt.
struct Helper {
    input: gio::InputStream,
    output: gio::OutputStream,
    process: Option<gio::Subprocess>,
    socket: Option<gio::SocketConnection>,
}

impl Helper {
    async fn start(user: &str, cookie: &str) -> Result<Self, String> {
        if let Some(path) = helper_socket().filter(|p| Path::new(p).exists()) {
            let address = gio::UnixSocketAddress::new(Path::new(&path));
            match gio::SocketClient::new().connect_future(&address).await {
                Ok(conn) => {
                    // The socket-activated helper reads the user name first.
                    let helper = Self {
                        input: conn.input_stream(),
                        output: conn.output_stream(),
                        process: None,
                        socket: Some(conn),
                    };
                    helper.write_line(user)?;
                    helper.write_line(cookie)?;
                    glib::g_message!("shellplus", "polkit: connected to the helper at {path}");
                    return Ok(helper);
                }
                Err(err) => glib::g_message!("shellplus", "polkit: can't connect to {path}, running the helper: {err}"),
            }
        }

        let program = helper_program();
        let argv = [OsStr::new(&program), OsStr::new(user)];
        let process = gio::Subprocess::newv(&argv, gio::SubprocessFlags::STDIN_PIPE | gio::SubprocessFlags::STDOUT_PIPE)
            .map_err(|err| format!("Can't run {program}: {err}"))?;
        let helper = Self {
            input: process.stdout_pipe().ok_or("no helper stdout")?,
            output: process.stdin_pipe().ok_or("no helper stdin")?,
            process: Some(process),
            socket: None,
        };
        // The cookie goes on stdin, not argv, so other users can't see it.
        helper.write_line(cookie)?;
        Ok(helper)
    }

    fn write_line(&self, text: &str) -> Result<(), String> {
        self.output
            .write_all(format!("{text}\n").as_bytes(), gio::Cancellable::NONE)
            .map(|_| ())
            .map_err(|err| format!("Can't write to the helper: {err}"))
    }

    fn kill(self) {
        let _ = self.output.close(gio::Cancellable::NONE);
        // SIGTERM, like libpolkit-agent-1. The setuid helper keeps our real
        // uid, so we may signal it.
        if let Some(process) = &self.process {
            process.send_signal(SIGTERM);
        }
        if let Some(socket) = &self.socket {
            let _ = socket.close(gio::Cancellable::NONE);
        }
    }
}

/// One request on screen.
struct Prompt {
    agent: Weak<Agent>,
    request: RefCell<Option<Request>>,
    cookie: String,
    users: Vec<User>,
    user: Cell<usize>,
    dialog: Rc<SystemDialog>,
    avatar: adw::Avatar,
    name: gtk::Label,
    password: gtk::PasswordEntry,
    /// For PAM_PROMPT_ECHO_ON (e.g. a user name or a code).
    response: gtk::Entry,
    message: gtk::Label,
    authenticate: gtk::Button,
    helper: RefCell<Option<Helper>>,
    /// Bumped for every helper run, so stale reads are ignored.
    generation: Cell<u64>,
    /// The helper waits for an answer.
    prompting: Cell<bool>,
    echo: Cell<bool>,
    /// This run got at least one answer from the user.
    answered: Cell<bool>,
    /// PAM reported an error during this run.
    pam_error: Cell<bool>,
    done: Cell<bool>,
}

impl Prompt {
    fn new(agent: &Rc<Agent>, request: Request) -> Rc<Self> {
        load_css();
        let dialog = SystemDialog::new(&agent.app, "polkit");
        dialog.set_heading("Authentication Required");
        dialog.set_body(&request.message);

        let user_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(8)
            .halign(gtk::Align::Center)
            .css_classes(["polkit-user-box"])
            .build();
        let avatar = adw::Avatar::builder()
            .name("polkit-avatar")
            .size(AVATAR_SIZE)
            .show_initials(true)
            .build();
        let name = gtk::Label::builder()
            .name("polkit-user-name")
            .css_classes(["polkit-user"])
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        user_box.append(&avatar);
        user_box.append(&name);

        let users = request.users.clone();
        let picker = (users.len() > 1).then(|| {
            let names: Vec<&str> = users.iter().map(|u| u.real_name.as_str()).collect();
            let picker = gtk::DropDown::from_strings(&names);
            picker.set_widget_name("polkit-user-picker");
            picker.set_halign(gtk::Align::Center);
            // The picker shows the name; the label only keeps it for tests.
            name.set_visible(false);
            user_box.append(&picker);
            picker
        });
        dialog.add_content(&user_box);

        let password = gtk::PasswordEntry::builder()
            .name("polkit-password")
            .placeholder_text("Password")
            .show_peek_icon(true)
            .sensitive(false)
            .build();
        let response = gtk::Entry::builder().name("polkit-response").visible(false).build();
        let message = gtk::Label::builder()
            .name("polkit-message")
            .css_classes(["polkit-message"])
            .justify(gtk::Justification::Center)
            .wrap(true)
            .max_width_chars(36)
            .build();
        dialog.add_content(&password);
        dialog.add_content(&response);
        dialog.add_content(&message);

        let cancel = dialog.add_button("polkit-cancel", "Cancel", false);
        let authenticate = dialog.add_button("polkit-authenticate", "Authenticate", true);
        authenticate.set_sensitive(false);

        let current = glib::user_name().to_string_lossy().into_owned();
        let preferred = users
            .iter()
            .position(|u| u.name == current)
            .or_else(|| users.iter().position(|u| u.admin))
            .unwrap_or(0);

        let prompt = Rc::new(Self {
            agent: Rc::downgrade(agent),
            cookie: request.cookie.clone(),
            request: RefCell::new(Some(request)),
            users,
            user: Cell::new(preferred),
            dialog,
            avatar,
            name,
            password,
            response,
            message,
            authenticate,
            helper: RefCell::default(),
            generation: Cell::new(0),
            prompting: Cell::new(false),
            echo: Cell::new(false),
            answered: Cell::new(false),
            pam_error: Cell::new(false),
            done: Cell::new(false),
        });
        prompt.show_user();

        let weak = Rc::downgrade(&prompt);
        let respond = move || {
            if let Some(prompt) = weak.upgrade() {
                prompt.respond();
            }
        };
        prompt.password.connect_activate({
            let respond = respond.clone();
            move |_| respond()
        });
        prompt.response.connect_activate({
            let respond = respond.clone();
            move |_| respond()
        });
        prompt.authenticate.connect_clicked(move |_| respond());

        if let Some(picker) = picker {
            picker.set_selected(preferred as u32);
            let weak = Rc::downgrade(&prompt);
            picker.connect_selected_notify(move |picker| {
                if let Some(prompt) = weak.upgrade() {
                    prompt.select(picker.selected() as usize);
                }
            });
        }

        let weak = Rc::downgrade(&prompt);
        prompt.dialog.connect_cancel(move || {
            if let Some(prompt) = weak.upgrade() {
                prompt.finish(Err("Authentication dialog was dismissed by the user"));
            }
        });
        let dialog = Rc::downgrade(&prompt.dialog);
        cancel.connect_clicked(move |_| {
            if let Some(dialog) = dialog.upgrade() {
                dialog.cancel();
            }
        });
        prompt
    }

    fn selected(&self) -> &User {
        &self.users[self.user.get()]
    }

    fn show_user(&self) {
        let user = self.selected();
        self.avatar.set_text(Some(&user.real_name));
        let texture = user.icon.as_ref().and_then(|path| gdk::Texture::from_filename(path).ok());
        self.avatar.set_custom_image(texture.as_ref());
        self.name.set_label(&user.real_name);
        if user.uid == 0 {
            self.name.add_css_class("root");
        } else {
            self.name.remove_css_class("root");
        }
    }

    fn select(self: &Rc<Self>, index: usize) {
        if index >= self.users.len() || index == self.user.get() || self.done.get() {
            return;
        }
        self.user.set(index);
        self.show_user();
        self.set_message("", false);
        self.start_session();
    }

    fn set_message(&self, text: &str, error: bool) {
        self.message.set_label(text);
        if error {
            self.message.add_css_class("error");
        } else {
            self.message.remove_css_class("error");
        }
    }

    fn entry(&self) -> gtk::Widget {
        if self.echo.get() {
            self.response.clone().upcast()
        } else {
            self.password.clone().upcast()
        }
    }

    /// Starts a new helper run for the selected user.
    fn start_session(self: &Rc<Self>) {
        if self.done.get() {
            return;
        }
        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        self.kill_helper();
        self.prompting.set(false);
        self.answered.set(false);
        self.pam_error.set(false);
        self.password.set_sensitive(false);
        self.response.set_sensitive(false);
        self.authenticate.set_sensitive(false);

        let user = self.selected().name.clone();
        let cookie = self.cookie.clone();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let helper = Helper::start(&user, &cookie).await;
            let current = weak.upgrade().filter(|p| p.generation.get() == generation && !p.done.get());
            match (current, helper) {
                (Some(prompt), Ok(helper)) => {
                    let input = gio::DataInputStream::new(&helper.input);
                    prompt.helper.replace(Some(helper));
                    drop(prompt);
                    read_lines(weak, generation, input).await;
                }
                (Some(prompt), Err(err)) => {
                    glib::g_warning!("shellplus", "polkit: {err}");
                    prompt.finish(Err("Can't run the authentication helper"));
                }
                (None, Ok(helper)) => helper.kill(),
                (None, Err(_)) => {}
            }
        });
    }

    fn kill_helper(&self) {
        if let Some(helper) = self.helper.take() {
            helper.kill();
        }
    }

    /// Handles one line from the helper; false once it's done.
    fn on_line(self: &Rc<Self>, line: &str) -> bool {
        let (kind, text) = match line.split_once(' ') {
            Some((kind, text)) => (kind, unescape(text)),
            None => (line, String::new()),
        };
        match kind {
            "PAM_PROMPT_ECHO_OFF" => self.ask(&text, false),
            "PAM_PROMPT_ECHO_ON" => self.ask(&text, true),
            "PAM_ERROR_MSG" => {
                self.pam_error.set(true);
                self.set_message(text.trim_end(), true);
            }
            "PAM_TEXT_INFO" => self.set_message(text.trim_end(), false),
            "SUCCESS" => {
                self.completed(true);
                return false;
            }
            "FAILURE" => {
                self.completed(false);
                return false;
            }
            _ => glib::g_warning!("shellplus", "polkit: unknown line from the helper: {line}"),
        }
        true
    }

    fn ask(&self, request: &str, echo: bool) {
        self.echo.set(echo);
        self.password.set_visible(!echo);
        self.response.set_visible(echo);
        // PAM's "Password: " reads better as a placeholder without the colon.
        let hint = request.trim().trim_end_matches(':').trim_end();
        let hint = if hint.is_empty() || hint.eq_ignore_ascii_case("password") { "Password" } else { hint };
        self.password.set_placeholder_text(Some(hint));
        self.response.set_placeholder_text(Some(hint));
        self.password.set_text("");
        self.response.set_text("");
        let entry = self.entry();
        entry.set_sensitive(true);
        entry.grab_focus();
        self.authenticate.set_sensitive(true);
        self.prompting.set(true);
    }

    fn respond(&self) {
        if !self.prompting.get() || self.done.get() {
            return;
        }
        let text = if self.echo.get() { self.response.text() } else { self.password.text() };
        let written = self.helper.borrow().as_ref().map(|h| h.write_line(&text));
        if let Some(Err(err)) = written {
            glib::g_warning!("shellplus", "polkit: {err}");
        }
        self.prompting.set(false);
        self.answered.set(true);
        self.pam_error.set(false);
        self.set_message("", false);
        self.password.set_sensitive(false);
        self.response.set_sensitive(false);
        self.authenticate.set_sensitive(false);
    }

    /// The helper finished (or went away).
    fn completed(self: &Rc<Self>, gained: bool) {
        self.kill_helper();
        if gained {
            self.finish(Ok(()));
            return;
        }
        if self.done.get() {
            return;
        }
        // Keep PAM's own explanation if it gave one.
        if !self.pam_error.get() {
            self.set_message(FAILED_TEXT, true);
        }
        if self.answered.get() {
            self.start_session();
        } else {
            // Nothing was asked: don't restart in a tight loop.
            let generation = self.generation.get();
            let weak = Rc::downgrade(self);
            glib::timeout_add_local_once(RETRY_DELAY, move || {
                if let Some(prompt) = weak.upgrade().filter(|p| p.generation.get() == generation) {
                    prompt.start_session();
                }
            });
        }
    }

    /// Answers polkitd (Err = Cancelled with that message) and closes.
    fn finish(&self, result: Result<(), &str>) {
        if self.done.replace(true) {
            return;
        }
        self.generation.set(self.generation.get() + 1);
        self.kill_helper();
        if let Some(request) = self.request.take() {
            match result {
                Ok(()) => request.invocation.return_value(None),
                Err(message) => request.invocation.return_dbus_error(ERROR_CANCELLED, message),
            }
        }
        self.dialog.close();
        if let Some(agent) = self.agent.upgrade() {
            agent.finish();
        }
    }
}

async fn read_lines(prompt: Weak<Prompt>, generation: u64, input: gio::DataInputStream) {
    loop {
        let line = input.read_line_utf8_future(glib::Priority::DEFAULT).await;
        let Some(prompt) = prompt.upgrade().filter(|p| p.generation.get() == generation) else { return };
        match line {
            Ok(Some(line)) => {
                if !prompt.on_line(&line) {
                    return;
                }
            }
            Ok(None) | Err(_) => {
                // The helper exited without a verdict.
                prompt.completed(false);
                return;
            }
        }
    }
}

fn helper_program() -> String {
    std::env::var(HELPER_ENV).ok().filter(|p| !p.is_empty()).unwrap_or_else(|| HELPER.to_owned())
}

fn helper_socket() -> Option<String> {
    match std::env::var(HELPER_SOCKET_ENV).ok().filter(|p| !p.is_empty()) {
        Some(path) => Some(path),
        // A test helper means a test session: don't use the real socket.
        None if std::env::var_os(HELPER_ENV).is_some() => None,
        None => Some(HELPER_SOCKET.to_owned()),
    }
}

/// What the helper's g_strescape() did, undone (like g_strcompress()).
fn unescape(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let byte = bytes[i];
        i += 1;
        if byte != b'\\' {
            out.push(byte);
            continue;
        }
        let Some(&next) = bytes.get(i) else { break };
        i += 1;
        match next {
            b'0'..=b'7' => {
                let mut value = u32::from(next - b'0');
                for _ in 0..2 {
                    match bytes.get(i) {
                        Some(&d @ b'0'..=b'7') => {
                            value = value * 8 + u32::from(d - b'0');
                            i += 1;
                        }
                        _ => break,
                    }
                }
                out.push(value as u8);
            }
            b'b' => out.push(8),
            b'f' => out.push(12),
            b'n' => out.push(b'\n'),
            b'r' => out.push(b'\r'),
            b't' => out.push(b'\t'),
            b'v' => out.push(11),
            other => out.push(other),
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// polkitd translates messages into this.
fn locale() -> String {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .find_map(|var| std::env::var(var).ok().filter(|v| !v.is_empty()))
        .unwrap_or_else(|| "en_US.UTF-8".to_owned())
}

/// Our logind session: the one this process is in, else XDG_SESSION_ID,
/// else the user's display session (when started from a user service).
fn session_id(conn: &gio::DBusConnection) -> Option<String> {
    let pid = std::process::id();
    let path_of = |method: &str| {
        system::call_sync(conn, LOGIND, LOGIND_PATH, LOGIND_MANAGER, method, Some(&(pid,).to_variant()))
            .and_then(|reply| reply.child_value(0).str().map(str::to_owned))
    };
    let property = |path: &str, iface: &str, name: &str| {
        system::call_sync(
            conn,
            LOGIND,
            path,
            "org.freedesktop.DBus.Properties",
            "Get",
            Some(&(iface, name).to_variant()),
        )
        .and_then(|reply| reply.child_value(0).as_variant())
    };
    let own = path_of("GetSessionByPID")
        .and_then(|path| property(&path, "org.freedesktop.login1.Session", "Id"))
        .and_then(|id| id.str().map(str::to_owned));
    own.filter(|id| !id.is_empty())
        .or_else(|| std::env::var("XDG_SESSION_ID").ok().filter(|id| !id.is_empty()))
        .or_else(|| {
            let user = path_of("GetUserByPID")?;
            let display = property(&user, "org.freedesktop.login1.User", "Display")?;
            Some(display.child_value(0).str()?.to_owned()).filter(|id| !id.is_empty())
        })
}

/// The users behind polkit's identities (groups are expanded to their
/// members), in order, without duplicates.
fn resolve_users(conn: &gio::DBusConnection, identities: &glib::Variant) -> Vec<User> {
    let mut uids: Vec<u32> = Vec::new();
    for identity in identities.iter() {
        let details = Props(identity.child_value(1));
        match identity.child_value(0).str() {
            Some("unix-user") => uids.extend(details.get::<u32>("uid")),
            Some("unix-group") => {
                let Some(gid) = details.get::<u32>("gid") else { continue };
                for member in group_members(&gid.to_string()) {
                    if let Some(uid) = getent("passwd", &member).and_then(|pw| pw.get(2)?.parse().ok()) {
                        uids.push(uid);
                    }
                }
            }
            _ => {}
        }
    }
    let mut seen = Vec::new();
    uids.retain(|uid| {
        let new = !seen.contains(uid);
        seen.push(*uid);
        new
    });
    uids.into_iter().filter_map(|uid| lookup_user(conn, uid)).collect()
}

/// Real name, avatar and account type from AccountsService, falling back
/// to the passwd entry.
fn lookup_user(conn: &gio::DBusConnection, uid: u32) -> Option<User> {
    let passwd = getent("passwd", &uid.to_string()).unwrap_or_default();
    let field = |i: usize| passwd.get(i).cloned().unwrap_or_default();
    let accounts = system::call_sync(
        conn,
        ACCOUNTS,
        ACCOUNTS_PATH,
        ACCOUNTS,
        "FindUserById",
        Some(&(i64::from(uid),).to_variant()),
    )
    .and_then(|reply| reply.child_value(0).str().map(str::to_owned))
    .and_then(|path| system::get_all(conn, ACCOUNTS, &path, ACCOUNTS_USER));

    let name = Some(field(0))
        .filter(|n| !n.is_empty())
        .or_else(|| accounts.as_ref().map(|a| a.string("UserName")).filter(|n| !n.is_empty()))?;
    let gecos = field(4).split(',').next().unwrap_or_default().trim().to_owned();
    let real_name = accounts
        .as_ref()
        .map(|a| a.string("RealName"))
        .filter(|n| !n.is_empty())
        .or(Some(gecos).filter(|n| !n.is_empty()))
        .unwrap_or_else(|| name.clone());
    let icon = accounts
        .as_ref()
        .map(|a| a.string("IconFile"))
        .filter(|path| !path.is_empty() && Path::new(path).is_file());
    let admin = uid == 0
        || match accounts.as_ref().and_then(|a| a.get::<i32>("AccountType")) {
            Some(account_type) => account_type == 1,
            None => ["wheel", "sudo", "admin"].iter().any(|group| {
                getent("group", group).is_some_and(|gr| {
                    gr.get(2) == passwd.get(3) || gr.get(3).is_some_and(|m| m.split(',').any(|m| m == name))
                })
            }),
        };
    Some(User { uid, name, real_name, icon, admin })
}

fn group_members(group: &str) -> Vec<String> {
    let Some(entry) = getent("group", group) else { return Vec::new() };
    let mut members: Vec<String> = entry
        .get(3)
        .map(|m| m.split(',').filter(|m| !m.is_empty()).map(str::to_owned).collect())
        .unwrap_or_default();
    // Users whose primary group it is aren't listed as members.
    if let Some(gid) = entry.get(2) {
        if let Ok(output) = std::process::Command::new("getent").arg("passwd").output() {
            for line in String::from_utf8_lossy(&output.stdout).lines() {
                let fields: Vec<&str> = line.split(':').collect();
                if fields.get(3) == Some(&gid.as_str()) && !members.iter().any(|m| m == fields[0]) {
                    members.push(fields[0].to_owned());
                }
            }
        }
    }
    members
}

/// One entry of a NSS database (`passwd`, `group`), split at the colons.
fn getent(database: &str, key: &str) -> Option<Vec<String>> {
    let output = std::process::Command::new("getent").args([database, key]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    Some(text.lines().next()?.split(':').map(str::to_owned).collect())
}

fn load_css() {
    static LOADED: Once = Once::new();
    LOADED.call_once(|| {
        let Some(display) = gdk::Display::default() else { return };
        let provider = gtk::CssProvider::new();
        provider.load_from_string(CSS);
        gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
    });
}

#[cfg(test)]
mod tests {
    use super::unescape;

    #[test]
    fn unescapes_like_g_strcompress() {
        assert_eq!(unescape("Password: "), "Password: ");
        assert_eq!(unescape(r"Line one\nline two"), "Line one\nline two");
        assert_eq!(unescape(r#"caf\303\251 \\ \"x\""#), "café \\ \"x\"");
        assert_eq!(unescape(r"trailing\"), "trailing");
    }
}
