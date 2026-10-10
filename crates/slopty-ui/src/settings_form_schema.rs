//! Where the settings form lists each key of `settings.toml`, and how a stepper moves a number.
//!
//! What a key is (its type, default, range, choices and words) is `slopty_settings`'s
//! ([`schema::fields`]); this adds only the page and the group it is shown under and its place
//! among them (`LAYOUT`). A key the layout does not name is still a row, at the end of its
//! table's page (`home`) under the table's own title, so a setting added to the file shows up
//! before anyone places it.
//!
//! One row is the system's rather than the file's ([`System`]): whether the app opens at login,
//! which System Settings changes too, so the form reads and sets it there and keeps no copy.
//!
//! Two pages hold no key of a table of their own: Keyboard lists the keymap's commands
//! ([`key_rows`]), each set in `[keys]` when a chord is recorded for it, and About says which
//! build this is ([`about`]).

use std::sync::LazyLock;

use slopty_settings::schema::{self, Field};

use crate::keymap::{Command, Keymap, Scope};
use crate::palette::{PaletteItem, PaletteRun};

/// A page of the form, in the order the section list gives them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Section {
    /// The theme, the chrome's size and the terminal's colours.
    Appearance,
    /// The terminal's face, cursor and text.
    Terminal,
    /// Keys, the pointer and the clipboard.
    Input,
    /// The agents beyond those Slopty knows, what projects may run, and notes to the phone.
    Agents,
    /// The server and the workers, and who may connect.
    Network,
    /// The keymap: every command's chords, recorded into `[keys]`.
    Keyboard,
    /// The version, the build and where the project lives.
    About,
}

impl Section {
    /// Every section, in order. An iPhone or an iPad runs no worker or server of its own; its
    /// Agents page edits the server's or a worker's ([`super::remote`]).
    pub const ALL: [Self; 7] = [
        Self::Appearance,
        Self::Terminal,
        Self::Input,
        Self::Agents,
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
            Self::Agents => "Agents",
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

    /// The group the section list puts it under.
    #[must_use]
    pub const fn group(self) -> Group {
        match self {
            Self::Appearance | Self::Terminal | Self::Input => Group::App,
            Self::Agents | Self::Network => Group::Machines,
            Self::Keyboard | Self::About => Group::Help,
        }
    }

    /// Its glyph in the section list.
    #[must_use]
    pub const fn symbol(self) -> crate::icons::Symbol {
        use crate::icons::Symbol;
        match self {
            Self::Appearance => Symbol::Palette,
            Self::Terminal => Symbol::Terminal,
            Self::Input => Symbol::Cursorarrow,
            Self::Agents => crate::icons::AGENT,
            Self::Network => Symbol::ServerRack,
            Self::Keyboard => Symbol::Keyboard,
            Self::About => Symbol::InfoCircle,
        }
    }
}

/// The section list's groups, each under its name, in [`Section::ALL`]'s order: how this app
/// looks and takes input, the machines it reaches, and what the Help menu opens.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Group {
    /// The theme, the terminal and the input.
    App,
    /// The agents the machines run, the server and the workers.
    Machines,
    /// The keymap and the build: the Help menu's Keyboard Shortcuts and About.
    Help,
}

impl Group {
    /// Every group, in order.
    pub const ALL: [Self; 3] = [Self::App, Self::Machines, Self::Help];

    /// Its name over its sections.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::App => "App",
            Self::Machines => "Machines",
            Self::Help => "Help",
        }
    }

    /// Its sections, in order.
    pub fn sections(self) -> impl Iterator<Item = Section> {
        Section::ALL.into_iter().filter(move |s| s.group() == self)
    }
}

/// The pages' groups in order, each with its keys (`table.key`) in order.
const LAYOUT: &[(Section, &str, &[&str])] = &[
    (Section::Appearance, "Interface", &["theme.appearance", "font.ui_size", "font.prose_size"]),
    (
        Section::Appearance,
        "Light terminal colours",
        &[
            "colors.light.foreground",
            "colors.light.background",
            "colors.light.cursor",
            "colors.light.cursor_text",
            "colors.light.selection",
            "colors.light.ansi",
        ],
    ),
    (
        Section::Appearance,
        "Dark terminal colours",
        &[
            "colors.dark.foreground",
            "colors.dark.background",
            "colors.dark.cursor",
            "colors.dark.cursor_text",
            "colors.dark.selection",
            "colors.dark.ansi",
        ],
    ),
    (
        Section::Terminal,
        "Font",
        &["font.mono_family", "font.mono_size", "font.mono_line_height", "font.ligatures"],
    ),
    (Section::Terminal, "Text", &["terminal.minimum_contrast"]),
    (Section::Terminal, "Behaviour", &["terminal.alert"]),
    (Section::Input, "Keys", &["terminal.option_as_alt", "terminal.secure_keyboard_entry"]),
    (
        Section::Input,
        "Clipboard",
        &["clipboard.sync", "clipboard.workers", "terminal.copy_on_select"],
    ),
    (Section::Agents, "ACP agents", &["worker.acp"]),
    (
        Section::Agents,
        "Projects",
        &["server.projects.live_agents", "server.projects.permission_flags"],
    ),
    (
        Section::Agents,
        "Notes on your phone",
        &["server.push.apns_key", "server.push.key_id", "server.push.team_id"],
    ),
    (Section::Network, THIS_APP, &["client.server", "client.editor"]),
    (
        Section::Network,
        "Share this Mac's shells and windows",
        &["worker.server", "worker.allow", "worker.keep_awake"],
    ),
    (Section::Network, "This Mac as a server", &["server.allow"]),
];

/// What a group says under its rows, as System Settings notes a consequence under a group: what
/// its rows' titles cannot say. A row has one line and no description of its own, since most
/// only said its title again ("Follow the system, or stay light or dark"); the rest are said
/// here once, for the group. A search still finds every row's own words ([`Row::haystack`]).
const FOOTERS: &[(&str, &str)] = &[
    ("Interface", "Reading size is for agents' answers and your messages; text size for the rest"),
    (
        "Light terminal colours",
        "An empty colour keeps the theme's own; these colour the terminal alone",
    ),
    (
        "Dark terminal colours",
        "An empty colour keeps the theme's own; these colour the terminal alone",
    ),
    ("Font", "JetBrains Mono is built in. Zooming a tile changes its size for that tile only"),
    ("Text", "Text under the minimum contrast moves toward black or white; 1 keeps every colour"),
    ("Behaviour", "When hidden, the alert sounds only while Slopty is behind other windows"),
    ("Keys", "Secure keyboard entry keeps passwords typed here from other apps, as Terminal does"),
    ("Clipboard", "Off, each machine keeps its own. A paste that would run a command asks first"),
    (
        "ACP agents",
        "Each serves the Agent Client Protocol on stdio; an empty command hides a known one",
    ),
    (THIS_APP, "The server lists your machines; it is empty until this app is set up"),
    ("Share this Mac's shells and windows", "Loopback and the tailnet are always let in"),
    ("This Mac as a server", "Loopback and the tailnet are always let in"),
    ("Projects", "Live agents counts every agent running across the fleet, in a project or not"),
    (
        "Notes on your phone",
        "A relay you deployed carries notes to your phone; your own APNs key needs none",
    ),
];

/// What `group` says under its rows, where it says anything (`FOOTERS`).
#[must_use]
pub fn footer(group: &str) -> Option<&'static str> {
    FOOTERS.iter().find(|(g, _)| *g == group).map(|(_, words)| *words)
}

/// The group of the app's own keys, which the system's [`System::OpenAtLogin`] closes.
const THIS_APP: &str = "This app";

/// The page a key of `table` that [`LAYOUT`] does not name goes on: the project bounds' and
/// the phone's notes' on Agents, any other nested table's (`server.x`) its root table's.
fn home(table: &str) -> Section {
    if matches!(table, "server.projects" | "server.push") {
        return Section::Agents;
    }
    let root = table.split_once('.').map_or(table, |(root, _)| root);
    match root {
        "theme" | "colors" => Section::Appearance,
        "clipboard" => Section::Input,
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
    /// The system holds it rather than the file: the app reads and sets it ([`System`]).
    pub system: Option<System>,
}

/// A row the system holds rather than the file. The app answers for it through a global it
/// installs (`settings_form::LoginItem`), and a form with none installed does not show it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum System {
    /// Whether the app opens at login, which System Settings' Login Items changes too.
    OpenAtLogin,
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

    /// What it does, in the file's own words: a search finds it by them, and the row says
    /// nothing under its title ([`footer`] says what its group must).
    #[must_use]
    pub fn meta(&self) -> &'static str {
        &self.field.summary
    }

    /// The text a search is held against: its words, its section and group, and its key as the
    /// file spells it.
    #[must_use]
    pub fn haystack(&self) -> String {
        let said = footer(self.group).unwrap_or_default();
        if self.system.is_some() {
            return format!(
                "{} {} {} {} {said}",
                self.label(),
                self.meta(),
                self.group,
                self.section.label()
            );
        }
        format!(
            "{} {} {} {} {}.{} {said}",
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
    static ROWS: LazyLock<Vec<Row>> = LazyLock::new(|| {
        let fields: Vec<&Field> = schema::fields().iter().collect();
        let mut rows = rows_of(&fields);
        if LOGIN_ITEMS {
            let at = rows.iter().rposition(|r| r.group == THIS_APP).map_or(rows.len(), |i| i + 1);
            let field = &*OPEN_AT_LOGIN;
            let section = Section::Network;
            let row = Row { field, section, group: THIS_APP, system: Some(System::OpenAtLogin) };
            rows.insert(at, row);
        }
        rows
    });
    &ROWS
}

/// Whether this platform has login items: a Mac does; an iPhone or an iPad opens apps itself.
const LOGIN_ITEMS: bool = cfg!(target_os = "macos");

/// [`System::OpenAtLogin`]'s words and control: a switch, off until the system says.
static OPEN_AT_LOGIN: LazyLock<Field> = LazyLock::new(|| Field {
    table: String::new(),
    table_title: THIS_APP.to_owned(),
    key: String::new(),
    title: "Open at login".to_owned(),
    summary: "Opens Slopty when you log in, so agents can reach you after a restart".to_owned(),
    kind: schema::Kind::Switch,
    default: slopty_settings::edit::Value::Bool(false),
    example: None,
});

/// Whether `table` is read by a daemon rather than by this app: a worker's or the server's,
/// which another machine's may be ([`super::remote`]).
pub(in crate::settings_form) fn daemons(table: &str) -> bool {
    let root = table.split_once('.').map_or(table, |(root, _)| root);
    matches!(root, "worker" | "server")
}

/// `fields` as rows: those [`LAYOUT`] names in its order, then the rest on their [`home`] page.
/// A key the layout does not name joins the group its table's title names, after that group's
/// own keys, so a page never heads two groups alike; one whose title names no group there
/// closes its page.
fn rows_of(fields: &[&'static Field]) -> Vec<Row> {
    let named =
        |name: &str| fields.iter().copied().find(|f| format!("{}.{}", f.table, f.key) == name);
    let placed = |f: &Field| {
        let name = format!("{}.{}", f.table, f.key);
        LAYOUT.iter().any(|(_, _, keys)| keys.contains(&name.as_str()))
    };
    let mut rows = Vec::with_capacity(fields.len());
    for section in Section::ALL {
        let groups: Vec<(&'static str, &[&str])> =
            LAYOUT.iter().filter(|(s, ..)| *s == section).map(|&(_, g, keys)| (g, keys)).collect();
        let unplaced =
            || fields.iter().copied().filter(|f| !placed(f) && home(&f.table) == section);
        for &(group, keys) in &groups {
            rows.extend(keys.iter().filter_map(|k| named(k)).map(|field| Row {
                field,
                section,
                group,
                system: None,
            }));
            rows.extend(unplaced().filter(|f| f.table_title == group).map(|field| Row {
                field,
                section,
                group,
                system: None,
            }));
        }
        rows.extend(
            unplaced()
                .filter(|f| groups.iter().all(|(g, _)| f.table_title != *g))
                .map(|field| Row { field, section, group: &field.table_title, system: None }),
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
pub const KEY_GROUPS: [&str; 10] = [
    "General",
    "Layout",
    "Terminal",
    "Threads",
    "Reviews",
    "Files",
    "Folders",
    "Project boards",
    "Search in files",
    "Pages",
];

/// The group a command is listed under: its scope's, the workspace's split into what arranges
/// the panes and the tabs and the rest.
fn key_group(command: &Command) -> &'static str {
    match command.scope() {
        Scope::App => "General",
        Scope::Workspace => {
            let name = command.name();
            let layout = name.starts_with("select_tab_")
                || matches!(
                    name,
                    "focus_left"
                        | "focus_right"
                        | "focus_up"
                        | "focus_down"
                        | "move_left"
                        | "move_right"
                        | "move_up"
                        | "move_down"
                        | "zoom_pane"
                        | "equalize_panes"
                        | "previous_project"
                        | "next_project"
                        | "previous_tab"
                        | "next_tab"
                        | "last_tab"
                        | "go_back"
                        | "go_forward"
                        | "previous_pane_tab"
                        | "next_pane_tab"
                        | "split_right"
                        | "split_down"
                        | "other_tabs"
                        | "close_other_tabs"
                        | "move_to_project"
                );
            if layout { "Layout" } else { "General" }
        }
        Scope::Terminal => "Terminal",
        Scope::Conversation => "Threads",
        Scope::File => "Files",
        Scope::Folder => "Folders",
        Scope::Project => "Project boards",
        Scope::Review => "Reviews",
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
/// scope's group. A variant of a command in its scope, named after it with more words
/// (`toggle_mute_in_remote_window`), is its wording and those words.
#[must_use]
pub fn key_rows(keymap: &Keymap, palette: &[PaletteItem]) -> Vec<KeyRow> {
    let commands = keymap.commands();
    let said = |command: &Command| {
        let action = command.action();
        let elsewhere =
            commands.iter().any(|c| c.scope() != command.scope() && c.action().partial_eq(action));
        if elsewhere {
            return None;
        }
        palette.iter().find_map(|item| match &item.run {
            PaletteRun::Action(listed) if listed.partial_eq(action) => Some(item.label.clone()),
            _ => None,
        })
    };
    let worded = |command: &Command| {
        let variant = commands.iter().find_map(|base| {
            let more = command.name().strip_prefix(base.name())?.strip_prefix('_')?;
            (base.scope() == command.scope() && base.action().partial_eq(command.action()))
                .then_some((base, more))
        });
        let said = match variant {
            Some((base, more)) => {
                said(base).map(|base| format!("{base} {}", more.replace('_', " ")))
            }
            None => said(command),
        };
        said.unwrap_or_else(|| words(command.name()))
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
            label: worded(command),
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

    /// The groups list every section once, in [`Section::ALL`]'s order, so ↑ and ↓ walk the
    /// list as it is drawn.
    #[test]
    fn the_groups_list_the_sections_in_order() {
        let listed: Vec<Section> = Group::ALL.into_iter().flat_map(Group::sections).collect();
        assert_eq!(listed, Section::ALL);
        assert!(Group::ALL.into_iter().all(|g| g.sections().next().is_some()), "none empty");
    }

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

    /// A nested table's keys go on the page of the table they sit in, but for the projects'
    /// bounds and the phone's notes, which are the Agents page's.
    #[test]
    fn a_nested_table_is_on_its_roots_page() {
        assert_eq!(home("server.projects"), Section::Agents);
        assert_eq!(home("server.push"), Section::Agents);
        assert_eq!(home("server"), Section::Network);
    }

    /// Every key of the file is one row, every key the layout names is one of the file's, and
    /// every section has rows. Beside them, on a Mac, the system's one row closes "This app".
    #[test]
    fn every_key_is_a_row_once() {
        let fields = schema::fields();
        let shown = fields.iter();
        let (system, file): (Vec<&Row>, Vec<&Row>) =
            rows().iter().partition(|r| r.system.is_some());
        assert_eq!(file.len(), shown.clone().count(), "a row per key");
        let login: Vec<_> = system.iter().map(|r| (r.system, r.group)).collect();
        let expected =
            if LOGIN_ITEMS { vec![(Some(System::OpenAtLogin), THIS_APP)] } else { vec![] };
        assert_eq!(login, expected);
        let app: Vec<_> = rows().iter().filter(|r| r.group == THIS_APP).map(Row::label).collect();
        assert_eq!(app.last().copied(), Some(if LOGIN_ITEMS { "Open at login" } else { "Editor" }));
        for f in shown {
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
    /// one several do, a variant's after its command's, its name in the file, and the groups in
    /// their order.
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
        assert_eq!((new.keys.clone(), new.set), (vec!["⇧⌘T".into()], false));
        let note = line("workspace.new_note");
        assert_eq!(
            (note.keys.clone(), note.defaults.clone()),
            (vec!["⌥⌘N".into()], vec!["⇧⌘N".into()])
        );
        assert!(note.set, "the file's");
        let find = line("terminal.find");
        assert_eq!((find.label.as_str(), find.group), ("Find", "Terminal"), "several scopes");
        assert_eq!(line("workspace.select_tab_3").label, "Select tab 3");
        assert_eq!(line("workspace.select_tab_3").group, "Layout");
        assert_eq!(line("terminal.scroll_page_up").label, "Scroll page up", "the name's words");
        assert_eq!(line("conversation.interrupt").label, "Stop the agent");
        assert_eq!(line("workspace.toggle_mute").label, "Mute sound", "its variant is no scope");
        assert_eq!(
            line("workspace.toggle_mute_in_remote_window").label,
            "Mute sound in remote window",
            "a variant is its command's words and more"
        );
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
        let mut extra = fields
            .iter()
            .find(|f| f.key == "live_agents")
            .cloned()
            .expect("a [server.projects] key");
        "sound_on_connect".clone_into(&mut extra.key);
        "Sound on connect".clone_into(&mut extra.title);
        fields.push(extra);
        let fields: &'static [Field] = fields.leak();
        let rows = rows_of(&fields.iter().collect::<Vec<_>>());
        let row = rows.iter().find(|r| r.key() == "sound_on_connect").expect("its row");
        assert_eq!((row.section, row.group), (Section::Agents, "Projects"));
        let last = rows.iter().rposition(|r| r.group == row.group);
        let last = last.and_then(|at| rows.get(at)).map(Row::key);
        assert_eq!(last, Some("sound_on_connect"), "last in its group, after the group's own keys");
        let heads: Vec<(Section, &str)> = rows
            .iter()
            .enumerate()
            .filter(|(ix, r)| {
                ix.checked_sub(1).and_then(|p| rows.get(p)).is_none_or(|p| p.group != r.group)
            })
            .map(|(_, r)| (r.section, r.group))
            .collect();
        let mut once = heads.clone();
        once.dedup();
        once.sort_unstable_by_key(|(s, g)| (s.index(), *g));
        once.dedup();
        assert_eq!(once.len(), heads.len(), "no page heads two groups alike: {heads:?}");
    }

    /// Every platform lists the daemons' rows, a phone's for another machine's file
    /// ([`super::remote`]), and the Agents page that holds most of them.
    #[test]
    fn every_platform_lists_the_daemons_rows() {
        assert!(rows().iter().any(|r| r.table() == "worker"));
        assert!(rows().iter().any(|r| r.table() == "client"), "beside the app's own");
        assert!(Section::ALL.contains(&Section::Agents));
    }

    /// The Agents page holds what the machines run agents by: the ACP agents beside the known
    /// ones, the bounds every project stays under, and how notes reach the phone, each its
    /// own group in that order.
    #[test]
    fn the_agents_page_holds_the_agents_projects_and_notes() {
        let page: Vec<(&str, String)> = rows()
            .iter()
            .filter(|r| r.section == Section::Agents)
            .map(|r| (r.group, format!("{}.{}", r.table(), r.key())))
            .collect();
        let expected: Vec<(&str, String)> = [
            ("ACP agents", "worker.acp"),
            ("Projects", "server.projects.live_agents"),
            ("Projects", "server.projects.permission_flags"),
            ("Notes on your phone", "server.push.apns_key"),
            ("Notes on your phone", "server.push.key_id"),
            ("Notes on your phone", "server.push.team_id"),
        ]
        .map(|(g, k)| (g, k.to_owned()))
        .to_vec();
        assert_eq!(page, expected);
        assert!(footer("ACP agents").is_some(), "its group says what a command is");
    }
}
