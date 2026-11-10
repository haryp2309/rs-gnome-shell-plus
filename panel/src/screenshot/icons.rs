//! GNOME Shell's screenshot UI icons (selection, screen, window, pointer),
//! which aren't part of Adwaita. They're written to a runtime directory
//! and added to the icon theme so GTK recolours them like any symbolic icon.

use gtk::{gdk, glib};
use std::sync::Once;

pub const AREA: &str = "shellplus-screenshot-area-symbolic";
pub const DISPLAY: &str = "shellplus-screenshot-display-symbolic";
pub const WINDOW: &str = "shellplus-screenshot-window-symbolic";
pub const POINTER: &str = "shellplus-screenshot-pointer-symbolic";

const ICONS: [(&str, &str); 4] = [
    (
        AREA,
        r#"<path d="M1 1h5v2H3v3H1zM10 1h5v5h-2V3h-3zM1 10h2v3h3v2H1zM13 10h2v5h-5v-2h3z"/>
           <path d="M7 1h2v2H7zM7 13h2v2H7zM1 7h2v2H1zM13 7h2v2h-2z" opacity=".5"/>"#,
    ),
    (
        DISPLAY,
        r#"<path fill-rule="evenodd" d="M2.5 1A1.5 1.5 0 0 0 1 2.5v8A1.5 1.5 0 0 0 2.5 12H7v1.5H5V15h6v-1.5H9V12h4.5a1.5 1.5 0 0 0 1.5-1.5v-8A1.5 1.5 0 0 0 13.5 1zm.5 2h10v7H3z"/>"#,
    ),
    (
        WINDOW,
        r#"<path fill-rule="evenodd" d="M2.5 1A1.5 1.5 0 0 0 1 2.5v11A1.5 1.5 0 0 0 2.5 15h11a1.5 1.5 0 0 0 1.5-1.5v-11A1.5 1.5 0 0 0 13.5 1zM3 5h10v8H3z"/>"#,
    ),
    (POINTER, r#"<path d="M3 1v13l3.3-3.1L8.6 15l2.2-1-2.3-4.1H13z"/>"#),
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
