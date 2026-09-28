//! What a call shows under its title: an edit's diff, a command and its output, a subagent's
//! card, a result's text, a plan, the questions asked.
//!
//! Code sits in one frame everywhere: the panel under a quiet hairline at the medium radius,
//! the mono face at the small size. A diff heads its frame with the file and its size, washes
//! its added and removed lines in the success and error fills at the faint step and colours
//! their text by the file's grammar; context stays muted so the change is what the eye finds.
//! Nothing shows raw JSON: a tool's input reads as its keys and their values.

use std::time::Duration;

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, Context, Div, ElementId, InteractiveElement as _, IntoElement as _,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, StyledText,
    div,
};
use slopty_proto::conversation::{
    AgentDetail, AgentRun, BashDetail, Entry, Patch, QuestionDetail, ResultStatus, ThreadId,
    ToolCall, ToolDetail,
};
use slopty_theme::{Rgb, alpha};

use super::{ConversationView, SPLIT_FROM};
use crate::colors::{hsla, hsla_alpha};
use crate::conversation::diff::{self, Block, Kind, Line};
use crate::conversation::model::Expanded;
use crate::conversation::rows::{self, Level};
use crate::conversation::tools;
use crate::highlight::{self, Span};
use crate::icons::IconName;

/// Lines of a command's output a summary shows: its end, where a log says how it went.
const OUTPUT_TAIL: usize = 4;

/// Spaces a tab stands for in code.
const TAB: &str = "    ";

/// Whether a call has anything to show under its title.
#[must_use]
pub(super) fn has_body(call: &ToolCall) -> bool {
    let result_text = call
        .result
        .as_ref()
        .and_then(|r| r.text.as_ref())
        .is_some_and(|t| !t.text.trim().is_empty())
        || rows::has_pictures(call);
    match &call.detail {
        ToolDetail::Edit(edit) => !edit.patch.hunks.is_empty() || result_text,
        ToolDetail::Write(write) => !write.patch.hunks.is_empty() || result_text,
        ToolDetail::Bash(_)
        | ToolDetail::Agent(_)
        | ToolDetail::Question(_)
        | ToolDetail::Plan { .. }
        | ToolDetail::Mcp(_)
        | ToolDetail::TodoWrite { .. } => true,
        ToolDetail::Other { input } => result_text || input.text.trim() != "{}",
        ToolDetail::TaskCreate(task) => task.description.is_some(),
        ToolDetail::WebFetch(fetch) => fetch.prompt.is_some() || result_text,
        ToolDetail::WebSearch(search) => !search.links.is_empty() || result_text,
        ToolDetail::Read(_)
        | ToolDetail::Grep(_)
        | ToolDetail::Glob(_)
        | ToolDetail::TaskUpdate(_) => result_text,
    }
}

/// A tool's input as its keys and their values, when it is a JSON object: a string's first
/// line, anything else as compact JSON. `None` for input that is not an object (a clipped one).
#[must_use]
pub(super) fn input_pairs(input: &str) -> Option<Vec<(String, String)>> {
    let serde_json::Value::Object(map) = serde_json::from_str::<serde_json::Value>(input).ok()?
    else {
        return None;
    };
    Some(
        map.into_iter()
            .map(|(key, value)| {
                let value = match value {
                    serde_json::Value::String(text) => {
                        let first = text.lines().next().unwrap_or_default().to_owned();
                        let more = text.lines().count().saturating_sub(1);
                        if more > 0 {
                            format!(
                                "{first} \u{2026} +{}",
                                tools::count(more as u64, "line", "lines")
                            )
                        } else {
                            first
                        }
                    }
                    other => other.to_string(),
                };
                (key, value)
            })
            .collect(),
    )
}

/// `text` with its tabs as spaces, and `spans` stretched to match.
fn detab(text: &str, spans: Option<&[Span]>) -> (String, Option<Vec<Span>>) {
    if !text.contains('\t') {
        return (text.to_owned(), spans.map(<[Span]>::to_vec));
    }
    let out = text.replace('\t', TAB);
    let spans = spans.map(|spans| {
        let mut at = 0_usize;
        spans
            .iter()
            .map(|span| {
                let end = at.saturating_add(span.len);
                let tabs = text.get(at..end).map_or(0, |piece| piece.matches('\t').count());
                at = end;
                Span {
                    len: span.len.saturating_add(tabs.saturating_mul(TAB.len().saturating_sub(1))),
                    ..*span
                }
            })
            .collect()
    });
    (out, spans)
}

impl ConversationView {
    /// Code's frame: the panel, a quiet hairline at the medium radius, the mono face at the
    /// small size, lines at the prose's leading.
    pub(super) fn code_frame(&self) -> Div {
        let theme = &self.theme;
        let s = theme.surfaces;
        div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .rounded(self.z(theme.radii.md))
            .border_1()
            .border_color(hsla(s.border_subtle))
            .bg(hsla(s.panel))
            .overflow_hidden()
            .font_family(self.mono())
            .text_size(self.z(theme.typography.small()))
            .line_height(gpui::relative(theme.typography.markdown_line_height))
    }

    /// Plain text in the mono face, wrapped, in `tone`.
    pub(super) fn code_text(&self, text: &str, tone: Rgb) -> Div {
        div()
            .w_full()
            .min_w_0()
            .whitespace_normal()
            .font_family(self.mono())
            .text_size(self.z(self.theme.typography.small()))
            .text_color(hsla(tone))
            .child(SharedString::from(text.replace('\t', TAB)))
    }

    /// The body a call shows at `level`.
    pub(super) fn tool_body(
        &self,
        entry: &Entry,
        call: &ToolCall,
        level: Level,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        let s = self.theme.surfaces;
        let failure = call
            .result
            .as_ref()
            .filter(|r| r.status != ResultStatus::Ok)
            .and_then(|r| r.text.as_ref())
            .filter(|t| !t.text.trim().is_empty());
        let body = match &call.detail {
            ToolDetail::Edit(edit) => {
                self.patch_block(&self.thread, &entry.id, &edit.path, &edit.patch, level, cx)
            }
            ToolDetail::Write(write) => {
                self.patch_block(&self.thread, &entry.id, &write.path, &write.patch, level, cx)
            }
            ToolDetail::Bash(bash) => Some(self.shell_block(&entry.id, bash, failure, level, cx)),
            ToolDetail::Agent(agent) => Some(self.agent_card(entry, agent, level, cx)),
            ToolDetail::Question(question) => Some(self.questions(question)),
            ToolDetail::Plan { plan } => Some(
                div()
                    .text_size(self.z(self.theme.typography.prose()))
                    .line_height(gpui::relative(self.theme.typography.markdown_line_height))
                    .child(self.markdown(
                        format!("plan-{}-{}", self.session, entry.id),
                        self.text_of(plan),
                    ))
                    .children(self.expand_link(&entry.id, plan, cx))
                    .into_any_element(),
            ),
            ToolDetail::TaskCreate(task) => task.description.as_ref().map(|d| {
                div()
                    .text_size(self.z(self.theme.typography.small()))
                    .text_color(hsla(s.text_secondary))
                    .child(SharedString::from(d.clone()))
                    .into_any_element()
            }),
            ToolDetail::TodoWrite { todos } => Some(self.task_lines(todos, true)),
            ToolDetail::Mcp(mcp) => {
                Some(self.input_and_result(&entry.id, Some(&mcp.input), call, level, cx))
            }
            ToolDetail::Other { input } => {
                Some(self.input_and_result(&entry.id, Some(input), call, level, cx))
            }
            ToolDetail::WebFetch(fetch) => {
                let prompt = fetch.prompt.as_ref().map(|p| {
                    div()
                        .text_size(self.z(self.theme.typography.small()))
                        .text_color(hsla(s.text_secondary))
                        .child(SharedString::from(p.clone()))
                });
                Some(
                    div()
                        .flex()
                        .flex_col()
                        .gap(self.z(self.theme.spacing.xs))
                        .children(prompt)
                        .children(self.result_block(&entry.id, call, level, cx))
                        .into_any_element(),
                )
            }
            ToolDetail::WebSearch(search) if !search.links.is_empty() => {
                Some(self.links(&entry.id, &search.links))
            }
            ToolDetail::Read(_)
            | ToolDetail::Grep(_)
            | ToolDetail::Glob(_)
            | ToolDetail::WebSearch(_)
            | ToolDetail::TaskUpdate(_) => self.result_block(&entry.id, call, level, cx),
        };
        let shows_failure = !matches!(
            call.detail,
            ToolDetail::Bash(_) | ToolDetail::Other { .. } | ToolDetail::Mcp(_)
        ) && !matches!(
            call.detail,
            ToolDetail::Read(_)
                | ToolDetail::Grep(_)
                | ToolDetail::Glob(_)
                | ToolDetail::WebSearch(_)
                | ToolDetail::TaskUpdate(_)
        );
        let failure = failure.filter(|_| shows_failure).map(|text| {
            self.code_text(self.text_of(text), s.error)
                .pt(self.z(self.theme.spacing.xs))
                .into_any_element()
        });
        let pictures = call
            .result
            .as_ref()
            .filter(|r| !r.images.is_empty())
            .map(|r| self.thumbnails(&format!("result-{}", entry.id), &r.images, cx));
        match (body, failure, pictures) {
            (None, None, None) => None,
            (body, failure, pictures) => Some(
                div()
                    .flex()
                    .flex_col()
                    .gap(self.z(self.theme.spacing.xs))
                    .children(pictures)
                    .children(body)
                    .children(failure)
                    .into_any_element(),
            ),
        }
    }

    /// A result's text in the code frame: all of it at full, its head at summary.
    fn result_block(
        &self,
        id: &str,
        call: &ToolCall,
        level: Level,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        let result = call.result.as_ref()?;
        let text = result.text.as_ref().filter(|t| !t.text.trim().is_empty())?;
        let s = self.theme.surfaces;
        let tone = if result.status == ResultStatus::Ok { s.text_secondary } else { s.error };
        let shown = self.text_of(text);
        let shown = if level == Level::Full {
            shown.to_owned()
        } else {
            shown.lines().take(diff::SUMMARY_LINES).collect::<Vec<_>>().join("\n")
        };
        Some(
            self.code_frame()
                .px(self.z(self.theme.spacing.sm))
                .py(self.z(self.theme.spacing.xs))
                .child(self.code_text(&shown, tone))
                .children(
                    self.expand_link(&format!("result-{id}"), text, cx)
                        .filter(|_| level == Level::Full),
                )
                .into_any_element(),
        )
    }

    /// An unknown tool's input, then its result.
    fn input_and_result(
        &self,
        id: &str,
        input: Option<&slopty_proto::conversation::Clipped>,
        call: &ToolCall,
        level: Level,
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let input = input.filter(|i| i.text.trim() != "{}").map(|input| {
            let text = self.text_of(input);
            match input_pairs(text) {
                Some(pairs) => div()
                    .debug_selector({
                        let id = id.to_owned();
                        move || format!("input-{id}")
                    })
                    .flex()
                    .flex_col()
                    .gap(self.z(theme.spacing.xxs))
                    .text_size(self.z(theme.typography.small()))
                    .children(pairs.into_iter().map(|(key, value)| {
                        div()
                            .flex()
                            .gap(self.z(theme.spacing.sm))
                            .child(
                                div()
                                    .flex_none()
                                    .text_color(hsla(s.text_muted))
                                    .child(SharedString::from(key)),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .text_color(hsla(s.text_secondary))
                                    .child(SharedString::from(value)),
                            )
                    })),
                None => self
                    .code_frame()
                    .px(self.z(theme.spacing.sm))
                    .py(self.z(theme.spacing.xs))
                    .child(self.code_text(text, s.text_secondary)),
            }
        });
        div()
            .flex()
            .flex_col()
            .gap(self.z(self.theme.spacing.xs))
            .children(input)
            .children(self.result_block(id, call, level, cx))
            .into_any_element()
    }

    /// A command, and the end of what it printed.
    fn shell_block(
        &self,
        id: &str,
        bash: &BashDetail,
        failure: Option<&slopty_proto::conversation::Clipped>,
        level: Level,
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let command = self.text_of(&bash.command).to_owned();
        let tail = |text: &str| -> String {
            if level == Level::Full {
                return text.to_owned();
            }
            let lines: Vec<&str> = text.lines().collect();
            lines
                .iter()
                .skip(lines.len().saturating_sub(OUTPUT_TAIL))
                .copied()
                .collect::<Vec<_>>()
                .join("\n")
        };
        // A background command's output is in its file, which the worker tails.
        let tailed = self.model.output(&self.thread, id).map(|o| &o.tail);
        let out = bash.stdout.as_ref().or(tailed).filter(|o| !o.text.trim().is_empty());
        let err = bash.stderr.as_ref().filter(|o| !o.text.trim().is_empty());
        let mut output: Vec<AnyElement> = Vec::new();
        if let Some(out) = out {
            let hidden = out.lines.saturating_sub(u32::try_from(OUTPUT_TAIL).unwrap_or(u32::MAX));
            if level != Level::Full && hidden > 0 {
                output.push(
                    div()
                        .text_color(hsla(s.text_muted))
                        .child(SharedString::from(format!(
                            "\u{2026} {}",
                            tools::count(hidden.into(), "line", "lines")
                        )))
                        .into_any_element(),
                );
            }
            output.push(
                self.code_text(&tail(self.text_of(out)), s.text_secondary).into_any_element(),
            );
            // A running command's output is still growing: only a finished one opens whole.
            if level == Level::Full
                && bash.status != slopty_proto::conversation::ShellStatus::Running
            {
                output.extend(self.expand_link(&format!("stdout-{id}"), out, cx));
            }
        }
        if let Some(err) = err {
            output.push(self.code_text(&tail(self.text_of(err)), s.error).into_any_element());
            if level == Level::Full {
                output.extend(self.expand_link(&format!("stderr-{id}"), err, cx));
            }
        }
        if out.is_none()
            && err.is_none()
            && let Some(failure) = failure
        {
            output.push(self.code_text(&tail(self.text_of(failure)), s.error).into_any_element());
        }
        let prompt_line = div()
            .flex()
            .gap(self.z(theme.spacing.sm))
            .child(div().flex_none().text_color(hsla(s.text_muted)).child("$"))
            .child(self.code_text(&command, s.text));
        self.code_frame()
            .debug_selector({
                let id = id.to_owned();
                move || format!("shell-{id}")
            })
            .child(
                div().px(self.z(theme.spacing.sm)).py(self.z(theme.spacing.xs)).child(prompt_line),
            )
            .when(!output.is_empty(), |el| {
                el.child(
                    div()
                        .px(self.z(theme.spacing.sm))
                        .py(self.z(theme.spacing.xs))
                        .border_t_1()
                        .border_color(hsla(s.border_subtle))
                        .flex()
                        .flex_col()
                        .children(output),
                )
            })
            .into_any_element()
    }

    /// A subagent under its call's title, with no frame of its own: while it runs, the call
    /// it is on and for how long; once done, its figures and the head of its report. A click
    /// opens its thread.
    fn agent_card(
        &self,
        entry: &Entry,
        agent: &AgentDetail,
        level: Level,
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let id = entry.id.as_str();
        let word = match agent.status {
            AgentRun::Running => "Working",
            AgentRun::Completed => "Done",
            AgentRun::Failed => "Failed",
            AgentRun::Killed => "Stopped",
        };
        let thread = agent.agent_id.clone().map(ThreadId::Agent);
        let own = thread.as_ref().and_then(|t| self.model.thread(t));
        let running = agent.status == AgentRun::Running;
        // What it is doing now: the newest call in its own thread.
        let now = own.filter(|_| running).and_then(|t| {
            t.entries().iter().rev().find_map(|e| match &e.body {
                slopty_proto::conversation::Body::Tool(call) => {
                    let title = tools::title(call, t.tasks());
                    Some(match title.subject {
                        Some(subject) => format!("{} {subject}", title.verb),
                        None => title.verb,
                    })
                }
                _ => None,
            })
        });
        let elapsed = (running && entry.at_ms > 0).then(|| {
            crate::kit::duration(Duration::from_millis(super::now_ms().saturating_sub(entry.at_ms)))
        });
        let facts = [
            agent.tool_uses.map(|n| tools::count(n, "tool use", "tool uses")),
            agent.tokens.map(|n| format!("{} tokens", tools::tokens(n))),
            agent.duration_ms.map(|ms| crate::kit::duration(Duration::from_millis(ms))).or(elapsed),
        ];
        let facts = facts.into_iter().flatten().collect::<Vec<_>>().join(" \u{b7} ");
        let report = agent.report.as_ref().filter(|_| !running).map(|report| {
            let text = self.text_of(report);
            let text = if level == Level::Full {
                text.to_owned()
            } else {
                text.lines().filter(|l| !l.trim().is_empty()).take(3).collect::<Vec<_>>().join("\n")
            };
            div()
                .text_size(self.z(theme.typography.small()))
                .text_color(hsla(s.text_secondary))
                .child(self.markdown(format!("report-{}-{id}", self.session), &text))
        });
        let brief = (level == Level::Full).then(|| {
            div()
                .text_size(self.z(theme.typography.small()))
                .text_color(hsla(s.text_muted))
                .whitespace_normal()
                .child(SharedString::from(self.text_of(&agent.prompt).to_owned()))
        });
        let opens = own.is_some();
        let selector = format!("subagent-{id}");
        let name = agent.description.clone().unwrap_or_else(|| "Subagent".to_owned());
        let tone = match agent.status {
            AgentRun::Failed => s.error,
            _ => s.text_secondary,
        };
        let status = crate::kit::tabular(div())
            .flex()
            .items_center()
            .gap(self.z(theme.spacing.sm))
            .text_size(self.z(theme.typography.small()))
            .child(div().flex_none().text_color(hsla(tone)).child(word))
            .children(now.map(|now| {
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_color(hsla(s.text_secondary))
                    .child(SharedString::from(now))
            }))
            .child(div().flex_1())
            .when(!facts.is_empty(), |el| {
                el.child(
                    div()
                        .flex_none()
                        .text_size(self.z(theme.typography.meta()))
                        .text_color(hsla(s.text_muted))
                        .child(SharedString::from(facts)),
                )
            })
            .when(opens, |el| el.child(self.icon(IconName::ChevronRight, s.text_muted)));
        let card = div()
            .id(ElementId::Name(SharedString::from(selector.clone())))
            .debug_selector(move || selector)
            .role(if opens { Role::Button } else { Role::Article })
            .aria_label(SharedString::from(format!("Subagent {name}: {word}")))
            .w_full()
            .flex()
            .flex_col()
            .gap(self.z(theme.spacing.xs))
            .py(self.z(theme.spacing.xxs))
            .rounded(self.z(theme.radii.sm))
            .child(status)
            .children(brief)
            .children(report);
        match thread.filter(|_| opens) {
            Some(thread) => crate::a11y::tab_stop(
                card.cursor_pointer().hover(move |el| el.bg(hsla(s.raised))),
                s.accent,
            )
            .on_click(cx.listener(move |this, _ev, _w, cx| this.open_thread(thread.clone(), cx)))
            .into_any_element(),
            None => card.into_any_element(),
        }
    }

    /// The pages a web search found: each title over its host; a click opens the page.
    fn links(&self, id: &str, links: &[slopty_proto::conversation::Link]) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        div()
            .debug_selector({
                let id = id.to_owned();
                move || format!("links-{id}")
            })
            .flex()
            .flex_col()
            .text_size(self.z(theme.typography.small()))
            .children(links.iter().enumerate().map(|(n, link)| {
                let host = link
                    .url
                    .split("://")
                    .nth(1)
                    .and_then(|rest| rest.split('/').next())
                    .unwrap_or(&link.url)
                    .trim_start_matches("www.")
                    .to_owned();
                let url = link.url.clone();
                let title =
                    if link.title.trim().is_empty() { host.clone() } else { link.title.clone() };
                crate::a11y::tab_stop(
                    div()
                        .id(ElementId::Name(SharedString::from(format!("link-{id}-{n}"))))
                        .role(Role::Link)
                        .aria_label(SharedString::from(format!("{title}, {host}")))
                        .flex()
                        .items_center()
                        .gap(self.z(theme.spacing.sm))
                        .min_h(self.z(theme.density.row))
                        .px(self.z(theme.spacing.xs))
                        .rounded(self.z(theme.radii.sm))
                        .cursor_pointer()
                        .hover(move |el| el.bg(hsla(s.raised)))
                        .child(self.icon(IconName::Globe, s.text_muted))
                        .child(
                            div()
                                .min_w_0()
                                .overflow_hidden()
                                .text_ellipsis()
                                .whitespace_nowrap()
                                .text_color(hsla(s.text))
                                .child(SharedString::from(title)),
                        )
                        .child(
                            div()
                                .flex_none()
                                .text_size(self.z(theme.typography.meta()))
                                .text_color(hsla(s.text_muted))
                                .child(SharedString::from(host)),
                        ),
                    s.accent,
                )
                .on_click(move |_ev, _window, cx| cx.open_url(&url))
            }))
            .into_any_element()
    }

    fn questions(&self, question: &QuestionDetail) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        div()
            .flex()
            .flex_col()
            .gap(self.z(theme.spacing.sm))
            .text_size(self.z(theme.typography.small()))
            .children(question.questions.iter().map(|q| {
                let answer = question
                    .answers
                    .iter()
                    .find(|a| a.question == q.text)
                    .map(|a| a.answer.clone());
                div()
                    .flex()
                    .flex_col()
                    .gap(self.z(theme.spacing.xxs))
                    .child(
                        div()
                            .text_color(hsla(s.text))
                            .whitespace_normal()
                            .child(SharedString::from(q.text.clone())),
                    )
                    .child(div().flex().flex_wrap().gap(self.z(theme.spacing.xs)).children(
                        q.options.iter().map(|option| {
                            let picked = answer
                                .as_deref()
                                .is_some_and(|a| a.split(", ").any(|p| p == option));
                            div()
                                .px(self.z(theme.spacing.sm))
                                .rounded(self.z(theme.radii.xs))
                                .border_1()
                                .border_color(hsla(if picked { s.accent } else { s.border_subtle }))
                                .text_color(hsla(if picked { s.text } else { s.text_secondary }))
                                .child(SharedString::from(option.clone()))
                        }),
                    ))
            }))
            .into_any_element()
    }

    /// Tasks as a list of marks and subjects; `quiet` sets them at the meta size.
    pub(super) fn task_lines(
        &self,
        tasks: &[slopty_proto::conversation::Task],
        quiet: bool,
    ) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let size = if quiet { theme.typography.meta() } else { theme.typography.small() };
        div()
            .flex()
            .flex_col()
            .gap(self.z(theme.spacing.xxs))
            .text_size(self.z(size))
            .children(tasks.iter().map(|task| {
                let (icon, tone, ink) = match task.status.as_str() {
                    "completed" => (IconName::CircleCheck, s.success, s.text_muted),
                    "in_progress" => (IconName::CircleDot, s.accent, s.text),
                    _ => (IconName::Circle, s.text_muted, s.text_secondary),
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
                    .items_center()
                    .gap(self.z(theme.spacing.sm))
                    .child(self.icon(icon, tone))
                    .child(
                        div()
                            .min_w_0()
                            .text_color(hsla(ink))
                            .when(task.status == "completed", gpui::Styled::line_through)
                            .child(SharedString::from(task.subject.clone())),
                    )
            }))
            .into_any_element()
    }

    /// An edit's diff: the whole of it at full, its first lines at summary with the way to
    /// the rest; side by side when the tile is wide.
    #[expect(
        clippy::too_many_arguments,
        reason = "a diff's place (thread, entry), its file and patch, its level, and the view"
    )]
    pub(super) fn patch_block(
        &self,
        thread: &ThreadId,
        id: &str,
        path: &str,
        patch: &Patch,
        level: Level,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        // The worker clipped it: once the whole diff is here, that is what shows.
        let whole = patch
            .full
            .as_ref()
            .and_then(|r| match self.model.expanded(thread, r)? {
                Expanded::Whole(whole) => Some(whole),
                Expanded::Gone => None,
            })
            .map(|c| parse_patch(&c.text));
        let patch = whole.as_ref().unwrap_or(patch);
        if patch.hunks.is_empty() {
            return None;
        }
        let blocks = self.diff_blocks(thread, id, path, patch);
        let theme = &self.theme;
        let s = theme.surfaces;
        let total: usize = blocks.iter().map(|b| b.lines.len()).sum();
        let budget = if level == Level::Full { usize::MAX } else { diff::SUMMARY_LINES };
        let split = self.width >= SPLIT_FROM;
        let digits = blocks
            .iter()
            .flat_map(|b| &b.lines)
            .filter_map(|l| l.new.max(l.old))
            .max()
            .map_or(2, |n| n.checked_ilog10().map_or(1, |d| d.saturating_add(1)).max(2));
        let digits = u8::try_from(digits).unwrap_or(u8::MAX);
        let mut left = budget;
        let mut children: Vec<AnyElement> = Vec::new();
        for (bx, block) in blocks.iter().enumerate() {
            if left == 0 {
                break;
            }
            if bx > 0 {
                children.push(self.hunk_divider(block));
            }
            if split {
                for (l, r) in diff::pairs(block).into_iter().take(left) {
                    children.push(self.split_line(l, r, digits));
                    left = left.saturating_sub(1);
                }
            } else {
                for line in block.lines.iter().take(left) {
                    children.push(self.unified_line(line, digits));
                    left = left.saturating_sub(1);
                }
            }
        }
        let shown = budget.min(total);
        let more = total.saturating_sub(shown);
        let key = rows::entry_key(id);
        let foot = (more > 0).then(|| {
            let label =
                SharedString::from(format!("{} more", tools::count(more as u64, "line", "lines")));
            let selector = format!("diff-more-{id}");
            crate::a11y::tab_stop(
                div()
                    .id(ElementId::Name(SharedString::from(selector.clone())))
                    .debug_selector(move || selector)
                    .role(Role::Button)
                    .aria_label(label.clone())
                    .px(self.z(theme.spacing.sm))
                    .py(self.z(theme.spacing.xxs))
                    .border_t_1()
                    .border_color(hsla(s.border_subtle))
                    .font_family(theme.typography.ui_family.clone())
                    .text_color(hsla(s.text_muted))
                    .cursor_pointer()
                    .hover(move |el| el.text_color(hsla(s.text)))
                    .child(label),
                s.accent,
            )
            .on_click(cx.listener(move |this, _ev, _w, cx| this.toggle(key, cx)))
        });
        let clipped = (level == Level::Full && whole.is_none())
            .then(|| {
                let full = patch.full.clone()?;
                let text = slopty_proto::conversation::Clipped {
                    text: String::new(),
                    lines: patch
                        .clipped_lines
                        .saturating_add(u32::try_from(total).unwrap_or(u32::MAX)),
                    chars: 0,
                    full: Some(full),
                };
                self.expand_link_in(thread, &format!("patch-{id}"), &text, cx)
            })
            .flatten()
            .map(|link| {
                div().px(self.z(theme.spacing.sm)).py(self.z(theme.spacing.xxs)).child(link)
            });
        Some(
            self.code_frame()
                .debug_selector({
                    let id = id.to_owned();
                    move || format!("diff-{id}")
                })
                .id(ElementId::Name(SharedString::from(format!("diff-{id}"))))
                .role(Role::Figure)
                .aria_label(SharedString::from(format!("Diff of {}", tools::file_name(path))))
                .child(self.diff_head(path, patch.added, patch.removed))
                .child(div().py(self.z(theme.spacing.xxs)).children(children))
                .children(foot)
                .children(clipped)
                .into_any_element(),
        )
    }

    /// A diff's head: where the file is, its name at the medium weight, its size at the right.
    fn diff_head(&self, path: &str, added: u32, removed: u32) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let dir = crate::conversation::figures::short_dir(path);
        div()
            .flex()
            .items_center()
            .h(self.z(theme.density.row))
            .px(self.z(theme.spacing.sm))
            .border_b_1()
            .border_color(hsla(s.border_subtle))
            .font_family(theme.typography.ui_family.clone())
            .text_size(self.z(theme.typography.small()))
            .when(!dir.is_empty(), |el| {
                el.child(
                    div()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .text_color(hsla(s.text_muted))
                        .child(SharedString::from(format!("{dir}/"))),
                )
            })
            .child(
                div()
                    .flex_none()
                    .text_color(hsla(s.text))
                    .font_weight(gpui::FontWeight(slopty_theme::Typography::MEDIUM_WEIGHT))
                    .child(SharedString::from(tools::file_name(path).to_owned())),
            )
            .child(div().flex_1())
            .child(
                div()
                    .text_size(self.z(theme.typography.meta()))
                    .child(self.changes_label(added, removed)),
            )
            .into_any_element()
    }

    fn hunk_divider(&self, block: &Block) -> AnyElement {
        let s = self.theme.surfaces;
        div()
            .px(self.z(self.theme.spacing.sm))
            .py(self.z(self.theme.spacing.xxs))
            .my(self.z(self.theme.spacing.xxs))
            .bg(hsla(s.canvas))
            .text_color(hsla(s.text_muted))
            .font_family(self.theme.typography.ui_family.clone())
            .child(SharedString::from(format!("Line {}", block.new_start)))
            .into_any_element()
    }

    /// A line's text in its grammar's colours (a context line muted).
    fn line_text(&self, line: &Line) -> AnyElement {
        let s = self.theme.surfaces;
        let (text, spans) = detab(&line.text, line.spans.as_deref());
        let text = if text.is_empty() { " ".to_owned() } else { text };
        let context = line.kind == Kind::Context;
        let ink = match line.kind {
            Kind::Context => s.text_secondary,
            Kind::Added | Kind::Removed => s.text,
        };
        let styled = match spans.filter(|sp| !sp.is_empty() && !context) {
            Some(spans) => {
                let font = gpui::font(self.mono());
                let runs = highlight::runs(text.len(), Some(&spans), &font, &self.theme);
                StyledText::new(SharedString::from(text)).with_runs(runs).into_any_element()
            }
            None => SharedString::from(text).into_any_element(),
        };
        let marker = line.no_newline.then(|| self.no_newline_mark());
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .items_baseline()
            .gap(self.z(self.theme.spacing.xs))
            .child(div().min_w_0().whitespace_normal().text_color(hsla(ink)).child(styled))
            .children(marker)
            .into_any_element()
    }

    /// The mark after a line the file ends on without a newline: the return icon struck
    /// through by a hairline, in the muted tone, named by its hint.
    fn no_newline_mark(&self) -> AnyElement {
        let theme = &self.theme;
        let hint_theme = std::rc::Rc::clone(&self.hint_theme);
        let muted = theme.surfaces.text_muted;
        div()
            .id("no-newline")
            .role(Role::Image)
            .aria_label("No newline at end of file")
            .relative()
            .flex_none()
            .flex()
            .items_center()
            .child(self.icon(IconName::CornerDownLeft, muted))
            .child(div().absolute().left_0().right_0().top_1_2().h(gpui::px(1.0)).bg(hsla(muted)))
            .tooltip(move |_window, cx| {
                let theme = std::rc::Rc::clone(&hint_theme);
                gpui::AppContext::new(cx, |_| {
                    crate::kit::Hint::new("No newline at end of file", "", theme)
                })
                .into()
            })
            .into_any_element()
    }

    /// The line's wash and its sign's tone.
    fn line_tone(&self, kind: Kind) -> (Option<gpui::Hsla>, &'static str, Rgb) {
        let s = self.theme.surfaces;
        match kind {
            Kind::Added => (Some(hsla_alpha(s.success_fill, alpha::FAINT)), "+", s.success),
            Kind::Removed => (Some(hsla_alpha(s.error_fill, alpha::FAINT)), "\u{2212}", s.error),
            Kind::Context => (None, " ", s.text_muted),
        }
    }

    fn number(&self, n: Option<u32>, digits: u8) -> Div {
        let s = self.theme.surfaces;
        let width = self.z(self.theme.typography.meta() * 0.62 * f32::from(digits));
        crate::kit::tabular(div())
            .flex_none()
            .w(width)
            .flex()
            .justify_end()
            .text_color(hsla(s.text_muted))
            .child(SharedString::from(n.map(|n| n.to_string()).unwrap_or_default()))
    }

    fn unified_line(&self, line: &Line, digits: u8) -> AnyElement {
        let (wash, sign, tone) = self.line_tone(line.kind);
        let theme = &self.theme;
        div()
            .w_full()
            .flex()
            .gap(self.z(theme.spacing.sm))
            .px(self.z(theme.spacing.sm))
            .when_some(wash, gpui::Styled::bg)
            .child(self.number(line.old, digits))
            .child(self.number(line.new, digits))
            .child(div().flex_none().text_color(hsla(tone)).child(sign))
            .child(self.line_text(line))
            .into_any_element()
    }

    fn split_line(&self, left: Option<&Line>, right: Option<&Line>, digits: u8) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let half = |line: Option<&Line>, old: bool| {
            let base = div()
                .flex_1()
                .min_w_0()
                .flex()
                .gap(self.z(theme.spacing.sm))
                .px(self.z(theme.spacing.sm));
            match line {
                Some(line) => {
                    let (wash, sign, tone) = self.line_tone(line.kind);
                    base.when_some(wash, gpui::Styled::bg)
                        .child(self.number(if old { line.old } else { line.new }, digits))
                        .child(div().flex_none().text_color(hsla(tone)).child(sign))
                        .child(self.line_text(line))
                }
                None => base.bg(hsla(s.canvas)),
            }
        };
        div()
            .w_full()
            .flex()
            .child(half(left, true))
            .child(div().flex_none().w(gpui::px(1.0)).bg(hsla(s.border_subtle)))
            .child(half(right, false))
            .into_any_element()
    }
}

/// Unified hunks as text (what [`slopty_proto::conversation::Part::Patch`] expands to) read
/// back into a patch.
#[must_use]
pub fn parse_patch(text: &str) -> Patch {
    let mut patch = Patch::default();
    for line in text.lines() {
        if let Some(header) = line.strip_prefix("@@") {
            let numbers = |sign: char| -> Option<(u32, u32)> {
                let part = header.split_whitespace().find(|p| p.starts_with(sign))?;
                let part = part.trim_start_matches(sign);
                let (start, count) = part.split_once(',').unwrap_or((part, "1"));
                Some((start.parse().ok()?, count.parse().ok()?))
            };
            let (old_start, old_lines) = numbers('-').unwrap_or((1, 0));
            let (new_start, new_lines) = numbers('+').unwrap_or((1, 0));
            patch.hunks.push(slopty_proto::conversation::Hunk {
                old_start,
                old_lines,
                new_start,
                new_lines,
                lines: Vec::new(),
            });
            continue;
        }
        if let Some(hunk) = patch.hunks.last_mut() {
            match line.chars().next() {
                Some('+') => patch.added = patch.added.saturating_add(1),
                Some('-') => patch.removed = patch.removed.saturating_add(1),
                _ => {}
            }
            hunk.lines.push(line.to_owned());
        }
    }
    patch
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An expanded diff reads back into the hunks it was written from.
    #[test]
    fn a_whole_diff_reads_back_into_hunks() {
        let patch =
            parse_patch("@@ -1,3 +1,3 @@\n alpha\n-beta\n+BETA\n gamma\n@@ -9 +9,2 @@\n x\n+y\n");
        assert_eq!(patch.hunks.len(), 2);
        assert_eq!((patch.hunks[0].old_start, patch.hunks[0].new_lines), (1, 3));
        assert_eq!(patch.hunks[0].lines, [" alpha", "-beta", "+BETA", " gamma"]);
        assert_eq!(
            (patch.hunks[1].old_start, patch.hunks[1].old_lines, patch.hunks[1].new_lines),
            (9, 1, 2)
        );
        assert_eq!((patch.added, patch.removed), (2, 1));
    }

    /// A tool's input reads as its keys and values in the order the model wrote them, a long
    /// string by its first line; input
    /// that is not an object (clipped mid-way) is left to the code frame.
    #[test]
    fn a_tools_input_reads_as_keys_and_values() {
        let pairs = input_pairs(r#"{"query": "rust gpui", "limit": 5, "body": "a\nb\nc"}"#);
        assert_eq!(
            pairs.as_deref(),
            Some(
                &[
                    ("query".to_owned(), "rust gpui".to_owned()),
                    ("limit".to_owned(), "5".to_owned()),
                    ("body".to_owned(), "a \u{2026} +2 lines".to_owned()),
                ][..]
            )
        );
        assert_eq!(input_pairs(r#"{"query": "rust"#), None);
        assert_eq!(input_pairs("[1, 2]"), None);
    }

    /// A tab widens to spaces, and the colours after it move with the text.
    #[test]
    fn tabs_widen_and_the_colours_follow() {
        let spans = [
            Span { len: 2, token: highlight::Token::Keyword, italic: false, bold: false },
            Span { len: 3, token: highlight::Token::String, italic: false, bold: false },
        ];
        let (text, spans) = detab("\tx\"a\"", Some(&spans));
        assert_eq!(text, "    x\"a\"");
        let lens: Vec<usize> = spans.unwrap().iter().map(|s| s.len).collect();
        assert_eq!(lens, [5, 3]);
        assert_eq!(lens.iter().sum::<usize>(), text.len());
    }
}
