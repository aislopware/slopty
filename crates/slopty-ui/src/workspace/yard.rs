//! The yard (lively companions): each live agent's companion standing in the status bar, beside
//! its agent summary (`docs/decisions/brand.md`, "Companions").
//!
//! Needs you stands first, then the rest down the attention ladder. Each is a button: a click
//! goes to its agent's tile (or opens one), and the pointer's hint says who it is and what it
//! does. Past what fits, a count. Idle ones walk out to meet a neighbour and blink, but only on
//! frames a working one draws anyway; with nothing at work the yard holds still. Once the person
//! has been away from the machine for two minutes, everyone not waiting on them falls asleep,
//! and nothing in the yard moves.

use std::rc::Rc;

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AppContext as _, InteractiveElement as _, IntoElement as _, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, px,
};
use slopty_proto::thread::AgentId;

use super::WorkspaceView;
use super::agents::{Step, Waiting, agent_mark_of, agent_status_text};
use super::faces::ThreadWait;
use crate::companions::{self, Kind, Pose, companion};
use crate::draw::Draw;
use crate::icons::Status;
use crate::kit;

/// The most of the window's width the yard takes, so the bar's own readouts always fit.
const YARD_SHARE: f32 = 0.25;

/// One agent standing in the yard.
struct Dweller {
    /// Its agent, by its [`AgentId`] name.
    agent: String,
    pose: Pose,
    /// Where a click takes the person.
    step: Step,
    /// Who it is and what it does, as its hint and a screen reader say.
    label: String,
    /// Its own, for its moments and its phase among the others.
    key: String,
}

impl WorkspaceView {
    /// Everyone in the yard, needs you first: the agents at work in terminals, on every
    /// worker, and the threads no terminal speaks for.
    fn dwellers(&self) -> Vec<Dweller> {
        let mut out = Vec::new();
        for (session, stand) in self.agent_sessions() {
            let worker = stand.worker;
            let Some(agent) = self.session_agent(session) else { continue };
            let status = agent_mark_of(stand);
            let what = agent_status_text(stand);
            out.push(Dweller {
                agent: agent.to_owned(),
                pose: Pose::of_status(Some(status)),
                step: Step::Session(Waiting {
                    worker,
                    tile: self.tile_of_session(session),
                    session,
                }),
                label: said(agent, &what),
                key: session.to_string(),
            });
        }
        for (thread, stand) in self.thread_stands() {
            let Some(agent) = self.thread_agent(thread) else { continue };
            let status = stand.status();
            out.push(Dweller {
                agent: agent.to_owned(),
                pose: Pose::of_status(status),
                step: Step::Thread(ThreadWait {
                    worker: stand.worker,
                    thread,
                    tile: self.tile_of_thread(thread),
                }),
                label: said(agent, status.map_or("Idle", Status::label)),
                key: thread.to_string(),
            });
        }
        out.sort_by(|a, b| b.pose.rank().cmp(&a.pose.rank()).then_with(|| a.key.cmp(&b.key)));
        out
    }

    /// The yard, lively only and not on a phone: none with no agent about.
    pub(super) fn render_yard(
        &self,
        window: &Window,
        cx: &Draw<'_, Self>,
    ) -> Option<gpui::AnyElement> {
        let theme = &self.theme;
        if !companions::lively(theme) {
            return None;
        }
        let mut dwellers = self.dwellers();
        if dwellers.is_empty() {
            return None;
        }
        let away = companions::away(cx);
        let rides = !away && dwellers.iter().any(|d| matches!(d.pose, Pose::Working(_)));
        let slot = theme.typography.icon_large();
        let gap = theme.spacing.sm;
        let budget = f32::from(window.viewport_size().width) * YARD_SHARE;
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a count of slots in a quarter of a window, floored and at least nought"
        )]
        let fits = (budget / (slot + gap)).floor().max(0.0) as usize;
        let more = dwellers.len().saturating_sub(fits);
        // The count takes a slot of its own.
        dwellers.truncate(if more > 0 { fits.saturating_sub(1) } else { fits });
        let more = more.saturating_add(usize::from(more > 0));
        let hint_theme = Rc::new(theme.clone());
        let shown = dwellers.into_iter().enumerate().map(|(i, d)| {
            let pose = posed(d.pose, away);
            let phase = u32::try_from(i / 2).unwrap_or(0).saturating_mul(37);
            let toward = if i % 2 == 0 { 1 } else { -1 };
            let mut mark = companion(theme, Kind::of(&d.agent), pose)
                .large()
                .side(px(slot))
                .phase(phase)
                .one_shots(SharedString::from(format!("yard-companion-{}", d.key)));
            if rides {
                mark = mark.playing().walks(toward);
            }
            let (label, step) = (SharedString::from(d.label), d.step);
            let hint_theme = Rc::clone(&hint_theme);
            div()
                .id(SharedString::from(format!("yard-{}", d.key)))
                .debug_selector({
                    let key = d.key;
                    move || format!("yard-{key}")
                })
                .role(Role::Button)
                .aria_label(label.clone())
                .flex_none()
                .size(px(slot))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .map(kit::hint_timing)
                .tooltip(move |_window, cx| {
                    let theme = Rc::clone(&hint_theme);
                    cx.new(|_| kit::Hint::new(label.clone(), "", theme)).into()
                })
                .on_click(cx.listener(move |this, _ev, _w, cx| this.go_to_step(step, cx)))
                .child(mark)
        });
        let count = (more > 0).then(|| {
            div()
                .debug_selector(|| "yard-more".to_owned())
                .flex_none()
                .child(SharedString::from(format!("+{more}")))
        });
        Some(
            div()
                .id("yard")
                .debug_selector(|| "yard".to_owned())
                .role(Role::Group)
                .aria_label("Agents")
                .flex_none()
                .h_full()
                .flex()
                .items_center()
                .gap(px(gap))
                .children(shown)
                .children(count)
                .into_any_element(),
        )
    }
}

/// How a dweller in `pose` stands: asleep while the person is `away`, unless it waits on them
/// or failed, which still want a look when they come back.
const fn posed(pose: Pose, away: bool) -> Pose {
    if away && !matches!(pose, Pose::NeedsYou | Pose::Failed) { Pose::Asleep } else { pose }
}

#[cfg(test)]
impl WorkspaceView {
    /// The yard's poses in order, the person `away` or not.
    pub(super) fn yard_poses(&self, away: bool, _cx: &gpui::App) -> Vec<Pose> {
        self.dwellers().into_iter().map(|d| posed(d.pose, away)).collect()
    }
}

/// Who `agent` is and what it does: "Claude Code · Edit src/main.rs".
fn said(agent: &str, what: &str) -> String {
    let who = super::projects::agent_label(&AgentId(agent.to_owned()));
    if what.is_empty() { who } else { format!("{who} \u{b7} {what}") }
}
