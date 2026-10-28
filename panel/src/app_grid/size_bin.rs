//! A one-child container that reports its allocated size, so the grid and
//! the search results can fit as many icons as there is room for (GTK has
//! no size-allocate signal). It asks for no minimum size of its own; a
//! child that needs more than it gets is clipped.

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

mod imp {
    use super::*;
    use std::cell::{Cell, RefCell};

    #[derive(Default)]
    pub struct SizeBin {
        pub child: RefCell<Option<gtk::Widget>>,
        pub callback: RefCell<Option<Box<dyn Fn(i32, i32)>>>,
        pub last: Cell<(i32, i32)>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for SizeBin {
        const NAME: &'static str = "ShellPlusSizeBin";
        type Type = super::SizeBin;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for SizeBin {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().set_overflow(gtk::Overflow::Hidden);
        }

        fn dispose(&self) {
            if let Some(child) = self.child.take() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for SizeBin {
        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            match &*self.child.borrow() {
                Some(child) => {
                    let (_, natural, _, _) = child.measure(orientation, for_size);
                    (0, natural, -1, -1)
                }
                None => (0, 0, -1, -1),
            }
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            if let Some(child) = &*self.child.borrow() {
                let (min_w, _, _, _) = child.measure(gtk::Orientation::Horizontal, -1);
                let (min_h, _, _, _) = child.measure(gtk::Orientation::Vertical, width.max(min_w));
                child.allocate(width.max(min_w), height.max(min_h), baseline, None);
            }
            if self.last.replace((width, height)) != (width, height) {
                // Rebuilding children during allocation isn't allowed.
                let obj = self.obj().downgrade();
                glib::idle_add_local_once(move || {
                    let Some(obj) = obj.upgrade() else { return };
                    let imp = obj.imp();
                    let (w, h) = imp.last.get();
                    let callback = imp.callback.borrow();
                    if let Some(callback) = callback.as_ref() {
                        callback(w, h);
                    }
                });
            }
        }
    }
}

glib::wrapper! {
    pub struct SizeBin(ObjectSubclass<imp::SizeBin>) @extends gtk::Widget, @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl SizeBin {
    pub fn new(child: &impl IsA<gtk::Widget>, on_resize: impl Fn(i32, i32) + 'static) -> Self {
        let obj: Self = glib::Object::new();
        child.set_parent(&obj);
        obj.imp().child.replace(Some(child.clone().upcast()));
        obj.imp().callback.replace(Some(Box::new(on_resize)));
        obj
    }

    pub fn size(&self) -> (i32, i32) {
        self.imp().last.get()
    }
}
