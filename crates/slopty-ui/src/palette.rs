//! `CommandPalette`: every action by name with its shortcut, filtered as you type; ↩ runs the
//! selected one, a click runs any, Esc dismisses.
//!
//! Shown by the workspace on ⌘⇧P over whatever has the keyboard; the action runs once the
//! palette is gone and the focus is back where it was, so a terminal's own actions (find,
//! the prompts) reach the terminal that was focused.

use std::cell::{Cell, RefCell};
use std::hash::{Hash, Hasher as _};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::prelude::FluentBuilder as _;
use gpui::{
    Action, AnimationExt as _, App, AppContext as _, Bounds, Context, ElementId, Entity,
    EventEmitter, FocusHandle, Focusable, InteractiveElement as _, IntoElement, KeyBinding,
    ListAlignment, ListSizingBehavior, ListState, MouseButton, ParentElement, Pixels, Render,
    SharedString, StatefulInteractiveElement as _, Styled, Subscription, Window, div, px,
};
use gpui_kit::component::input::{Escape, Input, InputEvent, InputState, MoveDown, MoveUp};
use slopty_core::SessionId;
use slopty_theme::Theme;

use crate::colors::hsla;
use crate::fuzzy::{Fuzzy, Rank, best_first};
use crate::icons::{self, IconSize, Mark, Status, Symbol};
use crate::kit::Pace;

/// What the list says when the query leaves nothing.
pub(crate) const NO_COMMAND_MATCHES: &str = "No command matches";

/// The keys the palette's foot names beside ↩, each with what it does. What ↩ does is the
/// selected line's own verb ([`PaletteRun::verb`]).
pub(crate) const LEGEND: [(&str, &str); 2] = [("↑↓", "Move"), ("Esc", "Close")];

/// What the palette's foot says ↩ does with nothing selected.
const RETURN_VERB: &str = "Open";

/// The heading over the commands an empty field lists because they were run last.
const RECENT: &str = "Recent";

/// How many commands an empty field lists: the ones run last, then the first of the rest.
pub const RECENT_COMMANDS: usize = 5;

/// How far past its edges the list lays out lines, so a scroll never shows one appear.
const OVERDRAW: f32 = 256.0;

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
    /// A notice in the title bar or by its tile: laid out in place, painted over whatever else is
    /// up, so the inbox's "Undo" is pressed through an open popover's click-away.
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
    text: impl Into<SharedString>,
) -> gpui::Stateful<gpui::Div> {
    let text = text.into();
    crate::kit::inset_x(div(), theme)
        .id(id)
        .debug_selector(move || id.to_owned())
        .role(gpui::accesskit::Role::Status)
        .aria_label(text.clone())
        .py(px(theme.spacing.sm))
        .text_size(px(theme.typography.small()))
        .text_color(hsla(theme.surfaces.text_muted))
        .child(text)
}

/// The height of a line in a list that floats (the palette's, a picker's): a step taller than
/// the navigator's rows, since a list typed at is read one line at a time, and its foot takes
/// the same height.
pub(crate) fn line_height(theme: &Theme) -> f32 {
    theme.density.row + theme.spacing.xs
}

/// The pad round a floating list's rows: their fills sit this far in from the sheet's edges, so
/// a row's radius and the pad make the sheet's (6 + 6 = 12).
pub(crate) fn list_pad(theme: &Theme) -> f32 {
    crate::kit::sheet_pad(theme)
}

/// The ink of the dot between the facts of a meta line: the muted tone at the pressed step,
/// so the dot divides the facts without reading as one of them (monocode's footer sets its
/// dot at a quarter of the text). The hairline's own colour vanished on the light theme's `canvas`.
pub(crate) fn separator_ink(theme: &Theme) -> gpui::Hsla {
    crate::colors::hsla_alpha(theme.surfaces.text_muted, slopty_theme::alpha::PRESSED)
}

/// `text`, a meta line whose facts are joined by a spaced middle dot, with each dot in
/// [`separator_ink`]: one run of text, so the line still ends in one ellipsis.
pub(crate) fn dotted(theme: &Theme, text: impl Into<SharedString>) -> gpui::StyledText {
    const SEPARATOR: &str = " \u{b7} ";
    let text: SharedString = text.into();
    let ink = gpui::HighlightStyle { color: Some(separator_ink(theme)), ..Default::default() };
    let dots: Vec<_> = text
        .match_indices(SEPARATOR)
        .map(|(at, _)| {
            let dot = at.saturating_add(' '.len_utf8());
            (dot..dot.saturating_add('\u{b7}'.len_utf8()), ink)
        })
        .collect();
    gpui::StyledText::new(text).with_highlights(dots)
}

/// A floating list's field row: the query bare at the title size, the text on the rows' edge.
/// The field wears no frame, no fill and no rule under it: the sheet is its frame, and the
/// list's first heading parts it from the rows by space alone, as `MonoCode`'s and Raycast's do.
pub(crate) fn field_row(
    theme: &Theme,
    input: &Entity<InputState>,
    label: &'static str,
) -> gpui::Div {
    crate::kit::inset_x(div(), theme)
        .flex_none()
        .h(px(theme.density.row + theme.spacing.lg))
        .flex()
        .items_center()
        .child(
            Input::new(input)
                .appearance(false)
                .px_0()
                .text_size(px(theme.roles().panel_title.size))
                .aria_label(label),
        )
}

/// How long the selection plate takes to land while a key is held: a move that comes before
/// the last one settled glides the rest of the way this fast, and one that comes sooner than
/// this snaps, so a held arrow never trails the list.
const HELD: Duration = Duration::from_millis(60);

/// The selection plate: the one `overlay` fill under a list's selected row, which settles from
/// row to row as one plate (its place and its size eased) rather than blinking from one row to
/// the next. The navigator's, the palette's, a picker's and the inbox's views share it.
///
/// The selected row reports where it was laid out ([`Plate::mark`]), and the plate, laid under
/// the rows ([`Plate::under`]), paints there in the same frame: every element is laid out
/// before any is painted, so a list that scrolls carries the plate with it. A move starts from
/// where the plate is drawn at that moment and is never queued behind the last one; a key that
/// repeats faster than the plate can land snaps it. Under Reduce Motion it is on the row at once.
#[derive(Clone, Default)]
pub(crate) struct Plate(Rc<RefCell<Glide>>);

impl std::fmt::Debug for Plate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Plate").finish_non_exhaustive()
    }
}

impl Plate {
    /// `row`, the selected one, `key` naming it among the list's rows: it tells the plate where
    /// it was laid out this frame.
    pub(crate) fn mark<E: ParentElement + Styled>(&self, row: E, key: impl Hash) -> E {
        let glide = Rc::clone(&self.0);
        let key = key_of(key);
        row.child(
            gpui::canvas(
                move |bounds, _window, _cx| {
                    glide.borrow_mut().row = Some(Marked { key, bounds, seated: false });
                },
                |_, (), _, _| {},
            )
            .absolute()
            .inset_0(),
        )
    }

    /// [`Self::mark`], for a row that paints the plate itself while it rests there: the plate is
    /// then part of the list's content, so a list on a scroll layer keeps it in its tiles and
    /// the background under the list stays the panel's one fill, which the layer bakes. A glide
    /// is still painted by [`Self::under_on`], under the rows. Call it before the row gets its
    /// content, so the plate paints under that.
    pub(crate) fn seat<E: ParentElement + Styled>(
        &self,
        row: E,
        key: impl Hash,
        theme: &Theme,
    ) -> E {
        let glide = Rc::clone(&self.0);
        let key = key_of(key);
        let theme = theme.clone();
        let radius = px(theme.radii.sm);
        row.child(
            gpui::canvas(
                {
                    let glide = Rc::clone(&glide);
                    move |bounds, _window, _cx| {
                        glide.borrow_mut().row = Some(Marked { key, bounds, seated: true });
                    }
                },
                move |bounds, (), window, _cx| {
                    if glide.borrow().seated == Some(key) {
                        crate::kit::paint_chosen(&theme, bounds, radius, window);
                    }
                },
            )
            .absolute()
            .inset_0(),
        )
    }

    /// The plate, to lay first in the region the rows scroll in, so it paints under them. It
    /// fills that region and draws only inside it.
    pub(crate) fn under(&self, theme: &Theme) -> impl IntoElement + use<> {
        self.under_on(theme, true, None)
    }

    /// [`Self::under`], on its row at once unless `moves` (as under Reduce
    /// Motion, for a list whose owner holds its chrome still), and on its owner's clock: the
    /// plate glides by `now`, the instant the owner's frame stands for, so it moves in step
    /// with the owner's other motion (the workspace's springs) and a frame drawn again at that
    /// instant draws it in the same place. `None` reads GPUI's clock at paint: the wall in the
    /// app, the test's own under a test, so no test can be caught by a slow machine half way
    /// through a move.
    pub(crate) fn under_on(
        &self,
        theme: &Theme,
        moves: bool,
        now: Option<Instant>,
    ) -> impl IntoElement + use<> {
        let theme = theme.clone();
        let radius = px(theme.radii.sm);
        self.under_painted(moves, now, move |plate, window| {
            crate::kit::paint_chosen(&theme, plate, radius, window);
        })
    }

    /// [`Self::under`] as a segmented control's thumb ([`crate::kit::paint_thumb`]): the
    /// raised surface sliding from option to option.
    /// `moves` and `now` as for [`Self::under_on`].
    pub(crate) fn under_thumb(
        &self,
        theme: &Theme,
        moves: bool,
        now: Option<Instant>,
    ) -> impl IntoElement + use<> {
        let theme = theme.clone();
        self.under_painted(moves, now, move |plate, window| {
            crate::kit::paint_thumb(&theme, plate, window);
        })
    }

    /// The plate laid under the rows, drawn by `paint` where it stands this frame.
    fn under_painted<P: Fn(Bounds<Pixels>, &mut Window) + 'static>(
        &self,
        moves: bool,
        now: Option<Instant>,
        paint: P,
    ) -> impl IntoElement + use<P> {
        let glide = Rc::clone(&self.0);
        gpui::canvas(
            |_, _, _| {},
            move |region, (), window, cx| {
                let moving = moves && crate::kit::motion(cx);
                let mut glide = glide.borrow_mut();
                let now = now.unwrap_or_else(|| cx.background_executor().now());
                let Some(plate) = glide.frame(now, moving) else {
                    return;
                };
                if glide.seated.is_some() {
                    return;
                }
                window.with_content_mask(Some(gpui::ContentMask { bounds: region }), |window| {
                    paint(plate, window);
                });
                if glide.flight.is_some() {
                    window.request_animation_frame();
                }
            },
        )
        .absolute()
        .inset_0()
    }
}

/// A row's name among its list's, as the plate compares them.
fn key_of(key: impl Hash) -> u64 {
    let mut hasher = std::hash::DefaultHasher::new();
    key.hash(&mut hasher);
    hasher.finish()
}

/// The selected row as it reported itself in a frame: its key, where it was laid out, and
/// whether it paints the plate itself at rest ([`Plate::seat`]).
#[derive(Clone, Copy, Debug)]
struct Marked {
    key: u64,
    bounds: Bounds<Pixels>,
    seated: bool,
}

/// The plate's motion: where the selected row is, where the plate was drawn, and the move in
/// flight, as offsets from the row it is going to so a scroll carries it along.
#[derive(Default, Debug)]
struct Glide {
    /// The selected row this frame, as it reported itself; taken by the frame that paints.
    row: Option<Marked>,
    /// The row painting the plate itself this frame: a seated row with no move in flight.
    seated: Option<u64>,
    /// The row the plate is on or going to.
    on: Option<u64>,
    /// Where the plate was drawn in the last frame, if it was.
    drawn: Option<Bounds<Pixels>>,
    /// When the selection last moved.
    moved: Option<Instant>,
    flight: Option<Flight>,
}

/// A move in flight: how far off the row the plate started, when, and how long it takes.
#[derive(Clone, Copy, Debug)]
struct Flight {
    from: [f32; 4],
    start: Instant,
    length: Duration,
}

impl Glide {
    /// Where to draw the plate at `now`, `None` when no selected row was laid out. A new row
    /// starts a move from where the plate was drawn, unless `moving` is off, the plate was not
    /// drawn, or the key repeats faster than [`HELD`].
    fn frame(&mut self, now: Instant, moving: bool) -> Option<Bounds<Pixels>> {
        let Some(Marked { key, bounds: row, seated }) = self.row.take() else {
            self.drawn = None;
            self.flight = None;
            self.seated = None;
            return None;
        };
        if self.on != Some(key) {
            // The plate's first row is where it appears, not a move.
            let moved = self.on.replace(key).is_some();
            let gap = self.moved.map(|at| now.saturating_duration_since(at));
            self.moved = moved.then_some(now);
            let length = match gap {
                Some(gap) if gap < HELD => None,
                Some(gap) if gap < Pace::Settle.duration() => Some(HELD),
                _ => Some(Pace::Settle.duration()),
            };
            self.flight = match (self.drawn, length) {
                (Some(drawn), Some(length)) if moving => {
                    Some(Flight { from: sides(drawn, row), start: now, length })
                }
                _ => None,
            };
        }
        let plate = match self.flight {
            Some(flight) if moving => {
                let progress = now.saturating_duration_since(flight.start).as_secs_f32()
                    / flight.length.as_secs_f32();
                if progress >= 1.0 {
                    self.flight = None;
                }
                let rest = 1.0 - Pace::Settle.curve().at(progress.min(1.0));
                let [left, top, width, height] = flight.from.map(|d| d * rest);
                Bounds::new(
                    gpui::point(row.origin.x + px(left), row.origin.y + px(top)),
                    gpui::size(row.size.width + px(width), row.size.height + px(height)),
                )
            }
            _ => {
                self.flight = None;
                row
            }
        };
        self.seated = (seated && self.flight.is_none()).then_some(key);
        self.drawn = Some(plate);
        Some(plate)
    }
}

/// How far `from` is off `to`: its left, top, width and height less `to`'s.
fn sides(from: Bounds<Pixels>, to: Bounds<Pixels>) -> [f32; 4] {
    [
        f32::from(from.origin.x - to.origin.x),
        f32::from(from.origin.y - to.origin.y),
        f32::from(from.size.width - to.size.width),
        f32::from(from.size.height - to.size.height),
    ]
}

/// One key of a foot: its cap, then what it does.
fn foot_key(theme: &Theme, key: &'static str, what: &'static str) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap(px(theme.spacing.xs + theme.spacing.xxs))
        .child(crate::kit::key_cap(theme, key))
        .child(what)
}

/// `rows` (the list and the plate under it) fading out at their foot while more runs on below:
/// a touch list has no scrollbar, and on a desktop the row the list's height cuts would
/// otherwise end on the foot's band, read as a row that lost its bottom. The rows fade per
/// pixel and the sheet's surface stays outside the fade, so it fades alike on a phone's sheet
/// and the desktop's plate.
fn more_below(theme: &Theme, list: &ListState, rows: gpui::Div) -> gpui::EdgeFadeElement {
    let foot = gpui::Edges { bottom: px(theme.spacing.lg), ..gpui::Edges::default() };
    gpui::edge_fade(rows.debug_selector(|| "palette-rows".to_owned()), gpui::EdgeFade::new(foot))
        .hidden_by_list(list)
}

/// The palette's foot: a quiet band across the sheet's bottom, what ↩ does with the selected
/// line on its right (where the eye ends), the other keys on its left. Its caps are plates
/// with no ring, and it needs no hairline: the band is a step off the sheet.
fn legend(theme: &Theme, verb: &'static str) -> gpui::Stateful<gpui::Div> {
    let s = &theme.surfaces;
    let said = LEGEND
        .iter()
        .chain(&[("↩", verb)])
        .map(|(key, what)| format!("{key} {what}"))
        .collect::<Vec<_>>()
        .join(" · ");
    let inner = theme.radii.lg - 1.0;
    crate::kit::inset_x(div(), theme)
        .id("palette-legend")
        .debug_selector(|| "palette-legend".to_owned())
        .role(gpui::accesskit::Role::Label)
        .aria_label(SharedString::from(said))
        .flex_none()
        .h(px(line_height(theme)))
        .flex()
        .items_center()
        .gap(px(theme.spacing.md))
        .map(|el| crate::kit::inset(el, theme))
        .rounded_b(px(inner))
        .text_size(px(theme.typography.small()))
        .text_color(hsla(s.text_muted))
        .children(LEGEND.map(|(key, what)| foot_key(theme, key, what)))
        .child(div().flex_1())
        .child(foot_key(theme, "↩", verb).text_color(hsla(s.text_secondary)))
}

/// What a line does when it is chosen.
pub enum PaletteRun {
    /// Dispatch this from the element that had the keyboard.
    Action(Box<dyn Action>),
    /// Reveal and focus this session's terminal in the workspace.
    Session(SessionId),
    /// Reveal this item (a file tile, a folder) in the workspace.
    Item(slopty_core::ItemId),
    /// Go to this worker's tiles, or give it a shell when it has none.
    Worker(slopty_client::layout::WorkerKey),
    /// Wake this worker from sleep, as the app offers for it.
    Wake(slopty_client::layout::WorkerKey),
    /// Open a file tile for the path the field holds (relative to the active shell, `~` the
    /// worker's home), landing on `line`.
    OpenFile {
        /// As typed, with any `:line` suffix removed.
        path: String,
        /// The `:line` suffix, 1-based.
        line: Option<u32>,
        /// The worker's file search found it, so it is a file and opens without asking the
        /// worker what it is.
        found: bool,
    },
    /// Open a folder tile at the directory the field holds.
    OpenFolder {
        /// As typed, without its trailing slash.
        path: String,
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
    /// Show this project's board in its orchestrator's tile.
    Project(slopty_proto::project::ProjectId),
    /// Go to the workspace that last held this project's tiles, and to its first tile there.
    Group(slopty_client::layout::GroupKey),
    /// Open again the tile closed as this closing (the workspace's count of them).
    Reopen(u64),
    /// Open this thread at the turn where its words were found.
    Thread {
        /// The thread.
        thread: slopty_proto::thread::ThreadId,
        /// The turn the words were said in.
        turn: slopty_proto::thread::TurnId,
    },
}

impl PaletteRun {
    /// What ↩ does with a line that runs this, as the palette's foot says it.
    #[must_use]
    pub const fn verb(&self) -> &'static str {
        match self {
            Self::Action(_) | Self::Wake(_) => "Run",
            Self::Session(_)
            | Self::Item(_)
            | Self::Worker(_)
            | Self::Project(_)
            | Self::Group(_)
            | Self::Thread { .. } => "Go to",
            Self::OpenFile { .. }
            | Self::OpenFolder { .. }
            | Self::OpenShell { .. }
            | Self::OpenAgent { .. }
            | Self::OpenUrl(_)
            | Self::OpenInTile(_) => "Open",
            Self::Reopen(_) => "Reopen",
        }
    }
}

impl Clone for PaletteRun {
    fn clone(&self) -> Self {
        match self {
            Self::Action(action) => Self::Action(action.boxed_clone()),
            Self::Session(session) => Self::Session(*session),
            Self::Item(item) => Self::Item(*item),
            Self::Worker(worker) => Self::Worker(*worker),
            Self::Wake(worker) => Self::Wake(*worker),
            Self::OpenFile { path, line, found } => {
                Self::OpenFile { path: path.clone(), line: *line, found: *found }
            }
            Self::OpenFolder { path } => Self::OpenFolder { path: path.clone() },
            Self::OpenShell { cwd } => Self::OpenShell { cwd: cwd.clone() },
            Self::OpenAgent { cwd } => Self::OpenAgent { cwd: cwd.clone() },
            Self::OpenUrl(url) => Self::OpenUrl(url.clone()),
            Self::OpenInTile(url) => Self::OpenInTile(url.clone()),
            Self::Project(project) => Self::Project(project.clone()),
            Self::Group(group) => Self::Group(group.clone()),
            Self::Reopen(closing) => Self::Reopen(*closing),
            Self::Thread { thread, turn } => Self::Thread { thread: *thread, turn: *turn },
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
            Self::Wake(worker) => f.debug_tuple("Wake").field(worker).finish(),
            Self::OpenFile { path, line, found } => f
                .debug_struct("OpenFile")
                .field("path", path)
                .field("line", line)
                .field("found", found)
                .finish(),
            Self::OpenFolder { path } => f.debug_struct("OpenFolder").field("path", path).finish(),
            Self::OpenShell { cwd } => f.debug_struct("OpenShell").field("cwd", cwd).finish(),
            Self::OpenAgent { cwd } => f.debug_struct("OpenAgent").field("cwd", cwd).finish(),
            Self::OpenUrl(url) => f.debug_tuple("OpenUrl").field(url).finish(),
            Self::OpenInTile(url) => f.debug_tuple("OpenInTile").field(url).finish(),
            Self::Project(project) => f.debug_tuple("Project").field(project).finish(),
            Self::Group(group) => f.debug_tuple("Group").field(group).finish(),
            Self::Reopen(closing) => f.debug_tuple("Reopen").field(closing).finish(),
            Self::Thread { thread, turn } => {
                f.debug_struct("Thread").field("thread", thread).field("turn", turn).finish()
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
    /// Going to a project: the workspace its tiles are in.
    Projects,
    /// Going to a tile: a session, a file tile, a named tile, a find's hit.
    Tiles,
    /// Going to a worker.
    Workers,
    /// An action, a command to run again, a forwarded port.
    Commands,
    /// A path: typed into the field, or found by the worker.
    Files,
    /// A thread where the field's words were said, found by its worker.
    Threads,
}

impl Section {
    /// The heading over its lines.
    #[must_use]
    pub const fn heading(self) -> &'static str {
        match self {
            Self::Projects => "Projects",
            Self::Tiles => "Tiles",
            Self::Workers => "Machines",
            Self::Commands => "Commands",
            Self::Files => "Files",
            Self::Threads => "Threads",
        }
    }

    const fn slug(self) -> &'static str {
        match self {
            Self::Projects => "projects",
            Self::Tiles => "tiles",
            Self::Workers => "workers",
            Self::Commands => "commands",
            Self::Files => "files",
            Self::Threads => "threads",
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
    /// The mark in the leading slot: what a thing is (a tile, a machine, a file, an agent's own
    /// mark). A command has none: it is its words (`docs/decisions/ui.md`, "A command is its
    /// words").
    pub icon: Option<Mark>,
    /// How the tile or worker is doing: while it is not idle its mark ends the line, and its
    /// word is read with the line.
    pub status: Option<Status>,
    /// The worker the tile is on, named only when more than one is known.
    pub worker: Option<String>,
    /// Where the tile is: its directory, as the headers print it.
    pub cwd: Option<String>,
    /// What the tile's header places its title by (a file's folder, a page's address), after the
    /// directory.
    pub place: Option<String>,
    /// How long the tile's session has run.
    pub age: Option<Duration>,
    /// What the tile is about beyond its title (an agent's first prompt and its last answer):
    /// a query finds the line by it, and the line never prints it.
    pub about: Option<String>,
    /// The group it is listed under.
    pub section: Section,
}

impl PaletteItem {
    const fn line(
        label: String,
        keys: String,
        run: PaletteRun,
        icon: Option<Mark>,
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
            about: None,
            section,
        }
    }

    /// Whether its right-hand text is a key chord (an action's binding), not a readout.
    #[must_use]
    pub const fn is_chord(&self) -> bool {
        matches!(self.run, PaletteRun::Action(_))
    }

    /// A command for `action`, its keys read from `bindings` (the first binding for it). It
    /// is its words: no icon, unless it lists a thing ([`Self::with_icon`]).
    #[must_use]
    pub fn new(label: &str, action: Box<dyn Action>, bindings: &[KeyBinding]) -> Self {
        let keys = keys_for(action.as_ref(), bindings);
        Self::line(label.to_owned(), keys, PaletteRun::Action(action), None, Section::Commands)
    }

    /// A line that goes to a session in the workspace, by its title.
    #[must_use]
    pub fn session(title: &str, session: SessionId) -> Self {
        Self::line(
            title.to_owned(),
            String::new(),
            PaletteRun::Session(session),
            Some(Mark::Symbol(Symbol::Terminal)),
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
            Some(Mark::Symbol(Symbol::ServerRack)),
            Section::Workers,
        )
    }

    /// `Reopen <title>` for a tile closed as closing `seq`, the latest first.
    #[must_use]
    pub fn reopen(title: &str, seq: u64) -> Self {
        let run = PaletteRun::Reopen(seq);
        Self::line(format!("Reopen {title}"), String::new(), run, None, Section::Commands)
    }

    /// `Wake <worker>` for a worker that sleeps.
    #[must_use]
    pub fn wake(name: &str, worker: slopty_client::layout::WorkerKey) -> Self {
        let run = PaletteRun::Wake(worker);
        Self::line(format!("Wake {name}"), String::new(), run, None, Section::Commands)
    }

    /// A project by its title: ↩ shows its board.
    #[must_use]
    pub fn project(title: &str, project: slopty_proto::project::ProjectId) -> Self {
        let run = PaletteRun::Project(project);
        Self::line(
            title.to_owned(),
            String::new(),
            run,
            Some(Mark::Symbol(Symbol::RectangleSplit3x1)),
            Section::Tiles,
        )
    }

    /// A project by its name, its glyph saying what it was found by: ↩ goes to its workspace.
    #[must_use]
    pub fn group(
        name: &str,
        glyph: impl Into<Mark>,
        group: slopty_client::layout::GroupKey,
    ) -> Self {
        let run = PaletteRun::Group(group);
        Self::line(name.to_owned(), String::new(), run, Some(glyph.into()), Section::Projects)
    }

    /// An item in the workspace by its title; its icon says what it is.
    #[must_use]
    pub fn item(title: &str, icon: impl Into<Mark>, item: slopty_core::ItemId) -> Self {
        let run = PaletteRun::Item(item);
        Self::line(title.to_owned(), String::new(), run, Some(icon.into()), Section::Tiles)
    }

    /// A thread where the field's words were said, by its title and its agent's mark, with the
    /// words round the match after the title: going to it opens the thread at that turn.
    #[must_use]
    pub fn thread_hit(
        title: &str,
        agent: Option<&str>,
        said: &str,
        thread: slopty_proto::thread::ThreadId,
        turn: slopty_proto::thread::TurnId,
    ) -> Self {
        let icon = agent.map_or_else(|| icons::AGENT.into(), Mark::agent);
        let run = PaletteRun::Thread { thread, turn };
        Self::line(title.to_owned(), String::new(), run, Some(icon), Section::Threads)
            .placed(Some(said.to_owned()))
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
        Self::line(label.to_owned(), detail.to_owned(), run, None, Section::Commands)
    }

    /// A line that opens `url` in a browser tile, `detail` on the right.
    #[must_use]
    pub fn in_tile(label: &str, detail: &str, url: &str) -> Self {
        let run = PaletteRun::OpenInTile(url.to_owned());
        Self::line(label.to_owned(), detail.to_owned(), run, None, Section::Commands)
    }

    /// `Open <relative>` for a file the worker found under `root`.
    #[must_use]
    pub fn found_file(root: &str, relative: &str) -> Self {
        let path = format!("{}/{relative}", root.trim_end_matches('/'));
        let glyph = icons::file_symbol(&path);
        let run = PaletteRun::OpenFile { path, line: None, found: true };
        Self::line(
            format!("Open {relative}"),
            String::new(),
            run,
            Some(Mark::Symbol(glyph)),
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

    /// `Open <path>` for a path typed into the field, `line N` on the right when it names one.
    #[must_use]
    pub fn open_file(path: &str, line: Option<u32>) -> Self {
        let keys = line.map(|n| format!("line {n}")).unwrap_or_default();
        let run = PaletteRun::OpenFile { path: path.to_owned(), line, found: false };
        Self::line(
            format!("Open {path}"),
            keys,
            run,
            Some(Mark::Symbol(icons::file_symbol(path))),
            Section::Files,
        )
    }

    /// `Open folder <dir>` for a directory typed into the field: a folder tile there.
    #[must_use]
    pub fn open_folder(path: &str) -> Self {
        let run = PaletteRun::OpenFolder { path: path.to_owned() };
        let label = format!("Open folder {path}");
        Self::line(label, String::new(), run, Some(Mark::Symbol(Symbol::Folder)), Section::Files)
    }

    /// `New terminal in <dir>` for a directory typed into the field.
    #[must_use]
    pub fn open_shell(cwd: &str) -> Self {
        let run = PaletteRun::OpenShell { cwd: cwd.to_owned() };
        let label = format!("New terminal in {cwd}");
        Self::line(label, String::new(), run, None, Section::Files)
    }

    /// `New agent in <dir>` for a directory typed into the field.
    #[must_use]
    pub fn open_agent(cwd: &str) -> Self {
        let run = PaletteRun::OpenAgent { cwd: cwd.to_owned() };
        let label = format!("New agent in {cwd}");
        Self::line(label, String::new(), run, None, Section::Files)
    }

    /// The same line with another kind icon, or an agent's mark.
    #[must_use]
    pub fn with_icon(mut self, icon: impl Into<Mark>) -> Self {
        self.icon = Some(icon.into());
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
    pub const fn aged(mut self, age: Option<Duration>) -> Self {
        self.age = age;
        self
    }

    /// The same line, found as well by what its tile is `about`.
    #[must_use]
    pub fn about(mut self, about: Option<String>) -> Self {
        self.about = about;
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
    /// A tile says its status word while something happens there, so a scan down the right
    /// edge reads what needs you, failed, works or waits; a finish not yet seen is the leading
    /// slot's accent dot, and rest is nothing. Otherwise how long it has run, once that is
    /// past a minute ("now" on every fresh tile is noise). Anything else says its keys or
    /// readout.
    #[must_use]
    pub fn trailing(&self) -> Option<(String, Option<Status>)> {
        if matches!(self.section, Section::Tiles | Section::Projects)
            && let Some(status) = self.status.filter(|s| !matches!(s, Status::Idle | Status::Done))
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
pub(crate) fn brief<T: Listed>(items: Vec<T>, recent: &[String]) -> Vec<T> {
    let (commands, mut out): (Vec<T>, Vec<T>) =
        items.into_iter().partition(|item| item.item().section == Section::Commands);
    let mut chosen: Vec<T> = recent
        .iter()
        .filter_map(|label| commands.iter().copied().find(|item| &item.item().label == label))
        .collect();
    for item in commands.iter().copied().filter(|item| item.item().is_chord()) {
        if chosen.len() >= RECENT_COMMANDS {
            break;
        }
        if !chosen.iter().any(|c| std::ptr::eq(c.item(), item.item())) {
            chosen.push(item);
        }
    }
    chosen.truncate(RECENT_COMMANDS);
    out.extend(chosen);
    out
}

/// The heading a line is listed under, and its selector's slug: its section's, except that a
/// command in `recent` (the history, while an empty field lists it) sits under *Recent*.
fn group(item: &PaletteItem, recent: &[String]) -> (&'static str, &'static str) {
    if item.section == Section::Commands && recent.contains(&item.label) {
        (RECENT, "recent")
    } else {
        (item.section.heading(), item.section.slug())
    }
}

/// `items` in the order they are shown and stepped through: grouped by section, the order
/// within each kept. The files come first when `path_first` (the field spells a path).
#[must_use]
pub(crate) fn in_sections<T: Listed>(items: Vec<T>, path_first: bool) -> Vec<T> {
    let rank = |section: Section| match section {
        Section::Files if path_first => 0,
        Section::Projects => 1,
        Section::Tiles => 2,
        Section::Workers => 3,
        Section::Commands => 4,
        Section::Files => 5,
        Section::Threads => 6,
    };
    let mut items = items;
    items.sort_by_key(|item| rank(item.item().section));
    items
}

/// A line as [`brief`] and [`in_sections`] order it: the item, or the item with where the
/// palette keeps it.
pub(crate) trait Listed: Copy {
    /// The line's item.
    fn item(&self) -> &PaletteItem;
}

impl Listed for &PaletteItem {
    fn item(&self) -> &PaletteItem {
        self
    }
}

impl Listed for (At, &PaletteItem) {
    fn item(&self) -> &PaletteItem {
        self.1
    }
}

/// Where a match is kept: among the lines a typed path makes, the palette's own items, or the
/// files the worker found.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum At {
    Path(usize),
    Item(usize),
    Found(usize),
    Thread(usize),
}

/// One line of the list: a group's heading, or the match at that place in the matches.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Line {
    Heading { heading: &'static str, slug: &'static str },
    Match(usize),
}

/// The quiet label over a group of rows ([`crate::kit::label`]) on the one edge grid: the
/// palette's sections, the pickers', the inbox's, the empty workspace's workers. A heading, not
/// a row, to a screen reader.
///
/// Sections part by space, not rules: every head but the list's `first` stands a large step
/// below the rows before it, so a group reads as one before its head is read.
pub(crate) fn section_heading(
    theme: &Theme,
    id: ElementId,
    text: impl Into<SharedString>,
    first: bool,
) -> gpui::Stateful<gpui::Div> {
    let text = text.into();
    let above = if first { theme.spacing.sm } else { theme.spacing.lg };
    let label =
        crate::kit::typed(crate::kit::label(theme, text.clone()), theme.roles().section, 1.0);
    crate::kit::inset_x(label, theme)
        .id(id)
        .role(gpui::accesskit::Role::Heading)
        .aria_label(text)
        .pt(px(above))
        .pb(px(theme.spacing.xs))
}

/// The fixed square a row's kind icon sits in, so every title starts on one edge.
pub(crate) fn icon_slot(theme: &Theme, name: Symbol, color: gpui::Hsla) -> gpui::Div {
    div()
        .flex_none()
        .size(px(theme.typography.icon()))
        .flex()
        .items_center()
        .justify_center()
        .child(icons::icon(theme, name, IconSize::Inline, color))
}

/// A row's leading slot, one fixed square for every list (the palette, the pickers, the
/// navigator, the tile headers): what the row is, its kind's symbol or its agent's own mark.
/// It never changes while the row lives, so every title starts on one edge and the eye finds
/// the same agent in the same column; how the row is doing is said at its line's end
/// ([`icons::status_mark`]), never on the mark (`docs/decisions/brand.md`, "Each agent wears
/// its owner's mark"). `k` is the chrome's zoom (a tile header's in the overview); a list
/// passes 1.
///
/// Every mark is drawn at the row title's size in the whole slot ([`IconSize::Lead`]), at the
/// regular weight beside a regular title ([`lead_slot_weighted`] for the others). An agent's
/// mark names its agent to a screen reader.
pub(crate) fn lead_slot(
    theme: &Theme,
    mark: impl Into<Mark>,
    ink: gpui::Hsla,
    k: f32,
) -> gpui::Stateful<gpui::Div> {
    lead_slot_weighted(theme, mark, icons::Weight::Regular, ink, k)
}

/// [`lead_slot`] beside a title of another weight: a symbol at `weight`, the title's
/// ([`icons::weight_beside`]), as a group's semibold name or a tile's medium title.
pub(crate) fn lead_slot_weighted(
    theme: &Theme,
    mark: impl Into<Mark>,
    weight: icons::Weight,
    ink: gpui::Hsla,
    k: f32,
) -> gpui::Stateful<gpui::Div> {
    let mark = mark.into();
    let large = px(IconSize::Lead.slot(theme) * k);
    let drawn = icons::Drawn::new(theme, mark, IconSize::Lead).weight(weight).slot(large, ink);
    div()
        .id("lead")
        .flex_none()
        .size(large)
        .flex()
        .items_center()
        .justify_center()
        .when_some(mark.label(), |el, label| {
            el.role(gpui::accesskit::Role::Image).aria_label(label)
        })
        .child(drawn)
}

/// A status slot holding nothing: a command's, beside lines that lead with a mark.
fn empty_slot(theme: &Theme) -> gpui::Div {
    div().flex_none().size(px(theme.typography.icon_large()))
}

/// An age as the inbox, the palette and the navigator print it: `now` under a minute, then
/// whole minutes, hours and days, the one unit that matters.
#[must_use]
pub fn age_label(age: Duration) -> String {
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
        "delete" => out.push('⌦'),
        "pageup" => out.push('⇞'),
        "pagedown" => out.push('⇟'),
        "home" => out.push('↖'),
        "end" => out.push('↘'),
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
/// A directory — a slash at the end, spelled from the worker's root or home — offers a folder
/// tile, a shell and a conversation there; anything else with the shape of a path opens as a file
/// tile. A relative directory is a file line: the worker resolves a file against the active shell,
/// but a shell has to know its directory from the start.
#[must_use]
pub fn path_items(query: &str) -> Vec<PaletteItem> {
    if commands_only(query).is_some() {
        return Vec::new();
    }
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
        return vec![
            PaletteItem::open_folder(cwd),
            PaletteItem::open_shell(cwd),
            PaletteItem::open_agent(cwd),
        ];
    }
    vec![PaletteItem::open_file(&path, line)]
}

/// The query worth asking the worker's files for: one word of two characters or more that is
/// not a path already spelled from its root (`/…`, `~…`, `.…`).
#[must_use]
pub fn files_query(query: &str) -> Option<&str> {
    let word = query.trim();
    let rooted = word.starts_with('/') || word.starts_with('~') || word.starts_with('.');
    if commands_only(word).is_some() {
        return None;
    }
    (word.chars().count() >= 2 && !word.contains(char::is_whitespace) && !rooted).then_some(word)
}

/// The items `query` keeps, in their order.
///
/// Every word of the query is found, fuzzily ([`crate::fuzzy`]), in the label or in where the
/// line is (its worker and directory); an empty query keeps all. So a worker's name finds its
/// tiles, a directory the shells in it, and "nwt" "New terminal".
#[must_use]
pub fn filter<'a>(query: &str, items: &'a [PaletteItem]) -> Vec<&'a PaletteItem> {
    let mut fuzzy = Fuzzy::new(query);
    items.iter().filter(|item| fuzzy.rank(&item.label, &place(item)).is_some()).collect()
}

/// The rest of a query that asks for commands only: what follows a leading `>`, as in VS Code's
/// and T3 Code's palettes.
#[must_use]
pub fn commands_only(query: &str) -> Option<&str> {
    query.trim_start().strip_prefix('>')
}

/// Where a line is, for the query's words that its label lacks: its worker and directory,
/// and what its tile is about.
fn place(item: &PaletteItem) -> String {
    let about = item.about.as_deref().unwrap_or_default();
    format!("{} {about}", item.context())
}

/// What the palette decided.
#[derive(Debug)]
pub enum PaletteEvent {
    /// The field changed; the workspace asks the worker for the files it names.
    Changed(String),
    /// Run this, once the palette is gone and the focus is back.
    Run(PaletteRun),
    /// Esc, or a click outside.
    Dismiss,
}

/// What a step makes of the text typed in its field ([`CommandPalette::set_typed`]).
type TypedLines = dyn Fn(&str) -> Vec<PaletteItem>;

/// The palette: a field and the items that match it.
pub struct CommandPalette {
    items: Vec<PaletteItem>,
    /// Where each item is ([`place`]), worked out once.
    places: Vec<String>,
    /// `Open <path>` when the field spells a path; recomputed on every change.
    path_items: Vec<PaletteItem>,
    /// What a step makes of typed text in place of [`path_items`]: the folder step's typed
    /// folder, for one.
    typed: Option<Rc<TypedLines>>,
    /// `Open <path>` for the files the worker found for the field's text; dropped on a change.
    found: Vec<PaletteItem>,
    /// The threads the workers found the field's words in, listed last; dropped on a change.
    threads: Vec<PaletteItem>,
    input: Entity<InputState>,
    /// What its whole surface tracks: Tab stays inside it ([`crate::a11y::trap`]).
    scope: FocusHandle,
    /// The matches ([`Self::matches`]), worked out when the field, the path lines or the found
    /// files change, never for a frame alone: the plate's glide draws many.
    matched: Vec<At>,
    /// Whether any line matched leads with a mark, so a command keeps an empty slot.
    marked: bool,
    /// The list's lines: the matches, a heading where a group starts.
    lines: Vec<Line>,
    /// What the query matched in each match's label, as byte ranges, by its place in the
    /// matches: what its row highlights.
    marks: Vec<Vec<std::ops::Range<usize>>>,
    /// Which match ↑/↓ have selected.
    selected: usize,
    /// The list's scroll and its lines' heights, so the selected row can be brought into view
    /// and only the lines in view are drawn.
    list: ListState,
    /// The selection moved (a step, a new query): the next frame scrolls to it.
    reveal: bool,
    /// Whether key chords are worth printing: not on a touch device with no keyboard, where
    /// no chord can be pressed.
    chords: bool,
    /// An empty field lists the tiles, the workers and a few commands ([`brief`]), not every
    /// line: the workspace's own palette. A list of workers, ports or hits shows all of it.
    brief: bool,
    /// Its lines are the workspace's own list ([`Self::set_live`]), so they follow it while
    /// it is open; a find or a list of workers keeps the lines it was opened with.
    live: bool,
    /// A window narrower than this (a phone's) gets the palette as a sheet from the bottom.
    sheet_below: f32,
    /// The last frame drew it as a phone's sheet.
    sheet: bool,
    /// Opened by the pointer, so it fades in. What the keyboard opens, and a palette nobody
    /// said was the pointer's, arrives in its first frame whole: frame one is the frame a
    /// draw from scratch gives.
    fades_in: bool,
    /// It was dismissed or chose, and draws its way out until its owner drops it.
    leaving: bool,
    /// The fill under the selected line.
    plate: Plate,
    /// What the list says while it has no line: that nothing matches, unless its owner says
    /// otherwise ([`Self::set_empty`]).
    empty: SharedString,
    theme: Theme,
    _events: Subscription,
    /// How many times the matches were worked out, and how many rows were drawn.
    counts: Cell<(usize, usize)>,
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
        Self::with_field(items, "Type a command", theme, window, cx)
    }

    /// The palette as one step of a choice (an agent, a machine, a folder): only `items`, its
    /// field saying what is chosen.
    pub fn pick_step(
        items: Vec<PaletteItem>,
        placeholder: &'static str,
        theme: Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::with_field(items, placeholder, theme, window, cx)
    }

    fn with_field(
        items: Vec<PaletteItem>,
        placeholder: &'static str,
        theme: Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        let events = cx.subscribe(&input, |this, _input, event, cx| match event {
            InputEvent::Change => this.changed(cx),
            InputEvent::PressEnter { .. } => this.run(cx),
            InputEvent::Focus | InputEvent::Blur => {}
        });
        let places = items.iter().map(place).collect();
        let mut palette = Self {
            items,
            places,
            path_items: Vec::new(),
            typed: None,
            found: Vec::new(),
            threads: Vec::new(),
            input,
            scope: cx.focus_handle(),
            matched: Vec::new(),
            marked: false,
            lines: Vec::new(),
            marks: Vec::new(),
            selected: 0,
            list: ListState::new(0, ListAlignment::Top, px(OVERDRAW)).measure_all(),
            reveal: false,
            chords: true,
            brief: false,
            live: false,
            sheet_below: 0.0,
            sheet: false,
            fades_in: false,
            leaving: false,
            plate: Plate::default(),
            empty: NO_COMMAND_MATCHES.into(),
            theme,
            _events: events,
            counts: Cell::default(),
        };
        palette.refresh(cx);
        palette
    }

    /// Make typed text into `lines` in this step, in place of the path lines: the folder step
    /// takes a folder typed from its root, for one.
    pub fn set_typed(&mut self, lines: impl Fn(&str) -> Vec<PaletteItem> + 'static, cx: &App) {
        self.typed = Some(Rc::new(lines));
        let text = self.input.read(cx).value().to_string();
        self.path_items = self.typed_lines(&text);
        self.refresh(cx);
    }

    /// The lines typed `text` adds: the step's own, else the path lines.
    fn typed_lines(&self, text: &str) -> Vec<PaletteItem> {
        self.typed.as_ref().map_or_else(|| path_items(text), |lines| lines(text))
    }

    /// What the list says while it has no line: what its lines wait on, or why there are none.
    pub fn set_empty(&mut self, words: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.empty = words.into();
        cx.notify();
    }

    /// List only the tiles, the workers and a few commands while the field is empty.
    pub fn set_brief(&mut self, brief: bool, cx: &App) {
        self.brief = brief;
        self.refresh(cx);
    }

    /// Whether its lines are the workspace's own list, which [`Self::set_items`] refreshes
    /// while it is open.
    pub const fn set_live(&mut self, live: bool) {
        self.live = live;
    }

    /// Whether its lines follow the workspace's.
    #[must_use]
    pub const fn is_live(&self) -> bool {
        self.live
    }

    /// Replace the lines it lists, as what they are drawn from changes while it is open (a
    /// worker's agents arriving): the field keeps what was typed, and the selected line stays
    /// selected wherever it lands.
    pub fn set_items(&mut self, items: Vec<PaletteItem>, cx: &mut Context<Self>) {
        let chosen = self
            .matched
            .get(self.selected(self.matched.len()))
            .and_then(|at| self.at(*at))
            .map(|item| item.label.clone());
        self.places = items.iter().map(place).collect();
        self.items = items;
        self.refresh(cx);
        if let Some(label) = chosen
            && let Some(ix) =
                self.matched.iter().position(|at| self.at(*at).is_some_and(|i| i.label == label))
        {
            self.selected = ix;
        }
        cx.notify();
    }

    /// Whether it fades in as it arrives: opened by the pointer. Opened by a key it arrives
    /// whole.
    pub const fn set_fades_in(&mut self, fades_in: bool) {
        self.fades_in = fades_in;
    }

    /// Whether it fades in as it arrives.
    #[cfg(test)]
    pub(crate) const fn fades_in(&self) -> bool {
        self.fades_in
    }

    /// Show as a sheet from the bottom in a window narrower than `width`.
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
        self.path_items = self.typed_lines(text);
        self.refresh(cx);
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
        self.refresh(cx);
        cx.notify();
    }

    /// The threads the workers found the field's words in, for `query`: the Threads section,
    /// last in the list, while the field still says `query`. The line the person is on stays
    /// selected: a line above the section keeps its place, since the section comes last, and a
    /// thread's line is found again by where it goes, wherever a later answer puts it.
    pub fn set_threads(&mut self, query: &str, lines: Vec<PaletteItem>, cx: &mut Context<Self>) {
        if self.input.read(cx).value().trim() != query {
            return;
        }
        let on = self.selected(self.matched.len());
        let goes = |at: &At, this: &Self| match at {
            At::Thread(ix) => this.threads.get(*ix).and_then(|item| match item.run {
                PaletteRun::Thread { thread, turn } => Some((thread, turn)),
                _ => None,
            }),
            _ => None,
        };
        let chosen = self.matched.get(on).and_then(|at| goes(at, self));
        self.threads = lines;
        self.refresh(cx);
        self.selected = match chosen {
            Some(chosen) => self
                .matched
                .iter()
                .position(|at| goes(at, self) == Some(chosen))
                .unwrap_or_else(|| self.matched.len().saturating_sub(self.threads.len())),
            None => on,
        };
        cx.notify();
    }

    /// The items matching the field, in the order they are shown, group by group: a path
    /// typed into it (`Open <path>`, or a shell and a conversation in a directory) first, then
    /// the tiles, the workers and the commands the text matches, then the files the worker
    /// found for it.
    ///
    /// A brief palette with nothing typed lists only the tiles, the workers and the commands
    /// run last, then the first of the rest.
    #[must_use]
    pub fn matches(&self) -> Vec<&PaletteItem> {
        self.matched.iter().filter_map(|at| self.at(*at)).collect()
    }

    /// The match kept at `at`.
    fn at(&self, at: At) -> Option<&PaletteItem> {
        match at {
            At::Path(ix) => self.path_items.get(ix),
            At::Item(ix) => self.items.get(ix),
            At::Found(ix) => self.found.get(ix),
            At::Thread(ix) => self.threads.get(ix),
        }
    }

    /// Work out the matches and the lines again, after the field, the path lines, the found
    /// files or the brevity changed.
    fn refresh(&mut self, cx: &App) {
        let (refreshed, rows) = self.counts.get();
        self.counts.set((refreshed.saturating_add(1), rows));
        let field = self.input.read(cx).value();
        let only_commands = commands_only(&field);
        let query = only_commands.unwrap_or(&field);
        let empty = query.trim().is_empty();
        let mut fuzzy = Fuzzy::new(query);
        let mut out: Vec<(At, &PaletteItem)> =
            self.path_items.iter().enumerate().map(|(ix, item)| (At::Path(ix), item)).collect();
        let path_first = !out.is_empty();
        let mut kept: Vec<(At, &PaletteItem, Rank)> = self
            .items
            .iter()
            .zip(&self.places)
            .enumerate()
            .filter(|(_, (item, _))| only_commands.is_none() || item.section == Section::Commands)
            .filter_map(|(ix, (item, place))| {
                Some((At::Item(ix), item, fuzzy.rank(&item.label, place)?))
            })
            .collect();
        // An empty field's commands are the ones run last, then a few more: the ones from
        // history sit under a heading of their own, so the list says why they are there.
        let recent = recent_commands(cx);
        let found = self.found.iter().enumerate().map(|(ix, item)| (At::Found(ix), item));
        // Last of all, so a thread found while the person moves through the list never moves
        // the line they are on.
        let threads = self.threads.iter().enumerate().map(|(ix, item)| (At::Thread(ix), item));
        let out = if empty {
            let kept = kept.into_iter().map(|(at, item, _)| (at, item));
            if self.brief {
                out.extend(brief(kept.collect(), &recent));
            } else {
                out.extend(kept);
            }
            out.extend(found);
            in_sections(out, path_first)
        } else {
            // The best line leads its group and the group with the best line leads the list;
            // a command run lately, then the order the lines came in, breaks a tie.
            let lately = |item: &PaletteItem| {
                recent.iter().position(|r| *r == item.label).unwrap_or(usize::MAX)
            };
            kept.sort_by(|a, b| best_first(&a.2, &b.2).then_with(|| lately(a.1).cmp(&lately(b.1))));
            let mut sections: Vec<Section> = Vec::new();
            for (_, item, _) in &kept {
                if !sections.contains(&item.section) {
                    sections.push(item.section);
                }
            }
            kept.sort_by_key(|(_, item, _)| sections.iter().position(|s| *s == item.section));
            out.extend(kept.into_iter().map(|(at, item, _)| (at, item)));
            out.extend(found);
            out.extend(threads);
            out
        };
        let recent = if self.brief && empty { recent } else { Vec::new() };
        let group = |item: &PaletteItem| group(item, &recent);
        // A heading only where there are two groups to tell apart: a list of workers, of
        // ports or of hits is one kind already, and the dialog's title names it.
        let grouped = out.iter().zip(out.iter().skip(1)).any(|(a, b)| group(a.1) != group(b.1));
        let mut lines = Vec::with_capacity(out.len());
        let mut section = None;
        for (ix, (_, item)) in out.iter().enumerate() {
            let (heading, slug) = group(item);
            if grouped && section != Some(slug) {
                section = Some(slug);
                lines.push(Line::Heading { heading, slug });
            }
            lines.push(Line::Match(ix));
        }
        self.marks = if empty {
            Vec::new()
        } else {
            out.iter().map(|(_, item)| fuzzy.ranges(&item.label)).collect()
        };
        let matched = out.into_iter().map(|(at, _)| at).collect();
        self.matched = matched;
        self.list.reset(lines.len());
        self.lines = lines;
    }

    /// Esc empties a field that holds text, and closes the palette once it is empty: a query
    /// typed wrong is taken back without starting over.
    fn escape(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.input.read(cx).value().is_empty() {
            cx.emit(PaletteEvent::Dismiss);
            return;
        }
        self.input.update(cx, |input, cx| input.set_value(String::new(), window, cx));
        self.changed(cx);
    }

    /// The field's text changed: the lines follow it from the first.
    fn changed(&mut self, cx: &mut Context<Self>) {
        self.selected = 0;
        self.reveal = true;
        let text = self.input.read(cx).value().to_string();
        self.path_items = self.typed_lines(&text);
        self.found.clear();
        self.threads.clear();
        self.refresh(cx);
        cx.emit(PaletteEvent::Changed(text));
        cx.notify();
    }

    /// The selected match's index, clamped to the matches.
    fn selected(&self, count: usize) -> usize {
        self.selected.min(count.saturating_sub(1))
    }

    fn run(&self, cx: &mut Context<Self>) {
        if self.leaving {
            return;
        }
        let at = self.selected(self.matched.len());
        let item = self.matched.get(at).and_then(|at| self.at(*at)).cloned();
        if let Some(item) = item {
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

    /// Whether an input method holds uncommitted text in the field (a Telex word, kana before
    /// conversion): the keys it reads then (the arrows, Tab, Esc) are its own.
    fn composing(&self, cx: &App) -> bool {
        self.input.read(cx).is_composing()
    }

    fn step(&mut self, delta: i64, cx: &mut Context<Self>) {
        let count = i64::try_from(self.matched.len()).unwrap_or(0);
        if count == 0 {
            return;
        }
        let at = i64::try_from(self.selected(usize::try_from(count).unwrap_or(0))).unwrap_or(0);
        self.selected = usize::try_from(at.saturating_add(delta).rem_euclid(count)).unwrap_or(0);
        self.reveal = true;
        cx.notify();
    }

    /// The pointer on a line selects it: the palette has one highlight, the line ↩ runs, and
    /// the pointer moves it as the arrows do (Raycast draws no hover on a list either).
    fn point_at(&mut self, ix: usize, cx: &mut Context<Self>) {
        if self.selected != ix {
            self.selected = ix;
            cx.notify();
        }
    }

    fn row(
        &self,
        ix: usize,
        item: &PaletteItem,
        chosen: bool,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let (refreshed, rows) = self.counts.get();
        self.counts.set((refreshed, rows.saturating_add(1)));
        let theme = &self.theme;
        let s = &theme.surfaces;
        let spacing = theme.spacing;
        let pad = list_pad(theme);
        let pressed = s.pressed;
        // A mark is a tier under its words, its words' tier when chosen; a machine out of reach
        // goes grey. A machine's own colour is the navigator's head's alone.
        let icon_ink = if item.status == Some(Status::Away) {
            s.text_muted
        } else if chosen {
            s.text
        } else {
            s.text_secondary
        };
        let trailing = item.trailing().filter(|_| self.chords || !item.is_chord());
        // How it is doing ends the line as its mark; its word, which the mark says, is read
        // with the line and not printed beside it.
        let state = item.status.filter(|status| *status != Status::Idle);
        let trailing = trailing.filter(|(text, tone)| !tone.is_some_and(|t| t.label() == text));
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
                context = context.child(div().text_color(separator_ink(theme)).child("·"));
            }
            context = context.child(
                // A path in the UI face, as every other context in the chrome is; mono is
                // for ports and figures.
                div()
                    .when(path, |el| el.min_w_0().overflow_hidden().text_ellipsis())
                    .when(!path, Styled::flex_none)
                    .child(SharedString::from(text)),
            );
        }
        let row = crate::kit::sheet_row(theme, crate::kit::Row::One)
            .id(ElementId::NamedInteger("palette-item".into(), u64::try_from(ix).unwrap_or(0)))
            .debug_selector(move || format!("palette-item-{ix}"))
            .role(gpui::accesskit::Role::ListBoxOption)
            .aria_label(SharedString::from(item.a11y_label()))
            .aria_selected(chosen)
            .h(px(line_height(theme)))
            // The fill sits the sheet's pad in from its edges, nested in its corners; the text
            // on the edge grid.
            .mx(px(pad))
            .cursor_pointer()
            .active(move |st| st.bg(hsla(pressed)))
            .on_mouse_move(cx.listener(move |this, _ev, _window, cx| this.point_at(ix, cx)))
            .on_mouse_down(MouseButton::Left, |_ev, _window, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _ev, _window, cx| {
                let item = this.matched.get(ix).and_then(|at| this.at(*at)).cloned();
                if let Some(item) = item.filter(|_| !this.leaving) {
                    Self::choose(&item, cx);
                }
            }))
            // A thing leads with what it is; a command is its words. While any line shown
            // leads with a mark, a command keeps the empty slot, so every title starts on one
            // edge.
            .children(match item.icon {
                None => self.marked.then(|| empty_slot(theme).into_any_element()),
                Some(icon) => Some(lead_slot(theme, icon, hsla(icon_ink), 1.0).into_any_element()),
            })
            .child(
                div()
                    .debug_selector(move || format!("palette-title-{ix}"))
                    .flex_initial()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_color(hsla(s.text))
                    .when(chosen, |el| {
                        el.font_weight(gpui::FontWeight(slopty_theme::Typography::MEDIUM_WEIGHT))
                    })
                    .child(self.label(ix, &item.label)),
            )
            .child(context)
            .children(trailing.map(|(text, tone)| {
                if item.is_chord() {
                    // Keys as plain muted glyphs, as Zed's and T3 Code's palettes print them;
                    // key caps are the foot's alone.
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
                        .when_some(tone, |el, tone| el.text_color(hsla(tone.word(theme))))
                        .child(SharedString::from(text))
                }
            }))
            .children(state.map(|status| {
                icons::status_mark(theme, Some(status), 1.0)
                    .debug_selector(move || format!("palette-status-{ix}"))
            }));
        if chosen { self.plate.mark(row, ix) } else { row }
    }

    /// A line's label: what the query matched in `text` over the rest in `text_secondary`,
    /// so the eye finds why the line is there; the whole label in `text` with nothing typed.
    fn label(&self, ix: usize, label: &str) -> gpui::StyledText {
        let s = self.theme.surfaces;
        let ranges = self.marks.get(ix).cloned().unwrap_or_default();
        let text = gpui::StyledText::new(SharedString::from(label.to_owned()));
        if ranges.is_empty() {
            return text;
        }
        let quiet =
            gpui::HighlightStyle { color: Some(hsla(s.text_secondary)), ..Default::default() };
        let mut runs = Vec::with_capacity(ranges.len().saturating_mul(2).saturating_add(1));
        let mut at = 0;
        for range in ranges {
            if range.start > at {
                runs.push((at..range.start, quiet));
            }
            at = range.end;
        }
        if label.len() > at {
            runs.push((at..label.len(), quiet));
        }
        text.with_highlights(runs)
    }

    /// Line `ix` of the list: a group's heading, or a match's row.
    fn line(&self, ix: usize, cx: &Context<Self>) -> gpui::AnyElement {
        // A list lays each line out on its own, so nothing stretches it to the list's width the
        // way a column stretches its children: the column is put back around each one.
        div().w_full().flex().flex_col().child(self.line_content(ix, cx)).into_any_element()
    }

    fn line_content(&self, ix: usize, cx: &Context<Self>) -> gpui::AnyElement {
        match self.lines.get(ix) {
            Some(Line::Heading { heading, slug }) => {
                let name = format!("palette-heading-{slug}");
                let first = ix == 0;
                section_heading(&self.theme, ElementId::Name(name.clone().into()), *heading, first)
                    .debug_selector(move || name)
                    .into_any_element()
            }
            Some(Line::Match(at)) => {
                let chosen = *at == self.selected(self.matched.len());
                match self.matched.get(*at).and_then(|place| self.at(*place)) {
                    Some(item) => self.row(*at, item, chosen, cx).into_any_element(),
                    None => gpui::Empty.into_any_element(),
                }
            }
            None => gpui::Empty.into_any_element(),
        }
    }

    /// Stop taking the keys and the pointer and draw the way out: on a desktop a fade on
    /// [`Pace::Exit`], as every overlay leaves; on a phone the sheet goes back down in three
    /// quarters of its way in. How long that takes, for the owner to keep drawing it before
    /// dropping it; nothing under Reduce Motion.
    pub fn leave(&mut self, cx: &mut Context<Self>) -> Duration {
        self.leaving = true;
        cx.notify();
        if !crate::kit::motion(cx) {
            return Duration::ZERO;
        }
        if self.sheet { sheet_leaving_time() } else { Pace::Exit.duration() }
    }
}

/// How long the phone's sheet takes to go back down: three quarters of its way in, since what is
/// dismissed is no longer looked at.
fn sheet_leaving_time() -> Duration {
    Pace::Sheet.duration().mul_f32(0.75)
}

/// The one-shot animation of the sheet leaving: [`sheet_leaving_time`] on a sheet's curve.
fn sheet_leaving() -> gpui::Animation {
    let curve = Pace::Sheet.curve();
    gpui::Animation::new(sheet_leaving_time()).with_easing(move |t| curve.at(t))
}

impl Render for CommandPalette {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme.clone();
        let chosen = self.selected(self.matched.len());
        self.marked =
            self.matched.iter().filter_map(|at| self.at(*at)).any(|item| item.icon.is_some());
        if std::mem::take(&mut self.reveal)
            && let Some(line) = self.lines.iter().position(|l| *l == Line::Match(chosen))
        {
            self.list.scroll_to_reveal_item(line);
        }
        let verb = self
            .matched
            .get(chosen)
            .and_then(|at| self.at(*at))
            .map_or(RETURN_VERB, |item| item.run.verb());
        let nothing = self.lines.is_empty();

        let viewport = window.viewport_size();
        let height = f32::from(viewport.height);
        let sheet = f32::from(viewport.width) < self.sheet_below;
        self.sheet = sheet;
        let safe_top = window.insets().effective().top;
        // A list typed at floats over the work (`kit::anchor`); only a touch screen's palette,
        // the phone's sheet or the iPad's with no keyboard, dims what it came over.
        let backdrop = crate::kit::anchor(&theme, window);
        let backdrop = if sheet {
            // A sheet from the bottom, as iOS search in a toolbar is: its field just above the
            // keyboard, in a thumb's reach, and what it finds above the field.
            backdrop.px_0().pt(safe_top + px(theme.spacing.sm)).justify_end()
        } else {
            backdrop
        };
        let dialog = crate::kit::dialog(&theme, crate::kit::Overlay::List);
        let dialog = if sheet {
            // A sheet from the bottom: the window's width, from under the status bar down to
            // the keyboard, its top corners rounded as a sheet's are. The scrim sets it off, so
            // it casts no shadow into the keyboard's grey.
            dialog
                .max_w_full()
                .max_h_full()
                .flex_1()
                .mb_0()
                .border_b_0()
                .border_x_0()
                .rounded_b(px(0.0))
                .shadow_none()
        } else {
            let ceiling = crate::kit::Overlay::List.bounds().1;
            dialog.max_h(px(ceiling.min(height * SHARE)))
        };
        // Glass has no Esc: the field ends in Cancel, as iOS search does.
        let cancel = (!self.chords).then(|| {
            crate::kit::button(&theme, "palette-cancel", "Cancel", crate::kit::ButtonKind::Link)
                .on_click(cx.listener(|_this, _ev, _window, cx| cx.emit(PaletteEvent::Dismiss)))
        });
        // The typed text starts on the rows' text: the edge grid.
        let field = field_row(&theme, &self.input, "Command")
            .debug_selector(|| "palette-field".to_owned())
            .gap(px(theme.spacing.md))
            .children(cancel);
        let list = more_below(
            &theme,
            &self.list,
            div()
                .relative()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .child(self.plate.under(&theme))
                .child(
                    div()
                        .id("palette-list")
                        .debug_selector(|| "palette-list".to_owned())
                        .role(gpui::accesskit::Role::ListBox)
                        .aria_label("Commands")
                        .flex_1()
                        .min_h_0()
                        .flex()
                        .flex_col()
                        .when(!self.lines.is_empty(), |el| {
                            el.child(
                                gpui::list(
                                    self.list.clone(),
                                    cx.processor(|this, ix, _window, cx| this.line(ix, cx)),
                                )
                                .with_sizing_behavior(ListSizingBehavior::Infer)
                                .flex_1()
                                .min_h_0()
                                .py(px(list_pad(&theme))),
                            )
                        })
                        .when(nothing, |el| {
                            el.py(px(list_pad(&theme))).child(quiet_line(
                                &theme,
                                "palette-empty",
                                self.empty.clone(),
                            ))
                        }),
                ),
        );
        let foot = self.chords.then(|| legend(&theme, verb));
        let panel = dialog
            .id("palette")
            .debug_selector(|| "palette".to_owned())
            .role(gpui::accesskit::Role::Dialog)
            .aria_label("Commands")
            .map(|el| {
                if sheet {
                    el.child(list).children(foot).child(field)
                } else {
                    el.child(field).child(list).children(foot)
                }
            });
        // The phone's sheet dims what it came down over; on glass anywhere the dim is also what a
        // finger taps to close it, where a desktop's Esc would.
        let scrim = (sheet || !self.chords).then(|| {
            div()
                .debug_selector(|| "palette-scrim".to_owned())
                .absolute()
                .inset_0()
                .bg(crate::kit::scrim(&theme))
        });
        let (panel, scrim) = if self.leaving {
            (leave_panel(panel, sheet, cx), scrim.map(|scrim| leave_scrim(scrim, cx)))
        } else {
            let panel = if self.fades_in || sheet {
                enter_panel(panel, sheet, cx)
            } else {
                panel.into_any_element()
            };
            (panel, scrim.map(|scrim| enter_scrim(scrim, cx)))
        };
        let root = backdrop.id("palette-backdrop").children(scrim).child(panel);
        let root = if self.leaving {
            root
        } else {
            let home = self.input.read(cx).focus_handle(cx);
            crate::a11y::hold(&self.scope, &home, cx);
            let root = crate::a11y::trap(root, &self.scope);
            // While an input method composes in the field, the arrows and Esc are its own.
            root.capture_action(cx.listener(|this, _: &MoveUp, _window, cx| {
                if !this.composing(cx) {
                    this.step(-1, cx);
                }
            }))
            .capture_action(cx.listener(|this, _: &MoveDown, _window, cx| {
                if !this.composing(cx) {
                    this.step(1, cx);
                }
            }))
            .capture_action(cx.listener(|this, _: &Escape, window, cx| {
                if !this.composing(cx) {
                    this.escape(window, cx);
                }
            }))
            // Tab walks the palette's stops rather than typing into its field. Off the field,
            // where the field's Esc binding does not reach, Esc closes the palette all the same.
            .capture_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, window, cx| {
                if crate::a11y::cycle(ev, window, cx) {
                    cx.stop_propagation();
                    return;
                }
                let in_field = this.input.read(cx).focus_handle(cx).is_focused(window);
                if ev.keystroke.key == "escape" && !ev.keystroke.modifiers.modified() && !in_field {
                    cx.stop_propagation();
                    cx.emit(PaletteEvent::Dismiss);
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_this, _ev, _window, cx| {
                    cx.emit(PaletteEvent::Dismiss);
                    cx.stop_propagation();
                }),
            )
        };
        // Leaving, it is drawn in its place among the workspace's overlays, under a menu opened
        // meanwhile, rather than lifted over everything: a palette fading out must never take
        // the press meant for what came after it.
        if self.leaving {
            root.into_any_element()
        } else {
            gpui::deferred(root).with_priority(Layer::Dialog.priority()).into_any_element()
        }
    }
}

/// The palette arriving. On a desktop it fades in where it stands, with no travel: it is a
/// surface for the keyboard, and what the keyboard summons does not move. The phone's sheet
/// comes up its whole height from the bottom, on a sheet's time and curve. The
/// field has the keys from the first frame either way: only paint moves.
fn enter_panel(panel: gpui::Stateful<gpui::Div>, sheet: bool, cx: &App) -> gpui::AnyElement {
    if !sheet {
        return crate::kit::fade_in(panel, "palette-open", cx);
    }
    if !crate::kit::motion(cx) {
        return panel.into_any_element();
    }
    panel
        .relative()
        .with_animation("palette-sheet-in", Pace::Sheet.animation(), |el, t| {
            el.top(gpui::relative(1.0 - t))
        })
        .into_any_element()
}

/// The palette leaving: on a desktop it fades where it is on [`Pace::Exit`], with no travel;
/// the sheet goes back down. Under Reduce Motion it is gone at once, as its owner drops it at
/// once.
fn leave_panel(panel: gpui::Stateful<gpui::Div>, sheet: bool, cx: &App) -> gpui::AnyElement {
    if !crate::kit::motion(cx) {
        return panel.opacity(0.0).into_any_element();
    }
    if sheet {
        panel
            .relative()
            .with_animation("palette-sheet-out", sheet_leaving(), |el, t| el.top(gpui::relative(t)))
            .into_any_element()
    } else {
        panel
            .with_animation("palette-close", Pace::Exit.animation(), |el, t| el.opacity(1.0 - t))
            .into_any_element()
    }
}

/// The sheet's scrim coming up with it.
fn enter_scrim(scrim: gpui::Div, cx: &App) -> gpui::AnyElement {
    if !crate::kit::motion(cx) {
        return scrim.into_any_element();
    }
    scrim
        .with_animation("palette-scrim-in", Pace::Sheet.animation(), Styled::opacity)
        .into_any_element()
}

/// The sheet's scrim going with it.
fn leave_scrim(scrim: gpui::Div, cx: &App) -> gpui::AnyElement {
    if !crate::kit::motion(cx) {
        return scrim.opacity(0.0).into_any_element();
    }
    scrim
        .with_animation("palette-scrim-out", sheet_leaving(), |el, t| el.opacity(1.0 - t))
        .into_any_element()
}

#[cfg(test)]
impl CommandPalette {
    /// How many times the matches were worked out, and how many rows were drawn: a frame of
    /// the plate's glide does neither for the whole list.
    #[must_use]
    const fn work_done(&self) -> (usize, usize) {
        self.counts.get()
    }

    /// The line the person is on.
    pub(crate) fn chosen(&self) -> Option<&PaletteItem> {
        self.matched.get(self.selected(self.matched.len())).and_then(|at| self.at(*at))
    }
}

#[cfg(test)]
impl Plate {
    /// Where the plate was drawn in the last frame.
    pub(crate) fn drawn(&self) -> Option<Bounds<Pixels>> {
        self.0.borrow().drawn
    }
}

#[cfg(test)]
mod tests {
    use gpui::{Keystroke, TestAppContext, VisualTestContext, point, size};
    use slopty_core::ItemId;

    use super::*;

    /// A row's box `y` down a list.
    fn line(y: f32) -> Bounds<Pixels> {
        Bounds::new(point(px(0.0), px(y)), size(px(320.0), px(32.0)))
    }

    /// The row `key` at `y`, as it reports itself to the plate.
    fn marked(key: u64, y: f32, seated: bool) -> Marked {
        Marked { key, bounds: line(y), seated }
    }

    /// `ms` past `t0`.
    fn after(t0: Instant, ms: u64) -> Instant {
        t0.checked_add(Duration::from_millis(ms)).expect("a test's instant")
    }

    /// The plate's top, drawn over `row` at `ms` past `t0`.
    fn top_at(glide: &mut Glide, row: (u64, f32), t0: Instant, ms: u64) -> f32 {
        glide.row = Some(marked(row.0, row.1, false));
        let drawn = glide.frame(after(t0, ms), true).expect("a row, a plate");
        f32::from(drawn.origin.y)
    }

    /// The plate appears on its first row, starts a move from where it is drawn, and lands on
    /// the settle. A move while it settles starts from where it is at that moment, never from
    /// the row it was going to, and lands in [`HELD`]; one sooner than that snaps; a scroll
    /// carries it along; and under Reduce Motion it is on the row at once.
    #[test]
    fn the_plate_retargets_from_where_it_is_and_never_queues() {
        let t0 = Instant::now();
        let mut glide = Glide::default();
        assert!(top_at(&mut glide, (1, 0.0), t0, 0).abs() < 0.01, "appears in place");
        assert!(top_at(&mut glide, (2, 32.0), t0, 1_000).abs() < 0.01, "a move starts there");
        let mid = top_at(&mut glide, (2, 32.0), t0, 1_060);
        assert!(mid > 0.0 && mid < 32.0, "on its way: {mid}");
        // A scroll of 10 during the move carries the plate the same 10.
        let scrolled = {
            glide.row = Some(marked(2, 42.0, false));
            let at = after(t0, 1_060);
            f32::from(glide.frame(at, true).expect("drawn").origin.y)
        };
        assert!((scrolled - mid - 10.0).abs() < 0.01, "carried by the scroll: {scrolled}");
        glide.row = Some(marked(2, 32.0, false));
        let _back = glide.frame(after(t0, 1_060), true);
        // ↓ again before it landed: from `mid`, straight to the third row.
        assert!((top_at(&mut glide, (3, 64.0), t0, 1_080) - mid).abs() < 0.01, "from where it is");
        let on = top_at(&mut glide, (3, 64.0), t0, 1_100);
        assert!(on > mid && on < 64.0, "towards the new row only: {on}");
        // A key repeating faster than the plate can land puts it on its row.
        assert!((top_at(&mut glide, (4, 96.0), t0, 1_110) - 96.0).abs() < 0.01, "snaps");
        // A held key's pace: a move before the settle is over lands in `HELD`.
        let held = u64::try_from(HELD.as_millis()).unwrap_or(0);
        assert!((top_at(&mut glide, (5, 128.0), t0, 1_210) - 96.0).abs() < 0.01, "held, moving");
        let landed = top_at(&mut glide, (5, 128.0), t0, 1_210 + held);
        assert!((landed - 128.0).abs() < 0.01, "held, landed");
        let settle = u64::try_from(Pace::Settle.duration().as_millis()).unwrap_or(0);
        assert!((top_at(&mut glide, (6, 160.0), t0, 3_000) - 128.0).abs() < 0.01, "a new move");
        let before = top_at(&mut glide, (6, 160.0), t0, 3_000 + held);
        assert!(before < 160.0, "not landed at the held pace: {before}");
        let settled = top_at(&mut glide, (6, 160.0), t0, 3_000 + settle);
        assert!((settled - 160.0).abs() < 0.01, "on the settle");
        glide.row = Some(marked(7, 192.0, false));
        let still = glide.frame(after(t0, 9_000), false).expect("drawn");
        assert_eq!(still, line(192.0), "under Reduce Motion, on the row at once");
        glide.row = None;
        assert_eq!(glide.frame(after(t0, 10_000), true), None, "no row, no plate");
    }

    /// A seated row paints the plate itself while it rests there, so a list on a scroll layer
    /// holds it in its content; a move in flight is painted under the rows, and the row takes
    /// it back once it lands. A row that only marks itself never paints it.
    #[test]
    fn a_seated_row_holds_the_plate_at_rest_and_the_glide_is_painted_under() {
        let t0 = Instant::now();
        let mut glide = Glide::default();
        let at = |glide: &mut Glide, row: Marked, ms: u64| {
            glide.row = Some(row);
            glide.frame(after(t0, ms), true).expect("a row, a plate");
            glide.seated
        };
        assert_eq!(at(&mut glide, marked(1, 0.0, true), 0), Some(1), "at rest on its first row");
        assert_eq!(at(&mut glide, marked(2, 32.0, true), 1_000), None, "gliding: painted under");
        let settle = u64::try_from(Pace::Settle.duration().as_millis()).unwrap_or(0);
        let landed = 1_000_u64.saturating_add(settle);
        assert_eq!(
            at(&mut glide, marked(2, 32.0, true), landed),
            Some(2),
            "landed: the row's again"
        );
        assert_eq!(
            at(&mut glide, marked(2, 32.0, false), landed),
            None,
            "a marked row never holds it"
        );
        glide.row = None;
        assert_eq!(glide.frame(after(t0, 9_000), true), None);
        assert_eq!(glide.seated, None, "no row, nothing seated");
    }

    /// A palette over `n` tiles, drawn.
    fn palette_of(
        n: usize,
        cx: &mut TestAppContext,
    ) -> (Entity<CommandPalette>, &mut VisualTestContext) {
        cx.update(gpui_kit::init);
        let items: Vec<PaletteItem> =
            (0..n).map(|i| PaletteItem::session(&format!("tile {i}"), SessionId::new())).collect();
        let (palette, cx) = cx
            .add_window_view(|window, cx| CommandPalette::new(items, Theme::default(), window, cx));
        cx.run_until_parked();
        (palette, cx)
    }

    /// A line ranks by how well the query names it ([`crate::fuzzy`]): a label spelled whole
    /// leads, and letters in order find a line by its words' heads. The group whose best line
    /// is best leads the list. `>` keeps only the commands. The label shows what matched in
    /// `text` over the rest in `text_secondary`.
    #[gpui::test]
    fn a_query_ranks_each_section_and_shows_what_it_matched(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let action = |label: &str| PaletteItem::new(label, Box::new(MoveUp), &[]);
        let items = vec![
            PaletteItem::session("Fix the page header", SessionId::new()),
            action("Open last offered page"),
            action("Edit page address"),
            action("Page back"),
            action("Pages"),
            action("Page"),
        ];
        let (palette, cx) = cx
            .add_window_view(|window, cx| CommandPalette::new(items, Theme::default(), window, cx));
        let typed = |text: &str, palette: &Entity<CommandPalette>, cx: &mut VisualTestContext| {
            palette.update_in(cx, |p, window, cx| {
                p.input.update(cx, |input, cx| input.set_value(text.to_owned(), window, cx));
                p.changed(cx);
            });
            palette.read_with(cx, |p, _| {
                p.matches().iter().map(|i| i.label.clone()).collect::<Vec<_>>()
            })
        };
        let page = typed("page", &palette, cx);
        assert_eq!(page.len(), 6, "{page:?}");
        assert_eq!(page.first().map(String::as_str), Some("Page"), "spelled whole, it leads");
        assert_eq!(
            page.last().map(String::as_str),
            Some("Fix the page header"),
            "the commands' group has the best line: the tile's comes after it"
        );
        assert_eq!(typed("pg bk", &palette, cx), ["Page back"], "letters in order, every word");
        let marks = palette.read_with(cx, |p, _| p.marks.clone());
        assert_eq!(marks, [vec![0..1, 2..3, 5..6, 8..9]], "P, g, b and k of Page back");
        let edit = typed("page", &palette, cx).iter().position(|l| l == "Edit page address");
        let marks = palette.read_with(cx, |p, _| p.marks.clone());
        let ends = edit.and_then(|ix| marks.get(ix)).map(|m| m.iter().map(|r| (r.start, r.end)));
        assert_eq!(ends.map(Iterator::collect::<Vec<_>>), Some(vec![(5, 9)]), "page in Edit page");
        assert_eq!(
            typed(">page", &palette, cx).first().map(String::as_str),
            Some("Page"),
            "commands only"
        );
        assert_eq!(typed(">", &palette, cx).len(), 5, "every command, no tile");
        assert_eq!(files_query(">ma"), None, "a command query asks for no files");
        assert!(path_items(">/srv/a/").is_empty(), "nor spells a path");
    }

    /// A list that runs past the sheet's foot fades there, per pixel, once it has laid out, and
    /// the fade goes as the list reaches its end. The sheet's surface is outside the fade, and
    /// the frame shown is what a frame from scratch paints.
    #[gpui::test]
    fn a_long_list_fades_at_its_foot_until_its_end(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_reduce_motion(true);
        });
        let items: Vec<PaletteItem> = (0..300)
            .map(|i| PaletteItem::session(&format!("tile {i}"), SessionId::new()))
            .collect();
        let (palette, cx) = cx
            .add_window_view(|window, cx| CommandPalette::new(items, Theme::default(), window, cx));
        let rows = cx.debug_bounds("palette-rows").expect("the rows");
        let faded = |cx: &mut VisualTestContext| {
            cx.update(|window, _| crate::retained::faded_edges(window, rows))
        };
        assert_eq!(faded(cx), gpui::Edges { bottom: true, ..gpui::Edges::default() }, "more below");
        let sheet = cx.debug_bounds("palette").expect("the sheet");
        assert_eq!(
            cx.update(|window, _| crate::retained::faded_edges(window, sheet)),
            gpui::Edges::default(),
            "the sheet's surface does not fade"
        );
        let stale = cx.update(|window, cx| crate::retained::stale(window, cx, 12));
        assert_eq!(stale, None, "the frame shown is what a frame from scratch paints");
        palette.update(cx, |p, cx| p.step(-1, cx));
        cx.run_until_parked();
        cx.update(Window::simulate_next_frame);
        cx.run_until_parked();
        assert_eq!(faded(cx), gpui::Edges::default(), "at the end nothing lies below");
    }

    fn plate(palette: &Entity<CommandPalette>, cx: &VisualTestContext) -> Bounds<Pixels> {
        palette.read_with(cx, |p, _| p.plate.drawn()).expect("the plate is drawn")
    }

    fn step(palette: &Entity<CommandPalette>, cx: &mut VisualTestContext) {
        palette.update(cx, |p, cx| p.step(1, cx));
        cx.run_until_parked();
    }

    /// A line fills the list's width whatever its words, so the plate spans the list and a
    /// chord sits at the right edge, not beside a short label.
    #[gpui::test]
    fn a_line_spans_the_list_whatever_its_words(cx: &mut TestAppContext) {
        let (_palette, cx) = palette_of(3, cx);
        let list = cx.debug_bounds("palette-list").expect("the list");
        let line = cx.debug_bounds("palette-item-0").expect("a line");
        let slack = list.size.width - line.size.width;
        assert!(
            slack < px(2. * 16.),
            "a line {:?} wide in a list {:?} wide",
            line.size.width,
            list.size.width
        );
    }

    /// The palette's plate sits under the selected line, and a step draws it first where it
    /// was; under Reduce Motion a step puts it on the new line in the same frame.
    #[gpui::test]
    fn the_palette_plate_settles_from_line_to_line(cx: &mut TestAppContext) {
        let (palette, cx) = palette_of(4, cx);
        let first = cx.debug_bounds("palette-item-0").expect("a line");
        assert_eq!(plate(&palette, cx), first, "under the selected line");
        step(&palette, cx);
        assert_eq!(plate(&palette, cx), first, "the move starts where the plate was");
        cx.update(|_w, cx| cx.set_reduce_motion(true));
        // As if the last step were long past, so only Reduce Motion can land it at once.
        palette.update(cx, |p, _| p.plate.0.borrow_mut().moved = None);
        step(&palette, cx);
        let third = cx.debug_bounds("palette-item-2").expect("a line");
        assert_eq!(plate(&palette, cx), third, "still: on the line at once");
    }

    /// A frame of the plate's glide draws the lines in view and works out no match: the matches
    /// are worked out when the field changes, and a list of 300 draws the rows it shows.
    #[gpui::test]
    fn a_glide_frame_draws_the_lines_in_view_and_filters_nothing(cx: &mut TestAppContext) {
        const LINES: usize = 300;
        let (palette, cx) = palette_of(LINES, cx);
        let counts = |cx: &VisualTestContext| palette.read_with(cx, |p, _| p.work_done());
        let (refreshed, _) = counts(cx);
        step(&palette, cx);
        let (_, before) = counts(cx);
        // The plate glides on GPUI's clock, which the test holds still: every frame here falls
        // inside the move however slow the machine.
        let frames = 20;
        for _ in 0..frames {
            assert!(palette.read_with(cx, |p, _| p.plate.0.borrow().flight.is_some()), "gliding");
            cx.update(Window::simulate_next_frame);
            cx.run_until_parked();
        }
        let (now, rows) = counts(cx);
        assert_eq!(now, refreshed, "no match worked out again");
        let per_frame = rows.saturating_sub(before) / frames;
        assert!(per_frame < LINES / 4, "{per_frame} rows a frame, of {LINES}");
        println!(
            "MEASURE palette glide over {LINES} lines: {frames} frames, {per_frame} rows a frame"
        );
    }

    /// What a frame of the plate's glide costs over 300 lines, each frame timed while the plate
    /// is on its way. Run by hand (it prints, it does not judge); `docs/MEASUREMENTS.md`.
    #[gpui::test]
    #[ignore = "a measurement, run by hand: see docs/MEASUREMENTS.md"]
    fn measure_a_palette_glide_frame(cx: &mut TestAppContext) {
        const LINES: usize = 300;
        const FRAMES: usize = 400;
        let (palette, cx) = palette_of(LINES, cx);
        let mut took = Vec::with_capacity(FRAMES);
        while took.len() < FRAMES {
            if !palette.read_with(cx, |p, _| p.plate.0.borrow().flight.is_some()) {
                step(&palette, cx);
                continue;
            }
            let start = Instant::now();
            cx.update(Window::simulate_next_frame);
            cx.run_until_parked();
            took.push(start.elapsed());
        }
        took.sort_unstable();
        let pct = |p: usize| slopty_client::pacing::percentile(&took, p).as_secs_f64() * 1e3;
        println!(
            "MEASURE palette glide frame over {LINES} lines, {FRAMES} frames: p50 {:.3} ms p95 {:.3} ms",
            pct(50),
            pct(95)
        );
    }

    /// The palette leaves quicker than it came: a fade on the exit pace on a desktop, as every
    /// overlay leaves, and the sheet in three quarters of its way in on a phone; at once under
    /// Reduce Motion. Once leaving, it runs nothing more.
    #[gpui::test]
    fn the_palette_leaves_quicker_than_it_came(cx: &mut TestAppContext) {
        let (palette, cx) = palette_of(2, cx);
        let fade = palette.update(cx, CommandPalette::leave);
        assert_eq!(fade, Pace::Exit.duration(), "100 ms");
        assert!(fade < Pace::Fade.duration());
        let sheet = palette.update(cx, |p, cx| {
            p.sheet = true;
            p.leave(cx)
        });
        assert_eq!(sheet, Pace::Sheet.duration().mul_f32(0.75), "180 ms");
        cx.update(|_w, cx| cx.set_reduce_motion(true));
        assert_eq!(palette.update(cx, CommandPalette::leave), Duration::ZERO, "at once");
        let ran: Rc<Cell<bool>> = Rc::default();
        let sink = Rc::clone(&ran);
        cx.update(|_w, cx| {
            cx.subscribe(&palette, move |_, _ev: &PaletteEvent, _| sink.set(true)).detach();
        });
        palette.update(cx, |p, cx| p.run(cx));
        assert!(!ran.get(), "a leaving palette runs nothing");
    }

    #[test]
    fn an_age_says_the_one_unit_that_matters() {
        let s = Duration::from_secs;
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

        // A directory, spelled from the root or home with a slash at the end, offers a folder
        // tile, a shell and a conversation there; anything else is a file. The label says what the
        // line opens, so only a line number is left for the right edge.
        let labels = |s: &str| {
            path_items(s)
                .iter()
                .map(|i| format!("{} {}", i.label, i.keys).trim_end().to_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            labels("~/proj/"),
            ["Open folder ~/proj", "New terminal in ~/proj", "New agent in ~/proj"]
        );
        assert_eq!(labels("/"), ["Open folder /", "New terminal in /", "New agent in /"]);
        assert!(
            matches!(&path_items("/srv/a/")[0].run, PaletteRun::OpenFolder { path } if path == "/srv/a")
        );
        assert!(
            matches!(&path_items("/srv/a/")[1].run, PaletteRun::OpenShell { cwd } if cwd == "/srv/a")
        );
        assert!(
            matches!(&path_items("/srv/a/")[2].run, PaletteRun::OpenAgent { cwd } if cwd == "/srv/a")
        );
        assert_eq!(labels("~/proj"), ["Open ~/proj"], "no slash at the end: a file");
        assert_eq!(labels("src/"), ["Open src/"], "relative: no shell to spell it from");
        assert_eq!(labels("./x/"), ["Open ./x/"]);
        assert_eq!(labels("/w/a.rs:3"), ["Open /w/a.rs line 3"]);
        assert_eq!(labels("note"), Vec::<String>::new());
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
            matches!(&found.run, PaletteRun::OpenFile { path, line: None, found: true } if path == "/tmp/work/src/main.rs"),
            "{found:?}"
        );
    }

    /// Every query a label's words make, through the real command list: where scoring would
    /// change the first line from the order the lines are built in.
    #[test]
    #[ignore = "measurement: cargo test -p slopty-ui --lib -- --ignored palette_scoring --nocapture"]
    fn palette_scoring_on_the_real_commands() {
        let items = crate::workspace::palette_items();
        let mut queries: Vec<String> = items
            .iter()
            .flat_map(|i| {
                i.label.to_lowercase().split_whitespace().map(str::to_owned).collect::<Vec<_>>()
            })
            .flat_map(|w| {
                let three: String = w.chars().take(3).collect();
                [w, three]
            })
            .filter(|w| w.chars().count() >= 2)
            .collect();
        queries.sort();
        queries.dedup();
        let (mut moved, mut asked) = (0, 0);
        for q in &queries {
            let kept = filter(q, &items);
            if kept.len() < 2 {
                continue;
            }
            asked += 1;
            let mut fuzzy = Fuzzy::new(q);
            let mut ranked = kept.clone();
            ranked.sort_by_cached_key(|i| std::cmp::Reverse(fuzzy.rank(&i.label, &place(i))));
            if ranked.first().map(|i| &i.label) != kept.first().map(|i| &i.label) {
                moved += 1;
                let top = |v: &[&PaletteItem]| {
                    v.iter().take(3).map(|i| i.label.clone()).collect::<Vec<_>>().join(" | ")
                };
                println!("{q:>12}  built: {}\n{:>12}  ranked: {}", top(&kept), "", top(&ranked));
            }
        }
        println!(
            "{} commands, {asked} queries matching two or more, {moved} change their first line",
            items.len()
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
        assert_eq!(label("shift-pageup"), "⇧⇞", "a named key reads as its menu glyph");
        assert_eq!(label("cmd-end"), "⌘↘");
        let item = |label: &str| PaletteItem::new(label, Box::new(MoveUp), &[]);
        let items = vec![item("New note"), item("Move column left"), item("Move column right")];
        let labels =
            |q: &str| filter(q, &items).iter().map(|i| i.label.as_str()).collect::<Vec<_>>();
        assert_eq!(labels(""), ["New note", "Move column left", "Move column right"]);
        assert_eq!(labels("column"), ["Move column left", "Move column right"]);
        assert_eq!(labels("RIGHT col"), ["Move column right"], "every word, any order, any case");
        assert_eq!(labels("nothing"), Vec::<&str>::new());
    }

    /// The lines are shown, and stepped through, group by group: tiles, workers, commands, then
    /// the files the worker found, each group in the order it was given. A path typed into the
    /// field is what was asked for, so its lines lead.
    #[test]
    fn lines_group_into_sections_in_a_fixed_order() {
        let worker = slopty_client::layout::WorkerKey::new(1);
        let items = [
            PaletteItem::new("New note", Box::new(MoveUp), &[]),
            PaletteItem::found_file("~", "notes.md"),
            PaletteItem::worker("studio", "connected", worker),
            PaletteItem::session("zsh", SessionId::new()),
            PaletteItem::new("Larger text", Box::new(MoveDown), &[]),
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
            ["zsh", "claude", "studio", "New note", "Larger text", "Open notes.md"]
        );
        assert_eq!(order(true)[0], "Open notes.md");
        let typed = path_items("~/proj/");
        assert!(typed.iter().all(|i| i.section == Section::Files), "{typed:?}");
        assert!(path_items("https://example.com").iter().all(|i| i.section == Section::Files));
    }

    /// An empty field lists every tile and worker and five commands: the ones run last, newest
    /// first, and then the first actions in their order. A readout line (a port) only shows
    /// once it has been run from here.
    #[test]
    fn an_empty_field_lists_the_tiles_and_the_recent_commands() {
        let worker = slopty_client::layout::WorkerKey::new(1);
        let action = |label: &str| PaletteItem::new(label, Box::new(MoveUp), &[]);
        let mut items = vec![
            PaletteItem::session("zsh", SessionId::new()),
            PaletteItem::worker("studio", "", worker),
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
            shown(&["G", "Gone"]),
            ["zsh", "studio", "G", "A", "B", "C", "D"],
            "the recent first, a line no longer offered skipped"
        );
    }

    /// The commands run lately sit under *Recent*, the rest of the commands under *Commands*,
    /// and a tile keeps its group whatever its name; ↩ says what it will do with each.
    #[test]
    fn the_recent_commands_have_their_own_group() {
        let recent = vec!["New note".to_owned()];
        let command = |label: &str| PaletteItem::new(label, Box::new(MoveUp), &[]);
        assert_eq!(group(&command("New note"), &recent), ("Recent", "recent"));
        assert_eq!(group(&command("Larger text"), &recent), ("Commands", "commands"));
        assert_eq!(group(&command("New note"), &[]), ("Commands", "commands"), "a typed query");
        let tile = PaletteItem::session("New note", SessionId::new());
        assert_eq!(group(&tile, &recent), ("Tiles", "tiles"));
        assert_eq!(command("New note").run.verb(), "Run");
        assert_eq!(tile.run.verb(), "Go to");
        assert_eq!(PaletteItem::open_file("/w/a.rs", None).run.verb(), "Open");
    }

    /// A line that goes somewhere carries its kind in the leading slot; a command carries
    /// none. The right edge says a tile's status while it is not idle, else its age past a
    /// minute, and a command's keys; the kind is read after where the tile is.
    #[test]
    fn the_right_edge_says_status_age_or_keys_and_the_kind_is_context() {
        let minutes = |n: u64| Some(Duration::from_secs(n * 60));
        let tile = PaletteItem::session("claude", SessionId::new()).aged(minutes(12));
        assert_eq!(tile.trailing(), Some(("12m".to_owned(), None)));
        let working = tile.clone().with_status(Some(Status::Working));
        assert_eq!(working.trailing(), Some(("Working".to_owned(), Some(Status::Working))));
        let idle = tile.clone().with_status(Some(Status::Idle));
        assert_eq!(idle.trailing(), Some(("12m".to_owned(), None)), "idle: the age");
        let done = tile.with_status(Some(Status::Done));
        assert_eq!(done.trailing(), Some(("12m".to_owned(), None)), "done: the dot, then the age");
        let fresh = PaletteItem::session("zsh", SessionId::new()).aged(minutes(0));
        assert_eq!(fresh.trailing(), None, "no age under a minute");
        let file = PaletteItem::item("PLAN.md", Symbol::DocText, ItemId::new())
            .placed(Some("docs".to_owned()))
            .on_worker(Some("studio".to_owned()));
        assert_eq!(file.context(), "studio · docs");
        assert_eq!(file.trailing(), None);
        let command = PaletteItem::new("New note", Box::new(MoveUp), &[]);
        assert_eq!(command.icon, None, "a command is its words");
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

    /// Every line that is a thing leads with what it is, and a file's line with its kind.
    #[test]
    fn every_thing_leads_with_what_it_is() {
        let session = SessionId::new();
        assert_eq!(PaletteItem::session("zsh", session).icon, Some(Mark::Symbol(Symbol::Terminal)));
        let worker = PaletteItem::worker("w", "", slopty_client::layout::WorkerKey::new(1));
        assert_eq!(worker.icon, Some(Mark::Symbol(Symbol::ServerRack)));
        assert_eq!(
            PaletteItem::open_file("/w/a.rs", None).icon,
            Some(Mark::Symbol(icons::FileType::Code.symbol())),
            "a file line shows its kind"
        );
        assert_eq!(PaletteItem::open_file("/w/notes", None).icon, Some(Mark::Symbol(Symbol::Doc)));
    }
}
