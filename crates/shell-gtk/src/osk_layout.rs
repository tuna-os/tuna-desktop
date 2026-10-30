//! On-screen keyboard layouts, ported from GNOME Shell 51.0's
//! `js/ui/keyboard.js` (`KeyboardModel`, `KeyContainer`, `Keyboard._updateLayout`,
//! `Keyboard._addRowKeys`, `EmojiSelection._populateSections`).
//!
//! The layouts and the emoji list are GNOME's own JSON files
//! (`osk-layouts/README.md`), bundled as a GResource under
//! [`RESOURCE_PREFIX`]. Everything here is pure data; the GTK view lives
//! elsewhere.

use std::fmt;

use serde_json::{Map, Value};

/// GResource prefix of the bundled layouts (GNOME: `/org/gnome/shell/osk-layouts`).
pub const RESOURCE_PREFIX: &str = "/org/tuna/Shell/osk-layouts";

/// Content purpose of the focused text field, as Clutter's
/// `InputContentPurpose` (mutter's `translate_purpose` of the
/// zwp_text_input_v3 value).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    Normal,
    Alpha,
    Digits,
    Number,
    Phone,
    Url,
    Email,
    Name,
    Password,
    Date,
    Time,
    DateTime,
    Terminal,
}

impl Purpose {
    /// zwp_text_input_v3 `content_purpose`: normal, alpha, digits, number,
    /// phone, url, email, name, password, pin, date, time, datetime,
    /// terminal. Like mutter's `translate_purpose`, PIN and unknown values
    /// map to [`Purpose::Normal`].
    pub fn from_wire(purpose: u32) -> Self {
        match purpose {
            1 => Purpose::Alpha,
            2 => Purpose::Digits,
            3 => Purpose::Number,
            4 => Purpose::Phone,
            5 => Purpose::Url,
            6 => Purpose::Email,
            7 => Purpose::Name,
            8 => Purpose::Password,
            10 => Purpose::Date,
            11 => Purpose::Time,
            12 => Purpose::DateTime,
            13 => Purpose::Terminal,
            _ => Purpose::Normal,
        }
    }
}

/// zwp_text_input_v3 `content_hint` bits the keyboard consults.
pub mod hints {
    pub const AUTO_CAPITALIZATION: u32 = 0x4;
    pub const LOWERCASE: u32 = 0x8;
    pub const UPPERCASE: u32 = 0x10;
    pub const TITLECASE: u32 = 0x20;
    /// `on_screen_input_provided`, Clutter's `INHIBIT_OSK`.
    pub const INHIBIT_OSK: u32 = 0x400;
    pub const NO_EMOJI: u32 = 0x800;
}

/// A level's `mode` (`default`, `latched`, `locked`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LevelMode {
    Default,
    Latched,
    Locked,
}

/// A key's `action`.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Hide,
    LanguageMenu,
    Emoji,
    Modifier,
    Delete,
    LevelSwitch,
    Other(String),
}

/// One key of a layout row.
#[derive(Debug, Clone, PartialEq)]
pub struct Key {
    /// `[0]` is the commit string, the rest the long-press extended keys.
    pub strings: Vec<String>,
    pub label: Option<String>,
    pub icon_name: Option<String>,
    /// Parsed from the layout's hex string (`"0xff0d"`).
    pub keyval: Option<u32>,
    pub action: Option<Action>,
    pub level: Option<String>,
    pub width: f64,
    pub height: f64,
    pub left_offset: f64,
}

impl Key {
    /// The string committed on click (`strings.shift()` in `_addRowKeys`).
    pub fn commit_string(&self) -> Option<&str> {
        self.strings.first().map(String::as_str)
    }

    /// The long-press popup keys (what remains of `strings` after the shift).
    pub fn extended(&self) -> &[String] {
        self.strings.get(1..).unwrap_or(&[])
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Level {
    pub level: String,
    pub mode: LevelMode,
    pub rows: Vec<Vec<Key>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    pub name: String,
    pub locale: String,
    pub levels: Vec<Level>,
}

impl Layout {
    pub fn level(&self, name: &str) -> Option<&Level> {
        self.levels.iter().find(|l| l.level == name)
    }
}

#[derive(Debug)]
pub enum Error {
    Json(serde_json::Error),
    Shape(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Json(e) => write!(f, "invalid JSON: {e}"),
            Error::Shape(s) => write!(f, "unexpected layout shape: {s}"),
        }
    }
}

impl std::error::Error for Error {}

fn shape<T>(msg: impl Into<String>) -> Result<T, Error> {
    Err(Error::Shape(msg.into()))
}

fn opt_str(obj: &Map<String, Value>, field: &str) -> Result<Option<String>, Error> {
    match obj.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => shape(format!("\"{field}\" is not a string")),
    }
}

fn opt_num(obj: &Map<String, Value>, field: &str, default: f64) -> Result<f64, Error> {
    match obj.get(field) {
        None | Some(Value::Null) => Ok(default),
        Some(v) => v
            .as_f64()
            .map_or_else(|| shape(format!("\"{field}\" is not a number")), Ok),
    }
}

fn parse_keyval(v: &Value) -> Result<u32, Error> {
    match v {
        Value::String(s) => {
            let hex = s
                .strip_prefix("0x")
                .or_else(|| s.strip_prefix("0X"))
                .unwrap_or(s);
            u32::from_str_radix(hex, 16).or_else(|_| shape(format!("bad keyval {s:?}")))
        }
        Value::Number(n) => n
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .map_or_else(|| shape(format!("bad keyval {n}")), Ok),
        _ => shape("\"keyval\" is not a string"),
    }
}

fn parse_key(v: &Value) -> Result<Key, Error> {
    let Some(obj) = v.as_object() else {
        return shape("key is not an object");
    };
    let strings = match obj.get("strings") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(a)) => a
            .iter()
            .map(|s| {
                s.as_str()
                    .map(str::to_owned)
                    .map_or_else(|| shape("\"strings\" entry is not a string"), Ok)
            })
            .collect::<Result<_, _>>()?,
        Some(_) => return shape("\"strings\" is not an array"),
    };
    let action = opt_str(obj, "action")?.map(|a| match a.as_str() {
        "hide" => Action::Hide,
        "languageMenu" => Action::LanguageMenu,
        "emoji" => Action::Emoji,
        "modifier" => Action::Modifier,
        "delete" => Action::Delete,
        "levelSwitch" => Action::LevelSwitch,
        _ => Action::Other(a),
    });
    let keyval = match obj.get("keyval") {
        None | Some(Value::Null) => None,
        Some(v) => Some(parse_keyval(v)?),
    };
    Ok(Key {
        strings,
        label: opt_str(obj, "label")?,
        icon_name: opt_str(obj, "iconName")?,
        keyval,
        action,
        level: opt_str(obj, "level")?,
        width: opt_num(obj, "width", 1.0)?,
        height: opt_num(obj, "height", 1.0)?,
        left_offset: opt_num(obj, "leftOffset", 0.0)?,
    })
}

fn parse_level(v: &Value) -> Result<Level, Error> {
    let Some(obj) = v.as_object() else {
        return shape("level is not an object");
    };
    let Some(level) = opt_str(obj, "level")? else {
        return shape("level without \"level\" name");
    };
    let mode = match opt_str(obj, "mode")?.as_deref() {
        None | Some("default") => LevelMode::Default,
        Some("latched") => LevelMode::Latched,
        Some("locked") => LevelMode::Locked,
        Some(m) => return shape(format!("unknown level mode {m:?}")),
    };
    let Some(rows) = obj.get("rows").and_then(Value::as_array) else {
        return shape(format!("level {level:?} without \"rows\" array"));
    };
    let rows = rows
        .iter()
        .map(|row| match row.as_array() {
            Some(keys) => keys.iter().map(parse_key).collect(),
            None => shape("row is not an array"),
        })
        .collect::<Result<_, _>>()?;
    Ok(Level { level, mode, rows })
}

/// Parse a layout file (`KeyboardModel._loadModel`). Strict about the shape
/// the keyboard walks (`levels`, `rows`, key objects); unknown fields are
/// ignored.
pub fn parse(json: &str) -> Result<Layout, Error> {
    let root: Value = serde_json::from_str(json).map_err(Error::Json)?;
    let Some(obj) = root.as_object() else {
        return shape("layout is not an object");
    };
    let Some(levels) = obj.get("levels").and_then(Value::as_array) else {
        return shape("layout without \"levels\" array");
    };
    let levels: Vec<Level> = levels.iter().map(parse_level).collect::<Result<_, _>>()?;
    if levels.is_empty() {
        return shape("layout without levels");
    }
    Ok(Layout {
        name: opt_str(obj, "name")?.unwrap_or_default(),
        locale: opt_str(obj, "locale")?.unwrap_or_default(),
        levels,
    })
}

fn resource_text(file: &str) -> Option<String> {
    let bytes = gio::resources_lookup_data(
        &format!("{RESOURCE_PREFIX}/{file}"),
        gio::ResourceLookupFlags::NONE,
    )
    .ok()?;
    String::from_utf8(bytes.to_vec()).ok()
}

/// Load the bundled layout `model` (`new KeyboardModel(model)`); `None` when
/// it is missing or fails to parse.
pub fn load(model: &str) -> Option<Layout> {
    parse(&resource_text(&format!("{model}.json"))?).ok()
}

/// The layouts `Keyboard._updateLayout` tries, in order, for a keyboard
/// group and content purpose.
pub fn candidates(group: &str, purpose: Purpose) -> Vec<String> {
    let fixed = match purpose {
        Purpose::Digits => Some("digits"),
        Purpose::Number => Some("number"),
        Purpose::Phone => Some("phone"),
        Purpose::Email => Some("email"),
        Purpose::Url => Some("url"),
        _ => None,
    };
    if let Some(model) = fixed {
        return vec![model.to_owned()];
    }
    let mut groups = vec![group.to_owned()];
    if let Some((base, _)) = group.split_once('+') {
        groups.push(base.to_owned());
    }
    groups.push("us".to_owned());
    if purpose == Purpose::Terminal {
        for g in &mut groups {
            g.push_str("-extended");
        }
    }
    groups
}

/// The first of [`candidates`] that loads.
pub fn resolve(group: &str, purpose: Purpose) -> Option<Layout> {
    candidates(group, purpose).iter().find_map(|m| load(m))
}

/// One key placed on KeyContainer's grid (keyboard.js KeyContainer/_addRowKeys), in key units.
#[derive(Debug, Clone, PartialEq)]
pub struct PlacedKey {
    pub row: usize,
    /// Index into `level.rows[row]`.
    pub index: usize,
    pub left: f64,
    pub top: f64,
    pub width: f64,
    pub height: f64,
}

/// Geometry of one level: emoji keys dropped when `emoji_visible` is false
/// with their width carried to the next key of the row (exactly as
/// `_addRowKeys`); returns the keys plus the level ratio (max columns, rows)
/// as `KeyContainer.getRatio`.
///
/// `top` is the 0-based row index; GNOME's `appendRow` increments before
/// attaching so its grid starts at row 1, which a homogeneous grid ignores.
pub fn place(level: &Level, emoji_visible: bool) -> (Vec<PlacedKey>, (f64, f64)) {
    let mut placed = Vec::new();
    let mut max_cols: f64 = 0.0;
    for (row, keys) in level.rows.iter().enumerate() {
        let mut col = 0.0;
        let mut accumulated = 0.0;
        for (index, key) in keys.iter().enumerate() {
            if key.action == Some(Action::Emoji) && !emoji_visible {
                // GNOME assigns rather than adds here.
                accumulated = key.width;
                continue;
            }
            let mut width = key.width;
            if accumulated > 0.0 {
                width += accumulated;
                accumulated = 0.0;
            }
            // KeyContainer.appendKey
            placed.push(PlacedKey {
                row,
                index,
                left: col + key.left_offset,
                top: row as f64,
                width,
                height: key.height,
            });
            col += key.left_offset + width;
            max_cols = max_cols.max(col);
        }
    }
    (placed, (max_cols, level.rows.len() as f64))
}

#[derive(Debug, Clone, PartialEq)]
pub struct EmojiKey {
    pub label: String,
    pub variants: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EmojiSection {
    /// Name of the section's first emoji.
    pub first: String,
    pub label: String,
    pub keys: Vec<EmojiKey>,
}

/// `EmojiSelection._sections`.
const SECTIONS: [(&str, &str); 9] = [
    ("grinning face", "🙂️"),
    ("selfie", "👍️"),
    ("monkey face", "🌷️"),
    ("grapes", "🍴️"),
    ("globe showing Europe-Africa", "✈️"),
    ("jack-o-lantern", "🏃️"),
    ("muted speaker", "🔔️"),
    ("ATM sign", "❤️"),
    ("chequered flag", "🚩️"),
];

/// The nine emoji sections from the bundled `emoji.json`; empty if missing.
pub fn emoji_sections() -> Vec<EmojiSection> {
    resource_text("emoji.json")
        .and_then(|json| parse_emoji_sections(&json).ok())
        .unwrap_or_default()
}

/// `EmojiSelection._populateSections`, quirks included: an emoji whose name
/// starts with the current key's name joins its variants; only the very
/// first key gets its own char among its variants (later keys start with
/// an empty list, so a key without such followers has no variants); the
/// last emoji is dropped unless it is a variant; labels get U+FE0F
/// appended.
pub fn parse_emoji_sections(json: &str) -> Result<Vec<EmojiSection>, Error> {
    let root: Value = serde_json::from_str(json).map_err(Error::Json)?;
    let Some(list) = root.as_array() else {
        return shape("emoji list is not an array");
    };
    let emoji = list
        .iter()
        .map(|e| {
            let obj = e.as_object();
            let field = |f: &str| obj.and_then(|o| o.get(f)).and_then(Value::as_str);
            match (field("name"), field("char")) {
                (Some(name), Some(ch)) => Ok((name, ch)),
                _ => shape("emoji without \"name\"/\"char\""),
            }
        })
        .collect::<Result<Vec<_>, _>>()?;

    let mut sections: Vec<EmojiSection> = SECTIONS
        .iter()
        .map(|(first, label)| EmojiSection {
            first: (*first).to_owned(),
            label: (*label).to_owned(),
            keys: Vec::new(),
        })
        .collect();
    let mut variants = Vec::new();
    let mut current_key = 0;
    let mut current_section: Option<usize> = None;
    for (i, &(name, ch)) in emoji.iter().enumerate() {
        if name.starts_with(emoji[current_key].0) {
            variants.push(ch.to_owned());
            if i < emoji.len() - 1 {
                continue;
            }
        }
        if let Some(s) = sections
            .iter()
            .position(|s| s.first == emoji[current_key].0)
        {
            current_section = Some(s);
            sections[s].keys.clear();
        }
        let label = format!("{}\u{FE0F}", emoji[current_key].1);
        let variants = std::mem::take(&mut variants);
        // GNOME would throw before the first section; skip instead.
        if let Some(s) = current_section {
            sections[s].keys.push(EmojiKey { label, variants });
        }
        current_key = i;
    }
    Ok(sections)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::Once;

    /// Register the bundled layouts once for tests that load them.
    pub(crate) fn register() {
        static ONCE: Once = Once::new();
        ONCE.call_once(|| {
            gio::resources_register_include!("osk-layouts.gresource")
                .expect("register osk-layouts.gresource");
        });
    }

    #[test]
    fn every_vendored_layout_parses_except_ge() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("osk-layouts");
        let mut count = 0;
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if !name.ends_with(".json") || name == "emoji.json" {
                continue;
            }
            count += 1;
            let text = std::fs::read_to_string(&path).unwrap();
            if name == "ge.json" {
                assert!(parse(&text).is_err(), "ge.json is malformed upstream");
                continue;
            }
            let layout = parse(&text).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(
                layout.level("default").is_some(),
                "{name} has no default level"
            );
        }
        assert_eq!(count, 67);
    }

    #[test]
    fn ge_falls_back_to_us() {
        register();
        assert!(load("ge").is_none());
        let layout = resolve("ge", Purpose::Normal).unwrap();
        assert_eq!(
            layout,
            parse(include_str!("../osk-layouts/us.json")).unwrap()
        );
    }

    #[test]
    fn candidate_chains() {
        assert_eq!(
            candidates("de+nodeadkeys", Purpose::Normal),
            ["de+nodeadkeys", "de", "us"]
        );
        assert_eq!(candidates("fr", Purpose::Password), ["fr", "us"]);
        assert_eq!(
            candidates("de+nodeadkeys", Purpose::Terminal),
            ["de+nodeadkeys-extended", "de-extended", "us-extended"]
        );
        assert_eq!(candidates("fr", Purpose::Digits), ["digits"]);
        assert_eq!(candidates("fr", Purpose::Number), ["number"]);
        assert_eq!(candidates("fr", Purpose::Phone), ["phone"]);
        assert_eq!(candidates("fr", Purpose::Email), ["email"]);
        assert_eq!(candidates("fr", Purpose::Url), ["url"]);
    }

    #[test]
    fn resolve_loads_from_resource() {
        register();
        assert_eq!(
            resolve("de+nodeadkeys", Purpose::Normal).unwrap().locale,
            "de"
        );
        assert!(resolve("xx", Purpose::Terminal).is_some());
        assert!(resolve("fr", Purpose::Digits).is_some());
        assert_eq!(emoji_sections().len(), 9);
    }

    #[test]
    fn purpose_from_wire() {
        assert_eq!(Purpose::from_wire(9), Purpose::Normal);
        assert_eq!(Purpose::from_wire(13), Purpose::Terminal);
        assert_eq!(Purpose::from_wire(99), Purpose::Normal);
    }

    fn us() -> Layout {
        parse(include_str!("../osk-layouts/us.json")).unwrap()
    }

    #[test]
    fn us_default_first_row() {
        let layout = us();
        let row = &layout.level("default").unwrap().rows[0];
        let letters: Vec<_> = row[..10]
            .iter()
            .map(|k| k.commit_string().unwrap())
            .collect();
        assert_eq!(letters, ["q", "w", "e", "r", "t", "y", "u", "i", "o", "p"]);
        assert_eq!(row[10].action, Some(Action::Delete));
        assert_eq!(row[10].width, 1.5);
        assert_eq!(row[2].extended()[0], "è");
        assert!(row[0].extended().is_empty());
    }

    #[test]
    fn keyval_parses_hex() {
        let layout = us();
        let enter = layout.level("default").unwrap().rows[1]
            .last()
            .unwrap()
            .clone();
        assert_eq!(enter.keyval, Some(0xff0d));
        assert_eq!(enter.width, 2.0);
    }

    #[test]
    fn shape_errors() {
        assert!(parse("{}").is_err());
        assert!(parse(r#"{"levels":[{"level":"default"}]}"#).is_err());
        assert!(parse(r#"{"levels":[{"level":"default","rows":[[1]]}]}"#).is_err());
        assert!(parse(r#"{"levels":[{"level":"default","rows":[[{"x":1}]]}],"y":2}"#).is_ok());
    }

    #[test]
    fn placement_carries_emoji_width() {
        let layout = us();
        let level = layout.level("default").unwrap();
        let (shown, ratio) = place(level, true);
        let row3: Vec<_> = shown.iter().filter(|k| k.row == 3).collect();
        assert_eq!(row3.len(), 5);
        assert_eq!(row3[2].width, 5.0);
        assert_eq!(row3[2].left, 3.0);
        assert_eq!(ratio, (11.5, 4.0));

        let (hidden, ratio) = place(level, false);
        let row3: Vec<_> = hidden.iter().filter(|k| k.row == 3).collect();
        assert_eq!(row3.len(), 4);
        assert!(row3
            .iter()
            .all(|k| level.rows[3][k.index].action != Some(Action::Emoji)));
        assert_eq!(row3[1].index, 2);
        assert_eq!(row3[1].left, 1.5);
        assert_eq!(row3[1].width, 6.5);
        assert_eq!(row3[1].top, 3.0);
        assert_eq!(ratio, (11.5, 4.0));
    }

    #[test]
    fn emoji_grouping() {
        let sections = parse_emoji_sections(include_str!("../osk-layouts/emoji.json")).unwrap();
        assert_eq!(sections.len(), 9);
        assert_eq!(sections[0].label, "🙂️");
        assert!(sections.iter().all(|s| !s.keys.is_empty()));
        assert_eq!(sections[0].keys[0].label, "😀\u{FE0F}");
        assert_eq!(sections[0].keys[0].variants, ["😀"]);
        assert!(sections
            .iter()
            .flat_map(|s| &s.keys)
            .any(|k| k.variants.len() > 1));
    }
}
