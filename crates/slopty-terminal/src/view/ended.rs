//! A session that ended: a bar across the foot of its pane, as ghostty's
//! `ChildExitedMessageBar` draws it (`docs/decisions/terminal.md`, "A session that ended says
//! how"). It says who ended, how quickly when that was too quick to be a real run, and the
//! code when it was not 0, tinted as an error then; ↩ starts the session again
//! ([`TerminalViewEvent::StartAgain`]) and the shell's close key closes it. The bar steps aside
//! while the pointer is over it, so the last lines stay readable.

use std::fmt::Write as _;
use std::time::{Duration, Instant};

use gpui::prelude::FluentBuilder as _;
use gpui::{
    Context, InteractiveElement as _, IntoElement as _, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, px,
};
use slopty_shell::live::chrome;

use super::{TerminalView, TerminalViewEvent};

/// A run shorter than this ended abnormally, whatever its code: ghostty's
/// `abnormal-command-exit-runtime`.
pub(super) const ABNORMAL_RUN: Duration = Duration::from_millis(250);

/// What the bar calls a program the shell did not name.
const NAMELESS: &str = "The process";

/// The bar's padding across and down: ghostty's 8 × 4.
const BAR_PADDING: (f32, f32) = (8.0, 4.0);

/// The bar's text at most: ghostty's 30 pt cap on a cell's 0.8.
const BAR_TEXT_MOST: f32 = 30.0;

/// What the view knows of the program's run, for the bar that says it ended.
#[derive(Default, Debug)]
pub(super) struct Run {
    /// What the program is called, as the shell names it ("Claude Code", "zsh").
    program: Option<SharedString>,
    /// When it started, on the shell's clock: the shell says, since a view made for a session
    /// that ran long before it sees only the end.
    began: Option<Instant>,
    /// How long it ran, once it ended with a start known.
    lasted: Option<Duration>,
    /// The pointer is over the bar, which steps aside.
    hovered: bool,
}

impl Run {
    /// It ended at `now`.
    fn ended(&mut self, now: Instant) {
        if self.lasted.is_none() {
            self.lasted = self.began.map(|began| now.saturating_duration_since(began));
        }
    }

    /// It runs again from `now`, after it ended: the next end is timed from here.
    const fn again(&mut self, now: Instant) {
        if self.lasted.is_some() {
            self.began = Some(now);
            self.lasted = None;
        }
    }
}

/// What the bar says of a run of `program` that ended with `status` after `lasted`: who
/// ended; how quickly, when under [`ABNORMAL_RUN`]; and the code, when it was not 0.
/// "Claude Code exited after 120 ms, code 1".
pub(super) fn words(program: &str, status: Option<i32>, lasted: Option<Duration>) -> String {
    let mut out = format!("{program} exited");
    if let Some(quick) = lasted.filter(|lasted| *lasted < ABNORMAL_RUN) {
        let _infallible = write!(out, " after {} ms", quick.as_millis());
    }
    if let Some(code) = status.filter(|code| *code != 0) {
        let _infallible = write!(out, ", code {code}");
    }
    out
}

/// Whether a run that ended with `status` after `lasted` failed: a code other than 0, or a run
/// under [`ABNORMAL_RUN`] (a program that could not start).
pub(super) fn failed(status: Option<i32>, lasted: Option<Duration>) -> bool {
    status.is_some_and(|code| code != 0) || lasted.is_some_and(|lasted| lasted < ABNORMAL_RUN)
}

impl TerminalView {
    /// What the program is called and how long it had run when this view was made, from the
    /// shell's records of the session (`Duration::ZERO` for one it starts now): the bar then
    /// says a run under 250 ms (ghostty's `abnormal-command-exit-runtime`) ended that quickly.
    pub fn set_run(
        &mut self,
        program: impl Into<SharedString>,
        ran: Duration,
        cx: &mut Context<Self>,
    ) {
        self.run.program = Some(program.into());
        self.run.began = slopty_shell::clock::now(cx).checked_sub(ran);
        cx.notify();
    }

    /// The program ended: how long it ran is read now.
    pub(super) fn run_ended(&mut self, cx: &Context<Self>) {
        self.run.ended(slopty_shell::clock::now(cx));
    }

    /// The session runs, after its program had ended: the worker started it again.
    pub(super) fn run_going(&mut self, cx: &Context<Self>) {
        if self.exited().is_none() {
            self.run.again(slopty_shell::clock::now(cx));
        }
    }

    /// ↩ after the program ended: the shell starts the session again.
    pub(super) fn start_again(&self, cx: &mut Context<Self>) -> bool {
        if self.exited().is_none() {
            return false;
        }
        cx.emit(TerminalViewEvent::StartAgain);
        true
    }

    /// The bar's height, laid out at the grid's line height: the foot's pills stand above it.
    pub(super) fn ended_bar_height(&self) -> f32 {
        if self.exited().is_none() {
            return 0.0;
        }
        self.metrics.map_or(0.0, |m| {
            let text = (f32::from(m.line_height) * 0.8).min(BAR_TEXT_MOST);
            // The line box GPUI gives the text, 1.25 of its size, and the padding around it.
            text.mul_add(1.25, BAR_PADDING.1 * 2.0)
        })
    }

    /// The bar across the pane's foot once the program ended: the words, then the keys that
    /// start it again and close it, on the pane's own ground, or that ground half red when the
    /// run failed. Nothing while it runs.
    pub(super) fn render_ended(&self, cx: &Context<Self>) -> Option<gpui::AnyElement> {
        let status = self.exited()?;
        let m = self.metrics?;
        let program = self.run.program.as_deref().unwrap_or(NAMELESS);
        let words = SharedString::from(words(program, status, self.run.lasted));
        let failed = failed(status, self.run.lasted);
        let shell = chrome::theme(cx);
        let ground = self.ground();
        let fg = self.ground_text();
        let text = (f32::from(m.line_height) * 0.8).min(BAR_TEXT_MOST);
        let again = slopty_shell::keys::stroke_text(gpui::Modifiers::default(), "enter");
        let close = slopty_shell::keys::for_action_anywhere(&slopty_shell::app_menus::CloseTab, cx);
        let hint = |key: String, what: &'static str| {
            div()
                .flex()
                .flex_none()
                .gap(px(4.0))
                .child(div().text_color(fg).child(key))
                .child(div().text_color(fg.opacity(0.7)).child(what))
        };
        let hovered = self.run.hovered;
        Some(
            div()
                .id("terminal-ended")
                .debug_selector(|| "terminal-ended".to_owned())
                .role(gpui::Role::Status)
                .aria_label(words.clone())
                .absolute()
                .left_0()
                .right_0()
                .bottom_0()
                .bg(slopty_kit::colors::hsla(ground.mix(self.ground_ink(), 0.08)))
                .when(hovered, |el| el.opacity(0.0))
                .on_hover(cx.listener(|this, hovered: &bool, _window, cx| {
                    if this.run.hovered != *hovered {
                        this.run.hovered = *hovered;
                        cx.notify();
                    }
                }))
                .child(
                    div()
                        .when(failed, |el| el.bg(shell.danger.opacity(0.5)))
                        .px(px(BAR_PADDING.0))
                        .py(px(BAR_PADDING.1))
                        .flex()
                        .items_center()
                        .gap(px(16.0))
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .font_family(shell.font_sans.clone())
                        .text_size(px(text))
                        .text_color(fg)
                        .child(div().flex_1().min_w_0().truncate().child(words))
                        .child(hint(again, "Start again"))
                        .children(close.map(|close| hint(close.to_string(), "Close"))),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The words say who ended, how quickly only when that was too quick to be a run, and the
    /// code only when it was not 0; a failure is either.
    #[test]
    fn an_ended_run_says_how_it_ended() {
        let ms = Duration::from_millis;
        assert_eq!(
            words("Claude Code", Some(1), Some(ms(120))),
            "Claude Code exited after 120 ms, code 1"
        );
        assert_eq!(words("zsh", Some(0), Some(ms(90_000))), "zsh exited");
        assert_eq!(words("zsh", Some(130), Some(ms(90_000))), "zsh exited, code 130");
        assert_eq!(words("pi", Some(0), Some(ms(12))), "pi exited after 12 ms");
        assert_eq!(words("pi", None, None), "pi exited");
        assert!(failed(Some(1), None), "a code");
        assert!(failed(Some(0), Some(ms(249))), "too quick");
        assert!(failed(None, Some(ms(0))), "too quick, no code");
        assert!(!failed(Some(0), Some(ABNORMAL_RUN)), "250 ms is a run");
        assert!(!failed(None, None), "nothing known");
    }

    /// A start the shell never said leaves the length unknown, the first end is the one that
    /// counts, and a run started again is timed from then.
    #[test]
    fn a_run_is_timed_from_the_start_the_shell_said() {
        let began = Instant::now();
        let mut run = Run::default();
        run.ended(began);
        assert_eq!(run.lasted, None, "no start known");
        let mut run = Run { began: Some(began), ..Run::default() };
        run.ended(began.checked_add(Duration::from_millis(120)).unwrap());
        run.ended(began.checked_add(Duration::from_secs(9)).unwrap());
        assert_eq!(run.lasted, Some(Duration::from_millis(120)));
        let again = began.checked_add(Duration::from_secs(10)).unwrap();
        run.again(again);
        assert_eq!((run.began, run.lasted), (Some(again), None));
        run.again(began);
        assert_eq!(run.began, Some(again), "still running: the start stands");
        run.ended(again.checked_add(Duration::from_millis(30)).unwrap());
        assert_eq!(run.lasted, Some(Duration::from_millis(30)));
    }
}
