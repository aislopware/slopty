//! A thread whose agent has exited: what it takes to go on, through the agent's own door.
//!
//! - **Claude Code** is taken up again by a start of its own session, `claude --resume <id>` in one
//!   of the worker's terminals ([`Gone::Resume`]). The composer gives way to a strip with a Resume
//!   button, since a message typed now would reach no Claude Code.
//! - **pi and an ACP agent that loads its sessions** start again on the same session with the next
//!   message ([`Gone::ByMessage`]): the composer stays and says so.
//! - **Codex** runs its threads in the person's own app-server, which stopped. Resume is a start of
//!   the same thread, `resume <thread>` in Codex's own words, which brings the person's app-server
//!   up with Codex's published command and takes the thread up again there.
//! - **An agent that cannot load its sessions** has nothing to go on with ([`Gone::Over`]).

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, Context, InteractiveElement as _, IntoElement as _, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, div,
};
use slopty_proto::thread::wire::Start;
use slopty_proto::thread::{AgentId, Drive, Liveness, ThreadState};

use super::{ThreadView, agent_name};
use crate::colors::hsla;
use crate::icons::Symbol;
use crate::kit::ButtonKind;

/// What `claude` takes to go on with a session: `--resume <id>`
/// (`slopty_agent::resume::RESUME_FLAG`, which the app does not link).
pub const RESUME_FLAG: &str = "--resume";

/// What a start of a Codex thread takes to take it up again: `resume <thread>`
/// (`slopty_agent::codex::shared::RESUME`).
pub const CODEX_RESUME: &str = "resume";

/// How a thread whose agent exited goes on.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Gone {
    /// By this start of the agent's own session.
    Resume(Start),
    /// With the next message.
    ByMessage,
    /// It does not: the agent cannot take the session up again.
    Over,
}

/// How `state`'s thread goes on, when its agent has exited; `None` while it runs.
#[must_use]
pub fn gone(state: &ThreadState) -> Option<Gone> {
    let Liveness::Exited { resumable } = state.status.liveness else { return None };
    let meta = &state.meta;
    if !resumable {
        return Some(Gone::Over);
    }
    let codex = meta.agent.is(AgentId::CODEX) && meta.drive.is(Drive::SHARED);
    let claude = meta.agent.is(AgentId::CLAUDE_CODE) && meta.drive.is(Drive::OBSERVED);
    if !codex && !claude {
        return Some(Gone::ByMessage);
    }
    if meta.native.trim().is_empty() {
        return Some(Gone::Over);
    }
    let (model, args) = if codex {
        (None, vec![CODEX_RESUME.to_owned(), meta.native.clone()])
    } else {
        let model = state.meters.model_id.clone().filter(|m| !m.trim().is_empty());
        (model, vec![RESUME_FLAG.to_owned(), meta.native.clone()])
    };
    Some(Gone::Resume(Start {
        agent: meta.agent.clone(),
        cwd: meta.cwd.clone(),
        drive: Some(meta.drive.clone()),
        prompt: None,
        model,
        args,
        worktree: None,
    }))
}

impl ThreadView {
    /// How the thread on show goes on, when its agent has exited.
    pub(super) fn gone(&self, cx: &App) -> Option<Gone> {
        self.state(cx).and_then(gone)
    }

    /// Take the thread's exited agent up again by `start`.
    pub(super) fn resume(&self, start: Start, cx: &mut Context<Self>) {
        let thread = self.thread;
        let _id = self.hub.update(cx, |hub, cx| hub.resume(thread, start, cx));
    }

    /// The line at the composer's head while the next message is what starts the agent again.
    pub(super) fn exited_line(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let state = self.state(cx)?;
        if self.gone(cx) != Some(Gone::ByMessage) {
            return None;
        }
        let name = agent_name(&state.meta.agent);
        let words = format!("{name} exited. Your next message starts it again");
        let theme = &self.theme;
        let s = theme.surfaces;
        Some(
            div()
                .id("thread-exited-line")
                .debug_selector(|| "thread-exited-line".to_owned())
                .role(Role::Status)
                .aria_label(SharedString::from(words.clone()))
                .flex()
                .items_center()
                .gap(self.z(theme.spacing.xs))
                .text_size(self.z(theme.typography.small()))
                .text_color(hsla(s.text_secondary))
                .child(self.icon(Symbol::Power, s.text_muted))
                .child(div().min_w_0().flex_1().child(SharedString::from(words)))
                .into_any_element(),
        )
    }

    /// In the composer's place while nothing typed would reach the agent: that it exited,
    /// and the way to take it up again where there is one.
    pub(super) fn exited_strip(&self, capped: bool, cx: &Context<Self>) -> Option<AnyElement> {
        let gone = self.gone(cx).filter(|g| *g != Gone::ByMessage)?;
        let state = self.state(cx)?;
        let theme = &self.theme;
        let s = theme.surfaces;
        let name = agent_name(&state.meta.agent);
        let resuming = self.hub.read(cx).resuming(self.thread);
        let words = match &gone {
            Gone::Resume(_) if resuming => format!("Resuming {name}\u{2026}"),
            Gone::Resume(_) if state.meta.agent.is(AgentId::CODEX) => {
                format!("{name} stopped running this thread")
            }
            Gone::Resume(_) => format!("{name} exited"),
            Gone::Over | Gone::ByMessage => {
                format!("{name} exited and can't pick this session up again")
            }
        };
        let button = match gone {
            Gone::Resume(start) if !resuming => Some(
                self.button("thread-resume", "Resume", ButtonKind::Secondary)
                    .on_click(cx.listener(move |this, _ev, _w, cx| this.resume(start.clone(), cx)))
                    .into_any_element(),
            ),
            _ => None,
        };
        Some(
            div()
                .id("thread-exited")
                .debug_selector(|| "thread-exited".to_owned())
                .role(Role::Status)
                .aria_label(SharedString::from(words.clone()))
                .w_full()
                .flex()
                .items_center()
                .gap(self.z(theme.spacing.xs))
                .px(self.z(theme.spacing.md))
                .py(self.z(theme.spacing.sm))
                .map(|el| self.shell(el, capped, false))
                .text_size(self.z(theme.typography.small()))
                .text_color(hsla(s.text_secondary))
                .child(if resuming {
                    self.spinner(true)
                } else {
                    self.icon(Symbol::Power, s.text_muted)
                })
                .child(div().min_w_0().flex_1().child(SharedString::from(words)))
                .children(button)
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use slopty_proto::thread::{AgentId, Drive, Liveness, ThreadState};

    use super::{CODEX_RESUME, Gone, RESUME_FLAG, gone};
    use crate::conversation::thread::fixtures;

    fn exited(agent: &str, drive: &str, resumable: bool) -> ThreadState {
        let mut state = fixtures::empty();
        state.meta.agent = AgentId::named(agent);
        state.meta.drive = Drive::named(drive);
        state.status.liveness = Liveness::Exited { resumable };
        state
    }

    /// Each agent goes on through its own door: Claude Code by a start of its own session with
    /// the model it last ran, Codex by a start that takes its thread up again, pi and an ACP
    /// agent by the next message; one that cannot load its session does not, and a live one is
    /// not gone.
    #[test]
    fn each_agent_goes_on_through_its_own_door() {
        assert_eq!(RESUME_FLAG, slopty_agent::resume::RESUME_FLAG, "the agent's own words");
        assert_eq!(CODEX_RESUME, slopty_agent::codex::shared::RESUME);
        let mut claude = exited(AgentId::CLAUDE_CODE, Drive::OBSERVED, true);
        claude.meters.model_id = Some("opus".to_owned());
        let Some(Gone::Resume(start)) = gone(&claude) else { panic!("Claude Code resumes") };
        assert_eq!(start.args, [RESUME_FLAG.to_owned(), "s1".to_owned()]);
        assert_eq!((start.cwd.as_str(), start.model.as_deref()), ("/w", Some("opus")));
        assert_eq!(start.prompt, None, "a resume says nothing for the person");

        assert_eq!(gone(&exited(AgentId::PI, Drive::DRIVEN, true)), Some(Gone::ByMessage));
        assert_eq!(gone(&exited("acp:opencode", Drive::DRIVEN, true)), Some(Gone::ByMessage));
        assert_eq!(gone(&exited("acp:opencode", Drive::DRIVEN, false)), Some(Gone::Over));
        let Some(Gone::Resume(codex)) = gone(&exited(AgentId::CODEX, Drive::SHARED, true)) else {
            panic!("Codex takes its thread up again")
        };
        assert_eq!(codex.args, [CODEX_RESUME.to_owned(), "s1".to_owned()]);
        assert_eq!((codex.model, codex.prompt), (None, None));

        let mut nameless = exited(AgentId::CLAUDE_CODE, Drive::OBSERVED, true);
        nameless.meta.native.clear();
        assert_eq!(gone(&nameless), Some(Gone::Over), "no session id, nothing to resume");

        let mut live = exited(AgentId::CLAUDE_CODE, Drive::OBSERVED, true);
        live.status.liveness = Liveness::Live;
        assert_eq!(gone(&live), None);
    }
}
