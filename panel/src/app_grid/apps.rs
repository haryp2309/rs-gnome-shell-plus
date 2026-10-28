//! Installed applications: lookup, desktop file details, launching, and the
//! running apps the overview tells us about.
//!
//! `gio::DesktopAppInfo` lives in the gio-unix crate, which the panel doesn't
//! depend on, so its few functions we need are declared here; they are part
//! of libgio itself. They are only called on objects that really are
//! GDesktopAppInfos.

use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use std::cell::RefCell;
use std::ffi::{c_char, c_void, CStr, CString};
use std::rc::Rc;

mod ffi {
    use super::*;
    extern "C" {
        pub fn g_desktop_app_info_get_string(info: *mut c_void, key: *const c_char) -> *mut c_char;
        pub fn g_desktop_app_info_get_generic_name(info: *mut c_void) -> *const c_char;
        pub fn g_desktop_app_info_get_categories(info: *mut c_void) -> *const c_char;
        pub fn g_desktop_app_info_get_keywords(info: *mut c_void) -> *const *const c_char;
        pub fn g_desktop_app_info_list_actions(info: *mut c_void) -> *const *const c_char;
        pub fn g_desktop_app_info_launch_action(info: *mut c_void, action: *const c_char, ctx: *mut c_void);
    }
}

/// An installed application.
#[derive(Clone)]
pub struct App {
    pub info: gio::AppInfo,
    /// Desktop file id without ".desktop".
    pub id: String,
    pub name: String,
}

impl App {
    pub fn new(info: gio::AppInfo) -> Option<Self> {
        let id = info.id()?.trim_end_matches(".desktop").to_owned();
        let name = info.display_name().to_string();
        Some(Self { info, id, name })
    }

    pub fn desktop_id(&self) -> String {
        format!("{}.desktop", self.id)
    }

    fn desktop_ptr(&self) -> Option<*mut c_void> {
        use glib::object::ObjectType;
        (self.info.type_().name() == "GDesktopAppInfo").then(|| self.info.as_ptr() as *mut c_void)
    }

    pub fn string(&self, key: &str) -> Option<String> {
        let ptr = self.desktop_ptr()?;
        let key = CString::new(key).ok()?;
        // SAFETY: a GDesktopAppInfo; transfer full.
        unsafe {
            let value = ffi::g_desktop_app_info_get_string(ptr, key.as_ptr());
            if value.is_null() {
                return None;
            }
            let s = CStr::from_ptr(value).to_string_lossy().into_owned();
            glib::ffi::g_free(value as *mut _);
            Some(s)
        }
    }

    pub fn generic_name(&self) -> Option<String> {
        let ptr = self.desktop_ptr()?;
        // SAFETY: a GDesktopAppInfo; transfer none.
        unsafe { borrowed(ffi::g_desktop_app_info_get_generic_name(ptr)) }
    }

    pub fn categories(&self) -> Vec<String> {
        let Some(ptr) = self.desktop_ptr() else { return Vec::new() };
        // SAFETY: a GDesktopAppInfo; transfer none.
        let categories = unsafe { borrowed(ffi::g_desktop_app_info_get_categories(ptr)) };
        categories
            .map(|c| c.split(';').filter(|c| !c.is_empty()).map(str::to_owned).collect())
            .unwrap_or_default()
    }

    pub fn keywords(&self) -> Vec<String> {
        let Some(ptr) = self.desktop_ptr() else { return Vec::new() };
        // SAFETY: a GDesktopAppInfo; transfer none.
        unsafe { strv(ffi::g_desktop_app_info_get_keywords(ptr)) }
    }

    pub fn actions(&self) -> Vec<String> {
        let Some(ptr) = self.desktop_ptr() else { return Vec::new() };
        // SAFETY: a GDesktopAppInfo; transfer none.
        unsafe { strv(ffi::g_desktop_app_info_list_actions(ptr)) }
    }

    pub fn has_new_window_action(&self) -> bool {
        self.actions().iter().any(|a| a == "new-window")
    }

    pub fn icon(&self) -> Option<gio::Icon> {
        self.info.icon()
    }

    /// Launches the app (or one of its desktop actions) with a launch
    /// context from the display, so startup notification works.
    pub fn launch(&self, action: Option<&str>) {
        let context = gdk::Display::default().map(|d| d.app_launch_context());
        match (action, self.desktop_ptr()) {
            (Some(action), Some(ptr)) => {
                use glib::object::ObjectType;
                let Ok(action) = CString::new(action) else { return };
                let ctx = context.as_ref().map(|c| c.as_ptr() as *mut c_void).unwrap_or(std::ptr::null_mut());
                // SAFETY: a GDesktopAppInfo and a GAppLaunchContext (or NULL).
                unsafe { ffi::g_desktop_app_info_launch_action(ptr, action.as_ptr(), ctx) };
            }
            _ => {
                if let Err(err) = self.info.launch(&[], context.as_ref()) {
                    glib::g_warning!("shellplus", "Failed to launch {}: {err}", self.id);
                }
            }
        }
    }
}

unsafe fn borrowed(ptr: *const c_char) -> Option<String> {
    (!ptr.is_null()).then(|| CStr::from_ptr(ptr).to_string_lossy().into_owned())
}

unsafe fn strv(mut ptr: *const *const c_char) -> Vec<String> {
    let mut out = Vec::new();
    if ptr.is_null() {
        return out;
    }
    while !(*ptr).is_null() {
        out.push(CStr::from_ptr(*ptr).to_string_lossy().into_owned());
        ptr = ptr.add(1);
    }
    out
}

/// Every installed app that should show in GNOME (NoDisplay=false,
/// OnlyShowIn/NotShowIn respected), sorted by name.
pub fn visible_apps() -> Vec<App> {
    let mut apps: Vec<App> = gio::AppInfo::all()
        .into_iter()
        .filter(|info| info.should_show())
        .filter_map(App::new)
        .collect();
    apps.sort_by_cached_key(|app| (app.name.to_lowercase(), app.id.clone()));
    apps
}

/// The installed app with desktop file id `id` (with or without
/// ".desktop"), matched case-insensitively.
pub fn lookup(id: &str) -> Option<App> {
    let wanted = format!("{}.desktop", id.trim_end_matches(".desktop")).to_lowercase();
    gio::AppInfo::all()
        .into_iter()
        .find(|info| info.id().is_some_and(|i| i.to_lowercase() == wanted))
        .and_then(App::new)
}

/// The app a window's app id (a GTK application id or a WM_CLASS) belongs
/// to: `<id>.desktop` case-insensitively, else a desktop file whose
/// StartupWMClass matches, else one whose id's last component matches
/// (e.g. "firefox" → org.mozilla.firefox.desktop).
pub fn resolve_running(app_id: &str, apps: &[App]) -> Option<App> {
    if app_id.is_empty() {
        return None;
    }
    let lower = app_id.to_lowercase();
    let all: Vec<App> = gio::AppInfo::all().into_iter().filter_map(App::new).collect();
    let pool = |f: &dyn Fn(&App) -> bool| apps.iter().chain(all.iter()).find(|a| f(a)).cloned();
    pool(&|a| a.id.to_lowercase() == lower)
        .or_else(|| pool(&|a| a.string("StartupWMClass").is_some_and(|w| w.to_lowercase() == lower)))
        .or_else(|| pool(&|a| a.id.rsplit('.').next().is_some_and(|last| last.to_lowercase() == lower)))
}

// --- running apps and activation ----------------------------------------------

type ActivateRunning = Rc<dyn Fn(&str)>;

thread_local! {
    /// Window app ids the overview last passed to `Dash::set_running`.
    static RUNNING: RefCell<Vec<String>> = RefCell::default();
    static ACTIVATE_RUNNING: RefCell<Option<ActivateRunning>> = RefCell::default();
}

pub fn set_running(ids: &[String]) {
    RUNNING.with(|r| *r.borrow_mut() = ids.to_vec());
}

pub fn set_activate_running(f: impl Fn(&str) + 'static) {
    ACTIVATE_RUNNING.with(|a| *a.borrow_mut() = Some(Rc::new(f)));
}

/// The running window app id that belongs to `app`, if any.
pub fn running_id(app: &App) -> Option<String> {
    let running = RUNNING.with(|r| r.borrow().clone());
    let wm_class = app.string("StartupWMClass").map(|w| w.to_lowercase());
    let id = app.id.to_lowercase();
    let last = id.rsplit('.').next().unwrap_or(&id).to_owned();
    running.into_iter().find(|r| {
        let r = r.to_lowercase();
        r == id || wm_class.as_deref() == Some(r.as_str()) || r == last
    })
}

/// What clicking an app icon does: raise the running app when the overview
/// can, otherwise launch it; then tell the overview.
pub fn activate(app: &App) {
    let raise = ACTIVATE_RUNNING.with(|a| a.borrow().clone());
    match (running_id(app), raise) {
        (Some(id), Some(raise)) => raise(&id),
        _ => app.launch(None),
    }
    super::emit_launched();
}

/// Raises a running app that has no desktop file.
pub fn activate_running_id(id: &str) {
    if let Some(raise) = ACTIVATE_RUNNING.with(|a| a.borrow().clone()) {
        raise(id);
    }
    super::emit_launched();
}

/// Opens the app in GNOME Software, when it's installed.
pub fn show_details(app: &App) {
    crate::system::spawn(&["gnome-software", &format!("--details={}", app.desktop_id())]);
}

pub fn software_installed() -> bool {
    glib::find_program_in_path("gnome-software").is_some()
}
