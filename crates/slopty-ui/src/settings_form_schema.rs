//! Where the settings form lists each key of `settings.toml`, and how a stepper moves a number.
//!
//! What a key is (its type, default, range, choices and words) is `slopty_settings`'s
//! ([`schema::fields`]); this adds only the page and the group it is shown under and its place
//! among them (`LAYOUT`). A key the layout does not name is still a row, at the end of its
//! table's page (`home`) under the table's own title, so a setting added to the file shows up
//! before anyone places it.
//!
//! Two pages hold no key of a table of their own: Keyboard lists the keymap's commands
//! ([`key_rows`]), each set in `[keys]` when a chord is recorded for it, and About says which
//! build this is ([`about`]).

use std::sync::LazyLock;

use gpui::Action;
use slopty_settings::schema::{self, Field};

use crate::keymap::{Command, Keymap, Scope};
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
    /// The keymap: every command's chords, recorded into `[keys]`.
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

/// The page a key of `table` that [`LAYOUT`] does not name goes on: a nested table's
/// (`server.projects`) is its root table's.
fn home(table: &str) -> Section {
    let root = table.split_once('.').map_or(table, |(root, _)| root);
    match root {
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

/// One line of the Keyboard page: a command of the keymap, the chords that run it now, and
/// whether the file set them.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct KeyRow {
    /// Its place among the keymap's commands.
    pub command: usize,
    /// The group it is listed under.
    pub group: &'static str,
    /// The palette's words for it, else its name in words.
    pub label: String,
    /// Its scope and name as the file spells them (`workspace.new_terminal`).
    pub key: String,
    /// Each chord that runs it now, as the palette spells keys.
    pub keys: Vec<String>,
    /// Its default chords, spelled the same way.
    pub defaults: Vec<String>,
    /// The file sets its chords.
    pub set: bool,
}

impl KeyRow {
    /// The text a search is held against: its words, its name in the file, its keys and its
    /// group.
    #[must_use]
    pub fn haystack(&self) -> String {
        format!("{} {} {} {} Keyboard", self.label, self.key, self.keys.join(" "), self.group)
    }
}

/// The Keyboard page's groups, in order.
pub const KEY_GROUPS: [&str; 9] = [
    "General",
    "Layout",
    "Terminal",
    "Conversation",
    "Files",
    "Folders",
    "Project boards",
    "Search in files",
    "Pages",
];

/// The group a command is listed under: its scope's, the workspace's split into what arranges
/// the strip and the rest.
fn key_group(command: &Command) -> &'static str {
    match command.scope() {
        Scope::App => "General",
        Scope::Workspace => {
            let name = command.name();
            let layout = ["column", "workspace", "width", "tabbed", "overview", "fullscreen"]
                .iter()
                .any(|word| name.contains(word))
                || matches!(name, "focus_up" | "focus_down" | "move_up" | "move_down");
            if layout { "Layout" } else { "General" }
        }
        Scope::Terminal => "Terminal",
        Scope::Conversation => "Conversation",
        Scope::File => "Files",
        Scope::Folder => "Folders",
        Scope::Project => "Project boards",
        Scope::Search => "Search in files",
        Scope::Page => "Pages",
    }
}

/// A command's name in words: `scroll_page_up` is "Scroll page up".
fn words(name: &str) -> String {
    let spaced = name.replace('_', " ");
    let mut chars = spaced.chars();
    chars.next().map(|first| first.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

/// `keymap`'s commands as the Keyboard page lists them, group by group in [`KEY_GROUPS`]' order
/// and in the table's order within a group.
///
/// A command is worded as the palette words it (`palette`) when the palette lists its action
/// and no other scope runs the same action, so a line reads the same in both; an action run in
/// several scopes (find, in a terminal, a file and a face) is worded by its name, under its
/// scope's group.
#[must_use]
pub fn key_rows(keymap: &Keymap, palette: &[PaletteItem]) -> Vec<KeyRow> {
    let commands = keymap.commands();
    let said = |action: &dyn Action| {
        let shared = commands.iter().filter(|c| c.action().partial_eq(action)).count() > 1;
        if shared {
            return None;
        }
        palette.iter().find_map(|item| match &item.run {
            PaletteRun::Action(listed) if listed.partial_eq(action) => Some(item.label.clone()),
            _ => None,
        })
    };
    let spelled = |chords: &[String]| -> Vec<String> {
        chords.iter().map(|chord| crate::keymap::label(chord)).collect()
    };
    let mut rows: Vec<KeyRow> = commands
        .iter()
        .enumerate()
        .map(|(ix, command)| KeyRow {
            command: ix,
            group: key_group(command),
            label: said(command.action()).unwrap_or_else(|| words(command.name())),
            key: command.key(),
            keys: spelled(keymap.chords(ix)),
            defaults: spelled(&keymap.default_chords(ix)),
            set: keymap.is_set(ix),
        })
        .collect();
    rows.sort_by_key(|row| KEY_GROUPS.iter().position(|g| *g == row.group));
    rows
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

    /// A nested table's keys go on the page of the table they sit in: the server's projects
    /// with the server.
    #[test]
    fn a_nested_table_is_on_its_roots_page() {
        assert_eq!(home("server.projects"), Section::Network);
        assert_eq!(home("server"), Section::Network);
        assert_eq!(home("remote"), Section::Streams);
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

    /// The Keyboard page is the keymap: a line a command, its chords in effect beside its
    /// defaults, the palette's words for an action only one scope runs and its name's words for
    /// one several do, its name in the file, and the groups in their order.
    #[test]
    fn the_keyboard_page_reads_the_keymap() {
        let keys: slopty_settings::KeySettings =
            slopty_settings::Settings::parse("[keys.workspace]\nnew_note = \"cmd-alt-n\"\n")
                .settings
                .keys;
        let keymap = Keymap::new(&keys, Vec::new());
        let listed = key_rows(&keymap, &crate::workspace::palette_items());
        assert_eq!(listed.len(), keymap.commands().len(), "a line a command");
        let line =
            |key: &str| listed.iter().find(|r| r.key == key).unwrap_or_else(|| panic!("{key}"));
        let new = line("workspace.new_terminal");
        assert_eq!((new.group, new.label.as_str()), ("General", "New terminal"));
        assert_eq!((new.keys.clone(), new.set), (vec!["⌘T".into(), "⌘N".into()], false));
        let note = line("workspace.new_note");
        assert_eq!(
            (note.keys.clone(), note.defaults.clone()),
            (vec!["⌥⌘N".into()], vec!["⇧⌘N".into()])
        );
        assert!(note.set, "the file's");
        let find = line("terminal.find");
        assert_eq!((find.label.as_str(), find.group), ("Find", "Terminal"), "several scopes");
        assert_eq!(line("workspace.focus_column_3").label, "Focus column 3");
        assert_eq!(line("workspace.focus_column_3").group, "Layout");
        assert_eq!(line("terminal.scroll_page_up").label, "Scroll page up", "the name's words");
        assert_eq!(line("conversation.interrupt").label, "Stop the agent");
        assert_eq!(line("file.save").group, "Files");
        assert!(line("workspace.open_url").keys.is_empty(), "none by default, still listed");
        let order: Vec<usize> = listed
            .iter()
            .map(|r| KEY_GROUPS.iter().position(|g| *g == r.group).unwrap_or(usize::MAX))
            .collect();
        assert!(order.is_sorted(), "{listed:#?}");
        assert!(!order.contains(&usize::MAX), "every group is listed");
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
