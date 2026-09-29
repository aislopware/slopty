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
        AArrowDown,
        AArrowUp,
        Activity,
        AlignCenterHorizontal,
        AppWindow,
        ArrowDown,
        ArrowLeft,
        ArrowLeftToLine,
        ArrowRight,
        ArrowRightToLine,
        ArrowUp,
        ArrowUpDown,
        Bell,
        BellRing,
        BetweenHorizontalEnd,
        BetweenHorizontalStart,
        Bot,
        Brain,
        Cable,
        Cast,
        Check,
        ChevronDown,
        ChevronRight,
        ChevronUp,
        ChevronsLeft,
        ChevronsRight,
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
        CornerDownLeft,
        Cpu,
        Download,
        Ellipsis,
        Eraser,
        Expand,
        ExternalLink,
        File,
        FilePen,
        FilePlus,
        FileText,
        FoldHorizontal,
        Folder,
        FolderOpen,
        FolderSearch,
        GitBranch,
        Globe,
        Hand,
        Image,
        Inbox,
        Info,
        Keyboard,
        LayoutGrid,
        Link,
        ListChecks,
        ListFilter,
        ListTodo,
        Loader,
        LoaderCircle,
        Map,
        Maximize2,
        MessageSquare,
        MessageSquareWarning,
        Monitor,
        MousePointer2,
        MoveDown,
        MoveHorizontal,
        MoveLeft,
        MoveRight,
        MoveUp,
        MoveVertical,
        NotebookPen,
        PanelLeft,
        PanelsTopLeft,
        Paperclip,
        Pause,
        Pencil,
        Plug,
        Plus,
        Power,
        Regex,
        Replace,
        ReplaceAll,
        RotateCw,
        Save,
        Scissors,
        Search,
        Server,
        ServerOff,
        Settings,
        Square,
        SquareTerminal,
        StickyNote,
        Terminal,
        Type,
        Undo2,
        UnfoldHorizontal,
        UnfoldVertical,
        Unplug,
        Upload,
        Volume2,
        VolumeX,
        WholeWord,
        Wifi,
        WifiOff,
        Wrench,
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
    /// Busy on its own: an agent thinking or running a tool, a remote picture on its way.
    Working,
    /// A shell's command running for a while: busy, but nothing to watch for. The neutral
    /// tone and the calm mark, beside how long it has run.
    Running,
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
            Self::Running => IconName::Loader,
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
            Self::Running => s.text_secondary,
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
            Self::Running => "Running",
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

/// `status`'s icon, `side` square, in `color`. [`Status::Working`]'s turns ([`spin_step`]);
/// [`Status::Running`]'s steps once a second ([`calm_step`]).
#[must_use]
pub fn status_icon(theme: &Theme, status: Status, side: Pixels, color: Hsla) -> AnyElement {
    match status {
        Status::Working => Spinner { side, color, calm: false, inner: None }.into_any_element(),
        Status::Running => Spinner { side, color, calm: true, inner: None }.into_any_element(),
        _ => icon(theme, status.icon(), IconSize::Inline, color).size(side).into_any_element(),
    }
}

/// The step the calm mark shows `since` the spin clock started: one a second, a turn in
/// twelve, and always the first under Reduce Motion.
#[must_use]
pub fn calm_step(since: Duration, reduce_motion: bool) -> u32 {
    if reduce_motion {
        return 0;
    }
    u32::try_from(since.as_secs().checked_rem(u64::from(SPIN_STEPS)).unwrap_or(0)).unwrap_or(0)
}

/// How long after `since` the calm mark's next step begins: the next whole second.
#[must_use]
pub fn until_next_second(since: Duration) -> Duration {
    Duration::from_secs(1).saturating_sub(Duration::from_nanos(u64::from(since.subsec_nanos())))
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
///
/// While a typed key waits for its echo ([`hold_steps`]) a step that falls due wakes nobody: it
/// waits for the next frame drawn for another reason, the echo's ([`release_steps`]), or for the
/// hold's end, whichever comes first. A frame drawn for the step alone just before the echo
/// would hold the echo's frame back a whole refresh.
struct SpinClock {
    /// The executor's clock when the first mark was drawn; the test executor's is simulated.
    epoch: Instant,
    /// Reduce Motion, read from the system once, when the clock is made.
    reduce_motion: bool,
    /// The views that painted a turning mark since the last step.
    wake: Vec<EntityId>,
    /// The timer that is out, if one is, and its number: a timer whose number is not the
    /// latest was let go and does nothing when it fires.
    armed: Option<Timer>,
    timers: u64,
    /// Steps wake nobody until then: a key waits for its echo.
    hold_until: Option<Instant>,
    /// A step fell due during the hold and waits to be drawn.
    held: bool,
    /// The views that painted a calm mark since the last whole second. They are woken each
    /// second even under Reduce Motion, where the mark stands still: how long the thing has run
    /// is drawn beside it and must keep counting.
    calm_wake: Vec<EntityId>,
    /// The next second's timer is out.
    calm_armed: bool,
}

/// What an armed timer is for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Timer {
    /// The next step's start.
    Step,
    /// The end of a hold, with a step waiting.
    HoldEnd,
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
                armed: None,
                timers: 0,
                hold_until: None,
                held: false,
                calm_wake: Vec::new(),
                calm_armed: false,
            };
            cx.set_global(clock);
        }
        cx.global_mut::<Self>()
    }

    /// Set a timer of `kind` to fire after `wait`.
    fn arm(cx: &mut App, kind: Timer, wait: Duration) {
        let clock = Self::get(cx);
        clock.armed = Some(kind);
        clock.timers = clock.timers.wrapping_add(1);
        let number = clock.timers;
        let timer = cx.background_executor().timer(wait);
        cx.spawn(async move |cx| {
            timer.await;
            cx.update(|cx| {
                if Self::get(cx).timers == number {
                    Self::fire(cx, kind);
                }
            });
        })
        .detach();
    }

    /// A timer fired: a step fell due, or a hold with a step waiting ran out.
    fn fire(cx: &mut App, kind: Timer) {
        let now = cx.background_executor().now();
        let clock = Self::get(cx);
        clock.armed = None;
        match kind {
            Timer::Step => {
                if let Some(until) = clock.hold_until.filter(|until| now < *until) {
                    clock.held = true;
                    Self::arm(cx, Timer::HoldEnd, until.saturating_duration_since(now));
                    return;
                }
                clock.hold_until = None;
                Self::wake(cx);
            }
            Timer::HoldEnd => {
                clock.held = false;
                clock.hold_until = None;
                Self::wake(cx);
            }
        }
    }

    /// Set the calm lane's timer to fire after `wait`: the views that painted a calm mark draw
    /// again then, unless a key still waits for its echo, when they wait for the hold's end.
    fn arm_calm(cx: &mut App, wait: Duration) {
        Self::get(cx).calm_armed = true;
        let timer = cx.background_executor().timer(wait);
        cx.spawn(async move |cx| {
            timer.await;
            cx.update(|cx| {
                let now = cx.background_executor().now();
                let clock = Self::get(cx);
                if let Some(until) = clock.hold_until.filter(|until| now < *until) {
                    Self::arm_calm(cx, until.saturating_duration_since(now));
                    return;
                }
                clock.calm_armed = false;
                for view in std::mem::take(&mut clock.calm_wake) {
                    cx.notify(view);
                }
            });
        })
        .detach();
    }

    /// Wake every view that painted a mark since the last step.
    fn wake(cx: &mut App) {
        for view in std::mem::take(&mut Self::get(cx).wake) {
            cx.notify(view);
        }
    }
}

/// A typed key waits for its echo, at most until `until`: steps of the working mark that fall
/// due meanwhile wait for the echo's frame (or `until`), rather than draw a frame of their own.
pub fn hold_steps(cx: &mut App, until: Instant) {
    let clock = SpinClock::get(cx);
    clock.hold_until = Some(clock.hold_until.map_or(until, |held| held.max(until)));
}

/// Until when the working marks' steps are held for a typed key's echo, if they are.
#[cfg(test)]
pub(crate) fn steps_held_until(cx: &mut App) -> Option<Instant> {
    SpinClock::get(cx).hold_until
}

/// A frame is coming anyway (a terminal changed): a step that waited on the hold rides it.
pub fn release_steps(cx: &mut App) {
    let clock = SpinClock::get(cx);
    if clock.hold_until.take().is_some() && std::mem::take(&mut clock.held) {
        // The hold's timer is let go; the marks that paint in this frame set the next step's.
        clock.armed = None;
        clock.timers = clock.timers.wrapping_add(1);
        SpinClock::wake(cx);
    }
}

/// Whether the system asks for motion to be reduced. Always false under test, so a test that
/// counts frames does not depend on the machine's setting; `App::set_reduce_motion` is how a
/// test asks for it.
fn system_reduce_motion() -> bool {
    !cfg!(test) && slopty_platform::reduce_motion()
}

/// The working mark: [`Status::Working`]'s icon, turned to the spin clock's step when laid out;
/// calm, [`Status::Running`]'s, turned a step a second.
struct Spinner {
    side: Pixels,
    color: Hsla,
    /// [`Status::Running`]'s mark: its icon, a step a second.
    calm: bool,
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
        let still = reduce || clock.reduce_motion;
        let (step, status) = if self.calm {
            (calm_step(since, still), Status::Running)
        } else {
            (spin_step(since, still), Status::Working)
        };
        #[expect(clippy::cast_precision_loss, reason = "a step under twelve")]
        let turn = step as f32 / SPIN_STEPS as f32;
        let mut inner = svg()
            .path(status.icon().path())
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
        if self.calm {
            if !clock.calm_wake.contains(&view) {
                clock.calm_wake.push(view);
            }
            if !clock.calm_armed {
                let wait = until_next_second(now.saturating_duration_since(clock.epoch));
                SpinClock::arm_calm(cx, wait);
            }
            return;
        }
        if reduce || clock.reduce_motion {
            return;
        }
        if !clock.wake.contains(&view) {
            clock.wake.push(view);
        }
        if clock.armed.is_some() {
            return;
        }
        let wait = until_next_step(now.saturating_duration_since(clock.epoch));
        SpinClock::arm(cx, Timer::Step, wait);
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
            Status::Running,
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
        status: Status,
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
            div().children(self.shown.then(|| status_mark(&theme, Some(self.status), 1.0)))
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
        let (view, cx) =
            cx.add_window_view(|_, _| Turning { shown: true, status: Status::Working, renders: 0 });
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

    /// The calm mark wakes its view once a second, a twelfth as often as the working mark, and
    /// keeps doing so under Reduce Motion, where it stands still but what it times goes on.
    #[gpui::test]
    fn a_running_mark_steps_once_a_second_even_under_reduce_motion(cx: &mut gpui::TestAppContext) {
        let (view, cx) =
            cx.add_window_view(|_, _| Turning { shown: true, status: Status::Running, renders: 0 });
        cx.run_until_parked();
        let shown = renders_over(&view, cx, Duration::from_secs(3));
        assert!((2..=4).contains(&shown), "{shown} renders in three seconds");
        cx.update(|_w, cx| cx.set_reduce_motion(true));
        let still = renders_over(&view, cx, Duration::from_secs(3));
        assert!((2..=4).contains(&still), "{still} renders in three seconds, standing still");
        assert_eq!(calm_step(Duration::from_millis(2_500), false), 2);
        assert_eq!(calm_step(Duration::from_secs(13), false), 1, "a turn in twelve seconds");
        assert_eq!(calm_step(Duration::from_secs(5), true), 0);
        assert_eq!(until_next_second(Duration::from_millis(2_300)), Duration::from_millis(700));
    }

    /// Renders of `view` while the executor's clock runs `for`, a refresh at a time.
    fn renders_over(
        view: &gpui::Entity<Turning>,
        cx: &gpui::VisualTestContext,
        span: Duration,
    ) -> usize {
        let before = view.read_with(cx, |v, _| v.renders);
        let refresh = Duration::from_nanos(8_333_333);
        let mut ran = Duration::ZERO;
        while ran < span {
            cx.executor().advance_clock(refresh);
            cx.run_until_parked();
            ran = ran.saturating_add(refresh);
        }
        view.read_with(cx, |v, _| v.renders).saturating_sub(before)
    }

    /// While a key waits for its echo, a step that falls due wakes nobody: it waits for the
    /// echo's frame, or for the hold's end, and then the steps go on as before.
    #[gpui::test]
    fn a_held_step_waits_for_the_echo_or_the_end_of_the_hold(cx: &mut gpui::TestAppContext) {
        let (view, cx) =
            cx.add_window_view(|_, _| Turning { shown: true, status: Status::Working, renders: 0 });
        cx.run_until_parked();
        let now =
            |cx: &mut gpui::VisualTestContext| cx.update(|_w, cx| cx.background_executor().now());
        let hold = SPIN_STEP.saturating_mul(4);
        let until = now(cx).checked_add(hold).expect("a hold in range");
        cx.update(|_w, cx| hold_steps(cx, until));
        assert_eq!(renders_over(&view, cx, SPIN_STEP.saturating_mul(2)), 0, "held");
        cx.update(|_w, cx| release_steps(cx));
        cx.run_until_parked();
        assert_eq!(view.read_with(cx, |v, _| v.renders), 2, "the echo's frame carries it");
        let turning = renders_over(&view, cx, Duration::from_secs(1));
        assert!((11..=13).contains(&turning), "{turning} steps after the release");

        let until = now(cx).checked_add(hold).expect("a hold in range");
        cx.update(|_w, cx| hold_steps(cx, until));
        let held = renders_over(&view, cx, hold.saturating_sub(Duration::from_millis(10)));
        assert_eq!(held, 0, "no echo came: nothing before the hold ends");
        let after = renders_over(&view, cx, SPIN_STEP);
        assert!((1..=2).contains(&after), "the hold's end draws the step: {after}");
        cx.update(|_w, cx| release_steps(cx));
        cx.run_until_parked();
        let turning = renders_over(&view, cx, Duration::from_secs(1));
        assert!((11..=13).contains(&turning), "{turning} steps once it ran out");
    }

    #[test]
    fn an_unknown_path_loads_nothing() {
        assert!(!matches!(Assets.load("icons/not-an-icon.svg"), Ok(Some(_))));
    }
}
