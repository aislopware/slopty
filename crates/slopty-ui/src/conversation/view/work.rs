//! What the agent is working through, drawn: thinking as a line that opens, a plan as its own
//! document, the task list's progress, and the work running in the background.
//!
//! - **Thinking** is one line, "Thought for 12 s", with the first words of it in the muted tone; a
//!   click opens the whole of it. The density decides whether the line shows at all (Normal hides
//!   it) and whether it opens unasked (Verbose).
//! - **A plan** (`ExitPlanMode`) reads as a document in a frame: "Plan", its title and whether it
//!   was approved over its Markdown at the prose size. A long one shows its head and opens on
//!   request. It stays in view when its turn folds.
//! - **The task list** is a section of the composer's shell while any task is open: the one in
//!   progress, "3/7", a bar of one segment a task, and when opened each task with how long it took.
//! - **Background work** (a command or a subagent started with `run_in_background`) is the top
//!   section of the composer's shell while it runs, and after until the next prompt or until the
//!   transcript shows its call: what it is, how it stands and how long it has run, and while it
//!   runs its last line; it opens to its last lines. Claude Code gives Slopty no way to stop one
//!   short of typing into its terminal, so the tray shows the state only.

use std::time::Duration;

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, Context, ElementId, FontWeight, InteractiveElement as _, IntoElement as _,
    ParentElement as _, Pixels, SharedString, StatefulInteractiveElement as _, Styled as _, div,
    px, relative,
};
use slopty_proto::conversation::{
    AgentDetail, AgentRun, BashDetail, Body, Clipped, Entry, ResultStatus, ShellStatus, Task,
    ThreadId, ToolCall, ToolDetail,
};
use slopty_theme::Typography;

use super::ConversationView;
use crate::colors::hsla;
use crate::conversation::rows::{self, Level};
use crate::conversation::{figures, tools};
use crate::icons::{IconName, Status};
use crate::kit;

/// Lines of a long plan shown before it opens.
const PLAN_HEAD: usize = 12;

/// A plan longer than this opens on request.
const PLAN_FOLD: usize = 16;

/// Background work listed before "N more".
const TRAY_ROWS: usize = 3;

/// Last lines a background command shows once opened in the tray.
const TRAY_LINES: usize = 12;

/// Most segments the task bar draws; a longer list shows its count alone.
const SEGMENTS: usize = 10;

/// How a piece of background work stands.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Standing {
    /// Still going.
    Running,
    /// Finished well.
    Done,
    /// Failed, or exited otherwise than 0.
    Failed,
    /// Stopped before it finished.
    Stopped,
}

/// One piece of background work as the tray shows it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) struct Work {
    /// Its call.
    pub id: String,
    /// What it is: the model's description, else the command's first line.
    pub name: String,
    /// It is a subagent (its thread opens), not a command.
    pub agent: Option<String>,
    /// How it stands.
    pub standing: Standing,
    /// "Running", "Done", "Exit 1", "Stopped".
    pub word: String,
    /// How long it ran, or has run so far, in ms.
    pub took_ms: Option<u64>,
}

/// The work `entry` started in the background, as of `now_ms`.
#[must_use]
pub(super) fn work_of(entry: &Entry, now_ms: u64) -> Option<Work> {
    let Body::Tool(call) = &entry.body else { return None };
    let since = |end: Option<u64>| {
        let end = end.unwrap_or(now_ms);
        (entry.at_ms > 0 && end > entry.at_ms).then(|| end.saturating_sub(entry.at_ms))
    };
    match &call.detail {
        ToolDetail::Bash(bash) => {
            let (standing, word) = bash_standing(bash);
            let name = bash
                .description
                .clone()
                .filter(|d| !d.trim().is_empty())
                .unwrap_or_else(|| first_line(&bash.command.text));
            let took = match standing {
                Standing::Running => since(None),
                _ => bash.finished_ms.and_then(|at| since(Some(at))),
            };
            Some(Work { id: entry.id.clone(), name, agent: None, standing, word, took_ms: took })
        }
        ToolDetail::Agent(agent) => {
            let (standing, word) = match agent.status {
                AgentRun::Running => (Standing::Running, "Working"),
                AgentRun::Completed => (Standing::Done, "Done"),
                AgentRun::Failed => (Standing::Failed, "Failed"),
                AgentRun::Killed => (Standing::Stopped, "Stopped"),
            };
            let name = agent.description.clone().unwrap_or_else(|| first_line(&agent.prompt.text));
            let took = match standing {
                Standing::Running => since(None),
                _ => agent.duration_ms,
            };
            Some(Work {
                id: entry.id.clone(),
                name,
                agent: agent.agent_id.clone(),
                standing,
                word: word.to_owned(),
                took_ms: took,
            })
        }
        _ => None,
    }
}

fn bash_standing(bash: &BashDetail) -> (Standing, String) {
    match bash.status {
        ShellStatus::Running => (Standing::Running, "Running".to_owned()),
        ShellStatus::Done => (Standing::Done, "Done".to_owned()),
        ShellStatus::Failed => (
            Standing::Failed,
            bash.exit_code.map_or_else(|| "Failed".to_owned(), |code| format!("Exit {code}")),
        ),
        ShellStatus::Interrupted | ShellStatus::Killed => (Standing::Stopped, "Stopped".to_owned()),
    }
}

fn first_line(text: &str) -> String {
    text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or_default().to_owned()
}

/// A plan's title (its first heading, else its first line) and the Markdown under it.
#[must_use]
pub(super) fn plan_parts(plan: &str) -> (String, &str) {
    let trimmed = plan.trim_start();
    let (first, rest) = trimmed.split_once('\n').unwrap_or((trimmed, ""));
    match first.trim().strip_prefix('#') {
        Some(heading) => (heading.trim_start_matches('#').trim().to_owned(), rest.trim_start()),
        None => ("Proposed plan".to_owned(), trimmed),
    }
}

/// Whether the person approved a plan, as its call's result says.
#[must_use]
pub(super) fn plan_word(call: &ToolCall) -> &'static str {
    match call.result.as_ref().map(|r| r.status) {
        None => "Awaiting approval",
        Some(ResultStatus::Ok) => "Approved",
        Some(ResultStatus::Error | ResultStatus::Rejected) => "Not approved",
    }
}

/// The task list's progress: tasks done, and the task on show (in progress, else the next).
#[must_use]
pub(super) fn progress(tasks: &[Task]) -> (usize, Option<&Task>) {
    let done = tasks.iter().filter(|t| t.status == "completed").count();
    let current = tasks
        .iter()
        .find(|t| t.status == "in_progress")
        .or_else(|| tasks.iter().find(|t| t.status == "pending"));
    (done, current)
}

impl ConversationView {
    // ----- thinking ------------------------------------------------------------------------

    /// Thinking as one line that opens: "Thought for 12 s" (or "Thinking" while it streams)
    /// and its first words; opened, the whole of it under the line. `key` is what a click
    /// flips.
    #[expect(clippy::too_many_arguments, reason = "the row's name, its text, its state, the view")]
    pub(super) fn thinking_row(
        &self,
        id: &str,
        text: &str,
        took_ms: Option<u64>,
        open: bool,
        live: bool,
        key: String,
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let lead = match (live, took_ms) {
            (true, _) => "Thinking".to_owned(),
            (false, Some(ms)) => {
                format!("Thought for {}", kit::duration(Duration::from_millis(ms)))
            }
            (false, None) => "Thought".to_owned(),
        };
        let preview = if live {
            text.lines().rev().map(str::trim).find(|l| !l.is_empty()).unwrap_or_default()
        } else {
            text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or_default()
        };
        let mark = if live {
            crate::icons::status_icon(
                theme,
                Status::Working,
                self.z(theme.typography.icon()),
                hsla(s.accent),
            )
        } else {
            self.icon(IconName::Brain, s.text_muted)
        };
        let selector = format!("thinking-{id}");
        let line = div()
            .id(ElementId::Name(SharedString::from(selector.clone())))
            .debug_selector(move || selector)
            .group("thinking")
            .role(Role::Button)
            .aria_label(SharedString::from(lead.clone()))
            .aria_expanded(open)
            .w_full()
            .min_w_0()
            .flex()
            .items_center()
            .gap(self.z(theme.spacing.xs))
            .min_h(self.z(super::entries::TOOL_ROW))
            .rounded(self.z(theme.radii.sm))
            .text_size(self.z(theme.typography.small()))
            .cursor_pointer()
            .hover(move |el| el.bg(hsla(s.raised)))
            .child(self.disclosure_slot(mark, open, "thinking"))
            .child(
                div()
                    .flex_none()
                    .text_color(hsla(s.text_secondary))
                    .child(SharedString::from(lead)),
            )
            .when(!open && !preview.is_empty(), |el| {
                el.child(
                    div()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .text_color(hsla(s.text_muted))
                        .child(SharedString::from(preview.to_owned())),
                )
            });
        let line = crate::a11y::tab_stop(line, s.accent)
            .on_click(cx.listener(move |this, _ev, _w, cx| this.toggle(key.clone(), cx)));
        let body = open.then(|| {
            div()
                .pl(self.indent())
                .pb(self.z(theme.spacing.xs))
                .text_size(self.z(theme.typography.small()))
                .line_height(relative(theme.typography.markdown_line_height))
                .text_color(hsla(s.text_muted))
                .whitespace_normal()
                .child(SharedString::from(text.to_owned()))
        });
        div()
            .id(ElementId::Name(SharedString::from(format!("thinking-row-{id}"))))
            .role(Role::Article)
            .aria_label("Thinking")
            .flex()
            .flex_col()
            .child(line)
            .children(body)
            .into_any_element()
    }

    /// A row's leading slot that says it opens: its mark at rest, the chevron while the
    /// pointer is on the row named `group` or while it is open.
    pub(super) fn disclosure_slot(
        &self,
        mark: AnyElement,
        open: bool,
        group: &'static str,
    ) -> AnyElement {
        let s = self.theme.surfaces;
        let chevron = self
            .icon(if open { IconName::ChevronDown } else { IconName::ChevronRight }, s.text_muted);
        self.slot()
            .relative()
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .when(open, gpui::Styled::invisible)
                    .when(!open, |el| el.group_hover(group, gpui::Styled::invisible))
                    .child(mark),
            )
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .when(!open, |el| el.invisible().group_hover(group, gpui::Styled::visible))
                    .child(chevron),
            )
            .into_any_element()
    }

    // ----- plans ---------------------------------------------------------------------------

    /// A plan as a document in the conversation: its head (what it is, its title, whether it
    /// was approved, a copy), then its Markdown at the prose size; a long one shows its head
    /// until opened.
    pub(super) fn plan_card(
        &self,
        entry: &Entry,
        call: &ToolCall,
        plan: &Clipped,
        level: Level,
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let id = entry.id.as_str();
        let whole = self.text_of(plan);
        let (title, body) = plan_parts(whole);
        let lines = body.lines().count();
        let long = lines > PLAN_FOLD;
        let open = level == Level::Full || !long;
        let shown = if open {
            body.to_owned()
        } else {
            body.lines().take(PLAN_HEAD).collect::<Vec<_>>().join("\n")
        };
        let word = plan_word(call);
        let key = rows::entry_key(id);
        let group = "plan";
        let head = div()
            .flex()
            .items_center()
            .gap(self.z(theme.spacing.sm))
            .h(self.z(theme.density.row))
            .px(self.z(theme.spacing.sm))
            .border_b_1()
            .border_color(hsla(s.border_subtle))
            .text_size(self.z(theme.typography.small()))
            .child(self.icon(IconName::Map, s.text_muted))
            .child(div().flex_none().text_color(hsla(s.text_muted)).child("Plan"))
            .child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_color(hsla(s.text))
                    .font_weight(FontWeight(Typography::MEDIUM_WEIGHT))
                    .child(SharedString::from(title.clone())),
            )
            .child(div().flex_1())
            .child(
                div()
                    .flex_none()
                    .text_size(self.z(theme.typography.meta()))
                    .text_color(hsla(s.text_muted))
                    .child(word),
            )
            .child(self.copy_button(
                format!("plan:{id}"),
                group,
                "Copy plan",
                whole.to_owned(),
                cx,
            ));
        let more = (!open).then(|| {
            let label = SharedString::from(format!(
                "Show the whole plan \u{b7} {}",
                tools::count(lines as u64, "line", "lines")
            ));
            let selector = format!("plan-more-{id}");
            let key = key.clone();
            crate::a11y::tab_stop(
                div()
                    .id(ElementId::Name(SharedString::from(selector.clone())))
                    .debug_selector(move || selector)
                    .role(Role::Button)
                    .aria_label(label.clone())
                    .px(self.z(theme.spacing.md))
                    .py(self.z(theme.spacing.xs))
                    .border_t_1()
                    .border_color(hsla(s.border_subtle))
                    .text_size(self.z(theme.typography.small()))
                    .text_color(hsla(s.text_muted))
                    .cursor_pointer()
                    .hover(move |el| el.text_color(hsla(s.text)))
                    .child(label),
                s.accent,
            )
            .on_click(cx.listener(move |this, _ev, _w, cx| this.toggle(key.clone(), cx)))
        });
        let selector = format!("plan-{id}");
        div()
            .id(ElementId::Name(SharedString::from(selector.clone())))
            .debug_selector(move || selector)
            .role(Role::Article)
            .aria_label(SharedString::from(format!("Plan: {title}, {word}")))
            .group(group)
            .my(self.z(theme.spacing.sm))
            .flex()
            .flex_col()
            .rounded(self.z(theme.radii.md))
            .border_1()
            .border_color(hsla(s.border_subtle))
            .bg(hsla(s.panel))
            .overflow_hidden()
            .child(head)
            .child(
                div()
                    .px(self.z(theme.spacing.md))
                    .py(self.z(theme.spacing.sm))
                    .text_size(self.z(theme.typography.prose()))
                    .line_height(relative(theme.typography.markdown_line_height))
                    .child(self.markdown(format!("plan-{}-{id}", self.session), &shown))
                    .children(open.then(|| self.expand_link(id, plan, cx)).flatten()),
            )
            .children(more)
            .into_any_element()
    }

    // ----- the task list -------------------------------------------------------------------

    /// The task list, a section of the composer's shell while a task is open or the agent
    /// works: the task in progress, "3/7" and a bar of a segment a task; opened, each task with
    /// how long it took.
    pub(super) fn tasks_card(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let thread = self.model.thread(&self.thread)?;
        let tasks = thread.tasks();
        let (done, current) = progress(tasks);
        if tasks.is_empty() || (done == tasks.len() && !self.working()) {
            return None;
        }
        let theme = &self.theme;
        let s = theme.surfaces;
        let open = self.tasks_open;
        let total = tasks.len();
        let all_done = done == total;
        let segments = (total > 1 && total <= SEGMENTS).then(|| {
            div()
                .flex_none()
                .w(self.z(theme.spacing.xl * 2.0))
                .flex()
                .items_center()
                .gap(self.z(theme.spacing.xxs))
                .children(tasks.iter().map(|t| {
                    let tone = match t.status.as_str() {
                        "completed" => s.success,
                        "in_progress" => s.accent_fill,
                        _ => s.border,
                    };
                    div().flex_1().min_w_0().h(px(3.0)).rounded_full().bg(hsla(tone))
                }))
        });
        let label = format!(
            "Tasks: {done} of {total} done{}",
            current.map(|t| format!(", now {}", t.subject)).unwrap_or_default()
        );
        let head = crate::a11y::tab_stop(
            kit::tabular(div())
                .id("tasks-head")
                .debug_selector(|| "tasks-head".to_owned())
                .group("tasks-head")
                .role(Role::Button)
                .aria_label(SharedString::from(label))
                .aria_expanded(open)
                .flex()
                .items_center()
                .gap(self.z(theme.spacing.sm))
                .h(self.z(theme.density.row))
                .pl(self.shell_lead())
                .pr(self.shell_trail())
                .rounded(self.z(theme.radii.sm))
                .cursor_pointer()
                .hover(move |el| el.bg(hsla(s.raised)))
                .child(self.disclosure_slot(
                    self.icon(IconName::ListTodo, s.text_muted),
                    open,
                    "tasks-head",
                ))
                .child(div().flex_none().text_color(hsla(s.text_muted)).child("Tasks"))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .text_color(hsla(s.text))
                        .font_weight(FontWeight(Typography::MEDIUM_WEIGHT))
                        .children(current.map(|t| SharedString::from(t.subject.clone()))),
                )
                .child(
                    div()
                        .flex_none()
                        .text_size(self.z(theme.typography.meta()))
                        .text_color(hsla(if all_done { s.success } else { s.text_muted }))
                        .child(SharedString::from(format!("{done}/{total}"))),
                )
                .children(segments),
            s.accent,
        )
        .on_click(cx.listener(|this, _ev, _w, cx| {
            this.tasks_open = !this.tasks_open;
            cx.notify();
        }));
        let list = open.then(|| {
            let times = figures::task_times(thread.entries());
            let now = super::now_ms();
            div()
                .flex()
                .flex_col()
                .pl(self.shell_lead())
                .pr(self.shell_trail())
                .pb(self.z(theme.spacing.xs))
                .children(tasks.iter().map(|task| {
                    let (icon, tone, ink) = match task.status.as_str() {
                        "completed" => (IconName::CircleCheck, s.success, s.text_muted),
                        "in_progress" => (IconName::CircleDot, s.accent, s.text),
                        _ => (IconName::Circle, s.text_muted, s.text_secondary),
                    };
                    let (started, ended) = times.get(&task.id).copied().unwrap_or_default();
                    let took = match (task.status.as_str(), started, ended) {
                        ("completed", Some(a), Some(b)) if b > a => {
                            Some(kit::duration(Duration::from_millis(b.saturating_sub(a))))
                        }
                        ("in_progress", Some(a), _) if now > a => {
                            Some(kit::duration(Duration::from_millis(now.saturating_sub(a))))
                        }
                        _ => None,
                    };
                    div()
                        .id(ElementId::Name(SharedString::from(format!("task-{}", task.id))))
                        .role(Role::ListItem)
                        .aria_label(SharedString::from(format!(
                            "{}: {}",
                            task.subject,
                            tools::status_label(&task.status)
                        )))
                        .flex()
                        .items_start()
                        .gap(self.z(theme.spacing.xs))
                        .min_h(self.z(super::entries::TOOL_ROW))
                        .child(self.slot().child(self.icon(icon, tone)))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .pt(self.z(theme.spacing.xxs))
                                .whitespace_normal()
                                .text_color(hsla(ink))
                                .child(SharedString::from(task.subject.clone())),
                        )
                        .children(took.map(|took| {
                            kit::tabular(div())
                                .flex_none()
                                .pt(self.z(theme.spacing.xxs))
                                .text_size(self.z(theme.typography.meta()))
                                .text_color(hsla(s.text_muted))
                                .child(SharedString::from(took))
                        }))
                }))
        });
        Some(
            div()
                .id("tasks")
                .debug_selector(|| "tasks".to_owned())
                .role(Role::Group)
                .aria_label("Tasks")
                .flex()
                .flex_col()
                .text_size(self.z(theme.typography.small()))
                .child(head)
                .children(list)
                .into_any_element(),
        )
    }

    // ----- the composer's sections -----------------------------------------------------------

    /// Where a row of the composer's shell starts inside its section, so its mark sits on the
    /// field's text edge: the shell's pad and the field's inset, less the section's pad and the
    /// room the mark's slot leaves round it.
    fn shell_lead(&self) -> Pixels {
        let t = &self.theme;
        let slack = (super::entries::TOOL_ROW - t.typography.icon()) / 2.0;
        self.z(t.spacing.md + kit::FIELD_INSET - t.spacing.xs - slack)
    }

    /// Where a row of the composer's shell ends inside its section: on the send button's edge.
    fn shell_trail(&self) -> Pixels {
        self.z(self.theme.spacing.md - self.theme.spacing.xs)
    }

    // ----- background work -----------------------------------------------------------------

    /// The main thread's background work, as the tray lists it.
    #[must_use]
    pub(super) fn background_work(&self) -> Vec<Work> {
        let Some(main) = self.model.thread(&ThreadId::Main) else { return Vec::new() };
        let now = super::now_ms();
        figures::background(main.entries()).into_iter().filter_map(|e| work_of(e, now)).collect()
    }

    /// Whether any background work runs: the tray's clock ticks while it does.
    pub(super) fn background_running(&self) -> bool {
        self.background_work().iter().any(|w| w.standing == Standing::Running)
    }

    /// The work in the background, a section of the composer's shell: a row each, the first
    /// few unless all were asked for. It is the session's, so a subagent's thread leaves it out.
    pub(super) fn tray(&self, cx: &Context<Self>) -> Option<AnyElement> {
        if self.thread != ThreadId::Main {
            return None;
        }
        // A finished piece leaves once the transcript shows its call, so it is not said twice.
        let work: Vec<Work> = self
            .background_work()
            .into_iter()
            .filter(|w| {
                w.standing == Standing::Running
                    || crate::conversation::find::row_of(&self.rows, &w.id).is_none()
            })
            .collect();
        if work.is_empty() {
            return None;
        }
        let theme = &self.theme;
        let s = theme.surfaces;
        let hidden = work.len().saturating_sub(TRAY_ROWS);
        let shown = if self.tray_all { work.len() } else { TRAY_ROWS.min(work.len()) };
        let more = (hidden > 0).then(|| {
            let label: SharedString = if self.tray_all {
                "Show fewer".into()
            } else {
                format!("{hidden} more in the background").into()
            };
            crate::a11y::tab_stop(
                div()
                    .id("tray-more")
                    .debug_selector(|| "tray-more".to_owned())
                    .role(Role::Button)
                    .aria_label(label.clone())
                    .h(self.z(super::entries::TOOL_ROW))
                    .flex()
                    .items_center()
                    .pl(self.shell_lead() + self.indent())
                    .text_size(self.z(theme.typography.meta()))
                    .text_color(hsla(s.text_muted))
                    .cursor_pointer()
                    .hover(move |el| el.text_color(hsla(s.text)))
                    .child(label),
                s.accent,
            )
            .on_click(cx.listener(|this, _ev, _w, cx| {
                this.tray_all = !this.tray_all;
                cx.notify();
            }))
        });
        Some(
            div()
                .id("background")
                .debug_selector(|| "background".to_owned())
                .role(Role::List)
                .aria_label("In the background")
                .flex()
                .flex_col()
                .text_size(self.z(theme.typography.small()))
                .children(work.iter().take(shown).map(|w| self.tray_row(w, cx)))
                .children(more)
                .into_any_element(),
        )
    }

    fn tray_row(&self, work: &Work, cx: &Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let id = work.id.clone();
        let open = self.tray_open.contains(&id);
        let mark = match (work.standing, &work.agent) {
            (Standing::Running, _) => crate::icons::status_icon(
                theme,
                Status::Working,
                self.z(theme.typography.icon()),
                hsla(s.accent),
            ),
            (Standing::Stopped, _) => self.icon(IconName::CirclePause, s.text_muted),
            (_, Some(_)) => self.icon(IconName::Bot, s.text_muted),
            (_, None) => self.icon(IconName::SquareTerminal, s.text_muted),
        };
        let failed = (work.standing == Standing::Failed).then(|| {
            crate::icons::icon(theme, IconName::X, crate::icons::IconSize::Inline, hsla(s.error))
                .size(self.z(theme.typography.meta()))
                .flex_none()
        });
        let running = work.standing == Standing::Running;
        let detail = std::iter::once(work.word.clone())
            .chain(work.took_ms.map(|ms| kit::duration(Duration::from_millis(ms))))
            .collect::<Vec<_>>()
            .join(" \u{b7} ");
        let (preview, lines) = self.work_lines(work, cx);
        let group = "tray-row";
        let selector = format!("tray-{id}");
        let label = format!("{}: {detail}", work.name);
        // The last line only while the work runs, right-aligned and cut from its start, where
        // a build's progress changes least.
        let tail = preview.filter(|_| running && !open).map(|p| {
            div()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis_start()
                .font_family(self.mono())
                .text_size(self.z(theme.typography.meta()))
                .text_color(hsla(s.text_muted))
                .child(SharedString::from(p))
        });
        let line = div()
            .id(ElementId::Name(SharedString::from(selector.clone())))
            .debug_selector(move || selector)
            .group(group)
            .role(Role::Button)
            .aria_label(SharedString::from(label))
            .aria_expanded(open)
            .flex()
            .items_center()
            .gap(self.z(theme.spacing.xs))
            .h(self.z(theme.density.row))
            .pl(self.shell_lead())
            .pr(self.shell_trail())
            .rounded(self.z(theme.radii.sm))
            .cursor_pointer()
            .hover(move |el| el.bg(hsla(s.raised)))
            .child(if work.agent.is_some() {
                self.slot().child(mark).into_any_element()
            } else {
                self.disclosure_slot(mark, open, group)
            })
            .child(
                div()
                    .flex_none()
                    .max_w(relative(0.5))
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_color(hsla(s.text))
                    .child(SharedString::from(work.name.clone())),
            )
            .child(kit::separator(theme))
            .children(failed)
            .child(
                kit::tabular(div())
                    .flex_none()
                    .whitespace_nowrap()
                    .text_color(hsla(s.text_muted))
                    .child(SharedString::from(detail)),
            )
            .child(div().flex_1().min_w(self.z(theme.spacing.md)))
            .children(tail);
        let agent = work.agent.clone();
        let line = crate::a11y::tab_stop(line, s.accent).on_click(cx.listener(
            move |this, _ev, _w, cx| {
                if let Some(agent) = &agent {
                    this.open_thread(ThreadId::Agent(agent.clone()), cx);
                } else if !this.tray_open.remove(&id) {
                    this.tray_open.insert(id.clone());
                    cx.notify();
                } else {
                    cx.notify();
                }
            },
        ));
        let body = (open && work.agent.is_none()).then_some(lines).flatten();
        div().flex().flex_col().child(line).children(body).into_any_element()
    }

    /// A piece of work's last line, for its row at rest, and its last lines in a frame, for
    /// its row opened: a command's output, a subagent's call now or the head of its report.
    fn work_lines(&self, work: &Work, cx: &Context<Self>) -> (Option<String>, Option<AnyElement>) {
        let theme = &self.theme;
        let s = theme.surfaces;
        if let Some(agent) = &work.agent {
            let own = self.model.thread(&ThreadId::Agent(agent.clone()));
            let now =
                own.and_then(|t| {
                    t.entries().iter().rev().find_map(|e| match &e.body {
                        Body::Tool(call) => {
                            let title = tools::title(call, t.tasks());
                            Some(title.subject.map_or_else(
                                || title.verb.clone(),
                                |s| format!("{} {s}", title.verb),
                            ))
                        }
                        _ => None,
                    })
                });
            let report = self.agent_report(&work.id).map(|r| first_line(&r));
            let line = if work.standing == Standing::Running { now } else { report.or(now) };
            return (line, None);
        }
        let Some(output) = self.model.output(&ThreadId::Main, &work.id) else {
            return (None, None);
        };
        let text = self.text_in(&ThreadId::Main, &output.tail);
        let last =
            text.lines().rev().map(str::trim_end).find(|l| !l.trim().is_empty()).map(str::to_owned);
        let lines: Vec<&str> = text.lines().collect();
        let tail = lines
            .iter()
            .skip(lines.len().saturating_sub(TRAY_LINES))
            .copied()
            .collect::<Vec<_>>()
            .join("\n");
        let finished = work.standing != Standing::Running;
        let expand = finished
            .then(|| {
                self.expand_link_in(&ThreadId::Main, &format!("tray-{}", work.id), &output.tail, cx)
            })
            .flatten();
        let frame = (!tail.trim().is_empty()).then(|| {
            let selector = format!("tray-lines-{}", work.id);
            div()
                .debug_selector(move || selector)
                .pl(self.shell_lead() + self.indent())
                .pr(self.shell_trail())
                .pb(self.z(theme.spacing.xs))
                .child(
                    self.code_frame()
                        .px(self.z(theme.spacing.sm))
                        .py(self.z(theme.spacing.xs))
                        .child(self.code_text(&tail, s.text_secondary))
                        .children(expand),
                )
                .into_any_element()
        });
        (last, frame)
    }

    /// The report a subagent's call carries, whole or as far as it came.
    fn agent_report(&self, id: &str) -> Option<String> {
        let entry = self.model.thread(&ThreadId::Main)?.entry(id)?;
        let Body::Tool(call) = &entry.body else { return None };
        let ToolDetail::Agent(AgentDetail { report: Some(report), .. }) = &call.detail else {
            return None;
        };
        Some(self.text_in(&ThreadId::Main, report).to_owned())
    }
}

#[cfg(test)]
mod tests {
    use slopty_proto::conversation::{AgentDetail, ToolResult};

    use super::*;

    fn clipped(text: &str) -> Clipped {
        Clipped { text: text.to_owned(), lines: 1, chars: 1, full: None }
    }

    fn call(detail: ToolDetail, result: Option<ResultStatus>) -> ToolCall {
        ToolCall {
            name: "Bash".to_owned(),
            detail,
            result: result.map(|status| ToolResult {
                status,
                text: None,
                images: Vec::new(),
                at_ms: 0,
            }),
        }
    }

    fn other() -> ToolDetail {
        ToolDetail::Other { input: clipped("{}") }
    }

    fn bash(status: ShellStatus, exit_code: Option<i32>, finished_ms: Option<u64>) -> BashDetail {
        BashDetail {
            command: clipped("cargo build --release\n--verbose"),
            description: None,
            background: true,
            task_id: Some("b1".to_owned()),
            status,
            exit_code,
            stdout: None,
            stderr: None,
            output_file: None,
            finished_ms,
        }
    }

    fn entry(detail: ToolDetail) -> Entry {
        Entry { id: "t1".to_owned(), at_ms: 10_000, body: Body::Tool(Box::new(call(detail, None))) }
    }

    /// Background work says what it is, how it stands and how long it ran: a command by its
    /// first line until it ends, then by when it ended; a subagent by its own figure.
    #[test]
    fn background_work_says_how_it_stands() {
        let running =
            work_of(&entry(ToolDetail::Bash(bash(ShellStatus::Running, None, None))), 13_000)
                .unwrap();
        assert_eq!(
            (running.name.as_str(), running.standing, running.word.as_str(), running.took_ms),
            ("cargo build --release", Standing::Running, "Running", Some(3_000))
        );
        let failed = ToolDetail::Bash(bash(ShellStatus::Failed, Some(101), Some(15_000)));
        let failed = work_of(&entry(failed), 99_000).unwrap();
        assert_eq!((failed.standing, failed.word.as_str()), (Standing::Failed, "Exit 101"));
        assert_eq!(failed.took_ms, Some(5_000), "to when it ended, not to now");
        let agent = ToolDetail::Agent(AgentDetail {
            agent_id: Some("x1".to_owned()),
            agent_type: None,
            description: Some("Survey the chips".to_owned()),
            prompt: clipped("look"),
            background: true,
            status: AgentRun::Completed,
            report: None,
            tokens: None,
            tool_uses: None,
            duration_ms: Some(42_000),
        });
        let agent = work_of(&entry(agent), 99_000).unwrap();
        assert_eq!(
            (agent.name.as_str(), agent.word.as_str(), agent.took_ms, agent.agent.as_deref()),
            ("Survey the chips", "Done", Some(42_000), Some("x1"))
        );
        assert!(work_of(&entry(other()), 0).is_none());
    }

    /// A plan's title is its first heading, else it is a proposed plan; its word follows the
    /// answer to it.
    #[test]
    fn a_plan_has_a_title_and_a_word() {
        assert_eq!(
            plan_parts("# Fix the header\n\n1. Measure"),
            ("Fix the header".to_owned(), "1. Measure")
        );
        assert_eq!(plan_parts("1. Measure"), ("Proposed plan".to_owned(), "1. Measure"));
        let plan = other();
        assert_eq!(plan_word(&call(plan.clone(), None)), "Awaiting approval");
        assert_eq!(plan_word(&call(plan.clone(), Some(ResultStatus::Ok))), "Approved");
        assert_eq!(plan_word(&call(plan, Some(ResultStatus::Rejected))), "Not approved");
    }

    /// The task on show is the one in progress, else the next one waiting.
    #[test]
    fn progress_counts_done_and_names_the_task_on_show() {
        let task = |subject: &str, status: &str| Task {
            id: subject.to_owned(),
            subject: subject.to_owned(),
            status: status.to_owned(),
        };
        let tasks = [task("a", "completed"), task("b", "pending"), task("c", "in_progress")];
        let (done, current) = progress(&tasks);
        assert_eq!((done, current.map(|t| t.subject.as_str())), (1, Some("c")));
        let (_, next) = progress(&tasks[..2]);
        assert_eq!(next.map(|t| t.subject.as_str()), Some("b"));
    }
}
