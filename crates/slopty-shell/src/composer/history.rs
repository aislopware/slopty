// slopty: Slopty's own file, beside zeron's composer.
//! The composer's memory: ↑/↓ walk the chat's earlier prompts, the unsent words are kept on
//! the chat's machine for every device, a quote from the transcript lands as a chip, and the
//! keys for the queue and the new session's machine.
use gpui::WeakEntity;

use super::*;

/// How long typing rests before the draft is kept on the chat's machine.
pub(super) const DRAFT_SAVE_DEBOUNCE: Duration = Duration::from_millis(250);

actions!(
    composer,
    [
        /// ⌥↑: edit the newest queued message.
        EditLatestQueued,
        /// ⌘⇧↵: steer the oldest queued message in now.
        SteerOldestQueued,
        /// ⌘⇧↑ on the new-session canvas: the previous online machine.
        PreviousMachine,
        /// ⌘⇧↓ on the new-session canvas: the next online machine.
        NextMachine,
    ]
);

/// The composer's keys of Slopty's, bound with the message composer's.
pub(super) fn bindings(context: &'static str) -> Vec<KeyBinding> {
    let ctx = Some(context);
    vec![
        KeyBinding::new("alt-up", EditLatestQueued, ctx),
        KeyBinding::new(&platform_combo("mod-shift-enter"), SteerOldestQueued, ctx),
        KeyBinding::new(&platform_combo("mod-shift-up"), PreviousMachine, ctx),
        KeyBinding::new(&platform_combo("mod-shift-down"), NextMachine, ctx),
    ]
}

/// Where ↑ has walked to in the chat's earlier prompts.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct HistoryCursor {
    chat: String,
    /// 0 = the newest prompt.
    index: usize,
    /// What was in the composer when the walk began.
    draft: String,
    /// The prompt shown now; an edit to it ends the walk.
    shown: String,
}

/// The composer's state of Slopty's own.
pub(super) struct Extras {
    pub(super) history: Option<HistoryCursor>,
    /// A pending draft save per chat, so leaving a chat never drops its last words.
    pub(super) draft_saves: HashMap<String, Task<()>>,
    /// The draft each chat's machine holds, as this composer last kept or restored it.
    pub(super) saved_drafts: HashMap<String, String>,
    /// A read-gated approval's body.
    pub(super) wizard_scroll: gpui::ScrollHandle,
    /// Since when the chat's delivery has been degraded (the offline notice's grace).
    pub(super) degraded_since: Option<(String, Instant)>,
    pub(super) degraded_task: Option<Task<()>>,
    /// Redraws the limit notice when the limit lifts.
    pub(super) limit_task: Option<(chrono::DateTime<chrono::Utc>, Task<()>)>,
    pub(super) notice_task: Option<Task<()>>,
    /// Looks at a read-gated body again once it has been drawn.
    pub(super) gate_task: Option<Task<()>>,
    /// The chat whose stop was just confirmed, and when.
    pub(super) stopped: Option<(String, Instant)>,
    pub(super) stopped_task: Option<Task<()>>,
}

impl Default for Extras {
    fn default() -> Self {
        Self {
            history: None,
            draft_saves: HashMap::new(),
            saved_drafts: HashMap::new(),
            wizard_scroll: gpui::ScrollHandle::new(),
            degraded_since: None,
            degraded_task: None,
            limit_task: None,
            notice_task: None,
            gate_task: None,
            stopped: None,
            stopped_task: None,
        }
    }
}

/// The person's prompts in `transcript`, newest first, a repeat kept once.
pub(super) fn prompts(transcript: &[SessionMessageEntry]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for entry in transcript.iter().rev().filter(|entry| entry.role == MessageRole::User) {
        let text = entry
            .parts
            .iter()
            .filter_map(|part| match part {
                MessagePart::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        if !text.trim().is_empty() && out.last() != Some(&text) {
            out.push(text);
        }
    }
    out
}

/// One step of the walk: `older` steps back in time. What the composer should now hold, or
/// `None` to leave it be (a draft of one's own is never replaced).
pub(super) fn step_history(
    cursor: &mut Option<HistoryCursor>,
    prompts: &[String],
    current: &str,
    chat: &str,
    older: bool,
) -> Option<String> {
    if cursor.as_ref().is_some_and(|c| c.chat != chat || c.shown != current) {
        *cursor = None;
    }
    if older {
        let next = match cursor.as_ref() {
            None if !current.is_empty() => return None,
            None => 0,
            Some(c) => c.index + 1,
        };
        let shown = prompts.get(next)?.clone();
        let draft = cursor.take().map_or_else(|| current.to_owned(), |c| c.draft);
        *cursor = Some(HistoryCursor { chat: chat.to_owned(), index: next, draft, shown });
        cursor.as_ref().map(|c| c.shown.clone())
    } else {
        let c = cursor.as_mut()?;
        if c.index == 0 {
            return cursor.take().map(|c| c.draft);
        }
        c.index -= 1;
        c.shown = prompts.get(c.index)?.clone();
        Some(c.shown.clone())
    }
}

/// The composer a quote from the transcript goes to.
#[derive(Default)]
struct QuoteTargets(Vec<WeakEntity<Composer>>);

impl gpui::Global for QuoteTargets {}

/// Put `text`, quoted from the transcript, in this window's composer as a quote chip and give it
/// the keyboard: whether a composer took it.
pub(crate) fn insert_quote(text: &str, window: &mut Window, cx: &mut App) -> bool {
    insert_quote_for(None, text, window, cx)
}

/// [`insert_quote`] into the composer of the chat `state` shows (a side chat's own), or the
/// main composer when `state` is `None`.
pub(crate) fn insert_quote_for(
    state: Option<&Entity<AppState>>,
    text: &str,
    window: &mut Window,
    cx: &mut App,
) -> bool {
    let Some(link) = quote_link(text) else { return false };
    let composers: Vec<Entity<Composer>> = cx
        .try_global::<QuoteTargets>()
        .map(|targets| targets.0.iter().filter_map(WeakEntity::upgrade).collect())
        .unwrap_or_default();
    let target = composers
        .iter()
        .find(|composer| {
            let composer = composer.read(cx);
            match state {
                Some(state) => composer.state.entity_id() == state.entity_id(),
                None => !composer.side_chat,
            }
        })
        .cloned();
    let Some(composer) = target else { return false };
    composer.update(cx, |composer, cx| {
        let put = composer.input.update(cx, |input, cx| input.insert_reference(&link, cx));
        if put {
            let focus = composer.input.read(cx).focus_handle.clone();
            window.focus(&focus, cx);
            cx.notify();
        }
        put
    })
}

impl Composer {
    /// Let quotes find this composer.
    pub(super) fn serve_quotes(cx: &mut Context<Self>) {
        let me = cx.entity().downgrade();
        let targets = cx.default_global::<QuoteTargets>();
        targets.0.retain(|weak| weak.upgrade().is_some());
        targets.0.push(me);
    }

    /// ↑ on the composer's first line, ↓ on its last: walk the chat's earlier prompts while the
    /// composer is empty or still shows one. Whether it took the key.
    pub(super) fn walk_history(&mut self, older: bool, cx: &mut Context<Self>) -> bool {
        if self.wizard_open()
            || self.editing_queued.is_some()
            || self.mention.token.is_some()
            || self.slash.token.is_some()
        {
            return false;
        }
        let input = self.input.read(cx);
        if !input.selected_range.is_empty() || input.marked_range.is_some() {
            return false;
        }
        let edge = match input.cursor_point() {
            None => true,
            Some(point) if older => point.y < input.line_height,
            Some(point) => f32::from(point.y + input.line_height) >= input.content_height - 0.5,
        };
        if !edge {
            return false;
        }
        let current = input.text().to_owned();
        let prompts = prompts(&self.state.read(cx).transcript);
        let key = self.current_key.clone();
        let Some(text) = step_history(&mut self.extras.history, &prompts, &current, &key, older)
        else {
            return false;
        };
        self.input.update(cx, |input, cx| input.set_text(text, cx));
        cx.notify();
        true
    }

    /// Keep the draft on the chat's machine once typing rests ([`DRAFT_SAVE_DEBOUNCE`]); empty
    /// words clear it. A recalled prompt, a queued message being edited and the new-session
    /// canvas are no draft.
    pub(super) fn schedule_draft_save(&mut self, cx: &mut Context<Self>) {
        let chat = self.current_key.clone();
        if chat.is_empty() || self.wizard_open() || self.editing_queued.is_some() {
            return;
        }
        // A chat no machine holds yet (an unsaved side chat) keeps its draft here.
        let state = self.state.read(cx);
        if state.side_chat_unsaved() || !state.chats.iter().any(|c| c.id == chat) {
            return;
        }
        let text = self.input.read(cx).text().to_owned();
        if self.extras.history.as_ref().is_some_and(|c| c.shown == text) {
            return;
        }
        if self.extras.saved_drafts.get(&chat).map_or("", String::as_str) == text {
            self.extras.draft_saves.remove(&chat);
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else { return };
        let key = chat.clone();
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(DRAFT_SAVE_DEBOUNCE).await;
            let params = serde_json::json!({ "chatId": key, "text": text });
            let saved = engine.client().call(methods::SAVE_DRAFT, params).await;
            this.update(cx, |composer, _| {
                composer.extras.draft_saves.remove(&key);
                match saved {
                    Ok(_) => {
                        composer.extras.saved_drafts.insert(key, text);
                    }
                    Err(err) => tracing::debug!(error = %err, "keeping the draft failed"),
                }
            })
            .ok();
        });
        self.extras.draft_saves.insert(chat, task);
    }

    /// The draft the chat's machine keeps for `chat`, taken when the composer holds none of its
    /// own for it.
    pub(super) fn kept_draft(&mut self, chat: &str, cx: &App) -> Option<String> {
        if chat.is_empty() {
            return None;
        }
        let draft = self
            .state
            .read(cx)
            .chats
            .iter()
            .find(|c| c.id == chat)
            .and_then(|c| c.draft.clone())
            .filter(|draft| !draft.is_empty())?;
        self.extras.saved_drafts.insert(chat.to_owned(), draft.clone());
        Some(draft)
    }

    /// Compact the conversation through the agent's own `/compact`: sent now at rest, queued
    /// behind a running turn.
    pub(super) fn compact(&mut self, cx: &mut Context<Self>) {
        let busy = {
            let state = self.state.read(cx);
            let Some(chat) = state.selected_chat.as_deref() else { return };
            matches!(
                state.indicator_for(chat, chrono::Utc::now()),
                Indicator::Working | Indicator::AwaitingInput
            )
        };
        // The draft and its files are the person's, not the command's: they stay.
        let key = self.current_key.clone();
        let draft = self.input.read(cx).text().to_owned();
        let staged = self.attachments.remove(&key);
        let numbering = self.attachment_drafts.remove(&key);
        self.send("/compact".to_owned(), busy, cx);
        if let Some(staged) = staged {
            self.attachments.insert(key.clone(), staged);
        }
        if let Some(numbering) = numbering {
            self.attachment_drafts.insert(key, numbering);
        }
        if !draft.is_empty() {
            self.input.update(cx, |input, cx| input.set_text(draft, cx));
        }
        self.sync_attachment_chips(cx);
    }

    /// ⌘⇧↑/↓ on the new-session canvas: the target machine steps through the online ones. On a
    /// chat the keys select to the draft's start or end, as before.
    pub(super) fn step_machine(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.state.read(cx).selected_chat.is_some() {
            self.input.update(cx, |input, cx| {
                let to = if forward { input.content.len() } else { 0 };
                input.select_to(to, cx);
            });
            return;
        }
        let next = {
            let state = self.state.read(cx);
            let now = chrono::Utc::now();
            let mut online: Vec<(&str, &str)> = state
                .devices
                .iter()
                .filter(|d| state.device_online(&d.id, now))
                .map(|d| (d.name.as_str(), d.id.as_str()))
                .collect();
            online.sort();
            let current = state.effective_device_id();
            next_machine(&online, current.as_deref(), forward)
        };
        if let Some(id) = next {
            self.state.update(cx, |state, cx| {
                state.select_device(id, cx);
                cx.notify();
            });
            self.focus_pending = true;
            window.refresh();
        }
    }
}

/// The machine after (or before) `current` among `online` (name, id), wrapping.
pub(super) fn next_machine(
    online: &[(&str, &str)],
    current: Option<&str>,
    forward: bool,
) -> Option<String> {
    if online.is_empty() {
        return None;
    }
    let at = current.and_then(|id| online.iter().position(|(_, d)| *d == id));
    let n = online.len();
    let next = match (at, forward) {
        (None, true) => 0,
        (None, false) => n - 1,
        (Some(at), true) => (at + 1) % n,
        (Some(at), false) => (at + n - 1) % n,
    };
    online.get(next).map(|(_, id)| (*id).to_owned()).filter(|id| Some(id.as_str()) != current)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A chat's draft kept on its machine comes back when the chat opens, and what is typed is
    /// kept once typing rests 250 ms, never sooner.
    #[gpui::test]
    fn a_draft_is_restored_on_open_and_kept_once_typing_rests(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let _guard = runtime.enter();
        let directory = tempfile::tempdir().unwrap();
        cx.update(|cx| crate::settings::init(Default::default(), directory.path(), cx));
        let (out, mut requests) = tokio::sync::mpsc::channel::<String>(256);
        let (_replies, inbound) = tokio::sync::mpsc::channel::<String>(256);
        let state = cx.new(|_| AppState::new());
        state.update(cx, |state, _| {
            state.data_dir = Some(directory.path().to_path_buf());
            state.chats = vec![
                serde_json::from_value(serde_json::json!({
                    "id": "main", "deviceId": "local", "cwd": "/tmp/main",
                    "archived": false, "createdAt": chrono::Utc::now(),
                    "draft": "kept words",
                }))
                .unwrap(),
            ];
            state.set_test_engine(crate::state::EngineHandle::from_test_client(
                slopty_view_rpc::RpcClient::new(out, inbound),
            ));
        });
        let composer = cx.new(|cx| Composer::new(state.clone(), cx));
        state.update(cx, |state, cx| {
            state.selected_chat = Some("main".into());
            cx.notify();
        });
        cx.run_until_parked();
        let text = composer.read_with(cx, |composer, cx| composer.input.read(cx).text().to_owned());
        assert_eq!(text, "kept words", "the machine's draft fills an empty composer");
        let mut saves = || {
            let mut saves = Vec::new();
            while let Ok(frame) = requests.try_recv() {
                let frame = serde_json::from_str::<slopty_view_rpc::ClientFrame>(&frame).unwrap();
                if frame.method.as_deref() == Some(methods::SAVE_DRAFT) {
                    saves.push(frame.params);
                }
            }
            saves
        };
        assert!(saves().is_empty(), "a restored draft is not sent back");
        composer.update(cx, |composer, cx| {
            composer.input.update(cx, |input, cx| input.set_text("new words", cx));
        });
        cx.executor().advance_clock(Duration::from_millis(200));
        cx.run_until_parked();
        assert!(saves().is_empty(), "typing has not rested yet");
        cx.executor().advance_clock(Duration::from_millis(60));
        cx.run_until_parked();
        runtime.block_on(async { tokio::task::yield_now().await });
        cx.run_until_parked();
        assert_eq!(
            saves(),
            [serde_json::json!({ "chatId": "main", "text": "new words" })],
            "kept once, after 250 ms"
        );
    }

    fn said(id: &str, text: &str) -> SessionMessageEntry {
        SessionMessageEntry {
            id: id.into(),
            role: MessageRole::User,
            parts: vec![MessagePart::Text { id: "t".into(), text: text.into() }],
            created_at: 0,
            device_id: "d".into(),
            status: None,
            continuation_of: None,
            duration_ms: None,
        }
    }

    #[test]
    fn up_walks_back_through_the_prompts_and_down_brings_the_draft_back() {
        let prompts = prompts(&[said("1", "first"), said("2", "second"), said("3", "second")]);
        assert_eq!(prompts, ["second", "first"]);
        let mut cursor = None;
        assert_eq!(step_history(&mut cursor, &prompts, "", "c", true).as_deref(), Some("second"));
        assert_eq!(step_history(&mut cursor, &prompts, "second", "c", true).as_deref(), Some("first"));
        assert_eq!(step_history(&mut cursor, &prompts, "first", "c", true), None, "no older one");
        assert_eq!(step_history(&mut cursor, &prompts, "first", "c", false).as_deref(), Some("second"));
        assert_eq!(step_history(&mut cursor, &prompts, "second", "c", false).as_deref(), Some(""));
        assert_eq!(cursor, None, "back at the draft");
    }

    #[test]
    fn a_draft_of_ones_own_is_never_replaced() {
        let prompts = vec!["older".to_owned()];
        let mut cursor = None;
        assert_eq!(step_history(&mut cursor, &prompts, "my words", "c", true), None);
        assert_eq!(step_history(&mut cursor, &prompts, "", "c", true).as_deref(), Some("older"));
        // An edit to the recalled prompt makes it the person's own.
        assert_eq!(step_history(&mut cursor, &prompts, "older, edited", "c", true), None);
        assert_eq!(cursor, None);
        assert_eq!(step_history(&mut cursor, &prompts, "", "c", false), None, "↓ alone does nothing");
    }

    #[test]
    fn the_machine_steps_through_the_online_ones_and_wraps() {
        let online = [("mini", "m"), ("studio", "s"), ("vm", "v")];
        assert_eq!(next_machine(&online, Some("s"), true).as_deref(), Some("v"));
        assert_eq!(next_machine(&online, Some("v"), true).as_deref(), Some("m"));
        assert_eq!(next_machine(&online, Some("m"), false).as_deref(), Some("v"));
        assert_eq!(next_machine(&online, Some("gone"), true).as_deref(), Some("m"));
        assert_eq!(next_machine(&online[..1], Some("m"), true), None, "one machine: nowhere to go");
        assert_eq!(next_machine(&[], None, true), None);
    }
}
