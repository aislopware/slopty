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

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, Context, ElementId, Entity, FocusHandle, Focusable as _,
    FollowMode, InteractiveElement as _, IntoElement, ListAlignment, ListOffset, ListState,
    MouseButton, ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _,
    Subscription, Window, div, list, px,
};
use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use gpui_kit::component::text::TextView;
use slopty_proto::agent::{
    AgentInfo, AgentTask, Clipped, DiffKind, DiffLine, NoticeLevel, Question, SlashCommand, Todo,
    TodoStatus, ToolDetail, TranscriptBody, TranscriptEntry, TranscriptUpdate,
};
use slopty_theme::{Theme, alpha};

use crate::a11y::tab_stop;
use crate::colors::{hsla, hsla_alpha};
use crate::highlight::{self, Span, Syntax};
use crate::terminal::view::{TerminalView, TerminalViewEvent};

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

/// One line of the completion list: what Tab puts in the composer and what the line says.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Completion {
    /// What replaces the word being completed (`/compact`, `@src/main.rs`).
    pub insert: String,
    /// The argument hint after it, if any.
    pub hint: String,
    /// The description after it, if any.
    pub description: String,
}

impl From<&SlashCommand> for Completion {
    fn from(command: &SlashCommand) -> Self {
        Self {
            insert: command.name.clone(),
            hint: command.hint.clone(),
            description: command.description.clone(),
        }
    }
}

/// The `@file` word the composer's text ends in — what is typed after the `@` — when it
/// does: a word that starts with `@` and is not the whole `/…` command. The host is asked
/// for the paths it matches.
#[must_use]
pub fn file_query(text: &str) -> Option<&str> {
    let word = text.rsplit(char::is_whitespace).next().unwrap_or(text);
    word.strip_prefix('@')
}

/// The paths the host found for the `@` word, as the list shows them.
#[must_use]
pub fn file_completions(paths: &[String]) -> Vec<Completion> {
    paths.iter().map(|p| Completion { insert: format!("@{p}"), ..Completion::default() }).collect()
}

/// A completion as one line reads it (its a11y label): what it inserts, the argument hint
/// after it, the description after a dash — each only where there is one.
#[must_use]
pub fn completion_label(completion: &Completion) -> String {
    let mut label = completion.insert.clone();
    if !completion.hint.is_empty() {
        label.push(' ');
        label.push_str(&completion.hint);
    }
    if !completion.description.is_empty() {
        label.push_str(" — ");
        label.push_str(&completion.description);
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

/// The worktree an agent runs in, from the directory it reports: the name under the
/// repository's `.claude/worktrees`, where Claude Code makes them; `None` for any other
/// directory.
#[must_use]
pub fn worktree_name(cwd: &str) -> Option<&str> {
    let (_, rest) = cwd.split_once("/.claude/worktrees/")?;
    let name = rest.split('/').next().unwrap_or(rest);
    (!name.is_empty()).then_some(name)
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
    /// Which sent prompt (an index into [`Self::prompts`]) the composer shows for ↑ / ↓;
    /// none while it holds the human's own draft.
    recall: Option<usize>,
    /// The draft set aside while a prompt is recalled, put back past the newest one.
    draft: String,
    /// The model menu under the header is open.
    model_menu: bool,
    /// Which slash completion is selected, an index into the current matches.
    completion: usize,
    /// Esc hid the completions for the text as it is now; typing shows them again.
    completions_hidden: bool,
    /// Entries holding the find bar's needle, in order; shared with the render closure.
    hits: Rc<[usize]>,
    /// Index into `hits` of the one the reader is on.
    hit: Option<usize>,
    /// An edit's diff coloured by its file's grammar, by entry index, parsed on first draw
    /// (`None`: no grammar for the path); entries only append or reset, so an index stays
    /// good until a reset clears the map. Shared with the render closure.
    diffs: Rc<DiffCache>,
}

/// An edit's coloured lines by entry index; `None` for an edit whose path has no grammar.
type DiffCache = RefCell<HashMap<usize, Option<Rc<[Vec<Span>]>>>>;

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
            recall: None,
            draft: String::new(),
            hits: Rc::from([]),
            hit: None,
            diffs: Rc::default(),
        }
    }

    /// The find bar's hits (entry indices) and which one the reader is on; empty and none
    /// while nothing is sought.
    #[must_use]
    pub fn hits(&self) -> (&[usize], Option<usize>) {
        (&self.hits, self.hit)
    }

    /// The find bar's hits changed: `current` is drawn stronger than the rest.
    pub fn set_hits(&mut self, hits: Vec<usize>, current: Option<usize>) {
        self.hits = Rc::from(hits);
        self.hit = current;
    }

    /// Scroll entry `ix` into view and stop following the tail, so the agent's next line
    /// does not pull the reader off it (the list follows again from the bottom).
    pub fn reveal_entry(&self, ix: usize) {
        self.list.scroll_to_reveal_item(ix);
        self.list.pause_following_tail();
    }

    /// Scroll entry `ix`'s top edge to the top of the list and stop following the tail.
    pub fn scroll_to_entry(&self, ix: usize) {
        self.list.scroll_to(ListOffset { item_ix: ix, offset_in_item: px(0.0) });
        self.list.pause_following_tail();
    }

    /// The entry at the top of the list's viewport and whether its top edge is above it (the
    /// reader is part-way through it); none before the list has been drawn. A list following
    /// the tail keeps no top, so this steps off the tail from the very end first, which lands
    /// exactly where the reader is (the last entry ends at the content's end): it stops the
    /// following.
    #[must_use]
    pub fn top_entry(&self) -> Option<(usize, bool)> {
        if self.list.is_following_tail() {
            self.list.scroll_by(-self.list.viewport_bounds().size.height);
        }
        let top = self.list.logical_scroll_top();
        (top.item_ix < self.entries.len()).then_some((top.item_ix, top.offset_in_item > px(0.0)))
    }

    /// The newest answer's Markdown, for ⌘⇧C (as the grid copies the newest block's output).
    #[must_use]
    pub fn last_answer(&self) -> Option<&str> {
        self.entries.iter().rev().find_map(|entry| match &entry.body {
            TranscriptBody::Assistant { markdown } => Some(markdown.as_str()),
            _ => None,
        })
    }

    /// The prompt (a `User` entry) to scroll to for ⌘↑ (`delta` −1: the one above the
    /// viewport's top, or the top one when the reader is part-way through it) or ⌘↓ (+1:
    /// the one below); none when there is none that way.
    #[must_use]
    pub fn prompt_from_top(&self, delta: i8) -> Option<usize> {
        if delta > 0 && self.pinned() {
            // Following the tail: nothing is below.
            return None;
        }
        let (top, cut) = self.top_entry()?;
        let is_prompt = |ix: &usize| {
            matches!(self.entries.get(*ix).map(|e| &e.body), Some(TranscriptBody::User { .. }))
        };
        if delta < 0 {
            (0..top.saturating_add(usize::from(cut))).rev().find(is_prompt)
        } else {
            (top.saturating_add(1)..self.entries.len()).find(is_prompt)
        }
    }

    /// What completes the composer's text, unless Esc hid the list: the slash commands a
    /// `/…` text is a prefix of, else the paths the host found for the `@` word the text
    /// ends in (`files` is the host's answer, kept only while its query is that word).
    #[must_use]
    pub fn completions(
        &self,
        commands: &[SlashCommand],
        files: Option<&(String, Vec<String>)>,
        cx: &App,
    ) -> Vec<Completion> {
        if self.completions_hidden {
            return Vec::new();
        }
        let text = self.composer_text(cx);
        let slash: Vec<Completion> =
            slash_matches(&text, commands).iter().map(Completion::from).collect();
        if !slash.is_empty() {
            return slash;
        }
        match (file_query(&text), files) {
            (Some(query), Some((asked, paths))) if !query.is_empty() && asked == query => {
                file_completions(paths)
            }
            _ => Vec::new(),
        }
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
        // Typed into a recalled prompt: it is the human's own text now.
        self.recall = None;
    }

    /// The prompts sent so far, oldest first, a repeat of the one before it dropped.
    fn prompts(entries: &[TranscriptEntry]) -> Vec<&str> {
        let mut out: Vec<&str> = Vec::new();
        for entry in entries {
            if let TranscriptBody::User { text, .. } = &entry.body
                && !text.is_empty()
                && out.last() != Some(&text.as_str())
            {
                out.push(text);
            }
        }
        out
    }

    /// ↑ (`delta` −1) / ↓ (+1) in the composer as a shell's history: ↑ on an empty composer
    /// shows the newest prompt sent, ↑ / ↓ from there the older / newer ones, and ↓ past the
    /// newest puts the draft back. `false` when the key is not a recall (text of the human's
    /// own under the caret), so it moves the caret as it would.
    pub fn recall(&mut self, delta: i8, window: &mut Window, cx: &mut App) -> bool {
        let entries = Rc::clone(&self.entries);
        let prompts = Self::prompts(&entries);
        let Some(newest) = prompts.len().checked_sub(1) else { return false };
        let at = match self.recall {
            None if delta < 0 && self.composer_text(cx).is_empty() => Some(newest),
            None => return false,
            Some(at) if delta < 0 => Some(at.saturating_sub(1)),
            Some(at) => at.checked_add(1).filter(|next| *next <= newest),
        };
        if self.recall.is_none() {
            self.draft = self.composer_text(cx);
        }
        let value = match at {
            Some(at) => prompts.get(at).map(|p| (*p).to_owned()).unwrap_or_default(),
            None => std::mem::take(&mut self.draft),
        };
        self.recall = at;
        self.composer.update(cx, |input, cx| {
            let end = value.len();
            input.set_value(value, window, cx);
            // The caret at the end, as a shell puts it after a recalled line.
            input.set_selected_range(end..end, cx);
        });
        true
    }

    /// A click on the prompt at `ix`: it is in the composer, the caret at its end, as if
    /// recalled — ↑ / ↓ go on from it and ↓ past the newest puts the draft back. `false` for
    /// an entry that is not a prompt with text.
    pub fn reuse(&mut self, ix: usize, window: &mut Window, cx: &mut App) -> bool {
        let entries = Rc::clone(&self.entries);
        let Some(TranscriptBody::User { text, .. }) = entries.get(ix).map(|e| &e.body) else {
            return false;
        };
        if text.is_empty() {
            return false;
        }
        // Its place in the recall order: the prompts up to and including it, repeats dropped.
        let at = Self::prompts(entries.get(..=ix).unwrap_or_default()).len().saturating_sub(1);
        if self.recall.is_none() {
            self.draft = self.composer_text(cx);
        }
        self.recall = Some(at);
        self.composer.update(cx, |input, cx| {
            let end = text.len();
            input.set_value(text.clone(), window, cx);
            input.set_selected_range(end..end, cx);
            input.focus(window, cx);
        });
        true
    }

    /// Esc: hide the completions until the text changes.
    pub const fn hide_completions(&mut self) {
        self.completions_hidden = true;
    }

    /// Put `insert` and a space in the composer, the caret after them: a `/command`
    /// replaces the text, an `@path` replaces the `@` word the text ends in. A directory
    /// (`@docs/`) gets no space, so the next keystrokes descend into it.
    pub fn complete(&mut self, insert: &str, window: &mut Window, cx: &mut App) {
        let mut text = if insert.starts_with('@') {
            let current = self.composer_text(cx);
            let word = file_query(&current).map_or(0, |q| q.len().saturating_add(1));
            current.get(..current.len().saturating_sub(word)).unwrap_or_default().to_owned()
        } else {
            String::new()
        };
        text.push_str(insert);
        if !insert.ends_with('/') {
            text.push(' ');
        }
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

    /// Append `text` to the composer's text (a block asked of the agent lands after whatever
    /// the human had started to type).
    pub fn append_composer_text(&self, text: &str, window: &mut Window, cx: &mut App) {
        let mut value = self.composer_text(cx);
        value.push_str(text);
        self.composer.update(cx, |input, cx| input.set_value(value, window, cx));
    }

    /// Take the composer's text, leaving it empty.
    #[must_use]
    pub fn take_composer_text(&mut self, window: &mut Window, cx: &mut App) -> String {
        let text = self.composer_text(cx);
        self.composer.update(cx, |input, cx| input.clean(window, cx));
        self.recall = None;
        self.draft.clear();
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
            self.diffs.borrow_mut().clear();
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
        snapshots: &[(slopty_proto::screen::CaptureTarget, String)],
        preparing: usize,
        composer_focused: bool,
        working: bool,
        files: Option<&(String, Vec<String>)>,
        can_run: bool,
        find: Option<AnyElement>,
        theme: &Theme,
        cx: &Context<TerminalView>,
    ) -> AnyElement {
        let completions =
            info.map(|i| self.completions(&i.slash_commands, files, cx)).unwrap_or_default();
        let entries = Rc::clone(&self.entries);
        let open = Rc::clone(&self.open);
        let tasks = Rc::clone(&self.tasks);
        let hits = Rc::clone(&self.hits);
        let current = self.hit.and_then(|c| hits.get(c).copied());
        let diffs = Rc::clone(&self.diffs);
        let theme = theme.clone();
        let empty = entries.is_empty();
        let view = cx.entity();
        let s = theme.surfaces;
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
            .children(find)
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
                                            let run = can_run.then_some(&view);
                                            let hit = hits
                                                .binary_search(&ix)
                                                .is_ok()
                                                .then_some(current == Some(ix));
                                            let diff = diff_spans_cached(&diffs, ix, e);
                                            entry(
                                                ix,
                                                e,
                                                open.contains(&ix),
                                                task,
                                                run,
                                                hit,
                                                diff.as_deref(),
                                                &view,
                                                &theme,
                                            )
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
            .when(!attachments.is_empty() || !snapshots.is_empty() || preparing > 0, |el| {
                el.child(attachments_row(attachments, snapshots, preparing, &theme, cx))
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
        let worktree = info.cwd.as_deref().and_then(worktree_name).map(str::to_owned);
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
                    .when_some(worktree, |el, name| {
                        el.child(chip(
                            "conversation-worktree",
                            format!("⎇ {name}"),
                            format!("Worktree: {name}"),
                        ))
                    })
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
        completions: &[Completion],
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
            .children(completions.iter().enumerate().map(|(ix, completion)| {
                let chosen = ix == selected;
                let insert = completion.insert.clone();
                let label = completion_label(completion);
                let hint = (!completion.hint.is_empty()).then(|| completion.hint.clone());
                let description =
                    (!completion.description.is_empty()).then(|| completion.description.clone());
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
                    .on_mouse_down(MouseButton::Left, |_ev, _window, cx| {
                        // The composer keeps the caret; the click must not blur it.
                        cx.stop_propagation();
                    })
                    .on_click(cx.listener(move |this, _ev, window, cx| {
                        this.complete_with(&insert, window, cx);
                    }))
                    .child(
                        div()
                            .flex_none()
                            .font_family(mono.clone())
                            .child(SharedString::from(completion.insert.clone())),
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
                .style(crate::markdown::style(theme, &mono, 1.0)),
        )
        .into_any_element()
}

/// What the agent waits for, with the one-tap answers, above the composer: the warn tone,
/// faint, since the agent is blocked on the human.
/// The pictures waiting to go with the next prompt, one chip each (`composer-attachment-<i>`,
/// a button that drops it), the windows whose picture the host will take
/// (`composer-snapshot-<i>`, likewise), and a muted chip for pictures still being made fit
/// (`composer-attachment-preparing`).
fn attachments_row(
    attachments: &[slopty_proto::agent::Image],
    snapshots: &[(slopty_proto::screen::CaptureTarget, String)],
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
        .children(snapshots.iter().enumerate().map(|(i, (_target, title))| {
            let chip = div()
                .id(("composer-snapshot", i))
                .debug_selector(move || format!("composer-snapshot-{i}"))
                .role(Role::Button)
                .aria_label(SharedString::from(format!("Remove window {title}")))
                .px(px(spacing.sm))
                .py(px(spacing.xs))
                .rounded(px(theme.radii.sm))
                .border_1()
                .border_color(hsla(s.border))
                .bg(hsla(s.raised))
                .text_size(px(theme.typography.ui_size))
                .text_color(hsla(s.text))
                .cursor_pointer()
                .child(SharedString::from(format!("🖥 {title} ×")));
            tab_stop(chip, s.accent).on_click(cx.listener(move |view, _ev, _window, cx| {
                view.remove_snapshot(i, cx);
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
#[expect(clippy::too_many_arguments, reason = "one call site: the list's render closure")]
fn entry(
    ix: usize,
    entry: &TranscriptEntry,
    open: bool,
    task: Option<&AgentTask>,
    run: Option<&Entity<TerminalView>>,
    hit: Option<bool>,
    diff: Option<&[Vec<Span>]>,
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
        .py(px(spacing.xs))
        // A find hit is washed in the warn colour, the current one stronger, as the grid's.
        .when_some(hit, |el, current| {
            el.bg(hsla_alpha(s.warn, if current { alpha::TINT_STRONG } else { alpha::TINT }))
        });
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
                    .id(ElementId::NamedInteger(
                        "conversation-reuse".into(),
                        u64::try_from(ix).unwrap_or(0),
                    ))
                    .debug_selector(move || format!("conversation-reuse-{ix}"))
                    .role(Role::Button)
                    .aria_label("Edit and send again")
                    .max_w(px(ui_size * 40.0))
                    .px(px(spacing.md))
                    .py(px(spacing.sm))
                    .rounded(px(theme.radii.md))
                    .bg(hsla_alpha(s.accent, alpha::TINT_STRONG))
                    .line_height(prose)
                    .cursor_pointer()
                    .hover(move |st| st.bg(hsla_alpha(s.accent, alpha::TINT_PRESSED)))
                    .on_click({
                        let view = view.clone();
                        move |_ev, window, cx| {
                            cx.stop_propagation();
                            view.update(cx, |v, cx| v.reuse_prompt(ix, window, cx));
                        }
                    })
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
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .line_height(prose)
                    .flex()
                    .flex_col()
                    .gap(px(spacing.xs))
                    .children(segments(markdown).into_iter().enumerate().map(|(si, segment)| {
                        match segment {
                            Segment::Prose(text) => TextView::markdown(
                                ElementId::Name(format!("conversation-md-{ix}-{si}").into()),
                                SharedString::from(text),
                            )
                            .style(crate::markdown::style(theme, &mono, 1.0))
                            .into_any_element(),
                            Segment::Code { lang, body } => {
                                code_segment(ix, si, &lang, &body, run, theme)
                            }
                        }
                    })),
            )
            .children(time)
            .child(copy_button(ix, markdown, theme))
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
                    .children(view_button(ix, detail, view, theme))
                    .children(run.and_then(|r| open_button(ix, detail, r, theme)))
                    .on_click(toggle),
            )
            .children(task.map(|t| task_line(t, theme)))
            .children(tool_body(detail, open, diff, &mono, theme))
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
                .when(!shown.text.is_empty(), |el| {
                    el.child(result_block(ix, &shown, view, &mono, theme))
                })
                .into_any_element()
        }
    }
}

/// A tool's result, line by line: a line that names a file (a grep hit's `src/a.rs:12:`, a
/// compiler's ` --> src/b.rs:3:5`) is a button that views the file on the canvas at that
/// line; the rest is text.
fn result_block(
    ix: usize,
    text: &Clipped,
    view: &Entity<TerminalView>,
    mono: &str,
    theme: &Theme,
) -> AnyElement {
    let s = &theme.surfaces;
    let wash = hsla_alpha(s.text, alpha::HOVER);
    let rows: Vec<AnyElement> = text
        .text
        .lines()
        .enumerate()
        .map(|(n, line)| match crate::terminal::url::first_path(line) {
            Some((_, path, at)) => {
                let id = format!("result-path-{ix}-{n}");
                let label = at.map_or_else(|| path.clone(), |l| format!("{path}:{l}"));
                let view = view.clone();
                div()
                    .id(ElementId::Name(id.clone().into()))
                    .debug_selector(move || id)
                    .role(Role::Button)
                    .aria_label(SharedString::from(format!("View {label} on the canvas")))
                    .cursor_pointer()
                    .rounded(px(theme.radii.xs))
                    .hover(move |st| st.bg(wash))
                    .on_mouse_down(MouseButton::Left, |_ev, _window, cx| cx.stop_propagation())
                    .on_click(move |_ev, _window, cx| {
                        cx.stop_propagation();
                        view.update(cx, |v, cx| v.view_file(&path, at, cx));
                    })
                    .child(SharedString::from(line.to_owned()))
                    .into_any_element()
            }
            None => div().child(SharedString::from(line.to_owned())).into_any_element(),
        })
        .collect();
    div()
        .flex()
        .flex_col()
        .pl(px(theme.spacing.lg))
        .font_family(mono.to_owned())
        .text_size(px(theme.typography.small()))
        .whitespace_normal()
        .children(rows)
        .when(text.more_lines > 0, |el| {
            el.child(
                div().italic().child(SharedString::from(format!("{} more lines", text.more_lines))),
            )
        })
        .into_any_element()
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

/// The conversation as Markdown, to paste elsewhere.
///
/// "You" and "Claude" headed turns, a tool call as a quoted line, a failed result's first
/// line, a compaction as a rule with its numbers, a notice quoted in italics. Thinking and
/// tool output stay out — they are the agent's working, folded on screen too.
#[must_use]
pub fn as_markdown(entries: &[TranscriptEntry]) -> String {
    let first = |text: &str| text.lines().find(|l| !l.trim().is_empty()).unwrap_or("").to_owned();
    let mut parts: Vec<String> = Vec::new();
    for entry in entries {
        let part = match &entry.body {
            TranscriptBody::User { text, images } => {
                let mut part = "**You**\n\n".to_owned();
                if *images > 0 {
                    part.push('_');
                    part.push_str(&pictures_label(*images));
                    part.push_str("_\n\n");
                }
                part.push_str(text.trim_end());
                part
            }
            TranscriptBody::Assistant { markdown } => {
                format!("**Claude**\n\n{}", markdown.trim_end())
            }
            TranscriptBody::ToolUse { name, summary, .. } => format!("> **{name}** {summary}"),
            TranscriptBody::ToolResult { tool, output, is_error: true } => format!(
                "> **{} failed:** {}",
                tool.as_deref().unwrap_or("a tool"),
                first(&output.text)
            ),
            TranscriptBody::Compacted { trigger, pre_tokens, post_tokens } => {
                format!("---\n\n_{}_", compacted_label(trigger, *pre_tokens, *post_tokens))
            }
            TranscriptBody::Notice { text, .. } => format!("> _{}_", text.trim_end()),
            TranscriptBody::Thinking { .. } | TranscriptBody::ToolResult { .. } => continue,
        };
        parts.push(part);
    }
    let mut out = parts.join("\n\n");
    if !out.is_empty() {
        out.push('\n');
    }
    out
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

/// What the find bar searches in an entry.
///
/// The words a reader would look for (the prompt, the answer, the thinking, a call's name and
/// summary, a result, a notice), without the chrome the card adds around them.
#[must_use]
pub fn entry_text(entry: &TranscriptEntry) -> String {
    match &entry.body {
        TranscriptBody::User { text, .. } | TranscriptBody::Notice { text, .. } => text.clone(),
        TranscriptBody::Assistant { markdown } => markdown.clone(),
        TranscriptBody::Thinking { text } => text.text.clone(),
        TranscriptBody::ToolUse { name, summary, .. } => format!("{name} {summary}"),
        TranscriptBody::ToolResult { output, .. } => output.text.clone(),
        TranscriptBody::Compacted { trigger, pre_tokens, post_tokens } => {
            compacted_label(trigger, *pre_tokens, *post_tokens)
        }
    }
}

/// The entries holding `needle` (a regular expression when `regex`), in order.
///
/// None for an empty needle. Smart case, the terminal's rule: a needle with no capital
/// matches in any case, one with a capital as typed. `Err` is the regex's complaint.
///
/// # Errors
///
/// When `regex` is set and `needle` does not compile.
pub fn entry_hits(
    entries: &[TranscriptEntry],
    needle: &str,
    regex: bool,
) -> Result<Vec<usize>, String> {
    if needle.is_empty() {
        return Ok(Vec::new());
    }
    let insensitive = !needle.chars().any(char::is_uppercase);
    let matcher: Box<dyn Fn(&str) -> bool> = if regex {
        let re = regex::RegexBuilder::new(needle)
            .case_insensitive(insensitive)
            .build()
            .map_err(|e| e.to_string())?;
        Box::new(move |text| re.is_match(text))
    } else if insensitive {
        let needle = needle.to_lowercase();
        Box::new(move |text| text.to_lowercase().contains(&needle))
    } else {
        let needle = needle.to_owned();
        Box::new(move |text| text.contains(&needle))
    };
    Ok(entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| matcher(&entry_text(entry)))
        .map(|(ix, _)| ix)
        .collect())
}

/// A piece of an assistant turn: the prose between the fences, or one fenced code block.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Segment {
    /// Markdown between the fences.
    Prose(String),
    /// One fenced block: the language after the opening fence (may be empty) and the lines
    /// inside, without a trailing newline.
    Code {
        /// What followed the opening fence.
        lang: String,
        /// The lines inside.
        body: String,
    },
}

/// `markdown` split at its fences.
///
/// A line starting with three backticks opens a block (the rest of the line is its
/// language), the next such line closes it, an unclosed block runs to the end. Blank prose
/// between blocks is dropped.
#[must_use]
pub fn segments(markdown: &str) -> Vec<Segment> {
    let mut out = Vec::new();
    let mut prose: Vec<&str> = Vec::new();
    let mut code: Option<(String, Vec<&str>)> = None;
    for line in markdown.lines() {
        let trimmed = line.trim_start();
        match code.take() {
            Some((lang, body)) if trimmed.starts_with("```") => {
                out.push(Segment::Code { lang, body: body.join("\n") });
            }
            Some((lang, mut body)) => {
                body.push(line);
                code = Some((lang, body));
            }
            None if trimmed.starts_with("```") => {
                if prose.iter().any(|l| !l.trim().is_empty()) {
                    out.push(Segment::Prose(prose.join("\n")));
                }
                prose.clear();
                code = Some((trimmed.trim_start_matches('`').trim().to_owned(), Vec::new()));
            }
            None => prose.push(line),
        }
    }
    if let Some((lang, body)) = code {
        out.push(Segment::Code { lang, body: body.join("\n") });
    }
    if prose.iter().any(|l| !l.trim().is_empty()) {
        out.push(Segment::Prose(prose.join("\n")));
    }
    out
}

/// A fenced block of an assistant turn: its language, a "copy" button and — when `run` is
/// the view to ask, which the canvas only allows while it has a plain shell to run it in —
/// a "run" button, over the code in the mono face on the raised surface.
/// A "copy" button at the end of an answer: the whole answer's Markdown on the clipboard
/// (⌘⇧C copies the newest only).
fn copy_button(ix: usize, markdown: &str, theme: &Theme) -> AnyElement {
    let s = &theme.surfaces;
    let text = markdown.to_owned();
    let id = format!("conversation-copy-{ix}");
    div()
        .id(ElementId::Name(id.clone().into()))
        .debug_selector(move || id)
        .role(Role::Button)
        .aria_label("Copy answer")
        .flex_none()
        .px(px(theme.spacing.xs))
        .rounded(px(theme.radii.xs))
        .cursor_pointer()
        .text_size(px(theme.typography.caption()))
        .text_color(hsla(s.text_muted))
        .hover(move |st| st.bg(hsla_alpha(s.text, alpha::HOVER)))
        .on_mouse_down(MouseButton::Left, |_ev, _window, cx| cx.stop_propagation())
        .on_click(move |_ev, _window, cx| {
            cx.stop_propagation();
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(text.clone()));
        })
        .child("copy")
        .into_any_element()
}

fn code_segment(
    ix: usize,
    si: usize,
    lang: &str,
    body: &str,
    run: Option<&Entity<TerminalView>>,
    theme: &Theme,
) -> AnyElement {
    let s = &theme.surfaces;
    let spacing = theme.spacing;
    let mono = theme.typography.mono_families.first().cloned().unwrap_or_default();
    let text = body.to_owned();
    let copy_id = format!("conversation-code-copy-{ix}-{si}");
    let copy = div()
        .id(ElementId::Name(copy_id.clone().into()))
        .debug_selector(move || copy_id)
        .role(Role::Button)
        .aria_label("Copy code")
        .px(px(spacing.xs))
        .rounded(px(theme.radii.xs))
        .cursor_pointer()
        .text_color(hsla(s.text_muted))
        .hover(move |st| st.bg(hsla_alpha(s.text, alpha::HOVER)))
        .on_click(move |_ev, _window, cx| {
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(text.clone()));
        })
        .child("copy");
    let run = run.map(|view| {
        let view = view.clone();
        let code = body.to_owned();
        let run_id = format!("conversation-code-run-{ix}-{si}");
        div()
            .id(ElementId::Name(run_id.clone().into()))
            .debug_selector(move || run_id)
            .role(Role::Button)
            .aria_label("Run in shell")
            .px(px(spacing.xs))
            .rounded(px(theme.radii.xs))
            .cursor_pointer()
            .text_color(hsla(s.text_muted))
            .hover(move |st| st.bg(hsla_alpha(s.text, alpha::HOVER)))
            .on_click(move |_ev, _window, cx| {
                // The view knows the code; the canvas, which is subscribed, knows the shells.
                let code = code.clone();
                view.update(cx, |_v, cx| cx.emit(TerminalViewEvent::RunInShell(code)));
            })
            .child("run")
    });
    div()
        .flex()
        .flex_col()
        .rounded(px(theme.radii.xs))
        .bg(hsla(s.raised))
        .px(px(spacing.sm))
        .py(px(spacing.xs))
        .font_family(mono)
        .text_size(px(theme.typography.small()))
        .child(
            div()
                .flex()
                .justify_between()
                .items_center()
                .text_color(hsla(s.text_muted))
                .child(SharedString::from(lang.to_owned()))
                .child(div().flex().items_center().gap(px(spacing.xs)).children(run).child(copy)),
        )
        .child(
            div()
                .whitespace_normal()
                .text_color(hsla(s.text))
                .child(SharedString::from(body.to_owned())),
        )
        .into_any_element()
}

/// The file a tool call touched, when it named one — an edit, a write or a read — and the
/// line to open it at: a read's first line, when the agent asked for a slice.
#[must_use]
pub fn tool_path(detail: &ToolDetail) -> Option<(&str, Option<u32>)> {
    match detail {
        ToolDetail::Diff { path, line, .. } => Some((path, *line)),
        ToolDetail::Write { path, .. } => Some((path, None)),
        ToolDetail::Read { path, offset, .. } => Some((path, *offset)),
        _ => None,
    }
}

/// A "view" button on a tool call that named a file: a file card for it on the canvas, the
/// way to read the file on a phone. Always drawn: a card needs no shell.
fn view_button(
    ix: usize,
    detail: &ToolDetail,
    view: &Entity<TerminalView>,
    theme: &Theme,
) -> Option<AnyElement> {
    let (path, line) = tool_path(detail)?;
    let path = path.to_owned();
    let s = &theme.surfaces;
    let view = view.clone();
    let id = format!("conversation-view-{ix}");
    Some(
        div()
            .id(ElementId::Name(id.clone().into()))
            .debug_selector(move || id)
            .role(Role::Button)
            .aria_label(SharedString::from(format!("View {path} on the canvas")))
            .flex_none()
            .px(px(theme.spacing.xs))
            .rounded(px(theme.radii.xs))
            .cursor_pointer()
            .text_color(hsla(s.text_muted))
            .hover(move |st| st.bg(hsla_alpha(s.text, alpha::HOVER)))
            .on_mouse_down(MouseButton::Left, |_ev, _window, cx| cx.stop_propagation())
            .on_click(move |_ev, _window, cx| {
                cx.stop_propagation();
                view.update(cx, |v, cx| v.view_file(&path, line, cx));
            })
            .child("view")
            .into_any_element(),
    )
}

/// An "open" button on a tool call that named a file: it types the editor command for that
/// file into the canvas's shell (`run` is the view to ask, as for a fenced block). The
/// header row it sits in folds the call on a click, so the button keeps its own.
fn open_button(
    ix: usize,
    detail: &ToolDetail,
    run: &Entity<TerminalView>,
    theme: &Theme,
) -> Option<AnyElement> {
    let (path, line) = tool_path(detail)?;
    let path = path.to_owned();
    let s = &theme.surfaces;
    let view = run.clone();
    let id = format!("conversation-open-{ix}");
    Some(
        div()
            .id(ElementId::Name(id.clone().into()))
            .debug_selector(move || id)
            .role(Role::Button)
            .aria_label(SharedString::from(format!("Open {path} in the editor")))
            .flex_none()
            .px(px(theme.spacing.xs))
            .rounded(px(theme.radii.xs))
            .cursor_pointer()
            .text_color(hsla(s.text_muted))
            .hover(move |st| st.bg(hsla_alpha(s.text, alpha::HOVER)))
            .on_mouse_down(MouseButton::Left, |_ev, _window, cx| cx.stop_propagation())
            .on_click(move |_ev, _window, cx| {
                cx.stop_propagation();
                let command = crate::terminal::url::editor_command(&path, line);
                view.update(cx, |_v, cx| cx.emit(TerminalViewEvent::RunInShell(command)));
            })
            .child("open")
            .into_any_element(),
    )
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
fn tool_body(
    detail: &ToolDetail,
    open: bool,
    diff: Option<&[Vec<Span>]>,
    mono: &str,
    theme: &Theme,
) -> Option<AnyElement> {
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
            let font = crate::fonts::terminal_font(mono, false, false);
            let mut rows: Vec<AnyElement> = lines
                .iter()
                .take(shown)
                .enumerate()
                .map(|(i, l)| {
                    let spans = diff.and_then(|d| d.get(i)).map(Vec::as_slice);
                    diff_row(l, spans, &font, mono, theme).into_any_element()
                })
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

/// The diff's lines coloured by the grammar `path` names, one span list per line in the
/// diff's order; none when the bundle has no grammar for the path.
///
/// Each side is parsed as its own text — the old side is the context and removed lines, the
/// new side the context and added lines — so a removed line that opens a block comment does
/// not bleed into the lines that replaced it. Context lines take the new side's spans.
fn diff_spans(path: &str, lines: &[DiffLine]) -> Option<Rc<[Vec<Span>]>> {
    let syntax = Syntax::for_path(path, "")?;
    let side = |keep: DiffKind| -> String {
        lines
            .iter()
            .filter(|l| l.kind != keep)
            .map(|l| l.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    };
    let old = highlight::spans(&side(DiffKind::Added), syntax);
    let new = highlight::spans(&side(DiffKind::Removed), syntax);
    let (mut old_at, mut new_at) = (0_usize, 0_usize);
    let mut out = Vec::with_capacity(lines.len());
    for line in lines {
        let spans = match line.kind {
            DiffKind::Context => {
                old_at = old_at.saturating_add(1);
                let at = new_at;
                new_at = new_at.saturating_add(1);
                new.get(at)
            }
            DiffKind::Removed => {
                let at = old_at;
                old_at = old_at.saturating_add(1);
                old.get(at)
            }
            DiffKind::Added => {
                let at = new_at;
                new_at = new_at.saturating_add(1);
                new.get(at)
            }
        };
        out.push(spans.cloned().unwrap_or_default());
    }
    Some(Rc::from(out))
}

/// The entry's diff colouring from the cache, parsed on the first ask; nothing for an entry
/// that is not an edit.
fn diff_spans_cached(
    cache: &DiffCache,
    ix: usize,
    entry: &TranscriptEntry,
) -> Option<Rc<[Vec<Span>]>> {
    let TranscriptBody::ToolUse { detail: ToolDetail::Diff { path, lines, .. }, .. } = &entry.body
    else {
        return None;
    };
    if let Some(known) = cache.borrow().get(&ix) {
        return known.clone();
    }
    let parsed = diff_spans(path, lines);
    cache.borrow_mut().insert(ix, parsed.clone());
    parsed
}

/// One diff line: a sign column, then the text, the row tinted in the success tone for an
/// addition, the error tone for a removal, nothing for context. A changed line is coloured by
/// its grammar (`spans`); context stays muted so the change is what stands out.
fn diff_row(
    line: &DiffLine,
    spans: Option<&[Span]>,
    font: &gpui::Font,
    mono: &str,
    theme: &Theme,
) -> impl IntoElement {
    let s = &theme.surfaces;
    let (sign, tone) = match line.kind {
        DiffKind::Context => (" ", None),
        DiffKind::Added => ("+", Some(s.success)),
        DiffKind::Removed => ("−", Some(s.error)),
    };
    let text: SharedString =
        if line.text.is_empty() { " ".into() } else { line.text.clone().into() };
    let body = if tone.is_some() && spans.is_some_and(|sp| !sp.is_empty()) {
        gpui::StyledText::new(text.clone())
            .with_runs(highlight::runs(text.len(), spans, font, theme))
            .into_any_element()
    } else {
        text.into_any_element()
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
        .child(div().flex_1().min_w(px(0.0)).whitespace_normal().child(body))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::highlight::Token;

    fn diff(path: &str, lines: &[(DiffKind, &str)]) -> TranscriptEntry {
        TranscriptEntry {
            at: None,
            body: TranscriptBody::ToolUse {
                call: "c1".to_owned(),
                name: "Edit".to_owned(),
                summary: path.to_owned(),
                detail: ToolDetail::Diff {
                    path: path.to_owned(),
                    line: None,
                    lines: lines
                        .iter()
                        .map(|(kind, text)| DiffLine { kind: *kind, text: (*text).to_owned() })
                        .collect(),
                    more_lines: 0,
                    replace_all: false,
                },
            },
        }
    }

    /// The export keeps what a reader would paste: the turns, the calls, the failures, the
    /// compaction; not the thinking or the tool output.
    #[test]
    fn the_conversation_exports_as_markdown_turns() {
        let entry = |body| TranscriptEntry { at: None, body };
        let entries = [
            entry(TranscriptBody::User { text: "fix it\n".to_owned(), images: 1 }),
            entry(TranscriptBody::Thinking { text: Clipped::whole("hmm".to_owned()) }),
            entry(TranscriptBody::Assistant { markdown: "On it.\n\n```sh\nls\n```\n".to_owned() }),
            diff("/w/a.rs", &[(DiffKind::Added, "x")]),
            entry(TranscriptBody::ToolResult {
                tool: Some("Edit".to_owned()),
                output: Clipped::whole("ok\nmore".to_owned()),
                is_error: false,
            }),
            entry(TranscriptBody::ToolResult {
                tool: None,
                output: Clipped::whole("\nboom\nmore".to_owned()),
                is_error: true,
            }),
            entry(TranscriptBody::Compacted {
                trigger: "auto".to_owned(),
                pre_tokens: 167_000,
                post_tokens: Some(12_000),
            }),
            entry(TranscriptBody::Notice {
                level: NoticeLevel::Warning,
                text: "Stop says: red".to_owned(),
            }),
        ];
        assert_eq!(
            as_markdown(&entries),
            "**You**\n\n_1 picture_\n\nfix it\n\n\
             **Claude**\n\nOn it.\n\n```sh\nls\n```\n\n\
             > **Edit** /w/a.rs\n\n\
             > **a tool failed:** boom\n\n\
             ---\n\n_compacted (auto): 167k → 12k_\n\n\
             > _Stop says: red_\n"
        );
        assert_eq!(as_markdown(&[]), "");
    }

    /// Each side is parsed whole: a removed line that opens a comment does not colour the
    /// line that replaced it, and a context line takes the new side's spans.
    #[test]
    fn a_diff_is_coloured_side_by_side_and_cached_by_entry() {
        let entry = diff(
            "/w/src/lib.rs",
            &[
                (DiffKind::Context, "fn a() {}"),
                (DiffKind::Removed, "/* gone"),
                (DiffKind::Added, "let x = 1;"),
                (DiffKind::Context, "// tail"),
            ],
        );
        let cache = RefCell::new(HashMap::new());
        let spans = diff_spans_cached(&cache, 3, &entry);
        let spans = spans.as_deref().unwrap_or_default();
        assert_eq!(spans.len(), 4);
        let first = |ix: usize| spans.get(ix).and_then(|l| l.first()).map(|s| s.token);
        assert_eq!(first(0), Some(Token::Keyword), "{spans:?}");
        assert_eq!(first(1), Some(Token::Comment), "{spans:?}");
        assert_eq!(first(2), Some(Token::Keyword), "the removed comment did not bleed in");
        assert_eq!(first(3), Some(Token::Comment), "{spans:?}");
        assert!(cache.borrow().contains_key(&3), "parsed once, kept by entry index");

        let plain = diff("/w/notes.txt", &[(DiffKind::Added, "words")]);
        assert!(diff_spans_cached(&cache, 4, &plain).is_none(), "no grammar, no colours");
        assert_eq!(cache.borrow().get(&4), Some(&None), "the miss is cached too");
    }
}
