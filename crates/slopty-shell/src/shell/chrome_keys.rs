//! slopty: the window's keys beyond zeron's (T3 Code's set, in the Mac's spelling).
//!
//! - ⌘F finds in the chat's thread, or in the diff when the Changes surface has focus; ⌘G and
//!   ⌘⇧G step through the matches. The finding itself is the transcript's and the diff's own
//!   (`transcript::FindInThread`, `FindNext`, `FindPrevious`; `changes::FindInDiff`,
//!   `FindNextInDiff`, `FindPreviousInDiff` in the `Changes` context).
//! - A transcript's "Open diff" (`changes::ShowTurnDiff`) opens Changes on that turn.
//! - ⌘[ and ⌘] walk the history, as the titlebar's arrows do.
//! - ⌘T opens the right pane's new tab menu, and ⌘⇧T the tab closed last (`closed_tabs`).
//! - ⌥⌘B opens the branch picker and ⌘⇧E steps the model's effort. (⌘⇧G is Find previous, and
//!   ⌘⇧B is where an older map's sidebar key moves.)
//! - ⌘Z undoes the archive just made, while its toast is up.
//! - ⇧⌘O opens "Resume a session…", the picker of Claude Code's past sessions (`past_sessions`).
//!
//! A terminal keeps every key it takes for itself (its own find, and anything its program
//! reads), and the browser keeps its own ⌘T, ⌘[ and ⌘]: those bind in their contexts, and this
//! module's handlers let a key go on when a terminal has focus.

use std::time::Duration;

use gpui::{App, Context, KeyBinding, Window, actions, prelude::*};

use super::{RightSurface, Route, Shell};
use crate::changes::{FindInDiff, FindNextInDiff, FindPreviousInDiff, ShowTurnDiff};
use crate::settings::{KeymapConfig, ShortcutId, platform_combo};
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
        OpenBranchPicker,
        NextEffort,
        UndoArchive,
        ResumeSession
    ]
);

/// How long the toast that offers to undo an archive stays.
const UNDO_LASTS: Duration = Duration::from_secs(8);

/// Bind the keys, the configurable ones as `keymap` says. A fixed key another shortcut was given
/// stays with that shortcut.
pub(super) fn bind_keys(cx: &mut App, keymap: &KeymapConfig) {
    let taken = |combo: &str| {
        ShortcutId::ALL.iter().any(|id| {
            gpui::Keystroke::parse(&platform_combo(keymap.get(*id))).ok()
                == gpui::Keystroke::parse(&platform_combo(combo)).ok()
        })
    };
    let mut bindings = Vec::new();
    macro_rules! fixed {
        ($combo:literal, $action:expr) => {
            if !taken($combo) {
                bindings.push(KeyBinding::new(&platform_combo($combo), $action, None));
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
    // The diff's own find, where the Changes surface has the keyboard.
    bindings.push(KeyBinding::new(&platform_combo("mod-f"), FindInDiff, Some("Changes")));
    bindings.push(KeyBinding::new(&platform_combo("mod-g"), FindNextInDiff, Some("Changes")));
    bindings.push(KeyBinding::new(
        &platform_combo("mod-shift-g"),
        FindPreviousInDiff,
        Some("Changes"),
    ));
    for (id, action) in [
        (ShortcutId::ReopenClosedTab, Box::new(ReopenClosedTab) as Box<dyn gpui::Action>),
        (ShortcutId::OpenBranchPicker, Box::new(OpenBranchPicker)),
        (ShortcutId::NextEffort, Box::new(NextEffort)),
        (ShortcutId::ResumeSession, Box::new(ResumeSession)),
    ] {
        let combo = platform_combo(keymap.get(id));
        if combo.is_empty() {
            continue;
        }
        // A chord that is no chord (a hand-edited file) binds nothing.
        if let Ok(binding) =
            KeyBinding::load(&combo, action, None, false, None, &gpui::DummyKeyboardMapper)
        {
            bindings.push(binding);
        }
    }
    cx.bind_keys(bindings);
}

/// Whether the focus is in a terminal or a web page, which keep their own keys.
fn in_own_keys(window: &Window) -> bool {
    window
        .context_stack()
        .iter()
        .any(|context| context.contains("Terminal") || context.contains("Browser"))
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
        .on_action(cx.listener(|this, _: &OpenBranchPicker, window, cx| {
            if !matches!(this.route, Route::Chat) || this.overlay_owns_keyboard(cx) {
                return;
            }
            let pickers = this.composer.read(cx).pickers().clone();
            pickers.update(cx, |pickers, cx| pickers.open_branch_menu(window, cx));
        }))
        .on_action(cx.listener(|this, _: &NextEffort, _, cx| {
            if !matches!(this.route, Route::Chat) || this.overlay_owns_keyboard(cx) {
                return;
            }
            let pickers = this.composer.read(cx).pickers().clone();
            pickers.update(cx, |pickers, cx| pickers.step_effort(cx));
        }))
        .on_action(cx.listener(|this, _: &ResumeSession, window, cx| {
            // Over another overlay the key waits; the picker's own key shuts it.
            if this.overlay_owns_keyboard(cx) && !this.past_sessions.picker_open() {
                return;
            }
            this.toggle_resume_picker(window, cx);
        }))
        .on_action(cx.listener(|this, _: &UndoArchive, window, cx| {
            if in_own_keys(window) || !this.undo_last_archive(cx) {
                cx.propagate();
            }
        }))
        .on_action(cx.listener(|this, _: &ShowTurnDiff, window, cx| {
            this.show_turn_diff(window, cx);
        }))
    }

    /// A transcript's "Open diff": the chat's Changes surface in front, which takes the turn the
    /// transcript asked for (`changes::show_turn`).
    fn show_turn_diff(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
            Some(diff) => self.set_right_active(diff, cx),
            None => self.add_diff_surface(window, cx),
        }
        cx.notify();
    }

    /// Hand a find key to the chat's thread (the diff's own bindings take it in the Changes
    /// surface).
    fn find(&mut self, step: FindStep, window: &mut Window, cx: &mut Context<Self>) {
        if in_own_keys(window) || !matches!(self.route, Route::Chat) || self.overlay_owns_keyboard(cx)
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
            || self.overlay_owns_keyboard(cx)
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
            None => "Archived the session".to_owned(),
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
