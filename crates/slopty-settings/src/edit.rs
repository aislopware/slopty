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
    /// A table of keys and their values, in the file's order: a map's entries, an inline table.
    Map(Vec<(String, Self)>),
    /// Anything else, as written: a date, a mixed array.
    Other(String),
}

/// One key's lines.
#[derive(Clone, PartialEq, Debug)]
struct Entry {
    /// The table it sits under, part by part (none above every header).
    table: Vec<String>,
    /// The key as written, part by part, unquoted.
    key: Vec<String>,
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
    /// Its name, part by part, unquoted; an array of tables' first part starts with `[`.
    name: Vec<String>,
    line: usize,
}

/// The keys and headers of `text`, in order.
fn scan(text: &str) -> (Vec<Entry>, Vec<Header>) {
    let lines: Vec<&str> = text.lines().collect();
    let (mut entries, mut headers) = (Vec::new(), Vec::new());
    let mut table: Vec<String> = Vec::new();
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
                vec![format!("[{}", code.trim())]
            } else {
                parts(code.trim().trim_end_matches(']').trim())
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
        let key = parts(key);
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

/// A dotted name's parts (`a."b.c".d` is `a`, `b.c`, `d`), each trimmed and unquoted.
fn parts(name: &str) -> Vec<String> {
    let mut out = vec![String::new()];
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for c in name.chars() {
        match quote {
            Some('"') if escaped => escaped = false,
            Some('"') if c == '\\' => escaped = true,
            Some(q) if c == q => quote = None,
            None if c == '"' || c == '\'' => quote = Some(c),
            None if c == '.' => {
                out.push(String::new());
                continue;
            }
            Some(_) | None => {}
        }
        if let Some(part) = out.last_mut() {
            part.push(c);
        }
    }
    out.iter().map(|part| unquote(part.trim())).collect()
}

/// `key` as the file writes it: bare when it can be, else a basic string.
#[must_use]
pub fn key_text(key: &str) -> String {
    let bare =
        !key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if bare { key.to_owned() } else { quoted(key) }
}

/// The entry at `path`, wherever its parts are split between a header and a dotted key.
fn find<'a>(entries: &'a [Entry], path: &[String]) -> Option<&'a Entry> {
    entries.iter().rev().find(|e| {
        e.table.len().saturating_add(e.key.len()) == path.len()
            && e.table.iter().chain(&e.key).eq(path.iter())
    })
}

/// `table`'s parts, then `key` as one part (a key as written may be quoted).
fn path_of(table: &str, key: &str) -> Vec<String> {
    let mut path = if table.is_empty() { Vec::new() } else { parts(table) };
    path.push(unquote(key));
    path
}

/// The value of `table.key` in `text`, if it is set.
#[must_use]
pub fn read(text: &str, table: &str, key: &str) -> Option<Value> {
    let (entries, _) = scan(text);
    find(&entries, &path_of(table, key)).map(|e| parse(&e.value))
}

/// `text` with `table.key` set to `literal`, a TOML value as written; `key` as the file writes
/// it ([`key_text`]).
///
/// The value is swapped in place with the line's comment kept, else the key is added at the
/// end of its table, else the table at the end of the file.
#[must_use]
pub fn write(text: &str, table: &str, key: &str, literal: &str) -> String {
    let (entries, headers) = scan(text);
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    let table_path = if table.is_empty() { Vec::new() } else { parts(table) };
    if let Some(entry) = find(&entries, &path_of(table, key)) {
        let (first, last) = entry.lines;
        let Some(line) = lines.get(first) else { return text.to_owned() };
        let (start, end) = entry.span;
        let head = line.get(..start).unwrap_or_default();
        // A value over several lines becomes one; its trailing comment goes with it.
        let tail = if first == last { line.get(end..).unwrap_or_default() } else { "" };
        let replaced = format!("{head}{literal}{tail}");
        lines.splice(first..=last, [replaced]);
    } else if let Some(header) = headers.iter().rev().find(|h| h.name == table_path) {
        let after = entries
            .iter()
            .filter(|e| e.table == table_path && e.lines.0 > header.line)
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
    joined(&lines)
}

/// `lines` as a file's text, ending in a newline unless there are none.
fn joined(lines: &[String]) -> String {
    let mut out = lines.join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    out
}

/// `text` without `table.key`, so its default applies again: the key's lines go, every other
/// line (its table's header and the comments over it included) stays. Unchanged when the key
/// is not set.
#[must_use]
pub fn remove(text: &str, table: &str, key: &str) -> String {
    let (entries, _) = scan(text);
    let Some(entry) = find(&entries, &path_of(table, key)) else { return text.to_owned() };
    let (first, last) = entry.lines;
    let lines: Vec<String> = text
        .lines()
        .enumerate()
        .filter(|(ix, _)| !(first..=last).contains(ix))
        .map(|(_, line)| line.to_owned())
        .collect();
    joined(&lines)
}

/// The entries of the map at `table.key` (`[clipboard] workers`), by name.
///
/// The file may hold them under the map's own header (`[clipboard.workers]`), as dotted keys
/// (`workers.studio = true`), or inline (`workers = { studio = true }`).
#[must_use]
pub fn entries(text: &str, table: &str, key: &str) -> Vec<(String, Value)> {
    let (scanned, _) = scan(text);
    let map = path_of(table, key);
    if let Some(inline) = find(&scanned, &map).and_then(|e| inline(&e.value)) {
        return inline.into_iter().map(|(name, value)| (name, parse(&value))).collect();
    }
    let mut out: Vec<(String, Value)> = Vec::new();
    for entry in scanned.iter().filter(|e| in_map(e, &map)) {
        let name = entry.key.last().cloned().unwrap_or_default();
        out.retain(|(seen, _)| *seen != name);
        out.push((name, parse(&entry.value)));
    }
    out
}

/// Whether `entry` is one of the map at `map`'s, a key and nothing under it.
fn in_map(entry: &Entry, map: &[String]) -> bool {
    entry.table.len().saturating_add(entry.key.len()) == map.len().saturating_add(1)
        && entry.table.iter().chain(&entry.key).take(map.len()).eq(map.iter())
}

/// An inline table's value (`{ a = true }`) as its keys and their values as written, in the
/// file's order; `None` for any other value, or one nested deeper than a key each.
fn inline(value: &str) -> Option<Vec<(String, String)>> {
    let inner = value.trim().strip_prefix('{')?.strip_suffix('}')?;
    toml::from_str::<toml::Table>(&format!("v = {value}")).ok()?;
    let mut out = Vec::new();
    for item in split_top(inner).into_iter().filter(|i| !i.is_empty()) {
        let (key, value) = split_key(&item)?;
        let [name] = <[String; 1]>::try_from(parts(key)).ok()?;
        out.push((name, value.trim().to_owned()));
    }
    Some(out)
}

/// `item` (`key = value`) at its first `=` outside a quoted key.
fn split_key(item: &str) -> Option<(&str, &str)> {
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for (at, c) in item.char_indices() {
        match quote {
            Some('"') if escaped => escaped = false,
            Some('"') if c == '\\' => escaped = true,
            Some(q) if c == q => quote = None,
            None if c == '"' || c == '\'' => quote = Some(c),
            None if c == '=' => return Some((item.get(..at)?, item.get(at.saturating_add(1)..)?)),
            Some(_) | None => {}
        }
    }
    None
}

/// The items of an inline table's or an array's inside, split on the commas outside strings,
/// arrays and inline tables, trimmed.
fn split_top(inner: &str) -> Vec<String> {
    let mut items = vec![String::new()];
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let mut depth = 0_u32;
    for c in inner.chars() {
        match quote {
            Some('"') if escaped => escaped = false,
            Some('"') if c == '\\' => escaped = true,
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None => match c {
                '"' | '\'' => quote = Some(c),
                '[' | '{' => depth = depth.saturating_add(1),
                ']' | '}' => depth = depth.saturating_sub(1),
                ',' if depth == 0 => {
                    items.push(String::new());
                    continue;
                }
                _ => {}
            },
        }
        if let Some(item) = items.last_mut() {
            item.push(c);
        }
    }
    items.into_iter().map(|i| i.trim().to_owned()).collect()
}

/// `items` written inline, their keys as the file writes them.
fn inline_text(items: &[(String, String)]) -> String {
    let items: Vec<String> = items.iter().map(|(k, v)| format!("{} = {v}", key_text(k))).collect();
    if items.is_empty() { "{}".to_owned() } else { format!("{{ {} }}", items.join(", ")) }
}

/// `text` with the map at `table.key` holding `name` set to `literal`, a TOML value as written.
///
/// It is written the way the map is written already: in its inline table, beside its dotted keys,
/// or under its own header, made at the end of the file when the map is not there yet. Every other
/// line stays as it was.
#[must_use]
pub fn write_entry(text: &str, table: &str, key: &str, name: &str, literal: &str) -> String {
    let (scanned, _) = scan(text);
    let map = path_of(table, key);
    if let Some(mut held) = find(&scanned, &map).and_then(|e| inline(&e.value)) {
        match held.iter_mut().find(|(held, _)| held == name) {
            Some((_, value)) => literal.clone_into(value),
            None => held.push((name.to_owned(), literal.to_owned())),
        }
        return write(text, table, key, &inline_text(&held));
    }
    let mut path = map.clone();
    path.push(name.to_owned());
    let map_table = map.iter().map(|part| key_text(part)).collect::<Vec<_>>().join(".");
    if find(&scanned, &path).is_some() {
        return write(text, &map_table, &key_text(name), literal);
    }
    // Beside the map's last entry, in the form it was written.
    let Some(last) = scanned.iter().rev().find(|e| in_map(e, &map)) else {
        return write(text, &map_table, &key_text(name), literal);
    };
    let mut written: Vec<String> = last.key.iter().map(|part| key_text(part)).collect();
    if let Some(end) = written.last_mut() {
        *end = key_text(name);
    }
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    let at = last.lines.1.saturating_add(1).min(lines.len());
    lines.insert(at, format!("{} = {literal}", written.join(".")));
    joined(&lines)
}

/// `text` without `name` in the map at `table.key`. A map left empty goes with it: its inline
/// table's key, or its header, so no empty table is left behind. Every other line stays.
#[must_use]
pub fn remove_entry(text: &str, table: &str, key: &str, name: &str) -> String {
    let (scanned, headers) = scan(text);
    let map = path_of(table, key);
    if let Some(mut held) = find(&scanned, &map).and_then(|e| inline(&e.value)) {
        let before = held.len();
        held.retain(|(held, _)| held != name);
        if held.len() == before {
            return text.to_owned();
        }
        return if held.is_empty() {
            remove(text, table, key)
        } else {
            write(text, table, key, &inline_text(&held))
        };
    }
    let mut path = map.clone();
    path.push(name.to_owned());
    let Some(entry) = find(&scanned, &path) else { return text.to_owned() };
    let (first, last) = entry.lines;
    let mut gone: Vec<usize> = (first..=last).collect();
    let left = scanned.iter().any(|e| !std::ptr::eq(e, entry) && e.table == map);
    if !left && let Some(header) = headers.iter().rev().find(|h| h.name == map) {
        gone.push(header.line);
        // The blank line that set the header apart goes with it, when the header ends the file
        // or a blank line follows the table anyway.
        let lines: Vec<&str> = text.lines().collect();
        let blank = |ix: usize| lines.get(ix).is_some_and(|l| l.trim().is_empty());
        let after = last.max(header.line).saturating_add(1);
        if header.line > 0
            && blank(header.line.saturating_sub(1))
            && (blank(after) || after >= lines.len())
        {
            gone.push(header.line.saturating_sub(1));
        }
    }
    let lines: Vec<String> = text
        .lines()
        .enumerate()
        .filter(|(ix, _)| !gone.contains(ix))
        .map(|(_, line)| line.to_owned())
        .collect();
    joined(&lines)
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
    if let Some(items) = inline(value) {
        return Value::Map(items.into_iter().map(|(k, v)| (k, parse(&v))).collect());
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

    /// A removal takes the key's lines alone, a value over several lines whole; a key that is
    /// not set leaves the text as it was.
    #[test]
    fn a_removal_takes_the_key_alone() {
        let gone = remove(FILE, "font", "mono_size");
        assert_eq!(gone, FILE.replace("mono_size = 13.0 # mine\n", ""));
        let array = remove(FILE, "terminal", "allow");
        assert!(array.ends_with("[terminal]\ncursor_style = \"program\"\n"), "{array}");
        assert_eq!(remove(FILE, "remote", "fps"), FILE, "nothing to remove");
        let keys = write("", "keys.workspace", "new_note", "\"cmd-alt-n\"");
        assert_eq!(keys, "[keys.workspace]\nnew_note = \"cmd-alt-n\"\n");
        assert_eq!(remove(&keys, "keys.workspace", "new_note"), "[keys.workspace]\n");
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

    /// A map's entries read the same however the file writes the map: under its own header,
    /// as dotted keys under its table or above every header, or inline. A quoted name keeps its
    /// dots and spaces.
    #[test]
    fn a_maps_entries_read_in_every_form() {
        let on = |name: &str, v: bool| (name.to_owned(), Value::Bool(v));
        let want = vec![on("studio", true), on("my mac.local", false)];
        for file in [
            "[clipboard.workers]\nstudio = true\n\"my mac.local\" = false\n",
            "[clipboard]\nworkers.studio = true # here\nworkers.\"my mac.local\" = false\n",
            "clipboard.workers.studio = true\nclipboard.workers.\"my mac.local\" = false\n",
            "[clipboard]\nworkers = { studio = true, \"my mac.local\" = false }\n",
        ] {
            assert_eq!(entries(file, "clipboard", "workers"), want, "{file}");
        }
        let acp = "[worker.acp]\nmine = [\"/opt/mine\", \"--acp\"]\ngoose = []\n";
        assert_eq!(
            entries(acp, "worker", "acp"),
            [
                ("mine".to_owned(), Value::List(vec!["/opt/mine".into(), "--acp".into()])),
                ("goose".to_owned(), Value::List(Vec::new())),
            ]
        );
        assert_eq!(entries(FILE, "clipboard", "workers"), Vec::new(), "no map");
    }

    /// Setting an entry keeps every other line, comment and key where it was, in the form the
    /// map is written: in place, beside its last entry, in its inline table, or under a header
    /// of its own at the end of the file when the map is not there yet.
    #[test]
    fn an_entry_is_written_where_the_map_is() {
        let file = "\
# Mine.
[clipboard]
sync = true # on

[clipboard.workers]
# The laptop stays out.
laptop = false
studio = true # the big one

[font]
mono_size = 13.0
";
        let flipped = write_entry(file, "clipboard", "workers", "studio", "false");
        assert_eq!(
            flipped,
            file.replace("studio = true # the big one", "studio = false # the big one")
        );
        let added = write_entry(file, "clipboard", "workers", "my mac", "true");
        assert_eq!(
            added,
            file.replace("# the big one\n", "# the big one\n\"my mac\" = true\n"),
            "beside the last, quoted"
        );
        let dotted = "[clipboard]\nworkers.a = true\nsync = true\n";
        assert_eq!(
            write_entry(dotted, "clipboard", "workers", "b", "false"),
            "[clipboard]\nworkers.a = true\nworkers.b = false\nsync = true\n"
        );
        let inline = "[clipboard]\nworkers = { a = true } # mine\nsync = true\n";
        assert_eq!(
            write_entry(inline, "clipboard", "workers", "b c", "false"),
            "[clipboard]\nworkers = { a = true, \"b c\" = false } # mine\nsync = true\n"
        );
        let fresh = write_entry(FILE, "worker", "acp", "mine", "[\"/opt/mine\", \"--acp\"]");
        assert_eq!(fresh, format!("{FILE}\n[worker.acp]\nmine = [\"/opt/mine\", \"--acp\"]\n"));
        for text in [&flipped, &added, &fresh] {
            assert!(toml::from_str::<toml::Table>(text).is_ok(), "still TOML: {text}");
        }
    }

    /// Taking an entry out keeps the rest as written; the last one out takes its map with it,
    /// header or inline key, so no empty table is left behind.
    #[test]
    fn the_last_entry_out_takes_its_table() {
        let file = "\
[clipboard]
sync = true

[clipboard.workers]
laptop = false
studio = true

[font]
mono_size = 13.0
";
        let one = remove_entry(file, "clipboard", "workers", "laptop");
        assert_eq!(one, file.replace("laptop = false\n", ""));
        let none = remove_entry(&one, "clipboard", "workers", "studio");
        assert_eq!(none, "[clipboard]\nsync = true\n\n[font]\nmono_size = 13.0\n");
        let last = "[font]\nmono_size = 13.0\n\n[worker.acp]\nmine = []\n";
        assert_eq!(remove_entry(last, "worker", "acp", "mine"), "[font]\nmono_size = 13.0\n");
        let inline = "[clipboard]\nworkers = { a = true, b = false }\nsync = true\n";
        let less = remove_entry(inline, "clipboard", "workers", "a");
        assert_eq!(less, "[clipboard]\nworkers = { b = false }\nsync = true\n");
        let gone = remove_entry(&less, "clipboard", "workers", "b");
        assert_eq!(gone, "[clipboard]\nsync = true\n");
        let dotted = "[clipboard]\nworkers.a = true\nsync = true\n";
        assert_eq!(remove_entry(dotted, "clipboard", "workers", "a"), "[clipboard]\nsync = true\n");
        assert_eq!(remove_entry(file, "clipboard", "workers", "nobody"), file, "nothing to take");
    }
}
