//! slopty: the window's keys beyond zeron's (T3 Code's set, in the Mac's spelling).
//!
//! - ⌘F finds in the chat's thread, or in the diff when the Changes surface has focus; ⌘G and
//!   ⌘⇧G step through the matches. The finding itself is the transcript's and the diff's own
//!   (`transcript::FindInThread`, `FindNext`, `FindPrevious`; `changes::FindInDiff`,
//!   `FindNextInDiff`, `FindPreviousInDiff` in the `Changes` context).
//! - A session's panes (`docs/decisions/layout.md`, "One session per view, splits on demand"):
//!   ⌘D opens a shell to the right, ⇧⌘D one below, ⌘⏎ one where there is most room; ⇧⌘⏎ fills
//!   the view with the focused pane; ⌥⌘ arrows walk focus by direction, ⌘] and ⌘[ in reading
//!   order. ⌘W closes the focused pane (`app_menus::CloseTab`).
//! - Views beside the session: ⌘I the inspector, and the palette's Browser and Remote window.
//! - ⌥⌘[ and ⌥⌘] walk the history, as the mouse's side buttons do; ⌘⇧T opens the view closed
//!   last again (`closed_tabs`).
//! - ⌘Z undoes the archive just made, while its toast is up.
//! - ⇧⌘O opens "Resume a session…", the picker of Claude Code's past sessions (`past_sessions`).
//! - ⌘. opens the next chat that needs the person, across projects and machines, then the
//!   finished ones nobody has seen (`sidebar_status::needing_you`).
//! - ⌥⌘O opens Open in under its button.
//!
//! A terminal keeps every key it takes for itself (its own find, and anything its program
//! reads), and the browser keeps its own ⌘[ and ⌘]: those bind in their contexts, and this
//! module's handlers let a key go on when a terminal has focus.

use std::time::Duration;

use gpui::{App, Context, KeyBinding, Window, actions, prelude::*};

use super::panes::Direction;
use super::{Route, Shell};
use crate::changes::{FindInDiff, FindNextInDiff, FindPreviousInDiff};
use crate::settings::{KeymapConfig, ShortcutId, parse_combo, platform_combo};
use crate::transcript::{FindInThread, FindNext as FindNextInThread, FindPrevious as FindPreviousInThread};

actions!(
    shell,
    [
        Find,
        FindNext,
        FindPrevious,
        NavigateBack,
        NavigateForward,
        ReopenClosedTab,
        UndoArchive,
        ResumeSession,
        NextNeedsYou,
        // slopty: what needs the person, across sessions and machines (`inbox`).
        Inbox,
        OpenIn,
        // slopty: ⌘I summons the session's inspector (`docs/decisions/layout.md`, "No permanent
        // right column"): ⌘ is free of the terminal's keys and of every TUI's.
        ToggleInspector,
        // slopty: views the palette summons beside the session.
        OpenBrowser,
        OpenRemoteWindow,
        // slopty: a session's panes.
        SplitRight,
        SplitDown,
        SplitAuto,
        ZoomPane,
        FocusPaneLeft,
        FocusPaneRight,
        FocusPaneUp,
        FocusPaneDown,
        NextPane,
        PreviousPane
    ]
);

/// How long the toast that offers to undo an archive stays.
const UNDO_LASTS: Duration = Duration::from_secs(8);

/// Bind the keys, the configurable ones as `keymap` says. A fixed key another shortcut was given
/// stays with that shortcut.
pub(super) fn bind_keys(cx: &mut App, keymap: &KeymapConfig) {
    let taken = |combo: &str| {
        ShortcutId::ALL.iter().any(|id| {
            parse_combo(keymap.get(*id)).ok() == parse_combo(combo).ok()
        })
    };
    let mut bindings = Vec::new();
    macro_rules! fixed {
        ($combo:literal, $action:expr) => {
            if !taken($combo) {
                bindings.push(crate::keys::binding(&platform_combo($combo), $action, None, cx));
            }
        };
    }
    fixed!("mod-f", Find);
    fixed!("mod-g", FindNext);
    fixed!("mod-shift-g", FindPrevious);
    fixed!("alt-mod-[", NavigateBack);
    fixed!("alt-mod-]", NavigateForward);
    fixed!("mod-z", UndoArchive);
    fixed!("alt-mod-o", OpenIn);
    fixed!("mod-i", ToggleInspector);
    fixed!("mod-d", SplitRight);
    fixed!("mod-shift-d", SplitDown);
    fixed!("mod-enter", SplitAuto);
    fixed!("mod-shift-enter", ZoomPane);
    fixed!("alt-mod-left", FocusPaneLeft);
    fixed!("alt-mod-right", FocusPaneRight);
    fixed!("alt-mod-up", FocusPaneUp);
    fixed!("alt-mod-down", FocusPaneDown);
    fixed!("mod-]", NextPane);
    fixed!("mod-[", PreviousPane);
    // The diff's own find, where the Changes surface has the keyboard.
    bindings.push(crate::keys::binding(&platform_combo("mod-f"), FindInDiff, Some("Changes"), cx));
    bindings.push(crate::keys::binding(&platform_combo("mod-g"), FindNextInDiff, Some("Changes"), cx));
    bindings.push(crate::keys::binding(
        &platform_combo("mod-shift-g"),
        FindPreviousInDiff,
        Some("Changes"),
        cx,
    ));
    for (id, action) in [
        (ShortcutId::ReopenClosedTab, Box::new(ReopenClosedTab) as Box<dyn gpui::Action>),
        (ShortcutId::ResumeSession, Box::new(ResumeSession)),
        (ShortcutId::NextNeedsYou, Box::new(NextNeedsYou)),
        (ShortcutId::Inbox, Box::new(Inbox)),
    ] {
        let combo = platform_combo(keymap.get(id));
        if combo.is_empty() {
            continue;
        }
        // A chord that is no chord (a hand-edited file) binds nothing.
        if let Ok(binding) =
            KeyBinding::load(&combo, action, None, false, None, cx)
        {
            bindings.push(binding);
        }
    }
    cx.bind_keys(bindings);
}

/// Whether the focus is in a web page, which keeps its own ⌘ chords (back, forward, a new tab,
/// undo). A terminal keeps only the chords it binds, which its context takes first: every other
/// ⌘ chord is the app's (`slopty_terminal`), so the window's keys hold from a session's
/// terminal, where the keyboard lives.
fn in_own_keys(window: &Window) -> bool {
    window.context_stack().iter().any(|context| context.contains("Browser"))
}

/// Whether the focus is in a terminal or a web page, whose own find a find key is for.
fn in_own_find(window: &Window) -> bool {
    in_own_keys(window) || window.context_stack().iter().any(|context| context.contains("Terminal"))
}

/// Which find a key asks for.
#[derive(Clone, Copy)]
enum FindStep {
    Open,
    Next,
    Previous,
}

impl Shell {
    /// `root` taking this module's actions.
    pub(super) fn chrome_actions<E: InteractiveElement>(&self, root: E, cx: &mut Context<Self>) -> E {
        root.on_action(cx.listener(|this, _: &Find, window, cx| {
            this.find(FindStep::Open, window, cx);
        }))
        .on_action(cx.listener(|this, _: &FindNext, window, cx| {
            this.find(FindStep::Next, window, cx);
        }))
        .on_action(cx.listener(|this, _: &FindPrevious, window, cx| {
            this.find(FindStep::Previous, window, cx);
        }))
        .on_action(cx.listener(|this, _: &NavigateBack, window, cx| {
            if in_own_keys(window) {
                cx.propagate();
            } else {
                this.navigate_back(cx);
            }
        }))
        .on_action(cx.listener(|this, _: &NavigateForward, window, cx| {
            if in_own_keys(window) {
                cx.propagate();
            } else {
                this.navigate_forward(cx);
            }
        }))
        .on_action(cx.listener(|this, _: &ReopenClosedTab, window, cx| {
            if !matches!(this.route, Route::Chat) || !this.reopen_closed_tab(window, cx) {
                cx.propagate();
            }
        }))
        .on_action(cx.listener(|this, _: &ResumeSession, window, cx| {
            // Over another overlay the key waits; the picker's own key shuts it.
            if this.overlay_owns_keyboard() && !this.past_sessions.picker_open() {
                return;
            }
            this.toggle_resume_picker(window, cx);
        }))
        .on_action(cx.listener(|this, _: &NextNeedsYou, _, cx| {
            // Under an overlay that holds the keyboard the key waits, as the chat keys do.
            if !this.overlay_owns_keyboard() {
                this.open_next_needing_you(cx);
            }
        }))
        .on_action(cx.listener(|this, _: &Inbox, window, cx| {
            // Over another overlay the key waits; the inbox's own key puts it away.
            if this.overlay_owns_keyboard() && !this.inbox.is_open() {
                return;
            }
            this.toggle_inbox(window, cx);
        }))
        .on_action(cx.listener(|this, _: &UndoArchive, window, cx| {
            if in_own_keys(window) || !this.undo_last_archive(cx) {
                cx.propagate();
            }
        }))
        .on_action(cx.listener(|this, _: &OpenIn, window, cx| {
            if in_own_keys(window) || this.overlay_owns_keyboard() || !this.toggle_open_in(cx) {
                cx.propagate();
            }
        }))
        // slopty: from the terminal too, which never takes ⌘I.
        .on_action(cx.listener(|this, _: &ToggleInspector, _, cx| {
            if !this.view_keys_live() {
                cx.propagate();
                return;
            }
            this.toggle_inspector(cx);
        }))
        .on_action(cx.listener(|this, _: &OpenBrowser, window, cx| {
            if this.view_keys_live() {
                this.add_browser_surface(None, window, cx);
            }
        }))
        .on_action(cx.listener(|this, _: &OpenRemoteWindow, _, cx| {
            if this.view_keys_live() {
                this.add_screen_surface(cx);
            }
        }))
        .on_action(cx.listener(|this, _: &SplitRight, window, cx| {
            this.pane_key(window, cx, |this, _, cx| this.split_pane(Some(Direction::Right), cx));
        }))
        .on_action(cx.listener(|this, _: &SplitDown, window, cx| {
            this.pane_key(window, cx, |this, _, cx| this.split_pane(Some(Direction::Down), cx));
        }))
        .on_action(cx.listener(|this, _: &SplitAuto, window, cx| {
            this.pane_key(window, cx, |this, _, cx| this.split_pane(None, cx));
        }))
        .on_action(cx.listener(|this, _: &ZoomPane, window, cx| {
            this.pane_key(window, cx, |this, _, cx| this.toggle_pane_zoom(cx));
        }))
        .on_action(cx.listener(|this, _: &FocusPaneLeft, window, cx| {
            this.pane_key(window, cx, |this, _, cx| this.focus_pane_toward(Direction::Left, cx));
        }))
        .on_action(cx.listener(|this, _: &FocusPaneRight, window, cx| {
            this.pane_key(window, cx, |this, _, cx| this.focus_pane_toward(Direction::Right, cx));
        }))
        .on_action(cx.listener(|this, _: &FocusPaneUp, window, cx| {
            this.pane_key(window, cx, |this, _, cx| this.focus_pane_toward(Direction::Up, cx));
        }))
        .on_action(cx.listener(|this, _: &FocusPaneDown, window, cx| {
            this.pane_key(window, cx, |this, _, cx| this.focus_pane_toward(Direction::Down, cx));
        }))
        .on_action(cx.listener(|this, _: &NextPane, window, cx| {
            this.pane_key(window, cx, |this, _, cx| this.cycle_pane(true, cx));
        }))
        .on_action(cx.listener(|this, _: &PreviousPane, window, cx| {
            this.pane_key(window, cx, |this, _, cx| this.cycle_pane(false, cx));
        }))
    }

    /// Whether a view's key works now: on a session's page, under no overlay that holds the
    /// keyboard.
    fn view_keys_live(&self) -> bool {
        matches!(self.route, Route::Chat) && !self.active_chat.is_empty() && !self.overlay_owns_keyboard()
    }

    /// Run a pane key on a session's page or the new-session canvas, under no overlay that holds
    /// the keyboard, while the focus is not in a web page, which keeps its own ⌘[ and ⌘];
    /// elsewhere the key goes on.
    fn pane_key(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        run: impl FnOnce(&mut Self, &mut Window, &mut Context<Self>),
    ) {
        if matches!(self.route, Route::Chat) && !self.overlay_owns_keyboard() && !in_own_keys(window) {
            run(self, window, cx);
        } else {
            cx.propagate();
        }
    }

    /// slopty: the inspector's changed file: the chat's Changes in front, at that file.
    pub(super) fn show_change_at(&mut self, path: &str, window: &mut Window, cx: &mut Context<Self>) {
        crate::changes::show_file(path, cx);
        if matches!(self.route, Route::Chat) && !self.active_chat.is_empty() {
            self.show_changes(window, cx);
        }
    }

    /// Hand a find key to the chat's thread (the diff's own bindings take it in the Changes
    /// surface).
    fn find(&mut self, step: FindStep, window: &mut Window, cx: &mut Context<Self>) {
        if in_own_find(window) || !matches!(self.route, Route::Chat) || self.overlay_owns_keyboard()
        {
            cx.propagate();
            return;
        }
        let action: Box<dyn gpui::Action> = match step {
            FindStep::Open => Box::new(FindInThread),
            FindStep::Next => Box::new(FindNextInThread),
            FindStep::Previous => Box::new(FindPreviousInThread),
        };
        window.dispatch_action(action, cx);
    }

    /// After an archive: a toast that offers to undo it, and ⌘Z while it is up.
    pub(super) fn offer_undo_archive(&mut self, chat_id: String, cx: &mut Context<Self>) {
        let title = self
            .state
            .read(cx)
            .chats
            .iter()
            .find(|chat| chat.id == chat_id)
            .and_then(|chat| chat.title.clone())
            .filter(|title| !title.trim().is_empty())
            .map(|title| crate::transcript::single_line(&title));
        let words = match title {
            Some(title) => format!("Archived \u{201c}{title}\u{201d}"),
            None => "Archived the chat".to_owned(),
        };
        let shell = cx.weak_entity();
        let undo = chat_id.clone();
        let toast = crate::toast::offer(cx, words, "Undo", UNDO_LASTS, move |cx| {
            let undo = undo.clone();
            let _gone = shell.update(cx, |shell, cx| {
                shell.last_archive = None;
                shell.set_chat_archived(undo, false, cx);
            });
        });
        self.last_archive = toast.map(|toast| (chat_id, toast));
    }

    /// ⌘Z: bring back the chat archived last, while its toast is up. Whether there was one.
    fn undo_last_archive(&mut self, cx: &mut Context<Self>) -> bool {
        let Some((chat_id, toast)) = self.last_archive.take() else { return false };
        if !crate::toast::showing(cx, toast) {
            return false;
        }
        crate::toast::withdraw(cx, toast);
        self.set_chat_archived(chat_id, false, cx);
        true
    }
}
