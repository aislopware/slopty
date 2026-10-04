//! Companions: a small pixel character for each agent, standing in its mark's slot
//! (`docs/decisions/brand.md`, "Companions").
//!
//! Each agent kind has a character of its own, recognised by its colour and silhouette and
//! never drawn after a vendor's mascot or mark: Ember for Claude Code, Brace for Codex, Pi (from
//! pi's own MIT mark) for pi, Op for `OpenCode`, and Blob, wearing an accessory picked by its
//! name, for any other agent. Dot, the brand's grid come alive, stands where no agent is.
//!
//! A companion's [`Pose`] says its agent's state; the words beside it say it too, so it is
//! decorative to assistive tech wherever a row already speaks. Only a working companion moves
//! on its own, stepping on the working mark's clock (`icons::wake_at_next_step`), so it
//! adds no frame. Under Reduce Motion every companion holds its pose; a working one breathes
//! in opacity as the working mark does, the one live cue the platform keeps.
//!
//! `[theme] companions` sets how much they do ([`slopty_theme::Companions`]): off, quiet (poses,
//! a working one stepping) or lively (they also wave, hop and play in the yard).
//!
//! Sprites are code ([`sprites`]): ASCII grids read at compile time, their inks filled from the
//! theme as they paint ([`Palette`]), each row's runs of one ink painted as one snapped quad.

mod clock;
mod dot;
mod paint;
mod pose;
pub mod sprites;
#[cfg(test)]
mod tests;

use std::time::Duration;

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, Div, ElementId, Global, Hsla, InteractiveElement as _, IntoElement as _,
    ParentElement as _, Pixels, Stateful, StatefulInteractiveElement as _, Styled as _, div, px,
};
use slopty_proto::thread::{AgentId, ItemBody, Phase, ThreadState, ToolState, TurnId};
use slopty_theme::{Companions, Contrast, Rgb, Theme};

pub(crate) use self::dot::beside_mark;
pub use self::paint::{Companion, companion};
pub use self::pose::{Beat, Canvas, Pose, Rect, Rects, Task, frame, rects, runs};
use self::sprites::{Grid, Ink};
use crate::colors::hsla;
use crate::icons::{AgentMark, Glyph, Status};

/// Which character a companion is.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Kind {
    /// Claude Code's: round, a sparkle tuft, in the theme's agent orange.
    Ember,
    /// Codex's: a capsule with a visor, braces for arms, in ink.
    Brace,
    /// pi's: its own mark with eyes, in its three colours.
    Pi,
    /// `OpenCode`'s: a box in its mark's frame, in ink.
    Op,
    /// Any other agent's: a dome in ink wearing [`sprites::ACCESSORIES`]'s one at this index.
    Blob(u8),
    /// Slopty's own: the mark's grid in the brand's green, the cursor its eye.
    Dot,
}

impl Kind {
    /// Every character, a Blob with each accessory, for the sprite checks.
    pub fn all() -> impl Iterator<Item = Self> {
        let blobs = (0..sprites::ACCESSORIES.len()).filter_map(|i| u8::try_from(i).ok());
        [Self::Ember, Self::Brace, Self::Pi, Self::Op, Self::Dot]
            .into_iter()
            .chain(blobs.map(Self::Blob))
    }

    /// The companion of the agent named `agent` (an [`AgentId`]'s name), reached directly or
    /// over ACP. An agent Slopty has no character for is a Blob, and its name picks the
    /// accessory, the same on every run and machine.
    #[must_use]
    pub fn of(agent: &str) -> Self {
        match AgentMark::of(agent) {
            AgentMark::ClaudeCode => Self::Ember,
            AgentMark::Codex => Self::Brace,
            AgentMark::Pi => Self::Pi,
            AgentMark::OpenCode => Self::Op,
            AgentMark::Other => {
                let name = agent.strip_prefix(AgentId::ACP_PREFIX).unwrap_or(agent);
                let count = u64::try_from(sprites::ACCESSORIES.len()).unwrap_or(1);
                let index = fnv1a(name.as_bytes()).checked_rem(count).unwrap_or(0);
                Self::Blob(u8::try_from(index).unwrap_or(0))
            }
        }
    }

    /// Its standing body.
    const fn body(self) -> &'static Grid {
        match self {
            Self::Ember => &sprites::EMBER,
            Self::Brace => &sprites::BRACE,
            Self::Pi => &sprites::PI,
            Self::Op => &sprites::OP,
            Self::Blob(_) => &sprites::BLOB,
            Self::Dot => &sprites::DOT,
        }
    }

    /// The accessory at `index`, wrapped into the set.
    fn accessory(index: u8) -> &'static Grid {
        let count = sprites::ACCESSORIES.len().max(1);
        let at = usize::from(index).checked_rem(count).unwrap_or(0);
        sprites::ACCESSORIES.get(at).unwrap_or(&sprites::BLOB)
    }
}

/// FNV-1a over `bytes`: a hash that is the same on every run, unlike std's.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash: u64, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// Which grid a companion is drawn on.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Hash)]
pub enum Size {
    /// Eight cells a side: a row's mark slot, the working line.
    #[default]
    Small,
    /// Sixteen, the small one doubled and smoothed: the yard, the empty states.
    Large,
}

impl Size {
    /// Its side, in cells.
    #[must_use]
    pub const fn cells(self) -> usize {
        match self {
            Self::Small => sprites::SIDE,
            Self::Large => pose::LARGE,
        }
    }
}

/// The frame `kind` shows in `pose` turned by `beat`, on `size`'s grid.
#[must_use]
pub fn drawn(kind: Kind, pose: Pose, beat: Beat, size: Size) -> Canvas {
    let small = frame(kind, pose, beat);
    match size {
        Size::Small => small,
        // pi's mark is square blocks, kept square.
        Size::Large => small.doubled(kind != Kind::Pi),
    }
}

/// Whether the theme draws companions at all.
#[must_use]
pub fn shown(theme: &Theme) -> bool {
    theme.behaviour.companions != Companions::Off
}

/// Whether the theme's companions are lively: they wave, hop and play.
#[must_use]
pub fn lively(theme: &Theme) -> bool {
    theme.behaviour.companions == Companions::Lively
}

/// The colours one companion paints in, from the theme's tokens: every [`Ink`] but `Clear`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Palette {
    /// The kind's colour.
    pub body: Rgb,
    /// Its shade (pi's second colour, Dot's unlit dots).
    pub shade: Rgb,
    /// Its accent (pi's third colour).
    pub accent: Rgb,
    /// Feet, shut eyes, a mouth: `text` under Increase Contrast.
    pub outline: Rgb,
    /// The eyes: the content or the text, whichever reads more on the body.
    pub eye: Rgb,
    /// The light in an eye: the other of the two.
    pub glint: Rgb,
    /// What it holds.
    pub prop: Rgb,
    /// The state's tone.
    pub cue: Rgb,
    /// A finished turn's tone, for the hands raised as it hops.
    pub cheer: Rgb,
    /// A quiet mark, and a gone companion's outline.
    pub muted: Rgb,
}

impl Palette {
    /// `kind`'s colours in `theme`, its cue in the tone of `pose`.
    #[must_use]
    pub fn of(theme: &Theme, kind: Kind, pose: Pose) -> Self {
        let s = &theme.surfaces;
        let content = theme.content();
        let increased = theme.contrast == Contrast::Increased;
        let (dark, light) = if s.text.luminance() < content.luminance() {
            (s.text, content)
        } else {
            (content, s.text)
        };
        let ink = s.text_secondary;
        let unlit = s.brand.mix(content, 1.0 - theme.brand_unlit());
        let [coral, blue, gold] = slopty_theme::PI;
        let (body, shade, accent, outline) = match kind {
            Kind::Ember => (s.agent, s.agent.mix(dark, 0.3), s.agent.mix(light, 0.35), ink),
            Kind::Brace | Kind::Op | Kind::Blob(_) => {
                (ink, ink.mix(dark, 0.3), s.text, s.text_muted)
            }
            Kind::Pi => (coral, blue, gold, ink),
            Kind::Dot => (s.brand, unlit, s.brand, unlit),
        };
        let (eye, glint) = match kind {
            Kind::Dot => (s.brand, s.brand),
            _ if body.contrast(content) >= body.contrast(s.text) => (content, s.text),
            _ => (s.text, content),
        };
        let cue = match pose {
            Pose::NeedsYou => s.warn_fill,
            Pose::Failed => s.error_fill,
            Pose::Done | Pose::ToReview => s.accent_fill,
            _ => s.text_muted,
        };
        Self {
            body,
            shade,
            accent,
            outline: if increased { s.text } else { outline },
            eye,
            glint,
            prop: if increased { s.text_secondary } else { s.text_muted },
            cue,
            cheer: s.accent_fill,
            muted: if increased { s.text_secondary } else { s.text_muted },
        }
    }

    /// The colour of `ink`, none for a clear cell.
    #[must_use]
    pub const fn rgb(&self, ink: Ink) -> Option<Rgb> {
        Some(match ink {
            Ink::Clear | Ink::Erase => return None,
            Ink::Body => self.body,
            Ink::Shade => self.shade,
            Ink::Accent => self.accent,
            Ink::Outline => self.outline,
            Ink::Eye => self.eye,
            Ink::Glint => self.glint,
            Ink::Prop => self.prop,
            Ink::Cue => self.cue,
            Ink::Muted => self.muted,
        })
    }
}

/// A row's status slot ([`crate::palette::status_slot`]) with the agent's companion in it: the
/// agent named `agent` in the pose of `status`, in its mark's place and size, still unless it
/// works. With companions off, or no agent, it is the status slot as it was.
pub(crate) fn status_slot(
    theme: &Theme,
    agent: Option<&str>,
    kind: impl Into<Glyph>,
    status: Option<Status>,
    ink: Hsla,
    k: f32,
) -> Stateful<Div> {
    slot(theme, agent, kind, status).ink(ink).zoom(k).build()
}

/// A status slot that may hold a companion, built up: see [`Slot`].
pub(crate) fn slot<'a>(
    theme: &'a Theme,
    agent: Option<&'a str>,
    kind: impl Into<Glyph>,
    status: Option<Status>,
) -> Slot<'a> {
    Slot {
        theme,
        agent,
        kind: kind.into(),
        status,
        pose: status,
        moments: None,
        ink: hsla(theme.surfaces.text_muted),
        k: 1.0,
    }
}

/// A status slot: an agent's companion in the pose of its state where companions are on, else
/// the status mark or the kind's glyph. The slot keeps the status's name for a screen reader,
/// as the mark's does, and its size, so nothing moves with companions on or off.
pub(crate) struct Slot<'a> {
    theme: &'a Theme,
    agent: Option<&'a str>,
    kind: Glyph,
    status: Option<Status>,
    pose: Option<Status>,
    moments: Option<ElementId>,
    ink: Hsla,
    k: f32,
}

impl Slot<'_> {
    /// The glyph's ink, whose opacity a resting companion takes too.
    pub(crate) const fn ink(mut self, ink: Hsla) -> Self {
        self.ink = ink;
        self
    }

    /// At the chrome's zoom `k`.
    pub(crate) const fn zoom(mut self, k: f32) -> Self {
        self.k = k;
        self
    }

    /// The companion poses as `state`, where the slot's mark leaves a state to its words (a
    /// header with a needs-you band still has its companion wave).
    pub(crate) const fn posed(mut self, state: Option<Status>) -> Self {
        self.pose = state;
        self
    }

    /// Known as `id` across frames, so it plays its moments (lively): a wave into needs-you, a
    /// hop out of a finished turn. None plays nothing: a focused tile's finish was watched.
    pub(crate) fn moments(mut self, id: Option<ElementId>) -> Self {
        self.moments = id;
        self
    }

    pub(crate) fn build(self) -> Stateful<Div> {
        let theme = self.theme;
        let Some(agent) = self.agent.filter(|_| shown(theme)) else {
            return crate::palette::status_slot(theme, self.kind, self.status, self.ink, self.k);
        };
        let pose = Pose::of_status(self.pose);
        let fade = if self.pose.is_none() { self.ink.a } else { 1.0 };
        let mut mark = companion(theme, Kind::of(agent), pose)
            .side(px(theme.typography.icon() * self.k))
            .fade(fade);
        if let Some(id) = self.moments {
            mark = mark.one_shots(id);
        }
        div()
            .id("status")
            .flex_none()
            .size(px(theme.typography.icon_large() * self.k))
            .flex()
            .items_center()
            .justify_center()
            .when_some(self.status, |el, status| el.role(Role::Image).aria_label(status.label()))
            .child(mark)
    }
}

/// An agent's mark slot's companion: the agent named `agent` in the pose of `status`, `side`
/// square. None with companions off or no agent, and the mark keeps the slot.
pub(crate) fn mark(
    theme: &Theme,
    agent: Option<&str>,
    status: Option<Status>,
    side: Pixels,
) -> Option<AnyElement> {
    let agent = agent.filter(|_| shown(theme))?;
    Some(companion(theme, Kind::of(agent), Pose::of_status(status)).side(side).into_any_element())
}

/// The conversation's working line's mark, in the spinner's slot, `side` square.
///
/// The thread's companion at its task while the agent works on turn `turn`, its arm up (waving
/// as it begins, lively) while it waits on the person, and sat waiting while it stops. None with
/// companions off or before the thread's state is in, and the spinner keeps the slot.
pub fn at_work(
    theme: &Theme,
    state: Option<&ThreadState>,
    turn: TurnId,
    stopping: bool,
    side: Pixels,
) -> Option<AnyElement> {
    let state = state.filter(|_| shown(theme))?;
    let pose = if stopping {
        Pose::Waiting
    } else if state.status.phase == Phase::NeedsYou {
        Pose::NeedsYou
    } else {
        Pose::Working(Task::at(&state.items, turn))
    };
    Some(
        companion(theme, Kind::of(&state.meta.agent.0), pose)
            .one_shots("thread-working-companion")
            .side(side)
            .into_any_element(),
    )
}

/// The most subagents that trail a companion on the working line.
const TRAILING: usize = 3;

/// How large a trailing subagent is beside its parent: the same character, smaller.
const TRAILING_SCALE: f32 = 0.6;

/// A thread's subagents at work, trailing its companion on the working line.
///
/// A smaller one of its kind for each subagent call still running in turn `turn`, at most
/// three, each a few steps behind the last. Its parent's `side` sizes them. None with
/// companions off.
pub fn trailing(
    theme: &Theme,
    state: Option<&ThreadState>,
    turn: TurnId,
    side: Pixels,
) -> Vec<AnyElement> {
    let Some(state) = state.filter(|_| shown(theme)) else { return Vec::new() };
    let kind = Kind::of(&state.meta.agent.0);
    state
        .items
        .iter()
        .rev()
        .take_while(|item| item.turn == turn)
        .filter(|item| {
            matches!(&item.body, ItemBody::Tool(call)
                if call.kind == slopty_proto::thread::kind::AGENT
                    && matches!(call.state, ToolState::Streaming | ToolState::Running))
        })
        .take(TRAILING)
        .zip((1_u32..).map(|n| n.saturating_mul(3)))
        .map(|(_, phase)| {
            companion(theme, kind, Pose::Working(Task::Typing))
                .side(side * TRAILING_SCALE)
                .phase(phase)
                .into_any_element()
        })
        .collect()
}

/// How long the person goes without input before the yard's companions fall asleep: the
/// app's own measure of being away from the machine.
pub const AWAY_AFTER: Duration = Duration::from_secs(120);

/// A test's word on whether the person is away, in place of the machine's.
struct AssumedAway(bool);

impl Global for AssumedAway {}

/// Whether the person has been away from this machine past [`AWAY_AFTER`]: no key, click, move
/// or scroll in any app. An iPhone or iPad locks and resigns the app on its own.
pub(crate) fn away(cx: &App) -> bool {
    if let Some(assumed) = cx.try_global::<AssumedAway>() {
        return assumed.0;
    }
    #[cfg(target_os = "macos")]
    {
        slopty_platform::idle::since_input() >= AWAY_AFTER
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

/// Take the person as `away` (or not), whatever the machine says: for a test.
#[cfg(test)]
pub(crate) fn assume_away(cx: &mut App, away: bool) {
    cx.set_global(AssumedAway(away));
}
