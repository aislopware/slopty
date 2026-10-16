//! slopty: Slopty's own transcript rows, drawn in zeron's anatomy: an error that keeps what the
//! agent said behind its copy and can be tried again, a quiet divider no one wrote (a
//! compaction, a subagent's news), a proposed plan as a card folded past its first lines, what
//! a settled turn changed, and the working line's current step.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use gpui::{AnyElement, ClipboardItem, Context, SharedString, div, prelude::*, px};
use slopty_view_doc::{MessagePart, MessageStatus, SessionMessageEntry};
use slopty_view_proto::ToolCall;

use super::{Row, RowKind, Transcript, fnv1a};
use crate::markdown::parser::BlockTree;
use crate::notice::{NoticeChipIcon::Tile, notice_chip};
use crate::theme::Theme;

/// A plan shows this many source lines before it folds.
pub const PLAN_FOLD_LINES: usize = 20;

/// The longest step the working line shows, in characters.
const STEP_MAX_CHARS: usize = 64;

/// One file a turn changed, with its line counts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TurnFile {
    pub path: SharedString,
    pub additions: u64,
    pub deletions: u64,
}

/// What the rows of this module keep between frames.
#[derive(Default)]
pub(super) struct ExtrasState {
    /// Error cards whose detail is shown.
    details_open: HashSet<SharedString>,
    /// Turn-changes rows whose file list is shown.
    changes_open: HashSet<SharedString>,
    /// Error cards whose retry was asked for, until the thread moves past them.
    retried: HashSet<SharedString>,
    /// A selection's Quote: where the press ended, and the words.
    quote: Option<(gpui::Point<gpui::Pixels>, String)>,
}

/// A text part the view model marks as a proposed plan (`plan:<id>`).
pub fn is_plan_part(part_id: &str) -> bool {
    part_id.starts_with("plan:")
}

/// A row belongs to a plan's card.
pub fn is_plan_row(row: &Row) -> bool {
    row.id.contains("#plan:")
}

/// The card segment a plan row draws: whether it opens and whether it closes the card.
pub fn plan_segment(rows: &[Row], ix: usize) -> Option<(bool, bool)> {
    let row = rows.get(ix).filter(|row| is_plan_row(row))?;
    let same = |other: Option<&Row>| {
        other.is_some_and(|other| is_plan_row(other) && other.entry_id == row.entry_id)
    };
    let top = !same(ix.checked_sub(1).and_then(|i| rows.get(i)));
    let bottom = !same(rows.get(ix + 1));
    Some((top, bottom))
}

/// The line `offset` of `text` starts on, from 0.
fn line_at(text: &str, offset: usize) -> usize {
    text.as_bytes()
        .get(..offset.min(text.len()))
        .map_or(0, |head| head.iter().filter(|b| **b == b'\n').count())
}

/// The rows of a plan's text part `key` (`{entry}#plan:<id>`): a header, one row per block,
/// and past [`PLAN_FOLD_LINES`] the rest folded behind a "Show full plan" row.
pub fn plan_rows(
    rows: &mut Vec<Row>,
    entry: &SessionMessageEntry,
    key: &str,
    text: &str,
    tree: &Arc<BlockTree>,
    streaming: bool,
) {
    let entry_id: SharedString = entry.id.clone().into();
    let row = |id: String, version: u64, kind: RowKind| Row {
        id: id.into(),
        version,
        turn_start: false,
        kind,
        entry_id: entry_id.clone(),
        timestamp: None,
        copy_text: None,
        compact_fold: None,
    };
    rows.push(row(
        format!("{key}#head"),
        fnv1a(text.as_bytes()),
        RowKind::PlanHeader {
            copy: text.to_owned().into(),
        },
    ));
    let fold: SharedString = format!("{key}#fold").into();
    let total_lines = text.trim_end().lines().count();
    let mut folded = false;
    for (block_ix, top) in tree.blocks.iter().enumerate() {
        let end = top.range.end.min(text.len());
        let bytes = text
            .as_bytes()
            .get(top.range.start.min(end)..end)
            .unwrap_or_default();
        let version = (fnv1a(bytes) << 1) | streaming as u64;
        let kind = if streaming {
            RowKind::LiveMarkdown {
                tree: tree.clone(),
                block_ix,
            }
        } else {
            RowKind::Markdown {
                tree: tree.clone(),
                block_ix,
            }
        };
        let mut block = row(format!("{key}.{block_ix}"), version, kind);
        // A block that starts past the fold line hides with the fold; the first block
        // never does, however long.
        if block_ix > 0 && line_at(text, top.range.start) >= PLAN_FOLD_LINES {
            block.compact_fold = Some(fold.clone());
            folded = true;
        }
        rows.push(block);
    }
    if folded {
        let shown = tree
            .blocks
            .iter()
            .skip(1)
            .find(|top| line_at(text, top.range.start) >= PLAN_FOLD_LINES)
            .map_or(total_lines, |top| line_at(text, top.range.start));
        let hidden_lines = total_lines.saturating_sub(shown);
        rows.push(row(
            fold.to_string(),
            hidden_lines as u64,
            RowKind::PlanFold {
                fold: fold.clone(),
                hidden_lines,
            },
        ));
    }
}

/// An error part with the agent's own words or a retry, as a card.
pub fn error_card_row(
    entry_id: &str,
    part_id: &str,
    message: &str,
    detail: Option<&str>,
    retry: bool,
) -> Row {
    let message = slopty_view_proto::view::single_line(message);
    let version = fnv1a(format!("{message}\0{}\0{retry}", detail.unwrap_or_default()).as_bytes());
    Row {
        id: format!("{entry_id}#{part_id}").into(),
        version,
        turn_start: false,
        kind: RowKind::ErrorCard {
            message: message.into(),
            detail: detail.map(|detail| detail.trim().to_owned().into()),
            retry,
        },
        entry_id: entry_id.to_owned().into(),
        timestamp: None,
        copy_text: None,
        compact_fold: None,
    }
}

/// How the worker words a Claude Code too old to drive, after any words of what it refused.
const TOO_OLD: &str = "Update Claude Code on this machine to ";

/// The least version Slopty drives and the version the machine has, where `text` is the
/// worker's refusal of a Claude Code too old to drive: "… to 2.1.301 or later: it is 2.0.14".
#[must_use]
pub fn claude_too_old(text: &str) -> Option<(String, String)> {
    let (_, rest) = text.split_once(TOO_OLD)?;
    let (least, have) = rest.split_once(" or later: it is ")?;
    let have = have.lines().next().unwrap_or_default().trim().trim_end_matches('.');
    let least = least.trim();
    (!least.is_empty() && !have.is_empty()).then(|| (least.to_owned(), have.to_owned()))
}

/// [`claude_too_old`] of an error's words or of what the agent said behind them.
pub(super) fn too_old_of(message: &str, detail: Option<&str>) -> Option<(String, String)> {
    claude_too_old(message).or_else(|| detail.and_then(claude_too_old))
}

/// An error part refusing a Claude Code too old to drive, as its banner.
pub(super) fn update_needed_row(
    entry_id: &str,
    part_id: &str,
    message: &str,
    detail: Option<&str>,
) -> Option<Row> {
    let (least, have) = too_old_of(message, detail)?;
    Some(Row {
        id: format!("{entry_id}#{part_id}").into(),
        version: fnv1a(format!("{least}\0{have}").as_bytes()),
        turn_start: false,
        kind: RowKind::UpdateNeeded { least: least.into(), have: have.into() },
        entry_id: entry_id.to_owned().into(),
        timestamp: None,
        copy_text: None,
        compact_fold: None,
    })
}

/// The words of the update banner: what the machine `host` has and what Slopty drives.
pub(super) fn update_words(host: &str, least: &str, have: &str) -> String {
    format!("Claude Code on {host} is {have}. Slopty drives {least} or later: update it there, then send again.")
}

/// What updates Claude Code where it runs.
const UPDATE_COMMAND: &str = "claude update";

/// A divider part as its row.
pub fn marker_row(entry_id: &str, part_id: &str, text: &str) -> Row {
    let text = slopty_view_proto::view::single_line(text);
    Row {
        id: format!("{entry_id}#{part_id}").into(),
        version: fnv1a(text.as_bytes()),
        turn_start: false,
        kind: RowKind::Marker { text: text.into() },
        entry_id: entry_id.to_owned().into(),
        timestamp: None,
        copy_text: None,
        compact_fold: None,
    }
}

/// The files a turn's calls changed, summed by path in the order first touched.
pub fn turn_files(entry: &SessionMessageEntry) -> Vec<TurnFile> {
    let mut order: Vec<String> = Vec::new();
    let mut sums: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    let mut add = |path: &str, additions: u64, deletions: u64| {
        if path.is_empty() {
            return;
        }
        let sum = sums.entry(path.to_owned()).or_insert_with(|| {
            order.push(path.to_owned());
            (0, 0)
        });
        sum.0 += additions;
        sum.1 += deletions;
    };
    for part in &entry.parts {
        let MessagePart::Tool {
            is_error,
            diff,
            diff_stats,
            ..
        } = part
        else {
            continue;
        };
        if *is_error {
            continue;
        }
        if let Some(stats) = diff_stats {
            for stat in stats {
                add(&stat.path, stat.additions, stat.deletions);
            }
        } else if let Some(diff) = diff {
            let stat = slopty_view_doc::diff_stat(diff);
            add(&stat.path, stat.additions, stat.deletions);
        }
    }
    order
        .into_iter()
        .map(|path| {
            let (additions, deletions) = sums[&path];
            TurnFile {
                path: path.into(),
                additions,
                deletions,
            }
        })
        .collect()
}

/// slopty: a file's counts as words after its name (" +3 −1"), none when nothing was counted.
fn counted(additions: u64, deletions: u64) -> String {
    if additions + deletions == 0 {
        String::new()
    } else {
        format!(" +{additions} −{deletions}")
    }
}

/// The words of a turn-changes row: "3 files changed".
pub fn turn_changes_label(files: &[TurnFile]) -> String {
    match files.len() {
        1 => "1 file changed".to_owned(),
        n => format!("{n} files changed"),
    }
}

/// A settled answer that changed files ends with a row saying which.
pub fn turn_changes_row(entry: &SessionMessageEntry) -> Option<Row> {
    if entry.role != slopty_view_doc::MessageRole::Assistant {
        return None;
    }
    let files = turn_files(entry);
    if files.is_empty() {
        return None;
    }
    let mut key = String::new();
    for file in &files {
        key.push_str(&format!("{}\0{}\0{}\0", file.path, file.additions, file.deletions));
    }
    Some(Row {
        id: format!("{}#changes", entry.id).into(),
        version: fnv1a(key.as_bytes()),
        turn_start: false,
        kind: RowKind::TurnChanges {
            files: Arc::new(files),
        },
        entry_id: entry.id.clone().into(),
        timestamp: None,
        copy_text: None,
        compact_fold: None,
    })
}

/// A path shown under `root` when it lies there, as it is otherwise.
fn shown_path(path: &str, root: Option<&str>) -> String {
    root.and_then(|root| path.strip_prefix(root.trim_end_matches('/')))
        .and_then(|rest| rest.strip_prefix('/'))
        .filter(|rest| !rest.is_empty())
        .unwrap_or(path)
        .to_owned()
}

/// A path's last component.
fn file_name(path: &str) -> &str {
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or(path)
}

/// `text` clamped to [`STEP_MAX_CHARS`], on one line.
fn clamp(text: &str) -> String {
    let line = slopty_view_proto::view::single_line(text);
    if line.chars().count() <= STEP_MAX_CHARS {
        return line;
    }
    let mut out: String = line.chars().take(STEP_MAX_CHARS - 1).collect();
    out.push('…');
    out
}

/// What a call under way is doing, in the working line's words: "Editing main.rs".
pub fn step_words(call: &ToolCall) -> String {
    let words = match call {
        ToolCall::Exec { command } => format!("Running {command}"),
        ToolCall::ReadFile { path } => format!("Reading {}", file_name(path)),
        ToolCall::WriteFile { path, .. } => format!("Writing {}", file_name(path)),
        // slopty: a notebook's cell, in the TUI's words.
        ToolCall::EditFile { path, .. } if path.ends_with(".ipynb") => {
            format!("Editing notebook {}", file_name(path))
        }
        ToolCall::EditFile { path, .. } => format!("Editing {}", file_name(path)),
        ToolCall::ApplyPatch { path } => match path {
            Some(path) => format!("Patching {}", file_name(path)),
            None => "Patching files".to_owned(),
        },
        ToolCall::Search { pattern, .. } => format!("Searching for {pattern}"),
        ToolCall::Glob { pattern } => format!("Finding {pattern}"),
        ToolCall::WebFetch { url, .. } => {
            let host = url
                .split("://")
                .nth(1)
                .unwrap_or(url)
                .split('/')
                .next()
                .unwrap_or(url);
            format!("Fetching {host}")
        }
        ToolCall::WebSearch { query } => format!("Searching the web for {query}"),
        ToolCall::Todo { .. } => "Updating the plan".to_owned(),
        ToolCall::Mcp { server, tool, .. } => format!("Using {server} · {tool}"),
        ToolCall::Unknown { name, input } => match name.strip_prefix("Agent: ") {
            Some(description) => format!("Running an agent: {description}"),
            None if name == "Agent" => "Running an agent".to_owned(),
            // slopty: a monitor says what it watches, as the TUI's spinner does.
            None if name == "Monitor" => {
                match input.as_ref().and_then(|i| i.get("description")?.as_str()) {
                    Some(description) => format!("Monitoring: {description}"),
                    None => "Monitoring".to_owned(),
                }
            }
            // slopty: the advisor model reviewing the conversation, as the TUI's spinner says.
            None if name == "advisor" => "Advising".to_owned(),
            None => format!("Using {name}"),
        },
    };
    clamp(&words)
}

/// The working line's step for a streaming answer: the call under way, else the newest
/// thought's first line, else nothing (the flavour word stays).
pub fn working_step(entry: &SessionMessageEntry) -> Option<String> {
    if entry.status != Some(MessageStatus::Streaming) {
        return None;
    }
    let live_call = entry.parts.iter().rev().find_map(|part| match part {
        MessagePart::Tool {
            call,
            resolved: false,
            ..
        } => Some(call),
        _ => None,
    });
    if let Some(call) = live_call {
        return Some(step_words(call));
    }
    entry.parts.iter().rev().find_map(|part| match part {
        MessagePart::Reasoning { text, .. } => text
            .lines()
            .map(|line| line.trim().trim_matches(|c| c == '*' || c == '#').trim())
            .find(|line| !line.is_empty())
            .map(clamp),
        _ => None,
    })
}

type RunFn = dyn Fn(&str, &str, &mut gpui::Window, &mut gpui::App) -> bool;

/// How the shell runs a line in a chat's terminal, once its window is up.
#[derive(Clone)]
struct TerminalRuns(std::rc::Rc<RunFn>);

impl gpui::Global for TerminalRuns {}

/// Serve a fence's Run with `run`: chat, then the command line, typed into that chat's
/// terminal and run; whether the shell took it. Until it is served no fence offers Run.
pub fn serve_terminal_runs(
    cx: &mut gpui::App,
    run: impl Fn(&str, &str, &mut gpui::Window, &mut gpui::App) -> bool + 'static,
) {
    cx.set_global(TerminalRuns(std::rc::Rc::new(run)));
}

impl Transcript {
    /// A fence's Run for this transcript's chat, when the shell runs lines.
    pub(super) fn run_ui(
        &self,
        cx: &gpui::App,
    ) -> Option<std::rc::Rc<dyn Fn(String, &mut gpui::Window, &mut gpui::App)>> {
        if self.doc_override.is_some() {
            return None;
        }
        let chat = self.chat_id.clone()?;
        let TerminalRuns(run) = cx.try_global::<TerminalRuns>()?.clone();
        Some(std::rc::Rc::new(move |line, window, cx| {
            run(&chat, &line, window, cx);
        }))
    }

    /// Offer to quote the selection that a press ending at `at` left, in the main chat.
    pub(super) fn offer_quote(&mut self, at: gpui::Point<gpui::Pixels>, cx: &mut Context<Self>) {
        if self.doc_override.is_some() || self.chat_id.is_none() {
            return;
        }
        let words = crate::markdown::selection::selected_text()
            .map(|text| text.trim().to_owned())
            .filter(|text| !text.is_empty());
        if let Some(words) = words {
            self.extras.quote = Some((at, words));
            cx.notify();
        }
    }

    /// Take the Quote away.
    pub(super) fn withdraw_quote(&mut self, cx: &mut Context<Self>) {
        if self.extras.quote.take().is_some() {
            cx.notify();
        }
    }

    /// The Quote beside a selection's end, while the selection stands.
    pub(super) fn render_quote_offer(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (at, words) = self.extras.quote.clone()?;
        let standing = crate::markdown::selection::selected_text()
            .is_some_and(|text| text.trim() == words);
        if !standing {
            self.extras.quote = None;
            return None;
        }
        let theme = Theme::of(cx).clone();
        let button = div()
            .id("quote-selection")
            .role(gpui::Role::Button)
            .aria_label("Quote")
            .h(px(28.0))
            .px(px(10.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .rounded_full()
            .border_1()
            .border_color(theme.border)
            // slopty: the faded hover.
            .bg(crate::motion::mix(
                crate::popover::surface_bg(&theme),
                theme.text,
                0.08 * crate::motion::hover_t("quote-selection"),
            ))
            .on_hover(crate::motion::hover_listener("quote-selection"))
            .shadow_md()
            .cursor_pointer()
            .text_size(crate::typography::ui_rems(12.5))
            .text_color(theme.text)
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _, window, cx| {
                cx.stop_propagation();
                if crate::composer::insert_quote(&words, window, cx) {
                    this.extras.quote = None;
                    cx.notify();
                }
            }))
            .child(
                crate::icons::icon(crate::icons::CHAT_ROUND_LINE)
                    .size(px(13.0))
                    .text_color(theme.text_muted),
            )
            .child(SharedString::from("Quote"));
        Some(
            gpui::deferred(
                gpui::anchored()
                    .position(at + gpui::point(px(8.0), px(10.0)))
                    .snap_to_window_with_margin(px(8.0))
                    .child(button),
            )
            .priority(1)
            .into_any_element(),
        )
    }

    /// Ask the agent to run the failed turn again with the person's own words.
    fn retry_turn(&mut self, row_id: SharedString, cx: &mut Context<Self>) {
        let Some(chat_id) = self.chat_id.clone() else {
            return;
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        self.extras.retried.insert(row_id);
        cx.notify();
        cx.spawn(async move |_, _| {
            let params = serde_json::json!({ "chatId": chat_id });
            if let Err(err) = engine
                .client()
                .call(slopty_view_rpc::methods::RETRY_TURN, params)
                .await
            {
                tracing::warn!(error = %err, "turn retry failed");
            }
        })
        .detach();
    }

    /// Open or close a plan's fold at once (no tween: the rows below move as one).
    fn toggle_plan_fold(&mut self, fold: SharedString, cx: &mut Context<Self>) {
        let entry = self.folds.entry(fold).or_default();
        entry.open = Some(!entry.open.unwrap_or(false));
        entry.toggled_at = None;
        entry.epoch += 1;
        self.last_source = None;
        self.sync(cx);
        cx.notify();
    }

    fn copy_extra(&mut self, row_id: SharedString, text: SharedString, cx: &mut Context<Self>) {
        cx.stop_propagation();
        cx.write_to_clipboard(ClipboardItem::new_string(text.to_string()));
        // slopty: the house's copy span: the check comes back off on its own.
        crate::motion::copy_feedback(format!("copy-plan-{row_id}"), cx);
        cx.notify();
    }

    /// The row a [`RowKind`] of this module draws.
    pub(super) fn render_extra_row(
        &mut self,
        row: &Row,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match &row.kind {
            RowKind::ErrorCard {
                message,
                detail,
                retry,
            } => self.render_error_card(row, message, detail.as_ref(), *retry, theme, cx),
            RowKind::UpdateNeeded { least, have } => self.render_update_needed(row, least, have, theme, cx),
            RowKind::Marker { text } => marker(text.clone(), theme),
            RowKind::PlanHeader { copy } => self.render_plan_header(row, copy, theme, cx),
            RowKind::PlanFold { fold, hidden_lines } => {
                self.render_plan_fold(fold, *hidden_lines, theme, cx)
            }
            RowKind::TurnChanges { files } => self.render_turn_changes(row, files, theme, cx),
            _ => gpui::Empty.into_any_element(),
        }
    }

    fn render_error_card(
        &mut self,
        row: &Row,
        message: &SharedString,
        detail: Option<&SharedString>,
        retry: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let open = self.extras.details_open.contains(&row.id);
        let retried = self.extras.retried.contains(&row.id);
        let quiet = |id: String, label: &'static str| {
            let fade = format!("{id}-hover");
            div()
                .id(SharedString::from(id))
                .role(gpui::Role::Button)
                .aria_label(label)
                .h(px(24.0))
                .px(px(8.0))
                .rounded(px(6.0))
                .flex()
                .items_center()
                .cursor_pointer()
                // slopty: a tab stop with the shared ring.
                .tab_index(0)
                .map(|el| crate::surface_chrome::focus_ring(el, theme))
                .text_size(crate::typography::ui_rems(12.0))
                // slopty: the faded hover, at the control strength.
                .text_color(crate::motion::mix(theme.text_muted, theme.text, crate::motion::hover_t(&fade)))
                .bg(crate::motion::hover_blend(&fade, gpui::transparent_black(), crate::theme::wash(0.11)))
                .on_hover(crate::motion::hover_listener(fade))
                .child(SharedString::from(label))
        };
        let mut actions = div().flex().flex_row().items_center().gap(px(2.0)).ml(px(-8.0));
        if retry {
            let id = row.id.clone();
            actions = actions.child(if retried {
                div()
                    .h(px(24.0))
                    .px(px(8.0))
                    .flex()
                    .items_center()
                    .text_size(crate::typography::ui_rems(12.0))
                    .text_color(theme.text_faint)
                    .child(SharedString::from("Trying again…"))
                    .into_any_element()
            } else {
                quiet(format!("{}-retry", row.id), "Try again")
                    .on_click(cx.listener(move |this, _, _, cx| this.retry_turn(id.clone(), cx)))
                    .into_any_element()
            });
        }
        if detail.is_some() {
            let id = row.id.clone();
            actions = actions.child(
                quiet(
                    format!("{}-details", row.id),
                    if open { "Hide details" } else { "View details" },
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    if !this.extras.details_open.remove(&id) {
                        this.extras.details_open.insert(id.clone());
                    }
                    cx.notify();
                })),
            );
        }
        // slopty: the error's words and its detail, to paste where help is asked (T51).
        let copy_key = format!("{}-copy-error", row.id);
        let copied = crate::motion::copied(&copy_key, cx);
        let copy_text = match detail {
            Some(detail) => format!("{message}\n\n{detail}"),
            None => message.to_string(),
        };
        actions = actions.child(
            quiet(copy_key.clone(), if copied { "Copied" } else { "Copy error" }).on_click(move |_, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(copy_text.clone()));
                crate::motion::copy_feedback(copy_key.clone(), cx);
            }),
        );
        // slopty: a long detail scrolls in its box rather than being cut (T51).
        let detail_block = detail.filter(|_| open).map(|detail| {
            div()
                .id(SharedString::from(format!("{}-detail", row.id)))
                .w_full()
                .max_h(px(240.0))
                .overflow_y_scroll()
                .rounded(px(8.0))
                .bg(crate::theme::ink(0.04))
                .px(px(10.0))
                .py(px(8.0))
                .font_family(theme.font_mono.clone())
                .text_size(px(11.5))
                .line_height(px(17.0))
                .text_color(theme.text_muted)
                .child(detail.clone())
        });
        div()
            .id(SharedString::from(format!("{}-error", row.id)))
            .role(gpui::Role::Alert)
            .aria_label(message.clone())
            .py(px(4.0))
            .w_full()
            .child(
                notice_chip(theme, false, "Error", message.clone(), Tile)
                    .overflow_hidden()
                    .w_full()
                    .children(detail_block)
                    .child(actions),
            )
            .into_any_element()
    }

    /// The name of the machine the chat runs on, else "this machine".
    fn chat_host(&self, cx: &gpui::App) -> String {
        let device = self.image_host(cx).0;
        let state = self.state.read(cx);
        device
            .and_then(|id| state.devices.iter().find(|d| d.id == id))
            .map_or_else(|| "this machine".to_owned(), |d| d.name.clone())
    }

    /// A Claude Code too old to drive: a banner naming its machine and both versions, with the
    /// command that updates it to copy.
    fn render_update_needed(
        &mut self,
        row: &Row,
        least: &SharedString,
        have: &SharedString,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let words = update_words(&self.chat_host(cx), least, have);
        let key = format!("{}-update-copy", row.id);
        let copied = crate::motion::copied(&key, cx);
        let glyph = crate::motion::copy_icon(&key, cx);
        let label = if copied { "Copied" } else { "Copy update command" };
        let fade = format!("{key}-hover");
        let copy = div()
            .id(SharedString::from(key.clone()))
            .role(gpui::Role::Button)
            .aria_label(label)
            .h(px(24.0))
            .px(px(8.0))
            .rounded(px(6.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .cursor_pointer()
            // slopty: a tab stop with the shared ring.
            .tab_index(0)
            .map(|el| crate::surface_chrome::focus_ring(el, theme))
            .text_size(crate::typography::ui_rems(12.0))
            .text_color(crate::motion::mix(theme.text_muted, theme.text, crate::motion::hover_t(&fade)))
            .bg(crate::motion::hover_blend(&fade, gpui::transparent_black(), crate::theme::wash(0.11)))
            .on_hover(crate::motion::hover_listener(fade))
            .on_click(move |_, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(UPDATE_COMMAND.to_owned()));
                crate::motion::copy_feedback(key.clone(), cx);
            })
            .child(crate::icons::icon(glyph).size(px(12.0)))
            .child(SharedString::from(label));
        div()
            .id(SharedString::from(format!("{}-update", row.id)))
            .role(gpui::Role::Alert)
            .aria_label(SharedString::from(words.clone()))
            .py(px(4.0))
            .w_full()
            .child(
                notice_chip(theme, true, "Update needed", words, Tile)
                    .overflow_hidden()
                    .w_full()
                    .child(div().flex().flex_row().items_center().ml(px(-8.0)).child(copy)),
            )
            .into_any_element()
    }

    fn render_plan_header(
        &mut self,
        row: &Row,
        copy: &SharedString,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let copied = crate::motion::copied(&format!("copy-plan-{}", row.id), cx);
        let plan_copy_hover = SharedString::from(format!("{}-copy-hover", row.id));
        let (id, text) = (row.id.clone(), copy.clone());
        // slopty: a plan the person sent to be carried out is headed as the TUI boxes it.
        let title = if row.id.contains("#plan:implement:") { "Plan to implement" } else { "Plan" };
        div()
            .id(SharedString::from(format!("{}-heading", row.id)))
            .role(gpui::Role::Heading)
            .aria_label(title)
            .w_full()
            .h(px(28.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .child(
                crate::icons::icon(crate::icons::CHECKLIST)
                    .size(px(14.0))
                    .text_color(theme.text_muted),
            )
            .child(
                div()
                    .text_size(crate::typography::ui_rems(13.0))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(theme.text)
                    .child(SharedString::from(title)),
            )
            .child(div().flex_1())
            .child(
                div()
                    .id(SharedString::from(format!("{}-copy", row.id)))
                    .role(gpui::Role::Button)
                    .aria_label("Copy plan")
                    .size(px(24.0))
                    .rounded(px(Theme::CONTROL_RADIUS))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    // slopty: a tab stop with the shared ring.
                    .tab_index(0)
                    .map(|el| crate::surface_chrome::focus_ring(el, theme))
                    .bg(crate::motion::hover_blend(&plan_copy_hover, gpui::transparent_black(), crate::theme::ink(0.08)))
                    .on_hover(crate::motion::hover_listener(plan_copy_hover.clone()))
                    .tooltip(crate::settings::widgets::text_tooltip(if copied { "Copied" } else { "Copy plan" }))
                    .tooltip_show_delay(crate::settings::widgets::TOOLTIP_DELAY)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.copy_extra(id.clone(), text.clone(), cx)
                    }))
                    .child(
                        crate::icons::icon(if copied {
                            crate::icons::CHECK
                        } else {
                            crate::icons::COPY
                        })
                        .size(px(14.0))
                        .text_color(theme.text_muted),
                    ),
            )
            .into_any_element()
    }

    fn render_plan_fold(
        &mut self,
        fold: &SharedString,
        hidden_lines: usize,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let open = self
            .folds
            .get(fold)
            .is_some_and(|state| state.open == Some(true));
        let label: SharedString = if open {
            "Show less".into()
        } else {
            match hidden_lines {
                0 | 1 => "Show full plan".into(),
                n => format!("Show full plan · {n} more lines").into(),
            }
        };
        let fold = fold.clone();
        div()
            .w_full()
            .flex()
            .justify_center()
            .child(
                div()
                    .id(SharedString::from(format!("{fold}-toggle")))
                    .role(gpui::Role::Button)
                    .aria_label(label.clone())
                    .aria_expanded(open)
                    .h(px(28.0))
                    .px(px(10.0))
                    .rounded(px(Theme::CONTROL_RADIUS))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(6.0))
                    .cursor_pointer()
                    // slopty: a tab stop with the shared ring.
                    .tab_index(0)
                    .map(|el| crate::surface_chrome::focus_ring(el, theme))
                    .text_size(crate::typography::ui_rems(12.0))
                    // slopty: the faded hover, at the control strength.
                    .text_color(crate::motion::mix(
                        theme.text_muted,
                        theme.text,
                        crate::motion::hover_t(&format!("{fold}-toggle")),
                    ))
                    .bg(crate::motion::hover_blend(
                        &format!("{fold}-toggle"),
                        gpui::transparent_black(),
                        crate::theme::wash(0.11),
                    ))
                    .on_hover(crate::motion::hover_listener(format!("{fold}-toggle")))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.toggle_plan_fold(fold.clone(), cx)
                    }))
                    .child(label)
                    .child(
                        crate::icons::icon(if open {
                            crate::icons::ALT_ARROW_UP
                        } else {
                            crate::icons::ALT_ARROW_DOWN
                        })
                        .size(px(12.0))
                        .text_color(theme.text_muted),
                    ),
            )
            .into_any_element()
    }

    fn render_turn_changes(
        &mut self,
        row: &Row,
        files: &Arc<Vec<TurnFile>>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let open = self.extras.changes_open.contains(&row.id);
        let root = {
            let state = self.state.read(cx);
            self.chat_id
                .as_deref()
                .and_then(|chat_id| state.chats.iter().find(|chat| chat.id == chat_id))
                .and_then(|chat| chat.cwd.clone())
        };
        let (additions, deletions) = files
            .iter()
            .fold((0, 0), |(a, d), file| (a + file.additions, d + file.deletions));
        // slopty: a file whose lines were never counted says nothing, not "+0 −0".
        let counts = |additions: u64, deletions: u64| {
            (additions + deletions > 0).then(|| div()
                .flex()
                .flex_row()
                .gap(px(6.0))
                .font_family(theme.font_mono.clone())
                .text_size(px(11.5))
                .child(
                    div()
                        .text_color(theme.diff_add)
                        .child(SharedString::from(format!("+{additions}"))),
                )
                .child(
                    div()
                        .text_color(theme.diff_del)
                        .child(SharedString::from(format!("−{deletions}"))),
                ))
        };
        let toggle_id = row.id.clone();
        let entry_id = row.entry_id.clone();
        let header = div()
            .id(SharedString::from(format!("{}-toggle", row.id)))
            .role(gpui::Role::Button)
            .aria_label(SharedString::from(format!(
                "{}{}",
                turn_changes_label(files),
                counted(additions, deletions)
            )))
            .aria_expanded(open)
            .w_full()
            .h(px(32.0))
            .px(px(10.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .cursor_pointer()
            // slopty: the faded hover, at the row strength.
            .bg(crate::motion::hover_blend(
                &format!("{}-toggle", row.id),
                gpui::transparent_black(),
                crate::theme::ink(0.05),
            ))
            .on_hover(crate::motion::hover_listener(format!("{}-toggle", row.id)))
            .on_click(cx.listener(move |this, _, _, cx| {
                if !this.extras.changes_open.remove(&toggle_id) {
                    this.extras.changes_open.insert(toggle_id.clone());
                }
                cx.notify();
            }))
            .child(
                crate::icons::icon(crate::icons::DOCUMENT)
                    .size(px(14.0))
                    .text_color(theme.text_muted),
            )
            .child(
                div()
                    .text_size(crate::typography::ui_rems(12.5))
                    .text_color(theme.text)
                    .child(SharedString::from(turn_changes_label(files))),
            )
            .children(counts(additions, deletions))
            .child(div().flex_1())
            .child(
                div()
                    .id(SharedString::from(format!("{}-open-diff", row.id)))
                    .role(gpui::Role::Button)
                    .aria_label("Open diff")
                    .h(px(22.0))
                    .px(px(8.0))
                    .rounded(px(6.0))
                    .flex()
                    .items_center()
                    .text_size(crate::typography::ui_rems(12.0))
                    // slopty: the faded hover, at the control strength.
                    .text_color(crate::motion::mix(
                        theme.text_muted,
                        theme.text,
                        crate::motion::hover_t(&format!("{}-open-diff", row.id)),
                    ))
                    .bg(crate::motion::hover_blend(
                        &format!("{}-open-diff", row.id),
                        gpui::transparent_black(),
                        crate::theme::wash(0.11),
                    ))
                    .on_hover(crate::motion::hover_listener(format!("{}-open-diff", row.id)))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.open_turn_diff(entry_id.clone(), window, cx);
                    }))
                    .child(SharedString::from("Open diff")),
            )
            .child(
                crate::icons::icon(if open {
                    crate::icons::ALT_ARROW_UP
                } else {
                    crate::icons::ALT_ARROW_DOWN
                })
                .size(px(12.0))
                .text_color(theme.text_muted),
            );
        let list = open.then(|| {
            div()
                .w_full()
                .border_t_1()
                .border_color(theme.border)
                .py(px(4.0))
                .children(files.iter().map(|file| {
                    let path = shown_path(&file.path, root.as_deref());
                    div()
                        .id(SharedString::from(format!("{}-file-{path}", row.id)))
                        .role(gpui::Role::ListItem)
                        .aria_label(SharedString::from(format!(
                            "{path}{}",
                            counted(file.additions, file.deletions)
                        )))
                        .w_full()
                        .h(px(26.0))
                        .px(px(10.0))
                        .pl(px(32.0))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(8.0))
                        .child(
                            div()
                                .min_w_0()
                                .flex_1()
                                .truncate()
                                .text_size(crate::typography::ui_rems(12.0))
                                .text_color(theme.text_muted)
                                .child(SharedString::from(path)),
                        )
                        .children(counts(file.additions, file.deletions))
                }))
        });
        div()
            .py(px(4.0))
            .w_full()
            .child(
                div()
                    .w_full()
                    .rounded(px(super::TRANSCRIPT_CARD_RADIUS))
                    .border_1()
                    .border_color(theme.border)
                    .bg(crate::theme::ink(0.02))
                    .overflow_hidden()
                    .child(header)
                    .children(list),
            )
            .into_any_element()
    }

    /// Open Changes at the turn `entry_id` answered.
    fn open_turn_diff(
        &mut self,
        entry_id: SharedString,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        crate::changes::show_turn(&entry_id, window, cx);
    }

    /// `inner` dressed as its segment of a plan's card, with the row's gap moved inside the
    /// card so the card reads as one surface; other rows pass through.
    pub(super) fn plan_card_segment(
        &self,
        ix: usize,
        inner: AnyElement,
        top_gap: f32,
        theme: &Theme,
    ) -> (AnyElement, f32) {
        let Some((top, bottom)) = plan_segment(&self.rows, ix) else {
            return (inner, top_gap);
        };
        let radius = px(12.0);
        let segment = div()
            .w_full()
            .min_w_0()
            .bg(crate::theme::ink(0.025))
            .border_l_1()
            .border_r_1()
            .border_color(theme.border)
            .px(px(16.0))
            .when(top, |el| el.border_t_1().rounded_t(radius).pt(px(8.0)))
            .when(!top, |el| el.pt(px(top_gap)))
            .when(bottom, |el| el.border_b_1().rounded_b(radius).pb(px(12.0)))
            .child(inner)
            .into_any_element();
        (segment, if top { top_gap } else { 0.0 })
    }
}

thread_local! {
    /// Highlighted command lines by their text, a bounded few.
    static COMMANDS: std::cell::RefCell<std::collections::HashMap<String, Arc<Vec<slopty_syntax::HighlightSpan>>>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

/// The most command lines kept highlighted.
const COMMANDS_KEPT: usize = 512;

/// A command line's bash tokens, highlighted once.
fn command_spans(line: &str) -> Arc<Vec<slopty_syntax::HighlightSpan>> {
    COMMANDS.with(|commands| {
        if let Some(spans) = commands.borrow().get(line) {
            return spans.clone();
        }
        let spans = Arc::new(
            slopty_syntax::highlight(slopty_syntax::HighlightRequest {
                source: line,
                path: None,
                fence_tag: Some("bash"),
            })
            .ok()
            .and_then(|document| document.lines.into_iter().next())
            .unwrap_or_default(),
        );
        let mut commands = commands.borrow_mut();
        if commands.len() >= COMMANDS_KEPT {
            commands.clear();
        }
        commands.insert(line.to_owned(), spans.clone());
        spans
    })
}

/// An exec call's command, its lines highlighted as bash, selectable like any detail line.
pub(super) fn command_body(
    detail: &super::ToolDetail,
    selection_key: &str,
    theme: &Theme,
) -> Option<AnyElement> {
    let super::ToolDetail::Output {
        lines,
        truncated_by,
        ..
    } = detail
    else {
        return None;
    };
    let mono = gpui::font(theme.font_mono.clone());
    Some(
        div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .overflow_hidden()
            .py(px(6.0))
            .font_family(theme.font_mono.clone())
            .text_size(px(super::TOOL_TEXT_SIZE))
            .children(lines.iter().enumerate().map(|(line_ix, line)| {
                let row = div()
                    .h(px(super::OUTPUT_LINE_HEIGHT))
                    .w_full()
                    .min_w_0()
                    .flex()
                    .items_center();
                if line.is_empty() {
                    return row;
                }
                let spans = command_spans(line);
                let runs = crate::markdown::render::runs_for_syntax_line_with_plain(
                    line,
                    &spans,
                    &mono,
                    theme.text_muted,
                    theme,
                );
                row.child(
                    div()
                        .w_full()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .child(crate::markdown::render::selectable_text_element(
                            format!("{selection_key}-{line_ix}").into(),
                            line.clone(),
                            runs,
                            theme.selection,
                        )),
                )
            }))
            .when(*truncated_by > 0, |block| {
                block.child(super::more_lines_row(*truncated_by, theme))
            })
            .into_any_element(),
    )
}

/// A quiet one-line divider: a hairline, the words, a hairline.
fn marker(text: SharedString, theme: &Theme) -> AnyElement {
    let rule = || div().flex_1().min_w_0().h(px(1.0)).bg(theme.border);
    div()
        .id(SharedString::from(format!("marker-{text}")))
        .role(gpui::Role::Status)
        .aria_label(text.clone())
        .py(px(10.0))
        .w_full()
        .min_w_0()
        .flex()
        .items_center()
        .gap(px(10.0))
        .child(rule())
        .child(
            div()
                .flex_none()
                .max_w(px(420.0))
                .truncate()
                .text_size(crate::typography::ui_rems(12.0))
                .text_color(theme.text_muted.opacity(0.75))
                .child(text),
        )
        .child(rule())
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use slopty_view_doc::{MessageRole, ToolDiffStat};

    fn entry(parts: Vec<MessagePart>, status: MessageStatus) -> SessionMessageEntry {
        SessionMessageEntry {
            id: "a1".into(),
            role: MessageRole::Assistant,
            parts,
            created_at: 1,
            device_id: "d".into(),
            status: Some(status),
            continuation_of: None,
            duration_ms: None,
        }
    }

    fn edit(id: &str, path: &str, additions: u64, deletions: u64, resolved: bool) -> MessagePart {
        MessagePart::Tool {
            id: id.into(),
            call: ToolCall::EditFile {
                path: path.into(),
                old_string: None,
                new_string: None,
            },
            is_error: false,
            resolved,
            output: None,
            diff: None,
            output_ref: None,
            output_bytes: None,
            diff_ref: None,
            diff_stats: Some(vec![ToolDiffStat {
                path: path.into(),
                additions,
                deletions,
            }]),
            subagent_ref: None,
            subagent_status: None,
            subagent_tail: None,
            facts: None,
        }
    }

    #[test]
    fn a_claude_code_too_old_is_a_banner_naming_both_versions() {
        let said = "Update Claude Code on this machine to 2.1.301 or later: it is 2.0.14";
        assert_eq!(claude_too_old(said), Some(("2.1.301".to_owned(), "2.0.14".to_owned())));
        // A refused start keeps the worker's words after its own.
        let start = format!("It did not start: {said}");
        assert_eq!(claude_too_old(&start), Some(("2.1.301".to_owned(), "2.0.14".to_owned())));
        assert_eq!(too_old_of("The turn failed. Try again.", Some(said)).map(|v| v.1), Some("2.0.14".to_owned()));
        for other in ["The turn failed. Try again.", "Update Claude Code on this machine to 2.1.301", ""] {
            assert_eq!(claude_too_old(other), None, "{other}");
        }
        let turn = entry(
            vec![MessagePart::Error { id: "refused:1".into(), message: start, detail: None, retry: false }],
            MessageStatus::Complete,
        );
        let rows = super::super::rows_for_entry(&turn, false, false, &mut |_, text| {
            Arc::new(crate::markdown::parser::parse_full(text))
        });
        assert!(
            rows.iter().any(|row| matches!(&row.kind, RowKind::UpdateNeeded { least, have } if least == "2.1.301" && have == "2.0.14")),
            "a banner, not a raw error"
        );
        assert!(!rows.iter().any(|row| matches!(row.kind, RowKind::ErrorChip { .. } | RowKind::ErrorCard { .. })));
        assert_eq!(
            update_words("studio", "2.1.301", "2.0.14"),
            "Claude Code on studio is 2.0.14. Slopty drives 2.1.301 or later: update it there, then send again."
        );
    }

    #[test]
    fn a_turn_sums_its_changes_by_file_in_the_order_touched() {
        let turn = entry(
            vec![
                edit("t1", "/w/src/b.rs", 3, 1, true),
                edit("t2", "/w/src/a.rs", 1, 0, true),
                edit("t3", "/w/src/b.rs", 2, 2, true),
            ],
            MessageStatus::Complete,
        );
        let files = turn_files(&turn);
        assert_eq!(
            files,
            [
                TurnFile {
                    path: "/w/src/b.rs".into(),
                    additions: 5,
                    deletions: 3
                },
                TurnFile {
                    path: "/w/src/a.rs".into(),
                    additions: 1,
                    deletions: 0
                },
            ]
        );
        assert_eq!(turn_changes_label(&files), "2 files changed");
        let row = turn_changes_row(&turn).expect("a row");
        assert_eq!(row.id.as_ref(), "a1#changes");
        assert_eq!(shown_path("/w/src/a.rs", Some("/w/")), "src/a.rs");
        assert_eq!(shown_path("/elsewhere/a.rs", Some("/w")), "/elsewhere/a.rs");
        // slopty: an uncounted file says no "+0 −0".
        assert_eq!(counted(0, 0), "");
        assert_eq!(counted(3, 0), " +3 −0");
    }

    #[test]
    fn the_working_line_names_the_call_under_way_then_the_thought() {
        let live = entry(
            vec![
                MessagePart::Reasoning {
                    id: "r".into(),
                    text: "**Checking the layout**\nmore".into(),
                    thought_ms: None,
                },
                edit("t1", "/w/src/main.rs", 1, 0, false),
            ],
            MessageStatus::Streaming,
        );
        assert_eq!(working_step(&live).as_deref(), Some("Editing main.rs"));
        let thinking = entry(
            vec![
                edit("t1", "/w/src/main.rs", 1, 0, true),
                MessagePart::Reasoning {
                    id: "r".into(),
                    text: "\n**Checking the layout**\nmore".into(),
                    thought_ms: None,
                },
            ],
            MessageStatus::Streaming,
        );
        assert_eq!(working_step(&thinking).as_deref(), Some("Checking the layout"));
        let settled = entry(vec![edit("t1", "/a", 1, 0, false)], MessageStatus::Complete);
        assert_eq!(working_step(&settled), None);
        let long = ToolCall::Exec {
            command: "x".repeat(200),
        };
        assert_eq!(step_words(&long).chars().count(), STEP_MAX_CHARS);
    }

    #[test]
    fn a_long_plan_folds_past_its_first_lines() {
        let text: String = (0..30).map(|i| format!("Step {i}.\n\n")).collect();
        let tree = Arc::new(crate::markdown::parser::parse_full(&text));
        let plan = SessionMessageEntry {
            id: "plan-card:t".into(),
            role: MessageRole::Assistant,
            parts: vec![MessagePart::Text {
                id: "plan:t".into(),
                text: text.clone(),
            }],
            created_at: 1,
            device_id: "d".into(),
            status: Some(MessageStatus::Complete),
            continuation_of: None,
            duration_ms: None,
        };
        let mut rows = Vec::new();
        plan_rows(&mut rows, &plan, "plan-card:t#plan:t", &text, &tree, false);
        assert!(matches!(rows[0].kind, RowKind::PlanHeader { .. }));
        let shown = rows.iter().filter(|row| matches!(row.kind, RowKind::Markdown { .. }) && row.compact_fold.is_none()).count();
        assert_eq!(shown, 10, "20 source lines are 10 one-line paragraphs");
        let RowKind::PlanFold { hidden_lines, .. } = rows.last().unwrap().kind else {
            panic!("a fold row ends a long plan");
        };
        assert_eq!(hidden_lines, 39);
        assert!(rows.iter().all(is_plan_row));
        assert_eq!(plan_segment(&rows, 0), Some((true, false)));
        assert_eq!(plan_segment(&rows, rows.len() - 1), Some((false, true)));

        let short = "One step.\n";
        let tree = Arc::new(crate::markdown::parser::parse_full(short));
        let mut rows = Vec::new();
        plan_rows(&mut rows, &plan, "plan-card:t#plan:t", short, &tree, false);
        assert_eq!(rows.len(), 2, "a short plan has no fold");
    }
}
