//! Output and input volume sliders, talking to PulseAudio (or
//! pipewire-pulse) through libpulse on GLib's main loop, like GNOME Shell's
//! volume.js does through libgnome-volume-control.
//!
//! Everything is event driven: the context subscribes to sink, source,
//! server and source-output events and re-reads the affected lists. The
//! input slider and the microphone privacy indicator only show while an
//! application is recording.

use super::widgets::{MenuItem, QuickSlider};
use super::Ctx;
use crate::system;
use gtk::prelude::*;
use gtk::{gio, glib};
use libpulse_binding::callbacks::ListResult;
use libpulse_binding::context::subscribe::{Facility, InterestMaskSet};
use libpulse_binding::context::{Context, FlagSet, State};
use libpulse_binding::proplist::Proplist;
use libpulse_binding::volume::{ChannelVolumes, Volume};
use libpulse_glib_binding::Mainloop;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

const RECONNECT_DELAY: Duration = Duration::from_secs(2);

// Bits of `Sound::dirty`: what to re-read on the next idle.
const SINKS: u8 = 1;
const SOURCES: u8 = 2;
const SERVER: u8 = 4;
const OUTPUTS: u8 = 8;
const ALL: u8 = SINKS | SOURCES | SERVER | OUTPUTS;

/// Volume-meter and mixer applications whose source outputs don't count
/// as "recording" (GNOME ignores them too).
const MIXER_APPS: &[&str] = &["org.PulseAudio.pavucontrol", "org.gnome.VolumeControl", "org.gnome.Settings"];

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Sink = 0,
    Source = 1,
}

struct Device {
    name: String,
    description: String,
    icon: &'static str,
    volume: ChannelVolumes,
    mute: bool,
}

impl Device {
    fn new(kind: Kind, name: &Option<std::borrow::Cow<str>>, description: &Option<std::borrow::Cow<str>>,
           volume: ChannelVolumes, mute: bool, props: &Proplist) -> Self {
        let name = name.as_deref().unwrap_or_default().to_owned();
        let description = description.as_deref().map(str::to_owned).unwrap_or_else(|| name.clone());
        Self { name, description, icon: device_icon(kind, props), volume, mute }
    }

    /// Loudest channel, 1.0 being 100%.
    fn level(&self) -> f64 {
        self.volume.max().0 as f64 / Volume::NORMAL.0 as f64
    }
}

#[derive(Default)]
struct Mixer {
    devices: [Vec<Device>; 2],
    defaults: [String; 2],
    recording: bool,
}

impl Mixer {
    fn default_device(&self, kind: Kind) -> Option<&Device> {
        let default = &self.defaults[kind as usize];
        self.devices[kind as usize].iter().find(|d| &d.name == default)
    }
}

/// One slider row (output or input) and the device list its menu shows.
struct Side {
    kind: Kind,
    slider: Rc<QuickSlider>,
    menu_items: RefCell<Vec<(String, String, bool)>>,
}

struct Sound {
    ctx: Rc<Ctx>,
    mainloop: Mainloop,
    context: RefCell<Option<Context>>,
    connected: Cell<bool>,
    dirty: Cell<u8>,
    mixer: RefCell<Mixer>,
    output: Side,
    input: Side,
    settings: Option<gio::Settings>,
}

pub fn build(ctx: &Rc<Ctx>, sliders: &gtk::Box) {
    let Some(mainloop) = Mainloop::new(None) else {
        glib::g_warning!("shellplus", "Couldn't create the PulseAudio main loop");
        return;
    };
    let side = |kind, name, icon, title, header_icon| {
        let slider = QuickSlider::new(name, icon, Some((title, &ctx.host)));
        let menu = slider.menu.as_ref().expect("slider has a menu");
        menu.set_icon(header_icon);
        menu.add_footer(&format!("{name}-settings"), "Sound Settings", || system::open_settings("sound"));
        slider.root.set_visible(false);
        sliders.append(&slider.root);
        Side { kind, slider, menu_items: RefCell::default() }
    };
    let output = side(Kind::Sink, "qs-volume", "audio-volume-medium-symbolic", "Sound Output", "audio-headphones-symbolic");
    let input = side(Kind::Source, "qs-mic", "microphone-sensitivity-medium-symbolic", "Sound Input",
                     "audio-input-microphone-symbolic");
    ctx.indicators.microphone.set_icon_name(Some("audio-input-microphone-symbolic"));

    let sound = Rc::new(Sound {
        ctx: ctx.clone(),
        mainloop,
        context: RefCell::default(),
        connected: Cell::new(false),
        dirty: Cell::new(0),
        mixer: RefCell::default(),
        output,
        input,
        settings: system::settings("org.gnome.desktop.sound"),
    });

    for side in [&sound.output, &sound.input] {
        let kind = side.kind;
        side.slider.connect_changed(glib::clone!(
            #[strong]
            sound,
            move |value| sound.set_volume(kind, value)
        ));
        side.slider.icon_button.connect_clicked(glib::clone!(
            #[strong]
            sound,
            move |_| sound.toggle_mute(kind)
        ));
    }
    if let Some(settings) = &sound.settings {
        settings.connect_changed(
            Some("allow-volume-above-100-percent"),
            glib::clone!(
                #[weak]
                sound,
                move |_, _| sound.update()
            ),
        );
    }
    sound.connect();
}

impl Sound {
    fn side(&self, kind: Kind) -> &Side {
        match kind {
            Kind::Sink => &self.output,
            Kind::Source => &self.input,
        }
    }

    // --- connection ------------------------------------------------------

    fn connect(self: &Rc<Self>) {
        let Some(mut context) = Context::new(&self.mainloop, "GNOME Shell Plus") else {
            return self.reconnect_later();
        };
        // The state callback can run synchronously inside connect(), while
        // the context is still borrowed, so handle the change from an idle.
        let weak = Rc::downgrade(self);
        context.set_state_callback(Some(Box::new(move || {
            let weak = weak.clone();
            glib::idle_add_local_once(move || {
                if let Some(sound) = weak.upgrade() {
                    sound.state_changed();
                }
            });
        })));
        let failed = context.connect(None, FlagSet::NOFAIL, None).is_err();
        self.context.replace(Some(context));
        if failed {
            self.disconnected();
        }
    }

    fn state_changed(self: &Rc<Self>) {
        let state = self.context.borrow().as_ref().map(Context::get_state);
        match state {
            Some(State::Ready) if !self.connected.get() => {
                self.connected.set(true);
                self.subscribe();
                self.mark_dirty(ALL);
            }
            Some(State::Failed | State::Terminated) => self.disconnected(),
            _ => {}
        }
    }

    fn disconnected(self: &Rc<Self>) {
        let old = self.context.take();
        drop(old);
        self.connected.set(false);
        self.mixer.replace(Mixer::default());
        self.update();
        self.reconnect_later();
    }

    fn reconnect_later(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        glib::timeout_add_local_once(RECONNECT_DELAY, move || {
            if let Some(sound) = weak.upgrade() {
                sound.connect();
            }
        });
    }

    fn subscribe(self: &Rc<Self>) {
        let mut context = self.context.borrow_mut();
        let Some(context) = context.as_mut() else { return };
        let weak = Rc::downgrade(self);
        context.set_subscribe_callback(Some(Box::new(move |facility, _, _| {
            let bits = match facility {
                Some(Facility::Sink) => SINKS,
                Some(Facility::Source) => SOURCES,
                Some(Facility::Server) => SERVER,
                Some(Facility::SourceOutput) => OUTPUTS,
                _ => 0,
            };
            if let Some(sound) = weak.upgrade() {
                sound.mark_dirty(bits);
            }
        })));
        let mask = InterestMaskSet::SINK | InterestMaskSet::SOURCE | InterestMaskSet::SERVER
            | InterestMaskSet::SOURCE_OUTPUT;
        context.subscribe(mask, |_| {});
    }

    // --- reading state ---------------------------------------------------

    /// Coalesces bursts of events into one re-read per list.
    fn mark_dirty(self: &Rc<Self>, bits: u8) {
        if bits == 0 || self.dirty.replace(self.dirty.get() | bits) != 0 {
            return;
        }
        let weak = Rc::downgrade(self);
        glib::idle_add_local_once(move || {
            if let Some(sound) = weak.upgrade() {
                sound.refresh(sound.dirty.replace(0));
            }
        });
    }

    fn refresh(self: &Rc<Self>, bits: u8) {
        let context = self.context.borrow();
        let Some(context) = context.as_ref().filter(|_| self.connected.get()) else { return };
        let introspect = context.introspect();

        if bits & SERVER != 0 {
            let weak = Rc::downgrade(self);
            introspect.get_server_info(move |info| {
                let Some(sound) = weak.upgrade() else { return };
                let name = |n: &Option<std::borrow::Cow<str>>| n.as_deref().unwrap_or_default().to_owned();
                sound.mixer.borrow_mut().defaults = [name(&info.default_sink_name), name(&info.default_source_name)];
                sound.update();
            });
        }
        if bits & SINKS != 0 {
            let (weak, mut list) = (Rc::downgrade(self), Vec::new());
            introspect.get_sink_info_list(move |result| match result {
                ListResult::Item(i) => {
                    list.push(Device::new(Kind::Sink, &i.name, &i.description, i.volume, i.mute, &i.proplist));
                }
                ListResult::End => {
                    if let Some(sound) = weak.upgrade() {
                        sound.mixer.borrow_mut().devices[Kind::Sink as usize] = std::mem::take(&mut list);
                        sound.update();
                    }
                }
                ListResult::Error => {}
            });
        }
        if bits & SOURCES != 0 {
            let (weak, mut list) = (Rc::downgrade(self), Vec::new());
            introspect.get_source_info_list(move |result| match result {
                ListResult::Item(i) if i.monitor_of_sink.is_none() => {
                    list.push(Device::new(Kind::Source, &i.name, &i.description, i.volume, i.mute, &i.proplist));
                }
                ListResult::Item(_) | ListResult::Error => {}
                ListResult::End => {
                    if let Some(sound) = weak.upgrade() {
                        sound.mixer.borrow_mut().devices[Kind::Source as usize] = std::mem::take(&mut list);
                        sound.update();
                    }
                }
            });
        }
        if bits & OUTPUTS != 0 {
            let (weak, mut recording) = (Rc::downgrade(self), false);
            introspect.get_source_output_info_list(move |result| match result {
                ListResult::Item(o) => recording |= is_recording(o.resample_method.as_deref(), &o.proplist),
                ListResult::End => {
                    if let Some(sound) = weak.upgrade() {
                        sound.mixer.borrow_mut().recording = std::mem::replace(&mut recording, false);
                        sound.update();
                    }
                }
                ListResult::Error => {}
            });
        }
    }

    // --- UI --------------------------------------------------------------

    fn max_level(&self, kind: Kind) -> f64 {
        let loud = kind == Kind::Sink
            && self.settings.as_ref().is_some_and(|s| s.boolean("allow-volume-above-100-percent"));
        if loud { 1.5 } else { 1.0 }
    }

    fn update(self: &Rc<Self>) {
        let mixer = self.mixer.borrow();
        let connected = self.connected.get();
        for side in [&self.output, &self.input] {
            let device = mixer.default_device(side.kind).filter(|_| connected);
            let shown = device.is_some() && (side.kind == Kind::Sink || mixer.recording);
            side.slider.root.set_visible(shown);
            if let Some(device) = device {
                side.slider.set_range(self.max_level(side.kind));
                side.slider.set_value(if device.mute { 0.0 } else { device.level() });
                side.slider.set_icon(&volume_icon(side.kind, device.level(), device.mute));
            }
            self.sync_menu(side, &mixer);
        }

        let output = mixer.default_device(Kind::Sink).filter(|_| connected);
        let indicators = &self.ctx.indicators;
        if let Some(device) = output {
            indicators.volume.set_icon_name(Some(&volume_icon(Kind::Sink, device.level(), device.mute)));
        }
        indicators.volume.set_visible(output.is_some());
        indicators.microphone.set_visible(connected && mixer.recording);
    }

    /// Rebuilds a device menu, but only when its entries actually changed.
    fn sync_menu(self: &Rc<Self>, side: &Side, mixer: &Mixer) {
        let default = &mixer.defaults[side.kind as usize];
        let devices = &mixer.devices[side.kind as usize];
        let items: Vec<_> = devices
            .iter()
            .map(|d| (d.name.clone(), d.description.clone(), &d.name == default))
            .collect();
        if *side.menu_items.borrow() == items {
            return;
        }
        let Some(menu) = &side.slider.menu else { return };
        menu.clear();
        let prefix = if side.kind == Kind::Sink { "qs-sink" } else { "qs-source" };
        for (device, (name, label, checked)) in devices.iter().zip(&items) {
            let item = MenuItem::build(&format!("{prefix}-{name}"), Some(device.icon), label, None, *checked);
            let (kind, name) = (side.kind, name.clone());
            item.connect_clicked(glib::clone!(
                #[weak(rename_to = sound)]
                self,
                move |_| sound.set_default(kind, &name)
            ));
            menu.add(&item);
        }
        side.menu_items.replace(items);
    }

    // --- user actions ----------------------------------------------------

    fn set_volume(&self, kind: Kind, value: f64) {
        let (name, mut volume, mute) = {
            let mixer = self.mixer.borrow();
            let Some(device) = mixer.default_device(kind) else { return };
            (device.name.clone(), device.volume, device.mute)
        };
        // Keep the balance between channels: scale them so the loudest
        // one ends up at the new level.
        let level = (value.clamp(0.0, self.max_level(kind)) * Volume::NORMAL.0 as f64).round() as u32;
        volume.scale(Volume(level));
        // Dragging to zero mutes and dragging back up unmutes, like GNOME.
        let muted = level == 0;

        let mut context = self.context.borrow_mut();
        let Some(context) = context.as_mut() else { return };
        let mut introspect = context.introspect();
        match kind {
            Kind::Sink => {
                introspect.set_sink_volume_by_name(&name, &volume, None);
                if mute != muted {
                    introspect.set_sink_mute_by_name(&name, muted, None);
                }
            }
            Kind::Source => {
                introspect.set_source_volume_by_name(&name, &volume, None);
                if mute != muted {
                    introspect.set_source_mute_by_name(&name, muted, None);
                }
            }
        }
        // Remember it right away, so a drag continues from the new level.
        if let Some(device) = self.mixer.borrow_mut().devices[kind as usize].iter_mut().find(|d| d.name == name) {
            device.volume = volume;
            device.mute = muted;
        }
        let side = self.side(kind);
        side.slider.set_icon(&volume_icon(kind, value, muted));
    }

    fn toggle_mute(&self, kind: Kind) {
        let Some((name, mute)) = self.mixer.borrow().default_device(kind).map(|d| (d.name.clone(), d.mute)) else {
            return;
        };
        let mut context = self.context.borrow_mut();
        let Some(context) = context.as_mut() else { return };
        let mut introspect = context.introspect();
        match kind {
            Kind::Sink => introspect.set_sink_mute_by_name(&name, !mute, None),
            Kind::Source => introspect.set_source_mute_by_name(&name, !mute, None),
        };
    }

    fn set_default(&self, kind: Kind, name: &str) {
        let mut context = self.context.borrow_mut();
        let Some(context) = context.as_mut() else { return };
        match kind {
            Kind::Sink => context.set_default_sink(name, |_| {}),
            Kind::Source => context.set_default_source(name, |_| {}),
        };
    }
}

/// Whether a source output is an application recording, rather than a
/// level meter (peak detection) or a mixer application.
fn is_recording(resample_method: Option<&str>, props: &Proplist) -> bool {
    let app = props.get_str("application.id").unwrap_or_default();
    resample_method != Some("peaks") && !MIXER_APPS.contains(&app.as_str())
}

/// GNOME's icon buckets: thirds of 100%, plus "overamplified" above it.
fn volume_icon(kind: Kind, level: f64, mute: bool) -> String {
    let (prefix, names): (&str, &[&str]) = match kind {
        Kind::Sink => ("audio-volume", &["muted", "low", "medium", "high", "overamplified"]),
        Kind::Source => ("microphone-sensitivity", &["muted", "low", "medium", "high"]),
    };
    let index = if mute || level <= 0.0 {
        0
    } else {
        ((3.0 * level - 1e-6).ceil() as usize).clamp(1, names.len() - 1)
    };
    format!("{prefix}-{}-symbolic", names[index])
}

fn device_icon(kind: Kind, props: &Proplist) -> &'static str {
    let form_factor = props.get_str("device.form_factor").unwrap_or_default();
    match (kind, form_factor.as_str()) {
        (_, "headset") => "audio-headset-symbolic",
        (Kind::Sink, "headphone") => "audio-headphones-symbolic",
        (Kind::Sink, _) => "audio-speakers-symbolic",
        (Kind::Source, _) => "audio-input-microphone-symbolic",
    }
}
