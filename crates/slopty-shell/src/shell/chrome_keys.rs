//! slopty: the window's keys beyond zeron's (T3 Code's set, in the Mac's spelling).
//!
//! - ⌘F finds in the chat's thread, or in the diff when the Changes surface has focus; ⌘G and
//!   ⌘⇧G step through the matches. The finding itself is the transcript's and the diff's own
//!   (`transcript::FindInThread`, `FindNext`, `FindPrevious`; `changes::FindInDiff`,
//!   `FindNextInDiff`, `FindPreviousInDiff` in the `Changes` context).
//! - ⌘[ and ⌘] walk the history, as the titlebar's arrows do.
//! - ⌘T opens the right pane's new tab menu, and ⌘⇧T the tab closed last (`closed_tabs`).
//! - ⌘Z undoes the archive just made, while its toast is up.
//! - ⇧⌘O opens "Resume a session…", the picker of Claude Code's past sessions (`past_sessions`).
//! - ⌘. opens the next chat that needs the person, across projects and machines, then the
//!   finished ones nobody has seen (`sidebar_status::needing_you`).
//! - ⌥⌘O opens Open in under its button, ⇧⌘\\ expands the side pane over the chat, and ⌘I shows
//!   or hides the chat's details card (each its titlebar button's key).
//!
//! A terminal keeps every key it takes for itself (its own find, and anything its program
//! reads), and the browser keeps its own ⌘T, ⌘[ and ⌘]: those bind in their contexts, and this
//! module's handlers let a key go on when a terminal has focus.

use std::time::Duration;

use gpui::{App, Context, KeyBinding, Window, actions, prelude::*};

use super::{RightSurface, Route, Shell};
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
        NewRightTab,
        ReopenClosedTab,
        UndoArchive,
        ResumeSession,
        NextNeedsYou,
        OpenIn,
        ToggleExpandPane,
        // slopty: ⌘I shows or hides the session's inspector (`docs/decisions/ui.md`, "The
        // Inspector reads the session"): ⌘ is free of the terminal's keys and of every TUI's.
        ToggleInspector
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
    fixed!("mod-[", NavigateBack);
    fixed!("mod-]", NavigateForward);
    fixed!("mod-t", NewRightTab);
    fixed!("mod-z", UndoArchive);
    fixed!("alt-mod-o", OpenIn);
    fixed!("mod-shift-\\", ToggleExpandPane);
    fixed!("mod-i", ToggleInspector);
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
        .on_action(cx.listener(|this, _: &NewRightTab, window, cx| {
            if in_own_keys(window) || !this.open_new_right_tab(cx) {
                cx.propagate();
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
        .on_action(cx.listener(|this, _: &ToggleExpandPane, window, cx| {
            if in_own_keys(window) || !this.right_pane_shown(cx) {
                cx.propagate();
                return;
            }
            this.toggle_right_pane_expand(cx);
        }))
        // slopty: from the terminal too, which never takes ⌘I.
        .on_action(cx.listener(|this, _: &ToggleInspector, window, cx| {
            if !matches!(this.route, Route::Chat) || this.overlay_owns_keyboard() {
                cx.propagate();
                return;
            }
            this.toggle_inspector(window, cx);
        }))
    }

    /// The chat's Changes surface in front, opened when it is not up.
    fn show_changes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.route, Route::Chat) || self.active_chat.is_empty() {
            return;
        }
        self.set_surfaces_open(true, cx);
        let key = self.panel_key(cx);
        let shown = self
            .right_tabs
            .get(&key)
            .and_then(|tabs| tabs.iter().find(|tab| matches!(tab, RightSurface::Diff(_))).copied());
        match shown {
            Some(diff) => {
                self.set_right_active(diff, cx);
                if let RightSurface::Diff(id) = diff {
                    self.focus_diff(id, cx);
                }
            }
            None => self.open_diff_surface(window, cx),
        }
        cx.notify();
    }

    /// slopty: the inspector's changed file: the chat's Changes in front, at that file.
    pub(super) fn show_change_at(&mut self, path: &str, window: &mut Window, cx: &mut Context<Self>) {
        crate::changes::show_file(path, cx);
        self.show_changes(window, cx);
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

    /// ⌘T: the right pane's new tab menu, the pane opened first; with no tab yet the pane's own
    /// picker is the menu. Whether there was a chat to do it for.
    fn open_new_right_tab(&mut self, cx: &mut Context<Self>) -> bool {
        if !matches!(self.route, Route::Chat)
            || self.active_chat.is_empty()
            || self.overlay_owns_keyboard()
        {
            return false;
        }
        self.set_surfaces_open(true, cx);
        let key = self.panel_key(cx);
        if self.right_tabs.get(&key).is_some_and(|tabs| !tabs.is_empty()) {
            self.right_plus.open(());
        }
        cx.notify();
        true
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
