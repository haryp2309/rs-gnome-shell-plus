//! Media player controls in the date menu, like GNOME Shell's MPRIS
//! section: one card per org.mpris.MediaPlayer2.* player on the session
//! bus that has something to show, above the notification list.
//!
//! Each card shows the album art, title and artist, the player's app icon
//! and name, and previous / play-pause / next buttons. Clicking the card
//! raises the player. The section hides itself when there are no players.
//!
//! Widget names (for dev/driver.py): `media-card`, `media-previous`,
//! `media-play-pause`, `media-next`. The play-pause icon is named
//! `media-status-playing` or `media-status-paused`, and the art
//! `media-art` or `media-art-fallback`, so tests can see the state.

use crate::system::{self, Props};
use gtk::prelude::*;
use gtk::{gdk, gio, glib, pango};
use std::cell::RefCell;
use std::rc::{Rc, Weak};

const PREFIX: &str = "org.mpris.MediaPlayer2";
const PATH: &str = "/org/mpris/MediaPlayer2";
const PLAYER_IFACE: &str = "org.mpris.MediaPlayer2.Player";
const ART_SIZE: i32 = 48;

/// The date menu's media section.
pub fn section() -> gtk::Widget {
    load_css();
    let root = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .visible(false)
        .css_classes(["media-section"])
        .build();
    if let Some(conn) = system::session_bus() {
        let section = Rc::new(Section { conn, root: root.clone(), players: RefCell::default() });
        section.watch();
        // The menu keeps the widget for the panel's lifetime; so does this.
        root.connect_destroy(move |_| {
            let _ = &section;
        });
    }
    root.upcast()
}

struct Section {
    conn: gio::DBusConnection,
    root: gtk::Box,
    /// By bus name, in order of appearance.
    players: RefCell<Vec<Rc<Player>>>,
}

impl Section {
    fn watch(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        #[allow(deprecated)]
        self.conn.signal_subscribe(
            Some("org.freedesktop.DBus"),
            Some("org.freedesktop.DBus"),
            Some("NameOwnerChanged"),
            Some("/org/freedesktop/DBus"),
            Some(PREFIX),
            gio::DBusSignalFlags::MATCH_ARG0_NAMESPACE,
            move |_, _, _, _, _, params| {
                let (Some(this), Some((name, _, owner))) = (weak.upgrade(), params.get::<(String, String, String)>())
                else {
                    return;
                };
                this.remove(&name);
                if !owner.is_empty() {
                    this.add(&name, &owner);
                }
            },
        );

        let weak = Rc::downgrade(self);
        self.conn.call(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "ListNames",
            None,
            Some(glib::VariantTy::new("(as)").unwrap()),
            gio::DBusCallFlags::NONE,
            -1,
            gio::Cancellable::NONE,
            move |result| {
                let (Some(this), Ok(reply)) = (weak.upgrade(), result) else { return };
                for name in system::paths(&reply.child_value(0)) {
                    if name.starts_with(&format!("{PREFIX}.")) {
                        this.add_unresolved(&name);
                    }
                }
            },
        );
    }

    /// A player found through ListNames: look up its unique name first.
    fn add_unresolved(self: &Rc<Self>, name: &str) {
        let weak = Rc::downgrade(self);
        let name = name.to_owned();
        self.conn.call(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "GetNameOwner",
            Some(&(&name,).to_variant()),
            Some(glib::VariantTy::new("(s)").unwrap()),
            gio::DBusCallFlags::NONE,
            -1,
            gio::Cancellable::NONE,
            move |result| {
                let (Some(this), Ok(reply)) = (weak.upgrade(), result) else { return };
                if let (Some(owner), false) = (reply.child_value(0).str(), this.has(&name)) {
                    this.add(&name, owner);
                }
            },
        );
    }

    fn has(&self, name: &str) -> bool {
        self.players.borrow().iter().any(|p| p.name == name)
    }

    fn add(self: &Rc<Self>, name: &str, owner: &str) {
        let player = Player::new(&self.conn, name, owner, Rc::downgrade(self));
        self.root.append(&player.card.button);
        self.players.borrow_mut().push(player);
        self.update_visibility();
    }

    fn remove(&self, name: &str) {
        let removed: Vec<Rc<Player>> = {
            let mut players = self.players.borrow_mut();
            let (gone, kept) = players.drain(..).partition(|p| p.name == name);
            *players = kept;
            gone
        };
        for player in removed {
            #[allow(deprecated)]
            self.conn.signal_unsubscribe(player.subscription.borrow_mut().take().unwrap());
            self.root.remove(&player.card.button);
        }
        self.update_visibility();
    }

    fn update_visibility(&self) {
        let any = self.players.borrow().iter().any(|p| p.state.borrow().has_metadata);
        self.root.set_visible(any);
    }
}

#[derive(Default)]
struct State {
    identity: String,
    desktop_entry: String,
    can_raise: bool,
    status: String,
    can_control: bool,
    can_play: bool,
    can_pause: bool,
    can_previous: bool,
    can_next: bool,
    title: String,
    artists: Vec<String>,
    art_url: String,
    has_metadata: bool,
}

struct Player {
    name: String,
    conn: gio::DBusConnection,
    card: Card,
    state: RefCell<State>,
    subscription: RefCell<Option<gio::SignalSubscriptionId>>,
    section: Weak<Section>,
}

impl Player {
    fn new(conn: &gio::DBusConnection, name: &str, owner: &str, section: Weak<Section>) -> Rc<Self> {
        let this = Rc::new(Self {
            name: name.to_owned(),
            conn: conn.clone(),
            card: Card::new(),
            state: RefCell::default(),
            subscription: RefCell::default(),
            section,
        });
        this.card.button.set_visible(false);
        this.connect_buttons();

        let refresh = system::debounced(glib::clone!(
            #[weak]
            this,
            move || this.refresh()
        ));
        #[allow(deprecated)]
        let id = conn.signal_subscribe(
            Some(owner),
            Some("org.freedesktop.DBus.Properties"),
            Some("PropertiesChanged"),
            Some(PATH),
            None,
            gio::DBusSignalFlags::NONE,
            glib::clone!(
                #[strong]
                refresh,
                move |_, _, _, _, _, _| refresh()
            ),
        );
        this.subscription.replace(Some(id));
        this.refresh();
        this
    }

    fn connect_buttons(self: &Rc<Self>) {
        let card = &self.card;
        for (button, method) in [(&card.previous, "Previous"), (&card.play_pause, "PlayPause"), (&card.next, "Next")] {
            button.connect_clicked(glib::clone!(
                #[weak(rename_to = this)]
                self,
                move |_| this.call(PLAYER_IFACE, method)
            ));
        }
        card.button.connect_clicked(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |button| {
                if !this.state.borrow().can_raise {
                    return;
                }
                this.call(PREFIX, "Raise");
                if let Some(popover) = button.ancestor(gtk::Popover::static_type()).and_downcast::<gtk::Popover>() {
                    popover.popdown();
                }
            }
        ));
    }

    fn call(&self, iface: &str, method: &str) {
        system::call(&self.conn, &self.name, PATH, iface, method, None);
    }

    /// Re-reads both interfaces; PropertiesChanged only says something moved.
    fn refresh(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        get_all(&self.conn, &self.name, PREFIX, move |root| {
            let Some(this) = weak.upgrade() else { return };
            let weak = Rc::downgrade(&this);
            get_all(&this.conn, &this.name, PLAYER_IFACE, move |player| {
                if let Some(this) = weak.upgrade() {
                    this.apply(root, player);
                }
            });
        });
    }

    fn apply(&self, root: Option<Props>, player: Option<Props>) {
        let mut state = State::default();
        if let Some(root) = root {
            state.identity = root.string("Identity");
            state.desktop_entry = root.string("DesktopEntry");
            state.can_raise = root.get("CanRaise").unwrap_or(false);
        }
        if let Some(player) = player {
            state.status = player.string("PlaybackStatus");
            state.can_control = player.get("CanControl").unwrap_or(true);
            state.can_play = player.get("CanPlay").unwrap_or(false);
            state.can_pause = player.get("CanPause").unwrap_or(false);
            state.can_previous = player.get("CanGoPrevious").unwrap_or(false);
            state.can_next = player.get("CanGoNext").unwrap_or(false);
            if let Some(metadata) = player.value("Metadata").filter(|m| m.n_children() > 0) {
                let metadata = Props(metadata);
                state.has_metadata = true;
                state.title = metadata.string("xesam:title");
                state.artists = metadata.value("xesam:artist").map(|v| system::paths(&v)).unwrap_or_default();
                state.art_url = metadata.string("mpris:artUrl");
            }
        }
        self.card.show(&state);
        self.state.replace(state);
        if let Some(section) = self.section.upgrade() {
            section.update_visibility();
        }
    }
}

fn get_all(conn: &gio::DBusConnection, name: &str, iface: &str, f: impl FnOnce(Option<Props>) + 'static) {
    conn.call(
        Some(name),
        PATH,
        "org.freedesktop.DBus.Properties",
        "GetAll",
        Some(&(iface,).to_variant()),
        Some(glib::VariantTy::new("(a{sv})").unwrap()),
        gio::DBusCallFlags::NONE,
        2000,
        gio::Cancellable::NONE,
        move |result| f(result.ok().map(|v| Props(v.child_value(0)))),
    );
}

/// GNOME Shell's MediaMessage: app header, art with title and artist, and
/// the playback controls underneath.
struct Card {
    button: gtk::Button,
    app_icon: gtk::Image,
    app_name: gtk::Label,
    art: gtk::Image,
    title: gtk::Label,
    artist: gtk::Label,
    previous: gtk::Button,
    play_pause: gtk::Button,
    play_pause_icon: gtk::Image,
    next: gtk::Button,
}

impl Card {
    fn new() -> Self {
        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let button = gtk::Button::builder().name("media-card").child(&content).css_classes(["media-card"]).build();

        let header = gtk::Box::builder().spacing(6).css_classes(["header"]).build();
        let app_icon = gtk::Image::builder().pixel_size(16).build();
        let app_name = gtk::Label::builder()
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(pango::EllipsizeMode::End)
            .css_classes(["app-name"])
            .build();
        header.append(&app_icon);
        header.append(&app_name);
        content.append(&header);

        let body = gtk::Box::builder().spacing(12).build();
        let art = gtk::Image::builder()
            .pixel_size(ART_SIZE)
            .width_request(ART_SIZE)
            .height_request(ART_SIZE)
            .overflow(gtk::Overflow::Hidden)
            .css_classes(["art"])
            .build();
        let text = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .valign(gtk::Align::Center)
            .hexpand(true)
            .build();
        let label = |class: &str| {
            gtk::Label::builder()
                .xalign(0.0)
                .ellipsize(pango::EllipsizeMode::End)
                .css_classes([class])
                .build()
        };
        let title = label("title");
        let artist = label("artist");
        text.append(&title);
        text.append(&artist);
        body.append(&art);
        body.append(&text);
        content.append(&body);

        let controls = gtk::Box::builder().spacing(12).halign(gtk::Align::Center).css_classes(["controls"]).build();
        let control = |name: &str, icon: &str| {
            gtk::Button::builder()
                .name(name)
                .icon_name(icon)
                .css_classes(["circular", "flat"])
                .build()
        };
        let previous = control("media-previous", "media-skip-backward-symbolic");
        let play_pause_icon = gtk::Image::from_icon_name("media-playback-start-symbolic");
        let play_pause = gtk::Button::builder()
            .name("media-play-pause")
            .child(&play_pause_icon)
            .css_classes(["circular", "flat", "play-pause"])
            .build();
        let next = control("media-next", "media-skip-forward-symbolic");
        controls.append(&previous);
        controls.append(&play_pause);
        controls.append(&next);
        content.append(&controls);

        Self { button, app_icon, app_name, art, title, artist, previous, play_pause, play_pause_icon, next }
    }

    fn show(&self, state: &State) {
        self.button.set_visible(state.has_metadata);
        if !state.has_metadata {
            return;
        }
        let app = Some(&state.desktop_entry)
            .filter(|e| !e.is_empty())
            .and_then(|e| desktop_app(e));
        let name = app.as_ref().map(|a| a.name().to_string()).unwrap_or_else(|| state.identity.clone());
        self.app_name.set_label(&name);
        match app.and_then(|a| a.icon()) {
            Some(icon) => self.app_icon.set_from_gicon(&icon),
            None => self.app_icon.set_icon_name(Some("audio-x-generic-symbolic")),
        }

        self.title.set_label(if state.title.is_empty() { "Unknown title" } else { &state.title });
        let artist = state.artists.join(", ");
        self.artist.set_label(if artist.is_empty() { "Unknown artist" } else { &artist });

        match load_art(&state.art_url) {
            Some(texture) => {
                self.art.set_paintable(Some(&texture));
                self.art.set_pixel_size(ART_SIZE);
                self.art.set_widget_name("media-art");
                self.art.remove_css_class("fallback");
            }
            None => {
                self.art.set_icon_name(Some("audio-x-generic-symbolic"));
                self.art.set_pixel_size(ART_SIZE / 2);
                self.art.set_widget_name("media-art-fallback");
                self.art.add_css_class("fallback");
            }
        }

        let playing = state.status == "Playing";
        self.play_pause_icon.set_icon_name(Some(if playing {
            "media-playback-pause-symbolic"
        } else {
            "media-playback-start-symbolic"
        }));
        self.play_pause_icon
            .set_widget_name(if playing { "media-status-playing" } else { "media-status-paused" });
        let control = state.can_control;
        self.play_pause.set_sensitive(control && if playing { state.can_pause } else { state.can_play });
        self.previous.set_sensitive(control && state.can_previous);
        self.next.set_sensitive(control && state.can_next);
        self.button.set_tooltip_text(state.can_raise.then_some(name.as_str()));
    }
}

/// The installed application with desktop file id `id` (no ".desktop").
fn desktop_app(id: &str) -> Option<gio::AppInfo> {
    let desktop_id = format!("{id}.desktop");
    gio::AppInfo::all()
        .into_iter()
        .find(|info| info.id().is_some_and(|i| i == desktop_id.as_str()))
}

/// Local (file://) art only; anything else gets the fallback icon.
fn load_art(url: &str) -> Option<gdk::Texture> {
    if !url.starts_with("file://") {
        return None;
    }
    gdk::Texture::from_file(&gio::File::for_uri(url)).ok()
}

fn load_css() {
    let Some(display) = gdk::Display::default() else { return };
    let provider = gtk::CssProvider::new();
    provider.load_from_string(include_str!("style.css"));
    gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
}
