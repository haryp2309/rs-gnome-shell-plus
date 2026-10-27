//! Client side of the private `org.gnome.ShellPlus.Panel` D-Bus interface
//! exported by the compositor (see compositor/src/shellplus-dbus.c).

use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use std::cell::RefCell;
use std::rc::Rc;

const BUS_NAME: &str = "org.gnome.ShellPlus";
const PATH: &str = "/org/gnome/ShellPlus";
const IFACE: &str = "org.gnome.ShellPlus.Panel";

#[derive(Clone, Debug, PartialEq)]
pub struct ShellState {
    /// A maximized window touches the panel; draw it in `color`.
    pub solid: bool,
    pub color: gdk::RGBA,
    pub active_workspace: i32,
    pub n_workspaces: i32,
    /// Primary monitor geometry (x, y, width, height) in logical pixels.
    pub monitor: Option<(i32, i32, i32, i32)>,
    /// Backlight level 0..1, or `None` without a controllable backlight.
    pub brightness: Option<f64>,
}

impl Default for ShellState {
    fn default() -> Self {
        Self {
            solid: false,
            color: gdk::RGBA::BLACK,
            active_workspace: 0,
            n_workspaces: 1,
            monitor: None,
            brightness: None,
        }
    }
}

impl ShellState {
    fn from_variant(dict: &glib::Variant) -> Self {
        let dict = glib::VariantDict::new(Some(dict));
        let mut state = Self::default();

        if let Ok(Some(mode)) = dict.lookup::<String>("mode") {
            state.solid = mode == "solid";
        }
        if let Ok(Some(color)) = dict.lookup::<String>("color") {
            if let Ok(rgba) = gdk::RGBA::parse(color.as_str()) {
                state.color = rgba;
            }
        }
        if let Ok(Some(active)) = dict.lookup::<i32>("active-workspace") {
            state.active_workspace = active;
        }
        if let Ok(Some(n)) = dict.lookup::<i32>("n-workspaces") {
            state.n_workspaces = n.max(1);
        }
        if let Ok(Some(monitor)) = dict.lookup::<(i32, i32, i32, i32)>("monitor") {
            if monitor.2 > 0 && monitor.3 > 0 {
                state.monitor = Some(monitor);
            }
        }
        if let Ok(Some(brightness)) = dict.lookup::<f64>("brightness") {
            state.brightness = (brightness >= 0.0).then_some(brightness);
        }
        state
    }
}

type StateCallback = Box<dyn Fn(&ShellState)>;

pub struct ShellClient {
    connection: Option<gio::DBusConnection>,
    state: RefCell<ShellState>,
    callbacks: RefCell<Vec<StateCallback>>,
}

impl ShellClient {
    pub fn new() -> Rc<Self> {
        let connection = match gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) {
            Ok(connection) => Some(connection),
            Err(err) => {
                glib::g_warning!("shellplus", "No session bus, running standalone: {err}");
                None
            }
        };

        let client = Rc::new(Self {
            connection,
            state: RefCell::default(),
            callbacks: RefCell::default(),
        });

        if let Some(connection) = &client.connection {
            let weak = Rc::downgrade(&client);
            #[allow(deprecated)]
            connection.signal_subscribe(
                Some(BUS_NAME),
                Some(IFACE),
                Some("StateChanged"),
                Some(PATH),
                None,
                gio::DBusSignalFlags::NONE,
                move |_, _, _, _, _, params| {
                    if let Some(client) = weak.upgrade() {
                        client.set_state(ShellState::from_variant(&params.child_value(0)));
                    }
                },
            );

            // The compositor may (re)start its D-Bus service after us.
            let weak = Rc::downgrade(&client);
            gio::bus_watch_name_on_connection(
                connection,
                BUS_NAME,
                gio::BusNameWatcherFlags::NONE,
                move |_, _, _| {
                    if let Some(client) = weak.upgrade() {
                        client.fetch_state();
                    }
                },
                |_, _| {},
            );
        }

        client
    }

    pub fn state(&self) -> ShellState {
        self.state.borrow().clone()
    }

    /// Calls `callback` now and whenever the compositor state changes.
    pub fn connect_state(&self, callback: impl Fn(&ShellState) + 'static) {
        callback(&self.state.borrow());
        self.callbacks.borrow_mut().push(Box::new(callback));
    }

    pub fn activate_workspace(&self, index: i32) {
        self.call("ActivateWorkspace", Some((index,).to_variant()));
    }

    pub fn set_brightness(&self, value: f64) {
        self.call("SetBrightness", Some((value,).to_variant()));
    }

    pub fn log_out(&self) {
        self.call("LogOut", None);
    }

    fn set_state(&self, state: ShellState) {
        if *self.state.borrow() == state {
            return;
        }
        self.state.replace(state.clone());
        for callback in self.callbacks.borrow().iter() {
            callback(&state);
        }
    }

    fn fetch_state(self: &Rc<Self>) {
        let Some(connection) = &self.connection else {
            return;
        };
        let weak = Rc::downgrade(self);
        connection.call(
            Some(BUS_NAME),
            PATH,
            IFACE,
            "GetState",
            None,
            Some(glib::VariantTy::new("(a{sv})").unwrap()),
            gio::DBusCallFlags::NONE,
            -1,
            gio::Cancellable::NONE,
            move |result| match result {
                Ok(reply) => {
                    if let Some(client) = weak.upgrade() {
                        client.set_state(ShellState::from_variant(&reply.child_value(0)));
                    }
                }
                Err(err) => glib::g_warning!("shellplus", "GetState failed: {err}"),
            },
        );
    }

    fn call(&self, method: &str, args: Option<glib::Variant>) {
        let Some(connection) = &self.connection else {
            return;
        };
        let method_name = method.to_owned();
        connection.call(
            Some(BUS_NAME),
            PATH,
            IFACE,
            method,
            args.as_ref(),
            None,
            gio::DBusCallFlags::NONE,
            -1,
            gio::Cancellable::NONE,
            move |result| {
                if let Err(err) = result {
                    glib::g_warning!("shellplus", "{method_name} failed: {err}");
                }
            },
        );
    }
}
