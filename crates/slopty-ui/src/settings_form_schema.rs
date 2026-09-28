//! Where the settings form lists each key of `settings.toml`, and how a stepper moves a number.
//!
//! What a key is (its type, default, range, choices and words) is `slopty_settings`'s
//! ([`schema::fields`]); this adds only the page and the group it is shown under and its place
//! among them (`LAYOUT`). A key the layout does not name is still a row, at the end of its
//! table's page (`home`) under the table's own title, so a setting added to the file shows up
//! before anyone places it.
//!
//! Two pages set nothing: Keyboard lists the keymap's bindings ([`shortcuts`]) and About says
//! which build this is ([`about`]).

use std::sync::LazyLock;

use gpui::{Action, KeyBinding};
use slopty_settings::schema::{self, Field};

use crate::palette::{PaletteItem, PaletteRun};

/// A page of the form, in the order the sidebar lists them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Section {
    /// The theme, the chrome's size and the terminal's colours.
    Appearance,
    /// The terminal's face, cursor and text.
    Terminal,
    /// Keys, the pointer and the clipboard.
    Input,
    /// Remote windows and desktops.
    Streams,
    /// The server and the workers, and who may connect.
    Network,
    /// The keymap, read-only.
    Keyboard,
    /// The version, the build and where the project lives.
    About,
}

impl Section {
    /// Every section, in order.
    pub const ALL: [Self; 7] = [
        Self::Appearance,
        Self::Terminal,
        Self::Input,
        Self::Streams,
        Self::Network,
        Self::Keyboard,
        Self::About,
    ];

    /// Its name in the sidebar.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Appearance => "Appearance",
            Self::Terminal => "Terminal",
            Self::Input => "Input",
            Self::Streams => "Streams",
            Self::Network => "Network",
            Self::Keyboard => "Keyboard",
            Self::About => "About",
        }
    }

    /// Its place in [`Self::ALL`].
    #[must_use]
    pub fn index(self) -> usize {
        Self::ALL.iter().position(|s| *s == self).unwrap_or_default()
    }
}

/// The pages' groups in order, each with its keys (`table.key`) in order.
const LAYOUT: &[(Section, &str, &[&str])] = &[
    (Section::Appearance, "Interface", &["theme.appearance", "font.ui_size"]),
    (
        Section::Appearance,
        "Terminal colours",
        &[
            "colors.foreground",
            "colors.background",
            "colors.cursor",
            "colors.cursor_text",
            "colors.selection",
            "colors.ansi",
        ],
    ),
    (
        Section::Terminal,
        "Font",
        &["font.mono_family", "font.mono_size", "font.mono_line_height", "font.ligatures"],
    ),
    (Section::Terminal, "Cursor", &["terminal.cursor_style", "terminal.cursor_blink"]),
    (Section::Terminal, "Text", &["terminal.minimum_contrast", "terminal.bold_is_bright"]),
    (Section::Terminal, "Behaviour", &["terminal.confirm_close", "terminal.bell_alert"]),
    (Section::Input, "Keys", &["terminal.option_as_alt", "terminal.natural_editing"]),
    (Section::Input, "Clipboard", &["terminal.copy_on_select", "terminal.paste_protection"]),
    (
        Section::Input,
        "Pointer",
        &["terminal.hide_pointer_while_typing", "terminal.scroll_multiplier"],
    ),
    (
        Section::Streams,
        "Remote windows and desktops",
        &["remote.fps", "remote.max_bitrate_mbps", "remote.muted"],
    ),
    (Section::Network, "This app", &["client.server"]),
    (Section::Network, "This Mac as a worker", &["worker.server", "worker.allow"]),
    (Section::Network, "This Mac as a server", &["server.allow"]),
];

/// The page a key of `table` that [`LAYOUT`] does not name goes on.
fn home(table: &str) -> Section {
    match table {
        "theme" | "colors" => Section::Appearance,
        "remote" => Section::Streams,
        "client" | "worker" | "server" => Section::Network,
        _ => Section::Terminal,
    }
}

/// One row of the form: a key of the file, and where it is listed.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Row {
    /// The key, as `slopty_settings` describes it.
    pub field: &'static Field,
    /// Its page.
    pub section: Section,
    /// The quiet label over the rows it belongs with.
    pub group: &'static str,
}

impl Row {
    /// Its table in the file.
    #[must_use]
    pub fn table(&self) -> &'static str {
        &self.field.table
    }

    /// Its key in the table.
    #[must_use]
    pub fn key(&self) -> &'static str {
        &self.field.key
    }

    /// What the row is called.
    #[must_use]
    pub fn label(&self) -> &'static str {
        &self.field.title
    }

    /// The one line under the label.
    #[must_use]
    pub fn meta(&self) -> &'static str {
        &self.field.summary
    }

    /// The text a search is held against: its words, its section and group, and its key as the
    /// file spells it.
    #[must_use]
    pub fn haystack(&self) -> String {
        format!(
            "{} {} {} {} {}.{}",
            self.label(),
            self.meta(),
            self.group,
            self.section.label(),
            self.table(),
            self.key()
        )
    }
}

/// Every row the form shows, section by section in [`Section::ALL`]'s order.
#[must_use]
pub fn rows() -> &'static [Row] {
    static ROWS: LazyLock<Vec<Row>> = LazyLock::new(|| rows_of(schema::fields()));
    &ROWS
}

/// `fields` as rows: those [`LAYOUT`] names in its order, then the rest on their [`home`] page.
fn rows_of(fields: &'static [Field]) -> Vec<Row> {
    let named = |name: &str| fields.iter().find(|f| format!("{}.{}", f.table, f.key) == name);
    let placed = |f: &Field| {
        let name = format!("{}.{}", f.table, f.key);
        LAYOUT.iter().any(|(_, _, keys)| keys.contains(&name.as_str()))
    };
    let mut rows = Vec::with_capacity(fields.len());
    for section in Section::ALL {
        for &(_, group, keys) in LAYOUT.iter().filter(|(s, ..)| *s == section) {
            rows.extend(keys.iter().filter_map(|k| named(k)).map(|field| Row {
                field,
                section,
                group,
            }));
        }
        rows.extend(
            fields.iter().filter(|f| !placed(f) && home(&f.table) == section).map(|field| Row {
                field,
                section,
                group: &field.table_title,
            }),
        );
    }
    rows
}

/// One line of the Keyboard page: what a binding does and the keys that do it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Shortcut {
    /// The group it is listed under.
    pub group: &'static str,
    /// The palette's words for the action, else its name in words.
    pub label: String,
    /// Each chord that runs it, as the palette spells keys; a numbered family (⌘1 to ⌘9) is
    /// one span, "⌘1–9".
    pub keys: Vec<String>,
}

impl Shortcut {
    /// The text a search is held against: its words, its keys and its group.
    #[must_use]
    pub fn haystack(&self) -> String {
        format!("{} {} {} Keyboard", self.label, self.keys.join(" "), self.group)
    }
}

/// The Keyboard page's groups, in order.
pub const SHORTCUT_GROUPS: [&str; 5] = ["General", "Layout", "Terminal", "Conversation", "Files"];

/// The group an action of this name is listed under; `None` for an action that is not the
/// app's own (a text field's editing keys).
fn shortcut_group(name: &str) -> Option<&'static str> {
    let (namespace, action) = name.split_once("::")?;
    let layout = ["Column", "Workspace", "Width", "Tabbed", "Overview", "Fullscreen"]
        .iter()
        .any(|word| action.contains(word))
        || matches!(action, "FocusUp" | "FocusDown" | "MoveUp" | "MoveDown");
    Some(match namespace {
        "workspace" if layout => "Layout",
        "slopty" | "workers" | "workspace" => "General",
        "terminal" => "Terminal",
        "conversation" => "Conversation",
        "file" => "Files",
        _ => return None,
    })
}

/// An action's name in words: `workspace::ScrollPageUp` is "Scroll page up".
fn words(name: &str) -> String {
    let action = name.rsplit_once("::").map_or(name, |(_, action)| action);
    let mut out = String::with_capacity(action.len().saturating_add(4));
    for (ix, c) in action.chars().enumerate() {
        if ix == 0 {
            out.push(c);
        } else if c.is_uppercase() {
            out.push(' ');
            out.extend(c.to_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// A numbered family's keys as one span: `⌘1` to `⌘9` is "⌘1–9".
fn span(first: &str, last: &str) -> String {
    let shared = first.chars().zip(last.chars()).take_while(|(a, b)| a == b).count();
    let tail: String = last.chars().skip(shared).collect();
    format!("{first}\u{2013}{tail}")
}

/// The keymap's bindings as the Keyboard page lists them, group by group in
/// [`SHORTCUT_GROUPS`]' order and in the keymap's own order within a group.
///
/// One line an action: its chords from every context it is bound in, each once. An action bound
/// with a number (a column, a workspace) is one line for the family. The words are the
/// palette's (`palette`) when it lists the action, so a binding reads the same in both.
#[must_use]
pub fn shortcuts<'a>(
    bindings: impl IntoIterator<Item = &'a KeyBinding>,
    palette: &[PaletteItem],
) -> Vec<Shortcut> {
    struct Line {
        group: &'static str,
        name: &'static str,
        action: Box<dyn Action>,
        numbered: bool,
        keys: Vec<String>,
    }
    let mut lines: Vec<Line> = Vec::new();
    for binding in bindings {
        let action = binding.action();
        let Some(group) = shortcut_group(action.name()) else { continue };
        let keys: String =
            binding.keystrokes().iter().map(|k| crate::palette::keys_label(k.inner())).collect();
        let at = lines.iter().position(|l| l.name == action.name());
        let line = if let Some(at) = at {
            let Some(line) = lines.get_mut(at) else { continue };
            line.numbered |= !line.action.partial_eq(action);
            line
        } else {
            let name = action.name();
            let action = action.boxed_clone();
            lines.push(Line { group, name, action, numbered: false, keys: Vec::new() });
            let Some(line) = lines.last_mut() else { continue };
            line
        };
        if !line.keys.contains(&keys) {
            line.keys.push(keys);
        }
    }
    let said = |action: &dyn Action| {
        palette.iter().find_map(|item| match &item.run {
            PaletteRun::Action(listed) if listed.partial_eq(action) => Some(item.label.clone()),
            _ => None,
        })
    };
    SHORTCUT_GROUPS
        .iter()
        .flat_map(|group| lines.iter().filter(move |l| l.group == *group))
        .map(|line| {
            let (label, keys) = match (line.numbered, line.keys.first(), line.keys.last()) {
                (true, Some(first), Some(last)) => (words(line.name), vec![span(first, last)]),
                _ => (
                    said(line.action.as_ref()).unwrap_or_else(|| words(line.name)),
                    line.keys.clone(),
                ),
            };
            Shortcut { group: line.group, label, keys }
        })
        .collect()
}

/// Which build this is, as the About page says it: the version, then the profile and the
/// system it was built for ("Release build for macOS").
#[must_use]
pub fn about() -> (&'static str, String) {
    let profile = if cfg!(debug_assertions) { "Debug" } else { "Release" };
    let system = match std::env::consts::OS {
        "macos" => "macOS",
        "ios" => "iOS",
        "linux" => "Linux",
        other => other,
    };
    (env!("CARGO_PKG_VERSION"), format!("{profile} build for {system}"))
}

/// Where the project lives: the About page's links, as `(words, address)`.
pub const LINKS: [(&str, &str); 3] = [
    ("Source code", "https://github.com/aislopware/slopty"),
    ("Changes in each release", "https://github.com/aislopware/slopty/blob/main/CHANGELOG.md"),
    ("Report a problem", "https://github.com/aislopware/slopty/issues/new"),
];

/// How many decimals `step` is written with: none for a whole step, else one or two.
#[must_use]
pub fn decimals(step: f64) -> usize {
    if step.fract() == 0.0 {
        0
    } else if (step * 10.0).fract().abs() < 1e-4 {
        1
    } else {
        2
    }
}

/// `value` on `step`'s grid, within `min..=max`.
#[must_use]
pub fn snap(value: f64, min: f64, max: f64, step: f64) -> f64 {
    let on_grid = if step > 0.0 { (value / step).round() * step } else { value };
    let places = i32::try_from(decimals(step)).unwrap_or(2);
    let scale = 10_f64.powi(places);
    ((on_grid * scale).round() / scale).clamp(min, max)
}

/// The grid point after `value` (or before it) on `step`'s grid: from a value between two
/// points, the nearer one on that side.
#[must_use]
pub fn next(value: f64, step: f64, up: bool) -> f64 {
    if step <= 0.0 {
        return value;
    }
    // A hair of slack, so a value on the grid written as 1.2 (11.999… steps) counts as on it.
    let at = value / step;
    let point = if up { (at + 1e-3).floor() + 1.0 } else { (at - 1e-3).ceil() - 1.0 };
    point * step
}

/// `value` as a stepper shows it: at `step`'s decimals, or at up to two when the file holds a
/// value off the grid, so what is shown is what is set.
#[must_use]
pub fn figure(value: f64, step: f64) -> String {
    let places = decimals(step);
    let exact = format!("{value:.2}");
    let exact = exact.trim_end_matches('0').trim_end_matches('.');
    let decimals_shown = exact.split_once('.').map_or(0, |(_, fraction)| fraction.len());
    if decimals_shown > places { exact.to_owned() } else { format!("{value:.places$}") }
}

/// A number as the file takes it: `13.0` for a float key (a bare `13` fails a float field
/// when written as an integer), `60` for an integer one.
#[must_use]
pub fn number(value: f64, step: f64, integer: bool) -> String {
    if integer { format!("{value:.0}") } else { format!("{value:.*}", decimals(step).max(1)) }
}

#[cfg(test)]
impl Section {
    /// A page that shows what is, with no key of the file on it.
    #[must_use]
    const fn sets_nothing(self) -> bool {
        matches!(self, Self::Keyboard | Self::About)
    }
}

#[cfg(test)]
mod tests {
    use slopty_settings::edit::Value;
    use slopty_settings::schema::Kind;

    use super::*;

    /// The stepper's arithmetic: on the grid, off it, and written as the file takes it.
    #[test]
    fn a_stepper_moves_on_its_grid() {
        assert_eq!(number(13.0, 1.0, false), "13.0");
        assert_eq!(number(1.2, 0.1, false), "1.2");
        assert_eq!(number(60.0, 15.0, true), "60");
        assert!((snap(1.23, 0.5, 2.0, 0.1) - 1.2).abs() < 1e-9, "on the grid");
        assert!((snap(0.1, 0.5, 2.0, 0.1) - 0.5).abs() < 1e-9, "clamped");
        assert!((snap(3.4, 1.0, 21.0, 0.5) - 3.5).abs() < 1e-9, "the nearer point");
        assert!((next(13.0, 1.0, true) - 14.0).abs() < 1e-9, "one up");
        assert!((next(13.5, 1.0, true) - 14.0).abs() < 1e-9, "off the grid, the nearer point");
        assert!((next(13.5, 1.0, false) - 13.0).abs() < 1e-9, "and down");
        assert!((next(1.2, 0.1, true) - 1.3).abs() < 1e-9, "a tenth up");
        assert!((next(1.2, 0.1, false) - 1.1).abs() < 1e-9, "a tenth down");
        assert_eq!(figure(13.0, 1.0), "13");
        assert_eq!(figure(13.5, 1.0), "13.5", "what the file holds");
        assert_eq!(figure(1.0, 0.1), "1.0");
        assert_eq!(figure(3.0, 0.5), "3.0");
    }

    /// Every key of the file is one row, every key the layout names is one of the file's, and
    /// every section has rows.
    #[test]
    fn every_key_is_a_row_once() {
        let fields = schema::fields();
        assert_eq!(rows().len(), fields.len(), "a row per key");
        for f in fields {
            let n = rows().iter().filter(|r| std::ptr::eq(r.field, f)).count();
            assert_eq!(n, 1, "{}.{}", f.table, f.key);
        }
        for (_, group, keys) in LAYOUT {
            for key in *keys {
                let known = fields.iter().any(|f| format!("{}.{}", f.table, f.key) == *key);
                assert!(known, "{group}: {key} is not a key of the file");
            }
        }
        for section in Section::ALL {
            let has_rows = rows().iter().any(|r| r.section == section);
            assert_eq!(has_rows, !section.sets_nothing(), "{section:?}");
        }
        for r in rows() {
            assert!(r.label().chars().next().is_some_and(char::is_uppercase), "{r:?}");
            assert!(r.meta().chars().next().is_some_and(char::is_uppercase), "{r:?}");
        }
    }

    /// The Keyboard page is the keymap: one line an action whatever contexts bind it, its
    /// chords each once, the palette's words for it, a numbered family as one span, a text
    /// field's own editing keys left out, and the groups in their order.
    #[test]
    fn the_keyboard_page_reads_the_keymap() {
        use gpui_kit::component::input::MoveUp;

        let mut bindings = crate::workspace::key_bindings();
        bindings.extend(crate::terminal::key_bindings());
        bindings.push(KeyBinding::new("up", MoveUp, Some("Input")));
        let palette = crate::workspace::palette_items();
        let listed = shortcuts(&bindings, &palette);
        let line = |label: &str| {
            listed.iter().find(|s| s.label == label).unwrap_or_else(|| panic!("{label}"))
        };
        let new = line("New terminal");
        assert_eq!((new.group, new.keys.clone()), ("General", vec!["⌘T".into(), "⌘N".into()]));
        let find = line("Find in terminal, file or conversation");
        assert_eq!(find.keys, ["⌘F"], "bound in five contexts, said once");
        assert_eq!(line("Focus column").keys, ["⌘1\u{2013}9"], "the family, not nine lines");
        assert_eq!(line("Focus column").group, "Layout");
        assert_eq!(line("Scroll page up").keys.len(), 1, "no palette words: the name's");
        assert_eq!(line("Stop the agent").group, "Conversation");
        assert_eq!(line("Save file").group, "Files");
        assert!(!listed.iter().any(|s| s.keys.iter().any(|k| k == "↑")), "a field's own key");
        let order: Vec<usize> = listed
            .iter()
            .map(|s| SHORTCUT_GROUPS.iter().position(|g| *g == s.group).unwrap_or(usize::MAX))
            .collect();
        assert!(order.is_sorted(), "{listed:#?}");
        let names: Vec<&str> = listed.iter().map(|s| s.label.as_str()).collect();
        let mut unique = names.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), names.len(), "one line an action: {names:?}");
    }

    /// A row is the schema's field: its words, default and range are `slopty_settings`'s, and
    /// a key the layout has never heard of is still a row, on its table's page under the
    /// table's title.
    #[test]
    fn a_row_comes_from_the_schema() {
        let size = rows().iter().find(|r| r.table() == "font" && r.key() == "mono_size");
        let size = size.expect("a row for the terminal's size");
        assert_eq!((size.section, size.group, size.label()), (Section::Terminal, "Font", "Size"));
        assert_eq!(size.field.default, Value::Number(13.0));
        assert!(
            matches!(&size.field.kind, Kind::Number(n) if n.min.total_cmp(&6.0).is_eq() && n.max.total_cmp(&72.0).is_eq())
        );

        let mut fields = schema::fields().to_vec();
        let mut extra = fields.iter().find(|f| f.key == "muted").cloned().expect("a switch");
        "sound_on_connect".clone_into(&mut extra.key);
        "Sound on connect".clone_into(&mut extra.title);
        fields.push(extra);
        let rows = rows_of(Box::leak(fields.into_boxed_slice()));
        let row = rows.iter().find(|r| r.key() == "sound_on_connect").expect("its row");
        assert_eq!((row.section, row.group), (Section::Streams, "Remote windows and desktops"));
        let last = rows.iter().rposition(|r| r.section == Section::Streams);
        assert_eq!(last.and_then(|at| rows.get(at)).map(Row::key), Some("sound_on_connect"));
    }
}
