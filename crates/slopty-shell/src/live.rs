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
//! opens, in its chat's Files pane ([`open_file`]): the shell serves that once its window is up.

pub mod chrome;
pub mod menu;
pub mod screen;

use std::rc::Rc;

use gpui::{AnyView, App, FocusHandle, Global, Hsla, SharedString, Subscription};

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
    /// A find's hit, a lighter selection.
    pub search_match: Hsla,
    /// The find's hit the person is on: the selection itself.
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
            // In-surface hits are the selection's colour (zeron's find in a file), the hit the
            // person is on whole and the rest at half.
            search_match: theme.selection.opacity(theme.selection.a * 0.5),
            search_current: theme.selection,
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
            // slopty: what floats is the popover's plane, frosted where the theme frosts.
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
    crate::shell::apply_keymap(cx, &settings.keymap, settings.composer_send_behavior);
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
    /// slopty: a file the app draws itself rather than in a web view (a PDF's pages, a CSV's
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
    /// slopty: open it afresh, the person's Try again: its web view and what failed are let go,
    /// and it is read again from what showed it.
    Reload,
    /// slopty: draw page `index` (from 0) of a PDF `width` points wide, for the card or the pane.
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

/// The shell's way into a chat's draft: Slopty's terminals hand it a block or a selection.
#[derive(Clone)]
struct Drafts(Rc<dyn Fn(&str, &str, &mut App) -> bool>);

impl Global for Drafts {}

/// Serve [`attach`] with `put`, which puts text in a chat's draft: whether it could.
pub(crate) fn serve_drafts(cx: &mut App, put: impl Fn(&str, &str, &mut App) -> bool + 'static) {
    cx.set_global(Drafts(Rc::new(put)));
}

/// Put `text` (a terminal's block or selection, fenced) in chat `chat`'s draft at the caret and
/// give it the keyboard: whether the chat is open in this window to take it.
pub fn attach(chat: &str, text: &str, cx: &mut App) -> bool {
    let Some(Drafts(put)) = cx.try_global::<Drafts>().cloned() else { return false };
    put(chat, text, cx)
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
            crate::shell::apply_keymap(cx, &settings.keymap, settings.composer_send_behavior);
            assert!(marked(cx));
            assert_eq!(binds.get(), 1);
            crate::shell::apply_keymap(cx, &settings.keymap, settings.composer_send_behavior);
            assert!(marked(cx), "bound again after the shell cleared its bindings");
            super::keys(cx, |_| {});
            assert!(!marked(cx), "installed again: the keymap bound anew without them");
        });
    }
}
