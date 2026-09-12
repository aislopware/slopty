//! The conversation view: a Claude Code session's transcript read like a chat, shown in
//! place of the grid, with a composer to answer from.
//!
//! The host tails the agent's transcript file and streams [`TranscriptEntry`]s; the view keeps
//! them in a bottom-aligned [`gpui::list`] in tail-follow mode (a chat log: new entries append
//! at the bottom and the view stays there unless the reader scrolled up, when a "↓ latest"
//! pill offers the way back). Long parts (thinking, a tool's input, a tool's result past its
//! first lines) start folded and open on a click. Under the list sits the composer, a
//! multi-line input whose ↩ types the text into the session followed by Enter, and above it
//! the attention row: Allow / Deny when the agent waits for a permission.
//!
//! A driven session (the host speaks Claude Code's protocol) adds a header over the list with
//! what the agent said about itself: the model as a chip that opens a menu of the others, the
//! permission mode as a chip that cycles through the modes, the turn count and the cost. Its
//! composer completes the slash commands the agent announced: a `/` prefix lists the matches
//! above the field, Tab takes the selected one, ↑/↓ move, Esc hides the list.

use std::collections::HashSet;
use std::rc::Rc;

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, Context, ElementId, Entity, FocusHandle, Focusable as _,
    FollowMode, InteractiveElement as _, IntoElement, ListAlignment, ListState, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Window, div, list,
    px,
};
use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use gpui_kit::component::text::{TextView, TextViewStyle};
use slopty_proto::agent::{
    AgentInfo, AgentTask, Clipped, DiffKind, DiffLine, NoticeLevel, Question, SlashCommand, Todo,
    TodoStatus, ToolDetail, TranscriptBody, TranscriptEntry, TranscriptUpdate,
};
use slopty_theme::{Theme, alpha};

use crate::a11y::tab_stop;
use crate::colors::{hsla, hsla_alpha};
use crate::terminal::view::TerminalView;

/// Lines of a tool result shown before it is opened.
pub const RESULT_PREVIEW_LINES: usize = 4;
/// Lines of an edit's diff shown before it is opened: a diff is the point of the entry, so
/// it shows unasked, but a long one folds past this.
pub const DIFF_PREVIEW_LINES: usize = 12;
/// The composer grows with its text up to this many rows, then scrolls.
const COMPOSER_MAX_ROWS: usize = 6;
/// The models the chip offers, as Claude Code's `--model` aliases with their names.
pub const MODELS: [(&str, &str); 4] =
    [("fable", "Fable 5.1"), ("opus", "Opus 5"), ("sonnet", "Sonnet 5"), ("haiku", "Haiku 4.5")];
/// The permission modes the chip cycles through, in order.
pub const MODES: [&str; 3] = ["default", "acceptEdits", "plan"];

/// The slash commands that complete `text`.
///
/// Every command `text` is a prefix of, in the agent's order and at most [`SLASH_MAX`] of
/// them, when `text` is one word starting with `/`; nothing once `text` is exactly the only
/// match (there is nothing left to complete).
#[must_use]
pub fn slash_matches(text: &str, commands: &[SlashCommand]) -> Vec<SlashCommand> {
    if !text.starts_with('/') || text.contains(char::is_whitespace) {
        return Vec::new();
    }
    let needle = text.to_ascii_lowercase();
    let matches: Vec<SlashCommand> = commands
        .iter()
        .filter(|c| c.name.to_ascii_lowercase().starts_with(&needle))
        .take(SLASH_MAX)
        .cloned()
        .collect();
    match matches.as_slice() {
        [only] if only.name.eq_ignore_ascii_case(text) => Vec::new(),
        _ => matches,
    }
}

/// How many completions the list shows at once: a prefix narrows it fast, and a longer list
/// would push the transcript off the top.
pub const SLASH_MAX: usize = 8;

/// A completion as one line reads it (its a11y label): the name, the argument hint after
/// it, the description after a dash — each only where the agent gave one.
#[must_use]
pub fn slash_label(command: &SlashCommand) -> String {
    let mut label = command.name.clone();
    if !command.hint.is_empty() {
        label.push(' ');
        label.push_str(&command.hint);
    }
    if !command.description.is_empty() {
        label.push_str(" — ");
        label.push_str(&command.description);
    }
    label
}

/// A model as the chip shows it: an alias by its name, a full name without the `claude-`.
#[must_use]
pub fn model_label(model: &str) -> String {
    MODELS.iter().find(|(alias, _name)| *alias == model).map_or_else(
        || model.strip_prefix("claude-").unwrap_or(model).to_owned(),
        |(_a, name)| (*name).to_owned(),
    )
}

/// A permission mode as the chip shows it.
#[must_use]
pub fn mode_label(mode: &str) -> &str {
    match mode {
        "default" => "Ask",
        "acceptEdits" => "Accept edits",
        "plan" => "Plan",
        "bypassPermissions" => "Bypass",
        "dontAsk" => "Don't ask",
        other => other,
    }
}

/// The mode after `mode` in [`MODES`] (the first after an unknown one).
#[must_use]
pub fn next_mode(mode: &str) -> &'static str {
    MODES
        .iter()
        .position(|m| *m == mode)
        .and_then(|i| MODES.get(i.wrapping_add(1)))
        .copied()
        .unwrap_or(MODES[0])
}

/// "1 picture" / "3 pictures": how a bubble and its label say what went with a prompt.
#[must_use]
pub fn pictures_label(images: u32) -> String {
    if images == 1 { "1 picture".to_owned() } else { format!("{images} pictures") }
}

/// How an attachment chip names a picture waiting in the composer: its kind and size
/// ("PNG · 12 KB").
#[must_use]
pub fn attachment_label(image: &slopty_proto::agent::Image) -> String {
    let kind = image.media_type.rsplit('/').next().unwrap_or("image").to_ascii_uppercase();
    let bytes = image.data.len();
    let size = if bytes >= 1024 * 1024 {
        let tenths = bytes.saturating_mul(10) / (1024 * 1024);
        format!("{}.{} MB", tenths / 10, tenths % 10)
    } else if bytes >= 1024 {
        format!("{} KB", bytes / 1024)
    } else {
        format!("{bytes} B")
    };
    format!("{kind} · {size}")
}

/// The context chip: "ctx 16%" once the window is known, the token count ("ctx 31k")
/// before a turn result names it.
#[must_use]
pub fn context_label(context: &slopty_proto::agent::Context) -> String {
    match context.percent() {
        Some(percent) => format!("ctx {percent}%"),
        None => format!("ctx {}", tokens_label(context.tokens)),
    }
}

/// A token count in short form: "900", "31k", "1.2M".
fn tokens_label(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        let tenths = tokens.saturating_add(50_000).checked_div(100_000).unwrap_or(0);
        format!("{}.{}M", tenths.checked_div(10).unwrap_or(0), tenths.checked_rem(10).unwrap_or(0))
    } else if tokens >= 1_000 {
        format!("{}k", tokens.saturating_add(500).checked_div(1_000).unwrap_or(0))
    } else {
        tokens.to_string()
    }
}

/// The compaction divider: "compacted (auto): 167k → 12k", or just the before when the agent
/// did not say the after.
#[must_use]
pub fn compacted_label(trigger: &str, pre_tokens: u64, post_tokens: Option<u64>) -> String {
    match post_tokens {
        Some(post) => {
            format!("compacted ({trigger}): {} → {}", tokens_label(pre_tokens), tokens_label(post))
        }
        None => format!("compacted ({trigger}): {}", tokens_label(pre_tokens)),
    }
}

/// Whether the context chip should warn: four fifths of the window spent.
#[must_use]
pub fn context_is_tight(context: &slopty_proto::agent::Context) -> bool {
    context.percent().is_some_and(|p| p >= 80)
}

/// The subscription's windows as "5h 23% · 7d 74%", or "limited until HH:MM" when the
/// agent is refused (the earliest reset shown); empty windows read as nothing.
#[must_use]
pub fn usage_label(usage: &slopty_proto::agent::Usage) -> String {
    if usage.limited {
        let reset = [usage.five_hour, usage.seven_day]
            .into_iter()
            .flatten()
            .map(|w| w.resets_at)
            .filter(|&t| t > 0)
            .min()
            .and_then(|t| clock(Some(t.saturating_mul(1000))));
        return match reset {
            Some(at) => format!("limited until {at}"),
            None => "rate limited".to_owned(),
        };
    }
    let mut parts = Vec::new();
    if let Some(w) = usage.five_hour {
        parts.push(format!("5h {}%", w.percent));
    }
    if let Some(w) = usage.seven_day {
        parts.push(format!("7d {}%", w.percent));
    }
    parts.join(" · ")
}

/// The cost as the header shows it: cents under a dollar, else dollars to the cent.
#[must_use]
pub fn cost_label(micro_usd: u64) -> String {
    let cents = micro_usd / 10_000;
    if cents < 100 { format!("{cents}¢") } else { format!("${}.{:02}", cents / 100, cents % 100) }
}

/// What the agent is waiting for, as the view shows it above the composer.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Attention {
    /// A tool needs permission: Allow / Deny.
    Permission {
        /// The tool.
        tool: String,
        /// The human already answered; waiting for the host to report the next state.
        answered: Option<bool>,
        /// One line about the call (the command, the file), when the host knows it: a driven
        /// agent says what the tool would do; a watched one only names the tool.
        detail: Option<String>,
        /// What "Always" would do, when the driven agent suggested a way not to ask again.
        always: Option<String>,
    },
    /// A driven agent asks with options: the card answers in place.
    Question {
        /// The questions, in the agent's order.
        questions: Vec<Question>,
        /// The labels picked so far, per question.
        chosen: Vec<Vec<String>>,
        /// The answer went out; waiting for the host to report the next state.
        answered: bool,
    },
    /// The agent asked something: the composer has the focus.
    Prompt,
}

/// A session's conversation as received so far.
pub struct Conversation {
    /// Entries, oldest first; shared with the list's render closure.
    entries: Rc<[TranscriptEntry]>,
    list: ListState,
    /// Entries whose folded part is open; shared with the list's render closure.
    open: Rc<HashSet<usize>>,
    /// The subagents the agent spawned, newest state per call; shared with the render
    /// closure, looked up by the spawning entry's call id.
    tasks: Rc<[AgentTask]>,
    /// The text being written.
    composer: Entity<TextareaState>,
    _composer_events: Subscription,
    /// The model menu under the header is open.
    model_menu: bool,
    /// Which slash completion is selected, an index into the current matches.
    completion: usize,
    /// Esc hid the completions for the text as it is now; typing shows them again.
    completions_hidden: bool,
}

impl std::fmt::Debug for Conversation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Conversation").field("entries", &self.entries.len()).finish_non_exhaustive()
    }
}

impl Conversation {
    /// Empty, waiting for the host's snapshot. The composer's ↩ calls
    /// [`TerminalView::submit_composer`].
    #[must_use]
    pub fn new(window: &mut Window, cx: &mut Context<TerminalView>) -> Self {
        let composer = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("Message Claude  (↩ send, ⇧↩ newline)")
                .auto_grow(1, COMPOSER_MAX_ROWS)
                .submit_on_enter(true)
        });
        let events =
            cx.subscribe_in(&composer, window, |this, _input, event, window, cx| match event {
                InputEvent::PressEnter { shift: false, .. } => this.submit_composer(window, cx),
                InputEvent::Change => this.composer_changed(cx),
                InputEvent::PressEnter { .. } | InputEvent::Focus | InputEvent::Blur => {}
            });
        let list = ListState::new(0, ListAlignment::Bottom, px(512.0));
        list.set_follow_mode(FollowMode::Tail);
        Self {
            entries: Rc::from([]),
            list,
            open: Rc::new(HashSet::new()),
            tasks: Rc::from([]),
            composer,
            _composer_events: events,
            model_menu: false,
            completion: 0,
            completions_hidden: false,
        }
    }

    /// The slash commands completing the composer's text, unless Esc hid them.
    #[must_use]
    pub fn completions(&self, commands: &[SlashCommand], cx: &App) -> Vec<SlashCommand> {
        if self.completions_hidden {
            return Vec::new();
        }
        slash_matches(&self.composer_text(cx), commands)
    }

    /// Which completion ↑/↓ have selected.
    #[must_use]
    pub const fn selected_completion(&self) -> usize {
        self.completion
    }

    /// ↓ (`1`) or ↑ (`-1`) among `count` completions, wrapping.
    pub fn step_completion(&mut self, delta: i32, count: usize) {
        if count == 0 {
            self.completion = 0;
            return;
        }
        let at = i64::try_from(self.completion.min(count.saturating_sub(1))).unwrap_or(0);
        let n = i64::try_from(count).unwrap_or(1);
        let next = at.saturating_add(i64::from(delta)).rem_euclid(n);
        self.completion = usize::try_from(next).unwrap_or(0);
    }

    /// The text changed: the list starts over from its first match and shows again.
    pub const fn composer_changed(&mut self) {
        self.completion = 0;
        self.completions_hidden = false;
    }

    /// Esc: hide the completions until the text changes.
    pub const fn hide_completions(&mut self) {
        self.completions_hidden = true;
    }

    /// Put `command` and a space in the composer, the caret after them.
    pub fn complete(&mut self, command: &str, window: &mut Window, cx: &mut App) {
        let text = format!("{command} ");
        self.composer.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.insert(text, window, cx);
        });
        self.completion = 0;
        self.completions_hidden = false;
    }

    /// Whether the model menu is open.
    #[must_use]
    pub const fn model_menu_open(&self) -> bool {
        self.model_menu
    }

    /// Open or close the model menu.
    pub const fn set_model_menu(&mut self, open: bool) {
        self.model_menu = open;
    }

    /// The entries shown.
    #[must_use]
    pub fn entries(&self) -> &[TranscriptEntry] {
        &self.entries
    }

    /// Whether the list follows new entries (the reader has not scrolled up).
    #[must_use]
    pub fn pinned(&self) -> bool {
        self.list.is_following_tail()
    }

    /// Back to the bottom, following again.
    pub fn pin(&self) {
        self.list.scroll_to_end();
        self.list.set_follow_mode(FollowMode::Tail);
    }

    /// Whether entry `ix`'s folded part is open.
    #[must_use]
    pub fn is_open(&self, ix: usize) -> bool {
        self.open.contains(&ix)
    }

    /// Open or fold entry `ix`.
    pub fn toggle(&mut self, ix: usize) {
        let mut open = HashSet::clone(&self.open);
        if !open.remove(&ix) {
            open.insert(ix);
        }
        self.open = Rc::new(open);
        self.list.splice(ix..ix.saturating_add(1), 1);
    }

    /// The composer's focus.
    #[must_use]
    pub fn composer_focus(&self, cx: &App) -> FocusHandle {
        self.composer.read(cx).focus_handle(cx)
    }

    /// Put the caret in the composer.
    pub fn focus_composer(&self, window: &mut Window, cx: &mut App) {
        self.composer.update(cx, |input, cx| input.focus(window, cx));
    }

    /// What is written in the composer.
    #[must_use]
    pub fn composer_text(&self, cx: &App) -> String {
        self.composer.read(cx).value().to_string()
    }

    /// Put `text` into the composer at the caret (the key bar's paste on the phone).
    pub fn insert_composer_text(&self, text: &str, window: &mut Window, cx: &mut App) {
        self.composer.update(cx, |input, cx| input.insert(text.to_owned(), window, cx));
    }

    /// Take the composer's text, leaving it empty.
    #[must_use]
    pub fn take_composer_text(&self, window: &mut Window, cx: &mut App) -> String {
        let text = self.composer_text(cx);
        self.composer.update(cx, |input, cx| input.clean(window, cx));
        text
    }

    /// A subagent's newest state, replacing what was known of that call.
    pub fn set_task(&mut self, task: AgentTask) {
        let mut tasks = self.tasks.to_vec();
        match tasks.iter_mut().find(|t| t.call == task.call) {
            Some(seen) => *seen = task,
            None => tasks.push(task),
        }
        self.tasks = Rc::from(tasks);
    }

    /// The subagents known, in the order first seen.
    #[must_use]
    pub fn tasks(&self) -> &[AgentTask] {
        &self.tasks
    }

    /// Take a slice from the host: a reset replaces everything (and pins the view to the
    /// bottom again), otherwise the entries append.
    pub fn apply(&mut self, update: TranscriptUpdate) {
        let mut entries = if update.reset { Vec::new() } else { self.entries.to_vec() };
        let before = entries.len();
        entries.extend(update.entries);
        let after = entries.len();
        self.entries = Rc::from(entries);
        if update.reset {
            self.open = Rc::new(HashSet::new());
            self.tasks = Rc::from([]);
            self.list.reset(after);
            self.pin();
        } else {
            self.list.splice(before..before, after.saturating_sub(before));
        }
    }

    /// The chat, filling its container: the list, the attention row, the composer
    /// (`composer_focused` draws its focus ring).
    #[must_use]
    #[expect(
        clippy::too_many_arguments,
        reason = "one call site; the view's render passes its state through"
    )]
    pub fn render(
        &self,
        info: Option<&AgentInfo>,
        attention: Option<&Attention>,
        partial: &str,
        attachments: &[slopty_proto::agent::Image],
        preparing: usize,
        composer_focused: bool,
        working: bool,
        theme: &Theme,
        cx: &Context<TerminalView>,
    ) -> AnyElement {
        let completions = info.map(|i| self.completions(&i.slash_commands, cx)).unwrap_or_default();
        let entries = Rc::clone(&self.entries);
        let open = Rc::clone(&self.open);
        let tasks = Rc::clone(&self.tasks);
        let theme = theme.clone();
        let empty = entries.is_empty();
        let view = cx.entity();
        let s = theme.surfaces.clone();
        let spacing = theme.spacing;
        div()
            .id("conversation")
            .debug_selector(|| "conversation".to_owned())
            .role(Role::Group)
            .aria_label("Conversation")
            .size_full()
            .flex()
            .flex_col()
            .bg(hsla(s.panel))
            .text_size(px(theme.typography.ui_size))
            .text_color(hsla(s.text))
            .font_family(theme.typography.ui_family.clone())
            .when_some(info, |el, info| el.child(self.header_row(info, &theme, cx)))
            .child(
                div()
                    .relative()
                    .flex_1()
                    .w_full()
                    .min_h(px(0.0))
                    .when(empty, |el| {
                        el.flex()
                            .items_center()
                            .justify_center()
                            .text_color(hsla(s.text_muted))
                            .child("waiting for the agent's conversation…")
                    })
                    .when(!empty, |el| {
                        let theme = theme.clone();
                        el.child(
                            list(
                                self.list.clone(),
                                move |ix, _window: &mut Window, _cx: &mut App| {
                                    entries.get(ix).map_or_else(
                                        || div().into_any_element(),
                                        |e| {
                                            let task = match &e.body {
                                                TranscriptBody::ToolUse { call, .. } => {
                                                    tasks.iter().find(|t| &t.call == call)
                                                }
                                                _ => None,
                                            };
                                            entry(ix, e, open.contains(&ix), task, &view, &theme)
                                        },
                                    )
                                },
                            )
                            .size_full(),
                        )
                    })
                    .when(!empty && !self.pinned(), |el| {
                        let latest = div()
                            .id("conversation-latest")
                            .debug_selector(|| "conversation-latest".to_owned())
                            .role(Role::Button)
                            .aria_label("Jump to latest")
                            .absolute()
                            .bottom(px(spacing.sm))
                            .right(px(spacing.md))
                            .px(px(spacing.sm))
                            .py(px(spacing.xxs))
                            .rounded(px(theme.radii.sm))
                            .bg(hsla(s.accent))
                            .text_color(hsla(s.accent_fg))
                            .text_size(px(theme.typography.small()))
                            .shadow_sm()
                            .cursor_pointer()
                            .child("↓ latest");
                        el.child(tab_stop(latest, s.accent).on_click(cx.listener(
                            |this, _ev, _window, cx| {
                                if let Some(c) = this.conversation() {
                                    c.pin();
                                }
                                cx.notify();
                            },
                        )))
                    }),
            )
            .when(!partial.is_empty(), |el| el.child(partial_row(partial, &theme)))
            .when_some(attention, |el, attention| el.child(attention_row(attention, &theme, cx)))
            .when(!completions.is_empty(), |el| {
                el.child(self.completions_row(&completions, &theme, cx))
            })
            .when(!attachments.is_empty() || preparing > 0, |el| {
                el.child(attachments_row(attachments, preparing, &theme, cx))
            })
            .child(self.composer_row(composer_focused, working, &theme, cx))
            .into_any_element()
    }

    /// What the agent says about itself, over the list: the model chip (its menu below it
    /// while open), the permission-mode chip, the turns and the cost.
    fn header_row(
        &self,
        info: &AgentInfo,
        theme: &Theme,
        cx: &Context<TerminalView>,
    ) -> AnyElement {
        let s = &theme.surfaces;
        let spacing = theme.spacing;
        let small = theme.typography.small();
        let chip = |id: &'static str, label: String, aria: String| {
            let el = div()
                .id(id)
                .debug_selector(move || id.to_owned())
                .role(Role::Button)
                .aria_label(SharedString::from(aria))
                .px(px(spacing.sm))
                .py(px(spacing.xxs))
                .rounded(px(theme.radii.xs))
                .bg(hsla_alpha(s.accent, alpha::TINT))
                .text_color(hsla(s.text))
                .cursor_pointer()
                .hover(move |el| el.bg(hsla_alpha(s.accent, alpha::TINT_STRONG)))
                .child(SharedString::from(label));
            tab_stop(el, s.accent)
        };
        let model = info.model.as_deref().map_or_else(|| "Claude".to_owned(), model_label);
        let mode = info.permission_mode.as_deref().unwrap_or("default");
        let mut meta = format!("{} {}", info.turns, if info.turns == 1 { "turn" } else { "turns" });
        if info.cost_micro_usd > 0 {
            meta.push_str(" · ");
            meta.push_str(&cost_label(info.cost_micro_usd));
        }
        let usage = info.usage.as_ref().map(usage_label);
        let limited = info.usage.as_ref().is_some_and(|u| u.limited);
        let context = info.context.as_ref().map(context_label);
        let tight = info.context.as_ref().is_some_and(context_is_tight);
        let menu_open = self.model_menu;
        div()
            .id("conversation-header")
            .debug_selector(|| "conversation-header".to_owned())
            .role(Role::Group)
            .aria_label("Agent")
            .w_full()
            .flex_none()
            .flex()
            .flex_col()
            .border_b_1()
            .border_color(hsla(s.border))
            .bg(hsla(s.panel))
            .text_size(px(small))
            .child(
                div()
                    .w_full()
                    .flex()
                    .items_center()
                    .gap(px(spacing.sm))
                    .px(px(spacing.md))
                    .py(px(spacing.xs))
                    .child(
                        chip("conversation-model", model.clone(), format!("Model: {model}"))
                            .on_click(cx.listener(|this, _ev, _window, cx| {
                                this.toggle_model_menu(cx);
                            })),
                    )
                    .child(
                        chip(
                            "conversation-mode",
                            mode_label(mode).to_owned(),
                            format!("Permission mode: {}", mode_label(mode)),
                        )
                        .on_click(cx.listener(|this, _ev, _window, cx| {
                            this.cycle_permission_mode(cx);
                        })),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_color(hsla(s.text_muted))
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(SharedString::from(meta)),
                    )
                    .when_some(context, |el, context| {
                        // How full the context window is, in the warn tone once four fifths
                        // are spent (auto-compaction is near).
                        el.child(
                            div()
                                .id("conversation-context")
                                .debug_selector(|| "conversation-context".to_owned())
                                .role(Role::Status)
                                .aria_label(SharedString::from(format!("Context: {context}")))
                                .flex_none()
                                .text_size(px(theme.typography.caption()))
                                .text_color(hsla(if tight { s.warn } else { s.text_muted }))
                                .child(SharedString::from(context)),
                        )
                    })
                    .when_some(usage, |el, usage| {
                        // The subscription's windows, in the warn tone once a limit bites.
                        el.child(
                            div()
                                .id("conversation-usage")
                                .debug_selector(|| "conversation-usage".to_owned())
                                .role(Role::Status)
                                .aria_label(SharedString::from(format!("Usage: {usage}")))
                                .flex_none()
                                .text_size(px(theme.typography.caption()))
                                .text_color(hsla(if limited { s.warn } else { s.text_muted }))
                                .child(SharedString::from(usage)),
                        )
                    }),
            )
            .when(menu_open, |el| {
                let current = info.model.clone().unwrap_or_default();
                el.child(
                    div()
                        .id("conversation-models")
                        .debug_selector(|| "conversation-models".to_owned())
                        .role(Role::Menu)
                        .aria_label("Models")
                        .w_full()
                        .flex()
                        .flex_wrap()
                        .gap(px(spacing.xs))
                        .px(px(spacing.md))
                        .pb(px(spacing.xs))
                        .children(MODELS.iter().map(|(alias, name)| {
                            let alias: &'static str = alias;
                            let chosen = current == alias
                                || current
                                    .strip_prefix("claude-")
                                    .is_some_and(|m| m.starts_with(alias));
                            let row = div()
                                .id(ElementId::Name(format!("conversation-model-{alias}").into()))
                                .debug_selector(move || format!("conversation-model-{alias}"))
                                .role(Role::MenuItem)
                                .aria_label(SharedString::from((*name).to_owned()))
                                .px(px(spacing.sm))
                                .py(px(spacing.xxs))
                                .rounded(px(theme.radii.xs))
                                .bg(hsla_alpha(
                                    if chosen { s.accent } else { s.text_muted },
                                    if chosen { alpha::TINT_STRONG } else { alpha::TINT },
                                ))
                                .cursor_pointer()
                                .hover(move |el| el.bg(hsla_alpha(s.accent, alpha::TINT_PRESSED)))
                                .child(SharedString::from((*name).to_owned()));
                            tab_stop(row, s.accent).on_click(cx.listener(
                                move |this, _ev, _window, cx| {
                                    this.set_model(alias, cx);
                                },
                            ))
                        })),
                )
            })
            .into_any_element()
    }

    /// The slash commands completing the composer, above it, one per line: the name in the
    /// mono face, its argument hint and description muted after it; the selected one is
    /// highlighted and a click takes any of them.
    fn completions_row(
        &self,
        completions: &[SlashCommand],
        theme: &Theme,
        cx: &Context<TerminalView>,
    ) -> AnyElement {
        let s = &theme.surfaces;
        let spacing = theme.spacing;
        let selected = self.completion.min(completions.len().saturating_sub(1));
        let mono = theme.typography.mono_families.first().cloned().unwrap_or_default();
        div()
            .id("conversation-completions")
            .debug_selector(|| "conversation-completions".to_owned())
            .role(Role::ListBox)
            .aria_label("Slash commands")
            .w_full()
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(spacing.xxs))
            .px(px(spacing.md))
            .py(px(spacing.xs))
            .border_t_1()
            .border_color(hsla(s.border))
            .text_size(px(theme.typography.small()))
            .font_family(theme.typography.ui_family.clone())
            .children(completions.iter().enumerate().map(|(ix, command)| {
                let chosen = ix == selected;
                let name = command.name.clone();
                let label = slash_label(command);
                let hint = (!command.hint.is_empty()).then(|| command.hint.clone());
                let description =
                    (!command.description.is_empty()).then(|| command.description.clone());
                div()
                    .id(ElementId::NamedInteger(
                        "conversation-completion".into(),
                        u64::try_from(ix).unwrap_or(0),
                    ))
                    .debug_selector(move || format!("conversation-completion-{ix}"))
                    .role(Role::ListBoxOption)
                    .aria_label(SharedString::from(label))
                    .flex()
                    .items_baseline()
                    .gap(px(spacing.sm))
                    .px(px(spacing.sm))
                    .py(px(spacing.xxs))
                    .rounded(px(theme.radii.xs))
                    .bg(hsla_alpha(s.accent, if chosen { alpha::TINT_STRONG } else { alpha::TINT }))
                    .text_color(hsla(s.text))
                    .cursor_pointer()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .on_mouse_down(gpui::MouseButton::Left, |_ev, _window, cx| {
                        // The composer keeps the caret; the click must not blur it.
                        cx.stop_propagation();
                    })
                    .on_click(cx.listener(move |this, _ev, window, cx| {
                        this.complete_slash(&name, window, cx);
                    }))
                    .child(
                        div()
                            .flex_none()
                            .font_family(mono.clone())
                            .child(SharedString::from(command.name.clone())),
                    )
                    .children(hint.map(|hint| {
                        div()
                            .flex_none()
                            .text_color(hsla(s.text_muted))
                            .child(SharedString::from(hint))
                    }))
                    .children(description.map(|text| {
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .text_color(hsla(s.text_muted))
                            .child(SharedString::from(text))
                    }))
            }))
            .into_any_element()
    }

    /// The composer: the growing text field (accent ring while it has the caret) and the send
    /// button, the one primary action on this surface.
    /// The field and, next to it, Send — or Stop while the driven agent is working, since
    /// a finger has no Esc and a turn that runs away wants one tap.
    fn composer_row(
        &self,
        focused: bool,
        working: bool,
        theme: &Theme,
        cx: &Context<TerminalView>,
    ) -> AnyElement {
        let s = &theme.surfaces;
        let spacing = theme.spacing;
        let send = theme.typography.ui_size + spacing.md;
        div()
            .id("composer")
            .debug_selector(|| "composer".to_owned())
            .role(Role::Group)
            .aria_label("Composer")
            .w_full()
            .flex_none()
            .flex()
            .items_end()
            .gap(px(spacing.sm))
            .px(px(spacing.md))
            .py(px(spacing.sm))
            .border_t_1()
            .border_color(hsla(s.border))
            .bg(hsla(s.panel))
            .child(
                div()
                    .id("composer-field")
                    .debug_selector(|| "composer-field".to_owned())
                    .flex_1()
                    .min_w(px(0.0))
                    .px(px(spacing.sm))
                    .py(px(spacing.xs))
                    .rounded(px(theme.radii.sm))
                    .border_1()
                    .border_color(hsla(if focused { s.accent } else { s.border }))
                    .bg(hsla(s.raised))
                    .child(
                        Textarea::new(&self.composer)
                            .appearance(false)
                            .bordered(false)
                            .aria_label("Message to Claude"),
                    ),
            )
            .child({
                let (id, label, glyph, color) = if working {
                    ("composer-stop", "Stop", "■", s.warn)
                } else {
                    ("composer-send", "Send", "↑", s.accent)
                };
                let button = div()
                    .id(id)
                    .debug_selector(move || id.to_owned())
                    .role(Role::Button)
                    .aria_label(label)
                    .flex_none()
                    .w(px(send))
                    .h(px(send))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(theme.radii.sm))
                    .bg(hsla(color))
                    .text_color(hsla(s.accent_fg))
                    .cursor_pointer()
                    .child(glyph);
                tab_stop(button, s.accent).on_click(cx.listener(move |this, _ev, window, cx| {
                    if working {
                        this.interrupt_agent();
                    } else {
                        this.submit_composer(window, cx);
                    }
                }))
            })
            .into_any_element()
    }
}

/// What the agent is writing right now, under the list and ahead of its next entry: the
/// Markdown so far, capped so a long answer scrolls in the entry it becomes.
fn partial_row(partial: &str, theme: &Theme) -> AnyElement {
    let s = &theme.surfaces;
    let spacing = theme.spacing;
    let prose = gpui::relative(theme.typography.markdown_line_height);
    let mono = theme.typography.mono_families.first().cloned().unwrap_or_default();
    div()
        .id("conversation-partial")
        .debug_selector(|| "conversation-partial".to_owned())
        .role(Role::Status)
        .aria_label("Claude is writing")
        .w_full()
        .flex_none()
        .max_h(px(theme.typography.ui_size * 12.0))
        .overflow_hidden()
        .px(px(spacing.md))
        .py(px(spacing.xs))
        .border_t_1()
        .border_color(hsla(s.border))
        .line_height(prose)
        .child(
            TextView::markdown("conversation-partial-md", SharedString::from(partial.to_owned()))
                .style(markdown_style(theme, &mono)),
        )
        .into_any_element()
}

/// What the agent waits for, with the one-tap answers, above the composer: the warn tone,
/// faint, since the agent is blocked on the human.
/// The pictures waiting to go with the next prompt, one chip each (`composer-attachment-<i>`,
/// a button that drops it), and a muted chip for those still being made fit
/// (`composer-attachment-preparing`).
fn attachments_row(
    attachments: &[slopty_proto::agent::Image],
    preparing: usize,
    theme: &Theme,
    cx: &Context<TerminalView>,
) -> AnyElement {
    let s = &theme.surfaces;
    let spacing = theme.spacing;
    div()
        .id("composer-attachments")
        .debug_selector(|| "composer-attachments".to_owned())
        .role(Role::Group)
        .aria_label("Attachments")
        .w_full()
        .flex_none()
        .flex()
        .flex_wrap()
        .gap(px(spacing.sm))
        .px(px(spacing.md))
        .py(px(spacing.xs))
        .border_t_1()
        .border_color(hsla(s.border))
        .bg(hsla(s.panel))
        .children(attachments.iter().enumerate().map(|(i, image)| {
            let label = attachment_label(image);
            let chip = div()
                .id(("composer-attachment", i))
                .debug_selector(move || format!("composer-attachment-{i}"))
                .role(Role::Button)
                .aria_label(SharedString::from(format!(
                    "Remove picture {}: {label}",
                    i.saturating_add(1)
                )))
                .px(px(spacing.sm))
                .py(px(spacing.xs))
                .rounded(px(theme.radii.sm))
                .border_1()
                .border_color(hsla(s.border))
                .bg(hsla(s.raised))
                .text_size(px(theme.typography.ui_size))
                .text_color(hsla(s.text))
                .cursor_pointer()
                .child(SharedString::from(format!("🖼 {label} ×")));
            tab_stop(chip, s.accent).on_click(cx.listener(move |view, _ev, _window, cx| {
                view.remove_attachment(i, cx);
            }))
        }))
        .when(preparing > 0, |el| {
            el.child(
                div()
                    .id("composer-attachment-preparing")
                    .debug_selector(|| "composer-attachment-preparing".to_owned())
                    .role(Role::Status)
                    .aria_label(SharedString::from(format!(
                        "Preparing {}",
                        pictures_label(u32::try_from(preparing).unwrap_or(u32::MAX))
                    )))
                    .px(px(spacing.sm))
                    .py(px(spacing.xs))
                    .rounded(px(theme.radii.sm))
                    .border_1()
                    .border_color(hsla(s.border))
                    .text_size(px(theme.typography.ui_size))
                    .text_color(hsla(s.text_muted))
                    .child(SharedString::from(format!(
                        "preparing {}…",
                        pictures_label(u32::try_from(preparing).unwrap_or(u32::MAX))
                    ))),
            )
        })
        .into_any_element()
}

fn attention_row(attention: &Attention, theme: &Theme, cx: &Context<TerminalView>) -> AnyElement {
    let s = &theme.surfaces;
    let spacing = theme.spacing;
    let status = match attention {
        Attention::Permission { tool, answered: None, detail: None, .. } => {
            format!("Claude wants to use {tool}")
        }
        Attention::Permission { tool, answered: None, detail: Some(detail), .. } => {
            format!("Claude wants to use {tool}: {detail}")
        }
        Attention::Permission { tool, answered: Some(true), .. } => format!("{tool}: allowed"),
        Attention::Permission { tool, answered: Some(false), .. } => format!("{tool}: denied"),
        Attention::Question { questions, answered: false, .. } => format!(
            "Claude asks: {}",
            questions.first().map(|q| q.text.as_str()).unwrap_or_default()
        ),
        Attention::Question { chosen, answered: true, .. } => {
            format!(
                "Answered: {}",
                chosen.iter().map(|c| c.join(", ")).collect::<Vec<_>>().join("; ")
            )
        }
        Attention::Prompt => "Claude is waiting for your answer".to_owned(),
    };
    let row = div()
        .id("conversation-attention")
        .debug_selector(|| "conversation-attention".to_owned())
        .role(Role::Status)
        .aria_label(SharedString::from(status))
        .w_full()
        .flex_none()
        .flex()
        .items_center()
        .gap(px(spacing.sm))
        .px(px(spacing.md))
        .py(px(spacing.sm))
        .border_t_1()
        .border_color(hsla(s.border))
        .bg(hsla_alpha(s.warn, alpha::TINT))
        .text_size(px(theme.typography.small()));
    let button = |id: &'static str, label: &'static str, accent: bool| {
        let tone = if accent { s.accent } else { s.text_secondary };
        let pill = div()
            .id(id)
            .debug_selector(move || id.to_owned())
            .role(Role::Button)
            .aria_label(label)
            .flex_none()
            .px(px(spacing.sm))
            .py(px(spacing.xs))
            .rounded(px(theme.radii.xs))
            .bg(hsla_alpha(tone, alpha::TINT_STRONG))
            .text_color(hsla(s.text))
            .cursor_pointer()
            .hover(move |el| el.bg(hsla_alpha(tone, alpha::TINT_PRESSED)))
            .child(label);
        tab_stop(pill, s.accent)
    };
    match attention {
        Attention::Permission { tool, answered: None, detail, always } => row
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .flex()
                    .flex_col()
                    .child(SharedString::from(format!("Claude wants to use {tool}")))
                    .when_some(detail.as_ref(), |el, detail| {
                        el.child(
                            div()
                                .text_color(hsla(s.text_muted))
                                .font_family(
                                    theme
                                        .typography
                                        .mono_families
                                        .first()
                                        .cloned()
                                        .unwrap_or_default(),
                                )
                                .text_size(px(theme.typography.caption()))
                                .overflow_hidden()
                                .text_ellipsis()
                                .child(SharedString::from(detail.clone())),
                        )
                    }),
            )
            .child(
                button("conversation-allow", "Allow", true)
                    .on_click(cx.listener(|this, _ev, _window, cx| this.answer(true, cx))),
            )
            .when_some(always.as_ref(), |el, what| {
                // Always: the agent's own way of not asking again, named so the tap is
                // informed ("accept edits for this session"); a screen reader hears both.
                let label: SharedString = format!("Always: {what}").into();
                let pill = div()
                    .id("conversation-always")
                    .debug_selector(|| "conversation-always".to_owned())
                    .role(Role::Button)
                    .aria_label(label)
                    .flex_none()
                    .flex()
                    .flex_col()
                    .items_center()
                    .px(px(spacing.sm))
                    .py(px(spacing.xs))
                    .rounded(px(theme.radii.xs))
                    .bg(hsla_alpha(s.accent, alpha::TINT))
                    .text_color(hsla(s.text))
                    .cursor_pointer()
                    .hover(|el| el.bg(hsla_alpha(s.accent, alpha::TINT_STRONG)))
                    .child("Always")
                    .child(
                        div()
                            .text_size(px(theme.typography.caption()))
                            .text_color(hsla(s.text_muted))
                            .child(SharedString::from(what.clone())),
                    );
                el.child(
                    tab_stop(pill, s.accent)
                        .on_click(cx.listener(|this, _ev, _window, cx| this.answer_always(cx))),
                )
            })
            .child(
                button("conversation-deny", "Deny", false)
                    .on_click(cx.listener(|this, _ev, _window, cx| this.answer(false, cx))),
            )
            .into_any_element(),
        Attention::Permission { tool, answered: Some(allowed), .. } => row
            .text_color(hsla(s.text_muted))
            .child(SharedString::from(format!(
                "{tool}: {}",
                if *allowed { "allowed" } else { "denied" }
            )))
            .into_any_element(),
        Attention::Question { questions, chosen, answered: false } => {
            let all_single = questions.iter().all(|q| !q.multi);
            let complete = chosen.len() == questions.len() && chosen.iter().all(|c| !c.is_empty());
            row.items_start()
                .flex_col()
                .child(div().w_full().flex().flex_col().gap(px(spacing.sm)).children(
                    questions.iter().enumerate().map(|(qi, q)| {
                        question_block(
                            qi,
                            q,
                            chosen.get(qi).map_or(&[][..], Vec::as_slice),
                            theme,
                            cx,
                        )
                    }),
                ))
                .when(!all_single || questions.len() > 1, |el| {
                    let send = button("question-answer", "Answer", complete);
                    el.child(div().w_full().flex().justify_end().child(
                        send.on_click(
                            cx.listener(|this, _ev, _window, cx| this.answer_question(cx)),
                        ),
                    ))
                })
                .into_any_element()
        }
        Attention::Question { chosen, answered: true, .. } => row
            .text_color(hsla(s.text_muted))
            .child(SharedString::from(format!(
                "Answered: {}",
                chosen.iter().map(|c| c.join(", ")).collect::<Vec<_>>().join("; ")
            )))
            .into_any_element(),
        Attention::Prompt => row
            .text_color(hsla(s.text_muted))
            .child("Claude is waiting for your answer")
            .into_any_element(),
    }
}

/// One question of a pending `AskUserQuestion`: the header as a small chip, the question,
/// then the options as buttons (`question-option-<q>-<o>`, labelled by their label, the
/// picked ones in the accent tone) with each description under its label. The composer
/// below takes a typed answer instead.
fn question_block(
    qi: usize,
    question: &Question,
    picked: &[String],
    theme: &Theme,
    cx: &Context<TerminalView>,
) -> AnyElement {
    let s = &theme.surfaces;
    let spacing = theme.spacing;
    div()
        .w_full()
        .flex()
        .flex_col()
        .gap(px(spacing.xs))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(spacing.sm))
                .when(!question.header.is_empty(), |el| {
                    el.child(
                        div()
                            .px(px(spacing.xs))
                            .rounded(px(theme.radii.xs))
                            .bg(hsla_alpha(s.text_secondary, alpha::TINT_STRONG))
                            .text_size(px(theme.typography.caption()))
                            .text_color(hsla(s.text_secondary))
                            .child(SharedString::from(question.header.clone())),
                    )
                })
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.0))
                        .whitespace_normal()
                        .text_color(hsla(s.text))
                        .child(SharedString::from(question.text.clone())),
                ),
        )
        .child(div().flex().flex_wrap().gap(px(spacing.xs)).children(
            question.options.iter().enumerate().map(|(oi, option)| {
                let on = picked.iter().any(|l| l == &option.label);
                let tone = if on { s.accent } else { s.text_secondary };
                let label = option.label.clone();
                let id = ElementId::NamedInteger(
                    "question-option".into(),
                    u64::try_from(qi.saturating_mul(64).saturating_add(oi)).unwrap_or(0),
                );
                let pill = div()
                    .id(id)
                    .debug_selector(move || format!("question-option-{qi}-{oi}"))
                    .role(Role::Button)
                    .aria_label(SharedString::from(option.label.clone()))
                    .flex()
                    .flex_col()
                    .px(px(spacing.sm))
                    .py(px(spacing.xs))
                    .rounded(px(theme.radii.xs))
                    .bg(hsla_alpha(tone, if on { alpha::TINT_PRESSED } else { alpha::TINT_STRONG }))
                    .text_color(hsla(s.text))
                    .cursor_pointer()
                    .hover(move |el| el.bg(hsla_alpha(tone, alpha::TINT_PRESSED)))
                    .child(SharedString::from(option.label.clone()))
                    .when(!option.description.is_empty(), |el| {
                        el.child(
                            div()
                                .text_size(px(theme.typography.caption()))
                                .text_color(hsla(s.text_muted))
                                .child(SharedString::from(option.description.clone())),
                        )
                    });
                tab_stop(pill, s.accent).on_click(cx.listener(move |this, _ev, _window, cx| {
                    this.choose(qi, &label, cx);
                }))
            }),
        ))
        .into_any_element()
}

/// One entry: the human's prompt in an accent bubble, the agent's Markdown, a folded line
/// for thinking, a tool call that opens on its input, a tool result.
fn entry(
    ix: usize,
    entry: &TranscriptEntry,
    open: bool,
    task: Option<&AgentTask>,
    view: &Entity<TerminalView>,
    theme: &Theme,
) -> AnyElement {
    let s = &theme.surfaces;
    let spacing = theme.spacing;
    let ui_size = theme.typography.ui_size;
    let small = theme.typography.small();
    let prose = gpui::relative(theme.typography.markdown_line_height);
    let mono = theme.typography.mono_families.first().cloned().unwrap_or_default();
    let row = div()
        .id(ElementId::NamedInteger("conversation-entry".into(), u64::try_from(ix).unwrap_or(0)))
        .debug_selector(move || format!("conversation-entry-{ix}"))
        .role(Role::ListItem)
        .aria_label(SharedString::from(entry_label(entry, task)))
        .w_full()
        .px(px(spacing.md))
        .py(px(spacing.xs));
    let time = clock(entry.at).map(|t| {
        div()
            .flex_none()
            .text_size(px(theme.typography.caption()))
            .text_color(hsla(s.text_muted))
            .child(SharedString::from(t))
    });
    let toggle = {
        let view = view.clone();
        move |_ev: &gpui::ClickEvent, _window: &mut Window, cx: &mut App| {
            view.update(cx, |v, cx| v.toggle_entry(ix, cx));
        }
    };
    match &entry.body {
        TranscriptBody::User { text, images } => row
            .flex()
            .items_end()
            .justify_end()
            .gap(px(spacing.sm))
            .children(time)
            .child(
                div()
                    .max_w(px(ui_size * 40.0))
                    .px(px(spacing.md))
                    .py(px(spacing.sm))
                    .rounded(px(theme.radii.md))
                    .bg(hsla_alpha(s.accent, alpha::TINT_STRONG))
                    .line_height(prose)
                    .when(*images > 0, |el| {
                        // The pictures stay with the agent; the bubble says they were sent.
                        el.child(
                            div()
                                .text_size(px(theme.typography.ui_size))
                                .text_color(hsla(s.text_muted))
                                .child(SharedString::from(pictures_label(*images))),
                        )
                    })
                    .when(!text.is_empty(), |el| el.child(SharedString::from(text.clone()))),
            )
            .into_any_element(),
        TranscriptBody::Assistant { markdown } => row
            .flex()
            .items_end()
            .gap(px(spacing.sm))
            .child(
                div().flex_1().min_w(px(0.0)).line_height(prose).child(
                    TextView::markdown(
                        ElementId::NamedInteger(
                            "conversation-md".into(),
                            u64::try_from(ix).unwrap_or(0),
                        ),
                        SharedString::from(markdown.clone()),
                    )
                    .style(markdown_style(theme, &mono)),
                ),
            )
            .children(time)
            .into_any_element(),
        TranscriptBody::Thinking { text } => row
            .flex()
            .flex_col()
            .gap(px(spacing.xs))
            .text_size(px(small))
            .text_color(hsla(s.text_muted))
            .child(
                div()
                    .id("fold")
                    .cursor_pointer()
                    .child(SharedString::from(fold_label(open, "thinking", text)))
                    .on_click(toggle),
            )
            .when(open, |el| el.child(clipped_block(text, &mono, theme)))
            .into_any_element(),
        TranscriptBody::ToolUse { name, summary, detail, .. } => row
            .flex()
            .flex_col()
            .gap(px(spacing.xs))
            .text_size(px(small))
            .text_color(hsla(s.text_muted))
            .child(
                div()
                    .id("fold")
                    .flex()
                    .gap(px(spacing.sm))
                    .cursor_pointer()
                    .child(SharedString::from(format!("{} {name}", arrow(open))))
                    .child(
                        div()
                            .flex_1()
                            .overflow_hidden()
                            .text_ellipsis()
                            .font_family(mono.clone())
                            .text_color(hsla(s.text_secondary))
                            .child(SharedString::from(summary.clone())),
                    )
                    .children(tool_badge(detail, theme))
                    .on_click(toggle),
            )
            .children(task.map(|t| task_line(t, theme)))
            .children(tool_body(detail, open, &mono, theme))
            .into_any_element(),
        TranscriptBody::Compacted { trigger, pre_tokens, post_tokens } => row
            .flex()
            .items_center()
            .gap(px(spacing.sm))
            .text_size(px(theme.typography.caption()))
            .text_color(hsla(s.text_muted))
            .child(div().flex_1().h(px(1.0)).bg(hsla(s.border)))
            .child(SharedString::from(compacted_label(trigger, *pre_tokens, *post_tokens)))
            .child(div().flex_1().h(px(1.0)).bg(hsla(s.border)))
            .into_any_element(),
        TranscriptBody::Notice { level, text } => {
            let color = match level {
                NoticeLevel::Notice => s.text_muted,
                NoticeLevel::Suggestion => s.accent,
                NoticeLevel::Warning => s.warn,
            };
            row.text_size(px(small))
                .text_color(hsla(color))
                .child(SharedString::from(text.clone()))
                .into_any_element()
        }
        TranscriptBody::ToolResult { tool, output, is_error } => {
            let (shown, hidden) = preview(output, open);
            let color = if *is_error { s.error } else { s.text_muted };
            row.flex()
                .flex_col()
                .gap(px(spacing.xs))
                .text_size(px(small))
                .text_color(hsla(color))
                .child(
                    div()
                        .id("fold")
                        .cursor_pointer()
                        .child(SharedString::from(format!(
                            "{} {}{}",
                            if hidden > 0 || open { arrow(open) } else { "↳" },
                            tool.as_deref().unwrap_or("result"),
                            if *is_error { " failed" } else { "" }
                        )))
                        .on_click(toggle),
                )
                .when(!shown.text.is_empty(), |el| el.child(clipped_block(&shown, &mono, theme)))
                .into_any_element()
        }
    }
}

/// A subagent's progress under the call that spawned it: "Explore running · 1 tool use ·
/// 3 s · Bash", the state in the accent tone while it runs and muted once done.
fn task_line(task: &AgentTask, theme: &Theme) -> impl IntoElement {
    let s = &theme.surfaces;
    let (state, tone) = if task.done { ("done", s.text_muted) } else { ("running", s.accent) };
    div()
        .id("tool-task")
        .debug_selector(|| "tool-task".to_owned())
        .flex()
        .gap(px(theme.spacing.xs))
        .pl(px(theme.spacing.lg))
        .text_size(px(theme.typography.caption()))
        .child(div().text_color(hsla(tone)).child(SharedString::from(format!(
            "{} {state}",
            task.kind.as_deref().unwrap_or("agent")
        ))))
        .child(
            div()
                .text_color(hsla(s.text_muted))
                .child(SharedString::from(format!("· {}", task_counts(task).join(" · ")))),
        )
}

/// A subagent's counts as words: tool uses, seconds, the last tool.
fn task_counts(task: &AgentTask) -> Vec<String> {
    let mut parts =
        vec![format!("{} tool use{}", task.tool_uses, if task.tool_uses == 1 { "" } else { "s" })];
    let seconds = task.duration_ms.div_ceil(1000);
    if seconds > 0 {
        parts.push(format!("{seconds} s"));
    }
    if let Some(tool) = &task.last_tool {
        parts.push(tool.clone());
    }
    parts
}

/// One entry as a screen reader hears it: who, then the first line; a call that spawned a
/// subagent adds the subagent's state and counts.
#[must_use]
pub fn entry_label(entry: &TranscriptEntry, task: Option<&AgentTask>) -> String {
    let first = |text: &str| text.lines().find(|l| !l.trim().is_empty()).unwrap_or("").to_owned();
    match &entry.body {
        TranscriptBody::User { text, images } if *images > 0 => {
            format!("You: {} {}", pictures_label(*images), first(text)).trim_end().to_owned()
        }
        TranscriptBody::User { text, .. } => format!("You: {}", first(text)),
        TranscriptBody::Assistant { markdown } => format!("Claude: {}", first(markdown)),
        TranscriptBody::Thinking { .. } => "Claude thinking".to_owned(),
        TranscriptBody::ToolUse { name, summary, detail, .. } => match (task, detail) {
            (Some(task), _) => format!(
                "Tool {name}: {summary}, {} {}, {}",
                task.kind.as_deref().unwrap_or("agent"),
                if task.done { "done" } else { "running" },
                task_counts(task).join(", ")
            ),
            (None, ToolDetail::Diff { lines, more_lines, .. }) => {
                let (added, removed) = diff_counts(lines);
                format!("Tool {name}: {summary}, {added} added, {removed} removed{}", {
                    if *more_lines > 0 {
                        format!(", {more_lines} more lines")
                    } else {
                        String::new()
                    }
                })
            }
            (None, ToolDetail::Todos { items }) => {
                let done = items.iter().filter(|t| t.status == TodoStatus::Completed).count();
                format!("Tool {name}: {done} of {} done", items.len())
            }
            (None, _) => format!("Tool {name}: {summary}"),
        },
        TranscriptBody::ToolResult { tool, output, is_error } => format!(
            "Result of {}{}: {}",
            tool.as_deref().unwrap_or("a tool"),
            if *is_error { " failed" } else { "" },
            first(&output.text)
        ),
        TranscriptBody::Compacted { trigger, pre_tokens, post_tokens } => {
            let mut label = compacted_label(trigger, *pre_tokens, *post_tokens);
            if let Some(rest) = label.strip_prefix("compacted") {
                label = format!("Compacted{rest}");
            }
            label
        }
        TranscriptBody::Notice { level, text } => {
            let kind = match level {
                NoticeLevel::Notice => "Notice",
                NoticeLevel::Suggestion => "Suggestion",
                NoticeLevel::Warning => "Warning",
            };
            format!("{kind}: {}", first(text))
        }
    }
}

/// Markdown in an assistant turn: paragraphs one base unit apart, headings stepping down
/// from the title size to the base, code in the terminal mono at `small()` on the raised
/// surface with `radii.xs` corners. Colours come from the gpui-kit theme, which
/// [`crate::kit::sync`] keeps on the same tokens.
fn markdown_style(theme: &Theme, mono: &str) -> TextViewStyle {
    let small = theme.typography.small();
    let code_block = gpui::StyleRefinement::default()
        .font_family(mono.to_owned())
        .text_size(px(small))
        .bg(hsla(theme.surfaces.raised))
        .rounded(px(theme.radii.xs))
        .px(px(theme.spacing.sm))
        .py(px(theme.spacing.xs));
    let inline_code = gpui::HighlightStyle {
        background_color: Some(hsla(theme.surfaces.raised)),
        color: Some(hsla(theme.surfaces.text)),
        ..gpui::HighlightStyle::default()
    };
    let (title, base) = (theme.typography.title(), theme.typography.ui_size);
    TextViewStyle {
        paragraph_gap: gpui::rems(theme.spacing.sm / base),
        heading_base_font_size: px(base),
        heading_font_size: Some(std::sync::Arc::new(move |level: u8, _base| {
            px((title - f32::from(level.saturating_sub(1))).max(base))
        })),
        code_block,
        inline_code,
        ..TextViewStyle::default()
    }
}

/// A clipped text as a mono block at `small()`, with the count of what the host or the fold
/// dropped.
fn clipped_block(text: &Clipped, mono: &str, theme: &Theme) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .pl(px(theme.spacing.lg))
        .font_family(mono.to_owned())
        .text_size(px(theme.typography.small()))
        .whitespace_normal()
        .child(SharedString::from(text.text.clone()))
        .when(text.more_lines > 0, |el| {
            el.child(
                div().italic().child(SharedString::from(format!("{} more lines", text.more_lines))),
            )
        })
        .into_any_element()
}

/// What a tool call would do, under its header: an edit's diff and a todo list show
/// unasked (the diff folded past [`DIFF_PREVIEW_LINES`]); a command, a written file, a
/// subagent's brief and an unknown tool's JSON open on a click; a read or a search has
/// only its slice or filter to add and shows it when opened.
fn tool_body(detail: &ToolDetail, open: bool, mono: &str, theme: &Theme) -> Option<AnyElement> {
    let s = &theme.surfaces;
    let small = theme.typography.small();
    let block = |text: &Clipped| clipped_block(text, mono, theme);
    let line = |text: String| {
        div()
            .pl(px(theme.spacing.lg))
            .text_size(px(small))
            .text_color(hsla(s.text_muted))
            .child(SharedString::from(text))
    };
    match detail {
        ToolDetail::Diff { lines, more_lines, replace_all, .. } => {
            let shown = if open { lines.len() } else { lines.len().min(DIFF_PREVIEW_LINES) };
            let hidden = lines.len().saturating_sub(shown);
            let more = u32::try_from(hidden).unwrap_or(u32::MAX).saturating_add(*more_lines);
            let mut rows: Vec<AnyElement> = lines
                .iter()
                .take(shown)
                .map(|l| diff_row(l, mono, theme).into_any_element())
                .collect();
            if more > 0 {
                rows.push(
                    div()
                        .italic()
                        .text_color(hsla(s.text_muted))
                        .child(SharedString::from(format!("{more} more lines")))
                        .into_any_element(),
                );
            }
            if *replace_all {
                rows.push(
                    div()
                        .text_color(hsla(s.text_muted))
                        .child(SharedString::from("every occurrence"))
                        .into_any_element(),
                );
            }
            Some(
                div()
                    .id("tool-diff")
                    .flex()
                    .flex_col()
                    .pl(px(theme.spacing.lg))
                    .font_family(mono.to_owned())
                    .text_size(px(small))
                    .children(rows)
                    .into_any_element(),
            )
        }
        ToolDetail::Todos { items } => Some(
            div()
                .id("tool-todos")
                .flex()
                .flex_col()
                .pl(px(theme.spacing.lg))
                .text_size(px(small))
                .children(items.iter().map(|t| todo_row(t, theme)))
                .into_any_element(),
        ),
        ToolDetail::Command { command, description } if open => Some(
            div()
                .flex()
                .flex_col()
                .gap(px(theme.spacing.xs))
                .child(block(command))
                .children(description.as_ref().map(|d| line(d.clone())))
                .into_any_element(),
        ),
        ToolDetail::Write { content, .. } if open => Some(block(content)),
        ToolDetail::Agent { kind, prompt, .. } if open => Some(
            div()
                .flex()
                .flex_col()
                .gap(px(theme.spacing.xs))
                .children(kind.as_ref().map(|k| line(format!("as {k}"))))
                .child(block(prompt))
                .into_any_element(),
        ),
        ToolDetail::Question { questions } if open => Some(
            div()
                .flex()
                .flex_col()
                .gap(px(theme.spacing.xs))
                .children(questions.iter().map(|q| {
                    line(format!(
                        "{}: {}",
                        q.text,
                        q.options.iter().map(|o| o.label.as_str()).collect::<Vec<_>>().join(" / ")
                    ))
                }))
                .into_any_element(),
        ),
        ToolDetail::Json { input } if open => Some(block(input)),
        ToolDetail::Read { offset, limit, .. } if open => {
            let slice = match (offset, limit) {
                (Some(o), Some(n)) => format!("lines {o} to {}", o.saturating_add(*n)),
                (Some(o), None) => format!("from line {o}"),
                (None, Some(n)) => format!("first {n} lines"),
                (None, None) => "whole file".to_owned(),
            };
            Some(line(slice).into_any_element())
        }
        ToolDetail::Search { path, glob, .. } if open => {
            let mut parts = Vec::new();
            if let Some(p) = path {
                parts.push(format!("in {p}"));
            }
            if let Some(g) = glob {
                parts.push(format!("files {g}"));
            }
            if parts.is_empty() {
                parts.push("everywhere".to_owned());
            }
            Some(line(parts.join(", ")).into_any_element())
        }
        _ => None,
    }
}

/// The "+a −r" of an edit in the tool header, added in the success tone and removed in the
/// error tone; nothing for any other tool.
fn tool_badge(detail: &ToolDetail, theme: &Theme) -> Option<AnyElement> {
    let ToolDetail::Diff { lines, .. } = detail else { return None };
    let (added, removed) = diff_counts(lines);
    let s = &theme.surfaces;
    Some(
        div()
            .flex_none()
            .flex()
            .gap(px(theme.spacing.xs))
            .text_size(px(theme.typography.caption()))
            .child(div().text_color(hsla(s.success)).child(SharedString::from(format!("+{added}"))))
            .child(div().text_color(hsla(s.error)).child(SharedString::from(format!("−{removed}"))))
            .into_any_element(),
    )
}

/// Lines added and removed in a diff.
fn diff_counts(lines: &[DiffLine]) -> (usize, usize) {
    let added = lines.iter().filter(|l| l.kind == DiffKind::Added).count();
    let removed = lines.iter().filter(|l| l.kind == DiffKind::Removed).count();
    (added, removed)
}

/// One diff line: a sign column, then the text, the row tinted in the success tone for an
/// addition, the error tone for a removal, nothing for context.
fn diff_row(line: &DiffLine, mono: &str, theme: &Theme) -> impl IntoElement {
    let s = &theme.surfaces;
    let (sign, tone) = match line.kind {
        DiffKind::Context => (" ", None),
        DiffKind::Added => ("+", Some(s.success)),
        DiffKind::Removed => ("−", Some(s.error)),
    };
    div()
        .flex()
        .w_full()
        .px(px(theme.spacing.xs))
        .font_family(mono.to_owned())
        .text_color(hsla(if tone.is_some() { s.text } else { s.text_muted }))
        .when_some(tone, |el, tone| el.bg(hsla_alpha(tone, alpha::TINT)))
        .child(
            div()
                .flex_none()
                .w(px(theme.spacing.md))
                .text_color(hsla(tone.unwrap_or(s.text_muted)))
                .child(sign),
        )
        .child(div().flex_1().min_w(px(0.0)).whitespace_normal().child(SharedString::from(
            if line.text.is_empty() { " ".to_owned() } else { line.text.clone() },
        )))
}

/// One todo: a mark for its state, then the text; done items are struck through and muted,
/// the one in progress is in the accent tone.
fn todo_row(todo: &Todo, theme: &Theme) -> impl IntoElement {
    let s = &theme.surfaces;
    let (mark, color) = match todo.status {
        TodoStatus::Pending => ("○", s.text_secondary),
        TodoStatus::InProgress => ("●", s.accent),
        TodoStatus::Completed => ("✓", s.text_muted),
    };
    div()
        .flex()
        .gap(px(theme.spacing.sm))
        .text_color(hsla(color))
        .child(div().flex_none().w(px(theme.spacing.md)).child(mark))
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .whitespace_normal()
                .when(todo.status == TodoStatus::Completed, gpui::Styled::line_through)
                .child(SharedString::from(todo.text.clone())),
        )
}

/// A fold's header: "▸ thinking" / "▾ thinking", with the line count while folded.
fn fold_label(open: bool, what: &str, text: &Clipped) -> String {
    if open {
        return format!("{} {what}", arrow(open));
    }
    let lines =
        text.text.lines().count().saturating_add(usize::try_from(text.more_lines).unwrap_or(0));
    format!("{} {what} ({lines} lines)", arrow(open))
}

const fn arrow(open: bool) -> &'static str {
    if open { "▾" } else { "▸" }
}

/// A result's visible part: everything when open, else its first
/// [`RESULT_PREVIEW_LINES`], with the lines beyond them counted as more.
fn preview(output: &Clipped, open: bool) -> (Clipped, usize) {
    let total = output.text.lines().count();
    if open || total <= RESULT_PREVIEW_LINES {
        return (output.clone(), 0);
    }
    let head: Vec<&str> = output.text.lines().take(RESULT_PREVIEW_LINES).collect();
    let hidden = total.saturating_sub(RESULT_PREVIEW_LINES);
    let more = u32::try_from(hidden).unwrap_or(u32::MAX).saturating_add(output.more_lines);
    (Clipped { text: head.join("\n"), more_lines: more }, hidden)
}

/// A record's time of day in the local zone, "HH:MM".
#[must_use]
pub fn clock(at: Option<u64>) -> Option<String> {
    let ms = i64::try_from(at?).ok()?;
    let utc = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms)?;
    Some(utc.with_timezone(&chrono::Local).format("%H:%M").to_string())
}
