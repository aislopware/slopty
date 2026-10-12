//! "!" as the first key of an empty composer opens a terminal in the next turn's place, Delta's
//! inline terminal (`.research/delta-signed-in-2026-10-12.md` §3.15): a real shell on the
//! thread's machine, in its folder, that the person runs what they like in. The next message
//! goes with what it shows, as a console block over the words, and the shell goes with it;
//! its ✕ (or Esc in the composer) lets it go unsent.
//!
//! The workspace makes the shell ([`ThreadViewEvent::InlineTerminal`]) and hands its view
//! back ([`ThreadView::inline_terminal`]); the view says when it is done with it
//! ([`ThreadViewEvent::InlineTerminalDone`]).

use gpui::accesskit::Role;
use gpui::{
    AnyElement, App, Context, Entity, Focusable as _, InteractiveElement as _, IntoElement as _,
    KeyDownEvent, ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _,
    Window, div, px,
};
use slopty_core::SessionId;
use slopty_kit::colors::hsla;
use slopty_kit::icons::Symbol;
use slopty_kit::kit;
use slopty_terminal::TerminalView;

use super::{ThreadView, ThreadViewEvent};

/// How many rows of the shell the card shows.
pub(super) const ROWS: f32 = 12.0;

/// What the card's head names the shell by.
pub(super) const SHELL: &str = "Terminal";

/// What the card's ✕ says.
pub(super) const CLOSE: &str = "Close the terminal";

/// Where the inline terminal stands.
pub(super) enum InlineShell {
    /// Asked of the workspace; its shell has not come yet.
    Asked,
    /// On show in the next turn's place.
    Shown {
        /// Its shell.
        session: SessionId,
        /// Its view.
        view: Entity<TerminalView>,
    },
}

impl ThreadView {
    /// A key the composer would take: "!" typed into an empty composer of a thread with a
    /// folder opens the inline terminal instead of being typed. Whether it did.
    pub(super) fn bang(
        &mut self,
        ev: &KeyDownEvent,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let keys = &ev.keystroke;
        let plain = !keys.modifiers.control && !keys.modifiers.platform && !keys.modifiers.alt;
        // The character typed, or the key itself where nothing says what it typed.
        let typed = keys.key_char.as_deref().unwrap_or(keys.key.as_str());
        if !plain || typed != "!" {
            return false;
        }
        if self.inline.is_some()
            || self.draft.is_some()
            || !self.composer_focused(window, cx)
            || self.composing(cx)
            || !self.draft(cx).is_empty()
        {
            return false;
        }
        let Some(cwd) = self.root(cx) else { return false };
        self.inline = Some(InlineShell::Asked);
        cx.emit(ThreadViewEvent::InlineTerminal { cwd });
        cx.notify();
        true
    }

    /// The shell asked for by "!" came: it shows in the next turn's place, with the keyboard.
    pub fn inline_terminal(
        &mut self,
        session: SessionId,
        view: Entity<TerminalView>,
        cx: &mut Context<Self>,
    ) {
        if !matches!(self.inline, Some(InlineShell::Asked)) {
            // Let go before it came: it goes at once.
            cx.emit(ThreadViewEvent::InlineTerminalDone { session });
            return;
        }
        // The keyboard goes to it in the frame it first shows in ([`Self::inline_card`]).
        self.inline_focus = true;
        self.inline = Some(InlineShell::Shown { session, view });
        self.list.scroll_to_end();
        cx.notify();
    }

    /// The shell asked for by "!" never came, or ended: its place goes.
    pub fn inline_terminal_gone(&mut self, cx: &mut Context<Self>) {
        if self.inline.take().is_some() {
            cx.notify();
        }
    }

    /// The inline terminal's shell, while one is on show.
    #[must_use]
    pub const fn inline_session(&self) -> Option<SessionId> {
        match &self.inline {
            Some(InlineShell::Shown { session, .. }) => Some(*session),
            _ => None,
        }
    }

    /// Let the inline terminal go, unsent; the keyboard goes back to the composer. Whether one
    /// was open.
    pub(super) fn close_inline(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(shell) = self.inline.take() else { return false };
        if let InlineShell::Shown { session, .. } = shell {
            cx.emit(ThreadViewEvent::InlineTerminalDone { session });
        }
        window.focus(&self.composer.focus_handle(cx), cx);
        cx.notify();
        true
    }

    /// What the inline terminal shows, as a console block to lead a message: its lines with
    /// the blank ones before and after them dropped. `None` with none on show or nothing in
    /// it.
    pub(super) fn inline_block(&self, cx: &App) -> Option<String> {
        let Some(InlineShell::Shown { view, .. }) = &self.inline else { return None };
        console_block(&view.read(cx).lines())
    }

    /// The message about to go, led by what the inline terminal shows, which then goes:
    /// `words` alone with none on show.
    pub(super) fn with_inline(&mut self, words: &str, cx: &mut Context<Self>) -> Option<String> {
        let block = self.inline_block(cx)?;
        if let Some(InlineShell::Shown { session, .. }) = self.inline.take() {
            cx.emit(ThreadViewEvent::InlineTerminalDone { session });
        }
        Some(if words.is_empty() { block } else { format!("{block}\n\n{words}") })
    }

    /// The inline terminal's card in the next turn's place: the code well, its head the shell's
    /// name and the way to let it go, then the shell, [`ROWS`] tall.
    pub(super) fn inline_card(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if let Some(InlineShell::Shown { view, .. }) = &self.inline
            && std::mem::take(&mut self.inline_focus)
        {
            window.focus(&view.focus_handle(cx), cx);
            // A focus moved while drawing asks for no frame of its own: the shell's caret
            // shows it in the next.
            let shell = view.downgrade();
            window.on_next_frame(move |_w, cx| {
                let _gone = shell.update(cx, |_, cx| cx.notify());
            });
        }
        let shell = self.inline.as_ref()?;
        let theme = &self.theme;
        let s = theme.surfaces;
        let line = theme.typography.mono_size * theme.typography.mono_line_height;
        let close = div()
            .id("inline-terminal-close")
            .debug_selector(|| "inline-terminal-close".to_owned())
            .role(Role::Button)
            .aria_label(CLOSE)
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .size(px(theme.density.button))
            .rounded(px(theme.radii.sm))
            .cursor_pointer()
            .hover(move |el| el.bg(hsla(s.hover)))
            .child(slopty_kit::icons::icon(theme, Symbol::Xmark, hsla(s.icon_muted)))
            .on_click(cx.listener(|this, _ev, window, cx| {
                this.close_inline(window, cx);
            }));
        let head = div()
            .w_full()
            .flex()
            .items_center()
            .gap(px(theme.spacing.xs))
            .pl(px(theme.spacing.sm))
            .pr(px(theme.spacing.xxs))
            .h(px(theme.density.row))
            .text_size(px(theme.roles().row.size))
            .text_color(hsla(s.text_secondary))
            .child(slopty_kit::icons::icon(theme, Symbol::Terminal, hsla(s.icon_muted)))
            .child(div().flex_1().min_w_0().child(SharedString::from(SHELL)))
            .child(close);
        let body = match shell {
            InlineShell::Asked => kit::meta(div(), theme)
                .px(px(theme.spacing.sm))
                .py(px(theme.spacing.xs))
                .child("Starting a shell\u{2026}")
                .into_any_element(),
            InlineShell::Shown { view, .. } => {
                div().w_full().h(px((ROWS * line).round())).child(view.clone()).into_any_element()
            }
        };
        Some(
            kit::well(div(), theme)
                .id("inline-terminal")
                .debug_selector(|| "inline-terminal".to_owned())
                .role(Role::Group)
                .aria_label(SHELL)
                .w_full()
                .mt(px(theme.spacing.sm))
                .overflow_hidden()
                .flex()
                .flex_col()
                .child(head)
                .child(body)
                .into_any_element(),
        )
    }
}

/// `lines` as a console block: the blank lines before and after the rest dropped, `None` when
/// nothing is left.
pub(super) fn console_block(lines: &[String]) -> Option<String> {
    let first = lines.iter().position(|l| !l.trim().is_empty())?;
    let last = lines.iter().rposition(|l| !l.trim().is_empty())?;
    let body = lines.get(first..=last)?.join("\n");
    Some(format!("```console\n{body}\n```"))
}

#[cfg(test)]
mod tests {
    use super::console_block;

    #[test]
    fn the_block_keeps_what_the_shell_shows() {
        let lines: Vec<String> = ["", "~ % ls", "Cargo.toml  src", "~ %", "", ""]
            .iter()
            .map(|l| (*l).to_owned())
            .collect();
        assert_eq!(
            console_block(&lines).as_deref(),
            Some("```console\n~ % ls\nCargo.toml  src\n~ %\n```")
        );
        assert_eq!(console_block(&[String::new(), " ".to_owned()]), None);
    }
}
