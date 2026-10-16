// slopty: Slopty's own file (not in zeron).
//! slopty: what the dock around the composer shows of a chat beyond zeron's queue and checklist:
//! the chat's dock (`slopty.WatchDock`: the turn's live line, the goal, the agent's passing
//! notes, the prompt it guesses comes next, the status line, the permission mode, the background
//! counts, the context by category, the subagent kinds, its mods' surfaces) and its background
//! work (`slopty.WatchWork`, the Background tray's rows).
//!
//! Both are the view model's shapes (`slopty_view_doc::{dock, work}`); this module watches them
//! for the selected chat, keeps the latest of each where the transcript's working line reads it
//! ([`chat_dock`]), redraws once a second while anything drawn ticks, and draws the quiet lines
//! above the tray stack: what the agent waits on once its turn ended, its passing notes, a `!`
//! command under way, and the prompt suggestion that Tab takes.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use gpui::AnyElement;
use slopty_view_doc::calls;
use slopty_view_doc::dock::{ChatDock, NoteLine, SubagentKindRow};
use slopty_view_doc::work::WorkTray;

use super::*;

/// How long a `!` command's chip stays once its output went to the agent.
const SHELL_DONE_SHOWN: Duration = Duration::from_secs(4);
/// How often what ticks (elapsed times, countdowns, a goal's age) is drawn again.
const TICK: Duration = Duration::from_secs(1);

/// The latest dock of each chat a composer watches, for the transcript's working line
/// (`composer/activity.rs`).
#[derive(Default)]
pub struct DockCache(HashMap<String, Arc<ChatDock>>);

impl gpui::Global for DockCache {}

/// The latest dock of chat `chat`, while a composer watches it.
pub fn chat_dock(chat: &str, cx: &App) -> Option<Arc<ChatDock>> {
    cx.try_global::<DockCache>()?.0.get(chat).cloned()
}

/// A `!` command sent to run on the chat's machine.
#[derive(Clone, Debug)]
pub(crate) struct ShellRun {
    pub chat: String,
    pub command: String,
    pub started: Instant,
    /// `None` while it runs; `Ok` once its output went to the agent; `Err` with why it did not.
    pub outcome: Option<Result<(), String>>,
    pub ended: Option<Instant>,
}

/// What the dock keeps between frames.
#[derive(Default)]
pub(crate) struct DockFeed {
    /// The chat watched.
    pub chat: Option<String>,
    pub dock: Option<Arc<ChatDock>>,
    pub tray: Option<Arc<WorkTray>>,
    watch: Vec<Task<()>>,
    tick: Option<Task<()>>,
    /// Notes already raised as a toast, by key and time.
    toasted: HashSet<(String, i64)>,
    /// The `!` command under way, or just done.
    pub shell: Option<ShellRun>,
    shell_task: Option<Task<()>>,
    /// The suggestion the person set aside (Escape), so it does not come back.
    pub dismissed_suggestion: Option<String>,
}

impl DockFeed {
    /// The dock of `chat`, when it is the one watched.
    pub fn dock_of(&self, chat: &str) -> Option<&ChatDock> {
        (self.chat.as_deref() == Some(chat)).then_some(self.dock.as_deref()).flatten()
    }
}

/// The session's agent kinds an `@` query names, offered after the files; `@agent-` and case
/// are ignored, so `@rev`, `@agent-rev` and `@REV` all find "reviewer".
pub(crate) fn agent_matches(dock: Option<&ChatDock>, query: &str) -> Vec<SubagentKindRow> {
    let Some(dock) = dock else { return Vec::new() };
    let query = query.to_lowercase();
    let wanted = query.strip_prefix("agent-").unwrap_or(&query);
    dock.subagent_kinds
        .iter()
        .filter(|kind| kind.name.to_lowercase().contains(wanted))
        .cloned()
        .collect()
}

/// What an `@` agent row inserts: the agent's own mention of it.
pub(crate) fn agent_mention(name: &str) -> String {
    format!("@agent-{name}")
}

/// The command a `!` draft runs, when the draft is one: its words after the mark.
pub(crate) fn shell_command(text: &str) -> Option<&str> {
    let rest = text.trim_start().strip_prefix('!')?;
    let command = rest.trim();
    (!command.is_empty()).then_some(command)
}

/// Whether `text` is a `!` draft at all, empty command included: the composer turns to shell mode.
pub(crate) fn is_shell_draft(text: &str) -> bool {
    text.trim_start().starts_with('!')
}

/// Whether a note still shows at `now`.
fn note_showing(note: &NoteLine, now: i64) -> bool {
    note.until.is_none_or(|until| until > now)
}

impl Composer {
    /// Keep the selected chat's dock and work watched; the watch follows the selection.
    pub(super) fn sync_dock(&mut self, cx: &mut Context<Self>) {
        let chat = self.state.read(cx).selected_chat.clone();
        if self.dock.chat == chat {
            return;
        }
        if let Some(old) = self.dock.chat.take()
            && cx.has_global::<DockCache>()
        {
            cx.global_mut::<DockCache>().0.remove(&old);
        }
        self.dock.dock = None;
        self.dock.tray = None;
        self.dock.watch.clear();
        self.dock.chat = chat.clone();
        let (Some(chat), Some(engine)) = (chat, self.state.read(cx).engine().cloned()) else {
            return;
        };
        let params = serde_json::json!({ "chatId": chat });
        for method in [calls::WATCH_DOCK, calls::WATCH_WORK] {
            let (engine, params, chat) = (engine.clone(), params.clone(), chat.clone());
            self.dock.watch.push(cx.spawn(async move |this, cx| {
                let Ok(mut rx) = engine.client().subscribe_checked(method, params).await else {
                    return;
                };
                while let Some(value) = rx.recv().await {
                    let alive = if method == calls::WATCH_DOCK {
                        let Ok(dock) = serde_json::from_value::<ChatDock>(value) else { continue };
                        this.update(cx, |this, cx| this.apply_dock(&chat, dock, cx))
                    } else {
                        let Ok(tray) = serde_json::from_value::<WorkTray>(value) else { continue };
                        this.update(cx, |this, cx| this.apply_tray(&chat, tray, cx))
                    };
                    if alive.is_err() {
                        return;
                    }
                }
            }));
        }
    }

    /// The dock of `chat` as the view model last said it.
    pub(crate) fn apply_dock(&mut self, chat: &str, dock: ChatDock, cx: &mut Context<Self>) {
        if self.dock.chat.as_deref() != Some(chat) {
            return;
        }
        let now = chrono::Utc::now().timestamp_millis();
        let mut toasts = Vec::new();
        for note in dock.notes.iter().filter(|note| note_showing(note, now)) {
            if note.priority == "immediate" && self.dock.toasted.insert((note.key.clone(), note.at)) {
                toasts.push(note.clone());
            }
        }
        let mut surface_toasts = Vec::new();
        for surface in &dock.surfaces {
            if surface.kind == slopty_view_doc::ModSurface::TOAST
                && self.dock.toasted.insert((format!("surface:{}", surface.id), 0))
            {
                surface_toasts.push(surface.clone());
            }
        }
        let dock = Arc::new(dock);
        if !cx.has_global::<DockCache>() {
            cx.set_global(DockCache::default());
        }
        cx.global_mut::<DockCache>().0.insert(chat.to_owned(), dock.clone());
        self.dock.dock = Some(dock);
        for note in toasts {
            let kind = match note.tone.as_str() {
                NoteLine::ERROR => crate::toast::ToastKind::Error,
                _ => crate::toast::ToastKind::Info,
            };
            crate::toast::show(cx, kind, note.text);
        }
        for surface in surface_toasts {
            let words = surface.text.clone().or(surface.title.clone()).unwrap_or_default();
            if !words.trim().is_empty() {
                crate::toast::info(cx, words);
            }
        }
        self.dock_tick(cx);
        cx.notify();
    }

    /// The background work of `chat` as the view model last said it.
    pub(crate) fn apply_tray(&mut self, chat: &str, tray: WorkTray, cx: &mut Context<Self>) {
        if self.dock.chat.as_deref() != Some(chat) {
            return;
        }
        self.dock.tray = Some(Arc::new(tray));
        self.tray_rows_changed(cx);
        self.dock_tick(cx);
        cx.notify();
    }

    /// Whether anything the dock draws ticks: a turn's timer, a retry's countdown, a goal's age,
    /// a live row's elapsed time, a `!` command, a note that goes at its time, a request's countdown.
    fn dock_ticks(&self) -> bool {
        let dock = self.dock.dock.as_deref();
        dock.is_some_and(|dock| {
            dock.activity.is_some()
                || dock.goal.is_some()
                || dock.notes.iter().any(|note| note.until.is_some())
                || dock.surfaces.iter().any(|surface| surface.until.is_some())
        }) || self.dock.tray.as_deref().is_some_and(|tray| tray.rows.iter().any(|row| row.live))
            || self.dock.shell.is_some()
            || self.wizard.as_ref().and_then(Wizard::current).is_some_and(|q| q.until_ms.is_some())
    }

    /// Draw again in a second while anything drawn ticks.
    pub(super) fn dock_tick(&mut self, cx: &mut Context<Self>) {
        if self.dock.tick.is_some() || !self.dock_ticks() {
            return;
        }
        self.dock.tick = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(TICK).await;
            this.update(cx, |this, cx| {
                this.dock.tick = None;
                if let Some(run) = &this.dock.shell
                    && run.ended.is_some_and(|at| at.elapsed() >= SHELL_DONE_SHOWN)
                {
                    this.dock.shell = None;
                }
                cx.notify();
                this.dock_tick(cx);
            })
            .ok();
        }));
    }

    /// Ask the view model `method` for the selected chat with `params` (its `chatId` added);
    /// a failure says `failed` and why in the composer's failure chip.
    pub(crate) fn dock_call(
        &mut self,
        method: &'static str,
        params: serde_json::Value,
        failed: &'static str,
        cx: &mut Context<Self>,
    ) {
        self.dock_call_or(method, params, failed, |_| {}, cx);
    }

    /// [`Self::dock_call`], with `undo` taking back what the composer showed at once when the
    /// call fails (a row's "stopping", a mode switched ahead).
    pub(crate) fn dock_call_or(
        &mut self,
        method: &'static str,
        mut params: serde_json::Value,
        failed: &'static str,
        undo: impl FnOnce(&mut Self) + 'static,
        cx: &mut Context<Self>,
    ) {
        let chat = self.dock.chat.clone();
        let engine = self.state.read(cx).engine().cloned();
        let (Some(chat), Some(engine)) = (chat, engine) else {
            undo(self);
            self.failure = Some(format!("{failed}: engine not connected").into());
            self.failure_key = self.dock.chat.clone();
            cx.notify();
            return;
        };
        if let Some(map) = params.as_object_mut() {
            map.entry("chatId").or_insert_with(|| serde_json::Value::String(chat.clone()));
        }
        cx.spawn(async move |this, cx| {
            if let Err(err) = engine.client().call(method, params).await {
                this.update(cx, |this, cx| {
                    undo(this);
                    this.failure = Some(format!("{failed}: {err}").into());
                    this.failure_key = Some(chat);
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    /// Run the `!` command `command` on the chat's machine; its output goes to the agent as the
    /// person's next message (`slopty.RunShell`). Whether it was sent.
    pub(super) fn run_shell(&mut self, command: String, cx: &mut Context<Self>) -> bool {
        let Some(chat) = self.dock.chat.clone() else { return false };
        if !self.dock.dock_of(&chat).is_some_and(|dock| dock.can.shell) {
            return false;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else { return false };
        self.dock.shell = Some(ShellRun {
            chat: chat.clone(),
            command: command.clone(),
            started: Instant::now(),
            outcome: None,
            ended: None,
        });
        let params = serde_json::json!({ "chatId": chat, "command": command });
        self.dock.shell_task = Some(cx.spawn(async move |this, cx| {
            let answer = engine.client().call(calls::RUN_SHELL, params).await;
            this.update(cx, |this, cx| {
                if let Some(run) = this.dock.shell.as_mut() {
                    run.outcome = Some(answer.map(|_| ()).map_err(|err| err.to_string()));
                    run.ended = Some(Instant::now());
                }
                this.dock_tick(cx);
                cx.notify();
            })
            .ok();
        }));
        self.dock_tick(cx);
        cx.notify();
        true
    }

    /// Whether the draft is a `!` command the chat's agent can be given: shell mode.
    pub(super) fn shell_mode(&self, cx: &App) -> bool {
        let Some(chat) = self.state.read(cx).selected_chat.as_deref() else { return false };
        !self.wizard_open()
            && self.editing_queued.is_none()
            && self.dock.dock_of(chat).is_some_and(|dock| dock.can.shell)
            && is_shell_draft(self.input.read(cx).text())
    }

    /// Leave shell mode: the draft loses its `!`. Whether it was in it.
    pub(super) fn leave_shell_mode(&mut self, cx: &mut Context<Self>) -> bool {
        let text = self.input.read(cx).text().to_owned();
        let Some(rest) = text.trim_start().strip_prefix('!') else { return false };
        let rest = rest.trim_start().to_owned();
        self.input.update(cx, |input, cx| input.set_text(rest, cx));
        self.on_input_edited(cx);
        cx.notify();
        true
    }

    /// Shell mode's tag in the composer: where the command runs.
    pub(super) fn render_shell_tag(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let place = self
            .state
            .read(cx)
            .selected_chat_row()
            .and_then(|chat| chat.cwd.clone())
            .map_or_else(|| "the chat's folder".to_owned(), |cwd| cwd.rsplit('/').next().unwrap_or(&cwd).to_owned());
        let tip = format!("Runs in {place} on the chat's machine; its output goes to the agent. Escape leaves shell mode.");
        div()
            .id("composer-shell-tag")
            .role(Role::Status)
            .aria_label("Shell mode")
            .aria_description(SharedString::from(tip.clone()))
            .flex_none()
            .h(px(22.0))
            .px(px(8.0))
            .flex()
            .items_center()
            .gap(px(5.0))
            .rounded(px(Theme::CONTROL_RADIUS))
            .bg(theme.accent.opacity(0.12))
            .text_size(px(11.5))
            .font_family(theme.font_mono.clone())
            .text_color(theme.accent)
            .tooltip(crate::settings::widgets::text_tooltip(tip))
            .child(crate::icons::icon(crate::icons::TERMINAL).size(px(12.0)).text_color(theme.accent))
            .child("Shell")
            .into_any_element()
    }

    /// The prompt the agent guesses comes next, while the draft is empty and nothing runs.
    pub(super) fn suggestion(&self, cx: &App) -> Option<String> {
        let chat = self.state.read(cx).selected_chat.as_deref()?;
        let dock = self.dock.dock_of(chat)?;
        let suggestion = dock.suggestion.clone().filter(|s| !s.trim().is_empty())?;
        let idle = dock.activity.is_none() && !self.run_live(cx);
        (idle
            && self.input.read(cx).is_empty()
            && self.dock.dismissed_suggestion.as_deref() != Some(suggestion.as_str()))
        .then_some(suggestion)
    }

    /// Take the suggestion into the draft (Tab, or a click); whether there was one.
    pub(super) fn take_suggestion(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(suggestion) = self.suggestion(cx) else { return false };
        self.input.update(cx, |input, cx| input.set_text(suggestion, cx));
        self.focus_pending = true;
        self.on_input_edited(cx);
        cx.notify();
        true
    }

    /// The quiet lines above the tray stack: what the agent waits on once its turn ended, its
    /// passing notes, a `!` command under way, and the prompt suggestion.
    pub(super) fn render_dock_lines(&mut self, theme: &Theme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut lines = Vec::new();
        let Some(chat) = self.state.read(cx).selected_chat.clone() else { return lines };
        let now = chrono::Utc::now().timestamp_millis();
        let (waiting, notes, logs) = match self.dock.dock_of(&chat) {
            Some(dock) => (
                dock.waiting.clone().filter(|_| dock.activity.is_none()),
                dock.notes
                    .iter()
                    .filter(|note| note_showing(note, now) && note.priority != "immediate")
                    .cloned()
                    .collect::<Vec<_>>(),
                dock.surfaces
                    .iter()
                    .filter(|s| s.kind == slopty_view_doc::ModSurface::LOG)
                    .filter_map(|s| {
                        let text = s.text.clone().or(s.title.clone())?;
                        Some((s.plugin.clone(), text))
                    })
                    .collect::<Vec<_>>(),
            ),
            None => (None, Vec::new(), Vec::new()),
        };
        if let Some(waiting) = waiting {
            lines.push(caption_line("dock-waiting", None, theme.text_faint, waiting, theme, cx));
        }
        for note in notes {
            let dot = match note.tone.as_str() {
                NoteLine::ERROR => theme.danger,
                NoteLine::WARNING => theme.warning,
                _ => theme.text_faint,
            };
            let id = format!("dock-note-{}", note.key);
            lines.push(caption_line(&id, None, dot, note.text, theme, cx));
        }
        // A mod's log keeps its newest line, by mod.
        let mut seen = HashSet::new();
        for (plugin, text) in logs.into_iter().rev() {
            if seen.insert(plugin.clone()) {
                let id = format!("dock-log-{plugin}");
                lines.push(caption_line(&id, Some(plugin), theme.text_faint, text, theme, cx));
            }
        }
        if let Some(run) = self.dock.shell.clone().filter(|run| run.chat == chat) {
            lines.push(self.render_shell_run(&run, theme, cx));
        }
        if let Some(suggestion) = self.suggestion(cx) {
            lines.push(self.render_suggestion(suggestion, theme, cx));
        }
        lines
    }

    /// A `!` command's live chip: what runs and for how long, then where its output went.
    fn render_shell_run(&self, run: &ShellRun, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let elapsed = slopty_view_doc::duration_words(
            u64::try_from(run.ended.unwrap_or_else(Instant::now).duration_since(run.started).as_millis())
                .unwrap_or(u64::MAX),
        );
        let (words, color) = match &run.outcome {
            None => (format!("Running · {elapsed}"), theme.text_faint),
            Some(Ok(())) => (format!("Sent to the agent · {elapsed}"), theme.text_faint),
            Some(Err(err)) => (format!("Didn't run: {err}"), theme.danger),
        };
        let label = format!("! {} · {words}", run.command);
        div()
            .id("dock-shell-run")
            .role(Role::Status)
            .aria_label(SharedString::from(label))
            .mx(px(8.0))
            .flex()
            .items_center()
            .gap(px(6.0))
            .text_size(px(11.0))
            .line_height(px(14.0))
            .child(match run.outcome {
                None => crate::loaders::mini_mono_spinner(
                    "dock-shell-spinner",
                    1.5,
                    theme.text_muted,
                    cx.entity_id(),
                    cx,
                )
                .into_any_element(),
                Some(Ok(())) => crate::icons::icon(crate::icons::CHECK)
                    .size(px(10.0))
                    .text_color(theme.success)
                    .into_any_element(),
                Some(Err(_)) => div().flex_none().size(px(5.0)).rounded_full().bg(theme.danger).into_any_element(),
            })
            .child(
                div()
                    .flex_none()
                    .max_w(px(320.0))
                    .truncate()
                    .font_family(theme.font_mono.clone())
                    .text_color(theme.text_muted)
                    .child(SharedString::from(format!("! {}", run.command))),
            )
            .child(div().min_w_0().truncate().text_color(color).child(SharedString::from(words)))
            .into_any_element()
    }

    /// The prompt suggestion: a quiet chip over the composer; Tab or a click takes it, ✕ sets it
    /// aside.
    fn render_suggestion(&self, suggestion: String, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let key: SharedString = format!("dock-suggestion-{}", cx.entity_id()).into();
        let dismiss = suggestion.clone();
        div()
            .mx(px(4.0))
            .flex()
            .items_center()
            .gap(px(4.0))
            .child(
                div()
                    .id("dock-suggestion")
                    .role(Role::Button)
                    .aria_label(SharedString::from(format!("Suggested: {suggestion}")))
                    .aria_description("Tab takes it into the message")
                    .min_w_0()
                    .h(px(24.0))
                    .px(px(8.0))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .rounded(px(Theme::CONTROL_RADIUS))
                    .border_1()
                    .border_color(crate::theme::hairline(0.07))
                    .bg(motion::hover_blend(&key, crate::theme::ink(0.03), crate::theme::ink(0.06)))
                    .on_hover(motion::hover_listener(key.clone()))
                    .cursor_pointer()
                    .text_size(px(12.0))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.take_suggestion(cx);
                    }))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_color(theme.text_muted)
                            .child(SharedString::from(suggestion)),
                    )
                    .child(crate::popover::kbd_hint(theme, "Tab")),
            )
            .child(
                div()
                    .id("dock-suggestion-dismiss")
                    .role(Role::Button)
                    .aria_label("Dismiss suggestion")
                    .flex_none()
                    .size(px(20.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(5.0))
                    .cursor_pointer()
                    .hover(|s| s.bg(theme.element_hover))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.dock.dismissed_suggestion = Some(dismiss.clone());
                        cx.notify();
                    }))
                    .child(crate::icons::icon(crate::icons::CLOSE).size(px(10.0)).text_color(theme.text_faint)),
            )
            .into_any_element()
    }
}

/// One quiet caption line above the composer: a dot, who says it when a mod does, the words.
fn caption_line(
    id: &str,
    from: Option<String>,
    dot: gpui::Hsla,
    words: String,
    theme: &Theme,
    _cx: &mut Context<Composer>,
) -> AnyElement {
    let label = match &from {
        Some(from) => format!("{from}: {words}"),
        None => words.clone(),
    };
    div()
        .id(SharedString::from(id.to_owned()))
        .role(Role::Status)
        .aria_label(SharedString::from(label))
        .mx(px(8.0))
        .flex()
        .items_center()
        .gap(px(6.0))
        .text_size(px(11.0))
        .line_height(px(14.0))
        .text_color(theme.text_faint)
        .child(div().flex_none().size(px(5.0)).rounded_full().bg(dot))
        .when_some(from, |el, from| {
            el.child(div().flex_none().text_color(theme.text_muted).child(SharedString::from(from)))
        })
        .child(div().min_w_0().truncate().child(SharedString::from(words)))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `!` draft runs the words after the mark; an empty one runs nothing yet.
    #[test]
    fn a_bang_draft_names_its_command() {
        assert_eq!(shell_command("!pwd"), Some("pwd"));
        assert_eq!(shell_command("  ! ls -la "), Some("ls -la"));
        assert_eq!(shell_command("!"), None);
        assert_eq!(shell_command("say !pwd"), None);
        assert!(is_shell_draft("!"));
        assert!(!is_shell_draft("pwd"));
    }
}
