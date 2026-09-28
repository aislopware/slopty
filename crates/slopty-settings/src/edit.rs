//! `settings.toml` edited a key at a time, keeping the rest of the text as written.
//!
//! A key's value is swapped in place with the line's comment kept, else the key joins the end
//! of its table, else the table joins the end of the file, so an edit never reorders or drops
//! a line, a comment or a key this crate does not know. The whole text is parsed by
//! [`Settings::parse`](crate::Settings::parse) before it is written, so a line this cannot place
//! still surfaces there.

/// A value as the file holds it.
#[derive(Clone, PartialEq, Debug)]
pub enum Value {
    /// `true` or `false`.
    Bool(bool),
    /// An integer or a float.
    Number(f64),
    /// A string, unescaped.
    Str(String),
    /// An array of strings.
    List(Vec<String>),
    /// Anything else, as written: an inline table, a date, a mixed array.
    Other(String),
}

/// One key's lines.
#[derive(Clone, PartialEq, Debug)]
struct Entry {
    /// The table it sits under (`""` above every header).
    table: String,
    /// The key as written, unquoted, dotted parts joined by `.`.
    key: String,
    /// Its first and last lines, zero-based.
    lines: (usize, usize),
    /// Where the value starts and ends on the first line (a single-line value), in bytes.
    span: (usize, usize),
    /// The value's text with comments removed and lines joined.
    value: String,
}

/// A table header (`[name]`), or an array-of-tables header, which never matches a table.
#[derive(Clone, PartialEq, Debug)]
struct Header {
    name: String,
    line: usize,
}

/// The keys and headers of `text`, in order.
fn scan(text: &str) -> (Vec<Entry>, Vec<Header>) {
    let lines: Vec<&str> = text.lines().collect();
    let (mut entries, mut headers) = (Vec::new(), Vec::new());
    let mut table = String::new();
    let mut ix = 0_usize;
    while let Some(line) = lines.get(ix) {
        let trimmed = line.trim_start();
        let next = ix.saturating_add(1);
        if trimmed.is_empty() || trimmed.starts_with('#') {
            ix = next;
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix('[') {
            let code = strip_comment(rest);
            let name = if code.starts_with('[') {
                // `[[name]]`: an array of tables, which no row edits.
                format!("[{}", code.trim())
            } else {
                code.trim().trim_end_matches(']').trim().to_owned()
            };
            table.clone_from(&name);
            headers.push(Header { name, line: ix });
            ix = next;
            continue;
        }
        let Some((key, _)) = trimmed.split_once('=') else {
            ix = next;
            continue;
        };
        let eq = line.len().saturating_sub(trimmed.len()).saturating_add(key.len());
        let after = line.get(eq.saturating_add(1)..).unwrap_or_default();
        let lead = after.len().saturating_sub(after.trim_start().len());
        let start = eq.saturating_add(1).saturating_add(lead);
        let first = strip_comment(after).trim().to_owned();
        let end = start.saturating_add(first.len());
        let key = key.split('.').map(|part| unquote(part.trim())).collect::<Vec<_>>().join(".");
        // A value that opens a bracket or a triple quote may run on for lines.
        let mut value = first;
        let mut last = ix;
        while open(&value) {
            let Some(more) = lines.get(last.saturating_add(1)) else { break };
            last = last.saturating_add(1);
            value.push(' ');
            value.push_str(strip_comment(more).trim());
        }
        entries.push(Entry {
            table: table.clone(),
            key,
            lines: (ix, last),
            span: (start, end),
            value,
        });
        ix = last.saturating_add(1);
    }
    (entries, headers)
}

/// `key` without the quotes a quoted key wears.
fn unquote(key: &str) -> String {
    key.strip_prefix('"')
        .and_then(|k| k.strip_suffix('"'))
        .or_else(|| key.strip_prefix('\'').and_then(|k| k.strip_suffix('\'')))
        .unwrap_or(key)
        .to_owned()
}

/// Whether `value` leaves a bracket or a triple-quoted string open.
fn open(value: &str) -> bool {
    let trimmed = value.trim_start();
    for quotes in ["\"\"\"", "'''"] {
        if let Some(rest) = trimmed.strip_prefix(quotes) {
            return !rest.contains(quotes);
        }
    }
    if !trimmed.starts_with('[') {
        return false;
    }
    let mut depth = 0_i32;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for c in trimmed.chars() {
        match quote {
            Some('"') if escaped => escaped = false,
            Some('"') if c == '\\' => escaped = true,
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None => match c {
                '"' | '\'' => quote = Some(c),
                '[' => depth = depth.saturating_add(1),
                ']' => depth = depth.saturating_sub(1),
                _ => {}
            },
        }
    }
    depth > 0
}

/// `line` up to a `#` that is not inside a string.
fn strip_comment(line: &str) -> &str {
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for (at, c) in line.char_indices() {
        match quote {
            Some('"') if escaped => escaped = false,
            Some('"') if c == '\\' => escaped = true,
            Some(q) if c == q => quote = None,
            None if c == '"' || c == '\'' => quote = Some(c),
            None if c == '#' => return line.get(..at).unwrap_or(line).trim_end(),
            Some(_) | None => {}
        }
    }
    line.trim_end()
}

/// The entry for `table.key`: under its header, or dotted above every header.
fn find<'a>(entries: &'a [Entry], table: &str, key: &str) -> Option<&'a Entry> {
    let dotted = format!("{table}.{key}");
    entries
        .iter()
        .rev()
        .find(|e| (e.table == table && e.key == key) || (e.table.is_empty() && e.key == dotted))
}

/// The value of `table.key` in `text`, if it is set.
#[must_use]
pub fn read(text: &str, table: &str, key: &str) -> Option<Value> {
    let (entries, _) = scan(text);
    find(&entries, table, key).map(|e| parse(&e.value))
}

/// `text` with `table.key` set to `literal`, a TOML value as written.
///
/// The value is swapped in place with the line's comment kept, else the key is added at the
/// end of its table, else the table at the end of the file.
#[must_use]
pub fn write(text: &str, table: &str, key: &str, literal: &str) -> String {
    let (entries, headers) = scan(text);
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    if let Some(entry) = find(&entries, table, key) {
        let (first, last) = entry.lines;
        let Some(line) = lines.get(first) else { return text.to_owned() };
        let (start, end) = entry.span;
        let head = line.get(..start).unwrap_or_default();
        // A value over several lines becomes one; its trailing comment goes with it.
        let tail = if first == last { line.get(end..).unwrap_or_default() } else { "" };
        let replaced = format!("{head}{literal}{tail}");
        lines.splice(first..=last, [replaced]);
    } else if let Some(header) = headers.iter().rev().find(|h| h.name == table) {
        let after = entries
            .iter()
            .filter(|e| e.table == table && e.lines.0 > header.line)
            .map(|e| e.lines.1)
            .max()
            .unwrap_or(header.line);
        lines.insert(after.saturating_add(1), format!("{key} = {literal}"));
    } else {
        if lines.last().is_some_and(|l| !l.trim().is_empty()) {
            lines.push(String::new());
        }
        lines.push(format!("[{table}]"));
        lines.push(format!("{key} = {literal}"));
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

/// A TOML value's text, read.
fn parse(value: &str) -> Value {
    let value = value.trim();
    match value {
        "true" => return Value::Bool(true),
        "false" => return Value::Bool(false),
        _ => {}
    }
    if let Some(s) = string(value) {
        return Value::Str(s);
    }
    if let Some(inner) = value.strip_prefix('[').and_then(|v| v.strip_suffix(']')) {
        let items: Option<Vec<String>> =
            split_items(inner).into_iter().filter(|i| !i.is_empty()).map(|i| string(&i)).collect();
        return items.map_or_else(|| Value::Other(value.to_owned()), Value::List);
    }
    value
        .replace('_', "")
        .parse::<f64>()
        .ok()
        .filter(|n| n.is_finite())
        .map_or_else(|| Value::Other(value.to_owned()), Value::Number)
}

/// The items of an array's inside, split on the commas outside strings, trimmed.
fn split_items(inner: &str) -> Vec<String> {
    let mut items = vec![String::new()];
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for c in inner.chars() {
        match quote {
            Some('"') if escaped => escaped = false,
            Some('"') if c == '\\' => escaped = true,
            Some(q) if c == q => quote = None,
            None if c == '"' || c == '\'' => quote = Some(c),
            None if c == ',' => {
                items.push(String::new());
                continue;
            }
            Some(_) | None => {}
        }
        if let Some(item) = items.last_mut() {
            item.push(c);
        }
    }
    items.into_iter().map(|i| i.trim().to_owned()).collect()
}

/// A single-line TOML string's contents: basic (`"…"`, escapes read) or literal (`'…'`).
fn string(value: &str) -> Option<String> {
    if let Some(inner) = value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')) {
        return (!inner.contains('\'')).then(|| inner.to_owned());
    }
    let inner = value.strip_prefix('"')?.strip_suffix('"')?;
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c == '"' {
            return None;
        }
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next()? {
            'n' => out.push('\n'),
            't' => out.push('\t'),
            'r' => out.push('\r'),
            'b' => out.push('\u{8}'),
            'f' => out.push('\u{c}'),
            'e' => out.push('\u{1b}'),
            '"' => out.push('"'),
            '\\' => out.push('\\'),
            'u' => out.push(unicode(&mut chars, 4)?),
            'U' => out.push(unicode(&mut chars, 8)?),
            _ => return None,
        }
    }
    Some(out)
}

/// The `\u` or `\U` escape's scalar from its next `digits` hex digits.
fn unicode(chars: &mut std::str::Chars<'_>, digits: usize) -> Option<char> {
    let hex: String = chars.by_ref().take(digits).collect();
    (hex.len() == digits).then_some(())?;
    char::from_u32(u32::from_str_radix(&hex, 16).ok()?)
}

/// `text` as a TOML basic string.
#[must_use]
pub fn quoted(text: &str) -> String {
    let mut out = String::with_capacity(text.len().saturating_add(2));
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.extend(format!("\\u{:04X}", u32::from(c)).chars()),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A comma-separated list as a TOML array of strings; blank items are dropped.
#[must_use]
pub fn list(text: &str) -> String {
    let items: Vec<String> =
        text.split(',').map(str::trim).filter(|i| !i.is_empty()).map(quoted).collect();
    format!("[{}]", items.join(", "))
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    const FILE: &str = "\
# Slopty settings.

[font]
# Terminal size in points.
mono_size = 13.0 # mine
ligatures = true

[terminal]
cursor_style = \"program\"
allow = [
  \"a\", # first
  \"b\",
]
";

    /// A value is read under its own table, with its comment and quotes taken off.
    #[test]
    fn values_are_read_under_their_table() {
        assert_eq!(read(FILE, "font", "mono_size"), Some(Value::Number(13.0)));
        assert_eq!(read(FILE, "font", "ligatures"), Some(Value::Bool(true)));
        assert_eq!(read(FILE, "terminal", "cursor_style"), Some(Value::Str("program".into())));
        assert_eq!(
            read(FILE, "terminal", "allow"),
            Some(Value::List(vec!["a".into(), "b".into()])),
            "an array over several lines"
        );
        assert_eq!(read(FILE, "terminal", "mono_size"), None, "another table's key");
        assert_eq!(read(FILE, "remote", "fps"), None);
        assert_eq!(read("font.mono_size = 9\n", "font", "mono_size"), Some(Value::Number(9.0)));
        assert_eq!(
            read("[a]\nk = \"x # not a comment \\\"q\\\"\" # one\n", "a", "k"),
            Some(Value::Str("x # not a comment \"q\"".into()))
        );
    }

    /// A write swaps the value in place and keeps every other byte, comments included; a
    /// missing key joins the end of its table and a missing table the end of the file.
    #[test]
    fn a_write_keeps_the_rest_of_the_file() {
        let set = write(FILE, "font", "mono_size", "15.0");
        assert_eq!(set, FILE.replace("mono_size = 13.0 # mine", "mono_size = 15.0 # mine"));

        let added = write(FILE, "font", "ui_size", "14.0");
        assert!(added.contains("ligatures = true\nui_size = 14.0\n\n[terminal]"), "{added}");

        let table = write(FILE, "remote", "fps", "30");
        assert!(table.ends_with("]\n\n[remote]\nfps = 30\n"), "{table}");

        let array = write(FILE, "terminal", "allow", "[\"c\"]");
        assert!(array.ends_with("cursor_style = \"program\"\nallow = [\"c\"]\n"), "{array}");

        assert_eq!(
            write("", "theme", "appearance", "\"dark\""),
            "[theme]\nappearance = \"dark\"\n"
        );
        let dotted = write("font.mono_size = 9\n", "font", "mono_size", "10.0");
        assert_eq!(dotted, "font.mono_size = 10.0\n", "a dotted key is set where it is");
    }

    /// What a control writes reads back as the same value.
    #[test]
    fn literals_read_back() {
        for text in ["plain", "with \"quotes\" and \\ and #", "tab\there", "\u{1}"] {
            let file = write("", "a", "k", &quoted(text));
            assert_eq!(read(&file, "a", "k"), Some(Value::Str(text.into())), "{file}");
        }
        let file = write("", "a", "k", &list(" 10.0.0.0/8 , ,fd00::/8"));
        assert_eq!(file, "[a]\nk = [\"10.0.0.0/8\", \"fd00::/8\"]\n");
        assert_eq!(list(""), "[]");
    }
}
