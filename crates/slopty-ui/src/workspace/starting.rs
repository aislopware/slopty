//! A thread on its way: the tile an agent's thread will fill, there from the moment the person
//! chose it. A start from the palette opens it on the thread's own composer, in its draft mode
//! (`crate::conversation::thread::draft`): the first message is written as every later one is,
//! several lines kept as pasted, pictures and files attached, `/` and `@`, ↑ for the first
//! messages of earlier starts, and the model, mode and effort chosen on its chips. ↵ sends the
//! start with all of it (`Start::prompt`, `model`, `mode`, `effort`, `attachments`), so the
//! agent's first turn begins as it boots; ↵ on an empty composer starts it bare. Once sent it
//! says "Starting Codex" and where, until the machine answers with the thread, which takes the
//! tile's place and keyboard, or says why not and gives the draft back.
//!
//! The tile is the layout's alone: the item comes with the thread, under the tile's own id, so
//! nothing moves when it lands.
//!
//! A start in a new worktree names it, and its branch, from the first message's words
//! ([`worktree_name`]), so the branch says what the work is.

use std::collections::HashMap;

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AppContext as _, Context, ElementId, Entity, InteractiveElement as _, IntoElement as _,
    MouseButton, ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _,
    Subscription, Window, div, px,
};
use slopty_client::layout::{TileRef, WorkerKey};
use slopty_core::ItemId;
use slopty_proto::thread::wire::{Setup, Start};
use slopty_proto::thread::{AgentId, IntentId, ThreadId};

use super::WorkspaceView;
use super::actions::StartThread;
use super::area::{Handed, Placed};
use super::projects::agent_label;
use super::tile::title_ink;
use crate::colors::hsla;
use crate::conversation::attach::Target;
use crate::conversation::thread::{Draft, DraftSent, Place, ThreadView, ThreadViewEvent};
use crate::draw::Draw;
use crate::icons::Status;
use crate::kit;

/// How many first messages ↑ can bring back in a new start's composer.
const RECALLED: usize = 50;

/// The most words of the first message a worktree's name takes.
const NAME_WORDS: usize = 6;

/// The longest a worktree's name grows from those words, in bytes, before its tail.
const NAME_LEN: usize = 40;

/// The name of the worktree a start makes for `agent` in tile `item`: the first message's
/// words, lower case and joined by hyphens, as a branch reads ("fix-the-login-redirect"), then
/// the end of the tile's id, so two starts with the same words make two worktrees. A word is
/// its letters and digits in any script. Latin letters fold to ASCII ([`ascii_where_latin`]),
/// as remotes, pull request addresses, shells and CI take a branch best: "sửa lỗi đăng nhập"
/// names "sua-loi-dang-nhap"; a script with no Latin form (CJK, Cyrillic, Thai) keeps its
/// letters. With no words to take, the agent's name stands in for them.
pub(super) fn worktree_name(prompt: Option<&str>, agent: &AgentId, item: ItemId) -> String {
    // A mark is part of its word: Thai's vowels and tones, a decomposed accent.
    let category =
        icu_properties::CodePointMapData::<icu_properties::props::GeneralCategory>::new();
    let marks = icu_properties::props::GeneralCategoryGroup::Mark;
    let words: Vec<String> = prompt
        .unwrap_or_default()
        .split(|c: char| !c.is_alphanumeric() && !marks.contains(category.get(c)))
        .filter(|w| !w.is_empty())
        .take(NAME_WORDS)
        .map(|w| ascii_where_latin(&w.to_lowercase()))
        .filter(|w| !w.is_empty())
        .collect();
    let mut name = String::new();
    for word in &words {
        if !name.is_empty() && name.len().saturating_add(word.len()) >= NAME_LEN {
            break;
        }
        if !name.is_empty() {
            name.push('-');
        }
        // A word longer than the whole name is cut at a letter's edge.
        let room = NAME_LEN.saturating_sub(name.len());
        name.extend(word.chars().scan(0_usize, |used, c| {
            *used = used.saturating_add(c.len_utf8());
            (*used <= room).then_some(c)
        }));
    }
    if name.is_empty() {
        name = agent.0.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').collect();
    }
    // The id's end: its start is the clock, the same for starts close together.
    let id = item.as_uuid().simple().to_string();
    format!("{name}-{}", id.get(id.len().saturating_sub(4)..).unwrap_or(&id))
}

/// `word`, lower case, with its Latin letters as ASCII: each decomposed (NFD) and its
/// nonspacing marks dropped ("ử" is "u"), and those that do not decompose by [`LATIN_ALONE`]
/// ("đ" is "d"). A letter of another script stays as it is: a Thai or Devanagari vowel sign is
/// a nonspacing mark too, and is the word's own.
fn ascii_where_latin(word: &str) -> String {
    use icu_properties::CodePointMapData;
    use icu_properties::props::{GeneralCategory, Script};

    let nfd = icu_normalizer::DecomposingNormalizerBorrowed::new_nfd();
    let category = CodePointMapData::<GeneralCategory>::new();
    let script = CodePointMapData::<Script>::new();
    let mut out = String::with_capacity(word.len());
    // Whether the last letter was Latin: a nonspacing mark typed after it, as a decomposed
    // "u" and its horn arrive, goes with the accent it is.
    let mut after_latin = false;
    for c in word.chars() {
        let mark = category.get(c) == GeneralCategory::NonspacingMark;
        if mark && after_latin {
            continue;
        }
        if mark {
            out.push(c);
        } else if c.is_ascii() {
            after_latin = true;
            out.push(c);
        } else if script.get(c) != Script::Latin {
            after_latin = false;
            out.push(c);
        } else if let Some((_, ascii)) = LATIN_ALONE.iter().find(|(l, _)| *l == c) {
            after_latin = true;
            out.push_str(ascii);
        } else {
            after_latin = true;
            let mut one = [0_u8; 4];
            let parts = nfd.normalize(c.encode_utf8(&mut one));
            out.extend(
                parts.chars().filter(|p| category.get(*p) != GeneralCategory::NonspacingMark),
            );
        }
    }
    out
}

/// The lower-case Latin letters with no decomposition, as ASCII.
const LATIN_ALONE: [(char, &str); 15] = [
    ('đ', "d"),
    ('ð', "d"),
    ('ß', "ss"),
    ('ø', "o"),
    ('æ', "ae"),
    ('œ', "oe"),
    ('ł', "l"),
    ('þ', "th"),
    ('ı', "i"),
    ('ħ', "h"),
    ('ŧ', "t"),
    ('ŀ', "l"),
    ('ĸ', "k"),
    ('ŋ', "n"),
    ('ƒ', "f"),
];

/// The tiles of threads on their way, by the id their item will have.
#[derive(Default)]
pub(super) struct Starts {
    tiles: HashMap<ItemId, Starting>,
    /// The tile whose composer takes the keyboard in the next frame.
    focus: Option<ItemId>,
    /// The first messages of earlier starts, newest first: what ↑ brings back in a new one.
    sent: Vec<String>,
}

impl Starts {
    /// Whether `item` is a thread on its way.
    pub(super) fn has(&self, item: ItemId) -> bool {
        self.tiles.contains_key(&item)
    }

    /// The thread on its way in `item`'s tile.
    pub(super) fn get(&self, item: ItemId) -> Option<&Starting> {
        self.tiles.get(&item)
    }

    /// The thread on its way in `item`'s tile, to change before it goes.
    pub(super) fn get_mut(&mut self, item: ItemId) -> Option<&mut Starting> {
        self.tiles.get_mut(&item)
    }

    /// `item`'s start went as `start`: kept to send again as it was, and its last setup's
    /// words gone with the start they were for.
    pub(super) fn kept_start(&mut self, item: ItemId, start: Start) {
        if let Some(starting) = self.tiles.get_mut(&item) {
            starting.last = Some(start);
            starting.setup = None;
        }
    }

    /// The composer writing start `item`'s first message, while it is written: what a drop on
    /// its tile or files picked for it attach to.
    pub(super) fn composer(&self, item: ItemId) -> Option<Target> {
        let drafting = self.tiles.get(&item)?.draft.as_ref()?;
        Some(Target(drafting.view.downgrade()))
    }

    /// Each start's first message as its composer holds it, by where it starts: empty for
    /// one that went, which leaves nothing to keep.
    pub(super) fn drafts(&self, cx: &gpui::App) -> Vec<(WorkerKey, AgentId, String, String)> {
        self.tiles
            .values()
            .filter_map(|s| {
                let text =
                    if s.sent { String::new() } else { s.draft.as_ref()?.view.read(cx).draft(cx) };
                Some((s.worker, s.agent.clone(), s.cwd.clone(), text))
            })
            .collect()
    }

    /// The thread view writing start `item`'s first message, while it is written.
    pub(super) fn draft_view(&self, item: ItemId) -> Option<Entity<ThreadView>> {
        Some(self.tiles.get(&item)?.draft.as_ref()?.view.clone())
    }

    /// The link to `key`'s threads came up: the drafts of its starts ask for their folders'
    /// branches, if they have not had them ([`ThreadView::ask_branches`]).
    pub(super) fn linked(&self, key: WorkerKey, cx: &mut gpui::App) {
        for starting in self.tiles.values().filter(|s| s.worker == key) {
            if let Some(drafting) = &starting.draft {
                drafting.view.update(cx, ThreadView::ask_branches);
            }
        }
    }

    /// `key` found `paths` under `root` for `query`: the composers of its starts that asked
    /// list them in their `@` menus.
    pub(super) fn found(
        &self,
        key: WorkerKey,
        root: &str,
        query: &str,
        paths: &[String],
        cx: &mut gpui::App,
    ) {
        for starting in self.tiles.values().filter(|s| s.worker == key) {
            if let Some(drafting) = &starting.draft {
                drafting.view.update(cx, |v, cx| v.files_found(root, query, paths, cx));
            }
        }
    }
}

/// One thread on its way.
pub(super) struct Starting {
    /// The machine it starts on.
    pub worker: WorkerKey,
    /// Its agent.
    pub agent: AgentId,
    /// The folder it starts in, as the machine takes it (`~` its home).
    pub cwd: String,
    /// More words for its agent: those that take a past session up again.
    pub args: Vec<String>,
    /// It starts in a new worktree of its own, made from the clone `cwd` is in.
    pub worktree: bool,
    /// The branch that worktree starts from, as its draft chose; the clone's checked-out one
    /// when `None`.
    pub base: Option<String>,
    /// The pull request that worktree checks out, by number: its review opens beside the
    /// thread once it lands ([`super::pull_review`]).
    pub pull: Option<u32>,
    /// What its draft chose, with the first message: the model, the mode and the effort by
    /// the agent's ids (its defaults when `None`), and the files attached.
    pub chosen: Chosen,
    /// The composer writing its first message, from a start that asks for one.
    pub draft: Option<Drafting>,
    /// Whether the start went to the machine.
    pub sent: bool,
    /// The start as it last went, to send again as it was after its worktree's setup failed:
    /// the same worktree, which the worker reopens.
    pub last: Option<Start>,
    /// Its new worktree's setup, running or failed, as the machine last said.
    pub setup: Option<SetupSeen>,
}

/// A new worktree's setup, as its start tile says it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) enum SetupSeen {
    /// Running: where it came from and its newest lines.
    Running(Setup),
    /// Failed, with the code it ended with when it ended with one.
    Failed {
        /// Where it came from and its last lines.
        setup: Setup,
        /// Its exit code.
        code: Option<i32>,
    },
}

/// What a draft chose for its start ([`Starting::chosen`]).
#[derive(Default)]
pub(super) struct Chosen {
    pub model: Option<String>,
    pub mode: Option<String>,
    pub effort: Option<String>,
    pub attachments: Vec<String>,
}

/// A start's first message being written: its draft, the thread view that writes it, and
/// what the workspace hears from both.
pub(super) struct Drafting {
    draft: Entity<Draft>,
    view: Entity<ThreadView>,
    _heard: [Subscription; 2],
}

impl Starting {
    /// A thread of `agent` on `worker` in `cwd` on its way, with `draft` writing its first
    /// message or none when the start goes at once.
    pub(super) const fn new(
        worker: WorkerKey,
        agent: AgentId,
        cwd: String,
        draft: Option<Drafting>,
    ) -> Self {
        Self {
            worker,
            agent,
            cwd,
            args: Vec::new(),
            worktree: false,
            base: None,
            pull: None,
            chosen: Chosen { model: None, mode: None, effort: None, attachments: Vec::new() },
            draft,
            sent: false,
            last: None,
            setup: None,
        }
    }

    /// The same start, in a new worktree of its own when `worktree`.
    pub(super) fn in_worktree(self, worktree: bool) -> Self {
        Self { worktree, ..self }
    }

    /// The same start, with `args` for its agent.
    pub(super) fn with_args(self, args: Vec<String>) -> Self {
        Self { args, ..self }
    }
}

impl WorkspaceView {
    /// Open the tile of the thread `start` asks for, focused, the thread's composer writing
    /// its first message taking the keyboard: nothing goes to the machine until ↵. The tile's
    /// item, for what writes into that composer ([`Starts::draft_view`]).
    pub(super) fn begin_start(
        &mut self,
        start: StartThread,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> ItemId {
        let StartThread { worker, agent, cwd, worktree } = start;
        let item = ItemId::new();
        let hub = self.thread_hub(worker, cx);
        let place = self.start_place(worker, &cwd, worktree);
        let recall = self.starting.sent.clone();
        let offers = self.offers(worker, &agent);
        // Its chips begin where the agent's last start left them, as far as this machine
        // offers those choices.
        let chips = self.starts.seed(&agent, &offers);
        let others = self.startable_on(worker);
        let draft = cx.new(|_| {
            Draft::new(agent.clone(), cwd.clone(), offers, place, recall)
                .with_others(others)
                .seeded(chips)
        });
        let theme = self.theme.clone();
        // The first message left unsent in a start here before, as it was.
        let left = self.drafts.start(worker, &agent, &cwd).map(str::to_owned);
        let view = cx.new(|cx| {
            let mut view = ThreadView::drafting(hub, draft.clone(), theme, window, cx);
            if let Some(left) = left {
                view.restore_draft(&left, window, cx);
            }
            view
        });
        let sending = cx.subscribe(&draft, move |this, _draft, sent: &DraftSent, cx| {
            this.draft_sent(item, sent.clone(), cx);
        });
        let asking = cx.subscribe(&view, move |this, view, event: &ThreadViewEvent, cx| {
            this.draft_asks(item, &view, event.clone(), cx);
        });
        let drafting = Drafting { draft, view, _heard: [sending, asking] };
        let starting = Starting::new(worker, agent, cwd, Some(drafting)).in_worktree(worktree);
        self.open_starting(item, starting, cx);
        self.starting.focus = Some(item);
        item
    }

    /// `item`'s start checks pull request `number` out in its new worktree: its composer says
    /// so, holding `words` to send as they are or add to.
    pub(super) fn start_on_pull(
        &mut self,
        item: ItemId,
        number: u32,
        words: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(starting) = self.starting.get_mut(item) else { return };
        starting.pull = Some(number);
        if let Some(drafting) = &starting.draft {
            drafting.draft.update(cx, |d, cx| d.set_pull(number, cx));
            drafting.view.update(cx, |v, cx| v.restore_draft(words, window, cx));
        }
    }

    /// What a new thread of `agent` can start with on `worker`, as its link said.
    fn offers(&self, worker: WorkerKey, agent: &AgentId) -> slopty_proto::thread::Offers {
        let caps = self.workers.get(&worker).and_then(|w| w.caps.as_ref());
        let installed = caps.and_then(|c| c.agents.iter().find(|a| &a.agent == agent));
        installed.map(|a| a.offers.clone()).unwrap_or_default()
    }

    /// Where a start on `worker` in `cwd` works, by name: the folder as the tile's header
    /// says it, the machine, and whether in a new worktree of it.
    fn start_place(&self, worker: WorkerKey, cwd: &str, worktree: bool) -> Place {
        let folder = super::tile::place_name(cwd, None, self.home_of(worker))
            .unwrap_or_else(|| "~".to_owned());
        Place {
            folder: Some(folder),
            machine: self.worker_name(worker),
            worktree,
            pull: None,
            base: None,
        }
    }

    /// The empty workspace's way to begin, and ↵ there: a thread of the machine's usual agent
    /// in the machine's latest place, its first message asked in its own tile. The machine is
    /// the one in context.
    pub(super) fn start_here(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.context_worker() else { return };
        if let Some(w) = self.workers.get(&key).filter(|w| w.link.is_none()) {
            let text = format!("{} is {}", w.name, w.status.text());
            self.show_notice(text, cx);
            return;
        }
        let Some(agent) = self.agent_for(key) else {
            self.show_notice(super::agent_start::NO_AGENT.to_owned(), cx);
            return;
        };
        let latest = self.recent_places(Some(&agent), cx).into_iter().find(|p| p.worker == key);
        let cwd = latest.map_or_else(|| "~".to_owned(), |p| p.cwd);
        self.begin_start(StartThread { worker: key, agent, cwd, worktree: false }, window, cx);
    }

    /// A place the empty workspace offers, pressed: a thread of `worker`'s usual agent in
    /// `cwd`, its first message asked in its own tile; a shell there when it has no agent.
    pub(super) fn start_in(
        &mut self,
        worker: WorkerKey,
        cwd: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.agent_for(worker) {
            Some(agent) => {
                self.begin_start(StartThread { worker, agent, cwd, worktree: false }, window, cx);
            }
            None => self.open_session_on(worker, Some(cwd), Vec::new(), None, cx),
        }
    }

    /// Open the tile of `starting` in a tab of its own, focused: a start is a tab.
    pub(super) fn open_starting(
        &mut self,
        item: ItemId,
        starting: Starting,
        cx: &mut Context<Self>,
    ) {
        self.place_starting(item, starting, false, cx);
    }

    /// Open the tile of `starting`, focused: in a tab of its own, or `beside` the focused tile
    /// by the room rule (a run to compare beside the one before).
    fn place_starting(
        &mut self,
        item: ItemId,
        starting: Starting,
        beside: bool,
        cx: &mut Context<Self>,
    ) {
        let worker = starting.worker;
        self.starting.tiles.insert(item, starting);
        let tile = TileRef { worker, item };
        if beside {
            self.open_here(tile);
        } else {
            self.open_as(tile, super::tabs::Opening::Tab);
        }
        self.after_focus_moved(cx);
        self.layout_touched(cx);
        cx.notify();
    }

    /// ↵ in a start's composer: the start goes with the first message and what was chosen
    /// with it. A machine out of reach sends nothing, and the draft stays as it was.
    fn draft_sent(&mut self, item: ItemId, sent: DraftSent, cx: &mut Context<Self>) {
        let Some(starting) = self.starting.tiles.get_mut(&item) else { return };
        let worker = starting.worker;
        if let Some(w) = self.workers.get(&worker).filter(|w| w.link.is_none()) {
            let text = format!("{} is {}: nothing was sent", w.name, w.status.text());
            if let Some(drafting) = &starting.draft {
                drafting.draft.update(cx, Draft::unsent);
            }
            self.show_notice(text, cx);
            return;
        }
        let DraftSent { text, attachments, model, mode, effort, also, worktree, base } = sent;
        // Where the draft's place chip left it.
        starting.worktree = worktree;
        starting.base.clone_from(&base);
        let (cwd, pull) = (starting.cwd.clone(), starting.pull);
        let chips = slopty_client::starts::Chips {
            model: model.clone(),
            mode: mode.clone(),
            effort: effort.clone(),
        };
        let went = slopty_client::starts::LastStart {
            agent: starting.agent.clone(),
            worker,
            cwd: cwd.clone(),
            worktree,
            at: crate::clock::now(cx),
        };
        starting.chosen = Chosen { model, mode, effort, attachments: attachments.clone() };
        self.start_went(went, Some(chips), cx);
        let prompt = (!text.is_empty()).then_some(text);
        if let Some(words) = &prompt {
            let kept = &mut self.starting.sent;
            kept.retain(|w| w != words);
            kept.insert(0, words.clone());
            kept.truncate(RECALLED);
        }
        self.send_start(item, prompt.clone(), cx);
        // The same message on each other agent chosen, each in a new worktree of its own and a
        // pane of its own, opened beside the one before, at its agent's defaults: the runs
        // to compare, each on its own checkout of the pull request where the start is one's. The
        // keyboard stays with the run the person wrote.
        let runs: Vec<AgentId> = also.into_iter().filter(|_| worktree).collect();
        if runs.is_empty() {
            return;
        }
        for agent in runs {
            let run = ItemId::new();
            let mut starting = Starting::new(worker, agent, cwd.clone(), None).in_worktree(true);
            starting.pull = pull;
            starting.base.clone_from(&base);
            starting.chosen.attachments.clone_from(&attachments);
            self.place_starting(run, starting, true, cx);
            self.send_start(run, prompt.clone(), cx);
        }
        self.focus_tile(TileRef { worker, item }, cx);
    }

    /// What start `item`'s composer asks of the workspace: files attached go up to its
    /// machine, picked files are asked for, and `@` asks the machine for paths.
    fn draft_asks(
        &mut self,
        item: ItemId,
        view: &Entity<ThreadView>,
        event: ThreadViewEvent,
        cx: &mut Context<Self>,
    ) {
        let Some(worker) = self.starting.get(item).map(|s| s.worker) else { return };
        let tile = TileRef { worker, item };
        match event {
            ThreadViewEvent::Drafted => self.drafts_changed(cx),
            ThreadViewEvent::Attach { id, what } => {
                self.attach_to_composer(Some(tile), Target(view.downgrade()), id, what, cx);
            }
            ThreadViewEvent::Detach { id } => {
                self.detach_from_composer(&Target(view.downgrade()), id, cx);
            }
            ThreadViewEvent::PickFiles => {
                self.ask_files(&super::folders::FilesAsk::Import(tile), cx);
            }
            ThreadViewEvent::PickPhotos => {
                self.ask_files(&super::folders::FilesAsk::Photos(tile), cx);
            }
            ThreadViewEvent::FindFiles { root, query } => {
                self.send(worker, slopty_proto::ClientMsg::FindFiles { root, query });
            }
            // A draft has no thread to review, watch, show the terminal of or go on from yet,
            // nor a call naming a file.
            ThreadViewEvent::ShowTerminal
            | ThreadViewEvent::OpenFile { .. }
            | ThreadViewEvent::Review { .. }
            | ThreadViewEvent::ReviewRuns { .. }
            | ThreadViewEvent::KeepRun { .. }
            | ThreadViewEvent::Watch { .. }
            | ThreadViewEvent::RemoveWorktree(_)
            | ThreadViewEvent::EndAndRemove(_) => {}
        }
    }

    /// The start of `item`'s thread goes to its machine; the tile says it is starting, and a
    /// draft's composer keeps the keyboard until the thread's takes it.
    pub(super) fn starting_sent(&mut self, item: ItemId, cx: &mut Context<Self>) {
        let drafted = self.starting.tiles.get_mut(&item).is_some_and(|starting| {
            starting.sent = true;
            starting.draft.is_some()
        });
        if !drafted && self.focused().is_some_and(|t| t.item == item) {
            self.pending_focus_self = true;
        }
        cx.notify();
    }

    /// The machine answered `item`'s start with `thread`: the thread's tile, or its terminal's
    /// where its agent runs in one, takes the start's place, under its id, and its composer the
    /// keyboard when the start had it. A tile closed while it started opens nothing, and the
    /// thread is said to be there.
    pub(super) fn start_landed(
        &mut self,
        key: WorkerKey,
        item: ItemId,
        thread: ThreadId,
        agent: &AgentId,
        cx: &mut Context<Self>,
    ) {
        let tile = TileRef { worker: key, item };
        let Some(starting) = self.starting.tiles.remove(&item) else {
            let text = format!("{} started on {}", agent_label(agent), self.worker_name(key));
            self.show_notice(text, cx);
            return;
        };
        if starting.pull.is_some() {
            self.review_pull_thread(key, thread, cx);
        }
        if self.tile_of_thread(thread).is_some() {
            self.drop_starting_tile(tile, cx);
            self.open_thread(key, thread, cx);
            return;
        }
        // An agent already in a live terminal lands in that terminal's tile; else the thread's
        // own tile, until its table names a terminal ([`Self::settle_thread_tiles`]).
        match self.live_terminal(thread) {
            Some((at, session)) if at == key => self.open_terminal_as(key, session, item, cx),
            _ => self.open_thread_as(key, thread, item, cx),
        }
    }

    /// The machine would not start `item`'s thread, and `why` is said. A start written in its
    /// composer gives the draft back, its words and files as they were, to change or send
    /// again; any other start's tile goes.
    pub(super) fn start_failed(
        &mut self,
        key: WorkerKey,
        item: ItemId,
        why: String,
        cx: &mut Context<Self>,
    ) {
        if let Some(starting) = self.starting.tiles.get_mut(&item)
            && let Some(drafting) = &starting.draft
        {
            starting.sent = false;
            starting.setup = None;
            drafting.draft.update(cx, Draft::unsent);
        } else if self.starting.tiles.remove(&item).is_some() {
            self.drop_starting_tile(TileRef { worker: key, item }, cx);
        }
        self.show_notice(why, cx);
    }

    /// `key` is setting up the new worktree of start `id`: its tile says where the setup came
    /// from and its newest line, each word replacing the last.
    pub fn thread_setting_up(
        &mut self,
        key: WorkerKey,
        id: IntentId,
        setup: Setup,
        cx: &mut Context<Self>,
    ) {
        let Some(item) = self.start_item(key, id) else { return };
        let Some(starting) = self.starting.tiles.get_mut(&item) else { return };
        starting.setup = Some(SetupSeen::Running(setup));
        cx.notify();
    }

    /// The new worktree of `item`'s start failed its setup: the tile keeps the draft, as a
    /// refusal does, and says how it failed with its last lines, to try again or start
    /// without it. A start with no draft keeps its tile for the same.
    pub(super) fn setup_failed(
        &mut self,
        item: ItemId,
        setup: Setup,
        code: Option<i32>,
        cx: &mut Context<Self>,
    ) {
        let Some(starting) = self.starting.tiles.get_mut(&item) else { return };
        starting.sent = false;
        starting.setup = Some(SetupSeen::Failed { setup, code });
        if let Some(drafting) = &starting.draft {
            drafting.draft.update(cx, Draft::unsent);
        }
        cx.notify();
    }

    /// Send `item`'s start again as it last went, to the same worktree, with its setup run
    /// again when `setup`, else started without it.
    pub(super) fn start_again(&mut self, item: ItemId, setup: bool, cx: &mut Context<Self>) {
        let Some(starting) = self.starting.tiles.get(&item) else { return };
        let Some(mut start) = starting.last.clone() else { return };
        if let Some(worktree) = &mut start.worktree {
            worktree.setup = setup;
        }
        if let Some(drafting) = &starting.draft {
            drafting.draft.update(cx, Draft::resent);
        }
        self.send_start_as(item, start, cx);
    }

    /// ⌘W on a thread on its way: its tile goes. A start already sent still makes its thread,
    /// which is said when it comes.
    pub(super) fn close_starting(&mut self, tile: TileRef, cx: &mut Context<Self>) {
        let Some(starting) = self.starting.tiles.remove(&tile.item) else { return };
        tracing::debug!(item = %tile.item, sent = starting.sent, "close a thread on its way");
        // Closed, its words are let go: only a quit or a crash leaves them for the next start.
        let now = crate::clock::now(cx);
        self.drafts.set_start((starting.worker, &starting.agent, &starting.cwd), "", now);
        self.drafts_changed(cx);
        self.drop_starting_tile(tile, cx);
    }

    fn drop_starting_tile(&mut self, tile: TileRef, cx: &mut Context<Self>) {
        self.layout.remove(tile);
        self.after_focus_moved(cx);
        self.layout_touched(cx);
        cx.notify();
    }

    /// Start `item`'s composer takes the keyboard in the next frame.
    pub(super) const fn focus_start(&mut self, item: ItemId) {
        self.starting.focus = Some(item);
    }

    /// Give the keyboard to the composer of the start that asked for it.
    pub(super) fn settle_starting_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = self.starting.focus.take() else { return };
        if let Some(drafting) = self.starting.tiles.get(&item).and_then(|s| s.draft.as_ref()) {
            drafting.view.update(cx, |view, cx| view.focus(window, cx));
        }
    }

    /// The tile of a thread on its way, as the frame places it; `None` for any other tile
    /// with no item.
    pub(super) fn render_starting(
        &self,
        placed: &Placed,
        cx: &Draw<'_, Self>,
    ) -> Option<gpui::AnyElement> {
        let tile = placed.tile;
        let starting = self.starting.tiles.get(&tile.item)?;
        let theme = &self.theme;
        let s = &theme.surfaces;
        let id = tile.item;
        let label = agent_label(&starting.agent);
        let title = SharedString::from(format!("New {label} thread"));
        let ink = hsla(title_ink(theme, placed.focused));
        let status = starting.sent.then_some(Status::Working);
        // Its pane's tab row of one tab, as a tile's header is (`tile::render_header`); none
        // for a tab's one tile, which the title bar's tab names.
        let header = (!placed.lone).then(|| {
            super::tab_look::row(theme, div().id("title"))
                .debug_selector(move || format!("title-{}", id.as_uuid()))
                .role(Role::Heading)
                .aria_label(title.clone())
                .h(px(theme.density.header))
                .w_full()
                .flex_none()
                .flex()
                .items_center()
                .pr(px(theme.spacing.inset()))
                .overflow_hidden()
                .whitespace_nowrap()
                .text_size(px(theme.typography.ui_size))
                .text_color(ink)
                .font_family(theme.typography.ui_family.clone())
                .map(|el| {
                    let lead = crate::icons::Mark::agent(&starting.agent.0);
                    let look = super::tab_look::Look {
                        shown: true,
                        first: true,
                        marked: placed.focused && placed.shared,
                    };
                    let tab = super::tab_look::tab(theme, div().id("lone-tab"), look)
                        .min_w_0()
                        .gap(px(theme.spacing.sm))
                        .pl(px(theme.spacing.inset()))
                        .pr(px(theme.spacing.md))
                        .child(crate::palette::lead_slot(theme, lead, ink))
                        .child(div().min_w_0().overflow_hidden().child(title.clone()));
                    el.child(tab).children(status.map(|st| {
                        div().ml_auto().child(crate::icons::status_mark(theme, Some(st)))
                    }))
                })
        });
        // The new worktree's setup, while it runs in place of the rest, and once it failed over
        // the draft given back.
        let setup = starting.setup.as_ref().map(|seen| {
            let running = matches!(seen, SetupSeen::Running(_));
            (running, self.setup_view(seen, id, cx))
        });
        let (setting_up, failed) = match setup {
            Some((true, view)) => (Some(view), None),
            Some((false, view)) => (None, Some(view)),
            None => (None, None),
        };
        // A Claude Code start on a machine whose policy keeps the hooks off says so before
        // its first message, not after its first approval goes unseen.
        let hooks_off = self
            .hooks_off_on(starting.worker, &starting.agent.0)
            .map(|said| self.tile_line("hooks-off", id, said));
        // The thread's own composer under the tile's header, which names the agent.
        let body = Some({
            if let Some(view) = setting_up {
                div()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .p(px(theme.spacing.lg))
                    .child(view)
                    .into_any_element()
            } else if let Some(drafting) = &starting.draft {
                let view = &drafting.view;
                let width = placed.rect.w;
                let handed = Handed::Face { width };
                let theme = self.theme.clone();
                let stale = view.read(cx).theme() != &theme;
                self.hand_over(cx, view, handed, move |v, cx| {
                    v.set_layout(width, cx);
                    v.set_header(false, cx);
                });
                if stale {
                    let view = view.clone();
                    cx.later(move |_window, cx| view.update(cx, |v, cx| v.set_theme(theme, cx)));
                }
                div().flex_1().min_h_0().w_full().child(view.clone()).into_any_element()
            } else {
                let place = Place {
                    base: starting.base.clone(),
                    ..self.start_place(starting.worker, &starting.cwd, starting.worktree)
                }
                .said();
                let mark =
                    crate::icons::notice_status(theme, Status::Working, hsla(s.text_secondary));
                let said = SharedString::from(format!("Starting {label} {place}\u{2026}"));
                let notice =
                    kit::notice(theme, mark, format!("Starting {label}"), Some(place.into()))
                        .id("starting")
                        .debug_selector(move || format!("starting-{}", id.as_uuid()))
                        .role(Role::Status)
                        .aria_label(said);
                div()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .p(px(theme.spacing.lg))
                    .child(notice)
                    .into_any_element()
            }
        });
        Some(
            div()
                .id(ElementId::Uuid(*id.as_uuid()))
                .debug_selector(move || format!("item-{}", id.as_uuid()))
                .role(Role::Group)
                .aria_label(title)
                .size_full()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _ev, _w, cx| this.click_tile(tile, cx)),
                )
                // Files dropped on a start go to its composer, as on a thread's tile.
                .when(starting.draft.is_some() && !starting.sent, |el| {
                    el.on_drop(cx.listener(move |this, paths: &gpui::ExternalPaths, _w, cx| {
                        this.drop_files(tile, paths.paths(), cx);
                    }))
                })
                .map(|el| {
                    let inside = div()
                        .flex()
                        .flex_col()
                        .children(header)
                        .children(hooks_off)
                        .children(failed)
                        .children(body);
                    el.child(inside.relative().size_full().overflow_hidden())
                })
                .into_any_element(),
        )
    }

    /// What a start tile says of its new worktree's setup. Running: where it came from, under
    /// the working mark, and its newest line in the terminal's face, one line. Failed: how,
    /// its last lines in an inset as a failed check's are, and the ways on: try again in the
    /// same worktree, or start there without it.
    fn setup_view(&self, seen: &SetupSeen, id: ItemId, cx: &Draw<'_, Self>) -> gpui::AnyElement {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let mono = theme.typography.mono_families.first().cloned().unwrap_or_default();
        let line = |text: &str| {
            div()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .child(SharedString::from(text.to_owned()))
        };
        match seen {
            SetupSeen::Running(setup) => {
                let mark =
                    crate::icons::notice_status(theme, Status::Working, hsla(s.text_secondary));
                let title = format!("Setting up from {}", setup.from);
                let newest = setup.tail.last().map(|l| {
                    line(l)
                        .id("setup-line")
                        .debug_selector(move || format!("setup-line-{}", id.as_uuid()))
                        .role(Role::Label)
                        .aria_label(SharedString::from(l.clone()))
                        .max_w_full()
                        .font_family(mono.clone())
                        .text_size(px(theme.typography.small()))
                        .text_color(hsla(s.text_muted))
                });
                div()
                    .id("setting-up")
                    .debug_selector(move || format!("setting-up-{}", id.as_uuid()))
                    .role(Role::Status)
                    .aria_label(SharedString::from(title.clone()))
                    .max_w_full()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(theme.spacing.sm))
                    .child(kit::notice(theme, mark, title, None))
                    .children(newest)
                    .into_any_element()
            }
            SetupSeen::Failed { setup, code } => {
                let mark = crate::icons::notice_status(theme, Status::Failed, hsla(s.error));
                let title = format!("Setup from {} failed", setup.from);
                let detail = code.map(|c| SharedString::from(format!("exit {c}")));
                let tail = (!setup.tail.is_empty()).then(|| {
                    div()
                        .id("setup-tail")
                        .debug_selector(move || format!("setup-tail-{}", id.as_uuid()))
                        .w_full()
                        .flex()
                        .flex_col()
                        .px(px(theme.spacing.sm))
                        .py(px(theme.spacing.xs))
                        .rounded(px(theme.radii.sm))
                        .map(|el| kit::inset(el, theme))
                        .font_family(mono.clone())
                        .text_size(px(theme.typography.small()))
                        .line_height(gpui::relative(theme.typography.markdown_line_height))
                        .text_color(hsla(s.text_secondary))
                        .children(setup.tail.iter().map(|l| line(l)))
                });
                let again =
                    kit::button(theme, "setup-again", TRY_AGAIN, kit::ButtonKind::Secondary)
                        .on_click(
                            cx.listener(move |this, _ev, _w, cx| this.start_again(id, true, cx)),
                        );
                let without =
                    kit::button(theme, "setup-skip", WITHOUT_SETUP, kit::ButtonKind::Ghost)
                        .on_click(
                            cx.listener(move |this, _ev, _w, cx| this.start_again(id, false, cx)),
                        );
                let said = match code {
                    Some(c) => format!("{title}, exit {c}"),
                    None => title.clone(),
                };
                div()
                    .id("setup-failed")
                    .debug_selector(move || format!("setup-failed-{}", id.as_uuid()))
                    .role(Role::Group)
                    .aria_label(SharedString::from(said))
                    .w_full()
                    .flex_none()
                    .flex()
                    .flex_col()
                    .gap(px(theme.spacing.sm))
                    .px(px(theme.spacing.inset()))
                    .py(px(theme.spacing.md))
                    .child(kit::notice(theme, mark, title, detail))
                    .children(tail)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(theme.spacing.sm))
                            .child(again)
                            .child(without),
                    )
                    .into_any_element()
            }
        }
    }
}

/// The failed setup's way to run it again, in the same worktree.
pub(super) const TRY_AGAIN: &str = "Try again";

/// The failed setup's way to start the agent in its worktree as it is.
pub(super) const WITHOUT_SETUP: &str = "Start without setup";
