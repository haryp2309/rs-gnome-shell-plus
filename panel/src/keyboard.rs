//! Keyboard layout indicator: the active layout's short name in the panel,
//! with a menu to pick another one. Hidden with fewer than two layouts, like
//! GNOME Shell. The compositor owns the layouts (see
//! compositor/src/shellplus-input-sources.c); Super+Space is handled there.

use crate::ipc::{InputSource, ShellClient};
use crate::quick_settings::widgets::MenuItem;
use crate::system;
use gtk::glib;
use gtk::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

pub fn build(client: &Rc<ShellClient>) -> gtk::MenuButton {
    let label = gtk::Label::builder().css_classes(["input-source"]).build();
    let items = gtk::Box::new(gtk::Orientation::Vertical, 0);

    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .css_classes(["keyboard-menu"])
        .build();
    content.append(&items);
    content.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    let popover = crate::panel::menu_popover(&content);

    let button = gtk::MenuButton::builder()
        .name("keyboard-button")
        .child(&label)
        .popover(&popover)
        .css_classes(["panel-button"])
        .visible(false)
        .build();

    let current_id = Rc::new(RefCell::new(String::new()));
    content.append(&footer_item(&popover, "keyboard-show-layout", "Show Keyboard Layout", {
        let current_id = current_id.clone();
        move || system::spawn(&["tecla", &current_id.borrow()])
    }));
    content.append(&footer_item(&popover, "keyboard-settings", "Keyboard Settings", || {
        system::open_settings("keyboard")
    }));

    let shown: RefCell<Option<(Vec<InputSource>, i32)>> = RefCell::default();
    client.connect_state(glib::clone!(
        #[weak]
        button,
        #[weak]
        popover,
        #[strong]
        client,
        move |state| {
            let sources = &state.input_sources;
            let index = state.input_source;
            if shown.borrow().as_ref() == Some(&(sources.clone(), index)) {
                return;
            }
            shown.replace(Some((sources.clone(), index)));

            let current = usize::try_from(index).ok().and_then(|i| sources.get(i));
            button.set_visible(sources.len() > 1);
            label.set_label(current.map(|s| s.short_name.as_str()).unwrap_or_default());
            button.set_tooltip_text(current.map(|s| s.display_name.as_str()));
            current_id.replace(current.map(|s| s.id.clone()).unwrap_or_default());

            while let Some(child) = items.first_child() {
                items.remove(&child);
            }
            for (i, source) in sources.iter().enumerate() {
                let item = MenuItem::build(
                    &format!("keyboard-source-{i}"),
                    None,
                    &source.display_name,
                    None,
                    i32::try_from(i) == Ok(index),
                );
                let client = client.clone();
                item.connect_clicked(glib::clone!(
                    #[weak]
                    popover,
                    move |_| {
                        popover.popdown();
                        client.set_input_source(i as i32);
                    }
                ));
                items.append(&item);
            }
        }
    ));

    button
}

fn footer_item(popover: &gtk::Popover, name: &str, label: &str, action: impl Fn() + 'static) -> gtk::Button {
    let item = MenuItem::new(name, label);
    item.connect_clicked(glib::clone!(
        #[weak]
        popover,
        move |_| {
            popover.popdown();
            action();
        }
    ));
    item
}
