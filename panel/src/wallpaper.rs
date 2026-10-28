//! Renders a blurred copy of the strip of wallpaper that sits behind the
//! panel, using the same org.gnome.desktop.background settings the
//! compositor draws the desktop with.
//!
//! The panel only turns translucent when no window touches it, so the only
//! thing behind it is the wallpaper. Blurring that strip on the CPU once per
//! wallpaper change looks the same as a live compositor-side blur and costs
//! nothing per frame.

use crate::system;
use gtk::prelude::*;
use gtk::{cairo, gdk, gdk_pixbuf, gio, glib};

/// Render at a quarter of the physical resolution; the result is blurred
/// anyway and GTK scales it back up with linear filtering.
const DOWNSCALE: f64 = 4.0;
/// Box blur radius in downscaled pixels. Three passes approximate a
/// gaussian with an effective radius of about 3 * 6 * 4 = 72 physical px.
const RADIUS: usize = 6;
const PASSES: usize = 3;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Request {
    pub panel_height: i32,
    pub monitor_width: i32,
    pub monitor_height: i32,
    pub scale: i32,
}

struct Background {
    file: Option<std::path::PathBuf>,
    style: String,
    color: (f64, f64, f64),
}

/// Renders asynchronously; `callback` receives `None` if rendering failed,
/// in which case the panel falls back to its plain translucent tint.
pub fn render(request: Request, callback: impl FnOnce(Option<gdk::Texture>) + 'static) {
    let background = read_settings();

    glib::spawn_future_local(async move {
        let result = gio::spawn_blocking(move || render_blocking(&request, &background)).await;
        let texture = result.ok().flatten().map(|(width, height, stride, data)| {
            gdk::MemoryTexture::new(
                width,
                height,
                // Cairo's ARGB32 in little-endian byte order.
                gdk::MemoryFormat::B8g8r8a8Premultiplied,
                &glib::Bytes::from_owned(data),
                stride,
            )
            .upcast::<gdk::Texture>()
        });
        callback(texture);
    });
}

/// Renders the whole desktop background of a `width` x `height` monitor at
/// `scale`, unblurred (the overview's workspace cards).
pub fn render_desktop(width: i32, height: i32, scale: i32, callback: impl FnOnce(Option<gdk::Texture>) + 'static) {
    let background = read_settings();

    glib::spawn_future_local(async move {
        let result = gio::spawn_blocking(move || {
            let (w, h) = ((width * scale).max(1), (height * scale).max(1));
            let mut surface = cairo::ImageSurface::create(cairo::Format::ARgb32, w, h).ok()?;
            {
                let cr = cairo::Context::new(&surface).ok()?;
                let (r, g, b) = background.color;
                cr.set_source_rgb(r, g, b);
                cr.paint().ok()?;
                if let Some(file) = background.file.as_deref() {
                    if background.style != "none" {
                        paint_image(&cr, file, &background.style, w as f64, h as f64, scale as f64);
                    }
                }
            }
            surface.flush();
            let stride = surface.stride() as usize;
            let data = surface.data().ok()?.to_vec();
            Some((w, h, stride, data))
        })
        .await;
        let texture = result.ok().flatten().map(|(w, h, stride, data)| {
            gdk::MemoryTexture::new(w, h, gdk::MemoryFormat::B8g8r8a8Premultiplied, &glib::Bytes::from_owned(data), stride)
                .upcast::<gdk::Texture>()
        });
        callback(texture);
    });
}

fn read_settings() -> Background {
    let mut background = Background {
        file: None,
        style: "zoom".into(),
        color: (0.14, 0.14, 0.14),
    };
    let Some(settings) = system::settings("org.gnome.desktop.background") else {
        return background;
    };

    background.style = settings.string("picture-options").to_string();
    if let Ok(rgba) = gdk::RGBA::parse(settings.string("primary-color").as_str()) {
        background.color = (rgba.red() as f64, rgba.green() as f64, rgba.blue() as f64);
    }

    let dark = system::settings("org.gnome.desktop.interface")
        .map(|s| s.string("color-scheme") == "prefer-dark")
        .unwrap_or(false);
    let mut uri = if dark {
        settings.string("picture-uri-dark")
    } else {
        Default::default()
    };
    if uri.is_empty() {
        uri = settings.string("picture-uri");
    }
    if !uri.is_empty() {
        let path = gio::File::for_commandline_arg(uri.as_str()).path();
        background.file = match path {
            Some(path) if path.extension().is_some_and(|ext| ext == "xml") => {
                resolve_xml_wallpaper(&path)
            }
            path => path,
        };
    }
    background
}

/// Mirrors the compositor: use the first image an XML slideshow references.
fn resolve_xml_wallpaper(path: &std::path::Path) -> Option<std::path::PathBuf> {
    let contents = std::fs::read_to_string(path).ok()?;
    contents
        .split('>')
        .skip(1)
        .filter_map(|chunk| chunk.split('<').next())
        .map(str::trim)
        .find(|text| text.starts_with('/') && !text.ends_with(".xml"))
        .map(Into::into)
}

fn render_blocking(request: &Request, background: &Background) -> Option<(i32, i32, usize, Vec<u8>)> {
    let monitor_w = (request.monitor_width * request.scale) as f64 / DOWNSCALE;
    let monitor_h = (request.monitor_height * request.scale) as f64 / DOWNSCALE;
    let width = monitor_w.ceil().max(1.0) as i32;
    let strip_h = ((request.panel_height * request.scale) as f64 / DOWNSCALE).ceil().max(1.0) as i32;
    // Render extra rows below the strip so the blur pulls in what's
    // underneath it, like a real backdrop blur would.
    let height = (strip_h + (RADIUS * PASSES) as i32).min(monitor_h.ceil() as i32).max(strip_h);

    let mut surface = cairo::ImageSurface::create(cairo::Format::ARgb32, width, height).ok()?;
    {
        let cr = cairo::Context::new(&surface).ok()?;
        let (r, g, b) = background.color;
        cr.set_source_rgb(r, g, b);
        cr.paint().ok()?;

        if let Some(file) = background.file.as_deref() {
            if background.style != "none" {
                paint_image(&cr, file, &background.style, monitor_w, monitor_h, request.scale as f64 / DOWNSCALE);
            }
        }
    }
    surface.flush();

    let stride = surface.stride() as usize;
    let mut data = surface.data().ok()?.to_vec();
    for _ in 0..PASSES {
        box_blur(&mut data, width as usize, height as usize, stride, RADIUS);
    }
    data.truncate(stride * strip_h as usize);
    Some((width, strip_h, stride, data))
}

/// Draws the wallpaper with the same placement rules as MetaBackground.
fn paint_image(cr: &cairo::Context, file: &std::path::Path, style: &str, monitor_w: f64, monitor_h: f64, unit: f64) {
    let Some((_, image_w, image_h)) = gdk_pixbuf::Pixbuf::file_info(file) else {
        glib::g_warning!("shellplus", "Can't read wallpaper {}", file.display());
        return;
    };
    let (image_w, image_h) = (image_w as f64, image_h as f64);

    let (target_w, target_h) = match style {
        "zoom" | "spanned" => {
            let s = (monitor_w / image_w).max(monitor_h / image_h);
            (image_w * s, image_h * s)
        }
        "scaled" => {
            let s = (monitor_w / image_w).min(monitor_h / image_h);
            (image_w * s, image_h * s)
        }
        "stretched" => (monitor_w, monitor_h),
        // "centered" and "wallpaper" (tiled) draw at the image's own size.
        _ => (image_w * unit, image_h * unit),
    };

    // Let the loader decode straight to the target size; much faster than
    // decoding a 4K+ image and scaling it down afterwards.
    let Ok(pixbuf) = gdk_pixbuf::Pixbuf::from_file_at_scale(
        file,
        target_w.round().max(1.0) as i32,
        target_h.round().max(1.0) as i32,
        false,
    ) else {
        return;
    };

    let (x, y) = if style == "wallpaper" {
        (0.0, 0.0)
    } else {
        (((monitor_w - target_w) / 2.0).round(), ((monitor_h - target_h) / 2.0).round())
    };
    cr.set_source_pixbuf(&pixbuf, x, y);
    if style == "wallpaper" {
        cr.source().set_extend(cairo::Extend::Repeat);
    }
    let _ = cr.paint();
}

fn box_blur(data: &mut [u8], width: usize, height: usize, stride: usize, radius: usize) {
    let mut tmp = vec![0u8; data.len()];
    for y in 0..height {
        for channel in 0..4 {
            blur_line(data, &mut tmp, y * stride + channel, 4, width, radius);
        }
    }
    for x in 0..width {
        for channel in 0..4 {
            blur_line(&tmp, data, x * 4 + channel, stride, height, radius);
        }
    }
}

/// Sliding-window average along one row or column, clamping at the edges.
fn blur_line(src: &[u8], dst: &mut [u8], start: usize, step: usize, len: usize, radius: usize) {
    let last = len as isize - 1;
    let get = |i: isize| src[start + i.clamp(0, last) as usize * step] as u32;
    let window = (2 * radius + 1) as u32;
    let r = radius as isize;
    let mut sum: u32 = (-r..=r).map(get).sum();

    for i in 0..len as isize {
        dst[start + i as usize * step] = ((sum + window / 2) / window) as u8;
        sum += get(i + r + 1);
        sum -= get(i - r);
    }
}
