//! What the strip and the chrome show of a body that changes on its own: a shell's command and
//! title, a stream's first frame and sound, a file's unsaved edit, a page's address and title, a
//! folder's way up. Copied out of the body each time it changes, and news for the views that show
//! it only when the copy changes.
//!
//! GPUI draws a view again when an entity it read changed. A shell changes with every line of
//! output, a stream with every frame; a header or a navigator
//! row that read them would be built again as often, for a title or a mark that stayed as it
//! was. So the strip and the chrome read these facts, which are the workspace's, and never the
//! bodies themselves.

use std::time::{Duration, Instant};

use gpui::{App, Context};
use slopty_core::{ItemId, SessionId};
use slopty_proto::screen::SourceState;

use super::WorkspaceView;
use crate::browser::BrowserView;
use crate::file::FileView;
use crate::folder::FolderView;
use crate::screen::{ScreenView, StreamHeader};
use crate::terminal::TerminalView;

/// How often the running readouts' clock moves: they count whole seconds.
const READOUT_TICK: Duration = Duration::from_secs(1);

/// What the workspace shows of a shell.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct ShellFacts {
    /// The title its program set (OSC 0/2).
    pub title: Option<String>,
    /// The command it runs.
    pub running: Option<String>,
    /// Since when this client has seen it run, when it saw it start.
    pub started: Option<Instant>,
    /// The command it ran before the newest prompt.
    pub last: Option<String>,
    /// How the command before the newest prompt ended.
    pub exit: Option<u8>,
    /// It has drawn a prompt: a shell that marks its prompts, not a program run bare.
    pub prompted: bool,
    /// That command failed and its block shows in the grid ([`TerminalView::failure_in_view`]).
    pub failure_in_view: bool,
    /// This client's size rules the PTY.
    pub driving: bool,
}

impl ShellFacts {
    pub(super) fn of(view: &TerminalView) -> Self {
        let state = view.state();
        let prompt = state.prompt_before(slopty_grid::LineIndex(u64::MAX));
        let exit = prompt.and_then(|prompt| state.line(prompt)?.mark.exit());
        Self {
            title: view.title().map(str::to_owned),
            running: state.running_command().map(str::to_owned),
            started: view.command_started(),
            last: state.last_command(),
            exit,
            prompted: prompt.is_some(),
            failure_in_view: view.failure_in_view(),
            driving: view.driving(),
        }
    }
}

/// What the workspace shows of a remote window's or display's stream.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct ScreenFacts {
    /// Asked for and live, with no frame drawn yet.
    pub waiting: bool,
    /// A frame has been drawn.
    pub drawn: bool,
    /// The picture's size, in pixels.
    pub size: (u32, u32),
    /// The worker has sent sound for it.
    pub has_audio: bool,
    /// Its sound is silenced here.
    pub muted: bool,
    /// The system's shortcuts go to the worker.
    pub system_keys: bool,
    /// What its header shows of it.
    pub header: StreamHeader,
}

impl ScreenFacts {
    pub(super) fn of(view: &ScreenView) -> Self {
        let drawn = view.frames() > 0;
        Self {
            waiting: !drawn && view.source_state() == SourceState::Live,
            drawn,
            size: view.size(),
            has_audio: view.has_audio(),
            muted: view.muted(),
            system_keys: view.system_keys(),
            header: view.header(),
        }
    }
}

/// What the workspace shows of a file: whether its edit is not yet on disk, and which face a
/// Markdown file shows. Its editor changes with every keystroke and caret blink; the header's
/// dot and toggle, a few times an edit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct FileFacts {
    /// An edit not yet on disk, or a save not yet answered.
    pub unsaved: bool,
    /// A Markdown file's text is in: whether its preview shows (else its source). `None` for
    /// any other file, or one with no text yet.
    pub preview: Option<bool>,
    /// Its text is in the editor: the overview can say what it holds.
    pub has_text: bool,
    /// It changed on disk under an unsaved edit.
    pub conflict: bool,
}

impl FileFacts {
    pub(super) fn of(view: &FileView) -> Self {
        Self {
            unsaved: view.dirty() || view.saving(),
            preview: (view.has_preview() && view.shows_text()).then(|| view.previewing()),
            has_text: view.shows_text(),
            conflict: matches!(view.trouble(), Some(crate::file::Trouble::Conflict)),
        }
    }
}

/// What the workspace shows of a page.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct PageFacts {
    /// Its address, which the header shows.
    pub url: String,
    /// What names its tile ([`BrowserView::title`]).
    pub title: String,
    /// Its address, short ([`BrowserView::short_url`]).
    pub short_url: String,
    /// The page has a title of its own.
    pub titled: bool,
    /// It has a page to go back to.
    pub can_go_back: bool,
    /// It has a page to go forward to.
    pub can_go_forward: bool,
}

impl PageFacts {
    pub(super) fn of(view: &BrowserView) -> Self {
        let page = view.page();
        Self {
            url: page.url.clone(),
            title: view.title(),
            short_url: view.short_url().to_owned(),
            titled: !page.title.trim().is_empty(),
            can_go_back: page.can_go_back,
            can_go_forward: page.can_go_forward,
        }
    }
}

/// What the workspace shows of a folder: whether it has a folder above it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct FolderFacts {
    /// The way up has somewhere to go.
    pub has_parent: bool,
}

impl FolderFacts {
    pub(super) fn of(view: &FolderView) -> Self {
        Self { has_parent: view.parent().is_some() }
    }
}

impl WorkspaceView {
    /// `session`'s shell as last copied.
    pub(super) fn shell(&self, session: SessionId) -> Option<&ShellFacts> {
        self.facts.shells.get(&session)
    }

    /// Item `id`'s stream as last copied.
    pub(super) fn stream(&self, id: ItemId) -> Option<&ScreenFacts> {
        self.facts.screens.get(&id)
    }

    /// Copy `session`'s shell again: what changed, if anything.
    pub(super) fn copy_shell(
        &mut self,
        session: SessionId,
        cx: &App,
    ) -> Option<(ShellFacts, ShellFacts)> {
        let now = ShellFacts::of(self.terminals.get(&session)?.read(cx));
        let was = self.facts.shells.insert(session, now.clone()).unwrap_or_default();
        (was != now).then_some((was, now))
    }

    /// Copy item `id`'s stream again. When it changed, everything that shows it is drawn again:
    /// a stream's facts change a few times in its life, not with its frames.
    pub(super) fn stream_changed(&mut self, id: ItemId, cx: &mut Context<Self>) {
        self.follow_secure_input(cx);
        let Some(view) = self.screens.get(&id) else { return };
        let now = ScreenFacts::of(view.read(cx));
        if self.facts.screens.insert(id, now) != Some(now) {
            cx.notify();
        }
    }

    /// Item `id`'s file as last copied.
    pub(super) fn file_facts(&self, id: ItemId) -> FileFacts {
        self.facts.files.get(&id).copied().unwrap_or_default()
    }

    /// Item `id`'s page as last copied.
    pub(super) fn page_facts(&self, id: ItemId) -> Option<&PageFacts> {
        self.facts.pages.get(&id)
    }

    /// Item `id`'s folder as last copied.
    pub(super) fn folder_facts(&self, id: ItemId) -> FolderFacts {
        self.facts.folders.get(&id).copied().unwrap_or_default()
    }

    /// Copy item `id`'s file again. Its dot is its header's news, and only when it changed.
    pub(super) fn file_changed(&mut self, id: ItemId, cx: &mut Context<Self>) {
        let Some(view) = self.files.get(&id) else { return };
        let now = FileFacts::of(view.read(cx));
        if self.facts.files.insert(id, now) != Some(now) {
            App::notify(cx, self.area_host.entity_id());
        }
    }

    /// Copy item `id`'s page again. Its title or address names its tile everywhere; its way
    /// back is the strip's news.
    pub(super) fn page_changed(&mut self, id: ItemId, cx: &mut Context<Self>) {
        let Some(view) = self.browsers.get(&id) else { return };
        let now = PageFacts::of(view.read(cx));
        let was = self.facts.pages.insert(id, now.clone());
        let Some(was) = was else {
            self.titles_dirty = true;
            cx.notify();
            return;
        };
        if (&was.title, &was.short_url, was.titled) != (&now.title, &now.short_url, now.titled) {
            self.titles_dirty = true;
            cx.notify();
        } else if was != now {
            App::notify(cx, self.area_host.entity_id());
        }
    }

    /// Copy item `id`'s folder again: its way up is its header's news.
    pub(super) fn folder_changed(&mut self, id: ItemId, cx: &mut Context<Self>) {
        let Some(view) = self.folders.get(&id) else { return };
        let now = FolderFacts::of(view.read(cx));
        if self.facts.folders.insert(id, now) != Some(now) {
            App::notify(cx, self.area_host.entity_id());
        }
    }

    /// Now, as a running readout counts it (a command's time): the last tick,
    /// a second at most behind the clock. Built from the clock itself, a readout drawn from
    /// the last frame would show the second it was built in, and one built again another.
    pub(super) const fn ticked(&self) -> Option<(Instant, u64)> {
        self.facts.ticked
    }

    /// Keep the readouts' clock moving, once a second, while a command runs, each tick news for
    /// the views that count: the strip's headers and the navigator's rows.
    pub(super) fn keep_time(&mut self, cx: &Context<Self>) {
        if self.facts.ticking || !self.readouts_run() {
            return;
        }
        self.facts.ticking = true;
        // Nothing counted before this: the clock moves with no news for anybody.
        self.facts.ticked = Some(Self::readout_now());
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(READOUT_TICK).await;
                let going = this.update(cx, |this, cx| {
                    this.facts.ticking = this.readouts_run();
                    if this.facts.ticking {
                        this.tick_readouts(cx);
                    }
                    this.facts.ticking
                });
                if !matches!(going, Ok(true)) {
                    return;
                }
            }
        })
        .detach();
    }

    /// Move the readouts' clock to now, and tell the views that show a count: the navigator for
    /// a command's time, the strip for a header's.
    pub(super) fn tick_readouts(&mut self, cx: &mut Context<Self>) {
        self.facts.ticked = Some(Self::readout_now());
        let counting = |session: &SessionId| self.running_for(*session).is_some();
        if self.facts.shells.keys().any(counting) {
            App::notify(cx, self.chrome.nav_rows.entity_id());
            App::notify(cx, self.area_host.entity_id());
        }
    }

    /// The clocks a readout counts by, read now.
    fn readout_now() -> (Instant, u64) {
        (Instant::now(), slopty_core::WallMs::now().as_millis())
    }

    /// Whether a readout counts: a command runs.
    fn readouts_run(&self) -> bool {
        self.facts.shells.values().any(|shell| shell.running.is_some())
    }

    /// Forget the facts of bodies no longer kept.
    pub(super) fn prune_facts(&mut self) {
        let Self { facts, terminals, screens, files, browsers, folders, .. } = self;
        facts.shells.retain(|session, _| terminals.contains_key(session));
        facts.screens.retain(|id, _| screens.contains_key(id));
        facts.files.retain(|id, _| files.contains_key(id));
        facts.pages.retain(|id, _| browsers.contains_key(id));
        facts.folders.retain(|id, _| folders.contains_key(id));
    }
}

/// The facts of every body the workspace keeps, by kind, and the clock their running readouts
/// count by.
#[derive(Debug, Default)]
pub(super) struct Facts {
    shells: std::collections::HashMap<SessionId, ShellFacts>,
    screens: std::collections::HashMap<ItemId, ScreenFacts>,
    files: std::collections::HashMap<ItemId, FileFacts>,
    pages: std::collections::HashMap<ItemId, PageFacts>,
    folders: std::collections::HashMap<ItemId, FolderFacts>,
    /// The readouts' last tick ([`WorkspaceView::keep_time`]): the monotonic clock, and the
    /// wall clock in Unix milliseconds for what a worker stamped.
    ticked: Option<(Instant, u64)>,
    /// A tick is on its way.
    ticking: bool,
}

impl Facts {
    /// How many facts are kept of each kind, for the footprint.
    #[cfg(test)]
    pub(super) fn lens(&self) -> [(&'static str, usize); 5] {
        [
            ("facts.shells", self.shells.len()),
            ("facts.screens", self.screens.len()),
            ("facts.files", self.files.len()),
            ("facts.pages", self.pages.len()),
            ("facts.folders", self.folders.len()),
        ]
    }
}
