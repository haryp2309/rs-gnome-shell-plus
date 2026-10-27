//! Calendar events in the date menu, like GNOME Shell's EventsSection.
//!
//! Events come from GNOME Shell's calendar server
//! (`gnome-shell-calendar-server`, org.gnome.Shell.CalendarServer on the
//! session bus), which reads evolution-data-server. Our session doesn't
//! start it, but it's D-Bus activatable when gnome-shell is installed, so
//! the first SetTimeRange call (when the menu first opens) starts it. When
//! it's missing everything here stays hidden.
//!
//! Days of the shown month that have events are marked in the gtk::Calendar,
//! and a card below the calendar lists the selected day's events (all-day
//! ones first, then by start time). It hides when the day has none. Clicking
//! it opens GNOME Calendar, if installed.
//!
//! Widget names (for dev/driver.py): `calendar-events` (the card),
//! `calendar-events-title`, and per event, in display order,
//! `calendar-event-N` (summary) and `calendar-event-time-N`.

use crate::system;
use gtk::prelude::*;
use gtk::{gdk, gio, glib, pango};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

const NAME: &str = "org.gnome.Shell.CalendarServer";
const PATH: &str = "/org/gnome/Shell/CalendarServer";
const IFACE: &str = "org.gnome.Shell.CalendarServer";
const CALENDAR_APP: &str = "org.gnome.Calendar.desktop";

/// The events card for `calendar`, to place under it. It follows the
/// calendar's selected day and shown month by itself.
pub fn section(calendar: &gtk::Calendar) -> gtk::Widget {
    load_css();
    let card = Card::new();
    let root = card.button.clone().upcast::<gtk::Widget>();
    let Some(conn) = system::session_bus() else { return root };
    let this = Rc::new(Section {
        conn,
        calendar: calendar.clone(),
        card,
        events: RefCell::default(),
        owner: RefCell::default(),
        has_calendars: Cell::new(true),
        range: Cell::new(None),
        in_flight: Cell::new(false),
        refresh_pending: Cell::new(false),
    });
    this.connect();
    // The menu keeps the widget for the panel's lifetime; so does this.
    root.connect_destroy(move |_| {
        let _ = &this;
    });
    root
}

#[derive(Clone, Debug)]
struct Event {
    summary: String,
    start: i64,
    end: i64,
}

struct Section {
    conn: gio::DBusConnection,
    calendar: gtk::Calendar,
    card: Card,
    /// By the server's event id: "source uid \n event uid \n recurrence id".
    events: RefCell<HashMap<String, Event>>,
    owner: RefCell<Option<String>>,
    has_calendars: Cell<bool>,
    /// The range last asked for with SetTimeRange.
    range: Cell<Option<(i64, i64)>>,
    in_flight: Cell<bool>,
    refresh_pending: Cell<bool>,
}

impl Section {
    fn connect(self: &Rc<Self>) {
        // The server's signals. A well-known sender in the match rule follows
        // whichever process owns the name.
        let weak = Rc::downgrade(self);
        #[allow(deprecated)]
        self.conn.signal_subscribe(
            Some(NAME),
            Some(IFACE),
            None,
            Some(PATH),
            None,
            gio::DBusSignalFlags::NONE,
            move |_, _, _, _, signal, params| {
                if let Some(this) = weak.upgrade() {
                    this.on_signal(signal, params);
                }
            },
        );
        let weak = Rc::downgrade(self);
        #[allow(deprecated)]
        self.conn.signal_subscribe(
            Some(NAME),
            Some("org.freedesktop.DBus.Properties"),
            Some("PropertiesChanged"),
            Some(PATH),
            Some(IFACE),
            gio::DBusSignalFlags::MATCH_ARG0_NAMESPACE,
            move |_, _, _, _, _, params| {
                let Some(this) = weak.upgrade() else { return };
                if let Some(value) = system::lookup(&params.child_value(1), "HasCalendars").and_then(|v| v.get()) {
                    this.has_calendars.set(value);
                    this.refresh();
                }
            },
        );

        let appeared = Rc::downgrade(self);
        let vanished = Rc::downgrade(self);
        gio::bus_watch_name_on_connection(
            &self.conn,
            NAME,
            gio::BusNameWatcherFlags::NONE,
            move |_, _, owner| {
                if let Some(this) = appeared.upgrade() {
                    this.on_appeared(owner);
                }
            },
            move |_, _| {
                if let Some(this) = vanished.upgrade() {
                    this.on_vanished();
                }
            },
        );

        // Follow the selected day and the shown month. Changing the month
        // emits several notifications; handle them together.
        let schedule = {
            let weak = Rc::downgrade(self);
            move || {
                if let Some(this) = weak.upgrade() {
                    this.schedule_refresh();
                }
            }
        };
        for property in ["day", "month", "year"] {
            let schedule = schedule.clone();
            self.calendar.connect_notify_local(Some(property), move |_, _| schedule());
        }
        // Each time the menu opens: ask for the shown month, which starts the
        // server on demand.
        self.calendar.connect_map(move |_| schedule());

        let weak = Rc::downgrade(self);
        self.card.button.connect_clicked(move |button| {
            if let Some(this) = weak.upgrade() {
                this.open_calendar_app(button);
            }
        });
        self.refresh();
    }

    fn schedule_refresh(self: &Rc<Self>) {
        if self.refresh_pending.replace(true) {
            return;
        }
        let weak = Rc::downgrade(self);
        glib::idle_add_local_once(move || {
            if let Some(this) = weak.upgrade() {
                this.refresh_pending.set(false);
                if this.calendar.is_mapped() {
                    this.request(false);
                }
                this.refresh();
            }
        });
    }

    /// The shown month, as Unix times [first day 00:00, next first day).
    fn month_range(&self) -> Option<(i64, i64)> {
        let date = self.calendar.date();
        let begin = glib::DateTime::from_local(date.year(), date.month(), 1, 0, 0, 0.0).ok()?;
        let end = begin.add_months(1).ok()?;
        Some((begin.to_unix(), end.to_unix()))
    }

    /// SetTimeRange for the shown month, when it changed or with `force`.
    /// Without an owner this activates the server, if it's installed.
    fn request(self: &Rc<Self>, force: bool) {
        let Some(range) = self.month_range() else { return };
        let owned = self.owner.borrow().is_some();
        if owned && !force && self.range.get() == Some(range) {
            return;
        }
        if !owned && self.in_flight.get() {
            return; // Already activating it.
        }
        if self.range.get() != Some(range) {
            // A new range replaces the old one; the server resends it all.
            self.events.borrow_mut().clear();
            self.refresh();
        }
        self.range.set(Some(range));
        self.in_flight.set(true);
        let weak = Rc::downgrade(self);
        self.conn.call(
            Some(NAME),
            PATH,
            IFACE,
            "SetTimeRange",
            Some(&(range.0, range.1, true).to_variant()),
            None,
            gio::DBusCallFlags::NONE,
            -1,
            gio::Cancellable::NONE,
            move |result| {
                let Some(this) = weak.upgrade() else { return };
                this.in_flight.set(false);
                if result.is_err() && this.owner.borrow().is_none() {
                    // Not installed (or failed to start): stay hidden, and try
                    // again next time the menu opens.
                    this.range.set(None);
                }
            },
        );
    }

    fn on_appeared(self: &Rc<Self>, owner: &str) {
        self.owner.replace(Some(owner.to_owned()));
        self.events.borrow_mut().clear();
        let weak = Rc::downgrade(self);
        self.conn.call(
            Some(NAME),
            PATH,
            "org.freedesktop.DBus.Properties",
            "Get",
            Some(&(IFACE, "HasCalendars").to_variant()),
            Some(glib::VariantTy::new("(v)").unwrap()),
            gio::DBusCallFlags::NO_AUTO_START,
            2000,
            gio::Cancellable::NONE,
            move |result| {
                let Some(this) = weak.upgrade() else { return };
                let value = result.ok().and_then(|r| r.child_value(0).as_variant()).and_then(|v| v.get::<bool>());
                this.has_calendars.set(value.unwrap_or(true));
                this.refresh();
            },
        );
        // The activating call (if any) is on its way to the new owner already.
        if !self.in_flight.get() && (self.range.get().is_some() || self.calendar.is_mapped()) {
            self.request(true);
        }
        self.refresh();
    }

    fn on_vanished(&self) {
        self.owner.replace(None);
        self.events.borrow_mut().clear();
        // Ask again (and so restart it) the next time the menu opens.
        self.range.set(None);
        self.refresh();
    }

    fn on_signal(&self, signal: &str, params: &glib::Variant) {
        match signal {
            "EventsAddedOrUpdated" => {
                let Some((list,)) = params.get::<(Vec<(String, String, i64, i64, HashMap<String, glib::Variant>)>,)>() else {
                    return;
                };
                let mut events = self.events.borrow_mut();
                let mut cleared = std::collections::HashSet::new();
                for (id, summary, start, end, _extras) in list {
                    // An instance of a recurring event: the server sends all
                    // instances again, so drop the old ones first.
                    if !id.ends_with('\n') {
                        let parent = id[..id.rfind('\n').map_or(0, |i| i + 1)].to_owned();
                        if cleared.insert(parent.clone()) {
                            events.retain(|k, _| !k.starts_with(&parent));
                        }
                    }
                    events.insert(id, Event { summary, start, end });
                }
            }
            "EventsRemoved" => {
                let Some((ids,)) = params.get::<(Vec<String>,)>() else { return };
                self.events.borrow_mut().retain(|k, _| !ids.iter().any(|id| k.starts_with(id.as_str())));
            }
            "ClientDisappeared" => {
                let Some((source,)) = params.get::<(String,)>() else { return };
                let prefix = format!("{source}\n");
                self.events.borrow_mut().retain(|k, _| !k.starts_with(&prefix));
            }
            _ => return,
        }
        self.refresh();
    }

    /// Re-marks the shown month and rebuilds the selected day's list.
    fn refresh(&self) {
        let available = self.owner.borrow().is_some() && self.has_calendars.get();
        let events = self.events.borrow();

        self.calendar.clear_marks();
        let date = self.calendar.date();
        if available {
            for day in 1..=31 {
                let Some((begin, end)) = day_range(date.year(), date.month(), day) else { break };
                if events.values().any(|e| overlaps(e, begin, end)) {
                    self.calendar.mark_day(day as u32);
                }
            }
        }

        let Some((begin, end)) = day_range(date.year(), date.month(), date.day_of_month()) else { return };
        let mut today: Vec<&Event> = if available {
            events.values().filter(|e| overlaps(e, begin, end)).collect()
        } else {
            Vec::new()
        };
        let covers_day = |e: &Event| e.start <= begin && e.end >= end;
        today.sort_by(|a, b| {
            covers_day(b)
                .cmp(&covers_day(a))
                .then(a.start.cmp(&b.start))
                .then(a.end.cmp(&b.end))
                .then_with(|| a.summary.cmp(&b.summary))
        });
        self.card.show(begin, end, &today);
    }

    fn open_calendar_app(&self, button: &gtk::Button) {
        let launched = if let Some(app) = calendar_app() {
            let context = gdk::Display::default().map(|d| d.app_launch_context());
            app.launch(&[], context.as_ref()).is_ok()
        } else if glib::find_program_in_path("gnome-calendar").is_some() {
            system::spawn(&["gnome-calendar"]);
            true
        } else {
            false
        };
        if launched {
            if let Some(popover) = button.ancestor(gtk::Popover::static_type()).and_downcast::<gtk::Popover>() {
                popover.popdown();
            }
        }
    }
}

/// GNOME Calendar's desktop file, if installed.
fn calendar_app() -> Option<gio::AppInfo> {
    gio::AppInfo::all().into_iter().find(|info| info.id().is_some_and(|id| id == CALENDAR_APP))
}

/// Unix times of [day 00:00, next day 00:00), local time; `None` past the
/// end of the month.
fn day_range(year: i32, month: i32, day: i32) -> Option<(i64, i64)> {
    let begin = glib::DateTime::from_local(year, month, day, 0, 0, 0.0).ok()?;
    if begin.month() != month {
        return None;
    }
    Some((begin.to_unix(), begin.add_days(1).ok()?.to_unix()))
}

/// GNOME Shell's _eventOverlapsInterval (zero-length events count).
fn overlaps(e: &Event, begin: i64, end: i64) -> bool {
    if e.start >= begin && e.end < end {
        return true;
    }
    e.end > begin && e.start < end
}

fn local(time: i64) -> Option<glib::DateTime> {
    glib::DateTime::from_unix_local(time).ok()
}

fn format(time: i64, format: &str) -> String {
    local(time).and_then(|t| t.format(format).ok()).map(|s| s.to_string()).unwrap_or_default()
}

fn at_midnight(time: i64) -> bool {
    local(time).is_some_and(|t| t.hour() == 0 && t.minute() == 0 && t.second() == 0)
}

/// GNOME Shell's EventsSection._formatEventTime.
fn format_time(e: &Event, begin: i64, end: i64) -> String {
    const DASH: &str = "\u{2013}";
    let twelve_hour = system::settings("org.gnome.desktop.interface").is_some_and(|s| s.string("clock-format") == "12h");
    let time_format = if twelve_hour { "%-l:%M %p" } else { "%R" };
    let start_time = format(e.start, time_format);
    let end_time = format(e.end, time_format);

    if e.start == begin && e.end == end {
        return "All Day".into();
    }
    if e.start < begin || e.end > end {
        // Spans several days: show the dates (an end at midnight belongs to
        // the day before).
        let shown_end = if at_midnight(e.end) { e.end - 1 } else { e.end };
        let this_year = glib::DateTime::now_local().map(|n| n.year()).unwrap_or_default();
        let same_year = [e.start, shown_end].iter().all(|&t| local(t).is_some_and(|t| t.year() == this_year));
        let date_format = if same_year { "%m/%d" } else { "%x" };
        let start_date = format(e.start, date_format);
        let end_date = format(shown_end, date_format);
        return if at_midnight(e.start) && at_midnight(e.end) {
            format!("{start_date} {DASH} {end_date}")
        } else {
            format!("{start_date} {start_time} {DASH} {end_date} {end_time}")
        };
    }
    if e.start == e.end {
        return start_time;
    }
    format!("{start_time} {DASH} {end_time}")
}

/// The card: a day heading and the list, clickable as a whole like GNOME's.
struct Card {
    button: gtk::Button,
    title: gtk::Label,
    list: gtk::Box,
}

impl Card {
    fn new() -> Self {
        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let button = gtk::Button::builder()
            .name("calendar-events")
            .child(&content)
            .visible(false)
            .css_classes(["events-button"])
            .build();
        let title = gtk::Label::builder()
            .name("calendar-events-title")
            .xalign(0.0)
            .css_classes(["events-title"])
            .build();
        let list = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .css_classes(["events-list"])
            .build();
        content.append(&title);
        content.append(&list);
        Self { button, title, list }
    }

    fn show(&self, begin: i64, end: i64, events: &[&Event]) {
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        // GNOME shows "No Events"; we hide the card instead.
        self.button.set_visible(!events.is_empty());
        if events.is_empty() {
            return;
        }
        self.title.set_label(&day_title(begin, end));

        // Labels shouldn't widen the menu: they take the calendar's width
        // and ellipsize.
        let label = |name: String, text: &str, class: &str| {
            gtk::Label::builder()
                .name(name)
                .label(text)
                .xalign(0.0)
                .hexpand(true)
                .max_width_chars(1)
                .ellipsize(pango::EllipsizeMode::End)
                .css_classes([class])
                .build()
        };
        for (n, event) in events.iter().enumerate() {
            let row = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .css_classes(["event-box"])
                .build();
            let summary = label(format!("calendar-event-{n}"), &event.summary, "event-summary");
            summary.set_tooltip_text(Some(&event.summary));
            row.append(&summary);
            row.append(&label(format!("calendar-event-time-{n}"), &format_time(event, begin, end), "event-time"));
            self.list.append(&row);
        }
        let has_app = calendar_app().is_some()
            || glib::find_program_in_path("gnome-calendar").is_some();
        if has_app {
            self.button.remove_css_class("inert");
        } else {
            self.button.add_css_class("inert");
        }
    }
}

/// "Today", "Yesterday", "Tomorrow", or the date, like GNOME's events title.
fn day_title(begin: i64, end: i64) -> String {
    let now = glib::DateTime::now_local().map(|n| n.to_unix()).unwrap_or_default();
    const DAY: i64 = 24 * 60 * 60;
    if begin <= now && now < end {
        "Today".into()
    } else if end <= now && now - end < DAY {
        "Yesterday".into()
    } else if begin > now && begin - now <= DAY {
        "Tomorrow".into()
    } else {
        let this_year = glib::DateTime::now_local().map(|n| n.year()).unwrap_or_default();
        let year = local(begin).map(|t| t.year()).unwrap_or_default();
        format(begin, if year == this_year { "%B %-d" } else { "%B %-d %Y" })
    }
}

fn load_css() {
    let Some(display) = gdk::Display::default() else { return };
    let provider = gtk::CssProvider::new();
    provider.load_from_string(include_str!("style.css"));
    gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
}
