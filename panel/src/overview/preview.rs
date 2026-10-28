//! One window preview in the window picker (windowPreview.js): the live
//! picture of the window plus its chrome, the app icon overlapping the
//! bottom edge, and the title and close button that appear on hover.

use super::client::WindowInfo;
use super::layout::Rect;
use adw::prelude::*;
use gtk::gio;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;

/// windowPreview.js ICON_SIZE, ICON_OVERLAP, ICON_TITLE_SPACING.
pub const ICON_SIZE: f64 = 64.0;
pub const ICON_OVERLAP: f64 = 0.7;
pub const ICON_TITLE_SPACING: f64 = 6.0;
pub const CLOSE_SIZE: f64 = 30.0;
/// workspace.js WINDOW_ACTIVE_SIZE_INC: hovered previews grow this much
/// on each side.
pub const ACTIVE_SIZE_INC: f64 = 5.0;
const HOVER_MS: u32 = 200;
const FALLBACK_ICON: &str = "application-x-executable";

pub struct Preview {
    pub id: u64,
    /// The workspace (card) this preview is in.
    pub workspace: i32,
    pub info: RefCell<WindowInfo>,
    pub picture: gtk::Picture,
    pub close: gtk::Button,
    pub icon: gtk::Image,
    pub title: gtk::Label,
    /// Slot in the window picker, relative to the picker card at offset 0.
    pub slot: Cell<Rect>,
    /// Where the slot was when a relayout started.
    pub from_slot: Cell<Rect>,
    /// Last allocated window rect (stage coordinates).
    pub rect: Cell<Rect>,
    hover_target: Cell<bool>,
    hover_anim: RefCell<Option<adw::TimedAnimation>>,
    pub dragging: Cell<bool>,
}

impl Preview {
    pub fn new(info: &WindowInfo, workspace: i32) -> Self {
        let name_suffix = if info.workspace == -1 {
            format!("{}-{workspace}", info.id)
        } else {
            info.id.to_string()
        };
        let picture = gtk::Picture::builder()
            .name(format!("overview-window-{name_suffix}"))
            .can_shrink(true)
            .content_fit(gtk::ContentFit::Fill)
            .css_classes(["overview-window"])
            .build();
        let close = gtk::Button::builder()
            .name(format!("overview-close-{name_suffix}"))
            .icon_name("window-close-symbolic")
            .css_classes(["overview-close"])
            .tooltip_text("Close")
            .focusable(false)
            .build();
        let icon = app_icon(&info.app_id);
        icon.set_pixel_size(ICON_SIZE as i32);
        icon.set_can_target(false);
        icon.add_css_class("overview-window-icon");
        let title = gtk::Label::builder()
            .name(format!("overview-title-{name_suffix}"))
            .label(&info.title)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["overview-caption"])
            .can_target(false)
            .build();

        Self {
            id: info.id,
            workspace,
            info: RefCell::new(info.clone()),
            picture,
            close,
            icon,
            title,
            slot: Cell::default(),
            from_slot: Cell::default(),
            rect: Cell::default(),
            hover_target: Cell::new(false),
            hover_anim: RefCell::default(),
            dragging: Cell::new(false),
        }
    }

    pub fn set_info(&self, info: &WindowInfo) {
        if self.info.borrow().title != info.title {
            self.title.set_label(&info.title);
        }
        self.info.replace(info.clone());
    }

    pub fn widgets(&self) -> [gtk::Widget; 4] {
        [
            self.picture.clone().upcast(),
            self.close.clone().upcast(),
            self.icon.clone().upcast(),
            self.title.clone().upcast(),
        ]
    }

    /// Animates the hover highlight; `on_change` runs every frame.
    pub fn set_hovered(&self, hovered: bool, slowdown: f64, on_change: impl Fn() + 'static) {
        if self.hover_target.replace(hovered) == hovered {
            return;
        }
        // Clickable right away, so a click that arrives with the pointer
        // motion still lands on it.
        self.close.set_can_target(hovered);
        let target = adw::CallbackAnimationTarget::new(move |_| on_change());
        let anim = adw::TimedAnimation::builder()
            .widget(&self.picture)
            .value_from(self.hover_value())
            .value_to(if hovered { 1.0 } else { 0.0 })
            .duration((HOVER_MS as f64 * slowdown) as u32)
            .easing(adw::Easing::EaseOutQuad)
            .target(&target)
            .build();
        self.hover_anim.replace(Some(anim.clone()));
        anim.play();
    }

    /// The hover / keyboard selection highlight, 0..1 (animated).
    pub fn hover_value(&self) -> f64 {
        match self.hover_anim.borrow().as_ref() {
            Some(anim) if anim.state() == adw::AnimationState::Playing => anim.value(),
            _ => {
                if self.hover_target.get() {
                    1.0
                } else {
                    0.0
                }
            }
        }
    }

    pub fn unparent(&self) {
        for widget in self.widgets() {
            widget.unparent();
        }
    }
}

thread_local! {
    static APP_INFOS: RefCell<HashMap<String, Option<gio::AppInfo>>> = RefCell::default();
}

/// The installed app whose desktop file id is `<app_id>.desktop` (matched
/// case-insensitively; see switcher.rs).
pub fn app_info(app_id: &str) -> Option<gio::AppInfo> {
    if app_id.is_empty() {
        return None;
    }
    APP_INFOS.with(|cache| {
        cache
            .borrow_mut()
            .entry(app_id.to_owned())
            .or_insert_with(|| {
                let desktop_id = format!("{app_id}.desktop").to_lowercase();
                gio::AppInfo::all()
                    .into_iter()
                    .find(|info| info.id().is_some_and(|id| id.to_lowercase() == desktop_id))
            })
            .clone()
    })
}

fn app_icon(app_id: &str) -> gtk::Image {
    let image = gtk::Image::new();
    if let Some(icon) = app_info(app_id).and_then(|info| info.icon()) {
        image.set_from_gicon(&icon);
        return image;
    }
    let has_icon = !app_id.is_empty()
        && gtk::gdk::Display::default()
            .map(|display| gtk::IconTheme::for_display(&display).has_icon(app_id))
            .unwrap_or(false);
    image.set_icon_name(Some(if has_icon { app_id } else { FALLBACK_ICON }));
    image
}
