//! Icons for the keyboard's special keys, standing in for GNOME Shell's
//! osk-*-symbolic icons (drawn for this project). Like the screenshot UI's
//! icons, they're written to a runtime directory and added to the icon
//! theme so GTK recolours them like any symbolic icon.

use gtk::{gdk, glib};
use std::sync::Once;

pub const SHIFT: &str = "shellplus-osk-shift-symbolic";
pub const CAPS_LOCK: &str = "shellplus-osk-caps-lock-symbolic";
pub const DELETE: &str = "shellplus-osk-delete-symbolic";
pub const ENTER: &str = "shellplus-osk-enter-symbolic";
pub const HIDE: &str = "shellplus-osk-hide-symbolic";
/// Adwaita's.
pub const LAYOUT: &str = "input-keyboard-symbolic";

const ICONS: [(&str, &str); 5] = [
    (
        SHIFT,
        r#"<path fill-rule="evenodd" d="M8 1 1 8.5h4V14h6V8.5h4zm0 2.2L4.5 7.1h2V12.6h3V7.1h2z"/>"#,
    ),
    (CAPS_LOCK, r#"<path d="M8 1 1 8h4v3.5h6V8h4zM5 13h6v2H5z"/>"#),
    (
        DELETE,
        r#"<path fill-rule="evenodd" d="M5.5 2 .5 8l5 6h10V2zm.6 1.4H14.1v9.2H6.1L2.3 8zM9.26 10.33 10.60 8.99 11.94 10.33 12.93 9.34 11.59 8.00 12.93 6.66 11.94 5.67 10.60 7.01 9.26 5.67 8.27 6.66 9.61 8.00 8.27 9.34z"/>"#,
    ),
    (
        ENTER,
        r#"<path d="M13 3h1.5v7.5H4.8l2.6 2.6-1.05 1.05-4.4-4.4 4.4-4.4L7.4 6.4 4.8 9H13z"/>"#,
    ),
    (
        HIDE,
        r#"<path fill-rule="evenodd" d="M1 1h14v8.5H1zm1.5 1.5v5.5h11V2.5z"/><path d="M3.5 3.5h2V5h-2zM7 3.5h2V5H7zm3.5 0h2V5h-2zM5 6h6v1.2H5zM4.5 11 8 14.5l3.5-3.5-.9-.9L8 12.7l-2.6-2.6z"/>"#,
    ),
];

pub fn install() {
    static DONE: Once = Once::new();
    DONE.call_once(|| {
        let dir = glib::user_runtime_dir().join("gnome-shell-plus-icons");
        if std::fs::create_dir_all(&dir).is_err() {
            return;
        }
        for (name, body) in ICONS {
            let svg = format!(
                r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16" fill="#2e3436">{body}</svg>"##
            );
            let _ = std::fs::write(dir.join(format!("{name}.svg")), svg);
        }
        if let Some(display) = gdk::Display::default() {
            gtk::IconTheme::for_display(&display).add_search_path(&dir);
        }
    });
}
