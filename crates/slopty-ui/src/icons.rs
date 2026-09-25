//! The icons chrome draws: Lucide (ISC), from gpui-kit's asset crate.
//!
//! Only the icons this module lists are embedded; gpui-kit's own component bundle backs
//! them so its inputs and menus keep theirs. An icon takes its size from the type scale
//! ([`slopty_theme::Typography::icon`]) and its colour from the text beside it, so it never
//! outweighs the words it marks.

use std::borrow::Cow;

use gpui::{
    AnyElement, AssetSource, Hsla, IntoElement as _, ParentElement as _, SharedString, Styled as _,
    Svg, div, px, svg,
};
pub use gpui_kit::assets::IconName;
use slopty_proto::agent::{AgentEvent, AgentStatus, BlockReason};
use slopty_theme::{Rgb, Theme};

use crate::colors::hsla;

gpui_kit::assets::icon_assets!(
    Chosen,
    [
        Activity,
        AppWindow,
        ArrowDown,
        ArrowLeft,
        ArrowRight,
        ArrowUp,
        ArrowUpDown,
        Bell,
        Bot,
        Cable,
        Cast,
        Check,
        ChevronDown,
        ChevronRight,
        ChevronUp,
        Circle,
        CircleAlert,
        CircleCheck,
        CircleDot,
        CirclePause,
        CircleX,
        Clipboard,
        Clock,
        Columns2,
        Command,
        Copy,
        Cpu,
        Download,
        Ellipsis,
        Eraser,
        File,
        FileText,
        Folder,
        FolderOpen,
        GitBranch,
        Globe,
        Hand,
        Inbox,
        Info,
        Keyboard,
        LayoutGrid,
        Link,
        ListFilter,
        LoaderCircle,
        Maximize2,
        MessageSquareWarning,
        Monitor,
        MousePointer2,
        MoveHorizontal,
        MoveVertical,
        PanelLeft,
        Pause,
        Pencil,
        Plus,
        RotateCw,
        Save,
        Search,
        Server,
        Settings,
        SquareTerminal,
        StickyNote,
        Terminal,
        Type,
        Undo2,
        Unplug,
        Upload,
        Volume2,
        VolumeX,
        Wifi,
        WifiOff,
        X,
    ]
);

/// The asset source every window registers: the chosen icons, then gpui-kit's bundle.
#[derive(Clone, Copy, Debug, Default)]
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        match Chosen.load(path)? {
            Some(bytes) => Ok(Some(bytes)),
            None => gpui_kit::assets::Assets.load(path),
        }
    }

    fn list(&self, path: &str) -> gpui::Result<Vec<SharedString>> {
        let mut paths = Chosen.list(path)?;
        paths.extend(gpui_kit::assets::Assets.list(path)?);
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}

/// How large an icon is drawn.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum IconSize {
    /// Beside chrome text: [`slopty_theme::Typography::icon`].
    Inline,
    /// Standing alone: [`slopty_theme::Typography::icon_large`].
    Large,
}

/// `name` at `size`, in `color`.
#[must_use]
pub fn icon(theme: &Theme, name: IconName, size: IconSize, color: Hsla) -> Svg {
    let side = match size {
        IconSize::Inline => theme.typography.icon(),
        IconSize::Large => theme.typography.icon_large(),
    };
    svg().path(name.path()).flex_shrink_0().size(px(side)).text_color(color)
}

/// The one vocabulary for how a thing is doing, wherever it is shown: a tile's header, a
/// navigator row, the palette, a toast. Each state has one icon and one tone, so a glance
/// reads the same everywhere.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    /// Nothing to say: a shell at its prompt, an agent at rest.
    Idle,
    /// Busy on its own: an agent thinking or running a tool, a command running.
    Working,
    /// Waiting on the human: a permission, a question, an elicitation.
    NeedsYou,
    /// Finished and not yet looked at.
    Done,
    /// Failed: a command's non-zero exit, a session that ended in error.
    Failed,
    /// Out of reach: a worker away, a session reconnecting.
    Away,
}

impl Status {
    /// What an agent's report shows as; `None` when there is no agent to speak of.
    #[must_use]
    pub const fn of_agent(agent: &AgentEvent) -> Option<Self> {
        Some(match &agent.status {
            AgentStatus::None => return None,
            AgentStatus::Idle | AgentStatus::Blocked(BlockReason::IdlePrompt) => Self::Idle,
            AgentStatus::Working | AgentStatus::Tool { .. } => Self::Working,
            AgentStatus::Blocked(_) => Self::NeedsYou,
            AgentStatus::Done => Self::Done,
        })
    }

    /// The icon that marks it.
    #[must_use]
    pub const fn icon(self) -> IconName {
        match self {
            Self::Idle => IconName::Circle,
            Self::Working => IconName::LoaderCircle,
            Self::NeedsYou => IconName::CircleAlert,
            Self::Done => IconName::CircleCheck,
            Self::Failed => IconName::CircleX,
            Self::Away => IconName::Unplug,
        }
    }

    /// Its tone.
    #[must_use]
    pub const fn tone(self, theme: &Theme) -> Rgb {
        let s = &theme.surfaces;
        match self {
            Self::Idle | Self::Away => s.text_muted,
            Self::Working => s.accent,
            Self::NeedsYou => s.warn,
            Self::Done => s.success,
            Self::Failed => s.error,
        }
    }

    /// Its name for the accessibility tree and tooltips.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Idle => "Idle",
            Self::Working => "Working",
            Self::NeedsYou => "Needs you",
            Self::Done => "Done",
            Self::Failed => "Failed",
            Self::Away => "Away",
        }
    }
}

/// `status` in a fixed square slot, the width of a large icon, so rows that carry a mark and
/// rows that do not keep their titles on one edge.
#[must_use]
pub fn status_mark(theme: &Theme, status: Option<Status>) -> AnyElement {
    let side = px(theme.typography.icon_large());
    div()
        .flex_shrink_0()
        .size(side)
        .flex()
        .items_center()
        .justify_center()
        .children(status.map(|s| icon(theme, s.icon(), IconSize::Inline, hsla(s.tone(theme)))))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_chosen_icon_loads_and_the_component_bundle_still_does() {
        for name in Chosen.list("").unwrap_or_default() {
            let bytes = Assets.load(&name).ok().flatten();
            assert!(bytes.is_some_and(|b| b.starts_with(b"<svg")), "{name} did not load");
        }
        assert!(Assets.load(&IconName::SquareTerminal.path()).ok().flatten().is_some());
        let component = gpui_kit::assets::Assets.list("").unwrap_or_default();
        let first = component.first().map(SharedString::to_string).unwrap_or_default();
        assert!(Assets.load(&first).ok().flatten().is_some(), "component icon {first} lost");
    }

    #[test]
    fn each_status_has_its_own_icon_and_every_icon_is_embedded() {
        let all = [
            Status::Idle,
            Status::Working,
            Status::NeedsYou,
            Status::Done,
            Status::Failed,
            Status::Away,
        ];
        let icons: std::collections::HashSet<_> = all.iter().map(|s| s.icon()).collect();
        assert_eq!(icons.len(), all.len());
        for s in all {
            assert!(Chosen.load(&s.icon().path()).ok().flatten().is_some(), "{s:?}");
        }
    }

    #[test]
    fn an_unknown_path_loads_nothing() {
        assert!(!matches!(Assets.load("icons/not-an-icon.svg"), Ok(Some(_))));
    }
}
