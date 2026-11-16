//! GNOME Shell's keyboard layouts (layouts/*.json, see layouts/README) and
//! how one is picked, like js/ui/keyboard.js: by content purpose (number
//! pads, email and URL keys), else by the active XKB layout, else US.

use super::json::{self, Value};

/// The layouts we ship, by GNOME Shell's file name.
const LAYOUTS: &[(&str, &str)] = &[
    ("us", include_str!("layouts/us.json")),
    ("uk", include_str!("layouts/uk.json")),
    ("de", include_str!("layouts/de.json")),
    ("fr", include_str!("layouts/fr.json")),
    ("es", include_str!("layouts/es.json")),
    ("it", include_str!("layouts/it.json")),
    ("digits", include_str!("layouts/digits.json")),
    ("number", include_str!("layouts/number.json")),
    ("phone", include_str!("layouts/phone.json")),
    ("email", include_str!("layouts/email.json")),
    ("url", include_str!("layouts/url.json")),
];

pub const BACKSPACE: u32 = 0xff08;
pub const RETURN: u32 = 0xff0d;

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Type the text.
    Commit(String),
    /// A real key (e.g. Return).
    Keyval(u32),
    Delete,
    LevelSwitch(String),
    Hide,
    Language,
    Emoji,
}

#[derive(Debug, Clone)]
pub struct Key {
    pub action: Action,
    pub label: Option<String>,
    pub icon: Option<String>,
    /// Accented variants, shown on long press.
    pub variants: Vec<String>,
    pub width: f64,
    pub height: f64,
    pub left_offset: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Mode {
    Default,
    /// Back to the default level after one key (shift).
    Latched,
    Locked,
}

#[derive(Debug, Clone)]
pub struct Level {
    pub name: String,
    pub mode: Mode,
    pub rows: Vec<Vec<Key>>,
}

#[derive(Debug, Clone)]
pub struct Layout {
    pub levels: Vec<Level>,
}

/// The layout file for a content purpose (from org.gnome.ShellPlus.Osk).
pub fn for_purpose(purpose: &str) -> Option<&'static str> {
    match purpose {
        "digits" => Some("digits"),
        "number" => Some("number"),
        "phone" => Some("phone"),
        "email" => Some("email"),
        "url" => Some("url"),
        _ => None,
    }
}

/// The layout file for an XKB layout id such as "de+nodeadkeys".
pub fn for_xkb(id: &str) -> &'static str {
    let base = id.split('+').next().unwrap_or(id);
    // GNOME Shell's British layout file is "uk"; XKB calls it "gb".
    let base = if base == "gb" { "uk" } else { base };
    [id, base]
        .into_iter()
        .find_map(|name| LAYOUTS.iter().find(|(n, _)| *n == name).map(|(n, _)| *n))
        .unwrap_or("us")
}

pub fn load(id: &str) -> Layout {
    let source = LAYOUTS.iter().find(|(n, _)| *n == id).unwrap_or(&LAYOUTS[0]);
    let value = json::parse(source.1).unwrap_or_else(|err| {
        panic!("built-in keyboard layout {} is invalid: {err}", source.0)
    });
    Layout {
        levels: value.get("levels").map(Value::as_array).unwrap_or_default().iter().map(level).collect(),
    }
}

fn level(value: &Value) -> Level {
    let mode = match value.get("mode").and_then(Value::as_str) {
        Some("latched") => Mode::Latched,
        Some("locked") => Mode::Locked,
        _ => Mode::Default,
    };
    Level {
        name: value.get("level").and_then(Value::as_str).unwrap_or("default").to_owned(),
        mode,
        rows: value
            .get("rows")
            .map(Value::as_array)
            .unwrap_or_default()
            .iter()
            .map(|row| row.as_array().iter().filter_map(key).collect())
            .collect(),
    }
}

fn key(value: &Value) -> Option<Key> {
    let text = |name: &str| value.get(name).and_then(Value::as_str).map(str::to_owned);
    let number = |name: &str, default: f64| value.get(name).and_then(Value::as_f64).unwrap_or(default);
    let strings: Vec<String> = value
        .get("strings")
        .map(Value::as_array)
        .unwrap_or_default()
        .iter()
        .filter_map(|s| s.as_str().map(str::to_owned))
        .collect();

    let action = match value.get("action").and_then(Value::as_str) {
        Some("delete") => Action::Delete,
        Some("levelSwitch") => Action::LevelSwitch(text("level").unwrap_or_else(|| "default".into())),
        Some("hide") => Action::Hide,
        Some("languageMenu") => Action::Language,
        Some("emoji") => Action::Emoji,
        // Modifiers (Ctrl, Alt…) of the "extended" layouts aren't supported.
        Some(_) => return None,
        None => match text("keyval") {
            Some(keyval) => Action::Keyval(
                u32::from_str_radix(keyval.trim_start_matches("0x"), 16).ok()?,
            ),
            None => Action::Commit(strings.first()?.clone()),
        },
    };
    Some(Key {
        action,
        label: text("label"),
        icon: text("iconName"),
        variants: if strings.len() > 1 { strings[1..].to_vec() } else { Vec::new() },
        width: number("width", 1.0),
        height: number("height", 1.0),
        left_offset: number("leftOffset", 0.0),
    })
}

impl Layout {
    /// Drops keys we don't offer: the emoji key always, the layout key
    /// with a single input source. Their room goes to the space bar, so
    /// the rows keep their shape.
    pub fn without_unsupported_keys(mut self, multiple_sources: bool) -> Self {
        for level in &mut self.levels {
            for row in &mut level.rows {
                let mut freed = 0.0;
                row.retain(|key| {
                    let drop = key.action == Action::Emoji ||
                        (key.action == Action::Language && !multiple_sources);
                    if drop {
                        freed += key.width + key.left_offset;
                    }
                    !drop
                });
                if let Some(space) = row.iter_mut().find(|k| k.action == Action::Commit(" ".into())) {
                    space.width += freed;
                }
            }
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_layouts_load() {
        for (id, _) in LAYOUTS {
            let layout = load(id);
            assert!(!layout.levels.is_empty(), "{id}");
            assert!(layout.levels.iter().all(|l| !l.rows.is_empty()), "{id}");
        }
    }

    #[test]
    fn picks_layouts() {
        assert_eq!(for_xkb("de+nodeadkeys"), "de");
        assert_eq!(for_xkb("gb"), "uk");
        assert_eq!(for_xkb("ru"), "us");
        assert_eq!(for_purpose("number"), Some("number"));
        assert_eq!(for_purpose("password"), None);
    }

    #[test]
    fn us_has_shift_symbols_and_accents() {
        let us = load("us").without_unsupported_keys(false);
        let names: Vec<&str> = us.levels.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, ["default", "shift", "opt", "opt+shift"]);
        assert_eq!(us.levels[1].mode, Mode::Latched);
        let e = &us.levels[0].rows[0][2];
        assert_eq!(e.action, Action::Commit("e".into()));
        assert!(e.variants.contains(&"é".to_owned()));
        assert!(us.levels.iter().flat_map(|l| &l.rows).flatten().all(|k| k.action != Action::Emoji));
    }
}
