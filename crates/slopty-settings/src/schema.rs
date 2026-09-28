//! Every key of the file as a form draws it, read off the types' own schema.
//!
//! [`Settings`] and its tables derive [`JsonSchema`](schemars::JsonSchema), so a key's type, its
//! range, its choices and its words live on the field that holds it: a `title` names it, the
//! doc comment's first paragraph says what it does in a line, `range` bounds a number and
//! `x-step` and `x-unit` say how a stepper moves it and what it counts. [`fields`] walks that
//! schema once and takes each key's default from [`Settings::default`], so a key added to a
//! table is a row of the settings form with no second edit anywhere.

use std::sync::LazyLock;

use serde_json::Value as Json;

use crate::Settings;
use crate::edit::Value;

/// The `format` of a colour string (`"#rrggbb"`, or `""` for the theme's own).
pub const COLOR: &str = "color";

/// The `format` of a font family's name.
pub const FONT_FAMILY: &str = "font-family";

/// One key of the file.
#[derive(Clone, PartialEq, Debug)]
pub struct Field {
    /// Its table (`font`).
    pub table: String,
    /// What the table is called (`Font`).
    pub table_title: String,
    /// Its key in the table (`mono_size`).
    pub key: String,
    /// What it is called (`Size`).
    pub title: String,
    /// What it does, in a phrase: its doc comment's first paragraph, without the full stop.
    pub summary: String,
    /// What it holds.
    pub kind: Kind,
    /// Its value when the file does not set it.
    pub default: Value,
    /// A value it could take, as a field shows it (a list's items joined by `, `).
    pub example: Option<String>,
}

/// What a key holds, and so how it is set.
#[derive(Clone, PartialEq, Debug)]
pub enum Kind {
    /// A boolean.
    Switch,
    /// One of a few strings.
    Choice(Vec<Choice>),
    /// A number within bounds.
    Number(Number),
    /// A string typed in.
    Text,
    /// A list of strings.
    List,
    /// A colour, or empty for the theme's own.
    Colour,
    /// A font family.
    Font,
}

/// One option of a [`Kind::Choice`].
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Choice {
    /// As the file spells it.
    pub value: String,
    /// What it is called.
    pub title: String,
}

/// A [`Kind::Number`]'s bounds and grid.
#[derive(Clone, PartialEq, Debug)]
pub struct Number {
    /// The least value the app takes.
    pub min: f64,
    /// The greatest value the app takes.
    pub max: f64,
    /// One step of a stepper.
    pub step: f64,
    /// An integer key (`60`); otherwise a float one (`13.0`).
    pub integer: bool,
    /// What it counts (`pt`), or empty.
    pub unit: String,
}

impl Field {
    /// Whether `literal`, a TOML value as written, is a value this key takes.
    ///
    /// # Errors
    ///
    /// Why not, as the parser says it: a colour that is not one, a host with a bad port.
    pub fn check(&self, literal: &str) -> Result<(), String> {
        let text = format!("[{}]\n{} = {literal}\n", self.table, self.key);
        toml::from_str::<Settings>(&text).map(drop).map_err(|e| e.message().trim().to_owned())
    }
}

/// Every key of the file, table by table.
#[must_use]
pub fn fields() -> &'static [Field] {
    static FIELDS: LazyLock<Vec<Field>> = LazyLock::new(|| {
        let schema = schemars::generate::SchemaSettings::draft2020_12()
            .with(|s| s.inline_subschemas = true)
            .into_generator()
            .into_root_schema_for::<Settings>();
        let defaults = toml::Table::try_from(Settings::default()).unwrap_or_default();
        fields_of(schema.as_value(), &defaults)
    });
    &FIELDS
}

/// The keys of `schema` (a whole file's, subschemas inline) with a kind a form can set, each
/// with its value in `defaults`.
fn fields_of(schema: &Json, defaults: &toml::Table) -> Vec<Field> {
    let mut out = Vec::new();
    for (table, table_schema) in properties(schema) {
        let table_title = title(table_schema, table);
        for (key, field) in properties(table_schema) {
            let Some(kind) = kind(field) else { continue };
            let default = defaults
                .get(table)
                .and_then(|t| t.get(key))
                .map_or(Value::Other(String::new()), from_toml);
            out.push(Field {
                table: table.clone(),
                table_title: table_title.clone(),
                key: key.clone(),
                title: title(field, key),
                summary: summary(field),
                kind,
                default,
                example: field.get("examples").and_then(|e| e.get(0)).map(example),
            });
        }
    }
    out
}

fn properties(schema: &Json) -> impl Iterator<Item = (&String, &Json)> {
    schema.get("properties").and_then(Json::as_object).into_iter().flatten()
}

/// The schema's `title`, else `key` as words (`scroll_multiplier` is "Scroll multiplier").
fn title(schema: &Json, key: &str) -> String {
    if let Some(title) = schema.get("title").and_then(Json::as_str) {
        return title.to_owned();
    }
    let words = key.replace('_', " ");
    let mut chars = words.chars();
    chars.next().map(|first| first.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

/// The description's first paragraph on one line, in plain words, without its full stop.
fn summary(schema: &Json) -> String {
    let doc = schema.get("description").and_then(Json::as_str).unwrap_or_default();
    let first = doc.split("\n\n").next().unwrap_or_default();
    let line = first.split_whitespace().collect::<Vec<_>>().join(" ");
    // Plain words: a doc comment's `code` marks are Markdown, not what a row says.
    let line = line.replace('`', "");
    line.strip_suffix('.').map_or_else(|| line.clone(), str::to_owned)
}

fn kind(schema: &Json) -> Option<Kind> {
    if let Some(choices) = choices(schema) {
        return Some(Kind::Choice(choices));
    }
    let number = |key: &str| schema.get(key).and_then(Json::as_f64);
    match schema.get("type").and_then(Json::as_str)? {
        "boolean" => Some(Kind::Switch),
        kind @ ("integer" | "number") => {
            let integer = kind == "integer";
            Some(Kind::Number(Number {
                min: number("minimum").unwrap_or(f64::MIN),
                max: number("maximum").unwrap_or(f64::MAX),
                step: number("x-step").unwrap_or(if integer { 1.0 } else { 0.1 }),
                integer,
                unit: schema.get("x-unit").and_then(Json::as_str).unwrap_or_default().to_owned(),
            }))
        }
        "string" => Some(match schema.get("format").and_then(Json::as_str) {
            Some(COLOR) => Kind::Colour,
            Some(FONT_FAMILY) => Kind::Font,
            _ => Kind::Text,
        }),
        "array" => Some(Kind::List),
        _ => None,
    }
}

/// An enum's options: `oneOf` a `const` each (variants with docs), or a bare `enum`.
fn choices(schema: &Json) -> Option<Vec<Choice>> {
    if let Some(variants) = schema.get("oneOf").and_then(Json::as_array) {
        return variants
            .iter()
            .map(|v| {
                let value = v.get("const")?.as_str()?.to_owned();
                Some(Choice { title: title(v, &value), value })
            })
            .collect();
    }
    let values = schema.get("enum")?.as_array()?;
    values
        .iter()
        .map(|v| {
            v.as_str()
                .map(|value| Choice { title: title(&Json::Null, value), value: value.to_owned() })
        })
        .collect()
}

fn from_toml(value: &toml::Value) -> Value {
    match value {
        toml::Value::Boolean(on) => Value::Bool(*on),
        #[expect(clippy::cast_precision_loss, reason = "a setting's number, far under 2^52")]
        toml::Value::Integer(n) => Value::Number(*n as f64),
        toml::Value::Float(n) => Value::Number(*n),
        toml::Value::String(s) => Value::Str(s.clone()),
        toml::Value::Array(items) => items
            .iter()
            .map(|i| i.as_str().map(str::to_owned))
            .collect::<Option<Vec<_>>>()
            .map_or_else(|| Value::Other(value.to_string()), Value::List),
        toml::Value::Datetime(_) | toml::Value::Table(_) => Value::Other(value.to_string()),
    }
}

fn example(value: &Json) -> String {
    match value {
        Json::String(s) => s.clone(),
        Json::Array(items) => items
            .iter()
            .map(|i| i.as_str().map_or_else(|| i.to_string(), str::to_owned))
            .collect::<Vec<_>>()
            .join(", "),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    fn field(table: &str, key: &str) -> &'static Field {
        fields()
            .iter()
            .find(|f| f.table == table && f.key == key)
            .unwrap_or_else(|| panic!("{table}.{key}"))
    }

    /// Every key of the default file is a field, with the default the file writes, and every
    /// field names a key of it.
    #[test]
    fn every_key_is_a_field_with_its_default() {
        let defaults = toml::Table::try_from(Settings::default()).unwrap();
        let mut keys = Vec::new();
        for (table, inner) in &defaults {
            for key in inner.as_table().unwrap().keys() {
                keys.push(format!("{table}.{key}"));
            }
        }
        let mut named: Vec<String> =
            fields().iter().map(|f| format!("{}.{}", f.table, f.key)).collect();
        keys.sort();
        named.sort();
        assert_eq!(named, keys, "a field per key, and no other");
        for f in fields() {
            assert!(!matches!(f.default, Value::Other(_)), "{f:?}");
            assert!(!f.title.is_empty() && !f.summary.is_empty(), "{f:?}");
            assert!(!f.summary.ends_with('.'), "a phrase: {f:?}");
        }
    }

    /// A field says what it is from the attributes on the type: its words, its bounds, its
    /// grid and unit, its choices by their titles, and a colour or a font by its format.
    #[test]
    fn a_field_reads_its_type() {
        let size = field("font", "mono_size");
        assert_eq!((size.table_title.as_str(), size.title.as_str()), ("Font", "Size"));
        assert_eq!(size.summary, "Zooming a tile changes it for that tile only");
        assert_eq!(size.default, Value::Number(13.0));
        assert_eq!(
            size.kind,
            Kind::Number(Number {
                min: 6.0,
                max: 72.0,
                step: 1.0,
                integer: false,
                unit: "pt".to_owned()
            })
        );
        assert!(matches!(&field("remote", "fps").kind, Kind::Number(n) if n.integer));
        assert_eq!(field("remote", "fps").default, Value::Number(60.0));
        let Kind::Choice(options) = &field("terminal", "option_as_alt").kind else {
            panic!("a choice")
        };
        let options: Vec<_> =
            options.iter().map(|c| (c.value.as_str(), c.title.as_str())).collect();
        assert_eq!(
            options,
            [("false", "Off"), ("true", "Both"), ("left", "Left"), ("right", "Right")]
        );
        assert_eq!(field("theme", "appearance").default, Value::Str("system".to_owned()));
        assert_eq!(field("font", "ligatures").kind, Kind::Switch);
        assert_eq!(field("colors", "cursor").kind, Kind::Colour);
        assert_eq!(field("colors", "ansi").kind, Kind::List);
        assert_eq!(field("font", "mono_family").kind, Kind::Font);
        assert_eq!(field("font", "mono_family").default, Value::Str("JetBrains Mono".to_owned()));
        let server = field("client", "server");
        assert_eq!((&server.kind, &server.default), (&Kind::Text, &Value::Str(String::new())));
        assert_eq!(field("worker", "allow").example.as_deref(), Some("100.64.0.0/10, fd00::/8"));
    }

    /// A value is checked against its key's type alone, and a refusal is the parser's reason.
    #[test]
    fn a_value_is_checked_by_its_key() {
        field("colors", "cursor").check("\"#01abff\"").unwrap();
        field("colors", "cursor").check("\"\"").unwrap();
        let bad = field("colors", "cursor").check("\"#12\"").unwrap_err();
        assert!(bad.contains("#rrggbb") && !bad.contains('\n'), "{bad}");
        field("client", "server").check("\"studio:45560\"").unwrap();
        assert!(field("client", "server").check("\"studio:x\"").is_err());
        assert!(field("font", "mono_size").check("\"big\"").is_err());
    }

    /// A key without a title is called by its words, and one without a range still steps. (Their
    /// order is the schema map's, which `serde_json`'s `preserve_order` decides.)
    #[test]
    fn a_bare_key_still_reads() {
        let schema = serde_json::json!({ "properties": { "extra": { "properties": {
            "scroll_multiplier": { "type": "number", "description": "Lines.\n\nMore." },
            "mode": { "type": "string", "enum": ["one", "two"] },
            "nested": { "type": "object" },
        } } } });
        let defaults: toml::Table = toml::from_str("[extra]\nscroll_multiplier = 1.0\n").unwrap();
        let fields = fields_of(&schema, &defaults);
        let by_key = |key: &str| fields.iter().find(|f| f.key == key).expect("a field");
        let (scroll, mode) = (by_key("scroll_multiplier"), by_key("mode"));
        assert_eq!(fields.len(), 2, "no row for a nested table: {fields:?}");
        assert_eq!(
            (scroll.title.as_str(), scroll.summary.as_str()),
            ("Scroll multiplier", "Lines")
        );
        assert_eq!(scroll.table_title, "Extra");
        assert!(
            matches!(&scroll.kind, Kind::Number(n) if n.min.total_cmp(&f64::MIN).is_eq() && !n.integer)
        );
        let Kind::Choice(options) = &mode.kind else { panic!("{mode:?}") };
        assert_eq!(options[1], Choice { value: "two".to_owned(), title: "Two".to_owned() });
    }
}
