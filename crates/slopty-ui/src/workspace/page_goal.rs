//! The empty workspace's composer: the words written there start a thread of the person's usual
//! agent where work last stood, as a start with a first message would. That thread can start
//! others with the slopty tools, so a goal handed over here is how an orchestrator begins.

use gpui::{AppContext as _, Context, Window};
use slopty_client::layout::WorkerKey;
use slopty_proto::thread::AgentId;

use super::WorkspaceView;
use super::rollup::META_SEPARATOR;
use crate::conversation::thread::view::agent_label;

/// How many lines the empty workspace's composer grows to before it scrolls.
const GOAL_ROWS: usize = 8;

/// Where the empty workspace's words go: the machine, the folder and the agent.
struct PagePlace {
    worker: WorkerKey,
    /// The folder, where one is known; else the machine's home.
    cwd: Option<String>,
    agent: AgentId,
}

impl WorkspaceView {
    /// Where the page's words start a thread: the machine of the focus, else of the last start,
    /// else the first that can start an agent; there, the folder of the focus, else of the last
    /// start, else the latest work stood in, else none (home). `None` while no machine can
    /// start one.
    fn page_place(&self, cx: &gpui::App) -> Option<PagePlace> {
        let focus = self.focused().map(|t| t.worker);
        let last = self.starts.last().map(|l| l.worker);
        let mut keys: Vec<WorkerKey> = self.workers.keys().copied().collect();
        keys.sort_by_key(|k| self.worker_name(*k).to_lowercase());
        let worker = [focus, last]
            .into_iter()
            .flatten()
            .chain(keys)
            .find(|k| self.agent_for(*k).is_some())?;
        let agent = self.agent_for(worker)?;
        let here = self.focused().filter(|t| t.worker == worker).and_then(|_| self.active_cwd());
        let started = self.starts.last().filter(|l| l.worker == worker).map(|l| l.cwd.clone());
        let recent =
            || self.recent_places(Some(&agent), cx).into_iter().find(|p| p.worker == worker);
        let cwd = here.or(started).or_else(|| recent().map(|p| p.cwd));
        Some(PagePlace { worker, cwd, agent })
    }

    /// Where the empty workspace's composer sends its words, in words: the machine, the folder
    /// and the agent, as its foot shows them. `None` while no machine can start one.
    pub(super) fn page_goal_place(&self, cx: &gpui::App) -> Option<String> {
        let place = self.page_place(cx)?;
        let folder = place.cwd.as_deref().map(|cwd| {
            super::tile::place_name(cwd, None, self.home_of(place.worker))
                .unwrap_or_else(|| cwd.to_owned())
        });
        let machine = self.worker_name(place.worker);
        let parts = [machine, folder.unwrap_or_default(), agent_label(&place.agent)];
        Some(parts.into_iter().filter(|p| !p.is_empty()).collect::<Vec<_>>().join(META_SEPARATOR))
    }

    /// The empty workspace's words, by ↵ or its send: a thread of the usual agent starts with
    /// them where [`Self::page_goal_place`] says, and the composer empties.
    pub(super) fn send_page_goal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((goal, _)) = &self.empty_goal else { return };
        let goal = goal.clone();
        let words = goal.read(cx).value().trim().to_owned();
        if words.is_empty() {
            return;
        }
        let Some(place) = self.page_place(cx) else { return };
        let cwd = place.cwd.unwrap_or_else(|| "~".to_owned());
        self.start_thread(place.worker, place.agent, cwd, Some(words), cx);
        goal.update(cx, |input, cx| input.set_value("", window, cx));
    }

    /// Once a frame: the empty workspace's composer is made the first time the page shows with
    /// a machine to begin on, and takes the keyboard where nothing else holds it.
    pub(super) fn sync_empty_goal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.empty_goal.is_some() || !self.bare() || self.workers.is_empty() {
            return;
        }
        let goal = cx.new(|cx| {
            gpui_kit::component::input::TextareaState::new(window, cx)
                .placeholder(super::area::EMPTY_GOAL_HINT)
                .auto_grow(2, GOAL_ROWS)
                .submit_on_enter(true)
        });
        let entered = cx.subscribe_in(&goal, window, |this, _goal, event, window, cx| {
            match event {
                gpui_kit::component::input::InputEvent::PressEnter { shift: false, .. } => {
                    this.send_page_goal(window, cx);
                }
                // The send arms with the first word, as the thread's does.
                gpui_kit::component::input::InputEvent::Change => cx.notify(),
                _ => {}
            }
        });
        if self.focus.is_focused(window) {
            goal.update(cx, |input, cx| input.focus(window, cx));
        }
        self.empty_goal = Some((goal, entered));
    }
}
