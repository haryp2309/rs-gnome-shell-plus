//! Client side of the compositor's `org.gnome.ShellPlus.Overview`
//! interface (see the comment at the top of
//! compositor/src/shellplus-overview.c): the window list, window previews
//! passed through a memfd, window actions and the ShowOverview /
//! WindowsChanged / WindowsDamaged signals.

use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use std::io::Read;
use std::rc::Rc;

const BUS_NAME: &str = "org.gnome.ShellPlus";
const PATH: &str = "/org/gnome/ShellPlus";
const IFACE: &str = "org.gnome.ShellPlus.Overview";

/// One normal window, as listed by the compositor.
#[derive(Clone, Debug, PartialEq)]
pub struct WindowInfo {
    pub id: u64,
    pub title: String,
    pub app_id: String,
    /// -1: on every workspace.
    pub workspace: i32,
    pub monitor: i32,
    /// Frame rectangle in stage coordinates.
    pub rect: (i32, i32, i32, i32),
    pub minimized: bool,
    pub focused: bool,
}

impl WindowInfo {
    fn from_variant(id: u64, props: &glib::Variant) -> Self {
        let dict = glib::VariantDict::new(Some(props));
        Self {
            id,
            title: dict.lookup::<String>("title").ok().flatten().unwrap_or_default(),
            app_id: dict.lookup::<String>("app-id").ok().flatten().unwrap_or_default(),
            workspace: dict.lookup::<i32>("workspace").ok().flatten().unwrap_or(-1),
            monitor: dict.lookup::<i32>("monitor").ok().flatten().unwrap_or(0),
            rect: dict
                .lookup::<(i32, i32, i32, i32)>("rect")
                .ok()
                .flatten()
                .unwrap_or((0, 0, 1, 1)),
            minimized: dict.lookup::<bool>("minimized").ok().flatten().unwrap_or(false),
            focused: dict.lookup::<bool>("focused").ok().flatten().unwrap_or(false),
        }
    }
}

pub enum Event {
    WindowsChanged,
    WindowsDamaged(Vec<u64>),
    ShowOverview(String),
}

pub struct Client {
    connection: Option<gio::DBusConnection>,
}

impl Client {
    pub fn new(on_event: impl Fn(Event) + 'static) -> Rc<Self> {
        let connection = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE).ok();
        if let Some(connection) = &connection {
            #[allow(deprecated)]
            connection.signal_subscribe(
                Some(BUS_NAME),
                Some(IFACE),
                None,
                Some(PATH),
                None,
                gio::DBusSignalFlags::NONE,
                move |_, _, _, _, signal, params| match signal {
                    "WindowsChanged" => on_event(Event::WindowsChanged),
                    "WindowsDamaged" => {
                        if let Some((ids,)) = params.get::<(Vec<u64>,)>() {
                            on_event(Event::WindowsDamaged(ids));
                        }
                    }
                    "ShowOverview" => {
                        if let Some((mode,)) = params.get::<(String,)>() {
                            on_event(Event::ShowOverview(mode));
                        }
                    }
                    _ => {}
                },
            );
        }
        Rc::new(Self { connection })
    }

    /// Bottom-to-top list of windows; `None` if the call failed.
    pub fn list_windows(&self, callback: impl FnOnce(Option<Vec<WindowInfo>>) + 'static) {
        let Some(connection) = &self.connection else {
            callback(None);
            return;
        };
        connection.call(
            Some(BUS_NAME),
            PATH,
            IFACE,
            "ListWindows",
            None,
            Some(glib::VariantTy::new("(a(ta{sv}))").unwrap()),
            gio::DBusCallFlags::NONE,
            5000,
            gio::Cancellable::NONE,
            move |result| match result {
                Ok(reply) => {
                    let list = reply.child_value(0);
                    let windows = list
                        .iter()
                        .filter_map(|item| {
                            let id = item.child_value(0).get::<u64>()?;
                            Some(WindowInfo::from_variant(id, &item.child_value(1)))
                        })
                        .collect();
                    callback(Some(windows));
                }
                Err(err) => {
                    glib::g_warning!("shellplus", "ListWindows failed: {err}");
                    callback(None);
                }
            },
        );
    }

    /// The window's current contents, scaled down to fit max_w x max_h
    /// (physical pixels).
    pub fn get_preview(
        &self,
        id: u64,
        max_w: i32,
        max_h: i32,
        callback: impl FnOnce(Option<gdk::Texture>) + 'static,
    ) {
        let Some(connection) = &self.connection else {
            callback(None);
            return;
        };
        connection.call_with_unix_fd_list(
            Some(BUS_NAME),
            PATH,
            IFACE,
            "GetPreview",
            Some(&(id, max_w.max(1), max_h.max(1)).to_variant()),
            Some(glib::VariantTy::new("(hiii)").unwrap()),
            gio::DBusCallFlags::NONE,
            5000,
            None::<&gio::UnixFDList>,
            gio::Cancellable::NONE,
            move |result| {
                let texture = match result {
                    Ok((reply, fds)) => read_preview(&reply, fds.as_ref()),
                    Err(err) => {
                        glib::g_debug!("shellplus", "GetPreview({id}) failed: {err}");
                        None
                    }
                };
                callback(texture);
            },
        );
    }

    pub fn activate_window(&self, id: u64) {
        self.call("ActivateWindow", (id,).to_variant());
    }

    pub fn close_window(&self, id: u64) {
        self.call("CloseWindow", (id,).to_variant());
    }

    pub fn move_window_to_workspace(&self, id: u64, workspace: i32) {
        self.call("MoveWindowToWorkspace", (id, workspace).to_variant());
    }

    pub fn set_overview_active(&self, active: bool) {
        self.call("SetOverviewActive", (active,).to_variant());
    }

    fn call(&self, method: &str, args: glib::Variant) {
        let Some(connection) = &self.connection else {
            return;
        };
        let name = method.to_owned();
        connection.call(
            Some(BUS_NAME),
            PATH,
            IFACE,
            method,
            Some(&args),
            None,
            gio::DBusCallFlags::NONE,
            -1,
            gio::Cancellable::NONE,
            move |result| {
                if let Err(err) = result {
                    glib::g_warning!("shellplus", "{name} failed: {err}");
                }
            },
        );
    }
}

/// Reads the cairo ARGB32 image out of the memfd in the reply.
fn read_preview(reply: &glib::Variant, fds: Option<&gio::UnixFDList>) -> Option<gdk::Texture> {
    let (index, width, height, stride) = reply.get::<(glib::variant::Handle, i32, i32, i32)>()?;
    if width <= 0 || height <= 0 || stride < width * 4 {
        return None;
    }
    let fd = fds?.get(index.0).ok()?;
    let mut file = std::fs::File::from(fd);
    let mut data = vec![0u8; stride as usize * height as usize];
    file.read_exact(&mut data).ok()?;
    Some(
        gdk::MemoryTexture::new(
            width,
            height,
            // Cairo's ARGB32 in little-endian byte order.
            gdk::MemoryFormat::B8g8r8a8Premultiplied,
            &glib::Bytes::from_owned(data),
            stride as usize,
        )
        .upcast(),
    )
}
