//! Slopty's own live views inside the shell's chrome: a shell's terminal, a remote window or
//! desktop, and a page an agent shows inline ([`serve_pages`]).
//!
//! What the shell shows of these is no data a view model can carry: they are views Slopty's
//! workspace keeps, each with its session's link, its prediction and its picture path. The app
//! installs a [`Host`] here that hands the shell the view for an id the view model gave it
//! (a terminal's from `OpenTerminal`, a screen's from its surface list), and the shell draws it
//! in the shell's own places: the terminal panel's body, a right-pane surface.
//!
//! The other way round, a path a person ⌘-clicks in a shell opens where a transcript's file link
//! opens, in its chat's Files pane ([`open_file`]), and a web link one hands over opens where a
//! transcript's web link opens ([`open_link`]): the shell serves both once its window is up.

pub mod chrome;
pub mod key_row; // the touch key row's armed modifiers, and whether a keyboard stands in for it
pub mod menu;
pub mod screen;

use std::rc::Rc;

use gpui::{AnyView, App, FocusHandle, Global, Hsla, Pixels, SharedString, Size, Subscription, Window};

pub use chrome::floating_pill;

/// One of Slopty's views, as the shell places it.
#[derive(Clone)]
pub struct Hosted {
    /// What is drawn.
    pub view: AnyView,
    /// Where the keyboard goes when the surface is chosen.
    pub focus: FocusHandle,
    /// Its title now: a shell's own (OSC 0/2), a window's.
    pub title: Option<SharedString>,
    /// Its program has ended.
    pub ended: bool,
    /// The status it ended with, when its program said one.
    pub exit: Option<i32>,
    /// How far its program says it has got (a terminal's `OSC 9;4`), while it says.
    pub progress: Option<Progress>,
    /// What a drop of files on it does, in its words ("Drop to paste paths"); `None` when it
    /// takes no files.
    pub drop: Option<SharedString>,
    /// The background its program paints the pane with (OSC 11, else the theme's), while it is a
    /// terminal: what the titlebar row wears (`docs/decisions/layout.md`, "One row of chrome").
    /// Its watch ([`watch`]) fires when the program sets another.
    pub ground: Option<Hsla>,
}

/// How far a program says it has got: a terminal's `OSC 9;4` report, which its tab shows.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Progress {
    /// This share of it, 0 to 1.
    Share(f32),
    /// Under way, how far unknown.
    Busy,
    /// Held, at this share when one is known.
    Paused(Option<f32>),
    /// Stopped by a failure, at this share when one is known.
    Failed(Option<f32>),
}

impl Progress {
    /// Its share as a whole percent, when it has one.
    #[must_use]
    pub fn percent(self) -> Option<u8> {
        let share = match self {
            Self::Share(share) => Some(share),
            Self::Busy => None,
            Self::Paused(share) | Self::Failed(share) => share,
        }?;
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "0..=100")]
        let percent = (share.clamp(0.0, 1.0) * 100.0).round() as u8;
        Some(percent)
    }
}

/// How the shell's theme asks a terminal to look: zeron's font, padding and palette.
#[derive(Clone, Debug, PartialEq)]
pub struct TerminalLook {
    /// The monospace family.
    pub family: SharedString,
    /// The font size, in points.
    pub font_size: f32,
    /// The row height, in points.
    pub line_height: f32,
    /// The grid's inset from the panel's edges, in points.
    pub padding: f32,
    /// The panel fill under the grid.
    pub background: Hsla,
    /// Default text.
    pub foreground: Hsla,
    /// The block cursor.
    pub cursor: Hsla,
    /// The selection wash, translucent over whatever the program painted.
    pub selection: Hsla,
    /// A find's hit, washed as the transcript's find washes a match.
    pub search_match: Hsla,
    /// The find's hit the person is on, washed stronger.
    pub search_current: Hsla,
    /// The quietest ink: the scrollbar's thumb, a dim trailer.
    pub faint: Hsla,
    /// ANSI 0–15.
    pub ansi: [Hsla; 16],
    /// The dark appearance.
    pub dark: bool,
}

impl TerminalLook {
    /// The look the shell's theme gives a terminal.
    #[must_use]
    pub fn of(cx: &App) -> Self {
        Self::from_theme(crate::theme::Theme::of(cx))
    }

    /// The look `theme` gives a terminal.
    #[must_use]
    pub fn from_theme(theme: &crate::theme::Theme) -> Self {
        Self {
            family: theme.font_terminal.clone(),
            font_size: theme.terminal_font_size,
            line_height: theme.terminal_font_size * crate::terminal::view::TERM_LINE_HEIGHT
                / crate::terminal::view::TERM_FONT_SIZE,
            padding: crate::terminal::view::TERM_PADDING,
            background: theme.terminal.background,
            foreground: theme.terminal.foreground,
            cursor: theme.cursor,
            selection: theme.terminal.selection,
            // A terminal's hits wash as the transcript's find washes its matches
            // (`markdown::find::washes`): every one plainly marked, the one the person is on
            // stronger. Half the selection's faint wash would leave the rest all but unseen on
            // dark.
            search_match: crate::markdown::find::washes(theme).0,
            search_current: crate::markdown::find::washes(theme).1,
            faint: theme.text_faint,
            ansi: theme.terminal.ansi,
            dark: theme.appearance.is_dark(),
        }
    }
}

/// The shell's chrome as Slopty's own views draw theirs: zeron's face and colours, so a find
/// bar or a window's controls read as the shell's own.
#[derive(Clone, Debug, PartialEq)]
pub struct Chrome {
    /// The dark appearance.
    pub dark: bool,
    /// The interface face.
    pub sans: SharedString,
    /// The main panel.
    pub panel: Hsla,
    /// The shell's own plane: the sidebar's, a bar's.
    pub surface: Hsla,
    /// A pill or chip proud of the panel.
    pub raised: Hsla,
    /// A card resting on the panel.
    pub card: Hsla,
    /// What floats: a menu, a popover.
    pub overlay: Hsla,
    /// An input's plate.
    pub input: Hsla,
    /// The hover wash.
    pub hover: Hsla,
    /// The selected wash.
    pub active: Hsla,
    /// The hairline.
    pub border: Hsla,
    /// The stronger edge of what is focused or raised.
    pub border_strong: Hsla,
    /// Primary text.
    pub text: Hsla,
    /// Secondary text.
    pub text_muted: Hsla,
    /// The quietest text.
    pub text_faint: Hsla,
    /// The accent.
    pub accent: Hsla,
    /// The accent as a fill.
    pub accent_strong: Hsla,
    /// The accent's quiet wash.
    pub accent_wash: Hsla,
    /// Text on the accent's fill.
    pub on_accent: Hsla,
    /// Success.
    pub success: Hsla,
    /// Warning.
    pub warning: Hsla,
    /// Danger, as a word.
    pub danger: Hsla,
    /// Danger, as a fill.
    pub danger_strong: Hsla,
    /// Working.
    pub busy: Hsla,
}

impl Chrome {
    /// The chrome the shell's theme draws now; `None` before the shell has a theme.
    #[must_use]
    pub fn of(cx: &App) -> Option<Self> {
        let theme = cx.try_global::<crate::theme::Theme>()?;
        Some(Self {
            dark: theme.appearance.is_dark(),
            sans: theme.font_sans.clone(),
            panel: theme.bg,
            surface: theme.surface,
            raised: theme.surface_raised,
            card: theme.surface_card,
            // What floats is the popover's plane, frosted where the theme frosts.
            overlay: crate::popover::surface_bg(theme),
            input: theme.input_bg,
            hover: theme.element_hover,
            active: theme.element_active,
            border: theme.border,
            border_strong: theme.border_strong,
            text: theme.text,
            text_muted: theme.text_muted,
            text_faint: theme.text_faint,
            accent: theme.accent,
            accent_strong: theme.accent_strong,
            accent_wash: theme.accent_wash,
            on_accent: theme.on_accent,
            success: theme.success,
            warning: theme.warning,
            danger: theme.danger,
            danger_strong: theme.danger_strong,
            busy: theme.busy,
        })
    }
}

/// Call `changed` each time the shell's theme changes: its appearance, its family, its accent
/// or its fonts.
pub fn observe_theme(cx: &mut App, mut changed: impl FnMut(&mut App) + 'static) -> Subscription {
    cx.observe_global::<crate::theme::Theme>(move |cx| changed(cx))
}

type TerminalFn = dyn Fn(&str, &TerminalLook, &mut App) -> Option<Hosted>;
type ScreenFn = dyn Fn(&str, &mut App) -> Option<Hosted>;
type WatchFn = dyn Fn(&str, Box<dyn FnMut(&mut App)>, &mut App) -> Option<Subscription>;

/// What the app installed: the views by id.
#[derive(Clone)]
pub struct Host {
    terminal: Rc<TerminalFn>,
    screen: Rc<ScreenFn>,
    watch: Rc<WatchFn>,
}

impl Global for Host {}

/// Install the app's views: `terminal` gives a shell's terminal by its id, styled by the look
/// passed; `screen` a remote window or desktop by its id; `watch` calls back when what a
/// [`Hosted`] says of itself (its title, its end) changes.
pub fn install(
    cx: &mut App,
    terminal: impl Fn(&str, &TerminalLook, &mut App) -> Option<Hosted> + 'static,
    screen: impl Fn(&str, &mut App) -> Option<Hosted> + 'static,
    watch: impl Fn(&str, Box<dyn FnMut(&mut App)>, &mut App) -> Option<Subscription> + 'static,
) {
    cx.set_global(Host {
        terminal: Rc::new(terminal),
        screen: Rc::new(screen),
        watch: Rc::new(watch),
    });
}

/// The terminal `id`, looking as `look` says; `None` until it is attached, or with no host.
pub fn terminal(id: &str, look: &TerminalLook, cx: &mut App) -> Option<Hosted> {
    let host = cx.try_global::<Host>()?.terminal.clone();
    host(id, look, cx)
}

/// The remote window or desktop `id`; `None` while it has no stream, or with no host.
pub fn screen(id: &str, cx: &mut App) -> Option<Hosted> {
    let host = cx.try_global::<Host>()?.screen.clone();
    host(id, cx)
}

/// Call `changed` whenever the view `id` says something new of itself.
pub fn watch(id: &str, changed: Box<dyn FnMut(&mut App)>, cx: &mut App) -> Option<Subscription> {
    let host = cx.try_global::<Host>()?.watch.clone();
    host(id, changed, cx)
}

type FitFn = dyn Fn(Size<Pixels>, &mut Window, &mut App) -> (u16, u16);

/// How the app fits a terminal's grid.
#[derive(Clone)]
struct Fit(Rc<FitFn>);

impl Global for Fit {}

/// Install how a terminal's grid fits a size: `fit` gives the columns and rows a terminal view
/// draws in it, in the look [`terminal`] gives it.
pub fn serve_fit(cx: &mut App, fit: impl Fn(Size<Pixels>, &mut Window, &mut App) -> (u16, u16) + 'static) {
    cx.set_global(Fit(Rc::new(fit)));
}

/// The columns and rows a terminal view draws in `size`; `None` with no host.
pub fn fit(size: Size<Pixels>, window: &mut Window, cx: &mut App) -> Option<(u16, u16)> {
    let fit = cx.try_global::<Fit>()?.0.clone();
    Some(fit(size, window, cx))
}

type GridFn = dyn Fn(&mut Window, &mut App) -> (Size<Pixels>, Pixels);

/// How the app says a terminal's cell.
#[derive(Clone)]
struct GridOf(Rc<GridFn>);

impl Global for GridOf {}

/// Install how a terminal's cell is known: `grid` gives the cell a terminal view draws, in the
/// look [`terminal`] gives it, and the inset around its grid on each side.
pub fn serve_grid(cx: &mut App, grid: impl Fn(&mut Window, &mut App) -> (Size<Pixels>, Pixels) + 'static) {
    cx.set_global(GridOf(Rc::new(grid)));
}

/// The cell a terminal view draws and what its pane takes besides whole cells along each axis
/// (the inset on both sides): what a split snaps a terminal's pane to (`docs/decisions/
/// layout.md`, "One session per view, splits on demand"). `None` with no host.
pub fn grid(window: &mut Window, cx: &mut App) -> Option<(Size<Pixels>, Size<Pixels>)> {
    let grid = cx.try_global::<GridOf>()?.0.clone();
    let (cell, inset) = grid(window, cx);
    Some((cell, Size { width: inset * 2.0, height: inset * 2.0 }))
}

/// How much narrower and shorter than its bounds each held terminal view fits its grid, by the
/// view's entity ([`hold_fit`]).
#[derive(Default)]
struct FitHolds(Vec<(gpui::EntityId, Size<Pixels>)>);

impl Global for FitHolds {}

/// Hold the grid of the terminal drawn as `view` `by` narrower and shorter than its bounds, or
/// let it go with zero. The shell holds its session's terminal while the panes around it glide,
/// so the TUI in it reflows once for a glide rather than once a frame (`docs/decisions/ui.md`,
/// "A glide reflows the TUI once").
pub fn hold_fit(view: gpui::EntityId, by: Size<Pixels>, cx: &mut App) {
    let held = by.width > Pixels::ZERO || by.height > Pixels::ZERO;
    if !held && !cx.has_global::<FitHolds>() {
        return;
    }
    let holds = &mut cx.default_global::<FitHolds>().0;
    holds.retain(|(id, _)| *id != view);
    if held {
        holds.push((view, by));
    }
}

/// What [`hold_fit`] holds the terminal drawn as `view` by: zero when nothing does.
#[must_use]
pub fn fit_held(view: gpui::EntityId, cx: &App) -> Size<Pixels> {
    cx.try_global::<FitHolds>()
        .and_then(|FitHolds(holds)| holds.iter().find(|(id, _)| *id == view))
        .map_or_else(Size::default, |(_, by)| *by)
}

type KeysFn = dyn Fn(&mut App);

/// How Slopty binds its views' keys.
#[derive(Clone)]
struct Keys(Rc<KeysFn>);

impl Global for Keys {}

/// Slopty's views' own key bindings (a terminal's ⌘F, ⌘K): `bind` binds them, and runs again
/// each time the shell binds its keymap, which clears every binding first. Install it before the
/// shell's window opens; installed later, it binds at once.
pub fn keys(cx: &mut App, bind: impl Fn(&mut App) + 'static) {
    let bind: Rc<KeysFn> = Rc::new(bind);
    let late = cx.has_global::<Keys>();
    cx.set_global(Keys(Rc::clone(&bind)));
    if late {
        rebind(cx);
    }
}

/// Bind Slopty's views' keys, last in the shell's keymap.
pub(crate) fn bind_keys(cx: &mut App) {
    if let Some(Keys(bind)) = cx.try_global::<Keys>().cloned() {
        bind(cx);
    }
}

/// Bind the whole keymap again, Slopty's views' keys as [`keys`] now says them.
fn rebind(cx: &mut App) {
    let settings = crate::settings::current(cx);
    crate::shell::apply_keymap(cx, &settings.keymap);
}

type OpenFileFn = dyn Fn(&str, &str, &mut App) -> bool;

/// How the shell opens a file link for a chat, once its window is up.
#[derive(Clone)]
struct Files(Rc<OpenFileFn>);

impl Global for Files {}

/// The shell opens a chat's file links by `open`: chat, then the link.
pub(crate) fn serve_files(cx: &mut App, open: impl Fn(&str, &str, &mut App) -> bool + 'static) {
    cx.set_global(Files(Rc::new(open)));
}

/// Open `target` (a path or a `file://` link, its line as `#L12`) in chat `chat`'s Files pane, as
/// a transcript's file link opens: whether the shell took it.
pub fn open_file(chat: &str, target: &str, cx: &mut App) -> bool {
    let Some(Files(open)) = cx.try_global::<Files>().cloned() else { return false };
    open(chat, target, cx)
}

type OpenLinkFn = dyn Fn(&str, &str, &mut App) -> bool;

/// How the shell opens a web link a terminal of a chat handed over, once its window is up.
#[derive(Clone)]
struct Links(Rc<OpenLinkFn>);

impl Global for Links {}

/// The shell opens web links Slopty's views hand over for a chat by `open`: chat, then the URL.
pub(crate) fn serve_links(cx: &mut App, open: impl Fn(&str, &str, &mut App) -> bool + 'static) {
    cx.set_global(Links(Rc::new(open)));
}

/// Open `url` for chat `chat` where a transcript's web link opens: the session's Browser when the
/// person keeps web links in Slopty, else their browser. Whether the shell took it.
pub fn open_link(chat: &str, url: &str, cx: &mut App) -> bool {
    let Some(Links(open)) = cx.try_global::<Links>().cloned() else { return false };
    open(chat, url, cx)
}

type DimFn = dyn Fn(&AnyView, bool, &mut App);

/// How the app dims one of its terminals.
#[derive(Clone)]
struct Dim(Rc<DimFn>);

impl Global for Dim {}

/// Serve [`dim`] with `dim`: the terminal's view, and whether it is dimmed.
pub fn serve_dim(cx: &mut App, dim: impl Fn(&AnyView, bool, &mut App) + 'static) {
    cx.set_global(Dim(Rc::new(dim)));
}

/// Dim the terminal drawn as `view`, or bring it back: while a view holds more than one pane,
/// each pane without the keyboard is dimmed by brightness alone, in its own element
/// (`docs/decisions/layout.md`, "One session per view, splits on demand"). The split calls this
/// for every terminal pane it holds as the keyboard moves; nothing with no host.
pub fn dim(view: &AnyView, dimmed: bool, cx: &mut App) {
    let Some(Dim(dim)) = cx.try_global::<Dim>().cloned() else { return };
    dim(view, dimmed, cx);
}

type RunLineFn = dyn Fn(&str, &str, bool, &mut App) -> bool;

/// How the app types a line into one of its terminals.
#[derive(Clone)]
struct Runs(Rc<RunLineFn>);

impl Global for Runs {}

/// Serve [`run_line`] with `run`: the terminal's id, the line, and whether to press Enter after
/// it; whether that terminal took it.
pub fn serve_runs(cx: &mut App, run: impl Fn(&str, &str, bool, &mut App) -> bool + 'static) {
    cx.set_global(Runs(Rc::new(run)));
}

/// Type `line` into terminal `id`, and run it when `enter`, else leave it at the prompt: whether
/// the terminal took it.
pub fn run_line(id: &str, line: &str, enter: bool, cx: &mut App) -> bool {
    let Some(Runs(run)) = cx.try_global::<Runs>().cloned() else { return false };
    run(id, line, enter, cx)
}

type DraftingFn = dyn Fn(&AnyView, &App) -> bool;

/// How the app tells whether the person has unsent words in a terminal's line.
#[derive(Clone)]
struct Drafting(Rc<DraftingFn>);

impl Global for Drafting {}

/// Serve [`drafting`] with `drafting`: whether a terminal's view holds words the person typed
/// and has not sent.
pub fn serve_drafting(cx: &mut App, drafting: impl Fn(&AnyView, &App) -> bool + 'static) {
    cx.set_global(Drafting(Rc::new(drafting)));
}

/// Whether the terminal drawn as `view` holds words the person typed or pasted and has not sent:
/// what an agent driving the window reads as the person typing. `false` with no host.
#[must_use]
pub fn drafting(view: &AnyView, cx: &App) -> bool {
    cx.try_global::<Drafting>().is_some_and(|Drafting(drafting)| drafting(view, cx))
}

/// How a page an agent shows inline stands in the app's page host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PagePresence {
    /// No web view: its place, or its last picture, stands for it.
    Cold,
    /// A web view not placed: its last picture stands for it.
    Warm,
    /// Placed, taking the pointer and the keyboard.
    Live,
}

/// A page an agent shows inline, as the app hosts it (`slopty_platform::web::page`): the card
/// is the shell's (`crate::page`), the web view the app's, handed over by the page's key
/// (`slopty_view_doc::PageCard::page`).
#[derive(Clone)]
pub struct HostedPage {
    /// How it stands.
    pub presence: PagePresence,
    /// This window's host of the page's native view, while one is mounted: placed while live.
    pub native: Option<gpui::composition::NativeHost>,
    /// Its last picture, drawn while it is not live.
    pub snapshot: Option<std::sync::Arc<gpui::RenderImage>>,
    /// The height it asked for, within the inline cap, once it said.
    pub height: Option<f32>,
    /// Why it cannot be shown, when it cannot.
    pub failed: Option<SharedString>,
    /// The machine its document waits for while that machine is out of reach, by its
    /// name: the card says so in place of loading.
    pub waiting: Option<SharedString>,
    /// A file the app draws itself rather than in a web view (a PDF's pages, a CSV's
    /// table): the card draws this, and the page has no web view.
    pub doc: Option<crate::page::PageDoc>,
}

/// What the card asks of a page's host.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PageAct {
    /// The person engages it (a pointer over it, the keyboard in it): make it live, within the
    /// window's cap of live pages.
    Engage,
    /// Back to its picture: the person left it, or it scrolled under the composer's frost.
    Rest,
    /// Its card is this wide, in points.
    Width(f32),
    /// Its row left the transcript: let its web view go, its picture and state kept.
    Unmount,
    /// Open it afresh, the person's Try again: its web view and what failed are let go,
    /// and it is read again from what showed it.
    Reload,
    /// Draw page `index` (from 0) of a PDF `width` points wide, for the card or the pane.
    Draw {
        /// The page.
        index: usize,
        /// Its width, in points.
        width: f32,
    },
}

type PageFn = dyn Fn(&str, &mut gpui::Window, &mut App) -> Option<HostedPage>;
type PageActFn = dyn Fn(&str, PageAct, &mut App);

/// How the app hosts pages.
#[derive(Clone)]
struct Pages {
    page: Rc<PageFn>,
    act: Rc<PageActFn>,
    watch: Rc<WatchFn>,
}

impl Global for Pages {}

/// Serve inline pages: `page` gives the page of a key as it stands in a window, `act` takes what
/// its card asks, `watch` calls back when how it stands changes (a picture taken, a height
/// asked, live or not).
pub fn serve_pages(
    cx: &mut App,
    page: impl Fn(&str, &mut gpui::Window, &mut App) -> Option<HostedPage> + 'static,
    act: impl Fn(&str, PageAct, &mut App) + 'static,
    watch: impl Fn(&str, Box<dyn FnMut(&mut App)>, &mut App) -> Option<Subscription> + 'static,
) {
    cx.set_global(Pages {
        page: Rc::new(page),
        act: Rc::new(act),
        watch: Rc::new(watch),
    });
}

/// The page `key` as it stands in `window`; `None` with no host, or none for that key.
pub fn page(key: &str, window: &mut gpui::Window, cx: &mut App) -> Option<HostedPage> {
    let host = cx.try_global::<Pages>()?.page.clone();
    host(key, window, cx)
}

/// Ask the host of page `key` for `act`; nothing with no host.
pub fn page_act(key: &str, act: PageAct, cx: &mut App) {
    if let Some(host) = cx.try_global::<Pages>().map(|pages| pages.act.clone()) {
        host(key, act, cx);
    }
}

/// Call `changed` whenever how page `key` stands changes.
pub fn watch_page(
    key: &str,
    changed: Box<dyn FnMut(&mut App)>,
    cx: &mut App,
) -> Option<Subscription> {
    let host = cx.try_global::<Pages>()?.watch.clone();
    host(key, changed, cx)
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use gpui::{KeyBinding, TestAppContext, actions};

    actions!(live_tests, [Marked]);

    /// Slopty's keys are bound with the shell's keymap and again each time the shell binds it,
    /// which clears every binding first; installed late, they bind at once.
    #[gpui::test]
    fn slopty_s_keys_outlive_the_shell_s_keymap(cx: &mut TestAppContext) {
        let binds = Rc::new(Cell::new(0_u32));
        let counted = Rc::clone(&binds);
        let marked = |cx: &mut gpui::App| {
            cx.key_bindings().borrow().bindings().any(|b| b.action().partial_eq(&Marked))
        };
        cx.update(|cx| {
            cx.set_global(crate::theme::Theme::default());
            super::keys(cx, move |cx| {
                counted.set(counted.get() + 1);
                cx.bind_keys([KeyBinding::new("cmd-shift-y", Marked, Some("Terminal"))]);
            });
            assert_eq!(binds.get(), 0, "first installed: the shell's keymap binds them");
            let settings = crate::settings::current(cx);
            crate::shell::apply_keymap(cx, &settings.keymap);
            assert!(marked(cx));
            assert_eq!(binds.get(), 1);
            crate::shell::apply_keymap(cx, &settings.keymap);
            assert!(marked(cx), "bound again after the shell cleared its bindings");
            super::keys(cx, |_| {});
            assert!(!marked(cx), "installed again: the keymap bound anew without them");
        });
    }
}
