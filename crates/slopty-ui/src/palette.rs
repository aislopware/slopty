//! `CommandPalette`: every action by name with its shortcut, filtered as you type; ↩ runs the
//! selected one, a click runs any, Esc dismisses.
//!
//! Shown by the workspace on ⌘⇧P over whatever has the keyboard; the action runs once the
//! palette is gone and the focus is back where it was, so a terminal's own actions (find,
//! the prompts) reach the terminal that was focused.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    Action, App, AppContext as _, Context, ElementId, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, KeyBinding, MouseButton, ParentElement as _, Render,
    ScrollHandle, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Window,
    div, px,
};
use gpui_kit::component::input::{Escape, Input, InputEvent, InputState, MoveDown, MoveUp};
use slopty_core::SessionId;
use slopty_theme::Theme;

use crate::colors::hsla;
use crate::icons::{self, IconName, IconSize, Status};

/// What the list says when the query leaves nothing.
pub(crate) const NO_COMMAND_MATCHES: &str = "No command matches";

/// The keys the palette's foot names, each with what it does.
pub(crate) const LEGEND: [(&str, &str); 3] = [("↩", "open"), ("esc", "close"), ("↑↓", "move")];

/// How many commands an empty field lists: the ones run last, then the first of the rest.
pub const RECENT_COMMANDS: usize = 5;

/// The most of the window's height the palette takes on a desktop, under its ceiling.
const SHARE: f32 = 0.6;

/// Where every floating layer stacks, bottom to top, painted through
/// `gpui::deferred(..).with_priority(layer.priority())`.
///
/// Named once, so a notice cannot fall under a dialog, nor a menu opened from a popover under
/// the popover, nor the phone's drawer over the palette. A panel that is part of the frame (the
/// docked or drawn-out navigator) is drawn at the default priority, under all of them.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Layer {
    /// Anchored to what opened it and dismissed by a click away: the inbox, a menu, the hosts.
    Popover,
    /// Opened from a popover or a row inside one: a block's menu.
    Submenu,
    /// Modal over the window and its scrim: the palette, the picker, the settings.
    Dialog,
    /// A notice: it is seen whatever else is up.
    Toast,
}

impl Layer {
    /// Its paint priority for `gpui::deferred`: a higher one paints over a lower one.
    #[must_use]
    pub const fn priority(self) -> usize {
        match self {
            Self::Popover => 1,
            Self::Submenu => 2,
            Self::Dialog => 3,
            Self::Toast => 4,
        }
    }
}

/// The commands last run from the palette, newest first, app-wide as an editor's command
/// history is: what an empty field lists before the rest.
#[derive(Default, Debug)]
struct RecentCommands(Vec<String>);

impl gpui::Global for RecentCommands {}

/// The labels of the commands last run from the palette, newest first.
#[must_use]
pub fn recent_commands(cx: &App) -> Vec<String> {
    cx.try_global::<RecentCommands>().map(|r| r.0.clone()).unwrap_or_default()
}

fn remember_command(label: &str, cx: &mut App) {
    let recent = &mut cx.default_global::<RecentCommands>().0;
    recent.retain(|l| l != label);
    recent.insert(0, label.to_owned());
    recent.truncate(RECENT_COMMANDS);
}

/// `word` with its first letter upper-cased: a kind as chrome prints it (`note` → `Note`).
#[must_use]
pub fn sentence_case(word: &str) -> String {
    let mut chars = word.chars();
    chars.next().map(|first| first.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

/// A list's empty state when a filter leaves nothing: one quiet line on the rows' edge.
///
/// The fuller treatment is for a list with nothing to hold at all; a query that narrows to
/// nothing is a moment, and a heading or an icon for it would outweigh the list it replaced.
pub(crate) fn quiet_line(
    theme: &Theme,
    id: &'static str,
    text: &'static str,
) -> gpui::Stateful<gpui::Div> {
    crate::kit::inset_x(div(), theme)
        .id(id)
        .debug_selector(move || id.to_owned())
        .role(gpui::accesskit::Role::Status)
        .aria_label(text)
        .py(px(theme.spacing.sm))
        .text_size(px(theme.typography.small()))
        .text_color(hsla(theme.surfaces.text_muted))
        .child(text)
}

/// The palette's foot: its keys in key caps, small and muted, read as one line.
fn legend(theme: &Theme) -> gpui::Stateful<gpui::Div> {
    let s = &theme.surfaces;
    let said = LEGEND.map(|(key, what)| format!("{key} {what}")).join(" · ");
    crate::kit::inset_x(div(), theme)
        .id("palette-legend")
        .debug_selector(|| "palette-legend".to_owned())
        .role(gpui::accesskit::Role::Label)
        .aria_label(SharedString::from(said))
        .flex_none()
        .flex()
        .items_center()
        .gap(px(theme.spacing.md))
        .py(px(theme.spacing.xs))
        .border_t_1()
        .border_color(hsla(s.border))
        .text_size(px(theme.typography.small()))
        .text_color(hsla(s.text_muted))
        .children(LEGEND.map(|(key, what)| {
            div()
                .flex()
                .items_center()
                .gap(px(theme.spacing.xs))
                .child(crate::kit::key_cap(theme, key))
                .child(what)
        }))
}

/// What a line does when it is chosen.
pub enum PaletteRun {
    /// Dispatch this from the element that had the keyboard.
    Action(Box<dyn Action>),
    /// Reveal and focus this session's terminal in the workspace.
    Session(SessionId),
    /// Reveal this item (a file card, a note) in the workspace.
    Item(slopty_core::ItemId),
    /// Go to this worker's tiles, or give it a shell when it has none.
    Worker(slopty_client::layout::WorkerKey),
    /// Open a file card for the path the field holds (relative to the active shell, `~` the
    /// worker's home), landing on `line`.
    OpenFile {
        /// As typed, with any `:line` suffix removed.
        path: String,
        /// The `:line` suffix, 1-based.
        line: Option<u32>,
    },
    /// Open a shell in the directory the field holds (spelled from the worker's root or home).
    OpenShell {
        /// As typed, without its trailing slash.
        cwd: String,
    },
    /// Open a terminal running the agent in the directory the field holds.
    OpenAgent {
        /// As typed, without its trailing slash.
        cwd: String,
    },
    /// Open this address in the default browser (a forwarded port).
    OpenUrl(String),
    /// Open this address in a browser tile.
    OpenInTile(String),
    /// Reveal this session and open its find bar on `needle` (find in every card).
    FindIn {
        /// The card's session.
        session: SessionId,
        /// What was typed.
        needle: String,
    },
    /// Type `command` into this shell again (a paste then ↩).
    Rerun {
        /// The shell that ran it.
        session: SessionId,
        /// What was typed at its prompt.
        command: String,
    },
    /// Reveal this file card and open its find bar on `needle` (find in every card).
    FindInFile {
        /// The card.
        item: slopty_core::ItemId,
        /// What was typed.
        needle: String,
    },
}

impl Clone for PaletteRun {
    fn clone(&self) -> Self {
        match self {
            Self::Action(action) => Self::Action(action.boxed_clone()),
            Self::Session(session) => Self::Session(*session),
            Self::Item(item) => Self::Item(*item),
            Self::Worker(worker) => Self::Worker(*worker),
            Self::OpenFile { path, line } => Self::OpenFile { path: path.clone(), line: *line },
            Self::OpenShell { cwd } => Self::OpenShell { cwd: cwd.clone() },
            Self::OpenAgent { cwd } => Self::OpenAgent { cwd: cwd.clone() },
            Self::OpenUrl(url) => Self::OpenUrl(url.clone()),
            Self::OpenInTile(url) => Self::OpenInTile(url.clone()),
            Self::FindIn { session, needle } => {
                Self::FindIn { session: *session, needle: needle.clone() }
            }
            Self::Rerun { session, command } => {
                Self::Rerun { session: *session, command: command.clone() }
            }
            Self::FindInFile { item, needle } => {
                Self::FindInFile { item: *item, needle: needle.clone() }
            }
        }
    }
}

impl std::fmt::Debug for PaletteRun {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Action(action) => f.debug_tuple("Action").field(&action.name()).finish(),
            Self::Session(session) => f.debug_tuple("Session").field(session).finish(),
            Self::Item(item) => f.debug_tuple("Item").field(item).finish(),
            Self::Worker(worker) => f.debug_tuple("Worker").field(worker).finish(),
            Self::OpenFile { path, line } => {
                f.debug_struct("OpenFile").field("path", path).field("line", line).finish()
            }
            Self::OpenShell { cwd } => f.debug_struct("OpenShell").field("cwd", cwd).finish(),
            Self::OpenAgent { cwd } => f.debug_struct("OpenAgent").field("cwd", cwd).finish(),
            Self::OpenUrl(url) => f.debug_tuple("OpenUrl").field(url).finish(),
            Self::OpenInTile(url) => f.debug_tuple("OpenInTile").field(url).finish(),
            Self::FindIn { session, needle } => {
                f.debug_struct("FindIn").field("session", session).field("needle", needle).finish()
            }
            Self::Rerun { session, command } => {
                f.debug_struct("Rerun").field("session", session).field("command", command).finish()
            }
            Self::FindInFile { item, needle } => {
                f.debug_struct("FindInFile").field("item", item).field("needle", needle).finish()
            }
        }
    }
}

/// The group a line is listed under.
///
/// Tiles come first, since going somewhere is what the palette is opened for most; the files a
/// worker found come last, after what is already known, unless the field spells a path, when
/// they are what was asked for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Section {
    /// Going to a tile: a session, a file card, a named tile, a find's hit.
    Tiles,
    /// Going to a worker.
    Workers,
    /// An action, a command to run again, a forwarded port.
    Commands,
    /// A path: typed into the field, or found by the worker.
    Files,
}

impl Section {
    /// The heading over its lines.
    #[must_use]
    pub const fn heading(self) -> &'static str {
        match self {
            Self::Tiles => "Tiles",
            Self::Workers => "Workers",
            Self::Commands => "Commands",
            Self::Files => "Files",
        }
    }

    const fn slug(self) -> &'static str {
        match self {
            Self::Tiles => "tiles",
            Self::Workers => "workers",
            Self::Commands => "commands",
            Self::Files => "files",
        }
    }
}

/// One line of the palette: a name, and what ↩ does.
///
/// Around them: where it is (its worker, directory and kind), what the right edge says (the
/// keys that do the same, a tile's status or age, a readout), the kind icon of a line that
/// goes somewhere, and the section it is listed under.
#[derive(Clone, Debug)]
pub struct PaletteItem {
    /// What the line says (`New note`, `shell`): a tile or a worker by its name alone, since
    /// the section it sits in already says that choosing it goes there.
    pub label: String,
    /// The right edge: an action's keys (`⌘⇧N`), or a readout (`3 hits`, a worker's round trip
    /// or what is wrong with it); empty for none. A tile's status and age are its own fields.
    pub keys: String,
    /// What runs.
    pub run: PaletteRun,
    /// The icon in the leading slot: what a tile or a worker is, or what a command does. Every
    /// line has one, so no title reads as indented under the one above it.
    pub icon: IconName,
    /// How the tile or worker is doing: its mark takes the leading slot from the kind icon, and
    /// a tile's word takes the right edge while it is not idle.
    pub status: Option<Status>,
    /// The worker the tile is on, named only when more than one is known.
    pub worker: Option<String>,
    /// Where the tile is: its directory, as the headers print it.
    pub cwd: Option<String>,
    /// What the tile's header places its title by (a note's progress, a file's folder, a
    /// page's address), after the directory.
    pub place: Option<String>,
    /// How long the tile's session has run.
    pub age: Option<std::time::Duration>,
    /// The group it is listed under.
    pub section: Section,
}

impl PaletteItem {
    const fn line(
        label: String,
        keys: String,
        run: PaletteRun,
        icon: IconName,
        section: Section,
    ) -> Self {
        Self {
            label,
            keys,
            run,
            icon,
            status: None,
            worker: None,
            cwd: None,
            place: None,
            age: None,
            section,
        }
    }

    /// Whether its right-hand text is a key chord (an action's binding), not a readout.
    #[must_use]
    pub const fn is_chord(&self) -> bool {
        matches!(self.run, PaletteRun::Action(_))
    }

    /// An item for `action`, drawn with `icon`, its keys read from `bindings` (the first
    /// binding for it).
    #[must_use]
    pub fn new(
        label: &str,
        icon: IconName,
        action: Box<dyn Action>,
        bindings: &[KeyBinding],
    ) -> Self {
        let keys = keys_for(action.as_ref(), bindings);
        Self::line(label.to_owned(), keys, PaletteRun::Action(action), icon, Section::Commands)
    }

    /// A line that goes to a session in the workspace, by its title.
    #[must_use]
    pub fn session(title: &str, session: SessionId) -> Self {
        Self::line(
            title.to_owned(),
            String::new(),
            PaletteRun::Session(session),
            IconName::SquareTerminal,
            Section::Tiles,
        )
    }

    /// A worker by its name, `detail` (its round trip, or what is wrong) on the right.
    #[must_use]
    pub fn worker(name: &str, detail: &str, worker: slopty_client::layout::WorkerKey) -> Self {
        Self::line(
            name.to_owned(),
            detail.to_owned(),
            PaletteRun::Worker(worker),
            IconName::Server,
            Section::Workers,
        )
    }

    /// An item in the workspace by its title; its icon says what it is.
    #[must_use]
    pub fn item(title: &str, icon: IconName, item: slopty_core::ItemId) -> Self {
        let run = PaletteRun::Item(item);
        Self::line(title.to_owned(), String::new(), run, icon, Section::Tiles)
    }

    /// The same line, with what its tile's header places its title by.
    #[must_use]
    pub fn placed(mut self, place: Option<String>) -> Self {
        self.place = place;
        self
    }

    /// A line that opens `url` in the browser, `detail` on the right.
    #[must_use]
    pub fn url(label: &str, detail: &str, url: &str) -> Self {
        let run = PaletteRun::OpenUrl(url.to_owned());
        Self::line(
            label.to_owned(),
            detail.to_owned(),
            run,
            IconName::ExternalLink,
            Section::Commands,
        )
    }

    /// A line that opens `url` in a browser tile, `detail` on the right.
    #[must_use]
    pub fn in_tile(label: &str, detail: &str, url: &str) -> Self {
        let run = PaletteRun::OpenInTile(url.to_owned());
        Self::line(label.to_owned(), detail.to_owned(), run, IconName::Globe, Section::Commands)
    }

    /// `Rerun <command>` for a command the active shell ran (a multi-line command shows its
    /// first line and `…`).
    #[must_use]
    pub fn rerun(command: &str, session: SessionId) -> Self {
        let first = command.lines().next().unwrap_or_default();
        let label = if command.lines().nth(1).is_some() {
            format!("Rerun {first} …")
        } else {
            format!("Rerun {first}")
        };
        let run = PaletteRun::Rerun { session, command: command.to_owned() };
        Self::line(label, String::new(), run, IconName::RotateCw, Section::Commands)
    }

    /// `Open <relative>` for a file the worker found under `root`.
    #[must_use]
    pub fn found_file(root: &str, relative: &str) -> Self {
        let path = format!("{}/{relative}", root.trim_end_matches('/'));
        let run = PaletteRun::OpenFile { path, line: None };
        Self::line(
            format!("Open {relative}"),
            String::new(),
            run,
            IconName::FileText,
            Section::Files,
        )
    }

    /// A directory the worker found under `root`: a shell and a conversation in it.
    #[must_use]
    pub fn found_dir(root: &str, relative: &str) -> [Self; 2] {
        let relative = relative.trim_end_matches('/');
        let cwd = format!("{}/{relative}", root.trim_end_matches('/'));
        let mut shell = Self::open_shell(&cwd);
        shell.label = format!("New terminal in {relative}");
        let mut agent = Self::open_agent(&cwd);
        agent.label = format!("New agent in {relative}");
        [shell, agent]
    }

    /// `<title>` with `N hits` on the right for a card the needle was found in; ↩ does
    /// `run` (the card's own find bar, or the card itself).
    #[must_use]
    pub fn hits(title: &str, total: u32, run: PaletteRun) -> Self {
        let keys = if total == 1 { "1 hit".to_owned() } else { format!("{total} hits") };
        let icon = match run {
            PaletteRun::FindIn { .. } | PaletteRun::Session(_) | PaletteRun::Rerun { .. } => {
                IconName::SquareTerminal
            }
            PaletteRun::FindInFile { .. } | PaletteRun::OpenFile { .. } => IconName::FileText,
            PaletteRun::Item(_) => IconName::StickyNote,
            PaletteRun::Action(_)
            | PaletteRun::Worker(_)
            | PaletteRun::OpenShell { .. }
            | PaletteRun::OpenAgent { .. }
            | PaletteRun::OpenUrl(_)
            | PaletteRun::OpenInTile(_) => IconName::Search,
        };
        Self::line(title.to_owned(), keys, run, icon, Section::Tiles)
    }

    /// `Open <path>` for a path typed into the field, `line N` on the right when it names one.
    #[must_use]
    pub fn open_file(path: &str, line: Option<u32>) -> Self {
        let keys = line.map(|n| format!("line {n}")).unwrap_or_default();
        let run = PaletteRun::OpenFile { path: path.to_owned(), line };
        Self::line(format!("Open {path}"), keys, run, IconName::FileText, Section::Files)
    }

    /// `New terminal in <dir>` for a directory typed into the field.
    #[must_use]
    pub fn open_shell(cwd: &str) -> Self {
        let run = PaletteRun::OpenShell { cwd: cwd.to_owned() };
        let label = format!("New terminal in {cwd}");
        Self::line(label, String::new(), run, IconName::SquareTerminal, Section::Files)
    }

    /// `New agent in <dir>` for a directory typed into the field.
    #[must_use]
    pub fn open_agent(cwd: &str) -> Self {
        let run = PaletteRun::OpenAgent { cwd: cwd.to_owned() };
        Self::line(format!("New agent in {cwd}"), String::new(), run, IconName::Bot, Section::Files)
    }

    /// The same line with another kind icon.
    #[must_use]
    pub const fn with_icon(mut self, icon: IconName) -> Self {
        self.icon = icon;
        self
    }

    /// The same line, marked with how its tile or worker is doing.
    #[must_use]
    pub const fn with_status(mut self, status: Option<Status>) -> Self {
        self.status = status;
        self
    }

    /// The same line, naming the worker it is on (`None` when only one is known).
    #[must_use]
    pub fn on_worker(mut self, worker: Option<String>) -> Self {
        self.worker = worker;
        self
    }

    /// The same line, in `cwd` (the directory as the headers print it).
    #[must_use]
    pub fn in_dir(mut self, cwd: Option<String>) -> Self {
        self.cwd = cwd;
        self
    }

    /// The same line, `age` old.
    #[must_use]
    pub const fn aged(mut self, age: Option<std::time::Duration>) -> Self {
        self.age = age;
        self
    }

    /// Where the line is, as its muted second column says it: the worker, the directory, then
    /// what the tile is.
    #[must_use]
    pub fn context(&self) -> String {
        [self.worker.as_deref(), self.cwd.as_deref(), self.place.as_deref()]
            .into_iter()
            .flatten()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" · ")
    }

    /// What the right edge says, and the status whose tone it takes (`None`: muted).
    ///
    /// A tile says its status word while it is not idle, so a scan down the right edge reads
    /// what is working, done or waiting; otherwise how long it has run, once that is past a
    /// minute ("now" on every fresh tile is noise). Anything else says its keys or readout.
    #[must_use]
    pub fn trailing(&self) -> Option<(String, Option<Status>)> {
        if self.section == Section::Tiles
            && let Some(status) = self.status.filter(|s| *s != Status::Idle)
        {
            return Some((status.label().to_owned(), Some(status)));
        }
        if !self.keys.is_empty() {
            return Some((self.keys.clone(), self.status));
        }
        self.age.filter(|age| age.as_secs() >= 60).map(|age| (age_label(age), None))
    }

    /// The same line, listed under `section`.
    #[must_use]
    pub const fn in_section(mut self, section: Section) -> Self {
        self.section = section;
        self
    }

    /// The line as a screen reader reads it: the label, where it is, then the right edge.
    #[must_use]
    pub fn a11y_label(&self) -> String {
        let context = self.context();
        let trailing = self.trailing().map(|(text, _)| text).unwrap_or_default();
        [self.label.as_str(), context.as_str(), trailing.as_str()]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// What an empty field lists: every tile and worker, and [`RECENT_COMMANDS`] commands, the
/// ones in `recent` (newest first) and then the first actions in their order.
///
/// Going to a tile is what the palette is opened for most; the whole command list is one
/// keystroke away, and scrolling past fifty commands to reach it was the wall GR #5 named.
#[must_use]
pub fn brief<'a>(items: Vec<&'a PaletteItem>, recent: &[String]) -> Vec<&'a PaletteItem> {
    let (commands, mut out): (Vec<&PaletteItem>, Vec<&PaletteItem>) =
        items.into_iter().partition(|item| item.section == Section::Commands);
    let mut chosen: Vec<&PaletteItem> = recent
        .iter()
        .filter_map(|label| commands.iter().copied().find(|item| &item.label == label))
        .collect();
    for item in commands.iter().copied().filter(|item| item.is_chord()) {
        if chosen.len() >= RECENT_COMMANDS {
            break;
        }
        if !chosen.iter().any(|c| std::ptr::eq(*c, item)) {
            chosen.push(item);
        }
    }
    chosen.truncate(RECENT_COMMANDS);
    out.extend(chosen);
    out
}

/// `items` in the order they are shown and stepped through: grouped by section, the order
/// within each kept. The files come first when `path_first` (the field spells a path).
#[must_use]
pub fn in_sections(items: Vec<&PaletteItem>, path_first: bool) -> Vec<&PaletteItem> {
    let rank = |section: Section| match section {
        Section::Files if path_first => 0,
        Section::Tiles => 1,
        Section::Workers => 2,
        Section::Commands => 3,
        Section::Files => 4,
    };
    let mut items = items;
    items.sort_by_key(|item| rank(item.section));
    items
}

/// The quiet label over a group of rows ([`crate::kit::label`]) on the one edge grid: the
/// palette's sections, the pickers', the inbox's, the empty workspace's workers. A heading, not
/// a row, to a screen reader.
pub(crate) fn section_heading(
    theme: &Theme,
    id: ElementId,
    text: &'static str,
) -> gpui::Stateful<gpui::Div> {
    crate::kit::inset_x(crate::kit::label(theme, text), theme)
        .id(id)
        .role(gpui::accesskit::Role::Heading)
        .aria_label(text)
        .pt(px(theme.spacing.sm))
        .pb(px(theme.spacing.xxs))
}

/// The fixed square a row's kind icon sits in, so every title starts on one edge.
pub(crate) fn icon_slot(theme: &Theme, name: IconName, color: gpui::Hsla) -> gpui::Div {
    div()
        .flex_none()
        .size(px(theme.typography.icon()))
        .flex()
        .items_center()
        .justify_center()
        .child(icons::icon(theme, name, IconSize::Inline, color))
}

/// A row's leading slot, one fixed square for every list (the palette, the pickers, the
/// inbox, the tile headers): the kind icon while there is nothing to say, the status mark once
/// there is, so every title starts on one edge and a state reads in the same place on every
/// row. `k` is the chrome's zoom (a tile header's in the overview); a list passes 1.
pub(crate) fn status_slot(
    theme: &Theme,
    kind: IconName,
    status: Option<Status>,
    ink: gpui::Hsla,
    k: f32,
) -> gpui::Stateful<gpui::Div> {
    let side = px(theme.typography.icon() * k);
    let mark = match status {
        Some(status) => {
            icons::status_icon(theme, status, side, hsla(status.tone(theme))).into_any_element()
        }
        None => icons::icon(theme, kind, IconSize::Inline, ink).size(side).into_any_element(),
    };
    div()
        .id("status")
        .flex_none()
        .size(px(theme.typography.icon_large() * k))
        .flex()
        .items_center()
        .justify_center()
        .when_some(status, |el, status| {
            el.role(gpui::accesskit::Role::Image).aria_label(status.label())
        })
        .child(mark)
}

/// An age as the inbox, the palette and the navigator print it: `now` under a minute, then
/// whole minutes, hours and days, the one unit that matters.
#[must_use]
pub fn age_label(age: std::time::Duration) -> String {
    let secs = age.as_secs();
    match secs {
        0..60 => "now".to_owned(),
        60..3_600 => format!("{}m", secs / 60),
        3_600..86_400 => format!("{}h", secs / 3_600),
        _ => format!("{}d", secs / 86_400),
    }
}

/// The UI's monospace family: a port's number and the settings file are set in it. Paths in
/// chrome are context, in the UI face.
pub(crate) fn mono_family(theme: &Theme) -> SharedString {
    theme.typography.mono_families.first().cloned().unwrap_or_default().into()
}

/// The keys that run `action` in `bindings`, spelled as [`keys_label`] spells them.
///
/// The first binding wins; empty when nothing binds it. The one way chrome learns a shortcut,
/// so a menu and the palette never spell the same chord two ways.
#[must_use]
pub fn keys_for(action: &dyn Action, bindings: &[KeyBinding]) -> String {
    bindings
        .iter()
        .find(|b| b.action().partial_eq(action))
        .map(|b| b.keystrokes().iter().map(|k| keys_label(k.inner())).collect::<String>())
        .unwrap_or_default()
}

/// A keystroke as the palette shows it, the same on every platform: the modifiers in the
/// menu-bar order `⌃⌥⇧⌘`, then the key,
/// a letter upper-cased, the named keys as their glyphs.
#[must_use]
pub fn keys_label(keystroke: &gpui::Keystroke) -> String {
    let m = keystroke.modifiers;
    let mut out = String::new();
    if m.control {
        out.push('⌃');
    }
    if m.alt {
        out.push('⌥');
    }
    if m.shift {
        out.push('⇧');
    }
    if m.platform {
        out.push('⌘');
    }
    match keystroke.key.as_str() {
        "up" => out.push('↑'),
        "down" => out.push('↓'),
        "left" => out.push('←'),
        "right" => out.push('→'),
        "tab" => out.push('⇥'),
        "enter" => out.push('↩'),
        "escape" => out.push('⎋'),
        "backspace" => out.push('⌫'),
        "space" => out.push('␣'),
        key => out.extend(key.chars().flat_map(char::to_uppercase)),
    }
    out
}

/// Keys as they are drawn, ↩ asking for its text form.
///
/// ↩ has an emoji form too, and a phone's font fallback picks it: a blue tile among plain
/// glyphs. The text presentation selector after it asks for the glyph. Only the drawing
/// carries the selector; what a screen reader gets keeps the bare key.
#[must_use]
pub fn drawn_keys(keys: &str) -> String {
    const TEXT_PRESENTATION: char = '\u{FE0E}';
    keys.chars()
        .flat_map(|c| [Some(c), (c == '↩').then_some(TEXT_PRESENTATION)])
        .flatten()
        .collect()
}

/// The path a query spells, when it is one.
///
/// A single word starting with `/`, `~/`, `./` or `../`, or holding a `/` with no empty
/// segment — with an optional `:line` suffix (`src/main.rs:12`) split off. A word with no
/// `/` is a command, never a file, so `note` keeps matching `New note`.
#[must_use]
pub fn path_query(query: &str) -> Option<(String, Option<u32>)> {
    let word = query.trim();
    if word.is_empty() || word.contains(char::is_whitespace) || !word.contains('/') {
        return None;
    }
    let looks_like_a_path = word.starts_with('/')
        || word.starts_with("~/")
        || word.starts_with("./")
        || word.starts_with("../")
        || word.split('/').all(|part| !part.is_empty() || word.ends_with('/'));
    if !looks_like_a_path {
        return None;
    }
    let (path, line) = match word.rsplit_once(':') {
        Some((path, digits)) if !path.is_empty() && !digits.is_empty() => {
            match digits.parse::<u32>() {
                Ok(line) => (path, Some(line)),
                Err(_) => (word, None),
            }
        }
        _ => (word, None),
    };
    Some((path.to_owned(), line))
}

/// What a path typed into the field offers.
///
/// A directory — a slash at the end, spelled from the worker's root or home — offers a shell
/// and a conversation there; anything else with the shape of a path opens as a file card. A
/// relative directory is a file line: the worker resolves a file against the active shell, but
/// a shell has to know its directory from the start.
#[must_use]
pub fn path_items(query: &str) -> Vec<PaletteItem> {
    if let Some(url) =
        query.trim().contains("://").then(|| crate::browser::web_url(query)).flatten()
    {
        let label = format!("Open {} in a tile", crate::browser::short_url(&url));
        return vec![PaletteItem::in_tile(&label, "page", &url).in_section(Section::Files)];
    }
    let Some((path, line)) = path_query(query) else {
        return Vec::new();
    };
    let rooted = path.starts_with('/') || path.starts_with("~/");
    if line.is_none() && path.ends_with('/') && rooted {
        let cwd = if path == "/" { "/" } else { path.trim_end_matches('/') };
        return vec![PaletteItem::open_shell(cwd), PaletteItem::open_agent(cwd)];
    }
    vec![PaletteItem::open_file(&path, line)]
}

/// The query worth asking the worker's files for: one word of two characters or more that is
/// not a path already spelled from its root (`/…`, `~…`, `.…`).
#[must_use]
pub fn files_query(query: &str) -> Option<&str> {
    let word = query.trim();
    let rooted = word.starts_with('/') || word.starts_with('~') || word.starts_with('.');
    (word.chars().count() >= 2 && !word.contains(char::is_whitespace) && !rooted).then_some(word)
}

/// The items `query` keeps, in their order.
///
/// Every word of the query is found in the label or in where the line is (its worker and
/// directory), case-insensitive; an empty query keeps all. So a worker's name finds its tiles,
/// and a directory the shells in it.
#[must_use]
pub fn filter<'a>(query: &str, items: &'a [PaletteItem]) -> Vec<&'a PaletteItem> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    items
        .iter()
        .filter(|item| {
            let text = format!("{} {}", item.label, item.context()).to_lowercase();
            words.iter().all(|w| text.contains(w.as_str()))
        })
        .collect()
}

/// What the palette decided.
#[derive(Debug)]
pub enum PaletteEvent {
    /// The field changed; the canvas asks the worker for the files it names.
    Changed(String),
    /// Run this, once the palette is gone and the focus is back.
    Run(PaletteRun),
    /// Esc, or a click outside.
    Dismiss,
}

/// The palette: a field and the items that match it.
pub struct CommandPalette {
    items: Vec<PaletteItem>,
    /// `Open <path>` when the field spells a path; recomputed on every change.
    path_items: Vec<PaletteItem>,
    /// `Open <path>` for the files the worker found for the field's text; dropped on a change.
    found: Vec<PaletteItem>,
    input: Entity<InputState>,
    /// Which match ↑/↓ have selected.
    selected: usize,
    /// The list's scroll, so the selected row can be brought into view.
    scroll: ScrollHandle,
    /// The selection moved (a step, a new query): the next frame scrolls to it.
    reveal: bool,
    /// A find in every card: the field's text is a needle, never a path.
    finding: bool,
    /// Whether key chords are worth printing: not on a touch device with no keyboard, where
    /// no chord can be pressed.
    chords: bool,
    /// An empty field lists the tiles, the workers and a few commands ([`brief`]), not every
    /// line: the workspace's own palette. A list of workers, ports or hits shows all of it.
    brief: bool,
    /// A window narrower than this (a phone's) gets the palette as a sheet from the top.
    sheet_below: f32,
    /// The sheet's list runs on past its foot: a fade there says so, since a touch list shows
    /// no scrollbar.
    more_below: bool,
    theme: Theme,
    _events: Subscription,
}

impl std::fmt::Debug for CommandPalette {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CommandPalette")
            .field("items", &self.items.len())
            .field("selected", &self.selected)
            .finish_non_exhaustive()
    }
}

impl EventEmitter<PaletteEvent> for CommandPalette {}

impl Focusable for CommandPalette {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.read(cx).focus_handle(cx)
    }
}

impl CommandPalette {
    /// Over `items`, the field empty and focused by whoever shows it.
    pub fn new(
        items: Vec<PaletteItem>,
        theme: Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::with_field(items, "Type a command", false, theme, window, cx)
    }

    /// The palette as a search across every card: no commands, no path lines, the field
    /// says what it is for, and the lines are what the canvas sets from the hits. `seed` is
    /// what the field starts with (the active card's own needle), selected so typing replaces
    /// it; the canvas runs the first search itself, since a set value is no change.
    pub fn find(seed: &str, theme: Theme, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let palette = Self::with_field(Vec::new(), "Find in every card", true, theme, window, cx);
        if !seed.is_empty() {
            palette.input.update(cx, |input, cx| {
                input.set_value(seed.to_owned(), window, cx);
                input.select_all(window, cx);
            });
        }
        palette
    }

    fn with_field(
        items: Vec<PaletteItem>,
        placeholder: &'static str,
        finding: bool,
        theme: Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        let events = cx.subscribe(&input, |this, _input, event, cx| match event {
            InputEvent::Change => {
                this.selected = 0;
                this.reveal = true;
                let text = this.input.read(cx).value().to_string();
                this.path_items = if this.finding { Vec::new() } else { path_items(&text) };
                this.found.clear();
                cx.emit(PaletteEvent::Changed(text));
                cx.notify();
            }
            InputEvent::PressEnter { .. } => this.run(cx),
            InputEvent::Focus | InputEvent::Blur => {}
        });
        Self {
            items,
            path_items: Vec::new(),
            found: Vec::new(),
            input,
            selected: 0,
            scroll: ScrollHandle::new(),
            reveal: false,
            finding,
            chords: true,
            brief: false,
            sheet_below: 0.0,
            more_below: false,
            theme,
            _events: events,
        }
    }

    /// List only the tiles, the workers and a few commands while the field is empty.
    pub const fn set_brief(&mut self, brief: bool) {
        self.brief = brief;
    }

    /// Show as a sheet from the top in a window narrower than `width`.
    pub const fn set_sheet_below(&mut self, width: f32) {
        self.sheet_below = width;
    }

    /// Print the actions' key chords and the key legend, or, with no keyboard to press them
    /// on, leave both out.
    pub const fn set_chords(&mut self, shown: bool) {
        self.chords = shown;
    }

    /// Start the field at `text` (a path to finish), its path lines listed at once.
    pub fn seed(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| input.set_value(text.to_owned(), window, cx));
        self.path_items = path_items(text);
    }

    /// Replace the lines under the commands (the hits of a find in every card).
    pub fn set_lines(&mut self, lines: Vec<PaletteItem>, cx: &mut Context<Self>) {
        self.found = lines;
        cx.notify();
    }

    /// The worker found `paths` under `root` for `query`: they are `Open <path>` lines after
    /// the commands, while the field still says `query`; a directory (a slash at its end)
    /// is a shell and a conversation in it.
    pub fn set_found(&mut self, root: &str, query: &str, paths: &[String], cx: &mut Context<Self>) {
        if self.input.read(cx).value().trim() != query {
            return;
        }
        self.found = paths
            .iter()
            .flat_map(|p| {
                if p.ends_with('/') {
                    PaletteItem::found_dir(root, p).into_iter().collect::<Vec<_>>()
                } else {
                    vec![PaletteItem::found_file(root, p)]
                }
            })
            .collect();
        cx.notify();
    }

    /// The items matching the field, in the order they are shown ([`in_sections`]): a path
    /// typed into it (`Open <path>`, or a shell and a conversation in a directory) first, then
    /// the tiles, the workers and the commands the text matches, then the files the worker
    /// found for it.
    ///
    /// A brief palette with nothing typed lists only the tiles, the workers and the commands
    /// [`brief`] picks.
    #[must_use]
    pub fn matches(&self, cx: &App) -> Vec<&PaletteItem> {
        let query = self.input.read(cx).value();
        let mut out: Vec<&PaletteItem> = self.path_items.iter().collect();
        let kept = filter(&query, &self.items);
        if self.brief && query.trim().is_empty() {
            out.extend(brief(kept, &recent_commands(cx)));
        } else {
            out.extend(kept);
        }
        out.extend(&self.found);
        in_sections(out, !self.path_items.is_empty())
    }

    /// The selected match's index, clamped to the matches.
    fn selected(&self, count: usize) -> usize {
        self.selected.min(count.saturating_sub(1))
    }

    fn run(&self, cx: &mut Context<Self>) {
        let matches = self.matches(cx);
        let at = self.selected(matches.len());
        if let Some(item) = matches.get(at).map(|item| (*item).clone()) {
            Self::choose(&item, cx);
        }
    }

    /// `item` was chosen: a command joins the recent ones, and it runs.
    fn choose(item: &PaletteItem, cx: &mut Context<Self>) {
        if item.section == Section::Commands {
            remember_command(&item.label, cx);
        }
        cx.emit(PaletteEvent::Run(item.run.clone()));
    }

    /// Whether the list, as last laid out, runs on below what it shows: more than its own
    /// padding under the last row is still to come.
    fn runs_on(&self) -> bool {
        let left = self.scroll.max_offset().y + self.scroll.offset().y;
        left > px(self.theme.spacing.xs + 0.5)
    }

    fn step(&mut self, delta: i64, cx: &mut Context<Self>) {
        let count = i64::try_from(self.matches(cx).len()).unwrap_or(0);
        if count == 0 {
            return;
        }
        let at = i64::try_from(self.selected(usize::try_from(count).unwrap_or(0))).unwrap_or(0);
        self.selected = usize::try_from(at.saturating_add(delta).rem_euclid(count)).unwrap_or(0);
        self.reveal = true;
        cx.notify();
    }

    fn row(
        &self,
        ix: usize,
        item: &PaletteItem,
        chosen: bool,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let spacing = theme.spacing;
        let (raised, overlay) = (s.raised, s.overlay);
        let icon_ink = if chosen { s.text } else { s.text_muted };
        let trailing = item.trailing().filter(|_| self.chords || !item.is_chord());
        let places =
            [(item.worker.clone(), false), (item.cwd.clone(), true), (item.place.clone(), false)];
        let mut context = crate::kit::meta(div(), theme)
            .debug_selector(move || format!("palette-context-{ix}"))
            .flex_1()
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(spacing.xs))
            .overflow_hidden()
            .whitespace_nowrap();
        for (n, (text, path)) in
            places.into_iter().filter_map(|(text, path)| Some((text?, path))).enumerate()
        {
            if n > 0 {
                context = context.child("·");
            }
            context = context.child(
                // A path in the UI face, as every other context in the chrome is; mono is
                // for ports and figures.
                div()
                    .when(path, |el| el.min_w_0().overflow_hidden().text_ellipsis())
                    .when(!path, gpui::Styled::flex_none)
                    .child(SharedString::from(text)),
            );
        }
        let chosen_item = item.clone();
        crate::kit::row(theme, crate::kit::Row::One)
            .id(ElementId::NamedInteger("palette-item".into(), u64::try_from(ix).unwrap_or(0)))
            .debug_selector(move || format!("palette-item-{ix}"))
            .role(gpui::accesskit::Role::ListBoxOption)
            .aria_label(SharedString::from(item.a11y_label()))
            // The fill sits a base unit in from the dialog's edges; the text on the edge grid.
            .mx(px(spacing.xs))
            .px(px(spacing.inset() - spacing.xs))
            .rounded(px(theme.radii.sm))
            .cursor_pointer()
            .when(chosen, |el| el.bg(hsla(overlay)))
            .when(!chosen, |el| el.hover(move |st| st.bg(hsla(raised))))
            .active(move |st| st.bg(hsla(overlay)))
            .on_mouse_down(MouseButton::Left, |_ev, _window, cx| cx.stop_propagation())
            .on_click(cx.listener(move |_this, _ev, _window, cx| Self::choose(&chosen_item, cx)))
            .child(status_slot(theme, item.icon, item.status, hsla(icon_ink), 1.0))
            .child(
                div()
                    .debug_selector(move || format!("palette-title-{ix}"))
                    .flex_initial()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_color(hsla(s.text))
                    .child(SharedString::from(item.label.clone())),
            )
            .child(context)
            .children(trailing.map(|(text, tone)| {
                if item.is_chord() {
                    // Keys as plain muted glyphs, as Zed's palette prints them; key caps are
                    // the foot's alone.
                    div()
                        .debug_selector(move || format!("palette-keys-{ix}"))
                        .flex_none()
                        .text_size(px(theme.typography.small()))
                        .text_color(hsla(s.text_muted))
                        .child(SharedString::from(drawn_keys(&text)))
                } else {
                    crate::kit::meta(crate::kit::tabular(div()), theme)
                        .debug_selector(move || format!("palette-trailing-{ix}"))
                        .flex_none()
                        .when_some(tone, |el, tone| el.text_color(hsla(tone.tone(theme))))
                        .child(SharedString::from(text))
                }
            }))
    }
}

impl Render for CommandPalette {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme.clone();
        let s = theme.surfaces;
        let matches: Vec<PaletteItem> = self.matches(cx).into_iter().cloned().collect();
        let chosen = self.selected(matches.len());
        // A heading only where there are two groups to tell apart: a list of workers, of
        // ports or of hits is one kind already, and the dialog's title names it.
        let grouped =
            matches.iter().zip(matches.iter().skip(1)).any(|(a, b)| a.section != b.section);
        let mut rows: Vec<gpui::AnyElement> = Vec::new();
        let mut section = None;
        for (ix, item) in matches.iter().enumerate() {
            if grouped && section != Some(item.section) {
                section = Some(item.section);
                let name = format!("palette-heading-{}", item.section.slug());
                let heading = section_heading(
                    &theme,
                    ElementId::Name(name.clone().into()),
                    item.section.heading(),
                )
                .debug_selector(move || name);
                rows.push(heading.into_any_element());
            }
            if ix == chosen && std::mem::take(&mut self.reveal) {
                // Its place among the list's children, headings counted.
                self.scroll.scroll_to_item(rows.len());
            }
            rows.push(self.row(ix, item, ix == chosen, cx).into_any_element());
        }
        // A find with nothing typed yet has nothing to report: the field says what it is for.
        let waiting = self.finding && self.input.read(cx).value().trim().is_empty();
        let nothing = rows.is_empty() && !waiting;

        let viewport = window.viewport_size();
        let height = f32::from(viewport.height);
        let sheet = f32::from(viewport.width) < self.sheet_below;
        let safe_top = window.insets().effective().top;
        // No scrim: a list typed at floats over the work (`kit::anchor`).
        let backdrop = crate::kit::anchor(&theme, window);
        let backdrop = if sheet { backdrop.px_0().pt(safe_top) } else { backdrop };
        let dialog = crate::kit::dialog(&theme, crate::kit::Overlay::List);
        let dialog = if sheet {
            // A sheet from the top: the window's width, down to the keyboard.
            dialog
                .max_w_full()
                .max_h_full()
                .flex_1()
                .mb_0()
                .border_t_0()
                .border_x_0()
                .rounded_t(px(0.0))
        } else {
            let ceiling = crate::kit::Overlay::List.bounds().1;
            dialog.max_h(px(ceiling.min(height * SHARE)))
        };
        // A fade over the list's foot says there is more below: a touch list has no scrollbar,
        // and on a desktop the row the list's height cuts would otherwise end on the foot's
        // hairline, read as a row that lost its bottom. The scroll's extent is the last
        // layout's, so the frame after this one checks it again (a list just opened, or
        // narrowed by a query) and draws once more only if it changed.
        self.more_below = self.runs_on();
        cx.on_next_frame(window, |this, _window, cx| {
            if this.runs_on() != this.more_below {
                cx.notify();
            }
        });
        let fade = self.more_below.then(|| {
            let solid = hsla(s.elevated);
            div()
                .debug_selector(|| "palette-more".to_owned())
                .absolute()
                .left_0()
                .right_0()
                .bottom_0()
                .h(px(theme.density.row))
                .bg(gpui::linear_gradient(
                    180.0,
                    gpui::linear_color_stop(gpui::Hsla { a: 0.0, ..solid }, 0.0),
                    gpui::linear_color_stop(solid, 1.0),
                ))
        });
        let root = backdrop
            .id("palette-backdrop")
            .capture_action(cx.listener(|this, _: &MoveUp, _window, cx| this.step(-1, cx)))
            .capture_action(cx.listener(|this, _: &MoveDown, _window, cx| this.step(1, cx)))
            .capture_action(cx.listener(|_this, _: &Escape, _window, cx| {
                cx.emit(PaletteEvent::Dismiss);
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_this, _ev, _window, cx| {
                    cx.emit(PaletteEvent::Dismiss);
                    cx.stop_propagation();
                }),
            )
            .child(
                dialog
                    .id("palette")
                    .debug_selector(|| "palette".to_owned())
                    .role(gpui::accesskit::Role::Dialog)
                    .aria_label("Commands")
                    .child(
                        // The typed text starts on the rows' text: the edge grid.
                        crate::kit::inset_x(div(), &theme)
                            .py(px(theme.spacing.sm))
                            .border_b_1()
                            .border_color(hsla(s.border))
                            .child(
                                Input::new(&self.input)
                                    .appearance(false)
                                    .px_0()
                                    .aria_label("Command"),
                            ),
                    )
                    .child(
                        div()
                            .relative()
                            .flex_1()
                            .min_h_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .id("palette-list")
                                    .debug_selector(|| "palette-list".to_owned())
                                    .track_scroll(&self.scroll)
                                    .role(gpui::accesskit::Role::ListBox)
                                    .aria_label("Commands")
                                    .flex_1()
                                    .overflow_y_scroll()
                                    .py(px(theme.spacing.xs))
                                    .children(rows)
                                    .when(nothing, |el| {
                                        el.child(quiet_line(
                                            &theme,
                                            "palette-empty",
                                            NO_COMMAND_MATCHES,
                                        ))
                                    }),
                            )
                            .children(fade),
                    )
                    .when(self.chords, |el| el.child(legend(&theme))),
            );
        gpui::deferred(crate::kit::fade_in(root, "palette-fade", cx))
            .with_priority(Layer::Dialog.priority())
    }
}

#[cfg(test)]
mod tests {
    use gpui::Keystroke;
    use slopty_core::ItemId;

    use super::*;

    #[test]
    fn an_age_says_the_one_unit_that_matters() {
        let s = std::time::Duration::from_secs;
        assert_eq!(age_label(s(0)), "now");
        assert_eq!(age_label(s(59)), "now");
        assert_eq!(age_label(s(60)), "1m");
        assert_eq!(age_label(s(3_599)), "59m");
        assert_eq!(age_label(s(3_600)), "1h");
        assert_eq!(age_label(s(86_399)), "23h");
        assert_eq!(age_label(s(86_400 * 3)), "3d");
    }

    #[test]
    fn return_is_drawn_as_text_not_as_an_emoji() {
        assert_eq!(drawn_keys("⇧⌘↩"), "⇧⌘↩\u{FE0E}");
        assert_eq!(drawn_keys("↩"), "↩\u{FE0E}");
        assert_eq!(drawn_keys("⌘T"), "⌘T", "nothing else changes");
        assert_eq!(drawn_keys("↑↓"), "↑↓", "the arrows have no emoji form");
    }

    #[test]
    fn a_path_in_the_field_is_told_from_a_command() {
        let q = |s: &str| path_query(s);
        assert_eq!(q("/w/lib.rs"), Some(("/w/lib.rs".to_owned(), None)));
        assert_eq!(q(" /w/lib.rs:12 "), Some(("/w/lib.rs".to_owned(), Some(12))));
        assert_eq!(q("~/notes.md"), Some(("~/notes.md".to_owned(), None)));
        assert_eq!(q("./a"), Some(("./a".to_owned(), None)));
        assert_eq!(q("src/main.rs:7"), Some(("src/main.rs".to_owned(), Some(7))));
        assert_eq!(q("src/main.rs:x"), Some(("src/main.rs:x".to_owned(), None)), "not a line");
        assert_eq!(q("a//b"), None, "an empty segment is no path");
        assert_eq!(q("note"), None, "no slash: a command");
        assert_eq!(q("go to shell"), None, "words: a command");
        assert_eq!(q(""), None);

        // A directory, spelled from the root or home with a slash at the end, offers a
        // shell and a conversation there; anything else is a file. The label says what the
        // line opens, so only a line number is left for the right edge.
        let labels = |s: &str| {
            path_items(s)
                .iter()
                .map(|i| format!("{} {}", i.label, i.keys).trim_end().to_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(labels("~/proj/"), ["New terminal in ~/proj", "New agent in ~/proj"]);
        assert_eq!(labels("/"), ["New terminal in /", "New agent in /"]);
        assert!(
            matches!(&path_items("/srv/a/")[0].run, PaletteRun::OpenShell { cwd } if cwd == "/srv/a")
        );
        assert!(
            matches!(&path_items("/srv/a/")[1].run, PaletteRun::OpenAgent { cwd } if cwd == "/srv/a")
        );
        assert_eq!(labels("~/proj"), ["Open ~/proj"], "no slash at the end: a file");
        assert_eq!(labels("src/"), ["Open src/"], "relative: no shell to spell it from");
        assert_eq!(labels("./x/"), ["Open ./x/"]);
        assert_eq!(labels("/w/a.rs:3"), ["Open /w/a.rs line 3"]);
        assert!(labels("note").is_empty());
        let item = PaletteItem::open_file("/w/lib.rs", Some(3));
        assert_eq!((item.label.as_str(), item.keys.as_str()), ("Open /w/lib.rs", "line 3"));
        assert_eq!(PaletteItem::open_file("/w", None).keys, "");

        // What is asked of the worker's files: a word, not a rooted path, not one letter.
        assert_eq!(files_query("main"), Some("main"));
        assert_eq!(files_query(" src/ma "), Some("src/ma"));
        assert_eq!(files_query("m"), None, "one letter matches everything");
        assert_eq!(files_query("/w/lib.rs"), None, "spelled from the root already");
        assert_eq!(files_query("~/x"), None);
        assert_eq!(files_query("./x"), None);
        assert_eq!(files_query("go to"), None);
        let [shell, agent] = PaletteItem::found_dir("~", "docs/manual/");
        assert_eq!(shell.label, "New terminal in docs/manual");
        assert!(matches!(&shell.run, PaletteRun::OpenShell { cwd } if cwd == "~/docs/manual"));
        assert_eq!(agent.label, "New agent in docs/manual");
        assert!(matches!(&agent.run, PaletteRun::OpenAgent { cwd } if cwd == "~/docs/manual"));
        let found = PaletteItem::found_file("/tmp/work/", "src/main.rs");
        assert_eq!(found.label, "Open src/main.rs");
        assert!(
            matches!(&found.run, PaletteRun::OpenFile { path, line: None } if path == "/tmp/work/src/main.rs"),
            "{found:?}"
        );
    }

    #[test]
    fn keys_read_as_glyphs_and_the_filter_takes_every_word() {
        let label = |k: &str| keys_label(&Keystroke::parse(k).unwrap());
        assert_eq!(label("cmd-shift-n"), "⇧⌘N", "Apple's order: ⌃⌥⇧⌘");
        assert_eq!(label("cmd-up"), "⌘↑");
        assert_eq!(label("ctrl-tab"), "⌃⇥");
        assert_eq!(label("cmd-alt-r"), "⌥⌘R");
        assert_eq!(label("cmd-="), "⌘=");
        let item = |label: &str| PaletteItem::new(label, IconName::Command, Box::new(MoveUp), &[]);
        let items = vec![item("New note"), item("Zoom in"), item("Zoom to item")];
        let labels =
            |q: &str| filter(q, &items).iter().map(|i| i.label.as_str()).collect::<Vec<_>>();
        assert_eq!(labels(""), ["New note", "Zoom in", "Zoom to item"]);
        assert_eq!(labels("zoom"), ["Zoom in", "Zoom to item"]);
        assert_eq!(labels("item zo"), ["Zoom to item"], "every word, any order, any case");
        assert!(labels("nothing").is_empty());
    }

    /// The lines are shown, and stepped through, group by group: tiles, workers, commands, then
    /// the files the worker found, each group in the order it was given. A path typed into the
    /// field is what was asked for, so its lines lead.
    #[test]
    fn lines_group_into_sections_in_a_fixed_order() {
        let worker = slopty_client::layout::WorkerKey::new(1);
        let items = [
            PaletteItem::new("New note", IconName::Command, Box::new(MoveUp), &[]),
            PaletteItem::found_file("~", "notes.md"),
            PaletteItem::worker("studio", "connected", worker),
            PaletteItem::session("zsh", SessionId::new()),
            PaletteItem::new("Zoom in", IconName::Command, Box::new(MoveDown), &[]),
            PaletteItem::session("claude", SessionId::new()),
        ];
        let order = |path_first: bool| {
            in_sections(items.iter().collect(), path_first)
                .iter()
                .map(|i| i.label.as_str())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            order(false),
            ["zsh", "claude", "studio", "New note", "Zoom in", "Open notes.md"]
        );
        assert_eq!(order(true)[0], "Open notes.md");
        let typed = path_items("~/proj/");
        assert!(typed.iter().all(|i| i.section == Section::Files), "{typed:?}");
        assert!(path_items("https://example.com").iter().all(|i| i.section == Section::Files));
    }

    /// An empty field lists every tile and worker and five commands: the ones run last, newest
    /// first, and then the first actions in their order. A readout line (a rerun, a port) only
    /// shows once it has been run from here.
    #[test]
    fn an_empty_field_lists_the_tiles_and_the_recent_commands() {
        let worker = slopty_client::layout::WorkerKey::new(1);
        let action =
            |label: &str| PaletteItem::new(label, IconName::Command, Box::new(MoveUp), &[]);
        let mut items = vec![
            PaletteItem::session("zsh", SessionId::new()),
            PaletteItem::worker("studio", "", worker),
            PaletteItem::rerun("cargo test", SessionId::new()),
        ];
        items.extend(["A", "B", "C", "D", "E", "F", "G"].map(action));
        let shown = |recent: &[&str]| {
            let recent: Vec<String> = recent.iter().map(|l| (*l).to_owned()).collect();
            brief(items.iter().collect(), &recent)
                .iter()
                .map(|i| i.label.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(shown(&[]), ["zsh", "studio", "A", "B", "C", "D", "E"]);
        assert_eq!(
            shown(&["G", "Rerun cargo test", "Gone"]),
            ["zsh", "studio", "G", "Rerun cargo test", "A", "B", "C"],
            "the recent first, a line no longer offered skipped"
        );
    }

    /// A line that goes somewhere carries its kind in the leading slot; a command carries
    /// none. The right edge says a tile's status while it is not idle, else its age past a
    /// minute, and a command's keys; the kind is read after where the tile is.
    #[test]
    fn the_right_edge_says_status_age_or_keys_and_the_kind_is_context() {
        let minutes = |n: u64| Some(std::time::Duration::from_secs(n * 60));
        let tile = PaletteItem::session("claude", SessionId::new()).aged(minutes(12));
        assert_eq!(tile.trailing(), Some(("12m".to_owned(), None)));
        let working = tile.clone().with_status(Some(Status::Working));
        assert_eq!(working.trailing(), Some(("Working".to_owned(), Some(Status::Working))));
        let idle = tile.with_status(Some(Status::Idle));
        assert_eq!(idle.trailing(), Some(("12m".to_owned(), None)), "idle: the age");
        let fresh = PaletteItem::session("zsh", SessionId::new()).aged(minutes(0));
        assert_eq!(fresh.trailing(), None, "no age under a minute");
        let note = PaletteItem::item("Release", IconName::StickyNote, ItemId::new())
            .placed(Some("1 of 3 done".to_owned()))
            .on_worker(Some("studio".to_owned()));
        assert_eq!(note.context(), "studio · 1 of 3 done");
        assert_eq!(note.trailing(), None);
        let command = PaletteItem::new("New note", IconName::StickyNote, Box::new(MoveUp), &[]);
        assert_eq!(command.icon, IconName::StickyNote, "a command shows what it does");
        let away =
            PaletteItem::worker("studio", "unreachable", slopty_client::layout::WorkerKey::new(1))
                .with_status(Some(Status::Away));
        assert_eq!(away.trailing(), Some(("unreachable".to_owned(), Some(Status::Away))));
        assert_eq!(working.a11y_label(), "claude Working");
    }

    /// The layers stack in the one order: a popover, a menu from it, a dialog, a notice.
    #[test]
    fn the_layers_stack_popover_submenu_dialog_toast() {
        let order = [Layer::Popover, Layer::Submenu, Layer::Dialog, Layer::Toast];
        assert!(order.windows(2).all(|w| w[0].priority() < w[1].priority()), "{order:?}");
        assert!(Layer::Popover.priority() > 0, "over a panel drawn at the default");
    }

    /// Every icon a line can carry is one the app loads: an icon missing from the assets draws
    /// as nothing, and the slot would sit empty.
    #[test]
    fn every_line_icon_is_embedded() {
        use gpui::AssetSource as _;

        let session = SessionId::new();
        let mut names: Vec<IconName> = [
            PaletteRun::FindIn { session, needle: String::new() },
            PaletteRun::FindInFile { item: ItemId::new(), needle: String::new() },
            PaletteRun::Item(ItemId::new()),
        ]
        .map(|run| PaletteItem::hits("x", 1, run).icon)
        .into_iter()
        .collect();
        names.push(PaletteItem::session("zsh", session).icon);
        names.push(PaletteItem::worker("w", "", slopty_client::layout::WorkerKey::new(1)).icon);
        names.extend(PaletteItem::found_dir("/w", "src").map(|line| line.icon));
        names.extend([
            PaletteItem::open_file("/w/a.rs", None).icon,
            PaletteItem::url("a", "", "http://a/").icon,
            PaletteItem::in_tile("a", "", "http://a/").icon,
            PaletteItem::rerun("ls", session).icon,
        ]);
        names.extend(crate::workspace::palette_items().into_iter().map(|line| line.icon));
        for name in names {
            let bytes = icons::Assets.load(&name.path()).ok().flatten();
            assert!(bytes.is_some(), "{name:?} is not embedded");
        }
    }
}
