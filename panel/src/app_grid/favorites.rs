//! The favourite apps: org.gnome.shell `favorite-apps` when GNOME Shell's
//! schemas are installed; otherwise our own key file
//! (`~/.config/gnome-shell-plus/apps.ini`, `[Dash] favorite-apps`), which
//! starts out as GNOME's default favourites that are installed.

use super::apps;
use gtk::prelude::*;
use gtk::{gio, glib};
use std::cell::RefCell;
use std::rc::Rc;

const SHELL_SCHEMA: &str = "org.gnome.shell";
const KEY: &str = "favorite-apps";
const GROUP: &str = "Dash";

/// GNOME's (and Fedora's) default favourites; the first installed browser
/// is used.
const BROWSERS: &[&str] = &["firefox.desktop", "org.mozilla.firefox.desktop", "org.gnome.Epiphany.desktop"];
const DEFAULTS: &[&str] = &[
    "org.gnome.Calendar.desktop",
    "org.gnome.Nautilus.desktop",
    "org.gnome.TextEditor.desktop",
    "org.gnome.Software.desktop",
    "org.gnome.Settings.desktop",
];

thread_local! {
    static STORE: Rc<Store> = Rc::new(Store::new());
}

pub struct Store {
    settings: Option<gio::Settings>,
    listeners: RefCell<Vec<Rc<dyn Fn()>>>,
}

pub fn store() -> Rc<Store> {
    STORE.with(Rc::clone)
}

impl Store {
    fn new() -> Self {
        let settings = crate::system::settings(SHELL_SCHEMA).filter(|s| {
            s.settings_schema().is_some_and(|schema| schema.has_key(KEY))
        });
        Self { settings, listeners: RefCell::default() }
    }

    /// Desktop file ids (with ".desktop"), in order.
    pub fn get(&self) -> Vec<String> {
        if let Some(settings) = &self.settings {
            return settings.strv(KEY).iter().map(|s| s.to_string()).collect();
        }
        let file = glib::KeyFile::new();
        if file.load_from_file(key_file(), glib::KeyFileFlags::NONE).is_ok() {
            if let Ok(list) = file.string_list(GROUP, KEY) {
                return list.iter().map(|s| s.to_string()).collect();
            }
        }
        BROWSERS
            .iter()
            .find(|id| apps::lookup(id).is_some())
            .into_iter()
            .chain(DEFAULTS.iter().filter(|id| apps::lookup(id).is_some()))
            .map(|id| id.to_string())
            .collect()
    }

    pub fn set(&self, ids: &[String]) {
        let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
        if let Some(settings) = &self.settings {
            if let Err(err) = settings.set_strv(KEY, refs.as_slice()) {
                glib::g_warning!("shellplus", "Couldn't save favourites: {err}");
            }
            // The settings' "changed" signal notifies the listeners.
            return;
        }
        let path = key_file();
        let file = glib::KeyFile::new();
        let _ = file.load_from_file(&path, glib::KeyFileFlags::KEEP_COMMENTS);
        // A string list; desktop ids contain no ';'.
        file.set_string(GROUP, KEY, &format!("{};", refs.join(";")));
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Err(err) = file.save_to_file(&path) {
            glib::g_warning!("shellplus", "Couldn't save favourites: {err}");
        }
        self.notify();
    }

    pub fn contains(&self, desktop_id: &str) -> bool {
        self.get().iter().any(|id| id.eq_ignore_ascii_case(desktop_id))
    }

    pub fn add(&self, desktop_id: &str, position: Option<usize>) {
        let mut ids = self.get();
        ids.retain(|id| !id.eq_ignore_ascii_case(desktop_id));
        let position = position.unwrap_or(ids.len()).min(ids.len());
        ids.insert(position, desktop_id.to_owned());
        self.set(&ids);
    }

    pub fn remove(&self, desktop_id: &str) {
        let mut ids = self.get();
        ids.retain(|id| !id.eq_ignore_ascii_case(desktop_id));
        self.set(&ids);
    }

    pub fn connect_changed(self: &Rc<Self>, f: impl Fn() + 'static) {
        let f: Rc<dyn Fn()> = Rc::new(f);
        if let Some(settings) = &self.settings {
            let f = f.clone();
            settings.connect_changed(Some(KEY), move |_, _| f());
        }
        self.listeners.borrow_mut().push(f);
    }

    fn notify(&self) {
        let listeners: Vec<_> = self.listeners.borrow().clone();
        for f in listeners {
            f();
        }
    }
}

fn key_file() -> std::path::PathBuf {
    glib::user_config_dir().join("gnome-shell-plus").join("apps.ini")
}
