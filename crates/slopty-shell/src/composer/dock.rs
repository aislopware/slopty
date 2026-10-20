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
    /// The side exchange the person dismissed, by chat: its card stays away until a newer one.
    pub side_dismissed: HashMap<String, String>,
    /// The newest note the person dismissed, by chat (its time): it and every older note stay
    /// away; a newer one shows.
    pub notes_dismissed: HashMap<String, i64>,
    /// slopty: the moment the dock's ticking words read (a goal's age, a note's or a band's
    /// time, a `!` command's): taken as the dock or its work arrives, a command starts or ends
    /// and the tick turns, never as a frame is drawn, so a composer drawn again from scratch says
    /// what the one shown says. `None` before any.
    clock: Option<std::time::Instant>,
}

impl DockFeed {
    /// The dock of `chat`, when it is the one watched.
    pub fn dock_of(&self, chat: &str) -> Option<&ChatDock> {
        (self.chat.as_deref() == Some(chat)).then_some(self.dock.as_deref()).flatten()
    }

    /// slopty: the dock's ticking words read now ([`Self::clock`]).
    fn stamp(&mut self, cx: &App) {
        self.clock = Some(crate::clock::now(cx));
    }

    /// slopty: the moment the dock's ticking words read ([`Self::clock`]); the frame's before
    /// any.
    pub(crate) fn now(&self, cx: &App) -> std::time::Instant {
        self.clock.unwrap_or_else(|| crate::clock::now(cx))
    }

    /// slopty: [`Self::now`] as a wall date, in milliseconds.
    pub(crate) fn now_ms(&self, cx: &App) -> i64 {
        crate::clock::utc_at(self.now(cx)).timestamp_millis()
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
        self.dock.stamp(cx); // slopty: its words read as it arrives
        let now = crate::clock::utc(cx).timestamp_millis();
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
        self.dock.stamp(cx); // slopty: its words read as it arrives
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
                this.dock.stamp(cx); // slopty: the words turn with the tick
                if let Some(run) = &this.dock.shell
                    && run.ended.is_some_and(|at| crate::clock::now(cx).saturating_duration_since(at) >= SHELL_DONE_SHOWN)
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
            // slopty: what failed and what to do, no internals.
            self.failure =
                Some(format!("{failed} while Slopty is offline. Try again once it reconnects.").into());
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

    /// slopty: what the person asked of one of the agent's MCP servers on the context card, sent
    /// to the chat's agent; a failure says so in the composer's failure chip.
    pub(crate) fn ask_mcp_server(&mut self, asked: &crate::account_usage::McpServerAsked, cx: &mut Context<Self>) {
        use crate::account_usage::McpServerAsked;
        match asked {
            McpServerAsked::Reconnect(server) => self.dock_call(
                calls::MCP_RECONNECT,
                serde_json::json!({ "server": server }),
                "Couldn't reconnect the MCP server",
                cx,
            ),
            McpServerAsked::Enable(server, enabled) => self.dock_call(
                calls::MCP_ENABLE,
                serde_json::json!({ "server": server, "enabled": enabled }),
                if *enabled { "Couldn't enable the MCP server" } else { "Couldn't disable the MCP server" },
                cx,
            ),
        }
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
            started: crate::clock::now(cx),
            outcome: None,
            ended: None,
        });
        self.dock.stamp(cx); // slopty: its time starts at nothing
        let params = serde_json::json!({ "chatId": chat, "command": command });
        self.dock.shell_task = Some(cx.spawn(async move |this, cx| {
            let answer = engine.client().call(calls::RUN_SHELL, params).await;
            this.update(cx, |this, cx| {
                if let Some(run) = this.dock.shell.as_mut() {
                    run.outcome = Some(answer.map(|_| ()).map_err(|err| err.to_string()));
                    run.ended = Some(crate::clock::now(cx));
                }
                this.dock.stamp(cx); // slopty: its time stops where it ended
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
        let tip = format!("Runs in {place} on the chat's machine; its output goes to the agent.");
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
            // The tag in the interface's face: the command beside it is the mono one.
            .text_size(crate::typography::ui_rems(11.5))
            .font_weight(gpui::FontWeight::MEDIUM)
            .text_color(theme.accent)
            // slopty: the tag's name and its way out (Esc), what it does a quieter line.
            .tooltip(crate::settings::widgets::detail_tooltip(
                "Shell mode",
                tip,
                Some(Keys::Fixed("escape")),
            ))
            .tooltip_show_delay(crate::settings::widgets::TOOLTIP_DELAY)
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
        (idle && self.input.read(cx).is_empty()).then_some(suggestion)
    }

    /// Take the suggestion into the draft (Tab); whether there was one.
    pub(super) fn take_suggestion(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(suggestion) = self.suggestion(cx) else { return false };
        self.input.update(cx, |input, cx| input.set_text(suggestion, cx));
        self.focus_pending = true;
        self.on_input_edited(cx);
        cx.notify();
        true
    }

    /// The quiet lines above the tray stack: what the agent waits on once its turn ended, its
    /// passing notes and a `!` command under way. The prompt suggestion is not one of them: it
    /// shows faint in the empty draft.
    /// `drawer`: the inset at which they tuck in as a drawer of the tray stack (on a tray or the
    /// composer itself), or `None` where another card stands under them.
    pub(super) fn render_dock_lines(&mut self, drawer: Option<f32>, theme: &Theme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut lines = Vec::new();
        let Some(chat) = self.state.read(cx).selected_chat.clone() else { return lines };
        let now = self.dock.now_ms(cx); // slopty: the dock's moment, not the frame's
        // slopty: what still runs is said once, by the Background tray's header, by kind; the
        // waiting line stands only while no tray of this chat says it.
        let tray_says = self.dock.chat.as_deref() == Some(chat.as_str())
            && self.dock.tray.as_ref().is_some_and(|tray| tray.counts.total() > 0);
        let (waiting, notes, logs) = match self.dock.dock_of(&chat) {
            Some(dock) => (
                dock.waiting.clone().filter(|_| dock.activity.is_none() && !tray_says),
                dock.notes
                    .iter()
                    .filter(|note| note_showing(note, now) && note.priority != "immediate")
                    .filter(|note| self.dock.notes_dismissed.get(&chat).is_none_or(|gone| note.at > *gone))
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
            lines.push(caption_line("dock-waiting", None, theme.text_faint, waiting, None, theme, cx));
        }
        for note in notes {
            let dot = match note.tone.as_str() {
                NoteLine::ERROR => theme.danger,
                NoteLine::WARNING => theme.warning,
                _ => theme.text_faint,
            };
            let id = format!("dock-note-{}", note.key);
            lines.push(caption_line(&id, None, dot, note.text, Some((chat.clone(), note.at)), theme, cx));
        }
        // A mod's log keeps its newest line, by mod.
        let mut seen = HashSet::new();
        for (plugin, text) in logs.into_iter().rev() {
            if seen.insert(plugin.clone()) {
                let id = format!("dock-log-{plugin}");
                lines.push(caption_line(&id, Some(plugin), theme.text_faint, text, None, theme, cx));
            }
        }
        if let Some(run) = self.dock.shell.clone().filter(|run| run.chat == chat) {
            lines.push(self.render_shell_run(&run, theme, cx));
        }
        if lines.is_empty() {
            return lines;
        }
        // The transcript scrolls under these lines, so they sit on the composer's own material,
        // and stay legible over whatever passes beneath. On the tray stack they are one more of
        // its drawers (its top-rounded surface tucked under the layer below, one step in), so
        // the stack reads as one object; over another card they stand as a card of its edges.
        let card = match drawer {
            Some(inset) => div()
                .id("dock-lines")
                .debug_selector(|| "dock-lines".into())
                .mx(px(inset))
                .mb(px(-(Theme::SPACE_SM + crate::composer::QUEUE_COMPOSER_OVERLAP)))
                .child(crate::frost::frosted(
                    crate::queue::PANEL_RADIUS,
                    crate::frost::MENU_BLUR,
                    crate::queue::queue_panel_surface(theme, cx.has_active_drag())
                        .pt(px(crate::queue::PANEL_PAD_TOP + 4.0))
                        .pb(px(crate::composer::QUEUE_COMPOSER_OVERLAP + crate::queue::PANEL_PAD_TOP + 4.0))
                        .gap(px(4.0))
                        .children(lines),
                )),
            None => div()
                .id("dock-lines")
                .debug_selector(|| "dock-lines".into())
                // slopty: the edges every card over the composer shares (a side answer's, a
                // folded question's, a mod's band), so none reads as a seam.
                .mx(px(QUEUE_SIDE_INSET))
                .px(px(4.0))
                .py(px(4.0))
                .flex()
                .flex_col()
                .gap(px(4.0))
                .rounded(px(Theme::CONTROL_RADIUS))
                .border_1()
                .border_color(theme.composer_surface_border())
                .bg(theme.composer_surface_bg())
                .children(lines),
        };
        vec![card.into_any_element()]
    }

    /// A `!` command's live chip: what runs and for how long, then where its output went.
    fn render_shell_run(&self, run: &ShellRun, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let elapsed = slopty_view_doc::duration_words(
            // slopty: the dock's moment, not the frame's.
            u64::try_from(run.ended.unwrap_or_else(|| self.dock.now(cx)).saturating_duration_since(run.started).as_millis())
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
            .text_size(crate::typography::ui_rems(11.0))
            .line_height(crate::typography::ui_rems(14.0))
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
}

/// One quiet caption line above the composer: a dot, who says it when a mod does, the words.
/// Words the line cuts show whole in a tooltip; a note (`dismiss`: its chat and time) takes an
/// ✕ under the pointer that puts it, and every older note of the chat, away.
fn caption_line(
    id: &str,
    from: Option<String>,
    dot: gpui::Hsla,
    words: String,
    dismiss: Option<(String, i64)>,
    theme: &Theme,
    cx: &mut Context<Composer>,
) -> AnyElement {
    let label = match &from {
        Some(from) => format!("{from}: {words}"),
        None => words.clone(),
    };
    let hover: SharedString = format!("{id}-hover").into();
    let close = dismiss.map(|(chat, at)| {
        let close_hover: SharedString = format!("{id}-close-hover").into();
        // slopty: the keyboard reaches it too, and shows it while it holds focus (#19).
        crate::surface_chrome::focus_ring_revealing(div().id(SharedString::from(format!("{id}-dismiss"))), theme)
            .tab_index(0)
            .role(Role::Button)
            .flex_none()
            .size(px(24.0))
            .my(px(-5.0))
            .mr(px(-6.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(6.0))
            .cursor_pointer()
            // Under a pointer it comes with the hover; a touch screen has none, so there it stays.
            .opacity(crate::motion::revealed_by_hover(&hover).max(crate::motion::hover_t(&close_hover)))
            .bg(crate::motion::hover_blend(&close_hover, gpui::transparent_black(), crate::theme::ink(0.06)))
            .hover_fade(close_hover)
            .hint("Dismiss note")
            .on_click(cx.listener(move |this, _, _, cx| {
                let gone = this.dock.notes_dismissed.entry(chat.clone()).or_insert(at);
                *gone = (*gone).max(at);
                cx.notify();
            }))
            .child(crate::icons::icon(crate::icons::CLOSE).size(px(10.0)).text_color(theme.text_muted))
    });
    div()
        .id(SharedString::from(id.to_owned()))
        .role(Role::Status)
        .aria_label(SharedString::from(label))
        .mx(px(8.0))
        .flex()
        .items_center()
        .gap(px(6.0))
        .text_size(crate::typography::ui_rems(11.0))
        .line_height(crate::typography::ui_rems(14.0))
        .text_color(theme.text_faint)
        .when(close.is_some(), |el| el.hover_fade(hover))
        .child(div().flex_none().size(px(5.0)).rounded_full().bg(dot))
        .when_some(from, |el, from| {
            el.child(div().flex_none().text_color(theme.text_muted).child(SharedString::from(from)))
        })
        .child(crate::settings::widgets::truncate_with_tip(format!("{id}-words"), words))
        .children(close)
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
