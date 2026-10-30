//! JSON as a tree, for a step's output read as JSON and for its raw record
//! (`inspector.md`, "Any step opens to its whole detail").
//!
//! A document is parsed once into its values in reading order, each container followed by its
//! subtree, so a folded one is skipped in one step. Its lines are laid out per frame from that
//! and the folds the person changed: a container shows its count open and a preview of its first
//! fields folded, as a browser's console does, one fixed-height line each so the step's height
//! stays analytic.

use std::collections::HashSet;
use std::fmt::Write as _;

/// Most values a document keeps; past them the rest is left out, said by its last line.
const VALUES: usize = 20_000;

/// A document small enough to read open whole: every container starts open.
const SMALL: usize = 64;

/// Containers deeper than this start folded in a larger document: its top level and the
/// fields of each of its values show.
const OPEN_DEPTH: u16 = 2;

/// Characters a folded container's preview takes.
const PREVIEW_CHARS: usize = 72;

/// Characters of a string a preview shows.
const PREVIEW_TEXT_CHARS: usize = 24;

/// Characters of one line; a longer string is cut with an ellipsis.
const LINE_CHARS: usize = 8 * 1024;

/// A parsed document: its values in reading order.
#[derive(Debug, PartialEq)]
pub struct Doc {
    values: Vec<Value>,
    /// Values past [`VALUES`], left out.
    left_out: usize,
}

/// One value, its subtree after it up to `end`.
#[derive(Debug, PartialEq)]
struct Value {
    /// Its key in its object, or its index in its array; `None` for a root.
    key: Option<Key>,
    kind: Kind,
    depth: u16,
    /// The index after its subtree.
    end: usize,
}

#[derive(Debug, PartialEq)]
enum Key {
    Field(Box<str>),
    Index(usize),
}

#[derive(Debug, PartialEq)]
enum Kind {
    Object(usize),
    Array(usize),
    Text(Box<str>),
    Number(Box<str>),
    Bool(bool),
    Null,
}

/// How a run of a line reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tone {
    Key,
    Punctuation,
    Text,
    Number,
    Literal,
    /// A folded container's preview and an open one's count.
    Quiet,
}

/// One line of a document as laid out.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Line {
    /// The value it shows, which a click on a container's line folds or opens.
    pub value: usize,
    pub depth: u16,
    /// `Some(open)` for a container.
    pub fold: Option<bool>,
    pub text: String,
    /// The runs of `text`, by bytes.
    pub runs: Vec<(usize, Tone)>,
}

impl Doc {
    /// `text` as JSON: one object or array, or one on each of its lines (JSON Lines). `None` for
    /// anything else, a lone string or number too: there is no tree to show.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let trimmed = text.trim();
        if !(trimmed.starts_with('{') || trimmed.starts_with('[')) {
            return None;
        }
        let roots = match serde_json::from_str::<serde_json::Value>(trimmed) {
            Ok(value) => vec![value],
            Err(_) => trimmed
                .lines()
                .filter(|line| !line.trim().is_empty())
                .map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
                .collect::<Option<Vec<_>>>()?,
        };
        if roots.iter().any(|root| !(root.is_object() || root.is_array())) {
            return None;
        }
        let mut doc = Self { values: Vec::new(), left_out: 0 };
        for root in &roots {
            doc.push(None, root, 0);
        }
        Some(doc)
    }

    fn push(&mut self, key: Option<Key>, value: &serde_json::Value, depth: u16) {
        if self.values.len() >= VALUES {
            self.left_out = self.left_out.saturating_add(1);
            return;
        }
        let at = self.values.len();
        let kind = match value {
            serde_json::Value::Object(map) => Kind::Object(map.len()),
            serde_json::Value::Array(items) => Kind::Array(items.len()),
            serde_json::Value::String(text) => Kind::Text(quoted(text, LINE_CHARS).into()),
            serde_json::Value::Number(n) => Kind::Number(n.to_string().into()),
            serde_json::Value::Bool(b) => Kind::Bool(*b),
            serde_json::Value::Null => Kind::Null,
        };
        self.values.push(Value { key, kind, depth, end: at.saturating_add(1) });
        let below = depth.saturating_add(1);
        match value {
            serde_json::Value::Object(map) => {
                for (field, child) in map {
                    self.push(Some(Key::Field(field.as_str().into())), child, below);
                }
            }
            serde_json::Value::Array(items) => {
                for (ix, child) in items.iter().enumerate() {
                    self.push(Some(Key::Index(ix)), child, below);
                }
            }
            _ => {}
        }
        let end = self.values.len();
        if let Some(value) = self.values.get_mut(at) {
            value.end = end;
        }
    }

    /// Whether value `at` is open: as it starts (open in a small document, else to
    /// [`OPEN_DEPTH`]), unless the person turned it (`turned`).
    fn open(&self, at: usize, depth: u16, turned: &HashSet<usize>) -> bool {
        let starts_open = self.values.len() <= SMALL || depth < OPEN_DEPTH;
        starts_open != turned.contains(&at)
    }

    /// Its lines with the folds `turned` changed, at most `max`, and how many more there are.
    #[must_use]
    pub fn lines(&self, turned: &HashSet<usize>, max: usize) -> (Vec<Line>, usize) {
        let mut lines = Vec::new();
        let mut more = 0_usize;
        let mut at = 0;
        while let Some(value) = self.values.get(at) {
            let container = matches!(value.kind, Kind::Object(_) | Kind::Array(_));
            let open = container && self.open(at, value.depth, turned);
            if lines.len() < max {
                lines.push(self.line(at, value, container.then_some(open)));
            } else {
                more = more.saturating_add(1);
            }
            at = if container && !open { value.end } else { at.saturating_add(1) };
        }
        if self.left_out > 0 {
            let words = format!("… {} more values left out", self.left_out);
            let runs = vec![(words.len(), Tone::Quiet)];
            if lines.len() < max {
                lines.push(Line { value: usize::MAX, depth: 0, fold: None, text: words, runs });
            } else {
                more = more.saturating_add(1);
            }
        }
        (lines, more)
    }

    fn line(&self, at: usize, value: &Value, fold: Option<bool>) -> Line {
        let mut text = String::new();
        let mut runs = Vec::new();
        let mut run = |text: &mut String, part: &str, tone: Tone| {
            if !part.is_empty() {
                text.push_str(part);
                runs.push((part.len(), tone));
            }
        };
        match &value.key {
            Some(Key::Field(field)) => {
                run(&mut text, field, Tone::Key);
                run(&mut text, ": ", Tone::Punctuation);
            }
            Some(Key::Index(ix)) => {
                run(&mut text, &ix.to_string(), Tone::Quiet);
                run(&mut text, ": ", Tone::Punctuation);
            }
            None => {}
        }
        match (&value.kind, fold) {
            (Kind::Object(n), Some(true)) => run(&mut text, &format!("{{{n}}}"), Tone::Quiet),
            (Kind::Array(n), Some(true)) => run(&mut text, &format!("[{n}]"), Tone::Quiet),
            (Kind::Object(_) | Kind::Array(_), _) => {
                run(&mut text, &self.preview(at, PREVIEW_CHARS), Tone::Quiet);
            }
            (kind, _) => {
                let (words, tone) = scalar(kind);
                run(&mut text, &words, tone);
            }
        }
        Line { value: at, depth: value.depth, fold, text, runs }
    }

    /// A folded container `at` in a line of about `room` characters: its first fields or items,
    /// `{type: "assistant", uuid: "a1", …}`, `(3) [1, 2, 3]`.
    fn preview(&self, at: usize, room: usize) -> String {
        let Some(value) = self.values.get(at) else { return String::new() };
        let (open, close, object) = match value.kind {
            Kind::Object(_) => ("{", "}", true),
            Kind::Array(_) => ("[", "]", false),
            _ => return String::new(),
        };
        let mut out = String::new();
        if let Kind::Array(n) = value.kind {
            let _written = write!(out, "({n}) ");
        }
        out.push_str(open);
        let mut child = at.saturating_add(1);
        let mut first = true;
        while child < value.end {
            let Some(item) = self.values.get(child) else { break };
            let mut part = String::new();
            if object && let Some(Key::Field(field)) = &item.key {
                let _written = write!(part, "{field}: ");
            }
            part.push_str(&match &item.kind {
                Kind::Object(_) => "{…}".to_owned(),
                Kind::Array(_) => "[…]".to_owned(),
                Kind::Text(text) => short(text),
                kind => scalar(kind).0,
            });
            let sep = if first { "" } else { ", " };
            if out.chars().count().saturating_add(sep.len()).saturating_add(part.chars().count()) > room {
                out.push_str(if first { "…" } else { ", …" });
                break;
            }
            out.push_str(sep);
            out.push_str(&part);
            first = false;
            child = item.end;
        }
        out.push_str(close);
        out
    }

    /// The whole document's values, for a test's size.
    #[cfg(test)]
    fn len(&self) -> usize {
        self.values.len()
    }
}

/// A scalar as JSON writes it, and how it reads.
fn scalar(kind: &Kind) -> (String, Tone) {
    match kind {
        Kind::Text(text) => (text.to_string(), Tone::Text),
        Kind::Number(n) => (n.to_string(), Tone::Number),
        Kind::Bool(b) => (b.to_string(), Tone::Literal),
        Kind::Null => ("null".to_owned(), Tone::Literal),
        Kind::Object(_) | Kind::Array(_) => (String::new(), Tone::Quiet),
    }
}

/// `text` as a JSON string, cut past `max` characters.
fn quoted(text: &str, max: usize) -> String {
    let encoded = serde_json::to_string(text).unwrap_or_default();
    match encoded.char_indices().nth(max) {
        Some((cut, _)) => format!("{}…\"", encoded.get(..cut).unwrap_or_default()),
        None => encoded,
    }
}

/// A quoted string, shortened for a preview.
fn short(quoted: &str) -> String {
    let inner = quoted.strip_prefix('"').and_then(|q| q.strip_suffix('"')).unwrap_or(quoted);
    match inner.char_indices().nth(PREVIEW_TEXT_CHARS) {
        Some((cut, _)) => format!("\"{}…\"", inner.get(..cut).unwrap_or_default()),
        None => format!("\"{inner}\""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(doc: &Doc, turned: &HashSet<usize>) -> Vec<String> {
        doc.lines(turned, usize::MAX).0.into_iter().map(|line| line.text).collect()
    }

    /// A small document opens whole, each value on its line and each container with its count;
    /// a fold hides its subtree and shows a preview of it instead.
    #[test]
    fn a_small_document_opens_whole_and_folds_to_a_preview() {
        let doc = Doc::parse(r#"{"type": "user", "n": 3, "ok": true, "items": [1, "two", null]}"#)
            .expect("JSON");
        assert_eq!(
            texts(&doc, &HashSet::new()),
            ["{4}", "type: \"user\"", "n: 3", "ok: true", "items: [3]", "0: 1", "1: \"two\"", "2: null"]
        );
        let items = doc.lines(&HashSet::new(), usize::MAX).0[4].value;
        assert_eq!(texts(&doc, &HashSet::from([items]))[4], "items: (3) [1, \"two\", null]");
        assert_eq!(texts(&doc, &HashSet::from([0])), ["{type: \"user\", n: 3, ok: true, items: […]}"]);
    }

    /// A larger document opens its top level and its values' fields; deeper containers start
    /// folded, and the lines past the cap are counted.
    #[test]
    fn a_large_document_starts_folded_past_two_levels() {
        let records: Vec<serde_json::Value> = (0..40)
            .map(|n| serde_json::json!({"uuid": format!("u{n}"), "message": {"content": [n]}}))
            .collect();
        let doc = Doc::parse(&serde_json::to_string(&records).expect("json")).expect("JSON");
        assert!(doc.len() > SMALL);
        let lines = texts(&doc, &HashSet::new());
        assert_eq!(lines[..4], ["[40]", "0: {2}", "uuid: \"u0\"", "message: {content: […]}"]);
        let (shown, more) = doc.lines(&HashSet::new(), 10);
        assert_eq!((shown.len(), more), (10, lines.len() - 10));
    }

    /// JSON Lines read as one root each; text that is not JSON, or a lone scalar, has no tree.
    #[test]
    fn json_lines_have_a_root_each_and_other_text_none() {
        let doc = Doc::parse("{\"a\": 1}\n\n{\"b\": [2]}\n").expect("JSON Lines");
        assert_eq!(texts(&doc, &HashSet::new()), ["{1}", "a: 1", "{1}", "b: [1]", "0: 2"]);
        assert_eq!(Doc::parse("cargo test\nok"), None);
        assert_eq!(Doc::parse("{\"a\": 1}\nnot json"), None);
        assert_eq!(Doc::parse("42"), None);
        let long = "x".repeat(LINE_CHARS * 2);
        let doc = Doc::parse(&serde_json::json!({ "file": long }).to_string()).expect("JSON");
        let line = &texts(&doc, &HashSet::new())[1];
        assert!(line.chars().count() < LINE_CHARS + 16 && line.ends_with("…\""), "cut: {}", line.len());
    }
}
