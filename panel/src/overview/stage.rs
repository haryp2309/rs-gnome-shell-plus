//! The overview's single container widget. It places its children itself
//! (every frame of an animation), and draws the parts that aren't widgets
//! (background, workspace wallpapers, thumbnails) under them; the
//! overview controller supplies both as callbacks.

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

type AllocateFn = Box<dyn Fn(i32, i32)>;
type DrawFn = Box<dyn Fn(&gtk::Snapshot, f64, f64)>;

mod imp {
    use super::*;
    use std::cell::RefCell;

    #[derive(Default)]
    pub struct Stage {
        pub allocate: RefCell<Option<AllocateFn>>,
        pub draw_below: RefCell<Option<DrawFn>>,
        pub draw_above: RefCell<Option<DrawFn>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Stage {
        const NAME: &'static str = "ShellPlusOverviewStage";
        type Type = super::Stage;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for Stage {
        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for Stage {
        fn measure(&self, _orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            (0, 0, -1, -1)
        }

        fn size_allocate(&self, width: i32, height: i32, _baseline: i32) {
            if let Some(allocate) = self.allocate.borrow().as_ref() {
                allocate(width, height);
            }
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let obj = self.obj();
            let (w, h) = (obj.width() as f64, obj.height() as f64);
            if let Some(draw) = self.draw_below.borrow().as_ref() {
                draw(snapshot, w, h);
            }
            let mut child = obj.first_child();
            while let Some(c) = child {
                obj.snapshot_child(&c, snapshot);
                child = c.next_sibling();
            }
            if let Some(draw) = self.draw_above.borrow().as_ref() {
                draw(snapshot, w, h);
            }
        }
    }
}

glib::wrapper! {
    pub struct Stage(ObjectSubclass<imp::Stage>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Stage {
    pub fn new() -> Self {
        glib::Object::new()
    }

    pub fn set_allocate(&self, f: impl Fn(i32, i32) + 'static) {
        self.imp().allocate.replace(Some(Box::new(f)));
    }

    pub fn set_draw_below(&self, f: impl Fn(&gtk::Snapshot, f64, f64) + 'static) {
        self.imp().draw_below.replace(Some(Box::new(f)));
    }

    pub fn set_draw_above(&self, f: impl Fn(&gtk::Snapshot, f64, f64) + 'static) {
        self.imp().draw_above.replace(Some(Box::new(f)));
    }

    /// Moves `child` (a child of this stage, or a new one) to the top.
    pub fn raise(&self, child: &impl IsA<gtk::Widget>) {
        child.insert_before(self, None::<&gtk::Widget>);
    }
}

impl Default for Stage {
    fn default() -> Self {
        Self::new()
    }
}

/// Places `child` at (x, y) with the given size (rounded).
pub fn place(child: &impl IsA<gtk::Widget>, x: f64, y: f64, w: f64, h: f64) {
    let transform = gtk::gsk::Transform::new()
        .translate(&gtk::graphene::Point::new(x.round() as f32, y.round() as f32));
    child.allocate(w.round().max(0.0) as i32, h.round().max(0.0) as i32, -1, Some(transform));
}
