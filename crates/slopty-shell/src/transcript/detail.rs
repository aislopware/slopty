//! What an open step's detail holds past its output's plain lines (`inspector.md`, "Any step
//! opens to its whole detail").
//!
//! - Its output in the format the person keeps for its kind: text as the TUI shows it, JSON as a
//!   tree, markdown rendered. The menu always offers all three, so it never changes shape; one that
//!   cannot show this output is disabled and says why.
//! - A running command's output in a capped box that follows its own bottom until the person
//!   scrolls up in it, so the trace stays still while the output grows (Amp's).
//! - Its exit code on a line of its own.
//! - Two sections fetched when opened: the text the model read where the step shows something else
//!   ("Details for Claude", Claude Code desktop's), and its raw record, as a tree.
#![allow(
    clippy::cast_precision_loss,
    clippy::float_cmp_const,
    clippy::format_collect,
    clippy::match_same_arms,
    clippy::needless_pass_by_ref_mut,
    clippy::option_option,
    clippy::unused_self,
    reason = "shell lint migration: slice B"
)]

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use gpui::prelude::*;
use gpui::{AnyElement, App, Context, SharedString, TextRun, div, px};
use slopty_view_proto::ToolCall;

use super::json_tree::{self, Tone};
use super::{
    BLOB_AFFORDANCE_HEIGHT, BlobFetch, DETAIL_SEPARATOR, FULL_OUTPUT_MAX_LINES, OUTPUT_BODY_PAD,
    OUTPUT_LINE_HEIGHT, TOOL_TEXT_SIZE, ToolDetail, ToolItem, Transcript,
};
use crate::theme::Theme;

/// Lines a running command's box shows before it scrolls: 300 px with its padding, Amp's cap.
const BOX_LINES: usize = 16;

/// Height of a running command's box at its cap.
const BOX_HEIGHT: f32 = BOX_LINES as f32 * OUTPUT_LINE_HEIGHT + OUTPUT_BODY_PAD;

/// How near its bottom a box still counts as following it.
const BOX_PINNED: f32 = 10.0;

/// The indent of each level of a JSON tree.
const JSON_INDENT: f32 = 14.0;

/// Width of a JSON line's fold mark.
const JSON_MARK: f32 = 14.0;

/// Texts whose forms are kept at once; past them all are let go and made again when drawn.
const FORMS_KEPT: usize = 64;

/// How a step's output reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OutputFormat {
    /// As the TUI shows it: its lines, a line of JSON pretty-printed.
    Text,
    /// As a tree whose values fold.
    Json,
    /// Rendered.
    Markdown,
}

impl OutputFormat {
    const ALL: [Self; 3] = [Self::Text, Self::Json, Self::Markdown];

    /// Its name in the settings.
    const fn name(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Json => "json",
            Self::Markdown => "markdown",
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Text => "Text",
            Self::Json => "JSON",
            Self::Markdown => "Markdown",
        }
    }

    fn named(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|format| format.name() == name)
    }
}

/// The kind a step's format is kept for, by its tool's name as Claude Code calls it: one MCP
/// server's tool apart from another's, since each answers in its own shape.
pub(super) fn format_kind(call: &ToolCall) -> String {
    match call {
        ToolCall::Exec { .. } => "Bash".to_owned(),
        ToolCall::ReadFile { .. } => "Read".to_owned(),
        ToolCall::WriteFile { .. } => "Write".to_owned(),
        ToolCall::EditFile { .. } => "Edit".to_owned(),
        ToolCall::ApplyPatch { .. } => "Patch".to_owned(),
        ToolCall::Search { .. } => "Grep".to_owned(),
        ToolCall::Glob { .. } => "Glob".to_owned(),
        ToolCall::WebFetch { .. } => "WebFetch".to_owned(),
        ToolCall::WebSearch { .. } => "WebSearch".to_owned(),
        ToolCall::Todo { .. } => "TodoWrite".to_owned(),
        ToolCall::Mcp { server, tool, .. } => format!("mcp__{server}__{tool}"),
        ToolCall::Unknown { name, .. } => name.split(':').next().unwrap_or(name).trim().to_owned(),
    }
}

/// What the transcript keeps for open steps' details.
#[derive(Default)]
pub(super) struct DetailState {
    /// Texts fetched for a detail, by blob: an output's whole, what the model read, a raw record.
    texts: HashMap<SharedString, Arc<str>>,
    /// Each text's forms, by the text's allocation, which the entry holds so it is not reused.
    forms: HashMap<usize, Forms>,
    /// The JSON values the person folded or opened, by step and section.
    turned: HashMap<SharedString, HashSet<usize>>,
    /// Each running command's box, which keeps its scroll.
    boxes: HashMap<SharedString, gpui::ScrollHandle>,
    /// The fetched sections the person opened, by step and section.
    open: HashSet<SharedString>,
}

/// A text's forms, made when first drawn.
struct Forms {
    _text: Arc<str>,
    json: Option<Option<Arc<json_tree::Doc>>>,
    markdown: Option<Arc<ToolDetail>>,
}

impl DetailState {
    /// Keep `text`, fetched as `blob`.
    pub(super) fn fetched(&mut self, blob: SharedString, text: &str) {
        self.texts.insert(blob, Arc::from(text));
    }

    fn forms(&mut self, text: &Arc<str>) -> &mut Forms {
        if self.forms.len() >= FORMS_KEPT {
            self.forms.clear();
        }
        let at = Arc::as_ptr(text).cast::<u8>().addr();
        self.forms.entry(at).or_insert_with(|| Forms {
            _text: Arc::clone(text),
            json: None,
            markdown: None,
        })
    }

    fn json(&mut self, text: &Arc<str>) -> Option<Arc<json_tree::Doc>> {
        let forms = self.forms(text);
        forms.json.get_or_insert_with(|| json_tree::Doc::parse(text).map(Arc::new)).clone()
    }

    fn markdown(&mut self, text: &Arc<str>) -> Arc<ToolDetail> {
        let forms = self.forms(text);
        Arc::clone(forms.markdown.get_or_insert_with(|| {
            let tree = crate::markdown::parser::parse_full(text);
            let mut lines = super::thought_lines(&tree);
            let truncated_by = lines.len().saturating_sub(FULL_OUTPUT_MAX_LINES);
            lines.truncate(FULL_OUTPUT_MAX_LINES);
            Arc::new(ToolDetail::Thought { lines, truncated_by })
        }))
    }

    /// Document `doc` laid out with the folds the person changed in section `key`.
    fn tree(&self, key: &SharedString, doc: &json_tree::Doc) -> ToolDetail {
        let empty = HashSet::new();
        let turned = self.turned.get(key).unwrap_or(&empty);
        let (lines, truncated_by) = doc.lines(turned, FULL_OUTPUT_MAX_LINES);
        ToolDetail::Json { lines: Arc::new(lines), truncated_by, key: key.clone() }
    }
}

/// What an open step shows of its output.
pub(super) struct StepOutput {
    /// The body drawn: its output in its format, or zeron's body as it is.
    pub body: Option<Arc<ToolDetail>>,
    /// The format menu, for an output that is text.
    pub menu: Option<FormatMenu>,
    /// It still grows: the body scrolls in a capped box that follows its bottom.
    pub boxed: bool,
    /// Its standard error, drawn apart under a body that is not text (text holds it).
    pub stderr: Option<Arc<ToolDetail>>,
}

/// A step's format menu.
pub(super) struct FormatMenu {
    kind: String,
    shown: OutputFormat,
    /// JSON can show it: it parses, or it is cut and its whole may.
    json: bool,
    /// The whole to fetch first, where what is held is cut.
    whole: Option<SharedString>,
}

impl StepOutput {
    /// The height it adds to its step's detail.
    pub(super) fn height(&self) -> f32 {
        let menu = if self.menu.is_some() { BLOB_AFFORDANCE_HEIGHT } else { 0.0 };
        let body = self.body.as_deref().map_or(0.0, |body| {
            let height = super::detail_height(body);
            if self.boxed { height.min(DETAIL_SEPARATOR + BOX_HEIGHT) } else { height }
        });
        menu + body + self.stderr.as_deref().map_or(0.0, super::detail_height)
    }
}

/// A section of a step's detail fetched when opened.
#[derive(Clone)]
pub(super) struct Section {
    blob: SharedString,
    /// Its key among the step's sections: its folds and whether it is open.
    key: SharedString,
    name: &'static str,
    lines: Option<u32>,
    state: SectionState,
}

#[derive(Clone)]
enum SectionState {
    Closed,
    Loading,
    Failed(SharedString),
    Open(Arc<ToolDetail>),
}

impl Section {
    /// The height it adds: its line, and its body while open.
    pub(super) fn height(&self) -> f32 {
        let body = match &self.state {
            SectionState::Open(body) => super::detail_height(body),
            _ => 0.0,
        };
        BLOB_AFFORDANCE_HEIGHT + body
    }
}

/// Every line of `text`, a running command's output, its standard error after it: the box
/// scrolls, so nothing is cut but past the box's own ceiling, from the top.
fn growing_detail(text: &str, stderr: Option<&str>) -> Option<ToolDetail> {
    let mut lines = super::output_lines(text);
    while lines.last().is_some_and(|line| line.trim().is_empty()) {
        lines.pop();
    }
    let from = lines.len().saturating_sub(FULL_OUTPUT_MAX_LINES);
    lines.drain(..from);
    let detail = (!lines.is_empty()).then_some(ToolDetail::Output {
        lines,
        truncated_by: 0,
        stderr_from: None,
    });
    super::with_stderr(detail, stderr)
}

impl Transcript {
    /// What open step `tool`, its detail at `key`, shows of its output: `shown` is the detail
    /// zeron's body would draw, the summary's or the whole fetched as blob `whole`.
    pub(super) fn step_output(
        &mut self,
        tool: &ToolItem,
        key: &SharedString,
        shown: Option<Arc<ToolDetail>>,
        whole: Option<&SharedString>,
        cx: &App,
    ) -> StepOutput {
        let facts = tool.facts.as_deref();
        let growing = facts.is_some_and(|facts| facts.growing);
        let stderr = facts.and_then(|facts| facts.stderr.as_deref());
        let is_text = matches!(shown.as_deref(), Some(ToolDetail::Output { .. }));
        let text = match whole {
            Some(blob) => self.step_detail.texts.get(blob).cloned(),
            None => tool.output_text.clone(),
        }
        .filter(|_| is_text);
        let Some(text) = text else {
            return StepOutput {
                boxed: growing && shown.is_some(),
                body: shown,
                menu: None,
                stderr: None,
            };
        };
        let kind = format_kind(&tool.call);
        let json = self.step_detail.json(&text);
        let cut = (whole.is_none() && !growing).then(|| tool.output_ref.clone()).flatten();
        let format = match crate::settings::output_format(&kind, cx)
            .as_deref()
            .and_then(OutputFormat::named)
        {
            Some(OutputFormat::Json) if json.is_some() => OutputFormat::Json,
            Some(OutputFormat::Markdown) => OutputFormat::Markdown,
            Some(_) => OutputFormat::Text,
            None if json.is_some() => OutputFormat::Json,
            None => OutputFormat::Text,
        };
        let out_key = SharedString::from(format!("{key}-out"));
        let body = match (format, &json) {
            (OutputFormat::Json, Some(doc)) => Some(Arc::new(self.step_detail.tree(&out_key, doc))),
            (OutputFormat::Markdown, _) => Some(self.step_detail.markdown(&text)),
            _ if growing => growing_detail(&text, stderr).map(Arc::new),
            _ => shown,
        };
        // Text holds its standard error after its lines; another format draws it apart.
        let apart = format != OutputFormat::Text;
        StepOutput {
            body,
            menu: Some(FormatMenu {
                kind,
                shown: format,
                json: json.is_some() || cut.is_some(),
                whole: cut,
            }),
            boxed: growing,
            stderr: super::with_stderr(None, stderr.filter(|_| apart)).map(Arc::new),
        }
    }

    /// The sections of open step `tool`, its detail at `key`, fetched when opened.
    pub(super) fn step_sections(&mut self, tool: &ToolItem, key: &SharedString) -> Vec<Section> {
        let Some(facts) = tool.facts.as_deref() else { return Vec::new() };
        let seen = facts
            .seen
            .as_ref()
            .map(|seen| ("Details for Claude", seen.blob.as_str(), Some(seen.lines), "seen"));
        let raw = facts.raw.as_deref().map(|raw| ("Raw record", raw, None, "raw"));
        [seen, raw]
            .into_iter()
            .flatten()
            .map(|(name, blob, lines, part)| {
                let blob = SharedString::from(blob.to_owned());
                let key = SharedString::from(format!("{key}-{part}"));
                let state = match self.blob_details.get(&blob) {
                    Some(BlobFetch::Loading(_)) => SectionState::Loading,
                    Some(BlobFetch::Failed(why)) => SectionState::Failed(why.clone()),
                    Some(BlobFetch::Ready(detail)) if self.step_detail.open.contains(&key) => {
                        let doc = (part == "raw")
                            .then(|| self.step_detail.texts.get(&blob).cloned())
                            .flatten()
                            .and_then(|text| self.step_detail.json(&text));
                        SectionState::Open(match doc {
                            Some(doc) => Arc::new(self.step_detail.tree(&key, &doc)),
                            None => Arc::clone(detail),
                        })
                    }
                    _ => SectionState::Closed,
                };
                Section { blob, key, name, lines, state }
            })
            .collect()
    }

    /// A step of kind `kind` is opening: where the person reads its kind in a format that needs
    /// its whole output and only a cut of it is here (`whole` is where the rest is), fetch it.
    pub(super) fn step_opening(
        &mut self,
        kind: &str,
        whole: Option<SharedString>,
        cx: &mut Context<Self>,
    ) {
        let Some(whole) = whole else { return };
        let format = crate::settings::output_format(kind, cx);
        let wants_whole = format
            .as_deref()
            .and_then(OutputFormat::named)
            .is_some_and(|f| f != OutputFormat::Text);
        if wants_whole && !self.blob_details.contains_key(&whole) {
            self.spawn_blob_fetch(whole, cx);
        }
    }

    /// The format menu of step `key`: "Output", then each format, the one shown marked.
    pub(super) fn format_menu(
        &self,
        key: &SharedString,
        menu: &FormatMenu,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .id(SharedString::from(format!("{key}-formats")))
            .h(px(BLOB_AFFORDANCE_HEIGHT))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(10.0))
            .text_size(px(TOOL_TEXT_SIZE))
            .role(gpui::Role::TabList)
            .aria_label("Output format")
            .child(div().flex_none().text_color(theme.text_muted).child("Output"))
            .child(div().flex_1())
            .children(OutputFormat::ALL.into_iter().map(|format| {
                let shown = format == menu.shown;
                let can = format != OutputFormat::Json || menu.json;
                let item = div()
                    .id(SharedString::from(format!("{key}-format-{}", format.name())))
                    .flex_none()
                    .role(gpui::Role::Tab)
                    .aria_label(format.label())
                    .aria_selected(shown)
                    .aria_disabled(!can)
                    .text_color(if shown { theme.text } else { theme.text_faint })
                    .when(shown, |item| item.font_weight(gpui::FontWeight::MEDIUM))
                    .child(format.label());
                if !can {
                    return item
                        .tooltip(crate::settings::widgets::text_tooltip("Not JSON"))
                        .into_any_element();
                }
                if shown {
                    return item.into_any_element();
                }
                let kind = menu.kind.clone();
                let whole = menu.whole.clone().filter(|_| format != OutputFormat::Text);
                crate::surface_chrome::key_stop(item, theme)
                    .rounded(px(3.0))
                    .cursor_pointer()
                    .hover(|item| item.text_color(theme.text_muted))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        crate::settings::set_output_format(&kind, format.name(), cx);
                        if let Some(whole) =
                            whole.clone().filter(|whole| !this.blob_details.contains_key(whole))
                        {
                            this.spawn_blob_fetch(whole, cx);
                        }
                        cx.notify();
                    }))
                    .into_any_element()
            }))
            .into_any_element()
    }

    /// A step's output body `body`, in a box that follows its bottom while `boxed`.
    pub(super) fn output_body(
        &mut self,
        key: &SharedString,
        body: &ToolDetail,
        boxed: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let element = self.detail_element(body, &format!("{key}-out"), theme, cx);
        if !boxed {
            return element;
        }
        let handle = self.step_detail.boxes.entry(key.clone()).or_default().clone();
        // Following while the person left it at its bottom, as of the last frame: the person
        // scrolling up in it stops the following, back at its bottom starts it again.
        let offset = -f32::from(handle.offset().y);
        if offset >= f32::from(handle.max_offset().y) - BOX_PINNED {
            handle.scroll_to_bottom();
        }
        let height =
            super::detail_height(body).min(DETAIL_SEPARATOR + BOX_HEIGHT) - DETAIL_SEPARATOR;
        div()
            .id(SharedString::from(format!("{key}-box")))
            .h(px(height))
            .flex_none()
            .min_w_0()
            .overflow_y_scroll()
            .track_scroll(&handle)
            .child(element)
            .into_any_element()
    }

    /// `detail`'s body: a tree with its folds, else zeron's body.
    pub(super) fn detail_element(
        &self,
        detail: &ToolDetail,
        selection_key: &str,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match detail {
            ToolDetail::Json { lines, truncated_by, key } => {
                self.json_body(key, lines, *truncated_by, theme, cx)
            }
            other => super::detail_body(other, None, selection_key, theme),
        }
    }

    /// A JSON tree's lines: a container's line folds or opens it.
    fn json_body(
        &self,
        key: &SharedString,
        lines: &[json_tree::Line],
        more: usize,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .id(SharedString::from(format!("{key}-tree")))
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .overflow_hidden()
            .py(px(6.0))
            .font_family(theme.font_mono.clone())
            .text_size(px(TOOL_TEXT_SIZE))
            .role(gpui::Role::Tree)
            .children(lines.iter().enumerate().map(|(ix, line)| {
                let runs: Vec<TextRun> = line
                    .runs
                    .iter()
                    .map(|(len, tone)| TextRun {
                        len: *len,
                        font: gpui::font(theme.font_mono.clone()),
                        color: tone_colour(*tone, theme),
                        background_color: None,
                        underline: None,
                        strikethrough: None,
                    })
                    .collect();
                let depth = f32::from(line.depth);
                let mark = div().w(px(JSON_MARK)).flex_none().flex().items_center().when_some(
                    line.fold,
                    |mark, open| {
                        mark.child(
                            crate::icons::icon(crate::icons::ALT_ARROW_DOWN)
                                .size(px(11.0))
                                .text_color(theme.text_faint)
                                .with_transformation(gpui::Transformation::rotate(gpui::radians(
                                    if open { 0.0 } else { -std::f32::consts::FRAC_PI_2 },
                                ))),
                        )
                    },
                );
                let row = div()
                    .id(SharedString::from(format!("{key}-j{ix}")))
                    .h(px(OUTPUT_LINE_HEIGHT))
                    .w_full()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .pl(px(depth * JSON_INDENT))
                    .role(gpui::Role::TreeItem)
                    .aria_label(SharedString::from(line.text.clone()))
                    .child(mark)
                    .child(div().min_w_0().overflow_hidden().child(
                        crate::markdown::render::selectable_text_element(
                            format!("{key}-j{ix}").into(),
                            SharedString::from(line.text.clone()),
                            runs,
                            theme.selection,
                        ),
                    ));
                let Some(open) = line.fold else { return row.into_any_element() };
                let (turn, value) = (key.clone(), line.value);
                row.aria_expanded(open)
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        let turned = this.step_detail.turned.entry(turn.clone()).or_default();
                        if !turned.remove(&value) {
                            turned.insert(value);
                        }
                        cx.notify();
                    }))
                    .into_any_element()
            }))
            .when(more > 0, |tree| tree.child(super::more_lines_row(more, theme)))
            .into_any_element()
    }

    /// Step `key`'s exit code on its line: in the failure's colour where it failed.
    pub(super) fn exit_line(
        key: &SharedString,
        exit: slopty_view_doc::ExitCode,
        theme: &Theme,
    ) -> AnyElement {
        let failed = exit.code != 0 && !exit.quiet;
        div()
            .id(SharedString::from(format!("{key}-exit")))
            .h(px(BLOB_AFFORDANCE_HEIGHT))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(6.0))
            .text_size(px(TOOL_TEXT_SIZE))
            .role(gpui::Role::Paragraph)
            .aria_label(SharedString::from(format!("Exit code {}", exit.code)))
            .child(div().flex_none().text_color(theme.text_muted).child("Exit code"))
            .child(
                div()
                    .font_family(theme.font_mono.clone())
                    .text_color(if failed { theme.danger } else { theme.text_faint })
                    .child(SharedString::from(exit.code.to_string())),
            )
            .into_any_element()
    }

    /// A fetched section's line, its body under it while open: a press fetches it the first
    /// time, then opens and shuts it.
    pub(super) fn section(
        &self,
        section: &Section,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let open = matches!(section.state, SectionState::Open(_));
        let words: SharedString = match &section.state {
            SectionState::Loading => format!("Loading {}…", section.name.to_lowercase()).into(),
            SectionState::Failed(why) => {
                format!("Couldn't load {}: {why}", section.name.to_lowercase()).into()
            }
            _ => match section.lines {
                Some(lines) => format!(
                    "{} · {}",
                    section.name,
                    super::counted(lines as usize, "line", "lines")
                )
                .into(),
                None => section.name.into(),
            },
        };
        let retry = matches!(section.state, SectionState::Failed(_));
        let (blob, key) = (section.blob.clone(), section.key.clone());
        let mut line =
            div()
                .id(SharedString::from(format!("{}-line", section.key)))
                .h(px(BLOB_AFFORDANCE_HEIGHT))
                .flex_none()
                .flex()
                .items_center()
                .gap(px(6.0))
                .text_size(px(TOOL_TEXT_SIZE))
                .text_color(theme.text_faint)
                .child(
                    crate::icons::icon(crate::icons::ALT_ARROW_DOWN)
                        .size(px(12.0))
                        .text_color(theme.text_faint)
                        .with_transformation(gpui::Transformation::rotate(gpui::radians(
                            if open { 0.0 } else { -std::f32::consts::FRAC_PI_2 },
                        ))),
                )
                .child(div().min_w_0().truncate().child(words.clone()))
                .when(retry, |line| {
                    line.child(div().flex_none().text_color(theme.text_muted).child("Retry"))
                });
        if !matches!(section.state, SectionState::Loading) {
            line = crate::surface_chrome::key_stop(line, theme)
                .role(gpui::Role::Button)
                .aria_label(if retry {
                    SharedString::from(format!("Retry loading {}", section.name.to_lowercase()))
                } else {
                    words
                })
                .aria_expanded(open)
                .rounded(px(4.0))
                .cursor_pointer()
                .hover(|line| line.text_color(theme.text_muted))
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    if this.step_detail.open.remove(&key)
                        && matches!(this.blob_details.get(&blob), Some(BlobFetch::Ready(_)))
                    {
                        cx.notify();
                        return;
                    }
                    this.step_detail.open.insert(key.clone());
                    if !matches!(this.blob_details.get(&blob), Some(BlobFetch::Ready(_))) {
                        this.spawn_blob_fetch(blob.clone(), cx);
                    }
                    cx.notify();
                }));
        }
        let body = match &section.state {
            SectionState::Open(body) => {
                Some(self.detail_element(body, &format!("{}-body", section.key), theme, cx))
            }
            _ => None,
        };
        div()
            .flex_none()
            .min_w_0()
            .flex()
            .flex_col()
            .child(line)
            .when_some(body, |section, body| {
                section.child(div().h(px(DETAIL_SEPARATOR)).flex_none()).child(body)
            })
            .into_any_element()
    }
}

/// How a run of a JSON line is coloured, in the theme's syntax hues.
const fn tone_colour(tone: Tone, theme: &Theme) -> gpui::Hsla {
    match tone {
        Tone::Key => theme.syntax.property,
        Tone::Punctuation | Tone::Quiet => theme.text_faint,
        Tone::Text => theme.syntax.string,
        Tone::Number => theme.syntax.number,
        Tone::Literal => theme.syntax.boolean,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A kind's format is kept by the tool's name, one MCP tool apart from another.
    #[test]
    fn formats_are_kept_by_the_tools_name() {
        let mcp =
            |tool: &str| ToolCall::Mcp { server: "github".into(), tool: tool.into(), input: None };
        assert_eq!(format_kind(&ToolCall::Exec { command: "ls".into() }), "Bash");
        assert_eq!(format_kind(&mcp("get_issue")), "mcp__github__get_issue");
        assert_ne!(format_kind(&mcp("get_issue")), format_kind(&mcp("list_issues")));
        assert_eq!(
            format_kind(&ToolCall::Unknown { name: "Agent: Count".into(), input: None }),
            "Agent"
        );
        for format in OutputFormat::ALL {
            assert_eq!(OutputFormat::named(format.name()), Some(format));
        }
        assert_eq!(
            OutputFormat::named("yaml"),
            None,
            "a name this build does not know reads as none"
        );
    }

    /// A running command's box holds every line of its tail and its standard error after it:
    /// it scrolls, so nothing is cut at the head as a finished output's is.
    #[test]
    fn a_running_box_keeps_the_whole_tail() {
        let text: String = (1..=40).map(|n| format!("line {n}\n")).collect();
        let Some(ToolDetail::Output { lines, truncated_by, stderr_from }) =
            growing_detail(&text, Some("warn"))
        else {
            panic!("an output");
        };
        assert_eq!((lines.len(), truncated_by, stderr_from), (41, 0, Some(40)));
        assert_eq!(lines.first().map(AsRef::as_ref), Some("line 1"));
        let boxed = StepOutput {
            body: Some(Arc::new(ToolDetail::Output { lines, truncated_by, stderr_from })),
            menu: None,
            boxed: true,
            stderr: None,
        };
        assert_eq!(
            boxed.height(),
            DETAIL_SEPARATOR + BOX_HEIGHT,
            "capped, so the trace holds still"
        );
    }
}
