//! Client side of the compositor's private `org.gnome.ShellPlus.Screenshot`
//! interface (see the comment at the top of
//! compositor/src/shellplus-screenshot.c): screen and window captures
//! passed through a memfd, the flash, and requests to show the UI.

use gtk::prelude::*;
use gtk::{cairo, gdk, gio, glib};
use std::io::Read;
use std::rc::Rc;

const BUS_NAME: &str = "org.gnome.ShellPlus";
const PATH: &str = "/org/gnome/ShellPlus";
const IFACE: &str = "org.gnome.ShellPlus.Screenshot";

/// A captured image: cairo ARGB32, i.e. premultiplied BGRA in memory.
#[derive(Clone)]
pub struct Image {
    pub data: Rc<Vec<u8>>,
    pub width: i32,
    pub height: i32,
    pub stride: i32,
}

impl Image {
    pub fn texture(&self) -> gdk::Texture {
        gdk::MemoryTexture::new(
            self.width,
            self.height,
            gdk::MemoryFormat::B8g8r8a8Premultiplied,
            &glib::Bytes::from(&self.data[..]),
            self.stride as usize,
        )
        .upcast()
    }

    /// A cairo surface sharing nothing with `self`.
    pub fn surface(&self) -> Option<cairo::ImageSurface> {
        cairo::ImageSurface::create_for_data(
            (*self.data).clone(),
            cairo::Format::ARgb32,
            self.width,
            self.height,
            self.stride,
        )
        .ok()
    }

    /// The part inside (x, y, w, h), in pixels, clamped to the image.
    pub fn crop(&self, x: i32, y: i32, w: i32, h: i32) -> Image {
        let x0 = x.clamp(0, self.width - 1);
        let y0 = y.clamp(0, self.height - 1);
        let x1 = (x + w).clamp(x0 + 1, self.width);
        let y1 = (y + h).clamp(y0 + 1, self.height);
        let (width, height) = (x1 - x0, y1 - y0);
        let stride = width * 4;
        let mut data = Vec::with_capacity((stride * height) as usize);
        for row in y0..y1 {
            let start = (row * self.stride + x0 * 4) as usize;
            data.extend_from_slice(&self.data[start..start + stride as usize]);
        }
        Image { data: Rc::new(data), width, height, stride }
    }

    /// The colour at a pixel, un-premultiplied, as 0..1 RGB.
    pub fn pixel(&self, x: i32, y: i32) -> (f64, f64, f64) {
        let x = x.clamp(0, self.width - 1);
        let y = y.clamp(0, self.height - 1);
        let i = (y * self.stride + x * 4) as usize;
        let (b, g, r, a) = (self.data[i], self.data[i + 1], self.data[i + 2], self.data[i + 3]);
        let un = |c: u8| {
            if a == 0 {
                0.0
            } else {
                (c as f64 * 255.0 / a as f64).min(255.0) / 255.0
            }
        };
        (un(r), un(g), un(b))
    }
}

pub enum Event {
    /// Show the UI. `request` is 0, or a D-Bus request to [`Client::finish`].
    ShowUi { request: u32, mode: String },
    /// A Shift+Print / Alt+Print shot was saved here.
    Taken(String),
}

pub struct Client {
    connection: Option<gio::DBusConnection>,
}

impl Client {
    pub fn new(on_event: impl Fn(Event) + 'static) -> Rc<Self> {
        let connection = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE).ok();
        if let Some(connection) = &connection {
            #[allow(deprecated)]
            connection.signal_subscribe(
                Some(BUS_NAME),
                Some(IFACE),
                None,
                Some(PATH),
                None,
                gio::DBusSignalFlags::NONE,
                move |_, _, _, _, signal, params| match signal {
                    "ShowUI" => {
                        if let Some((request, mode)) = params.get::<(u32, String)>() {
                            on_event(Event::ShowUi { request, mode });
                        }
                    }
                    "ScreenshotTaken" => {
                        if let Some((path,)) = params.get::<(String,)>() {
                            on_event(Event::Taken(path));
                        }
                    }
                    _ => {}
                },
            );
        }
        Rc::new(Self { connection })
    }

    /// The whole stage and the scale it was captured at.
    pub fn capture_screen(&self, cursor: bool, callback: impl FnOnce(Option<(Image, f64)>) + 'static) {
        self.capture("CaptureScreen", (cursor,).to_variant(), "(hiiid)", move |reply, fds| {
            let (index, w, h, stride, scale) = reply.get::<(glib::variant::Handle, i32, i32, i32, f64)>()?;
            Some((read_image(fds, index.0, w, h, stride)?, scale))
        }, callback);
    }

    /// A window at full size (id 0: the focused one), frame included.
    pub fn capture_window(&self, id: u64, callback: impl FnOnce(Option<Image>) + 'static) {
        self.capture("CaptureWindow", (id, true).to_variant(), "(hiii)", move |reply, fds| {
            let (index, w, h, stride) = reply.get::<(glib::variant::Handle, i32, i32, i32)>()?;
            read_image(fds, index.0, w, h, stride)
        }, callback);
    }

    fn capture<T: 'static>(
        &self,
        method: &'static str,
        args: glib::Variant,
        reply_type: &str,
        parse: impl FnOnce(&glib::Variant, Option<&gio::UnixFDList>) -> Option<T> + 'static,
        callback: impl FnOnce(Option<T>) + 'static,
    ) {
        let Some(connection) = &self.connection else {
            callback(None);
            return;
        };
        connection.call_with_unix_fd_list(
            Some(BUS_NAME),
            PATH,
            IFACE,
            method,
            Some(&args),
            Some(glib::VariantTy::new(reply_type).unwrap()),
            gio::DBusCallFlags::NONE,
            10000,
            None::<&gio::UnixFDList>,
            gio::Cancellable::NONE,
            move |result| match result {
                Ok((reply, fds)) => callback(parse(&reply, fds.as_ref())),
                Err(err) => {
                    glib::g_warning!("shellplus", "{method} failed: {err}");
                    callback(None);
                }
            },
        );
    }

    pub fn flash(&self, rect: (i32, i32, i32, i32)) {
        self.call("Flash", rect.to_variant());
    }

    pub fn finish(&self, request: u32, success: bool, result: glib::VariantDict) {
        if request != 0 {
            let args = glib::Variant::tuple_from_iter([request.to_variant(), success.to_variant(), result.end()]);
            self.call("Finish", args);
        }
    }

    fn call(&self, method: &'static str, args: glib::Variant) {
        let Some(connection) = &self.connection else {
            return;
        };
        connection.call(
            Some(BUS_NAME),
            PATH,
            IFACE,
            method,
            Some(&args),
            None,
            gio::DBusCallFlags::NONE,
            -1,
            gio::Cancellable::NONE,
            move |result| {
                if let Err(err) = result {
                    glib::g_warning!("shellplus", "{method} failed: {err}");
                }
            },
        );
    }
}

fn read_image(fds: Option<&gio::UnixFDList>, index: i32, width: i32, height: i32, stride: i32) -> Option<Image> {
    if width <= 0 || height <= 0 || stride < width * 4 {
        return None;
    }
    let fd = fds?.get(index).ok()?;
    let mut file = std::fs::File::from(fd);
    let mut data = vec![0u8; stride as usize * height as usize];
    file.read_exact(&mut data).ok()?;
    Some(Image { data: Rc::new(data), width, height, stride })
}
