//! The conversation face of an agent's terminal: which tiles show it, what the worker streams
//! into it, and what it sends back.
//!
//! A terminal whose agent the worker sees can show its TUI or its face, toggled per tile (⌘J,
//! the header's button). The same PTY and session go on under both. Showing the face follows the
//! session (`ConversationRequest::Follow`); hiding it, closing the tile, or the agent going
//! unfollows it, and the worker hands any prompt it held for this client back to the TUI. The
//! face keeps its draft and its scroll place while hidden. On a phone-width layout an agent's
//! tile shows the face until the person picks the TUI.

use std::collections::{HashMap, HashSet};

use gpui::{AppContext as _, Context, Entity, Focusable as _, Keystroke, Window};
use slopty_core::{ClientId, SessionId};
use slopty_proto::ClientMsg;
use slopty_proto::agent::AgentStatus;
use slopty_proto::conversation::{ConversationEvent, ConversationRequest, PermissionEvent};
use slopty_proto::items::ItemKind;
use slopty_proto::terminal::TermRequest;

use super::WorkspaceView;
use super::actions::ToggleConversation;
use crate::conversation::composer::{self, Step};
use crate::conversation::{ConversationView, FaceEvent, menu};
use crate::icons::Status;

/// What the workspace keeps about faces.
#[derive(Default)]
pub(super) struct Faces {
    /// Each agent terminal's face, made the first time it shows and kept while its session
    /// lives, draft and scroll place with it.
    pub views: HashMap<SessionId, Entity<ConversationView>>,
    /// The face or the TUI, as the person last picked for a session.
    pub chosen: HashMap<SessionId, bool>,
    /// Sessions followed, with this client's id on the link that followed them: a new link
    /// follows again.
    pub following: HashMap<SessionId, ClientId>,
    /// Sessions whose composer takes the keyboard on the next frame.
    pub focus: HashSet<SessionId>,
    /// What each face asks for.
    pub subscriptions: HashMap<SessionId, gpui::Subscription>,
    /// Faces that showed when their worker's link dropped. The session's terminal and agent
    /// state go with the link, but the face stays on its tile, draft and all, saying the
    /// worker is away, until the worker says again what runs there.
    pub held: HashSet<SessionId>,
}

impl WorkspaceView {
    /// Whether `session`'s tile shows the face: the person's pick, else the face on a
    /// phone-width layout and the TUI elsewhere. Only while an agent runs in it.
    #[must_use]
    pub fn face_shown(&self, session: SessionId) -> bool {
        if self.faces.held.contains(&session) {
            return true;
        }
        let agent = self.agent_state(session).is_some_and(|a| a.status != AgentStatus::None);
        agent && self.faces.chosen.get(&session).copied().unwrap_or_else(|| self.layout.is_phone())
    }

    /// Whether `session`'s tile shows its face with an approval open in it: the card then says
    /// what the agent waits on, and nothing else on the tile says it again.
    #[must_use]
    pub fn face_asks(&self, session: SessionId) -> bool {
        self.face_shown(session) && self.face(session).is_some_and(|face| face.asks)
    }

    /// The face of `session`, once made.
    #[must_use]
    pub fn conversation(&self, session: SessionId) -> Option<&Entity<ConversationView>> {
        self.faces.views.get(&session)
    }

    /// Show `session`'s face or its TUI.
    pub fn show_face(&mut self, session: SessionId, face: bool, cx: &mut Context<Self>) {
        self.faces.chosen.insert(session, face);
        if face {
            self.faces.focus.insert(session);
        } else {
            self.faces.focus.remove(&session);
            self.pending_focus = Some(session);
        }
        cx.notify();
    }

    /// ⌘J: the focused agent terminal between its TUI and its face.
    pub(super) fn toggle_conversation(
        &mut self,
        _: &ToggleConversation,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.focused_session() else { return };
        if self.agent_state(session).is_none_or(|a| a.status == AgentStatus::None) {
            self.show_notice("No agent runs in this terminal".to_owned(), cx);
            return;
        }
        let face = !self.face_shown(session);
        self.show_face(session, face, cx);
    }

    /// The session of the focused tile, if it is a terminal.
    pub(super) fn focused_session(&self) -> Option<SessionId> {
        match self.item(self.focused()?)?.kind {
            ItemKind::Terminal { session } => Some(session),
            _ => None,
        }
    }

    /// Bring faces in step with the tiles, once a frame: make the face a tile shows, follow
    /// what shows and unfollow what no longer does (hidden, its tile closed, its agent gone,
    /// its link new).
    /// Returns whether it gave a face the keyboard.
    pub(super) fn sync_faces(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let tiled: HashSet<SessionId> = self
            .layout
            .tiles()
            .filter_map(|t| match self.item(t)?.kind {
                ItemKind::Terminal { session } => Some(session),
                _ => None,
            })
            .collect();
        let wanted: Vec<SessionId> = tiled
            .iter()
            .copied()
            .filter(|s| {
                (self.terminals.contains_key(s) || self.faces.held.contains(s))
                    && self.face_shown(*s)
            })
            .collect();
        for session in &wanted {
            if !self.faces.views.contains_key(session) {
                self.make_face(*session, window, cx);
            }
        }
        // Unfollow what no longer shows; a link that changed lost its follow already.
        let followed: Vec<(SessionId, ClientId)> =
            self.faces.following.iter().map(|(s, c)| (*s, *c)).collect();
        for (session, by) in followed {
            let link = self.worker_of_session(session).and_then(|w| self.me(w));
            if link != Some(by) {
                self.faces.following.remove(&session);
                if let Some(view) = self.faces.views.get(&session) {
                    view.update(cx, ConversationView::unfollowed);
                }
            } else if !wanted.contains(&session) {
                self.send_session(
                    session,
                    ClientMsg::Conversation(ConversationRequest::Unfollow { session }),
                );
                self.faces.following.remove(&session);
                if let Some(view) = self.faces.views.get(&session) {
                    view.update(cx, ConversationView::unfollowed);
                }
            }
        }
        for session in &wanted {
            let me = self.worker_of_session(*session).and_then(|w| self.me(w));
            if let Some(me) = me
                && !self.faces.following.contains_key(session)
            {
                self.send_session(
                    *session,
                    ClientMsg::Conversation(ConversationRequest::Follow { session: *session }),
                );
                self.faces.following.insert(*session, me);
                if let Some(view) = self.faces.views.get(session) {
                    view.update(cx, |v, _| v.set_me(Some(me)));
                }
            }
        }
        let views: Vec<(SessionId, Entity<ConversationView>)> =
            self.faces.views.iter().map(|(s, v)| (*s, v.clone())).collect();
        for (session, view) in views {
            let shown = wanted.contains(&session);
            let was = view.read(cx).shown();
            // The keyboard goes with the tile's body: into the face that replaced a focused
            // TUI (the phone's default, not only ⌘J), back to the TUI from a face that went.
            if shown && !was {
                let terminal = self.terminals.get(&session).map(|t| t.read(cx).focus_handle(cx));
                if terminal.is_some_and(|h| h.contains_focused(window, cx)) {
                    self.faces.focus.insert(session);
                }
            } else if !shown && was && view.read(cx).focus_handle(cx).contains_focused(window, cx) {
                self.pending_focus = Some(session);
            }
            view.update(cx, |v, cx| v.set_shown(shown, cx));
            let away = self
                .worker_of_session(session)
                .and_then(|key| self.workers.get(&key))
                .filter(|w| w.link.is_none())
                .map(|w| w.name.clone());
            view.update(cx, |v, cx| v.set_away(away, cx));
        }
        // A held face lets go once its worker has said again what runs in the session, or
        // once its tile or worker is gone.
        let held: Vec<SessionId> = self.faces.held.iter().copied().collect();
        for session in held {
            let back = self.terminals.contains_key(&session) && self.agent_state(session).is_some();
            if back || !tiled.contains(&session) || self.worker_of_session(session).is_none() {
                self.faces.held.remove(&session);
            }
        }
        // Faces of sessions that are gone go with them.
        let live: HashSet<SessionId> =
            self.terminals.keys().chain(&self.faces.held).copied().collect();
        self.faces.views.retain(|s, _| live.contains(s));
        self.faces.subscriptions.retain(|s, _| live.contains(s));
        self.faces.chosen.retain(|s, _| live.contains(s));
        self.faces.following.retain(|s, _| live.contains(s));
        let focus: Vec<SessionId> = self.faces.focus.drain().collect();
        let mut gave = false;
        for session in focus {
            if let Some(view) = self.faces.views.get(&session).filter(|_| wanted.contains(&session))
            {
                view.clone().update(cx, |v, cx| v.focus(window, cx));
                gave = true;
            }
        }
        gave
    }

    fn make_face(&mut self, session: SessionId, window: &mut Window, cx: &mut Context<Self>) {
        let theme = self.theme.clone();
        let view = cx.new(|cx| ConversationView::new(session, theme, window, cx));
        let agent = self.agent_state(session).cloned();
        view.update(cx, |v, cx| v.set_agent(agent, cx));
        let subscription = cx.subscribe(&view, move |this, _view, event: &FaceEvent, cx| {
            this.face_event(session, event.clone(), cx);
        });
        self.faces.subscriptions.insert(session, subscription);
        // Its facts, copied whenever it changes ([`Self::face_changed`]).
        cx.observe(&view, move |this, _view, cx| this.face_changed(session, cx)).detach();
        self.faces.views.insert(session, view);
        let _news = self.copy_face(session, cx);
    }

    /// What a face asks for.
    fn face_event(&mut self, session: SessionId, event: FaceEvent, cx: &mut Context<Self>) {
        match event {
            FaceEvent::Submit { text, paths } => {
                Self::type_into(session, composer::submission(&text, &paths), cx);
            }
            FaceEvent::Interrupt => Self::type_into(session, vec![composer::interrupt()], cx),
            FaceEvent::Answer { ask, verdict } => {
                let answer = ConversationRequest::Answer { session, ask, verdict };
                self.send_session(session, ClientMsg::Conversation(answer));
            }
            FaceEvent::Expand { thread, reference } => {
                let expand = ConversationRequest::Expand { session, thread, reference };
                self.send_session(session, ClientMsg::Conversation(expand));
            }
            FaceEvent::ShowTerminal => self.show_face(session, false, cx),
            FaceEvent::Attach { id, what } => self.attach_to_face(session, id, what, cx),
            FaceEvent::Detach { id } => self.detach_from_face(session, id, cx),
            FaceEvent::PickFiles => {
                if let Some(tile) = self.tile_of_session(session) {
                    self.ask_files(&super::folders::FilesAsk::Import(tile), cx);
                }
            }
            FaceEvent::Search { query } => {
                let search = ConversationRequest::Search { session, query, limit: menu::MENTIONS };
                self.send_session(session, ClientMsg::Conversation(search));
            }
            FaceEvent::OpenPath { path } => self.open_mention(session, &path, cx),
            FaceEvent::Reconnect => {
                let connect = self
                    .worker_of_session(session)
                    .and_then(|key| self.host_actions(key)?.connect.clone());
                if let Some(run) = connect {
                    self.pending_runs.push(run);
                    cx.notify();
                }
            }
            FaceEvent::Rewind => {
                // The person's click, typed as they would type it: Claude Code's own menu picks
                // the point, never the face.
                self.show_face(session, false, cx);
                Self::type_into(session, composer::submission("/rewind", &[]), cx);
            }
        }
    }

    /// Open what a prompt's `@` mention names on the session's worker: a path relative to the
    /// agent's directory, as Claude Code reads it, or one spelled from the root.
    fn open_mention(&mut self, session: SessionId, path: &str, cx: &mut Context<Self>) {
        let Some(key) = self.worker_of_session(session) else { return };
        let path = if path.starts_with('/') || path.starts_with('~') {
            path.to_owned()
        } else {
            let Some(cwd) = self.summary(session).and_then(|s| s.cwd.clone()) else { return };
            format!("{}/{path}", cwd.trim_end_matches('/'))
        };
        self.open_path_on(key, &path, None, cx);
    }

    /// Type `steps` into `session`'s terminal the way a person at its view would: a message as
    /// the view's paste, a command as raw input, keys through the view's key path.
    fn type_into(session: SessionId, steps: Vec<Step>, cx: &Context<Self>) {
        cx.spawn(async move |this, cx| {
            for step in steps {
                if let Step::Pause(pause) = step {
                    cx.background_executor().timer(pause).await;
                    continue;
                }
                let sent = this.update(cx, |this, cx| match step {
                    Step::Type(text) => {
                        let req = TermRequest::Raw(text.into_bytes());
                        this.send_session(session, ClientMsg::Term { session, req });
                    }
                    Step::Paste(text) => {
                        if let Some(view) = this.terminals.get(&session) {
                            view.update(cx, |v, cx| v.paste(text, cx));
                        }
                    }
                    Step::Key(key) => {
                        if let (Some(view), Ok(key)) =
                            (this.terminals.get(&session), Keystroke::parse(key))
                        {
                            view.update(cx, |v, cx| v.press(key, cx));
                        }
                    }
                    Step::Pause(_) => {}
                });
                if sent.is_err() {
                    return;
                }
            }
        })
        .detach();
    }

    /// An event on a conversation this client follows.
    pub fn conversation_event(
        &self,
        session: SessionId,
        event: ConversationEvent,
        cx: &mut Context<Self>,
    ) {
        // What the face's news changes for the tiles and the chrome follows it
        // ([`Self::face_changed`]): a streaming answer must not draw the frame once a delta.
        if let Some(view) = self.faces.views.get(&session) {
            view.update(cx, |v, cx| v.apply(event, cx));
        }
    }

    /// Whether `session`'s face shows the agent mid-turn ([`ConversationView::mid_turn`]).
    fn face_live(&self, session: SessionId) -> bool {
        self.face(session).is_some_and(|face| face.mid_turn)
    }

    /// `session`'s agent in the status vocabulary, as a tile marks it: the worker's word, but
    /// never calmer than working while the face shows a call in flight. A hook can lag or be
    /// missing; the transcript the face projects then knows more, and saying "Idle" over a
    /// spinning row reads as a broken status. Only the mark follows: nothing is driven.
    pub(super) fn agent_mark(&self, session: SessionId) -> Option<Status> {
        let status = self.agent_state(session).and_then(Status::of_agent)?;
        let calm = matches!(status, Status::Idle | Status::Done);
        Some(if calm && self.face_live(session) { Status::Working } else { status })
    }

    /// A permission prompt this client may answer was asked or settled: the face of a followed
    /// session, and the inbox and notes for a yes or no.
    pub fn permission_event(&mut self, event: PermissionEvent, cx: &mut Context<Self>) {
        self.approval_event(&event, cx);
        cx.notify();
        let session = event.session();
        if let Some(view) = self.faces.views.get(&session) {
            view.update(cx, |v, cx| v.permission(event, None, cx));
        }
    }

    /// The face's one line of what the agent does, for the navigator and the overview.
    pub(super) fn face_summary(&self, session: SessionId) -> Option<String> {
        self.face(session)?.summary.clone()
    }
}
