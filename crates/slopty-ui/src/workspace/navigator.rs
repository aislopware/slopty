//! The navigator: what is there and where it runs, down the whole left of the window.
//!
//! It runs from the window's top edge to its bottom, and its top row is the title bar's
//! height: the traffic lights sit in it on a Mac, then a field that filters every row below.
//! On a phone, where the title bar has no tabs, *Workspaces* heads the list: a row per
//! workspace with its tile count, the active one on a plate of its own, then "New workspace".
//! *Needs you* appears only while an agent waits on the human, *To review* only while one's turn
//! ended unseen (its row says what it did, else how long it ran), and *Working* only while one
//! is at its turn (its heading turns the working mark and counts them, its rows tick their
//! turn's time once a second), each only for agents whose own row is out of sight. Then the
//! workers, whose heading is left out when it would be the only one. Each worker has a header
//! disclosing its tiles: its name in the strong weight after a server icon (crossed out while the
//! worker is away), then on its right edge a word for what is wrong with its link, else its round
//! trip when that is slow enough to matter, led by what its tiles add up to while it is folded. The
//! pointer brings out the chevron and "+" (a new shell on that worker) in the readouts' place. Its
//! tiles come in order of attention: what needs the human, then what finished unseen, then what
//! is working, then the rest, each class in reading order. Each tile is two lines: its kind and
//! its title, ended by its state in a word ("Needs approval", "Working", "Done", "Failed"),
//! else the unseen dot, else its age past a minute (an agent at rest counts from its last turn);
//! then, muted, its directory (its worker's name where it has none), what its agent says or its
//! last command and its branch, or a note's progress; a shell reopened after its shell was lost
//! says "Restored" there, and one whose program reports progress (`OSC 9;4`) ends that line in
//! its figure and draws a hairline bar along the row's foot, as the Dock's does for them all,
//! from the session's summary, so a tile never viewed shows it too. A row waiting on the human is
//! not washed: the *Needs you* section above already leads with it, and its word says so in the
//! warn tone. A row flies the camera to what it names. Workspaces are the title bar's tabs, not a
//! section here.
//!
//! The palette's "Group the navigator by repository" swaps the workers for the repositories
//! the shells are in, whichever worker holds each checkout: a header per repository path (its
//! directory's name, and where it is when another listed has that name), its tiles from every
//! worker in order of attention, each second line naming the worker, the directory below the
//! repository and the branch. A file or folder joins the deepest repository of its worker's
//! shells that holds it; the rest sit under *No repository*, last. The lens is kept with the
//! layout, and the same palette line goes back by worker.
//!
//! On a window wide enough it docks beside the rest of the frame, 248 pt by default, dragged
//! from 200 to 400 by a 12 pt handle centred on its right edge, which a double-click puts back
//! at 248; ⌘B shows or hides it, and both are kept with the device's layout. Hidden there, a
//! 40 pt rail keeps one server glyph per worker, with what its tiles add up to, so what wants
//! the human stays on screen. On an iPad it opens over the frame and a scrim, and on a phone it
//! slides in as a drawer over the scrim, down through the home indicator's band; either closes
//! once a row is chosen.
//!
//! It is a view of its own, drawn cached: an echo in a terminal does not draw it again, only a
//! change to the workspace or its own clock does (`WorkspaceView::render_frame`).
//!
//! The rows are a virtual list: each drawing works out what every row says, but only the rows
//! in view, and a couple of rows' height past either edge, are laid out and drawn. A focused
//! tile whose row is out of view scrolls into it.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashSet};
use std::mem::{Discriminant, discriminant};
use std::time::{Duration, SystemTime};

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AppContext as _, Context, Div, ElementId, Entity, InteractiveElement as _, IntoElement as _,
    ListAlignment, ListOffset, ListState, MouseButton, MouseMoveEvent, MouseUpEvent,
    ParentElement as _, SharedString, Stateful, StatefulInteractiveElement as _, Styled as _,
    Subscription, Task, Window, canvas, div, list, px,
};
use gpui_kit::component::input::{Escape, Input, InputEvent, InputState};
use slopty_client::layout::{NavLens, Navigator, TileRef, WorkerKey};
use slopty_core::WallMs;
use slopty_proto::agent::{AgentEvent, AgentStatus, BlockReason};
use slopty_proto::items::{Item, ItemKind};
use slopty_proto::server::{Os, WorkerCaps};
use slopty_proto::tailnet::LinkPath;
use slopty_proto::terminal::{Progress, ProgressState, RepoChanges};
use slopty_proto::thread::attention::Rung;
use slopty_theme::{Rgb, Theme, Typography, alpha};

use super::actions::{ToggleNavigator, ToggleNavigatorLens};
use super::agents::{Waiting, agent_ask_line, agent_status_text, agent_status_word, needs_human};
use super::rollup::{META_SEPARATOR, Rollup, age_at, meta_line, rollup_slot};
use super::tile::kind_icon;
use super::titlebar::{LEADING_INSET, titlebar_height};
use super::{WorkerStatus, WorkspaceView};
use crate::a11y::tab_stop;
use crate::colors::hsla;
use crate::draw::Draw;
use crate::icons::{IconName, IconSize, Status, icon, status_icon, status_mark};
use crate::kit::{self, meta, tabular};
use crate::palette::{PaletteItem, Plate};

/// The two lines' height, as a multiple of their type size: tighter than a paragraph's, so
/// the pair reads as one row.
const TILE_LINE: f32 = 1.3;

/// The handle's hit area, centred on the navigator's 1 pt edge: easy to find with a pointer
/// without a visible grip.
pub(super) const HANDLE_W: f32 = 12.0;

/// The rail's width, where the navigator is hidden: one glyph per worker.
pub(super) const RAIL_W: f32 = 40.0;

/// What an open worker with no tile says under its name.
pub(super) const NO_TILES: &str = "No tiles";

/// What a shell reopened after its shell was lost says on its row.
pub(super) const RESTORED: &str = "Restored";

/// The palette's name for [`ToggleNavigatorLens`] while the navigator groups by worker.
pub(super) const BY_REPOSITORY: &str = "Group the navigator by repository";

/// The palette's name for [`ToggleNavigatorLens`] while it groups by repository.
pub(super) const BY_WORKER: &str = "Group the navigator by worker";

/// The group, under the repository lens, of the tiles in no repository.
pub(super) const NO_REPOSITORY: &str = "No repository";

/// How many rows *Working* lists before "Show N more".
const WORKING_SHOWN: usize = 4;

/// A round trip the navigator names: above it typing starts to feel remote, below it the
/// number is noise beside every worker. The status bar uses the same threshold (and shows it
/// under the pointer); the hosts popover gives it always.
pub(super) const RTT_SHOWN_FROM: Duration = Duration::from_millis(20);

/// How far past each edge of the view the list lays rows out, in points, so a scroll does not
/// show a row arriving: two rows of two lines.
const OVERDRAW: f32 = 80.0;

/// How the navigator sits in the window.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Mode {
    /// Beside the frame, which narrows to make room.
    Docked,
    /// Over the frame, which keeps its width: an iPad, or a window too narrow to give the
    /// room without turning the strip into a phone's.
    Overlay,
    /// Over the frame and a scrim, from the left edge: a phone.
    Drawer,
}

/// The strip a touch screen keeps beside a docked navigator: an iPad's regular width, room
/// for a terminal and a column beside it. Narrower, the navigator lays over the strip instead.
pub(super) const TOUCH_DOCK_STRIP: f32 = 900.0;

/// How the navigator sits in a window `window_w` wide, `width` its own width. A window that
/// is a phone's gets the drawer; a window that would leave the strip a phone's width, or a
/// touch screen (an iPad) that would leave it less than [`TOUCH_DOCK_STRIP`], gets the overlay;
/// anything else docks it.
pub(super) fn mode(window_w: f32, width: f32, phone_below: f32, touch: bool) -> Mode {
    let strip = window_w - width;
    if window_w < phone_below {
        Mode::Drawer
    } else if strip < phone_below || (touch && strip < TOUCH_DOCK_STRIP) {
        Mode::Overlay
    } else {
        Mode::Docked
    }
}

/// What the navigator keeps for this run only; whether it is docked and its width are the
/// layout's, and saved with it.
#[derive(Default)]
pub(super) struct NavState {
    /// Open over the strip, where it does not dock.
    pub open: bool,
    /// Workers whose tiles are folded away.
    pub folded: HashSet<WorkerKey>,
    /// Repositories whose tiles are folded away, under the repository lens, by path (the
    /// empty path for the tiles in none).
    pub folded_repos: HashSet<String>,
    /// The handle is being dragged: where the pointer and the width were when it was pressed.
    pub resize: Option<(f32, f32)>,
    /// How it sat in the last frame drawn, and whether it showed.
    pub drawn: Option<Mode>,
    /// The rail shows in its place: docked where it would dock, but hidden.
    pub rail: bool,
    /// *Working* lists every agent at work, not only the first few.
    pub working_all: bool,
    /// The filter over its rows.
    pub filter: Filter,
    /// Its rows, as the list lays them out.
    pub list: NavList,
    /// Draws it again when a label it shows changes with the clock (a turn's time, an age).
    pub tick: RefCell<Option<Task<()>>>,
}

/// The navigator's rows and the virtual list that shows them.
pub(super) struct NavList {
    /// The list's scroll and the heights of the rows it has measured. It measures every row
    /// once, so a reveal lands exactly: a row's height is its kind's, and a change to the rows
    /// sends only the rows whose kind changed to be measured again.
    state: ListState,
    /// This frame's rows, in order; the list draws the ones in view from here.
    rows: RefCell<Vec<NavRow>>,
    /// The tile whose row is selected: the focused one.
    selected: Cell<Option<TileRef>>,
    /// The focused tile the list last brought into view, so a row is revealed once per move
    /// of the focus, and a list the human scrolled away stays where they left it.
    revealed: Cell<Option<TileRef>>,
    /// The tile being revealed this frame: its row, once laid out, asks the list to scroll it
    /// fully into view.
    autoscroll: Cell<Option<TileRef>>,
    /// The fill under the selected row.
    plate: Plate,
    /// The fill under the active workspace's row, on a phone: a plate of its own, since the
    /// focused tile's row below keeps the list's.
    space_plate: Plate,
}

impl Default for NavList {
    fn default() -> Self {
        Self {
            state: ListState::new(0, ListAlignment::Top, px(OVERDRAW)).measure_all(),
            rows: RefCell::default(),
            selected: Cell::default(),
            revealed: Cell::default(),
            autoscroll: Cell::default(),
            plate: Plate::default(),
            space_plate: Plate::default(),
        }
    }
}

impl NavList {
    /// Take this frame's rows. Where a row's shape changed (its kind, or a tile's line
    /// count), the list forgets its height and measures it on its next layout.
    ///
    /// A list at its very top stays there: a section that opens above the first row (*Needs
    /// you*, *Working*) shows, rather than pushing the view down past it.
    ///
    /// The scroll offset is read only when rows are spliced: this runs in the render, and a
    /// render that reads the offset makes every scroll a change of what the list's scroll layer
    /// holds, so the layer could never be composited.
    fn set_rows(&self, rows: Vec<NavRow>) {
        let same = |(a, b): &(&NavRow, &NavRow)| a.shape() == b.shape();
        let mut was = self.rows.borrow_mut();
        let old = was.len();
        let head = was.iter().zip(&rows).take_while(same).count();
        let spliced = head != old || old != rows.len();
        if spliced {
            let scrolled = self.state.logical_scroll_top();
            let at_top = scrolled.item_ix == 0 && scrolled.offset_in_item <= px(0.0);
            let most = old.min(rows.len()).saturating_sub(head);
            let tail = was.iter().rev().zip(rows.iter().rev()).take(most).take_while(same);
            let tail = tail.count();
            let added = head..rows.len().saturating_sub(tail);
            self.state.splice(head..old.saturating_sub(tail), added.len());
            if !added.is_empty() {
                self.state.remeasure_items(added);
            }
            if at_top {
                self.state.scroll_to(ListOffset::default());
            }
        }
        *was = rows;
    }

    /// Whether `tile`'s row was in view in the last layout. A row the list has not placed yet
    /// (new this frame, or before the first layout) counts as in view, so nothing flashes in
    /// for a frame on its account; a row above the list's top is out.
    fn in_view(&self, tile: TileRef) -> bool {
        let Some(ix) =
            self.rows.borrow().iter().position(|r| matches!(r, NavRow::Tile(t) if t.tile == tile))
        else {
            return true;
        };
        if ix < self.state.logical_scroll_top().item_ix {
            return false;
        }
        let view = self.state.viewport_bounds();
        self.state
            .bounds_for_item(ix)
            .is_none_or(|row| row.bottom() > view.top() && row.top() < view.bottom())
    }

    /// Scroll the selected tile's row into view if the focus moved to it since the last reveal.
    /// The list places it by the heights it has measured, and a row added this frame has none
    /// yet, so the row also asks for the rest once it is laid out, in the same frame. Before the
    /// list's first layout there is no height to place it by, and the reveal waits a frame.
    fn reveal_selected(&self, window: &Window) {
        self.autoscroll.set(None);
        let selected = self.selected.get();
        if selected == self.revealed.get() {
            return;
        }
        let row = self
            .rows
            .borrow()
            .iter()
            .position(|r| matches!(r, NavRow::Tile(t) if Some(t.tile) == selected));
        let Some(ix) = row else {
            self.revealed.set(selected);
            return;
        };
        if self.state.viewport_bounds().size.height <= px(0.0) {
            window.request_animation_frame();
            return;
        }
        self.state.scroll_to_reveal_item(ix);
        self.revealed.set(selected);
        self.autoscroll.set(selected);
    }
}

/// The navigator's filter: its field, made on the first frame that shows the navigator (it
/// needs the window), what the field holds, and the field's events, held while it is.
#[derive(Default)]
pub(super) struct Filter {
    input: Option<Entity<InputState>>,
    query: String,
    events: Option<Subscription>,
    /// The field takes the keyboard once it is drawn: asked by key.
    focus_asked: bool,
}

impl std::fmt::Debug for NavState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NavState")
            .field("open", &self.open)
            .field("folded", &self.folded)
            .field("drawn", &self.drawn)
            .field("rail", &self.rail)
            .field("query", &self.filter.query)
            .finish_non_exhaustive()
    }
}

/// A round trip as the navigator and the status bar print it: tenths under 10 ms, whole
/// milliseconds above, so a jittery link does not repaint the chrome on every sample.
pub(super) fn rtt_label(rtt: Duration) -> String {
    let ms = rtt.as_secs_f64() * 1e3;
    if ms < 10.0 { format!("{ms:.1} ms") } else { format!("{ms:.0} ms") }
}

/// A worker's link where it is not simply up: a mark and a short word. A link that is up
/// shows neither, so a list of workers that are fine is a list of names; the word says what
/// is wrong only when something is.
pub(super) const fn worker_health(status: &WorkerStatus) -> Option<(Status, &'static str)> {
    match status {
        WorkerStatus::Connected => None,
        WorkerStatus::Connecting => Some((Status::Working, "connecting")),
        WorkerStatus::Silent(_) => Some((Status::Away, "silent")),
        WorkerStatus::Checking => Some((Status::Working, "checking")),
        WorkerStatus::Relinking => Some((Status::Working, "reconnecting")),
        WorkerStatus::Reconnecting(_) => Some((Status::Away, "reconnecting")),
        WorkerStatus::Unreachable => Some((Status::Away, "unreachable")),
        WorkerStatus::Gone => Some((Status::Away, "gone")),
        WorkerStatus::NotGranted => Some((Status::Away, "not granted")),
        WorkerStatus::NeedsUpdate(_) => Some((Status::Away, "different build")),
    }
}

/// What is wrong with a worker itself, as its header's warn line says it; nothing when all is
/// well. A Mac that has not granted Screen Recording cannot share a window, and one without
/// Accessibility cannot take the keys. A worker on another build never links at all: its
/// status says so ([`WorkerStatus::NeedsUpdate`]), from the wire's own fingerprint.
pub(super) fn worker_warning(caps: &WorkerCaps) -> Option<String> {
    let mac = caps.os == Os::MacOs;
    let wrong: Vec<String> = [
        (mac && !caps.can_capture).then(|| "Screen Recording off".to_owned()),
        (mac && !caps.can_inject).then(|| "Accessibility off".to_owned()),
    ]
    .into_iter()
    .flatten()
    .collect();
    (!wrong.is_empty()).then(|| wrong.join(META_SEPARATOR))
}

/// A worker's machine in a line, as the hosts list reads it: `macOS 26.5 · load 2.1`, without
/// the load until one is known.
pub(super) fn host_line(caps: &WorkerCaps, load: Option<f32>) -> String {
    let os = match caps.os {
        Os::MacOs => "macOS",
        Os::Linux => "Linux",
    };
    let os = format!("{os} {}", caps.os_version).trim().to_owned();
    match load {
        Some(load) => format!("{os}{META_SEPARATOR}load {load:.1}"),
        None => os,
    }
}

/// How a link's packets travel, as the chrome names it, and whether that is the slow path: a
/// DERP relay is TCP and often a detour, so it wears the warning tone.
pub(super) fn path_label(path: &LinkPath) -> (String, bool) {
    match path {
        LinkPath::Direct => ("Direct".to_owned(), false),
        LinkPath::PeerRelay => ("Peer relay".to_owned(), false),
        LinkPath::Derp { region } if region.is_empty() => ("DERP".to_owned(), true),
        LinkPath::Derp { region } => (format!("DERP · {region}"), true),
    }
}

/// Where a tile's row stands among its worker's, on the attention ladder: what needs the
/// human, then what failed, then what finished or has news not yet seen, then what works,
/// then what waits on work in the background, then the rest.
pub(super) const fn attention(status: Option<Status>, unseen: bool) -> u8 {
    match status {
        Some(Status::NeedsYou) => 0,
        Some(Status::Failed) => 1,
        _ if unseen => 2,
        Some(Status::Done) => 2,
        Some(Status::Working) => 3,
        Some(Status::Running) => 4,
        Some(Status::Idle | Status::Away) | None => 5,
    }
}

/// The state a tile's row names in a word at the end of its first line: what is happening
/// there. At rest, or out of reach (its worker's header says so), nothing; a finish not yet
/// seen is the accent dot, not a word.
pub(super) const fn status_word(status: Option<Status>) -> Option<Status> {
    match status {
        Some(s @ (Status::NeedsYou | Status::Failed | Status::Working | Status::Running)) => {
            Some(s)
        }
        Some(Status::Done | Status::Idle | Status::Away) | None => None,
    }
}

/// How strongly a tile's row is drawn: one at work recedes until it is hovered or selected,
/// so what needs the person leads the list.
pub(super) const fn row_strength(theme: &Theme, status: Option<Status>, selected: bool) -> f32 {
    match status {
        Some(Status::Working | Status::Running) if !selected => theme.set_back(alpha::STRONG),
        _ => 1.0,
    }
}

/// A tile's age as its row prints it: nothing under a minute, where every new tile would read
/// "now", then the one unit that matters.
pub(super) fn age_shown(age: Duration) -> Option<String> {
    (age >= Duration::from_secs(60)).then(|| crate::palette::age_label(age))
}

/// A clock that ticks once a second, as *Working* and a long command's row print it: whole
/// seconds in [`kit::duration`]'s one form, from "1 s" (never a fraction the next tick undoes).
pub(super) fn turn_label(elapsed: Duration) -> String {
    kit::duration(Duration::from_secs(elapsed.as_secs().max(1)))
}

/// What an agent at rest last said, as its row's second line gives it: a single word quoted
/// (`“done”`), so the agent's own last word does not read as a second state beside the row's.
pub(super) fn rest_words(said: &str) -> Option<String> {
    let said = said.trim();
    if said.is_empty() {
        None
    } else if said.contains(char::is_whitespace) {
        Some(said.to_owned())
    } else {
        Some(format!("\u{201c}{said}\u{201d}"))
    }
}

/// How long until an age's label next changes: the next whole unit of [`age_shown`].
pub(super) fn until_age_changes(age: Duration) -> Duration {
    let secs = age.as_secs();
    let unit: u64 = match secs {
        0..3_600 => 60,
        3_600..86_400 => 3_600,
        _ => 86_400,
    };
    Duration::from_secs(unit.saturating_sub(secs.checked_rem(unit).unwrap_or(0)))
}

/// A working tree's changes as a row counts them, the lines added and removed; nothing for a
/// tree with no line changed.
pub(super) const fn line_changes(changes: RepoChanges) -> Option<(u32, u32)> {
    if changes.added == 0 && changes.removed == 0 {
        None
    } else {
        Some((changes.added, changes.removed))
    }
}

/// A round trip as the chrome names it unasked: only once it is slow enough to feel, since a
/// 1 ms figure beside every worker is noise the frame already hides.
pub(super) fn slow_rtt(rtt: Option<Duration>) -> Option<String> {
    rtt.filter(|rtt| *rtt >= RTT_SHOWN_FROM).map(rtt_label)
}

/// A note's second line: its progress (its [`super::tile::note_progress`]) when it has tasks,
/// else its first line after the title, else nothing (its icon says it is a note).
pub(super) fn note_meta(text: &str, progress: Option<(usize, usize)>) -> String {
    if let Some((done, total)) = progress {
        return super::tile::note_done(done, total);
    }
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .nth(1)
        .map(|l| l.trim_start_matches(['#', '-', '*', '>', ' ']).trim().to_owned())
        .unwrap_or_default()
}

/// Whether `query` (already lowercase) is in any of `hay`; an empty query is in everything.
pub(super) fn matches(query: &str, hay: &[&str]) -> bool {
    query.is_empty() || hay.iter().any(|h| h.to_lowercase().contains(query))
}

/// Whether an agent is at rest: its turn over, or at its prompt with nothing asked.
const fn at_rest(agent: &AgentEvent) -> bool {
    matches!(
        agent.status,
        AgentStatus::Idle | AgentStatus::Done | AgentStatus::Blocked(BlockReason::IdlePrompt)
    )
}

/// The wall clock in Unix milliseconds, as the workers stamp an agent's change.
fn now_ms() -> u64 {
    WallMs::now().as_millis()
}

/// The unseen dot: something ended there while the human was elsewhere. It sits centred in a
/// fixed slot at the end of a row's first line, so a title keeps its length with or without it.
pub(super) fn unseen_dot(theme: &Theme, selector: String, shown: bool) -> Div {
    let s = &theme.surfaces;
    let dot = theme.spacing.xs + theme.spacing.xxs;
    div().flex_none().w(px(theme.spacing.sm)).flex().items_center().justify_center().when(
        shown,
        |slot| {
            slot.child(
                div()
                    .debug_selector(move || selector)
                    .size(px(dot))
                    .rounded_full()
                    .bg(hsla(s.accent_fill)),
            )
        },
    )
}

/// One row of the navigator's list: its density's height, on the one edge grid (the wash sits
/// a base unit in from the panel's edges), a tab stop named `label`. The pointer washes it
/// `raised`; the selected row sits on the list's plate, which settles from row to row.
pub(super) fn row(
    theme: &Theme,
    lines: kit::Row,
    id: impl Into<ElementId>,
    selector: String,
    label: SharedString,
    selected: bool,
) -> Stateful<Div> {
    let s = theme.surfaces;
    let spacing = theme.spacing;
    let el = div()
        .id(id)
        .debug_selector(move || selector)
        .role(Role::Button)
        .aria_label(label)
        .flex_none()
        .h(px(lines.height(theme)))
        .mx(px(spacing.xs))
        .px(px(spacing.inset() - spacing.xs))
        .flex()
        .items_center()
        .gap(px(spacing.xs))
        .rounded(px(theme.radii.sm))
        .cursor_pointer()
        .text_size(px(theme.typography.ui_size))
        .when(!selected, |el| el.hover(move |el| el.bg(hsla(s.raised))));
    tab_stop(el, s.accent)
}

/// A row's title, cut with an ellipsis.
pub(super) fn title(text: impl Into<SharedString>, color: gpui::Hsla) -> Div {
    div()
        .flex_1()
        .min_w_0()
        .overflow_hidden()
        .whitespace_nowrap()
        .text_ellipsis()
        .text_color(color)
        .child(text.into())
}

/// Meta text on the right of a row's line, in tabular figures: an age, a turn's time or a
/// round trip that changes does not move what is beside it.
pub(super) fn readout(theme: &Theme, text: impl Into<SharedString>) -> Div {
    tabular(meta(div(), theme)).flex_none().whitespace_nowrap().child(text.into())
}

/// A square of the leading slot's side around `child`, so every row's title starts on one edge.
fn lead_slot(theme: &Theme, child: impl gpui::IntoElement) -> Div {
    div()
        .flex_none()
        .size(px(theme.typography.icon_large()))
        .flex()
        .items_center()
        .justify_center()
        .child(child)
}

/// How far a row icon's glyph sits in from its slot's edge: the slot is `icon_large`, the
/// glyph `icon`, centred.
pub(super) fn glyph_margin(theme: &Theme) -> f32 {
    (theme.typography.icon_large() - theme.typography.icon()) / 2.0
}

/// The height of a row's first and second lines, the navigator's and the inbox's alike.
pub(super) fn line_heights(theme: &Theme) -> (f32, f32) {
    let typo = &theme.typography;
    (typo.ui_size * TILE_LINE, typo.meta() * TILE_LINE)
}

/// A tile as its row shows it.
#[derive(Clone)]
struct NavTile {
    tile: TileRef,
    kind: IconName,
    mark: Option<Status>,
    /// The state's word, said in the row's accessible name (the row draws the state as its
    /// glyph): [`agent_status_word`] for an agent that waits ("Needs approval", "Has a
    /// question"), else the mark's own label.
    word: Option<String>,
    unseen: bool,
    title: String,
    meta: String,
    age: Option<String>,
    /// When the age's label next changes.
    age_changes: Option<Duration>,
    /// How long its command has run, while it runs past [`RUNNING_AFTER`](super::RUNNING_AFTER).
    running: Option<String>,
    /// The lines its repository's working tree has added and removed, at the end of the
    /// second line.
    changes: Option<(u32, u32)>,
    /// Its program's progress report, while one stands.
    progress: Option<Progress>,
    /// It was reopened after its shell was lost.
    restored: bool,
    /// The repository it is in: its shell's, or for a file or a folder the deepest of its
    /// worker's shells' repositories that holds it.
    repo: Option<String>,
}

impl NavTile {
    /// Whether it has a second line.
    const fn two_lines(&self) -> bool {
        let figure = match self.progress {
            Some(progress) => has_figure(progress),
            None => false,
        };
        !self.meta.is_empty() || self.changes.is_some() || self.restored || figure
    }
}

/// Whether a progress report names how far along it is.
const fn has_figure(progress: Progress) -> bool {
    let measured =
        matches!(progress.state, ProgressState::Set | ProgressState::Paused | ProgressState::Error);
    measured && progress.percent.is_some()
}

/// What an agent says, its subagents at work folded in as a count: "Editing parser.rs, 2
/// subagents", or the count alone. A count is a part of the words rather than a part of the
/// line, so the line keeps its two separators.
fn with_subagents(words: Option<String>, subagents: usize) -> Option<String> {
    let count = match subagents {
        0 => return words,
        1 => "1 subagent".to_owned(),
        n => format!("{n} subagents"),
    };
    Some(match words {
        Some(words) if !words.is_empty() => format!("{words}, {count}"),
        _ => count,
    })
}

/// A progress report's figure at the end of a row's second line: "42%" while it has one.
pub(super) fn progress_figure(progress: Progress) -> Option<String> {
    let percent = progress.percent.filter(|_| has_figure(progress))?;
    Some(format!("{}%", percent.min(100)))
}

/// The hairline a progress report draws along a row's foot: its tone, how strongly, and the
/// share of the row it covers. A report with no figure covers the row, set back, and stands
/// still: the navigator never moves on its own. The tones are the tile header's bar's.
pub(super) fn progress_line(theme: &Theme, progress: Progress) -> Option<(Rgb, f32, f32)> {
    let s = &theme.surfaces;
    let tone = match progress.state {
        ProgressState::None => return None,
        ProgressState::Set | ProgressState::Indeterminate => s.accent,
        ProgressState::Paused => s.text_muted,
        ProgressState::Error => s.error,
    };
    Some(match (progress.state, progress.percent) {
        (ProgressState::Indeterminate, _) | (_, None) => (tone, alpha::STRONG, 1.0),
        (_, Some(percent)) => (tone, 1.0, f32::from(percent.min(100)) / 100.0),
    })
}

/// A worker's block as the navigator lists it.
struct NavWorker {
    header: NavHeader,
    tiles: Vec<NavTile>,
}

/// A worker's header as its row shows it.
#[derive(Clone)]
struct NavHeader {
    key: WorkerKey,
    name: String,
    health: Option<(Status, &'static str)>,
    /// The DERP relay its link goes through, the slow path; a direct or peer-relayed link
    /// names nothing here.
    relay: Option<String>,
    /// Its round trip, when it is slow enough to name.
    rtt: Option<String>,
    /// What is wrong with the worker itself, under its name: a permission off, another version.
    warning: Option<String>,
    linked: bool,
    rollup: Rollup,
    folded: bool,
    /// It follows another worker's rows, and stands a step off them.
    gap: bool,
}

/// A repository's header, under the repository lens.
#[derive(Clone)]
struct NavRepo {
    /// Its path, the same on every worker that has a checkout there; empty for the tiles in
    /// no repository.
    path: String,
    name: String,
    /// Where it is, when another repository listed has the same name.
    parent: Option<String>,
    rollup: Rollup,
    folded: bool,
    /// It follows another repository's rows, and stands a step off them.
    gap: bool,
}

/// One group of the list: a worker's or a repository's header, then its tiles unless folded.
struct NavBlock {
    head: NavRow,
    folded: bool,
    /// The worker whose empty block says so, where its tiles would be.
    vacant: Option<WorkerKey>,
    tiles: Vec<NavTile>,
}

/// An agent in *Needs you* or *Working*: what it is, what it says, where, and since when.
#[derive(Clone)]
struct NavAgent {
    at: Waiting,
    status: Status,
    title: String,
    words: String,
    /// Its worker and directory.
    place: String,
    /// When its status last changed, on the worker's clock (Unix milliseconds); zero unknown.
    since_ms: u64,
}

/// A workspace as the phone's drawer lists it.
#[derive(Clone)]
struct NavSpace {
    ix: usize,
    name: String,
    tiles: usize,
    active: bool,
}

/// One row of the navigator's list.
#[derive(Clone)]
enum NavRow {
    Heading {
        selector: &'static str,
        text: &'static str,
        /// How many rows the section holds, beside the working mark.
        working: Option<usize>,
    },
    Agent(NavAgent),
    /// *Working* holds this many more than it shows.
    More(usize),
    /// A workspace, on a phone, whose title bar has no tabs.
    Space(NavSpace),
    /// "New workspace", the last of *Workspaces*.
    NewSpace,
    Worker(NavHeader),
    Repo(NavRepo),
    Tile(NavTile),
    /// An open worker with no tile, in one quiet line where its tiles would be.
    Vacant(WorkerKey),
    /// The filter left nothing.
    Nothing,
}

impl NavRow {
    /// What a row's height follows: its kind, then for a tile whether it has a second line,
    /// and for a worker's header whether it has a warning line and a step above it.
    const fn shape(&self) -> (Discriminant<Self>, bool, bool) {
        let (lines, gap) = match self {
            Self::Tile(t) => (!t.two_lines(), false),
            Self::Worker(h) => (h.warning.is_some(), h.gap),
            Self::Repo(r) => (false, r.gap),
            Self::Heading { .. }
            | Self::Agent(_)
            | Self::More(_)
            | Self::Space(_)
            | Self::NewSpace
            | Self::Vacant(_)
            | Self::Nothing => (false, false),
        };
        (discriminant(self), lines, gap)
    }
}

impl WorkspaceView {
    /// ⌘B: dock or undock the navigator where it docks (kept with the layout), else open or
    /// close it over the strip.
    pub fn toggle_navigator(
        &mut self,
        _: &ToggleNavigator,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.navigator_mode(window) {
            Mode::Docked => {
                let nav = self.layout.navigator();
                self.layout.set_navigator(Navigator { shown: !nav.shown, ..nav });
                self.nav.open = false;
                self.layout_touched(cx);
            }
            Mode::Overlay | Mode::Drawer => self.nav.open = !self.nav.open,
        }
        cx.notify();
    }

    /// Show the navigator and type into its filter: what is typed narrows every row.
    pub fn filter_navigator(
        &mut self,
        _: &super::actions::FilterNavigator,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.navigator_mode(window) {
            Mode::Docked => {
                let nav = self.layout.navigator();
                if !nav.shown {
                    self.layout.set_navigator(Navigator { shown: true, ..nav });
                    self.layout_touched(cx);
                }
            }
            Mode::Overlay | Mode::Drawer => self.nav.open = true,
        }
        self.nav.filter.focus_asked = true;
        cx.notify();
    }

    /// Give the filter the keyboard when a key asked for it and the field is there.
    pub(super) fn settle_navigator_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.nav.filter.focus_asked {
            return;
        }
        if let Some(input) = self.nav.filter.input.clone() {
            self.nav.filter.focus_asked = false;
            input.update(cx, |input, cx| {
                input.focus(window, cx);
                input.select_all(window, cx);
            });
        }
    }

    /// Group the navigator by repository, or back by worker (kept with the layout).
    pub fn toggle_navigator_lens(
        &mut self,
        _: &ToggleNavigatorLens,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let nav = self.layout.navigator();
        let lens = match nav.lens {
            NavLens::Workers => NavLens::Repositories,
            NavLens::Repositories => NavLens::Workers,
        };
        self.layout.set_navigator(Navigator { lens, ..nav });
        self.layout_touched(cx);
        cx.notify();
    }

    /// The palette's line for the lens the navigator is not in.
    pub(super) fn lens_line(&self) -> PaletteItem {
        let (label, glyph) = match self.layout.navigator().lens {
            NavLens::Workers => (BY_REPOSITORY, IconName::FolderGit2),
            NavLens::Repositories => (BY_WORKER, IconName::Server),
        };
        PaletteItem::new(
            label,
            glyph,
            Box::new(ToggleNavigatorLens),
            &super::actions::key_bindings(),
        )
    }

    /// How the navigator sits in `window`.
    pub(super) fn navigator_mode(&self, window: &Window) -> Mode {
        let window_w = self.width(window);
        let width = self.layout.navigator().width;
        mode(window_w, width, self.layout.config().phone_below, cfg!(target_os = "ios"))
    }

    /// Whether the navigator is drawn in `mode`.
    pub(super) fn navigator_visible(&self, mode: Mode) -> bool {
        !self.workers.is_empty()
            && match mode {
                Mode::Docked => self.layout.navigator().shown,
                Mode::Overlay | Mode::Drawer => self.nav.open,
            }
    }

    /// Work out how the navigator sits this frame: shown in which mode, the rail in its place,
    /// or nothing. Everything else in the frame reads it, so it comes first.
    pub(super) fn place_navigator(&mut self, window: &Window) {
        let mode = self.navigator_mode(window);
        let visible = self.navigator_visible(mode);
        self.nav.drawn = visible.then_some(mode);
        self.nav.rail = !visible && mode == Mode::Docked && !self.workers.is_empty();
        if !visible {
            self.nav.resize = None;
            self.nav.tick.take();
        }
    }

    /// The navigator's width, in points.
    #[must_use]
    pub const fn navigator_width(&self) -> f32 {
        self.layout.navigator().width
    }

    /// The panel's width in `mode`: a phone's drawer leaves a margin of the strip showing.
    pub(super) fn navigator_panel_width(&self, mode: Mode, window: &Window) -> f32 {
        match mode {
            Mode::Drawer => {
                let room = self.theme.spacing.xl.mul_add(-2.0, self.width(window));
                self.navigator_width().min(room)
            }
            Mode::Docked | Mode::Overlay => self.navigator_width(),
        }
    }

    /// A row was chosen: the inbox closes, and over the strip the navigator gets out of the
    /// way of what it chose.
    fn navigated(&mut self) {
        self.menu = None;
        self.nav.open = false;
        if self.layout.overview_open() {
            self.layout.set_overview(false);
        }
    }

    /// Fly to `tile` and focus it.
    pub(super) fn go_to_tile(&mut self, tile: TileRef, cx: &mut Context<Self>) {
        self.tick();
        self.navigated();
        self.focus_tile(tile, cx);
    }

    /// Go to an agent waiting on the human or at work: its tile, or, with none here, a new one
    /// on its worker (the same as ⌘⇧A does for one waiting).
    pub(super) fn go_to_waiting(&mut self, waiting: Waiting, cx: &mut Context<Self>) {
        self.tick();
        self.navigated();
        if waiting.tile.is_some() {
            self.reveal_session(waiting.session, cx);
            return;
        }
        self.show_untiled(waiting.worker, waiting.session, cx);
    }

    /// Switch to workspace `ix`.
    pub(super) fn go_to_workspace(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.tick();
        self.navigated();
        self.layout.focus_workspace(ix);
        self.after_focus_moved(cx);
        self.layout_touched(cx);
        cx.notify();
    }

    /// The handle moved to `x` (window points): the width follows, within its clamps.
    fn resize_navigator_to(&mut self, x: f32, cx: &mut Context<Self>) {
        let Some((grab, from)) = self.nav.resize else { return };
        let nav = self.layout.navigator();
        let width = Navigator::clamp_width(from + x - grab);
        if (width - nav.width).abs() > f32::EPSILON {
            self.layout.set_navigator(Navigator { width, ..nav });
            cx.notify();
        }
    }

    fn end_navigator_resize(&mut self, cx: &mut Context<Self>) {
        if self.nav.resize.take().is_some() {
            self.layout_touched(cx);
            cx.notify();
        }
    }

    /// A double-click on the handle: the width it had before it was ever dragged.
    fn reset_navigator_width(&mut self, cx: &mut Context<Self>) {
        let nav = self.layout.navigator();
        self.nav.resize = None;
        self.layout.set_navigator(Navigator { width: Navigator::DEFAULT_WIDTH, ..nav });
        self.layout_touched(cx);
        cx.notify();
    }

    /// Whether a coding agent runs in `item`'s terminal.
    pub(super) fn runs_agent(&self, item: &Item) -> bool {
        let ItemKind::Terminal { session } = item.kind else { return false };
        self.agent_state(session).is_some_and(|a| a.status != AgentStatus::None)
    }

    /// A tile's status mark, and whether it holds news the human has not looked at: a long
    /// command that finished unwatched, while the tile is not busy or waiting.
    pub(super) fn tile_marks(&self, tile: TileRef, item: &Item) -> (Option<Status>, bool) {
        let mark = self.tile_status(tile, item);
        let unwatched = match item.kind {
            ItemKind::Terminal { session } => self.finished.contains_key(&session),
            _ => false,
        };
        (mark, unwatched && !matches!(mark, Some(Status::Working | Status::NeedsYou)))
    }

    /// What `worker`'s tiles add up to.
    pub(super) fn worker_rollup(&self, worker: WorkerKey) -> Rollup {
        let mut rollup = Rollup::default();
        let Some(w) = self.workers.get(&worker) else { return rollup };
        for item in w.doc.items() {
            let tile = TileRef { worker, item: item.id };
            if self.layout.contains(tile) {
                let (mark, unseen) = self.tile_marks(tile, item);
                rollup.add(mark, unseen);
            }
        }
        rollup
    }

    /// What a tile's second line says, and its age: a shell's agent words or its command,
    /// then where it is (its directory and branch); a page's address; a file's directory; a
    /// note's progress or next line. Empty when nothing is worth a line: the worker's name is
    /// its header's, a home directory alone says nothing, and a kind is its icon's. Such a
    /// row is one line.
    ///
    /// An agent waiting on the human gives what it asks and not its state: the row's first line
    /// ends in "Needs approval" already, and "Needs approval: …" under it said the state twice.
    /// An agent at rest gives what it last said and no state at all, "Idle · done" having read
    /// as two states; its age runs from when it came to rest, the one thing its row should say.
    ///
    /// An agent's subagents at work fold into its words as a count (`cx` reads them).
    pub(super) fn tile_meta(
        &self,
        item: &Item,
        now: SystemTime,
        cx: &gpui::App,
    ) -> (String, Option<Duration>) {
        match &item.kind {
            ItemKind::Terminal { session } => {
                let (doing, since) = self.shell_doing(*session);
                // A shell named by its command ([`Self::number_twins`]) does not say it twice.
                let doing = doing.filter(|d| self.derived.get(&item.id) != Some(d));
                let doing = with_subagents(doing, self.live_subagents(*session, cx));
                let branch = self.summary(*session).and_then(|s| s.branch.as_deref());
                let place = self
                    .session_tail(*session)
                    .filter(|p| p != "~" || doing.is_some() || branch.is_some());
                let meta = meta_line([doing.as_deref(), place.as_deref(), branch]);
                (meta, age_at(since, now))
            }
            // The header's place; a page named by its address says nothing more.
            ItemKind::Browser { .. }
            | ItemKind::File { .. }
            | ItemKind::Folder { .. }
            | ItemKind::Review { .. }
            | ItemKind::Thread { .. } => (self.tile_place(item).unwrap_or_default(), None),
            ItemKind::Window { .. } | ItemKind::Display { .. } => (String::new(), None),
            ItemKind::Note { text } => {
                (note_meta(text, self.note_progress_of(item.id, text)), None)
            }
        }
    }

    /// What a shell's second line says it is doing (its agent's words, else its command) and
    /// when its age runs from (Unix milliseconds): an agent at rest counts from its last turn,
    /// anything else from its spawn.
    fn shell_doing(&self, session: slopty_core::SessionId) -> (Option<String>, u64) {
        let summary = self.summary(session);
        let state = self.agent_state(session).filter(|a| a.status != AgentStatus::None);
        // A followed conversation says more than the hooks: the call the agent is on
        // where the hooks name none, and the gist of its last answer once it stopped.
        let face = state.and_then(|_| self.face_summary(session));
        let marked_working = self.agent_mark(session) == Some(Status::Working);
        let resting = state.filter(|a| at_rest(a) && !marked_working);
        let agent = match (state, face) {
            (Some(a), _) if needs_human(a) => agent_ask_line(a),
            // The hook lags a face mid-turn: the call, or nothing yet, never "Idle"
            // under a row that says it works.
            (Some(a), face) if marked_working && at_rest(a) => face,
            (Some(a), Some(face))
                if matches!(a.status, AgentStatus::Working) && a.detail.is_none() =>
            {
                Some(face)
            }
            (Some(a), face) if at_rest(a) => {
                face.or_else(|| a.detail.clone()).as_deref().and_then(rest_words)
            }
            (a, _) => a.map(agent_status_text),
        };
        let command = state.is_none().then(|| self.last_command(session)).flatten();
        let since = resting.map_or_else(
            || summary.map_or(0, |s| s.started_ms.as_millis()),
            |a| a.since_ms.as_millis(),
        );
        (agent.or(command), since)
    }

    /// [`Self::tile_meta`] under the repository lens, where the header names the repository
    /// and not the worker: the worker's name, then for a shell its directory below the
    /// repository (nothing at its root) and its branch.
    fn tile_meta_by_repo(
        &self,
        tile: TileRef,
        item: &Item,
        now: SystemTime,
        cx: &gpui::App,
    ) -> (String, Option<Duration>) {
        let worker = self.worker_name(tile.worker);
        let ItemKind::Terminal { session } = item.kind else {
            let (meta, age) = self.tile_meta(item, now, cx);
            return (meta_line([Some(worker.as_str()), Some(meta.as_str())]), age);
        };
        let (doing, since) = self.shell_doing(session);
        let doing = with_subagents(doing, self.live_subagents(session, cx));
        let summary = self.summary(session);
        let branch = summary.and_then(|s| s.branch.as_deref());
        let place = match summary.and_then(|s| Some((s.repo.as_deref()?, s.cwd.as_deref()?))) {
            Some((repo, cwd)) => within(repo, cwd),
            None => self.session_tail(session).filter(|p| p != "~"),
        };
        let meta = meta_line([doing.as_deref(), Some(worker.as_str()), place.as_deref(), branch]);
        (meta, age_at(since, now))
    }

    /// The command a shell runs now, else the last one it ran, else the one that finished
    /// unwatched: its first line.
    pub(super) fn last_command(&self, session: slopty_core::SessionId) -> Option<String> {
        let shell = self.shell(session);
        let typed = shell.and_then(|s| s.running.clone().or_else(|| s.last.clone()));
        let typed = typed.or_else(|| self.finished.get(&session).map(|f| f.command.clone()))?;
        typed.lines().next().map(str::to_owned).filter(|l| !l.trim().is_empty())
    }

    /// Every worker's block as the filter leaves it: a worker whose name matches keeps every
    /// tile; otherwise only its matching tiles, and without one it is not listed. Tiles come
    /// in order of attention.
    fn nav_listing(&self, cx: &gpui::App) -> Vec<NavWorker> {
        let query = self.nav.filter.query.trim().to_lowercase();
        let order = self.reading_order();
        let now = SystemTime::now();
        let clock = cx.background_executor().now();
        let by_repo = self.layout.navigator().lens == NavLens::Repositories;
        let mut out = Vec::new();
        for (key, w) in &self.workers {
            let key = *key;
            let named = matches(&query, &[&w.name]);
            let roots: Vec<&str> = w.sessions.values().filter_map(|s| s.repo.as_deref()).collect();
            let mut rollup = Rollup::default();
            let mut tiles: Vec<(u8, NavTile)> = Vec::new();
            for &tile in order.iter().filter(|t| t.worker == key) {
                let Some(item) = w.doc.get(tile.item) else { continue };
                let (mark, unseen) = self.tile_marks(tile, item);
                rollup.add(mark, unseen);
                let title = self.tile_title(item);
                let (meta, age) = if by_repo {
                    self.tile_meta_by_repo(tile, item, now, cx)
                } else {
                    self.tile_meta(item, now, cx)
                };
                let repo = match &item.kind {
                    ItemKind::Terminal { session } => {
                        w.sessions.get(session).and_then(|s| s.repo.clone())
                    }
                    ItemKind::File { path } | ItemKind::Folder { path } => {
                        repo_of(path, roots.iter().copied()).map(str::to_owned)
                    }
                    _ => None,
                };
                let repo_named = repo.as_deref().map_or("", repo_name);
                if !named && !matches(&query, &[&title, &meta, repo_named]) {
                    continue;
                }
                let kind = kind_icon(item, self.runs_agent(item));
                let summary = match &item.kind {
                    ItemKind::Terminal { session } => self.summary(*session),
                    _ => None,
                };
                let changes = summary.and_then(|s| s.changes).and_then(line_changes);
                let progress = summary.and_then(|s| s.progress);
                let restored = summary.is_some_and(|s| s.restored.is_some());
                let running = match (mark, &item.kind) {
                    (Some(Status::Running), ItemKind::Terminal { session }) => {
                        self.running_for(*session).map(turn_label)
                    }
                    _ => None,
                };
                let shell_runs = running.is_some();
                let word = status_word(mark).filter(|_| !shell_runs).map(|word| {
                    match (word, &item.kind) {
                        (Status::NeedsYou, ItemKind::Terminal { session }) => self
                            .agent_state(*session)
                            .map_or_else(|| word.label().to_owned(), agent_status_word),
                        _ => word.label().to_owned(),
                    }
                });
                let row = NavTile {
                    tile,
                    kind,
                    mark,
                    word,
                    unseen,
                    title,
                    meta,
                    age: age.and_then(age_shown),
                    age_changes: age.map(until_age_changes),
                    running,
                    changes,
                    progress,
                    restored,
                    repo,
                };
                tiles.push((attention(mark, unseen), row));
            }
            if !named && tiles.is_empty() {
                continue;
            }
            // Stable: within a class the tiles keep their reading order.
            tiles.sort_by_key(|(class, _)| *class);
            let tiles = tiles.into_iter().map(|(_, t)| t).collect();
            let health = worker_health(&w.status);
            let rtt = health.is_none().then(|| slow_rtt(self.shown_rtt(w))).flatten();
            // Like the round trip, the path is named here only when it is worth a look: a DERP
            // relay that has held, never one a direct path is still being found beside.
            let relay = w
                .relay
                .notice(clock)
                .and_then(|_| w.relay.path())
                .filter(|_| health.is_none())
                .map(|path| path_label(path).0);
            let folded = query.is_empty() && self.nav.folded.contains(&key);
            let header = NavHeader {
                key,
                name: w.name.clone(),
                health,
                relay,
                rtt,
                warning: w.caps.as_ref().and_then(worker_warning),
                linked: w.link.is_some(),
                rollup,
                folded,
                gap: false,
            };
            out.push(NavWorker { header, tiles });
        }
        out
    }

    /// An agent's row data: what its tile is called (else what the agent says), its words,
    /// and its worker and directory. A waiting agent's words are what it asks, under the
    /// section that already says it waits.
    fn nav_agent(&self, at: Waiting, status: Status) -> NavAgent {
        let agent = self.agent_state(at.session);
        let calm = |a: &AgentEvent| matches!(a.status, AgentStatus::Idle | AgentStatus::Done);
        let words = agent
            .map(|a| {
                if needs_human(a) {
                    agent_ask_line(a).unwrap_or_default()
                } else if status == Status::Done {
                    // Under *To review*, which says it ended: what it said it did, else how long
                    // the turn ran.
                    self.face_summary(at.session)
                        .or_else(|| {
                            self.finished.get(&at.session).map(|f| kit::duration(f.elapsed))
                        })
                        .unwrap_or_default()
                } else if status == Status::Working && calm(a) {
                    // Listed as working by its face while the hook lags: what the face says,
                    // or nothing yet, never the hook's calm word.
                    self.face_summary(at.session).unwrap_or_default()
                } else {
                    agent_status_text(a)
                }
            })
            .unwrap_or_default();
        let title = at
            .tile
            .and_then(|t| Some(self.tile_title(self.item(t)?)))
            .unwrap_or_else(|| if words.is_empty() { "Agent".to_owned() } else { words.clone() });
        let cwd = self.session_tail(at.session);
        let worker = self.worker_name(at.worker);
        let place = meta_line([Some(worker.as_str()), cwd.as_deref()]);
        NavAgent {
            at,
            status,
            title,
            words,
            place,
            since_ms: agent.map_or(0, |a| a.since_ms.as_millis()),
        }
    }

    /// How many of `session`'s subagents are at it: working, waiting on their own work, or on
    /// the person. One that finished is its parent's history, not its present.
    fn live_subagents(&self, session: slopty_core::SessionId, cx: &gpui::App) -> usize {
        self.subagents(session, cx)
            .iter()
            .filter(|row| matches!(Rung::of(row), Rung::NeedsYou | Rung::Working | Rung::Waiting))
            .count()
    }

    /// The words an agent's row under *Working* says.
    #[cfg(test)]
    pub(super) fn working_words(&self, at: Waiting) -> String {
        self.nav_agent(at, Status::Working).words
    }

    /// The filter's field, made once there is a window to make it in. ↩ in it goes to the
    /// first tile listed.
    pub(super) fn ensure_navigator_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.nav.filter.input.is_some() {
            return;
        }
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Filter"));
        self.nav.filter.events = Some(cx.subscribe(&input, |this, input, event, cx| match event {
            InputEvent::Change => {
                this.nav.filter.query = input.read(cx).value().to_string();
                cx.notify();
            }
            InputEvent::PressEnter { .. } => {
                let first = this.nav_listing(cx).into_iter().flat_map(|w| w.tiles).next();
                if let Some(first) = first {
                    this.go_to_tile(first.tile, cx);
                }
            }
            InputEvent::Focus | InputEvent::Blur => {}
        }));
        self.nav.filter.input = Some(input);
    }

    /// Empty the filter, and give the keyboard back to the workspace.
    fn clear_navigator_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(input) = self.nav.filter.input.clone() {
            input.update(cx, |input, cx| input.set_value(String::new(), window, cx));
        }
        self.nav.filter.query.clear();
        self.pending_focus_self = true;
        cx.notify();
    }

    /// The navigator's region of the frame as [`Self::place_navigator`] placed it: the panel,
    /// the rail, or nothing.
    pub(super) fn render_navigator_region(
        &self,
        window: &Window,
        cx: &Draw<'_, Self>,
    ) -> gpui::AnyElement {
        match self.nav.drawn {
            Some(mode) => self.navigator_panel(mode, window, cx),
            None if self.nav.rail => self.navigator_rail(cx),
            None => gpui::Empty.into_any_element(),
        }
    }

    /// The top row, the title bar's height: room for the traffic lights on a Mac, then the
    /// filter's field, a well a row tall on the selection's fill, the base unit in from the
    /// panel's edge. The `raised` step sits a hundredth from the light panel's own tone, and the
    /// field read as words on nothing; `MonoCode`'s sidebar search wears its selection fill. No
    /// hairline under it: the panel is one surface from the top to the bottom, as T3 Code's and
    /// Linear's sidebars are, and the rows under the field need no rule to start.
    fn navigator_header(&self, window: &Window, cx: &Draw<'_, Self>) -> Div {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let spacing = theme.spacing;
        let safe = window.insets().effective();
        let leading = if cfg!(target_os = "macos") { LEADING_INSET } else { spacing.sm };
        let input = self.nav.filter.input.as_ref().map(|input| {
            div().debug_selector(|| "nav-filter".to_owned()).flex_1().min_w_0().child(
                Input::new(input)
                    .appearance(false)
                    .px_0()
                    .text_size(px(theme.typography.ui_size))
                    .aria_label("Filter"),
            )
        });
        let clear = (!self.nav.filter.query.is_empty()).then(|| {
            let el = div()
                .id("nav-filter-clear")
                .debug_selector(|| "nav-filter-clear".to_owned())
                .role(Role::Button)
                .aria_label("Clear filter")
                .flex_none()
                .size(px(theme.typography.icon_large()))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(theme.radii.xs))
                .cursor_pointer()
                .hover(move |el| el.bg(hsla(s.overlay)))
                .child(icon(theme, IconName::X, IconSize::Inline, hsla(s.text_muted)))
                .on_click(
                    cx.listener(|this, _ev, window, cx| this.clear_navigator_filter(window, cx)),
                );
            tab_stop(el, s.accent)
        });
        let field = div()
            .id("nav-filter-field")
            .flex_1()
            .min_w_0()
            // A touch row is nearly the title bar's height; the field keeps a step clear of
            // its edges, as it does on the Mac.
            .h(px(theme.density.row.min(spacing.xs.mul_add(-2.0, titlebar_height(theme)))))
            .px(px(spacing.sm))
            .flex()
            .items_center()
            .gap(px(spacing.xs + spacing.xxs))
            .rounded(px(theme.radii.sm))
            .bg(hsla(s.overlay))
            .child(icon(theme, IconName::Search, IconSize::Inline, hsla(s.text_muted)))
            .children(input)
            .children(clear);
        div()
            .flex_none()
            .h(px(titlebar_height(theme)) + safe.top)
            .pt(safe.top)
            .pl(px(leading))
            .pr(px(spacing.sm))
            .flex()
            .items_center()
            .child(field)
    }

    /// Every row the list holds this frame: on a phone *Workspaces*, then *Needs you* while an
    /// agent waits out of sight, *Working* while one is at its turn out of sight, then each
    /// worker's header and, unless it is folded, its tiles. The workers' own heading shows only
    /// under another section. While the filter holds something, a fold hides nothing.
    ///
    /// An agent is listed under *Needs you* or *Working* only while its own row is not there to
    /// say so: its tile is folded away, scrolled out of view, filtered out, or it has none. A
    /// tile in view already ends its first line in its state, and a second row for it said the
    /// same thing again.
    fn nav_rows(&self, cx: &gpui::App) -> Vec<NavRow> {
        let query = self.nav.filter.query.trim().to_lowercase();
        let listing = self.nav_listing(cx);
        let (blocks, heading) = match self.layout.navigator().lens {
            NavLens::Workers => {
                let blocks = listing
                    .into_iter()
                    .map(|NavWorker { header, tiles }| NavBlock {
                        folded: header.folded,
                        // A worker listed under a filter matched by its name; with no tiles it
                        // has nothing else to say, and "No tiles" there would read as the
                        // filter's answer.
                        vacant: (tiles.is_empty() && query.is_empty()).then_some(header.key),
                        head: NavRow::Worker(header),
                        tiles,
                    })
                    .collect::<Vec<_>>();
                (
                    blocks,
                    NavRow::Heading { selector: "nav-workers", text: "Workers", working: None },
                )
            }
            NavLens::Repositories => {
                let folded = |path: &str| query.is_empty() && self.nav.folded_repos.contains(path);
                let tiles = listing.into_iter().flat_map(|w| w.tiles);
                let heading = NavRow::Heading {
                    selector: "nav-repositories",
                    text: "Repositories",
                    working: None,
                };
                (repo_blocks(tiles, folded), heading)
            }
        };
        let listed: HashSet<TileRef> = blocks
            .iter()
            .filter(|b| !b.folded)
            .flat_map(|b| b.tiles.iter().map(|t| t.tile))
            .collect();
        let seen = |at: &Waiting| {
            at.tile.is_some_and(|tile| listed.contains(&tile) && self.nav.list.in_view(tile))
        };
        let mut rows = Vec::new();
        if query.is_empty() && self.nav.drawn == Some(Mode::Drawer) {
            rows.extend(self.space_rows());
        }
        let agents = |list: &[Waiting], status: Status| -> Vec<NavAgent> {
            list.iter()
                .filter(|at| !seen(at))
                .map(|at| {
                    let mut agent = self.nav_agent(*at, status);
                    let words = (!agent.words.is_empty()).then(|| std::mem::take(&mut agent.words));
                    let subagents = self.live_subagents(at.session, cx);
                    agent.words = with_subagents(words, subagents).unwrap_or_default();
                    agent
                })
                .filter(|a| matches(&query, &[&a.title, &a.words, &a.place]))
                .collect()
        };
        let waiting = agents(&self.drawn_waiting, Status::NeedsYou);
        if !waiting.is_empty() {
            rows.push(NavRow::Heading {
                selector: "nav-needs-you",
                text: "Needs you",
                working: None,
            });
            rows.extend(waiting.into_iter().map(NavRow::Agent));
        }
        let review = agents(&self.to_review(), Status::Done);
        if !review.is_empty() {
            rows.push(NavRow::Heading {
                selector: "nav-to-review",
                text: "To review",
                working: None,
            });
            rows.extend(review.into_iter().map(NavRow::Agent));
        }
        let working = agents(&self.working(), Status::Working);
        if !working.is_empty() {
            let count = working.len();
            rows.push(NavRow::Heading {
                selector: "nav-working",
                text: "Working",
                working: Some(count),
            });
            let shown = if self.nav.working_all { count } else { WORKING_SHOWN };
            rows.extend(working.into_iter().take(shown).map(NavRow::Agent));
            if count > shown {
                rows.push(NavRow::More(count.saturating_sub(shown)));
            }
        }
        if !blocks.is_empty() && !rows.is_empty() {
            rows.push(heading);
        }
        for NavBlock { mut head, folded, vacant, tiles } in blocks {
            // A block after another's rows stands a step off them; under a heading, or first,
            // it needs none.
            let gap = matches!(
                rows.last(),
                Some(NavRow::Worker(_) | NavRow::Repo(_) | NavRow::Tile(_) | NavRow::Vacant(_))
            );
            match &mut head {
                NavRow::Worker(header) => header.gap = gap,
                NavRow::Repo(repo) => repo.gap = gap,
                _ => {}
            }
            rows.push(head);
            if folded {
                continue;
            }
            rows.extend(vacant.map(NavRow::Vacant));
            rows.extend(tiles.into_iter().map(NavRow::Tile));
        }
        if rows.is_empty() {
            rows.push(NavRow::Nothing);
        }
        rows
    }

    /// *Workspaces*, as a phone's drawer heads its list: each workspace the title bar would tab,
    /// named, with its tile count, and "New workspace" unless the active one is that new one.
    fn space_rows(&self) -> Vec<NavRow> {
        let active = self.layout.active_workspace();
        let spaces: Vec<NavRow> = self
            .tabbed_workspaces()
            .into_iter()
            .map(|ix| {
                let (_, tiles) = self.workspace_rollup(ix);
                let name = self.workspace_name_at(ix);
                NavRow::Space(NavSpace { ix, name, tiles, active: ix == active })
            })
            .collect();
        let fresh = self.layout.workspaces().get(active).is_some_and(|ws| ws.columns().is_empty());
        let heading =
            NavRow::Heading { selector: "nav-workspaces", text: "Workspaces", working: None };
        std::iter::once(heading).chain(spaces).chain((!fresh).then_some(NavRow::NewSpace)).collect()
    }

    /// Draw the navigator again when the soonest label it shows changes: a turn's time every
    /// second while *Working* ticks, else an age at its next minute (or hour).
    fn schedule_navigator_tick(&self, cx: &gpui::App) {
        let rows = self.nav.list.rows.borrow();
        // A turn's time and a command's are the readouts' clock's to move
        // ([`WorkspaceView::keep_time`]); an age at rest changes on its own schedule.
        let now = now_ms();
        let waited = rows.iter().filter_map(|r| match r {
            NavRow::Agent(a) if a.since_ms > 0 && a.status != Status::Working => {
                Some(until_age_changes(Duration::from_millis(now.saturating_sub(a.since_ms))))
            }
            _ => None,
        });
        let aged = rows.iter().filter_map(|r| match r {
            NavRow::Tile(t) => t.age_changes,
            _ => None,
        });
        let next = waited.chain(aged).min();
        *self.nav.tick.borrow_mut() = next.map(|wait| {
            let navigator = self.chrome.nav_rows.downgrade();
            cx.spawn(async move |cx| {
                cx.background_executor().timer(wait).await;
                let _gone = navigator.update(cx, |_, cx| cx.notify());
            })
        });
    }

    /// Row `ix` of the list, drawn only while it is in view or measured. The list lays a row
    /// out at its own size; the wrapper gives it the list's width, less its margins.
    fn nav_row(&self, ix: usize, cx: &Draw<'_, Self>) -> gpui::AnyElement {
        let gap = match self.nav.list.rows.borrow().get(ix) {
            Some(NavRow::Worker(h)) => h.gap,
            Some(NavRow::Repo(r)) => r.gap,
            _ => false,
        };
        div()
            .w_full()
            .flex()
            .flex_col()
            .when(gap, |el| el.pt(px(self.theme.spacing.sm)))
            .child(self.nav_row_content(ix, cx))
            .into_any_element()
    }

    fn nav_row_content(&self, ix: usize, cx: &Draw<'_, Self>) -> gpui::AnyElement {
        let theme = &self.theme;
        let rows = self.nav.list.rows.borrow();
        match rows.get(ix) {
            Some(NavRow::Heading { selector, text, working }) => {
                heading(theme, selector, text, *working).into_any_element()
            }
            Some(NavRow::Agent(agent)) => self.agent_row(agent, cx),
            Some(NavRow::More(hidden)) => self.more_row(*hidden, cx),
            Some(NavRow::Space(space)) => self.space_row(space, cx),
            Some(NavRow::NewSpace) => self.new_space_row(cx),
            Some(NavRow::Worker(header)) => self.worker_header(header, cx),
            Some(NavRow::Repo(repo)) => self.repo_header(repo, cx),
            Some(NavRow::Tile(tile)) => self.tile_row(tile, self.nav.list.selected.get(), cx),
            Some(NavRow::Vacant(key)) => {
                let key = *key;
                // On the tiles' titles' edge: past the worker's glyph and its gap, as a tile's
                // title stands past its kind's.
                let title_edge = theme.spacing.inset()
                    + theme.typography.icon_large().mul_add(2.0, -glyph_margin(theme))
                    + theme.spacing.xs;
                meta(div(), theme)
                    .id(ElementId::Name(format!("nav-vacant-{key}").into()))
                    .debug_selector(move || format!("nav-vacant-{key}"))
                    .role(Role::Label)
                    .aria_label(NO_TILES)
                    .h(px(kit::Row::One.height(theme)))
                    .flex()
                    .items_center()
                    .pl(px(theme.spacing.xs + title_edge))
                    .child(NO_TILES)
                    .into_any_element()
            }
            Some(NavRow::Nothing) => {
                crate::palette::quiet_line(theme, "nav-nothing", crate::picker::NOTHING_MATCHES)
                    .into_any_element()
            }
            None => div().into_any_element(),
        }
    }

    /// The panel itself, docked or laid over the frame. The frame places it and sizes it
    /// (`WorkspaceView::render_frame`); this fills that box.
    fn navigator_panel(
        &self,
        mode: Mode,
        window: &Window,
        cx: &Draw<'_, Self>,
    ) -> gpui::AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let safe = window.insets().effective();
        let rows = div().flex_1().min_h_0().flex().flex_col().child(self.chrome.nav_rows.clone());
        div()
            .id("navigator")
            .debug_selector(|| "navigator".to_owned())
            .role(Role::Navigation)
            .aria_label("Navigator")
            .occlude()
            .size_full()
            .pl(if mode == Mode::Docked { px(0.0) } else { safe.left })
            // Laid over the frame it runs through the home indicator's band; its rows stop
            // above it.
            .when(mode != Mode::Docked, |panel| panel.pb(safe.bottom))
            .flex()
            .flex_col()
            // The bars' surface: the navigator is chrome, one surface with them, and the panel
            // step is left to the unfocused tiles' headers.
            .bg(hsla(s.canvas))
            .border_r_1()
            .border_color(hsla(s.border))
            .font_family(theme.typography.ui_family.clone())
            // Over the frame it floats, as every floating layer does. It meets the window's
            // top, left and bottom edges, so only its trailing edge carries the hairline.
            .when(mode != Mode::Docked, |panel| kit::elevate(panel, theme).border_0().border_r_1())
            // Esc in the filter empties it and hands the keyboard back.
            .capture_action(cx.listener(|this, _: &Escape, window, cx| {
                let composing =
                    this.nav.filter.input.as_ref().is_some_and(|i| i.read(cx).is_composing());
                if !this.nav.filter.query.is_empty() && !composing {
                    this.clear_navigator_filter(window, cx);
                    cx.stop_propagation();
                }
            }))
            .child(self.navigator_header(window, cx))
            .child(rows)
            .into_any_element()
    }

    /// The panel's rows, for a view of their own ([`super::Region::NavigatorRows`]): the list's
    /// wheel builds the view that drew it again, and that is this one, not the panel with its
    /// filter field, whose input writes its own state each time it is built.
    pub(super) fn render_navigator_rows(
        &self,
        window: &Window,
        cx: &Draw<'_, Self>,
    ) -> gpui::AnyElement {
        let rows = self.nav_rows(cx);
        self.nav.list.set_rows(rows);
        self.nav.list.selected.set(self.focused());
        self.nav.list.reveal_selected(window);
        self.schedule_navigator_tick(cx);
        let theme = &self.theme;
        let moves = self.chrome_moves(cx);
        let rows = list(
            self.nav.list.state.clone(),
            cx.processor(|this, ix: usize, _window, cx| this.nav_row(ix, cx)),
        )
        .flex_1()
        .min_h_0()
        .pt(px(theme.spacing.xs))
        .pb(px(theme.spacing.md));
        div()
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .child(self.nav.list.space_plate.under_on(theme, moves, Some(self.clock_instant())))
            .child(self.nav.list.plate.under_on(theme, moves, Some(self.clock_instant())))
            .child(rows)
            .into_any_element()
    }

    /// Where the navigator is hidden but would dock: a column of one server glyph per worker,
    /// each with what its tiles add up to under it. A click flies to the worker.
    fn navigator_rail(&self, cx: &Draw<'_, Self>) -> gpui::AnyElement {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let spacing = theme.spacing;
        let side = kit::icon_button_side(theme);
        let buttons: Vec<gpui::AnyElement> = self
            .workers
            .iter()
            .map(|(key, w)| {
                let key = *key;
                let rollup = self.worker_rollup(key);
                let health = worker_health(&w.status);
                let label = [Some(w.name.clone()), health.map(|(_, word)| word.to_owned())]
                    .into_iter()
                    .flatten()
                    .chain(rollup.words())
                    .collect::<Vec<_>>()
                    .join(", ");
                let (glyph, ink) = match health {
                    Some((Status::Away, _)) => (IconName::ServerOff, s.text_muted),
                    _ => (IconName::Server, s.text_secondary),
                };
                let badge = rollup_slot(theme, format!("nav-rail-rollup-{key}"), rollup, true)
                    .absolute()
                    .right_0()
                    .bottom_0();
                let el = div()
                    .id(ElementId::Name(format!("nav-rail-{key}").into()))
                    .debug_selector(move || format!("nav-rail-{key}"))
                    .role(Role::Button)
                    .aria_label(SharedString::from(label))
                    .relative()
                    .flex_none()
                    .size(px(side))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(theme.radii.sm))
                    .cursor_pointer()
                    .hover(move |el| el.bg(hsla(s.raised)))
                    .child(icon(theme, glyph, IconSize::Inline, hsla(ink)))
                    .child(badge)
                    .on_click(cx.listener(move |this, _ev, _w, cx| {
                        this.tick();
                        this.navigated();
                        this.go_to_worker(key, cx);
                    }));
                tab_stop(el, s.accent).into_any_element()
            })
            .collect();
        div()
            .id("nav-rail")
            .debug_selector(|| "nav-rail".to_owned())
            .role(Role::Navigation)
            .aria_label("Workers")
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .pt(px(spacing.sm))
            .gap(px(spacing.xs))
            .bg(hsla(s.canvas))
            .border_r_1()
            .border_color(hsla(s.border))
            .children(buttons)
            .into_any_element()
    }

    /// The handle on the navigator's edge, `x` its centre in the frame: a 12 pt strip around
    /// the 1 pt edge, drawn over the strip so its outer half takes the pointer too. Pressed, it
    /// drags the width; double-clicked, it puts the width back. The listeners that follow the
    /// pointer are there in every frame the handle is, so the first move after the press is
    /// followed without waiting for a frame.
    pub(super) fn render_handle(x: gpui::Pixels, cx: &Context<Self>) -> gpui::AnyElement {
        let entity = cx.entity().downgrade();
        let follow = canvas(
            |_bounds, _window, _cx| (),
            move |_bounds, (), window, _cx| {
                let moved = entity.clone();
                window.on_mouse_event(move |ev: &MouseMoveEvent, phase, _window, cx| {
                    let resizing =
                        moved.read_with(cx, |this, _| this.nav.resize.is_some()).unwrap_or(false);
                    if phase != gpui::DispatchPhase::Capture || !resizing {
                        return;
                    }
                    let _gone = moved.update(cx, |this, cx| {
                        if ev.pressed_button == Some(MouseButton::Left) {
                            this.resize_navigator_to(f32::from(ev.position.x), cx);
                        } else {
                            this.end_navigator_resize(cx);
                        }
                    });
                });
                let released = entity;
                window.on_mouse_event(move |_ev: &MouseUpEvent, phase, _window, cx| {
                    if phase == gpui::DispatchPhase::Capture {
                        let _gone = released.update(cx, Self::end_navigator_resize);
                    }
                });
            },
        )
        .absolute()
        .size_0();
        div()
            .id("navigator-handle")
            .debug_selector(|| "navigator-handle".to_owned())
            .absolute()
            .top_0()
            .bottom_0()
            .left(x - px(HANDLE_W / 2.0))
            .w(px(HANDLE_W))
            .cursor_col_resize()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, ev: &gpui::MouseDownEvent, _w, cx| {
                    cx.stop_propagation();
                    if ev.click_count >= 2 {
                        this.reset_navigator_width(cx);
                        return;
                    }
                    this.nav.resize = Some((f32::from(ev.position.x), this.navigator_width()));
                    cx.notify();
                }),
            )
            .child(follow)
            .into_any_element()
    }

    /// `key`'s name, or nothing for a worker no longer known.
    pub(super) fn worker_name(&self, key: WorkerKey) -> String {
        self.workers.get(&key).map(|w| w.name.clone()).unwrap_or_default()
    }

    /// A row of *Needs you* or *Working*: what it is and how long it has waited or worked,
    /// then what the agent says in its status's tone, its worker and its directory.
    fn agent_row(&self, agent: &NavAgent, cx: &Draw<'_, Self>) -> gpui::AnyElement {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let (first, second) = line_heights(theme);
        let session = agent.at.session;
        let prefix = match agent.status {
            Status::NeedsYou => "nav-waiting",
            Status::Done => "nav-review",
            _ => "nav-working",
        };
        let label = [agent.title.as_str(), agent.words.as_str(), agent.status.label()]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(", ");
        // A turn's time counts by the readouts' clock, which runs only while something works;
        // an age at rest by the wall, the navigator's own tick drawing it again as it changes.
        let working = agent.status == Status::Working;
        let now = if working { self.ticked().map(|(_, now)| now) } else { Some(now_ms()) };
        let time = now
            .filter(|_| agent.since_ms > 0)
            .map(|now| Duration::from_millis(now.saturating_sub(agent.since_ms)))
            .and_then(
                |elapsed| if working { Some(turn_label(elapsed)) } else { age_shown(elapsed) },
            )
            .map(|text| {
                readout(theme, text).debug_selector(move || format!("{prefix}-time-{session}"))
            });
        let line1 = div()
            .h(px(first))
            .line_height(px(first))
            .flex()
            .items_center()
            .gap(px(theme.spacing.xs))
            .child(title(agent.title.clone(), hsla(s.text)))
            .children(time);
        // The agent's words, then where it runs, as a tile's second line reads: joined by the
        // same separator, spaces and all, so the two lines space their parts alike. A working
        // agent's words take its tone; what a waiting one asks is detail, muted, under the
        // heading that says it waits.
        let tone = (agent.status == Status::Working).then(|| hsla(agent.status.tone(theme)));
        let words = (!agent.words.is_empty()).then(|| {
            div()
                .debug_selector(move || format!("{prefix}-words-{session}"))
                .flex_none()
                .when_some(tone, gpui::Styled::text_color)
                .child(agent.words.clone())
        });
        let separator = (!agent.words.is_empty() && !agent.place.is_empty()).then(|| {
            div()
                .debug_selector(move || format!("{prefix}-separator-{session}"))
                .flex_none()
                .text_color(crate::palette::separator_ink(theme))
                .child(META_SEPARATOR)
        });
        let line2 = meta(div(), theme)
            .h(px(second))
            .line_height(px(second))
            .flex()
            .items_center()
            .overflow_hidden()
            .whitespace_nowrap()
            .children(words)
            .children(separator)
            .child(
                div()
                    .debug_selector(move || format!("{prefix}-place-{session}"))
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(crate::palette::dotted(theme, agent.place.clone())),
            );
        let waiting = agent.at;
        row(
            theme,
            kit::Row::Two,
            ElementId::Name(format!("{prefix}-{session}").into()),
            format!("{prefix}-{session}"),
            label.into(),
            false,
        )
        .items_start()
        .pt(px(theme.spacing.xs))
        .child(lead_slot(theme, icon(theme, IconName::Bot, IconSize::Inline, hsla(s.text_muted))))
        .child(div().flex_1().min_w_0().flex().flex_col().child(line1).child(line2))
        .on_click(cx.listener(move |this, _ev, _w, cx| this.go_to_waiting(waiting, cx)))
        .into_any_element()
    }

    /// A workspace in the phone's drawer: its glyph, its name and how many tiles it holds. The
    /// active one sits on its plate; a press goes there and closes the drawer.
    fn space_row(&self, space: &NavSpace, cx: &Draw<'_, Self>) -> gpui::AnyElement {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let ix = space.ix;
        let count = match space.tiles {
            1 => "1 tile".to_owned(),
            n => format!("{n} tiles"),
        };
        let label = SharedString::from(format!("{}, {count}", space.name));
        let ink = if space.active { s.text } else { s.text_secondary };
        let row = row(
            theme,
            kit::Row::One,
            ElementId::Name(format!("nav-space-{ix}").into()),
            format!("nav-space-{ix}"),
            label,
            space.active,
        )
        .map(|row| if space.active { self.nav.list.space_plate.seat(row, ix, theme) } else { row })
        .child(lead_slot(
            theme,
            icon(theme, IconName::PanelsTopLeft, IconSize::Inline, hsla(s.text_muted)),
        ))
        .child(
            title(space.name.clone(), hsla(ink)).when(space.active, |el| {
                el.font_weight(gpui::FontWeight(Typography::MEDIUM_WEIGHT))
            }),
        )
        .child(readout(theme, space.tiles.to_string()))
        .on_click(cx.listener(move |this, _ev, _w, cx| this.go_to_workspace(ix, cx)));
        row.into_any_element()
    }

    /// "New workspace", the last row of the phone's *Workspaces*: the empty workspace the
    /// layout always keeps last.
    fn new_space_row(&self, cx: &Draw<'_, Self>) -> gpui::AnyElement {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let text = super::strip::NEW_WORKSPACE;
        row(theme, kit::Row::One, "nav-new-space", "nav-new-space".to_owned(), text.into(), false)
            .text_color(hsla(s.text_secondary))
            .child(lead_slot(
                theme,
                icon(theme, IconName::Plus, IconSize::Inline, hsla(s.text_muted)),
            ))
            .child(title(text, hsla(s.text_secondary)))
            .on_click(cx.listener(|this, _ev, _w, cx| {
                let last = this.layout.workspaces().len().saturating_sub(1);
                this.go_to_workspace(last, cx);
            }))
            .into_any_element()
    }

    /// "Show N more" under the first few of *Working*.
    fn more_row(&self, hidden: usize, cx: &Draw<'_, Self>) -> gpui::AnyElement {
        let theme = &self.theme;
        let text = SharedString::from(format!("Show {hidden} more"));
        row(
            theme,
            kit::Row::One,
            "nav-working-more",
            "nav-working-more".to_owned(),
            text.clone(),
            false,
        )
        .text_size(px(theme.typography.small()))
        .text_color(hsla(theme.surfaces.text_secondary))
        .child(lead_slot(theme, div()))
        .child(tabular(div()).child(text))
        .on_click(cx.listener(|this, _ev, _w, cx| {
            this.nav.working_all = true;
            cx.notify();
        }))
        .into_any_element()
    }

    /// A worker's header: the server icon (crossed out, in the warn fill, while it is away),
    /// the name, then on the right edge what is wrong with its link or its round trip when it
    /// is slow, led by what a folded worker's tiles add up to. Under the pointer the chevron
    /// and "+" take the readouts' place; nothing moves when either shows.
    fn worker_header(&self, worker: &NavHeader, cx: &Draw<'_, Self>) -> gpui::AnyElement {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let key = worker.key;
        let folded = worker.folded;
        let label = SharedString::from(format!(
            "{}{}{}{}{}",
            worker.name,
            worker.health.map(|(_, word)| format!(", {word}")).unwrap_or_default(),
            worker.relay.as_ref().map(|relay| format!(", {relay}")).unwrap_or_default(),
            worker.warning.as_ref().map(|warning| format!(", {warning}")).unwrap_or_default(),
            if folded { ", folded" } else { "" }
        ));
        let lead =
            match worker.health {
                None => lead_slot(
                    theme,
                    icon(theme, IconName::Server, IconSize::Inline, hsla(s.text_muted)),
                ),
                Some((Status::Away, _)) => lead_slot(
                    theme,
                    div().id("away").role(Role::Image).aria_label(Status::Away.label()).child(
                        icon(theme, IconName::ServerOff, IconSize::Inline, hsla(s.text_muted)),
                    ),
                ),
                Some((mark, _)) => lead_slot(theme, status_mark(theme, Some(mark), 1.0)),
            };
        let name = div()
            .debug_selector(move || format!("nav-worker-name-{key}"))
            .flex_1()
            .min_w_0()
            .overflow_hidden()
            .whitespace_nowrap()
            .text_ellipsis()
            .font_weight(gpui::FontWeight(Typography::MEDIUM_WEIGHT))
            .text_color(hsla(s.text))
            .child(SharedString::from(worker.name.clone()));
        // Only when something is wrong with the worker itself: a line under its name, in the
        // warning tone.
        let name = match worker.warning.clone() {
            None => name,
            Some(warning) => div().flex_1().min_w_0().flex().flex_col().child(name).child(
                meta(div(), theme)
                    .debug_selector(move || format!("nav-worker-warn-{key}"))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_color(hsla(s.warn))
                    .child(warning),
            ),
        };
        let group = SharedString::from(format!("nav-worker-group-{key}"));
        // At rest: the readouts on the row's right edge, where the tiles' words end, and
        // before them what a folded worker's tiles add up to. They grow leftwards, so a rollup
        // coming or going moves nothing after it.
        let rollup = folded.then_some(worker.rollup).filter(|r| r.shown().is_some());
        let rest =
            div()
                .flex()
                .items_center()
                .justify_end()
                .gap(px(theme.spacing.xs))
                .group_hover(group.clone(), gpui::Styled::invisible)
                .children(rollup.map(|r| rollup_slot(theme, format!("nav-rollup-{key}"), r, false)))
                .children(worker.health.map(|(_, word)| readout(theme, word)))
                .children(worker.relay.clone().map(|relay| {
                    readout(theme, relay).debug_selector(move || format!("nav-path-{key}"))
                }))
                .children(worker.rtt.clone().map(|rtt| {
                    readout(theme, rtt).debug_selector(move || format!("nav-rtt-{key}"))
                }));
        let chevron = if folded { IconName::ChevronRight } else { IconName::ChevronDown };
        let side = theme.typography.icon_large();
        let add = worker.linked.then(|| {
            let el = div()
                .id(SharedString::from(format!("nav-new-shell-{key}")))
                .debug_selector(move || format!("nav-new-shell-{key}"))
                .role(Role::Button)
                .aria_label(SharedString::from(format!("New shell on {}", worker.name)))
                .flex_none()
                .size(px(side))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(theme.radii.xs))
                .cursor_pointer()
                .hover(move |el| el.bg(hsla(s.overlay)))
                .child(icon(theme, IconName::Plus, IconSize::Inline, hsla(s.text_secondary)))
                .on_mouse_down(MouseButton::Left, |_ev, _w, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this, _ev, _w, cx| {
                    cx.stop_propagation();
                    this.open_session_on(key, None, Vec::new(), None, cx);
                }));
            tab_stop(el, s.accent)
        });
        // Under the pointer: the chevron and "+" in their fixed places over the readouts.
        let hover = div()
            .absolute()
            .top_0()
            .bottom_0()
            .right_0()
            .flex()
            .items_center()
            .justify_end()
            .gap(px(theme.spacing.xxs))
            .invisible()
            .group_hover(group.clone(), gpui::Styled::visible)
            .child(lead_slot(theme, icon(theme, chevron, IconSize::Inline, hsla(s.text_muted))))
            .children(add);
        let trailing = div()
            .debug_selector(move || format!("nav-worker-slot-{key}"))
            .relative()
            .flex_none()
            .min_w(px(header_actions_width(theme)))
            .h(px(side))
            .flex()
            .items_center()
            .justify_end()
            .child(rest)
            .child(hover);
        let lines = if worker.warning.is_some() { kit::Row::Two } else { kit::Row::One };
        row(
            theme,
            lines,
            ElementId::Name(format!("nav-worker-{key}").into()),
            format!("nav-worker-{key}"),
            label,
            false,
        )
        .group(group)
        .child(lead)
        .child(name)
        .child(trailing)
        .on_click(cx.listener(move |this, _ev, _w, cx| {
            if !this.nav.folded.remove(&key) {
                this.nav.folded.insert(key);
            }
            cx.notify();
        }))
        .into_any_element()
    }

    /// A repository's header under the repository lens: its name in the strong weight after a
    /// repository glyph, where it is when another listed has its name, what its tiles add up to
    /// while it is folded, and the chevron under the pointer. A click folds it.
    fn repo_header(&self, repo: &NavRepo, cx: &Draw<'_, Self>) -> gpui::AnyElement {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let path = repo.path.clone();
        let key = if path.is_empty() { "none".to_owned() } else { path.clone() };
        let folded = repo.folded;
        let label = SharedString::from(format!(
            "{}{}{}",
            repo.name,
            repo.parent.as_ref().map(|p| format!(", in {p}")).unwrap_or_default(),
            if folded { ", folded" } else { "" }
        ));
        let glyph = if path.is_empty() { IconName::Folder } else { IconName::FolderGit2 };
        let lead = lead_slot(theme, icon(theme, glyph, IconSize::Inline, hsla(s.text_muted)));
        let name_key = key.clone();
        let name = div()
            .debug_selector(move || format!("nav-repo-name-{name_key}"))
            .flex_1()
            .min_w_0()
            .overflow_hidden()
            .whitespace_nowrap()
            .text_ellipsis()
            .font_weight(gpui::FontWeight(Typography::MEDIUM_WEIGHT))
            .text_color(hsla(s.text))
            .child(SharedString::from(repo.name.clone()));
        let group = SharedString::from(format!("nav-repo-group-{key}"));
        let rollup = folded.then_some(repo.rollup).filter(|r| r.shown().is_some());
        let rollup_key = key.clone();
        let rest = div()
            .flex()
            .items_center()
            .justify_end()
            .gap(px(theme.spacing.xs))
            .group_hover(group.clone(), gpui::Styled::invisible)
            .children(
                rollup.map(|r| rollup_slot(theme, format!("nav-rollup-{rollup_key}"), r, false)),
            )
            .children(repo.parent.clone().map(|parent| readout(theme, parent)));
        let chevron = if folded { IconName::ChevronRight } else { IconName::ChevronDown };
        let hover = div()
            .absolute()
            .top_0()
            .bottom_0()
            .right_0()
            .flex()
            .items_center()
            .justify_end()
            .invisible()
            .group_hover(group.clone(), gpui::Styled::visible)
            .child(lead_slot(theme, icon(theme, chevron, IconSize::Inline, hsla(s.text_muted))));
        let trailing = div()
            .relative()
            .flex_none()
            .min_w(px(header_actions_width(theme)))
            .h(px(theme.typography.icon_large()))
            .flex()
            .items_center()
            .justify_end()
            .child(rest)
            .child(hover);
        row(
            theme,
            kit::Row::One,
            ElementId::Name(format!("nav-repo-{key}").into()),
            format!("nav-repo-{key}"),
            label,
            false,
        )
        .group(group)
        .child(lead)
        .child(name)
        .child(trailing)
        .on_click(cx.listener(move |this, _ev, _w, cx| {
            if !this.nav.folded_repos.remove(&path) {
                this.nav.folded_repos.insert(path.clone());
            }
            cx.notify();
        }))
        .into_any_element()
    }

    /// A tile's row: its status glyph (else its kind), its title and at the end of that line
    /// the unseen dot, a running command's clock or its age, then the muted second line with
    /// the working tree's changes right-aligned under the line's end, as Zed's and `MonoCode`'s
    /// thread rows have them. Under the pointer the line's end gives way to a close button.
    fn tile_row(
        &self,
        t: &NavTile,
        focused: Option<TileRef>,
        cx: &Draw<'_, Self>,
    ) -> gpui::AnyElement {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let selected = focused == Some(t.tile);
        let label = [Some(t.title.as_str()), t.word.as_deref(), t.unseen.then_some("unseen")]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(", ");
        let ink = if selected { s.text } else { s.text_secondary };
        let id = t.tile.item.as_uuid();
        let (first, second) = line_heights(theme);
        // A row at work recedes until the pointer is on it: its inks at a share, so its wash
        // stays whole and nothing is drawn through a layer.
        let strength = row_strength(theme, t.mark, selected);
        let row_group = SharedString::from(format!("nav-tile-group-{id}"));
        let faded = move |tone: Rgb| crate::colors::hsla_alpha(tone, strength);
        // The status glyph, as Warp's agent rows lead with one: working, waiting on the person,
        // done, failed, away; one at rest shows its kind. The state is the glyph and never a
        // word on the title's line, where "Needs approval" took half a row's width from the
        // title; the second line says what is asked.
        let glyph = t.mark.filter(|m| *m != Status::Idle);
        let lead = crate::palette::status_slot(theme, t.kind, glyph, faded(s.text_muted), 1.0)
            .debug_selector(move || format!("nav-kind-{id}"));
        // One mark at the line's end: the unseen dot, else the clock of a command that runs,
        // else the age.
        let end = if t.unseen {
            Some(unseen_dot(theme, format!("nav-unseen-{id}"), true).into_any_element())
        } else if let Some(ran) = t.running.clone() {
            Some(
                readout(theme, ran)
                    .debug_selector(move || format!("nav-running-{id}"))
                    .text_color(hsla(s.text_secondary))
                    .into_any_element(),
            )
        } else {
            t.age.clone().map(|age| {
                readout(theme, age)
                    .debug_selector(move || format!("nav-age-{id}"))
                    .into_any_element()
            })
        };
        // Under the pointer the line's end gives way to the row's action, in the same place, so
        // nothing on the line moves.
        let tile = t.tile;
        let close = kit::icon_button_at(
            theme,
            format!("nav-close-{id}"),
            IconName::X,
            super::tile::CLOSE_TILE,
            1.0,
        )
        .on_mouse_down(MouseButton::Left, |_ev, _w, cx| cx.stop_propagation())
        .on_click(cx.listener(move |this, _ev, window, cx| {
            cx.stop_propagation();
            this.close_tile(tile, window, cx);
        }));
        let end = div()
            .relative()
            .flex_none()
            .min_w(px(theme.typography.icon_large()))
            .h(px(first))
            .flex()
            .items_center()
            .justify_end()
            .child(
                div()
                    .flex()
                    .items_center()
                    .group_hover(row_group.clone(), gpui::Styled::invisible)
                    .children(end),
            )
            .child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .right_0()
                    .flex()
                    .items_center()
                    .invisible()
                    .group_hover(row_group.clone(), gpui::Styled::visible)
                    .child(close),
            );
        let line1 = div()
            .h(px(first))
            .line_height(px(first))
            .flex()
            .items_center()
            .gap(px(theme.spacing.xs))
            .child(
                title(t.title.clone(), faded(ink))
                    .group_hover(row_group.clone(), move |st| st.text_color(hsla(ink)))
                    .when(selected, |el| {
                        el.font_weight(gpui::FontWeight(Typography::MEDIUM_WEIGHT))
                    }),
            )
            .child(end);
        // The working tree's changes end the line whole; the words before them give way.
        let changes = t
            .changes
            .and_then(|(added, removed)| kit::changes(theme, added, removed))
            .map(|changes| changes.debug_selector(move || format!("nav-changes-{id}")));
        let restored = t.restored.then(|| {
            div().debug_selector(move || format!("nav-restored-{id}")).flex_none().child(RESTORED)
        });
        let figure = t.progress.and_then(progress_figure).map(|figure| {
            readout(theme, figure).debug_selector(move || format!("nav-progress-{id}"))
        });
        let bar =
            t.progress.and_then(|p| progress_line(theme, p)).map(|(tone, strength, share)| {
                div()
                .debug_selector(move || format!("nav-progress-bar-{id}"))
                .absolute()
                // Just under the second line, clear of its descenders.
                .bottom(px(-theme.spacing.xxs))
                .left_0()
                .h(px(theme.spacing.xxs))
                .w(gpui::relative(share))
                .bg(crate::colors::hsla_alpha(tone, strength))
            });
        let line2 = t.two_lines().then(|| {
            meta(div(), theme)
                .text_color(faded(s.text_muted))
                .group_hover(row_group.clone(), move |st| st.text_color(hsla(s.text_muted)))
                .h(px(second))
                .line_height(px(second))
                .flex()
                .items_center()
                .gap(px(theme.spacing.xs))
                .overflow_hidden()
                .whitespace_nowrap()
                .child(
                    div()
                        .debug_selector(move || format!("nav-meta-{id}"))
                        .flex_initial()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .child(crate::palette::dotted(theme, t.meta.clone())),
                )
                .children(restored)
                .child(div().flex_1())
                .children(changes)
                .children(figure)
        });
        let lines = if line2.is_some() { kit::Row::Two } else { kit::Row::One };
        row(
            theme,
            lines,
            ElementId::Name(format!("nav-tile-{id}").into()),
            format!("nav-tile-{id}"),
            label.into(),
            selected,
        )
        .map(|row| if selected { self.nav.list.plate.seat(row, tile, theme) } else { row })
        .group(row_group)
        // The two lines sit in the middle of the row at either density, the kind beside the
        // first.
        .items_center()
        // The kind's glyph under the worker's name, past its icon and the gap after it: the
        // slot is wider than the glyph centred in it, so it starts that margin to the left.
        .pl(px(theme.spacing.inset() + theme.typography.icon_large() - glyph_margin(theme)))
        .child(
            div()
                .debug_selector(move || format!("nav-lines-{id}"))
                .flex_1()
                .min_w_0()
                .flex()
                .items_start()
                .gap(px(theme.spacing.xs))
                .child(div().h(px(first)).flex().items_center().child(lead))
                .child(
                    div()
                        .relative()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(line1)
                        .children(line2)
                        .children(bar),
                ),
        )
        .when(self.nav.list.autoscroll.get() == Some(tile), |row| {
            row.child(
                canvas(|bounds, window, _cx| window.request_autoscroll(bounds), |_, (), _, _| {})
                    .absolute()
                    .inset_0(),
            )
        })
        .on_click(cx.listener(move |this, _ev, _w, cx| this.go_to_tile(tile, cx)))
        .into_any_element()
    }
}

/// The repository lens's blocks: a repository's tiles from every worker under one header, in
/// order of attention (each worker's own order kept within a class), the repositories by name
/// and the tiles in none last. Two repositories of one name say where each is.
fn repo_blocks(
    tiles: impl Iterator<Item = NavTile>,
    folded: impl Fn(&str) -> bool,
) -> Vec<NavBlock> {
    let mut groups: BTreeMap<(bool, String, String), Vec<NavTile>> = BTreeMap::new();
    for tile in tiles {
        let path = tile.repo.clone().unwrap_or_default();
        let name =
            if path.is_empty() { NO_REPOSITORY.to_owned() } else { repo_name(&path).to_owned() };
        groups.entry((path.is_empty(), name.to_lowercase(), path)).or_default().push(tile);
    }
    let mut seen = HashSet::new();
    let mut shared = HashSet::new();
    for (_, name, _) in groups.keys() {
        if !seen.insert(name.clone()) {
            shared.insert(name.clone());
        }
    }
    groups
        .into_iter()
        .map(|((_, key, path), mut tiles)| {
            tiles.sort_by_key(|t| attention(t.mark, t.unseen));
            let mut rollup = Rollup::default();
            for t in &tiles {
                rollup.add(t.mark, t.unseen);
            }
            let name = if path.is_empty() {
                NO_REPOSITORY.to_owned()
            } else {
                repo_name(&path).to_owned()
            };
            let parent =
                (shared.contains(&key) && !path.is_empty()).then(|| repo_parent(&path)).flatten();
            let folded = folded(&path);
            let head = NavRow::Repo(NavRepo { path, name, parent, rollup, folded, gap: false });
            NavBlock { head, folded, vacant: None, tiles }
        })
        .collect()
}

/// The deepest of `roots` that holds `path`, on a directory boundary.
fn repo_of<'a>(path: &str, roots: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    roots.into_iter().filter(|root| within_or_at(root, path)).max_by_key(|root| root.len())
}

/// Whether `path` is `root` or below it.
fn within_or_at(root: &str, path: &str) -> bool {
    let root = root.trim_end_matches('/');
    path.strip_prefix(root).is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// `cwd` below `repo`, relative to it: `None` at the repository's root or outside it.
fn within(repo: &str, cwd: &str) -> Option<String> {
    let rest = cwd.strip_prefix(repo.trim_end_matches('/'))?;
    let rest = rest.strip_prefix('/')?.trim_end_matches('/');
    (!rest.is_empty()).then(|| rest.to_owned())
}

/// A repository's name: its directory's.
fn repo_name(path: &str) -> &str {
    path.trim_end_matches('/').rsplit('/').next().unwrap_or(path)
}

/// Where a repository is: its parent directory's last two components.
fn repo_parent(path: &str) -> Option<String> {
    let trimmed = path.trim_end_matches('/');
    let (parent, _) = trimmed.rsplit_once('/')?;
    let parts: Vec<&str> = parent.rsplit('/').take(2).filter(|p| !p.is_empty()).collect();
    (!parts.is_empty()).then(|| parts.into_iter().rev().collect::<Vec<_>>().join("/"))
}

/// A section's heading, as every list in the frame heads its sections: the quiet label, and
/// for *Working* the working mark and how many are at work.
fn heading(
    theme: &Theme,
    selector: &'static str,
    text: &'static str,
    working: Option<usize>,
) -> Stateful<Div> {
    let s = &theme.surfaces;
    let mark = working.map(|_| {
        div()
            .id("working-mark")
            .flex_none()
            .role(Role::Image)
            .aria_label(Status::Working.label())
            .child(status_icon(
                theme,
                Status::Working,
                px(theme.typography.meta()),
                hsla(s.text_muted),
            ))
    });
    let count = working.map(|n| readout(theme, n.to_string()));
    crate::palette::section_heading(theme, selector.into(), text)
        .debug_selector(move || selector.to_owned())
        .flex()
        .items_center()
        .gap(px(theme.spacing.xs))
        .children(mark)
        .children(count)
}

/// The least the worker header's trailing part takes: the chevron and "+" side by side.
fn header_actions_width(theme: &Theme) -> f32 {
    theme.typography.icon_large().mul_add(2.0, theme.spacing.xxs)
}

#[cfg(test)]
impl WorkspaceView {
    /// What the navigator's filter holds.
    #[must_use]
    pub(super) fn navigator_filter(&self) -> &str {
        &self.nav.filter.query
    }

    /// Every tile row the navigator lists, as drawn: its title, its second line and its age.
    pub(super) fn navigator_lines(&self, cx: &gpui::App) -> Vec<(String, String, Option<String>)> {
        self.nav_listing(cx)
            .into_iter()
            .flat_map(|w| w.tiles)
            .map(|t| (t.title, t.meta, t.age))
            .collect()
    }

    /// The state's word at the end of each tile row's first line, by title.
    pub(super) fn navigator_words(&self, cx: &gpui::App) -> Vec<(String, Option<String>)> {
        self.nav_listing(cx).into_iter().flat_map(|w| w.tiles).map(|t| (t.title, t.word)).collect()
    }

    /// Each tile row's second line, by tile, in the list's order.
    pub(super) fn navigator_metas(&self) -> Vec<(TileRef, String)> {
        let meta = |r: &NavRow| match r {
            NavRow::Tile(t) => Some((t.tile, t.meta.clone())),
            _ => None,
        };
        self.nav.list.rows.borrow().iter().filter_map(meta).collect()
    }

    /// What each repository's header is named to assistive technology, in the list's order.
    pub(super) fn navigator_repo_labels(&self) -> Vec<String> {
        let label = |r: &NavRow| match r {
            NavRow::Repo(repo) => Some(match &repo.parent {
                Some(parent) => format!("{}, in {parent}", repo.name),
                None => repo.name.clone(),
            }),
            _ => None,
        };
        self.nav.list.rows.borrow().iter().filter_map(label).collect()
    }

    /// The tiles the navigator's list holds, in its order, whether or not they are in view.
    pub(super) fn navigator_tiles(&self) -> Vec<TileRef> {
        let tile = |r: &NavRow| if let NavRow::Tile(t) = r { Some(t.tile) } else { None };
        self.nav.list.rows.borrow().iter().filter_map(tile).collect()
    }

    /// The sessions *Working* lists, in its order.
    pub(super) fn navigator_working(&self) -> Vec<slopty_core::SessionId> {
        let working = |r: &NavRow| match r {
            NavRow::Agent(a) if a.status == Status::Working => Some(a.at.session),
            _ => None,
        };
        self.nav.list.rows.borrow().iter().filter_map(working).collect()
    }

    /// Where the plate under the phone's active workspace row was drawn in the last frame.
    pub(super) fn navigator_space_plate(&self) -> Option<gpui::Bounds<gpui::Pixels>> {
        self.nav.list.space_plate.drawn()
    }

    /// Where the navigator's selection plate was drawn in the last frame.
    pub(super) fn navigator_plate(&self) -> Option<gpui::Bounds<gpui::Pixels>> {
        self.nav.list.plate.drawn()
    }

    /// Where the list showed its rows in the last frame drawn.
    pub(super) fn navigator_list_bounds(&self) -> gpui::Bounds<gpui::Pixels> {
        self.nav.list.state.viewport_bounds()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An age shows from a minute, and the tick comes when its label would change.
    #[test]
    fn an_age_shows_past_a_minute_and_ticks_at_its_next_unit() {
        assert_eq!(age_shown(Duration::from_secs(59)), None);
        assert_eq!(age_shown(Duration::from_secs(300)).as_deref(), Some("5m"));
        assert_eq!(until_age_changes(Duration::from_secs(59)), Duration::from_secs(1));
        assert_eq!(until_age_changes(Duration::from_secs(61)), Duration::from_secs(59));
        assert_eq!(until_age_changes(Duration::from_secs(3_700)), Duration::from_secs(3_500));
    }

    /// A ticking clock reads in whole seconds in the one duration form, from its first second.
    #[test]
    fn a_turn_ticks_in_whole_seconds() {
        assert_eq!(turn_label(Duration::from_millis(300)), "1 s");
        assert_eq!(turn_label(Duration::from_millis(9_999)), "9 s");
        assert_eq!(turn_label(Duration::from_secs(64)), "1m 4s");
        assert_eq!(turn_label(Duration::from_mins(62)), "1h 2m");
    }

    /// An agent at rest says what it last said, a lone word quoted so it is not read as a
    /// state, and nothing when it said nothing.
    #[test]
    fn a_resting_agent_is_quoted_not_stated() {
        assert_eq!(rest_words("done").as_deref(), Some("\u{201c}done\u{201d}"));
        assert_eq!(rest_words("Fixed the build").as_deref(), Some("Fixed the build"));
        assert_eq!(rest_words("  "), None);
    }

    /// A note's second line is its progress, else its next line, else nothing: its icon says
    /// it is a note, and its row is one line.
    #[test]
    fn a_notes_second_line_says_how_far_it_got() {
        let meta = |text: &str| note_meta(text, super::super::tile::note_progress(text));
        assert_eq!(meta("# Release\n- [x] tag\n- [ ] notes\n- [ ] ship\n"), "1 of 3 done");
        assert_eq!(meta("Groceries\n- milk\n"), "milk");
        assert_eq!(meta("Just a title\n"), "");
    }

    /// A report with a figure reads it against its sign; its line covers that share in its
    /// state's tone, and one without a figure covers the row set back; no report draws nothing.
    #[test]
    fn a_progress_report_is_a_figure_and_a_share_of_the_row() {
        let theme = Theme::default();
        let s = theme.surfaces;
        let report = |state, percent| Progress { state, percent };
        assert_eq!(progress_figure(report(ProgressState::Set, Some(42))).as_deref(), Some("42%"));
        assert_eq!(progress_figure(report(ProgressState::Set, Some(250))).as_deref(), Some("100%"));
        assert_eq!(progress_figure(report(ProgressState::Indeterminate, None)), None);
        assert_eq!(progress_line(&theme, Progress::default()), None);
        assert_eq!(
            progress_line(&theme, report(ProgressState::Set, Some(42))),
            Some((s.accent, 1.0, 0.42))
        );
        assert_eq!(
            progress_line(&theme, report(ProgressState::Indeterminate, None)),
            Some((s.accent, alpha::STRONG, 1.0))
        );
        assert_eq!(
            progress_line(&theme, report(ProgressState::Error, Some(80))).map(|l| l.0),
            Some(s.error)
        );
        assert_eq!(
            progress_line(&theme, report(ProgressState::Paused, None)).map(|l| l.0),
            Some(s.text_muted),
            "a pause is not a warning"
        );
        assert!(RESTORED.chars().next().is_some_and(char::is_uppercase), "sentence case");
    }

    /// Only a state worth a word gets one: at rest or out of reach, the row says nothing.
    #[test]
    fn a_row_names_a_state_that_is_happening() {
        assert_eq!(status_word(Some(Status::NeedsYou)), Some(Status::NeedsYou));
        assert_eq!(status_word(Some(Status::Failed)), Some(Status::Failed));
        assert_eq!(status_word(Some(Status::Working)), Some(Status::Working));
        assert_eq!(status_word(Some(Status::Running)), Some(Status::Running));
        assert_eq!(status_word(Some(Status::Done)), None, "the unseen dot says it");
        assert_eq!(status_word(Some(Status::Idle)), None);
        assert_eq!(status_word(Some(Status::Away)), None);
        assert_eq!(status_word(None), None);
    }

    /// The ladder: needs you, failed, unseen, working, waiting, the rest.
    #[test]
    fn rows_climb_the_attention_ladder() {
        let ranks = [
            attention(Some(Status::NeedsYou), false),
            attention(Some(Status::Failed), false),
            attention(Some(Status::Done), true),
            attention(None, true),
            attention(Some(Status::Working), false),
            attention(Some(Status::Running), false),
            attention(Some(Status::Idle), false),
            attention(Some(Status::Away), false),
        ];
        assert_eq!(ranks, [0, 1, 2, 2, 3, 4, 5, 5]);
    }

    /// A row at work recedes until it is selected; one that needs the person never does, and
    /// Increase Contrast draws them all whole.
    #[test]
    fn working_rows_recede_and_a_row_that_needs_you_does_not() {
        let mut theme = Theme::default();
        assert!((row_strength(&theme, Some(Status::Working), false) - alpha::STRONG).abs() < 1e-6);
        assert!((row_strength(&theme, Some(Status::Running), false) - alpha::STRONG).abs() < 1e-6);
        for (status, selected) in [
            (Some(Status::Working), true),
            (Some(Status::NeedsYou), false),
            (Some(Status::Failed), false),
            (None, false),
        ] {
            assert!((row_strength(&theme, status, selected) - 1.0).abs() < 1e-6, "{status:?}");
        }
        theme.contrast = slopty_theme::Contrast::Increased;
        assert!((row_strength(&theme, Some(Status::Working), false) - 1.0).abs() < 1e-6);
    }
}
