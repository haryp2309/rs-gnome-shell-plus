//! Test-only D-Bus interface, enabled with SHELLPLUS_PANEL_DEBUG=1.
//!
//! Lets dev/driver.py operate the panel by GTK widget name inside the
//! headless test session, where there's no pointer to click with:
//!
//!   Activate(name)        open a menu button, click a button, flip a switch
//!   SetValue(name, value) move a slider
//!   CloseMenus()          close every open popover
//!   ListNames() -> as     names of all named widgets
//!   Inspect(name) -> a{sv} a widget's state: sensitive, visible, mapped,
//!                         focused, and text for entries/labels

use gtk::prelude::*;
use gtk::{gio, glib};

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
            activate(&find(&name)?);
            Ok(None)
        }
        "SetValue" => {
            let (name, value) = params.get::<(String, f64)>().ok_or("bad arguments")?;
            let widget = find(&name)?;
            let range = widget.downcast_ref::<gtk::Range>().ok_or("not a slider")?;
            range.set_value(value);
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
    if let Some(editable) = widget.dynamic_cast_ref::<gtk::Editable>() {
        state.insert("text".into(), editable.text().to_variant());
    } else if let Some(label) = widget.downcast_ref::<gtk::Label>() {
        state.insert("text".into(), label.label().to_variant());
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
