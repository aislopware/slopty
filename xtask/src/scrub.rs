//! What a recorded fixture must not keep, for every recorder: the scratch paths, this
//! machine's name, ids and times.

use std::collections::HashMap;
use std::process::Command;

use anyhow::{Context as _, Result, ensure};
use serde_json::{Value, json};

/// What a fixture must not keep: scratch paths, ids and times.
pub struct Scrub {
    /// Each path or name, and what stands in for it.
    paths: Vec<(String, &'static str)>,
    /// Dated runs to zero, `9` standing for a digit.
    dated: &'static [&'static str],
    ids: HashMap<String, String>,
}

impl Scrub {
    /// A scrub of `paths` (each with its stand-in, longest first where one holds another) and
    /// of every run of text that fits one of `dated`.
    pub fn new(paths: Vec<(String, &'static str)>, dated: &'static [&'static str]) -> Self {
        Self { paths, dated, ids: HashMap::new() }
    }

    /// `value` with every string scrubbed, every time and duration zero.
    pub fn value(&mut self, value: Value) -> Value {
        match value {
            Value::String(text) => Value::String(self.text(&text)),
            Value::Array(items) => Value::Array(items.into_iter().map(|v| self.value(v)).collect()),
            Value::Object(map) => Value::Object(
                map.into_iter()
                    .map(|(key, value)| {
                        let value = if is_time(&key) && value.is_number() {
                            json!(0)
                        } else if key == "userAgent" {
                            // It names this Mac's macOS build.
                            json!("user-agent")
                        } else {
                            self.value(value)
                        };
                        (key, value)
                    })
                    .collect(),
            ),
            other => other,
        }
    }

    /// `text` with the paths replaced, the dates zeroed and each UUID a stable placeholder
    /// numbered in order of appearance.
    pub fn text(&mut self, text: &str) -> String {
        let mut out = text.to_owned();
        for (path, stand_in) in &self.paths {
            out = out.replace(path.as_str(), stand_in);
        }
        for template in self.dated {
            out = undate(&out, template);
        }
        let mut scrubbed = String::with_capacity(out.len());
        let mut rest = out.as_str();
        while let Some(at) = uuid_at(rest) {
            let (before, found) = rest.split_at(at);
            let (uuid, after) = found.split_at(36);
            scrubbed.push_str(before);
            let next = self.ids.len().saturating_add(1);
            let stand_in = self
                .ids
                .entry(uuid.to_owned())
                .or_insert_with(|| format!("00000000-0000-7000-8000-{next:012}"));
            scrubbed.push_str(stand_in);
            rest = after;
        }
        scrubbed.push_str(rest);
        scrubbed
    }
}

/// This machine's name, as an agent may say it.
pub fn host() -> Result<String> {
    let out = Command::new("hostname").output().context("run hostname")?;
    let name = String::from_utf8(out.stdout)?.trim().to_owned();
    ensure!(!name.is_empty(), "this machine has no name");
    Ok(name)
}

/// `text` with every run that fits `template` written with zeros for its digits.
fn undate(text: &str, template: &str) -> String {
    let fits = |window: &[u8]| {
        window.iter().zip(template.as_bytes()).all(|(b, t)| match t {
            b'9' => b.is_ascii_digit(),
            t => b == t,
        })
    };
    let zeros = template.replace('9', "0");
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.as_bytes().windows(template.len()).position(fits) {
        let (before, found) = rest.split_at(at);
        out.push_str(before);
        out.push_str(&zeros);
        rest = found.get(template.len()..).unwrap_or_default();
    }
    out.push_str(rest);
    out
}

/// Whether a field named `key` holds a time or a duration.
fn is_time(key: &str) -> bool {
    key.ends_with("At")
        || key.ends_with("_at")
        || key.ends_with("Ms")
        || key.ends_with("_ms")
        || key == "timestamp"
}

/// Where the first UUID in `text` starts.
fn uuid_at(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    (0..bytes.len().saturating_sub(35)).find(|&at| {
        bytes.get(at..at.saturating_add(36)).is_some_and(|window| {
            window.iter().enumerate().all(|(i, b)| match i {
                8 | 13 | 18 | 23 => *b == b'-',
                _ => b.is_ascii_hexdigit(),
            })
        })
    })
}
