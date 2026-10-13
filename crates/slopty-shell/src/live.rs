//! Slopty's own live views inside the shell's chrome: a shell's terminal and a remote window or
//! desktop.
//!
//! What the shell shows of these is no data a view model can carry: they are views Slopty's
//! workspace keeps, each with its session's link, its prediction and its picture path. The app
//! installs a [`Host`] here that hands the shell the view for an id the view model gave it
//! (a terminal's from `OpenTerminal`, a screen's from its surface list), and the shell draws it
//! in the shell's own places: the terminal panel's body, a right-pane surface.

pub mod screen;

use std::rc::Rc;

use gpui::{AnyView, App, FocusHandle, Global, Hsla, SharedString, Subscription};

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
    /// The selection wash.
    pub selection: Hsla,
    /// ANSI 0–15.
    pub ansi: [Hsla; 16],
    /// The dark appearance.
    pub dark: bool,
}

impl TerminalLook {
    /// The look the shell's theme gives a terminal.
    #[must_use]
    pub fn of(cx: &App) -> Self {
        let theme = crate::theme::Theme::of(cx);
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
            ansi: theme.terminal.ansi,
            dark: theme.appearance.is_dark(),
        }
    }
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
