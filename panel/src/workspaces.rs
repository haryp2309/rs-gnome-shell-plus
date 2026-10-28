//! The GNOME 45+ workspace indicator: one dot per workspace, with the
//! active one stretched into a pill. It is the Activities button: clicking
//! it toggles the overview, scrolling on it switches workspaces.

use crate::ipc::ShellClient;
use gtk::prelude::*;
use gtk::glib;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

pub struct Workspaces {
    root: gtk::Box,
    client: Rc<ShellClient>,
    dots: RefCell<Vec<gtk::Button>>,
    active: Cell<i32>,
}

impl Workspaces {
    pub fn new(client: Rc<ShellClient>, on_activities: Rc<dyn Fn()>) -> Rc<Self> {
        let root = gtk::Box::builder()
            .name("activities")
            .css_classes(["panel-button", "workspaces"])
            .valign(gtk::Align::Fill)
            .tooltip_text("Activities")
            .build();
        let click = gtk::GestureClick::new();
        click.connect_released(move |_, _, _, _| on_activities());
        root.add_controller(click);

        let this = Rc::new(Self {
            root,
            client,
            dots: RefCell::default(),
            active: Cell::new(0),
        });

        let scroll = gtk::EventControllerScroll::new(
            gtk::EventControllerScrollFlags::VERTICAL | gtk::EventControllerScrollFlags::DISCRETE,
        );
        let weak = Rc::downgrade(&this);
        scroll.connect_scroll(move |_, _, dy| {
            if let Some(this) = weak.upgrade() {
                this.step(if dy > 0.0 { 1 } else { -1 });
            }
            glib::Propagation::Stop
        });
        this.root.add_controller(scroll);

        this
    }

    pub fn widget(&self) -> &gtk::Box {
        &self.root
    }

    pub fn update(self: &Rc<Self>, active: i32, n_workspaces: i32) {
        let mut dots = self.dots.borrow_mut();
        let n = n_workspaces.max(1) as usize;

        while dots.len() < n {
            // Clicks go to the whole indicator (the Activities button).
            let dot = gtk::Button::builder()
                .css_classes(["ws-dot"])
                .valign(gtk::Align::Center)
                .can_target(false)
                .focusable(false)
                .build();
            self.root.append(&dot);
            dots.push(dot);
        }
        while dots.len() > n {
            self.root.remove(&dots.pop().unwrap());
        }

        for (index, dot) in dots.iter().enumerate() {
            if index as i32 == active {
                dot.add_css_class("active");
            } else {
                dot.remove_css_class("active");
            }
        }
        self.active.set(active);
    }

    fn step(&self, delta: i32) {
        let n = self.dots.borrow().len() as i32;
        let target = (self.active.get() + delta).clamp(0, n - 1);
        if target != self.active.get() {
            self.client.activate_workspace(target);
        }
    }
}
