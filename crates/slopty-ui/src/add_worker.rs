//! A worker put on a machine over SSH, step by step.
//!
//! The add-worker sheet and the tile of a worker on a different build draw it the same way: one
//! line per step with its mark, a thin bar under the list while a step runs, and a failure's
//! words over the last lines the machine printed.
//!
//! The app runs the deploy (`slopty_deploy`) and turns what it says into an [`Install`]; this
//! only draws one. The tiles learn of the app's way to update a worker, and of each update
//! under way, through the [`Updates`] global.

use std::collections::HashMap;
use std::rc::Rc;

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, Div, InteractiveElement as _, IntoElement as _, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, px,
};
use slopty_theme::{Theme, Typography};

use crate::colors::hsla;
use crate::icons::{Status, status_mark};
use crate::kit;

/// The tile's button that updates a worker on a different build.
pub const UPDATE: &str = "Update";
/// The tile's button after an update failed.
pub const TRY_AGAIN: &str = "Try again";

/// How a step stands.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mark {
    /// Not reached yet.
    Waiting,
    /// Under way.
    Running,
    /// Done.
    Done,
    /// Where it stopped.
    Failed,
}

impl Mark {
    /// The status mark it wears: a quiet ring until it runs, the calm spinner while it does.
    const fn status(self) -> Status {
        match self {
            Self::Waiting => Status::Idle,
            Self::Running => Status::Running,
            Self::Done => Status::Done,
            Self::Failed => Status::Failed,
        }
    }
}

/// One step's line.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct StepLine {
    /// Its id's last part (`install-step-<slug>`).
    pub slug: &'static str,
    /// What it does, sentence case.
    pub title: String,
    /// What it is at: the bytes sent, the line the machine printed last, what it found.
    pub detail: Option<String>,
    /// How it stands.
    pub mark: Mark,
}

/// The bar under the steps.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Bar {
    /// Nothing runs.
    Hidden,
    /// This share of the step is done, from 0 to 1.
    Share(f32),
    /// A step whose length nobody can tell.
    Busy,
}

/// Why it stopped, as a person reads it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Failed {
    /// One sentence, no full stop.
    pub title: String,
    /// What to do about it, when that is known.
    pub hint: Option<String>,
    /// The last lines the machine printed, oldest first.
    pub lines: Vec<String>,
}

/// An install or an update, as drawn.
#[derive(Clone, PartialEq, Debug)]
pub struct Install {
    /// Every step, in order.
    pub steps: Vec<StepLine>,
    /// The bar under them.
    pub bar: Bar,
    /// Why it stopped, once it has.
    pub failed: Option<Failed>,
}

impl Install {
    /// The step under way, or the one it stopped at.
    #[must_use]
    pub fn current(&self) -> Option<&StepLine> {
        self.steps.iter().find(|s| matches!(s.mark, Mark::Running | Mark::Failed))
    }
}

/// Runs the update of the worker at a host (as it was dialled): the app's.
pub type Update = Rc<dyn Fn(&str, &mut Window, &mut App)>;

/// What the tiles of a worker on a different build offer: the app's way to update it, where
/// there is one, and each update under way or failed, by host.
#[derive(Default)]
pub struct Updates {
    /// The app's update; `None` where it cannot run `ssh` (iOS, the self-test).
    pub start: Option<Update>,
    /// Each update under way or failed, by the host it runs against.
    pub runs: HashMap<String, Install>,
}

impl gpui::Global for Updates {}

impl std::fmt::Debug for Updates {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Updates")
            .field("start", &self.start.is_some())
            .field("runs", &self.runs)
            .finish()
    }
}

/// The steps, one line each, in one card ([`kit::card`]): a step over the page in dark, white
/// with its hairline in light.
#[must_use]
pub fn steps(theme: &Theme, install: &Install) -> gpui::Stateful<Div> {
    let lines = install.steps.iter().map(|line| step_line(theme, line));
    kit::card(theme)
        .id("install-steps")
        .debug_selector(|| "install-steps".to_owned())
        .role(Role::List)
        .aria_label("Install steps")
        .flex()
        .flex_col()
        .p(px(theme.spacing.xxs))
        .children(lines)
}

/// One step: its mark, its title over what it is at.
fn step_line(theme: &Theme, line: &StepLine) -> gpui::Stateful<Div> {
    let s = theme.surfaces;
    let slug = line.slug;
    let waiting = line.mark == Mark::Waiting;
    let status = line.mark.status();
    kit::inset_x(div(), theme)
        .id(gpui::ElementId::Name(format!("install-step-{slug}").into()))
        .debug_selector(move || format!("install-step-{slug}"))
        .role(Role::ListItem)
        .aria_label(SharedString::from(line.title.clone()))
        .when_some(line.detail.clone(), |el, detail| {
            el.aria_description(SharedString::from(detail))
        })
        .flex()
        .items_center()
        .gap(px(theme.spacing.sm))
        .min_h(px(kit::Row::One.height(theme)))
        .py(px(theme.spacing.xxs))
        .child(status_mark(theme, Some(status), 1.0))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .items_baseline()
                .gap(px(theme.spacing.sm))
                .child(
                    div()
                        .flex_none()
                        .text_size(px(theme.typography.ui_size))
                        .font_weight(gpui::FontWeight(Typography::MEDIUM_WEIGHT))
                        .text_color(hsla(if waiting { s.text_muted } else { s.text }))
                        .child(SharedString::from(line.title.clone())),
                )
                .when_some(line.detail.clone(), |el, detail| {
                    el.child(
                        kit::meta(div(), theme)
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(SharedString::from(detail)),
                    )
                }),
        )
}

/// The bar under the steps; `None` when nothing runs.
///
/// It is the kit's bar ([`kit::progress::Bar`]), filled to its share, or breathing for a step
/// nobody can time. The bar reads Reduce Motion itself.
#[must_use]
pub fn bar(theme: &Theme, bar: Bar, id: &'static str) -> Option<gpui::AnyElement> {
    let progress = match bar {
        Bar::Hidden => return None,
        Bar::Share(done) => kit::progress::Progress::Share(done),
        Bar::Busy => kit::progress::Progress::Busy,
    };
    Some(
        div()
            .debug_selector(move || id.to_owned())
            .w_full()
            .child(kit::progress::Bar::new(theme, id, progress).label("Installing"))
            .into_any_element(),
    )
}

/// Why it stopped: the title in the failure's tone, what to do, and the machine's last lines
/// in a quiet well, wrapped rather than cut so a path or a reason reads whole.
#[must_use]
pub fn failure(theme: &Theme, failed: &Failed) -> gpui::Stateful<Div> {
    let s = theme.surfaces;
    let lines = (!failed.lines.is_empty()).then(|| {
        div()
            .id("install-output")
            .debug_selector(|| "install-output".to_owned())
            .flex()
            .flex_col()
            .p(px(theme.spacing.sm))
            .rounded(px(theme.radii.sm))
            .map(|el| kit::inset(el, theme))
            .children(
                failed
                    .lines
                    .iter()
                    .map(|line| kit::meta(div(), theme).child(SharedString::from(line.clone()))),
            )
    });
    div()
        .id("install-failure")
        .debug_selector(|| "install-failure".to_owned())
        .role(Role::Alert)
        .aria_label(SharedString::from(failed.title.clone()))
        .flex()
        .flex_col()
        .gap(px(theme.spacing.xs))
        .child(
            div()
                .text_size(px(theme.typography.ui_size))
                .font_weight(gpui::FontWeight(Typography::MEDIUM_WEIGHT))
                .text_color(hsla(s.error))
                .child(SharedString::from(failed.title.clone())),
        )
        .when_some(failed.hint.clone(), |el, hint| {
            el.child(
                div()
                    .text_size(px(theme.typography.small()))
                    .text_color(hsla(s.text_secondary))
                    .child(SharedString::from(hint)),
            )
        })
        .children(lines)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn line(slug: &'static str, mark: Mark) -> StepLine {
        StepLine { slug, title: slug.to_owned(), detail: None, mark }
    }

    #[test]
    fn the_current_step_is_the_running_or_the_failed_one() {
        let mut install = Install {
            steps: vec![line("reach", Mark::Done), line("copy", Mark::Running)],
            bar: Bar::Busy,
            failed: None,
        };
        assert_eq!(install.current().map(|s| s.slug), Some("copy"));
        install.steps = vec![line("reach", Mark::Failed), line("copy", Mark::Waiting)];
        assert_eq!(install.current().map(|s| s.slug), Some("reach"));
        install.steps = vec![line("reach", Mark::Done)];
        assert_eq!(install.current(), None);
    }

    /// A busy install bar, counting its frames.
    struct BusyBar {
        theme: Theme,
        renders: u32,
    }

    impl gpui::Render for BusyBar {
        fn render(
            &mut self,
            _window: &mut Window,
            _cx: &mut gpui::Context<Self>,
        ) -> impl gpui::IntoElement {
            self.renders = self.renders.saturating_add(1);
            div().w(px(300.0)).children(bar(&self.theme, Bar::Busy, "busy-bar"))
        }
    }

    /// The frames a busy bar draws in a second of 120 Hz frames once it shows, with Reduce Motion
    /// `reduced`.
    fn busy_frames(cx: &mut gpui::TestAppContext, reduced: bool) -> u32 {
        cx.update(|cx| cx.set_reduce_motion(reduced));
        let (view, cx) = cx.add_window_view(|_, _| BusyBar { theme: Theme::default(), renders: 0 });
        cx.run_until_parked();
        // Past the wait before a bar shows: a bar not yet shown draws nothing.
        cx.executor().advance_clock(kit::progress::SHOW_AFTER);
        cx.run_until_parked();
        let before = view.read_with(cx, |v, _| v.renders);
        for _ in 0..120 {
            cx.executor().advance_clock(Duration::from_nanos(8_333_333));
            cx.update(Window::simulate_next_frame);
            cx.run_until_parked();
        }
        view.read_with(cx, |v, _| v.renders).saturating_sub(before)
    }

    /// A busy bar breathes on the working mark's clock, twelve frames a second and not one per
    /// refresh; under Reduce Motion it stands still and asks for no frame.
    #[gpui::test]
    fn a_busy_bar_breathes_on_the_spin_clock_unless_motion_is_reduced(
        cx: &mut gpui::TestAppContext,
    ) {
        let breathing = busy_frames(cx, false);
        assert!((10..=14).contains(&breathing), "twelve steps a second: {breathing}");
        assert_eq!(busy_frames(cx, true), 0, "still under Reduce Motion");
    }

    #[test]
    fn the_buttons_are_sentence_case() {
        for text in [UPDATE, TRY_AGAIN] {
            let rest: String = text.chars().skip(1).collect();
            assert!(text.starts_with(char::is_uppercase) && !rest.contains(char::is_uppercase));
        }
    }
}
