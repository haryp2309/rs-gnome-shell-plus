//! What happens to a capture: a PNG in ~/Pictures/Screenshots, the
//! clipboard, and GNOME's "Screenshot captured" notification with a
//! "Show in Files" action.

use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

const FDO: &str = "org.freedesktop.Notifications";
const FDO_PATH: &str = "/org/freedesktop/Notifications";

/// ~/Pictures/Screenshots (the XDG pictures directory, else ~/Pictures).
pub fn screenshots_dir() -> PathBuf {
    let home = glib::home_dir();
    let pictures = glib::user_special_dir(glib::UserDirectory::Pictures)
        .filter(|dir| !dir.as_os_str().is_empty() && *dir != home)
        .unwrap_or_else(|| home.join("Pictures"));
    pictures.join("Screenshots")
}

/// Saves as "Screenshot From YYYY-MM-DD HH-MM-SS.png" (adding -1, -2, …
/// if taken) and returns the path.
pub fn save_png(texture: &gdk::Texture) -> std::io::Result<PathBuf> {
    let dir = screenshots_dir();
    std::fs::create_dir_all(&dir)?;
    let stamp = glib::DateTime::now_local()
        .and_then(|now| now.format("%Y-%m-%d %H-%M-%S"))
        .map(|s| s.to_string())
        .unwrap_or_default();
    let base = format!("Screenshot From {stamp}");
    let png = texture.save_to_png_bytes();
    for i in 0..1000 {
        let name = if i == 0 { format!("{base}.png") } else { format!("{base}-{i}.png") };
        let path = dir.join(name);
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                use std::io::Write;
                file.write_all(&png)?;
                return Ok(path);
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
    }
    Err(std::io::Error::other("too many screenshots with the same name"))
}

/// Offers the image (image/png and friends) on the clipboard.
pub fn copy(texture: &gdk::Texture) {
    if let Some(display) = gdk::Display::default() {
        display.clipboard().set_texture(texture);
    }
}

thread_local! {
    /// Our notifications' ids -> the files they're about.
    static POSTED: RefCell<HashMap<u32, PathBuf>> = RefCell::default();
    static SUBSCRIBED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Posts "Screenshot captured" through org.freedesktop.Notifications
/// (served by the panel itself), with the image as its icon.
pub fn notify(path: &Path) {
    post(
        path,
        "camera-photo-symbolic",
        "Screenshot captured",
        "You can paste the image from the clipboard.",
        true,
    );
}

/// Posts a notification about `path` with GNOME's "Show in Files" action
/// (clicking it opens the file); `image` makes the file its icon.
pub fn post(path: &Path, icon: &str, summary: &str, body: &str, image: bool) {
    let Ok(conn) = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) else {
        return;
    };
    subscribe(&conn);

    let hints = glib::VariantDict::new(None);
    if image {
        hints.insert("image-path", path.to_string_lossy().as_ref());
    }
    let actions = ["default", "", "show-in-files", "Show in Files"];
    let args = glib::Variant::tuple_from_iter([
        "Screenshot".to_variant(),
        0u32.to_variant(),
        icon.to_variant(),
        summary.to_variant(),
        body.to_variant(),
        actions[..].to_variant(),
        hints.end(),
        (-1i32).to_variant(),
    ]);
    let path = path.to_owned();
    conn.call(
        Some(FDO),
        FDO_PATH,
        FDO,
        "Notify",
        Some(&args),
        Some(glib::VariantTy::new("(u)").unwrap()),
        gio::DBusCallFlags::NONE,
        -1,
        gio::Cancellable::NONE,
        move |result| match result {
            Ok(reply) => {
                if let Some((id,)) = reply.get::<(u32,)>() {
                    POSTED.with(|posted| posted.borrow_mut().insert(id, path));
                }
            }
            Err(err) => glib::g_warning!("shellplus", "Screenshot notification failed: {err}"),
        },
    );
}

fn subscribe(conn: &gio::DBusConnection) {
    if SUBSCRIBED.with(|s| s.replace(true)) {
        return;
    }
    #[allow(deprecated)]
    conn.signal_subscribe(
        None,
        Some(FDO),
        None,
        Some(FDO_PATH),
        None,
        gio::DBusSignalFlags::NONE,
        |conn, _, _, _, signal, params| match signal {
            "ActionInvoked" => {
                let Some((id, action)) = params.get::<(u32, String)>() else { return };
                let Some(path) = POSTED.with(|posted| posted.borrow().get(&id).cloned()) else { return };
                if action == "show-in-files" {
                    show_in_files(conn, &path);
                } else {
                    open(&gio::File::for_path(&path).uri());
                }
            }
            "NotificationClosed" => {
                if let Some((id, _)) = params.get::<(u32, u32)>() {
                    POSTED.with(|posted| posted.borrow_mut().remove(&id));
                }
            }
            _ => {}
        },
    );
}

/// Selects the file in the file manager (org.freedesktop.FileManager1),
/// or opens its folder.
fn show_in_files(conn: &gio::DBusConnection, path: &Path) {
    let uri = gio::File::for_path(path).uri().to_string();
    let folder = path.parent().map(|dir| gio::File::for_path(dir).uri().to_string());
    conn.call(
        Some("org.freedesktop.FileManager1"),
        "/org/freedesktop/FileManager1",
        "org.freedesktop.FileManager1",
        "ShowItems",
        Some(&(vec![uri], "").to_variant()),
        None,
        gio::DBusCallFlags::NONE,
        5000,
        gio::Cancellable::NONE,
        move |result| {
            if result.is_err() {
                if let Some(folder) = folder {
                    open(&folder);
                }
            }
        },
    );
}

fn open(uri: &str) {
    if let Err(err) = gio::AppInfo::launch_default_for_uri(uri, gio::AppLaunchContext::NONE) {
        glib::g_warning!("shellplus", "Could not open {uri}: {err}");
    }
}
