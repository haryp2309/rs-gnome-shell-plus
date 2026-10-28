//! Test-only D-Bus interface, enabled with SHELLPLUS_PANEL_DEBUG=1.
//!
//! Lets dev/driver.py operate the panel by GTK widget name inside the
//! headless test session, where there's no pointer to click with:
//!
//!   Activate(name)        open a menu button, click a button, flip a switch
//!   SetValue(name, value) move a slider; for a calendar, select the day
//!                         at Unix time `value` (local time)
//!   CloseMenus()          close every open popover
//!   ListNames() -> as     names of all named widgets
//!   Inspect(name) -> a{sv} a widget's state: sensitive, visible, mapped,
//!                         focused, opacity, `bounds` (x, y, width, height
//!                         in its window), `classes` (CSS classes), and
//!                         text for entries/labels; for a calendar, the
//!                         selected date as text (YYYY-MM-DD) and `marked`
//!                         (the marked days of the month)
//!   SetText(name, text)   set an entry's text
//!   ShowAppsTest()        open a window hosting the dash, app grid and
//!                         app search (see app_grid/test_host.rs)
//!
//! Modules can also [`register`] named hooks for things that aren't
//! widgets (e.g. "overview-toggle"); Activate(name) calls them with 0 and
//! SetValue(name, value) with the value.

use gtk::prelude::*;
use gtk::{gio, glib};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

thread_local! {
    static HOOKS: RefCell<HashMap<String, Rc<dyn Fn(f64)>>> = RefCell::default();
}

/// Registers a debug hook: Activate(name) calls `f(0.0)`, SetValue(name, v)
/// calls `f(v)`. Harmless when the debug interface is off.
pub fn register(name: &str, f: impl Fn(f64) + 'static) {
    HOOKS.with(|hooks| hooks.borrow_mut().insert(name.to_owned(), Rc::new(f)));
}

fn hook(name: &str) -> Option<Rc<dyn Fn(f64)>> {
    HOOKS.with(|hooks| hooks.borrow().get(name).cloned())
}

const NAME: &str = "org.gnome.ShellPlus.PanelDebug";
const PATH: &str = "/org/gnome/ShellPlus/PanelDebug";
const XML: &str = r#"
<node>
  <interface name="org.gnome.ShellPlus.PanelDebug">
    <method name="Activate"><arg type="s" name="name" direction="in"/></method>
    <method name="SetValue">
      <arg type="s" name="name" direction="in"/>
      <arg type="d" name="value" direction="in"/>
    </method>
    <method name="CloseMenus"/>
    <method name="SetText">
      <arg type="s" name="name" direction="in"/>
      <arg type="s" name="text" direction="in"/>
    </method>
    <method name="ShowAppsTest"/>
    <method name="ListNames"><arg type="as" name="names" direction="out"/></method>
    <method name="Inspect">
      <arg type="s" name="name" direction="in"/>
      <arg type="a{sv}" name="state" direction="out"/>
    </method>
  </interface>
</node>"#;

pub fn install() {
    if std::env::var_os("SHELLPLUS_PANEL_DEBUG").is_none() {
        return;
    }
    let Ok(conn) = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) else {
        return;
    };
    let node = gio::DBusNodeInfo::for_xml(XML).expect("valid introspection XML");
    let iface = node.lookup_interface(NAME).expect("interface in XML");

    let registration = conn
        .register_object(PATH, &iface)
        .method_call(|_, _, _, _, method, params, invocation| {
            match handle(method, &params) {
                Ok(reply) => invocation.return_value(reply.as_ref()),
                Err(message) => invocation.return_error(gio::IOErrorEnum::NotFound, &message),
            }
        })
        .build();
    if let Err(err) = registration {
        glib::g_warning!("shellplus", "Debug interface unavailable: {err}");
        return;
    }
    gio::bus_own_name_on_connection(&conn, NAME, gio::BusNameOwnerFlags::NONE, |_, _| {}, |_, _| {});
}

fn handle(method: &str, params: &glib::Variant) -> Result<Option<glib::Variant>, String> {
    match method {
        "Activate" => {
            let (name,) = params.get::<(String,)>().ok_or("bad arguments")?;
            if let Some(hook) = hook(&name) {
                hook(0.0);
                return Ok(None);
            }
            activate(&find(&name)?);
            Ok(None)
        }
        "SetValue" => {
            let (name, value) = params.get::<(String, f64)>().ok_or("bad arguments")?;
            if let Some(hook) = hook(&name) {
                hook(value);
                return Ok(None);
            }
            let widget = find(&name)?;
            if let Some(calendar) = widget.downcast_ref::<gtk::Calendar>() {
                let date = glib::DateTime::from_unix_local(value as i64).map_err(|e| e.to_string())?;
                calendar.select_day(&date);
                return Ok(None);
            }
            let range = widget.downcast_ref::<gtk::Range>().ok_or("not a slider")?;
            range.set_value(value);
            Ok(None)
        }
        "SetText" => {
            let (name, text) = params.get::<(String, String)>().ok_or("bad arguments")?;
            find(&name)?.dynamic_cast::<gtk::Editable>().map_err(|_| "not an entry")?.set_text(&text);
            Ok(None)
        }
        "ShowAppsTest" => {
            crate::app_grid::test_host::open();
            Ok(None)
        }
        "CloseMenus" => {
            for popover in widgets().iter().filter_map(|w| w.downcast_ref::<gtk::Popover>()) {
                popover.popdown();
            }
            Ok(None)
        }
        "ListNames" => {
            let names: Vec<String> = widgets()
                .iter()
                .map(|w| w.widget_name().to_string())
                .filter(|n| !n.starts_with("Gtk") && !n.starts_with("Adw"))
                .collect();
            Ok(Some((names,).to_variant()))
        }
        "Inspect" => {
            let (name,) = params.get::<(String,)>().ok_or("bad arguments")?;
            Ok(Some((inspect(&find(&name)?),).to_variant()))
        }
        _ => Err(format!("unknown method {method}")),
    }
}

fn activate(widget: &gtk::Widget) {
    if let Some(menu_button) = widget.downcast_ref::<gtk::MenuButton>() {
        // An auto-hiding popover takes an input grab, which Mutter only
        // grants in response to real input; without one it's dismissed
        // immediately. Debug activation has no input event, so skip the grab.
        if let Some(popover) = menu_button.popover() {
            popover.set_autohide(false);
        }
        menu_button.popup();
    } else if let Some(toggle) = widget.downcast_ref::<gtk::ToggleButton>() {
        toggle.set_active(!toggle.is_active());
    } else if let Some(button) = widget.downcast_ref::<gtk::Button>() {
        button.emit_clicked();
    } else if let Some(switch) = widget.downcast_ref::<gtk::Switch>() {
        switch.set_active(!switch.is_active());
    } else {
        widget.activate();
    }
}

fn inspect(widget: &gtk::Widget) -> std::collections::HashMap<String, glib::Variant> {
    let mut state = std::collections::HashMap::new();
    state.insert("sensitive".into(), widget.is_sensitive().to_variant());
    state.insert("visible".into(), widget.is_visible().to_variant());
    state.insert("mapped".into(), widget.is_mapped().to_variant());
    state.insert("focused".into(), widget.has_focus().to_variant());
    state.insert("opacity".into(), widget.opacity().to_variant());
    let classes: Vec<String> = widget.css_classes().iter().map(|c| c.to_string()).collect();
    state.insert("classes".into(), classes.to_variant());
    if let Some(bounds) = widget.root().and_then(|root| widget.compute_bounds(&root)) {
        let bounds = (
            bounds.x() as f64,
            bounds.y() as f64,
            bounds.width() as f64,
            bounds.height() as f64,
        );
        state.insert("bounds".into(), bounds.to_variant());
    }
    if let Some(editable) = widget.dynamic_cast_ref::<gtk::Editable>() {
        state.insert("text".into(), editable.text().to_variant());
    } else if let Some(label) = widget.downcast_ref::<gtk::Label>() {
        state.insert("text".into(), label.label().to_variant());
    } else if let Some(calendar) = widget.downcast_ref::<gtk::Calendar>() {
        let date = calendar.date().format("%Y-%m-%d").map(|s| s.to_string()).unwrap_or_default();
        state.insert("text".into(), date.to_variant());
        let marked: Vec<u32> = (1..=31).filter(|&d| calendar.day_is_marked(d)).collect();
        state.insert("marked".into(), marked.to_variant());
    }
    state
}

fn find(name: &str) -> Result<gtk::Widget, String> {
    widgets()
        .into_iter()
        .find(|w| w.widget_name() == name)
        .ok_or_else(|| format!("no widget named {name}"))
}

/// Every widget in every toplevel, including popovers (which are children
/// of their menu buttons).
fn widgets() -> Vec<gtk::Widget> {
    fn collect(widget: &gtk::Widget, out: &mut Vec<gtk::Widget>) {
        out.push(widget.clone());
        let mut child = widget.first_child();
        while let Some(c) = child {
            collect(&c, out);
            child = c.next_sibling();
        }
    }
    let mut out = Vec::new();
    for window in gtk::Window::list_toplevels() {
        collect(&window, &mut out);
    }
    out
}
