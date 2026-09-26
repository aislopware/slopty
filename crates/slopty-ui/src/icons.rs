//! The icons chrome draws: Lucide (ISC), from gpui-kit's asset crate.
//!
//! Only the icons this module lists are embedded; gpui-kit's own component bundle backs
//! them so its inputs and menus keep theirs. An icon takes its size from the type scale
//! ([`slopty_theme::Typography::icon`]) and its colour from the text beside it, so it never
//! outweighs the words it marks.

use std::borrow::Cow;
use std::time::{Duration, Instant};

use gpui::accesskit::Role;
use gpui::{
    AnyElement, App, AssetSource, Bounds, Div, Element, ElementId, EntityId, Global,
    GlobalElementId, Hsla, InspectorElementId, InteractiveElement as _, IntoElement, LayoutId,
    ParentElement as _, Pixels, SharedString, Stateful, StatefulInteractiveElement as _,
    Styled as _, Svg, Transformation, Window, div, px, radians, svg,
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
        ServerOff,
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
            Self::Idle => s.text_muted,
            Self::Working => s.accent,
            Self::NeedsYou | Self::Away => s.warn,
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

/// `status` in a fixed square slot, the width of a large icon at the chrome's zoom `k`.
///
/// Rows that carry a mark and rows that do not keep their titles on one edge. A mark is an
/// image named by [`Status::label`]; an empty slot is nothing to a screen reader.
#[must_use]
pub fn status_mark(theme: &Theme, status: Option<Status>, k: f32) -> Stateful<Div> {
    let slot = div()
        .id("status")
        .flex_shrink_0()
        .size(px(theme.typography.icon_large() * k))
        .flex()
        .items_center()
        .justify_center();
    match status {
        Some(status) => slot.role(Role::Image).aria_label(status.label()).child(status_icon(
            theme,
            status,
            px(theme.typography.icon() * k),
            hsla(status.tone(theme)),
        )),
        None => slot,
    }
}

/// `status`'s icon, `side` square, in `color`. [`Status::Working`]'s turns ([`spin_step`]).
#[must_use]
pub fn status_icon(theme: &Theme, status: Status, side: Pixels, color: Hsla) -> AnyElement {
    if status == Status::Working {
        return Spinner { side, color, inner: None }.into_any_element();
    }
    icon(theme, status.icon(), IconSize::Inline, color).size(side).into_any_element()
}

/// The steps in one turn of the working mark, which makes one turn a second.
pub const SPIN_STEPS: u32 = 12;

/// How long the working mark holds each step: a twelfth of a second. It steps rather than
/// glides, so a window with an agent at work draws twelve frames a second for it, not one on
/// every display refresh.
pub const SPIN_STEP: Duration = Duration::from_nanos(83_333_333);

/// The step of its turn the working mark shows `since` the spin clock started: always the
/// first under Reduce Motion, where it stands still.
#[must_use]
pub fn spin_step(since: Duration, reduce_motion: bool) -> u32 {
    if reduce_motion {
        return 0;
    }
    let steps = since.as_nanos().checked_div(SPIN_STEP.as_nanos()).unwrap_or(0);
    u32::try_from(steps.checked_rem(u128::from(SPIN_STEPS)).unwrap_or(0)).unwrap_or(0)
}

/// How long after `since` the next step begins.
#[must_use]
pub fn until_next_step(since: Duration) -> Duration {
    let into = since.as_nanos().checked_rem(SPIN_STEP.as_nanos()).unwrap_or(0);
    SPIN_STEP.saturating_sub(Duration::from_nanos(u64::try_from(into).unwrap_or(0)))
}

/// The clock every working mark turns by, one per app, so marks on screen together show the
/// same step.
///
/// A mark that paints names its view here, and the first to do so since the last step sets a
/// timer for the next one. When it fires the named views are notified and draw again, and those
/// still showing a mark name themselves anew. A view that stops painting one, because the work
/// ended or it scrolled away, is not woken again, and nothing turns while nothing shows.
struct SpinClock {
    /// The executor's clock when the first mark was drawn; the test executor's is simulated.
    epoch: Instant,
    /// Reduce Motion, read from the system once, when the clock is made.
    reduce_motion: bool,
    /// The views that painted a turning mark since the last step.
    wake: Vec<EntityId>,
    /// A timer is out for the next step.
    armed: bool,
}

impl Global for SpinClock {}

impl SpinClock {
    /// The app's clock, made on first use.
    fn get(cx: &mut App) -> &mut Self {
        if !cx.has_global::<Self>() {
            let clock = Self {
                epoch: cx.background_executor().now(),
                reduce_motion: system_reduce_motion(),
                wake: Vec::new(),
                armed: false,
            };
            cx.set_global(clock);
        }
        cx.global_mut::<Self>()
    }
}

/// Whether the system asks for motion to be reduced. Always false under test, so a test that
/// counts frames does not depend on the machine's setting; `App::set_reduce_motion` is how a
/// test asks for it.
fn system_reduce_motion() -> bool {
    !cfg!(test) && slopty_platform::reduce_motion()
}

/// The working mark: [`Status::Working`]'s icon, turned to the spin clock's step when laid out.
struct Spinner {
    side: Pixels,
    color: Hsla,
    inner: Option<AnyElement>,
}

impl IntoElement for Spinner {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for Spinner {
    type PrepaintState = ();
    type RequestLayoutState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let reduce = cx.reduce_motion();
        let now = cx.background_executor().now();
        let clock = SpinClock::get(cx);
        let since = now.saturating_duration_since(clock.epoch);
        let step = spin_step(since, reduce || clock.reduce_motion);
        #[expect(clippy::cast_precision_loss, reason = "a step under twelve")]
        let turn = step as f32 / SPIN_STEPS as f32;
        let mut inner = svg()
            .path(Status::Working.icon().path())
            .flex_shrink_0()
            .size(self.side)
            .text_color(self.color)
            .with_transformation(Transformation::rotate(radians(turn * std::f32::consts::TAU)))
            .into_any_element();
        let layout = inner.request_layout(window, cx);
        self.inner = Some(inner);
        (layout, ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        if let Some(inner) = &mut self.inner {
            inner.prepaint(window, cx);
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        if let Some(inner) = &mut self.inner {
            inner.paint(window, cx);
        }
        let reduce = cx.reduce_motion();
        let now = cx.background_executor().now();
        let view = window.current_view();
        let clock = SpinClock::get(cx);
        if reduce || clock.reduce_motion {
            return;
        }
        if !clock.wake.contains(&view) {
            clock.wake.push(view);
        }
        if clock.armed {
            return;
        }
        clock.armed = true;
        let wait = until_next_step(now.saturating_duration_since(clock.epoch));
        let timer = cx.background_executor().timer(wait);
        cx.spawn(async move |cx| {
            timer.await;
            cx.update(|cx| {
                let clock = SpinClock::get(cx);
                clock.armed = false;
                for view in std::mem::take(&mut clock.wake) {
                    cx.notify(view);
                }
            });
        })
        .detach();
    }
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

    /// Twelve steps make one turn a second, each held a twelfth of a second, and the timer
    /// always waits for the next step's start. Under Reduce Motion the mark stands on its
    /// first step whatever the time.
    #[test]
    fn the_working_mark_steps_twelve_times_a_turn_and_stands_under_reduce_motion() {
        let ms = Duration::from_millis;
        assert_eq!(spin_step(ms(0), false), 0);
        assert_eq!(spin_step(ms(80), false), 0, "still the first step");
        assert_eq!(spin_step(ms(90), false), 1);
        assert_eq!(spin_step(ms(990), false), 11);
        assert_eq!(spin_step(ms(1_000), false), 0, "one turn a second");
        assert_eq!(spin_step(ms(2_500), false), 6);
        for at in [0, 90, 500, 990, 12_345] {
            assert_eq!(spin_step(ms(at), true), 0, "Reduce Motion at {at} ms");
        }
        assert_eq!(until_next_step(ms(0)), SPIN_STEP, "on a step's start, a whole step away");
        assert_eq!(until_next_step(ms(80)), SPIN_STEP.saturating_sub(ms(80)));
        let next = ms(90).saturating_add(until_next_step(ms(90)));
        assert_eq!(spin_step(next, false), 2, "the timer lands on the next step");
        let turn = SPIN_STEP.checked_mul(SPIN_STEPS).unwrap_or_default();
        assert!(ms(1_000).saturating_sub(turn) < Duration::from_micros(1), "{turn:?}");
    }

    /// A mark is an image named by its status at any zoom; an empty slot is nothing to a
    /// screen reader, and takes the same room.
    #[gpui::test]
    fn a_status_mark_is_an_image_named_by_its_status(cx: &mut gpui::TestAppContext) {
        struct Marks;
        impl gpui::Render for Marks {
            fn render(
                &mut self,
                _window: &mut Window,
                _cx: &mut gpui::Context<Self>,
            ) -> impl IntoElement {
                let theme = Theme::default();
                div()
                    .id("marks")
                    .child(div().id("a").child(status_mark(&theme, Some(Status::NeedsYou), 1.0)))
                    .child(div().id("b").child(status_mark(&theme, Some(Status::Working), 0.5)))
                    .child(div().id("c").child(status_mark(&theme, None, 1.0)))
            }
        }
        let (view, cx) = cx.add_window_view(|_, _| Marks);
        cx.update(|window, _cx| window.set_a11y_active(true));
        view.update(cx, |_, cx| cx.notify());
        cx.run_until_parked();
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        assert!(tree.iter().any(|n| n.is("Image", Some("Needs you"))), "{tree:#?}");
        assert!(tree.iter().any(|n| n.is("Image", Some("Working"))), "{tree:#?}");
        assert_eq!(tree.iter().filter(|n| n.role == "Image").count(), 2, "{tree:#?}");
    }

    /// A view that shows a working mark or not, and counts its renders.
    struct Turning {
        shown: bool,
        renders: usize,
    }

    impl gpui::Render for Turning {
        fn render(
            &mut self,
            _window: &mut Window,
            _cx: &mut gpui::Context<Self>,
        ) -> impl IntoElement {
            self.renders = self.renders.saturating_add(1);
            let theme = Theme::default();
            div().children(self.shown.then(|| status_mark(&theme, Some(Status::Working), 1.0)))
        }
    }

    /// Renders of `view` over one second of the executor's clock.
    fn renders_in_a_second(view: &gpui::Entity<Turning>, cx: &gpui::VisualTestContext) -> usize {
        let before = view.read_with(cx, |v, _| v.renders);
        for _ in 0..120 {
            cx.executor().advance_clock(Duration::from_nanos(8_333_333));
            cx.run_until_parked();
        }
        view.read_with(cx, |v, _| v.renders).saturating_sub(before)
    }

    /// The mark wakes the view it was painted in once a step while it shows, and not once it
    /// is gone; under Reduce Motion it never wakes it.
    #[gpui::test]
    fn a_working_mark_wakes_its_view_only_while_it_shows(cx: &mut gpui::TestAppContext) {
        let (view, cx) = cx.add_window_view(|_, _| Turning { shown: true, renders: 0 });
        cx.run_until_parked();
        let shown = renders_in_a_second(&view, cx);
        assert!((11..=13).contains(&shown), "{shown} renders in a second");
        view.update(cx, |v, cx| {
            v.shown = false;
            cx.notify();
        });
        cx.run_until_parked();
        assert!(renders_in_a_second(&view, cx) <= 1, "the step already due at most");
        assert_eq!(renders_in_a_second(&view, cx), 0, "gone, it wakes nothing");
        cx.update(|_w, cx| cx.set_reduce_motion(true));
        view.update(cx, |v, cx| {
            v.shown = true;
            cx.notify();
        });
        cx.run_until_parked();
        assert_eq!(renders_in_a_second(&view, cx), 0, "Reduce Motion: it stands still");
    }

    #[test]
    fn an_unknown_path_loads_nothing() {
        assert!(!matches!(Assets.load("icons/not-an-icon.svg"), Ok(Some(_))));
    }
}
