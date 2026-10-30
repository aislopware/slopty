//! An agent drives the person's window (`slopty ui`, the `ui_*` tools): it opens a
//! shell, a file, a page or the changes in a pane beside the session (`docs/decisions/layout.md`,
//! "No permanent right column"), shows or hides a region, puts a chat on screen, lays the panes
//! at even shares, and reads what the window shows. Slopty's workspace hands each ask here through
//! [`act`] once the shell's window is up ([`serve`]), and sends what [`act`] answers back to the
//! agent.
//!
//! The person stays in charge (`docs/decisions/agents.md`, "Agents drive the person's window
//! through open verbs"). An ask never takes the keyboard: whatever had it has it after, and a
//! terminal or an editor the agent opens waits for a click. One that would move the chat or the
//! keyboard away from a draft being typed, or hide the region the keyboard is in, is refused
//! ([`Code::Busy`]), as is putting another chat on screen while the person is in Settings. A path
//! is the agent's machine's, so it opens only beside a chat on that machine
//! ([`Code::Elsewhere`]). A command is typed as a transcript's Run types it: one that acts as
//! root, destroys or spans lines waits at the prompt for the person to read and run. Each change
//! is marked with a notice naming the agent and what it did.
//!
//! Names stay open: a region, a place or a chat is a word, and one the window does not know is
//! answered with the ones it does ([`Code::Unknown`]).
#![allow(
    rustdoc::private_intra_doc_links,
    clippy::needless_pass_by_ref_mut,
    clippy::needless_pass_by_value,
    clippy::too_long_first_doc_paragraph,
    clippy::too_many_arguments,
    reason = "shell lint migration: slice A"
)]

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use gpui::{
    AnyWindowHandle, App, AppContext as _, Context, FocusHandle, Global, WeakEntity, Window,
};
use slopty_view_proto::TerminalSession;
use slopty_view_rpc::methods;

use super::panes::Pane;
use super::{NavEntry, RightSurface, Route, Shell};
use crate::terminal::panel::TerminalPanel;

/// How long a fresh terminal's shell is waited for before its command is given up on.
const SHELL_WITHIN: Duration = Duration::from_secs(10);

/// How often a terminal still coming up is asked again to take its command.
const ASK_EVERY: Duration = Duration::from_millis(50);

/// The regions an agent names, as [`WindowState::regions`] lists them: the sidebar, the views
/// it can summon beside the session, and the shells split beside it.
pub const REGIONS: [&str; 5] = ["sidebar", "files", "changes", "inspector", "terminal"];

/// One ask of the window: what to do and who asks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ask {
    /// What to do.
    pub act: Act,
    /// The asking agent's own chat, when the window lists it.
    pub chat: Option<String>,
    /// The machine the agent runs on, as the window names devices; its paths are that
    /// machine's.
    pub device: Option<String>,
    /// Who asks, as the mark names them.
    pub who: String,
}

/// What an agent asks of the window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Act {
    /// Say what the window shows.
    State,
    /// A shell in a pane beside the chat on screen.
    Terminal {
        /// The folder it starts in, absolute on the agent's machine; the chat's own when `None`.
        folder: Option<String>,
        /// A command line to type once its shell is up.
        command: Option<String>,
    },
    /// Something in a pane beside the session.
    Open(Target),
    /// One region shown, hidden, expanded or collapsed.
    Region {
        /// Its name, one of [`REGIONS`].
        region: String,
        /// What becomes of it.
        change: Change,
    },
    /// A chat on screen: by id, id prefix or title; the agent's own when `None`.
    Chat(Option<String>),
    /// The session's panes at even shares, `target` opened beside it first.
    Split(Option<Target>),
}

/// What a pane beside the session shows for an agent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// A file, as a transcript's file link names it (`file:///w/a.rs#L12`).
    File(String),
    /// A web page, by its address.
    Page(String),
    /// The chat's uncommitted changes.
    Diff,
}

/// What becomes of a region.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    /// On screen.
    Show,
    /// Off screen.
    Hide,
    /// The other of the two.
    Toggle,
    /// As large as it goes: shown.
    Expand,
    /// Back out of the way: hidden.
    Collapse,
}

/// What an ask did, and the window after it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Done {
    /// What changed, in words, as the mark says it; `None` when nothing changed.
    pub did: Option<String>,
    /// The tab it opened.
    pub made: Option<Tab>,
    /// For a terminal's command: it ran, or it waits at the prompt.
    pub ran: Option<bool>,
    /// The window after it.
    pub window: WindowState,
}

/// What the window shows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WindowState {
    /// `chat`, or `settings/<section>`.
    pub page: String,
    /// The chat on screen.
    pub chat: Option<ChatShown>,
    /// Each region and whether it shows.
    pub regions: Vec<Region>,
    /// The panes beside the session, in reading order.
    pub tabs: Vec<Tab>,
    /// The person is typing a draft.
    pub typing: bool,
}

/// The chat on screen.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChatShown {
    /// Its id.
    pub id: String,
    /// Its title.
    pub title: Option<String>,
    /// Its machine, by name.
    pub machine: Option<String>,
}

/// One region of the window.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Region {
    /// Its name.
    pub name: String,
    /// On screen.
    pub shown: bool,
    /// Its pane fills the session's view (zoomed).
    pub expanded: bool,
}

/// One pane beside the session.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tab {
    /// `pane/<kind>/<n>` for a view, `terminal/<n>` for a shell.
    pub id: String,
    /// What it shows.
    pub kind: String,
    /// Its title.
    pub title: String,
    /// The region it belongs to, one of [`REGIONS`], or `pane` for a view of no region.
    pub region: String,
    /// It has the keyboard.
    pub selected: bool,
    /// A terminal's session id.
    pub terminal: Option<String>,
    /// The file it shows, or the page's address.
    pub target: Option<String>,
}

/// Why an ask did nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    /// What kind of failure.
    pub code: Code,
    /// What went wrong and what would do it.
    pub message: String,
}

/// What kind of failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Code {
    /// No window to drive.
    NoWindow,
    /// A name the window does not know.
    Unknown,
    /// The person is typing or in Settings.
    Busy,
    /// The chat on screen is not on the agent's machine, or there is none.
    Elsewhere,
    /// It was tried and did not work.
    Failed,
}

fn fail<T>(code: Code, message: impl Into<String>) -> Result<T, Failure> {
    Err(Failure { code, message: message.into() })
}

/// Where an ask's answer goes, once.
pub type Answered = Box<dyn FnOnce(Result<Done, Failure>, &mut App)>;

/// The shell window agents drive.
struct Served {
    shell: WeakEntity<Shell>,
    window: AnyWindowHandle,
}

impl Global for Served {}

/// The shell in `window` takes agents' asks from now on. Called each frame, cheaply: only a
/// new shell or window changes what is served.
pub(super) fn serve(window: &Window, cx: &mut Context<Shell>) {
    let shell = cx.entity().downgrade();
    let handle = window.window_handle();
    let same = cx
        .try_global::<Served>()
        .is_some_and(|served| served.window == handle && served.shell == shell);
    if !same {
        cx.set_global(Served { shell, window: handle });
    }
}

/// Do `ask` in the person's window, and hand `answered` what it did; with no window up, that
/// there is none.
pub fn act(ask: Ask, answered: Answered, cx: &mut App) {
    let answered = Rc::new(Cell::new(Some(answered)));
    let once = Rc::clone(&answered);
    let reply = move |outcome: Result<Done, Failure>, cx: &mut App| {
        if let Some(answered) = once.take() {
            answered(outcome, cx);
        }
    };
    let served = cx.try_global::<Served>().map(|s| (s.shell.clone(), s.window));
    let done = served.and_then(|(shell, window)| {
        window
            .update(cx, |_, window, cx| {
                shell.update(cx, |shell, cx| shell.agent_act(ask, Box::new(reply), window, cx))
            })
            .ok()?
            .ok()
    });
    if done.is_none()
        && let Some(answered) = answered.take()
    {
        let message = "no Slopty window is open: the person's app has no window up";
        answered(fail(Code::NoWindow, message), cx);
    }
}

/// The keyboard as it was before an ask, put back after it and after the frames that follow,
/// where a surface mounting would claim it.
fn keep_keyboard(before: Option<FocusHandle>, window: &mut Window, cx: &mut App) {
    let Some(before) = before else { return };
    let put_back = move |window: &mut Window, cx: &mut App| {
        if !before.is_focused(window) {
            window.focus(&before, cx);
        }
    };
    let again = put_back.clone();
    put_back(window, cx);
    window.on_next_frame(move |window, cx| {
        again(window, cx);
        window.on_next_frame(put_back);
    });
}

/// What a region change does with what is now `shown`: show it, hide it, or nothing.
const fn shows(change: Change, shown: bool) -> bool {
    match change {
        Change::Show | Change::Expand => true,
        Change::Hide | Change::Collapse => false,
        Change::Toggle => !shown,
    }
}

/// The region an agent names, by its name or one the window's own words use.
fn region_named(name: &str) -> Option<&'static str> {
    let name = name.trim().to_ascii_lowercase().replace(['-', '_'], " ");
    Some(match name.as_str() {
        "sidebar" | "side bar" | "sessions" | "chats" => "sidebar",
        "files" | "explorer" => "files",
        "changes" | "diff" | "git" => "changes",
        "inspector" | "trace" => "inspector",
        "terminal" | "terminals" | "shell" | "shells" => "terminal",
        _ => return None,
    })
}

/// The chat `key` names among `chats` (id, title): its id exactly, else the one id it starts,
/// else its title. Several or none is an error naming them.
fn chat_named<'a>(
    key: &str,
    chats: impl Iterator<Item = (&'a str, Option<&'a str>)> + Clone,
) -> Result<String, Failure> {
    let key = key.trim();
    if let Some((id, _)) = chats.clone().find(|(id, _)| *id == key) {
        return Ok(id.to_owned());
    }
    let by = |hits: Vec<(&str, Option<&str>)>| -> Option<Result<String, Failure>> {
        match hits.as_slice() {
            [] => None,
            [(id, _)] => Some(Ok((*id).to_owned())),
            several => {
                let named: Vec<String> = several
                    .iter()
                    .map(|(id, title)| format!("{} ({id})", title.unwrap_or("untitled")))
                    .collect();
                Some(fail(
                    Code::Unknown,
                    format!("{key} names several chats: {}", named.join(", ")),
                ))
            }
        }
    };
    let prefixed = chats.clone().filter(|(id, _)| id.starts_with(key)).collect();
    if let Some(found) = by(prefixed) {
        return found;
    }
    let titled = chats
        .filter(|(_, title)| title.is_some_and(|t| t.trim().eq_ignore_ascii_case(key)))
        .collect();
    by(titled).unwrap_or_else(|| fail(Code::Unknown, format!("no chat is called {key}")))
}

/// What a view is, by its kind and number, and the region it belongs to.
const fn view_kind(view: RightSurface) -> (&'static str, u64, &'static str) {
    match view {
        RightSurface::Files => ("files", 0, "files"),
        RightSurface::File(n) => ("file", n, "pane"),
        RightSurface::Browser(n) => ("browser", n, "pane"),
        RightSurface::Diff(n) => ("diff", n, "changes"),
        RightSurface::Inspector => ("inspector", 0, "inspector"),
        RightSurface::Screen(n) => ("screen", n, "pane"),
        RightSurface::Page(n) => ("page", n, "pane"),
    }
}

/// Whether `pane` belongs to `region`.
const fn in_region(pane: Pane, region: &str) -> bool {
    match pane {
        Pane::View(view) => matches!(
            (view, region.as_bytes()),
            (RightSurface::Files, b"files")
                | (RightSurface::Diff(_), b"changes")
                | (RightSurface::Inspector, b"inspector")
        ),
        Pane::Shell(_) => matches!(region.as_bytes(), b"terminal"),
        Pane::Session => false,
    }
}

impl Shell {
    /// Whether the chat `asker` is on screen, read before its ask moves anything.
    fn asker_on_screen(&self, asker: Option<&str>) -> bool {
        asker.is_some_and(|chat| chat == self.active_chat && matches!(self.route, Route::Chat))
    }

    /// Do `ask` in this window and hand `answered` the outcome, at once or once a terminal is up.
    fn agent_act(
        &mut self,
        ask: Ask,
        answered: Answered,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let keyboard = window.focused(cx);
        let Ask { act, chat, device, who } = ask;
        // The asking agent's chat on screen already shows what it did (its call's row,
        // and the change beside it), so no toast says it again over the pane it just filled.
        let seen = self.asker_on_screen(chat.as_deref());
        let did = match act {
            Act::State => Ok(None),
            Act::Terminal { folder, command } => {
                let device = device.as_deref();
                self.agent_terminal(folder, command, who, device, answered, cx);
                return keep_keyboard(keyboard, window, cx);
            }
            Act::Open(target) => self.agent_open(&target, device.as_deref(), window, cx),
            Act::Region { region, change } => self.agent_region(&region, change, window, cx),
            Act::Chat(key) => self.agent_chat(key.as_deref(), chat.as_deref(), window, cx),
            Act::Split(target) => self.agent_split(target.as_ref(), device.as_deref(), window, cx),
        };
        keep_keyboard(keyboard, window, cx);
        let did = match did {
            Ok(did) => did,
            Err(failure) => return answered(Err(failure), cx),
        };
        if let Some(did) = did.as_ref().filter(|_| !seen) {
            crate::toast::info(cx, format!("{who}: {did}"));
        }
        cx.notify();
        // The window as it is once what the act set in motion has landed: the chat it selected
        // reaches `active_chat` through the state's observer.
        cx.defer_in(window, move |this, window, cx| {
            let window = this.agent_window(window, cx);
            answered(Ok(Done { did, made: None, ran: None, window }), cx);
        });
    }

    /// What the window shows now.
    pub(super) fn agent_window(&self, window: &Window, cx: &App) -> WindowState {
        let state = self.state.read(cx);
        let page = match self.route {
            Route::Chat => "chat".to_owned(),
            Route::Settings(section) => format!("settings/{section:?}").to_ascii_lowercase(),
        };
        let chat = (!self.active_chat.is_empty())
            .then(|| state.chats.iter().find(|row| row.id == self.active_chat))
            .flatten()
            .map(|row| ChatShown {
                id: row.id.clone(),
                title: row.title.clone(),
                machine: state.device_name(&row.device_id).map(str::to_owned),
            });
        let panes = self.panes_now(cx);
        let zoomed = panes.zoomed();
        let regions = REGIONS
            .iter()
            .map(|&name| {
                let (shown, expanded) = if name == "sidebar" {
                    (!self.settings.sidebar_collapsed && self.horizontal_fit().sidebar, false)
                } else {
                    let shown = panes.panes().into_iter().any(|pane| in_region(pane, name));
                    (shown, zoomed.is_some_and(|pane| in_region(pane, name)))
                };
                Region { name: name.to_owned(), shown, expanded }
            })
            .collect();
        let focused = panes.focused();
        let tabs = panes
            .panes()
            .into_iter()
            .filter_map(|pane| {
                let selected = pane == focused;
                match pane {
                    Pane::Session => None,
                    Pane::Shell(n) => {
                        let shell = self.pane_shells.get(&n)?.read(cx);
                        Some(Tab {
                            id: format!("terminal/{n}"),
                            kind: "terminal".to_owned(),
                            title: shell
                                .shown_title(cx)
                                .unwrap_or_else(|| "Shell".into())
                                .to_string(),
                            region: "terminal".to_owned(),
                            selected,
                            terminal: shell.pane_terminal_id().map(str::to_owned),
                            target: None,
                        })
                    }
                    Pane::View(view) => {
                        let (kind, n, region) = view_kind(view);
                        let target = match view {
                            RightSurface::File(id) => self.file_surface_paths.get(&id).cloned(),
                            RightSurface::Browser(id) => self
                                .browsers
                                .get(&id)
                                .and_then(|page| page.read(cx).url().map(str::to_owned)),
                            _ => None,
                        };
                        Some(Tab {
                            id: format!("pane/{kind}/{n}"),
                            kind: kind.to_owned(),
                            title: self.view_identity(view, cx).1.to_string(),
                            region: region.to_owned(),
                            selected,
                            terminal: None,
                            target,
                        })
                    }
                }
            })
            .collect();
        WindowState { page, chat, regions, tabs, typing: self.agent_sees_typing(window, cx) }
    }

    /// Do `open` as an agent's ask does: what it opens beside the session stands there, and the
    /// keyboard, and the pane that has it, stay where they were.
    fn agent_beside(
        &mut self,
        cx: &mut Context<Self>,
        open: impl FnOnce(&mut Self, &mut Context<Self>),
    ) {
        let had = self.panes_now(cx).focused();
        open(self, cx);
        self.panes_mut(cx).focus(had);
        self.pane_keyboard_pending = false;
        cx.notify();
    }

    /// Whether the person is typing: the keyboard in the session's terminal with words typed
    /// there and not yet sent. A terminal merely holding the keyboard is someone watching their
    /// agent, not typing.
    fn agent_sees_typing(&self, window: &Window, cx: &App) -> bool {
        let terminal = self.session_terminal.read(cx);
        terminal.focus_handle().contains_focused(window, cx) && terminal.drafting(cx)
    }

    /// The chat on screen, for an ask that needs one; with `device`, only one on the agent's
    /// machine, whose paths it names.
    fn agent_chat_here(&self, device: Option<&str>, cx: &App) -> Result<(), Failure> {
        if self.active_chat.is_empty() {
            return fail(
                Code::Elsewhere,
                "no chat is on screen: `slopty ui chat` puts yours there first",
            );
        }
        let Some(device) = device else { return Ok(()) };
        let state = self.state.read(cx);
        let Some(row) = state.chats.iter().find(|row| row.id == self.active_chat) else {
            return Ok(());
        };
        if row.device_id == device {
            return Ok(());
        }
        let machine = |id: &str| state.device_name(id).unwrap_or(id).to_owned();
        fail(
            Code::Elsewhere,
            format!(
                "the chat on screen runs on {}, and your paths are {}'s: `slopty ui chat` puts \
                 yours on screen first",
                machine(&row.device_id),
                machine(device),
            ),
        )
    }

    /// `slopty ui open`: a file, a page or the changes in a pane beside the session.
    fn agent_open(
        &mut self,
        target: &Target,
        device: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<Option<String>, Failure> {
        match target {
            Target::File(link) => {
                self.agent_chat_here(device, cx)?;
                let chat = self.active_chat.clone();
                let mut opened = false;
                self.agent_beside(cx, |this, cx| {
                    opened = this.open_workspace_file_link(&chat, link, window, cx);
                });
                if !opened {
                    return fail(Code::Unknown, format!("the window cannot open {link}"));
                }
                Ok(Some(format!("opened {}", shown_file(link))))
            }
            Target::Page(url) => {
                self.agent_chat_here(None, cx)?;
                self.agent_beside(cx, |this, cx| {
                    this.add_browser_surface(Some(url.clone()), window, cx);
                });
                Ok(Some(format!("opened {url}")))
            }
            Target::Diff => {
                self.agent_chat_here(None, cx)?;
                if !self.space_git_detected(cx) {
                    return fail(Code::Unknown, "the chat's folder is not a git repository");
                }
                self.agent_beside(cx, |this, cx| this.show_changes(window, cx));
                Ok(Some("opened the changes".to_owned()))
            }
        }
    }

    /// `slopty ui show|hide|toggle|expand|collapse <region>`.
    fn agent_region(
        &mut self,
        name: &str,
        change: Change,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<Option<String>, Failure> {
        let Some(region) = region_named(name) else {
            return fail(
                Code::Unknown,
                format!("no region is called {name}; the window has {}", REGIONS.join(", ")),
            );
        };
        let state = self.agent_window(window, cx);
        let now = state.regions.iter().find(|r| r.name == region).cloned().unwrap_or_default();
        let show = shows(change, now.shown);
        if show == now.shown {
            return Ok(None);
        }
        let named = match region {
            "sidebar" => "the sidebar",
            "files" => "Files",
            "changes" => "the changes",
            "inspector" => "the inspector",
            _ => "the shells",
        };
        let did = Some(format!("{} {named}", if show { "showed" } else { "hid" }));
        if region == "sidebar" {
            if show && !self.settings.sidebar_collapsed {
                return fail(
                    Code::Failed,
                    "the window is too narrow for the sidebar beside the session",
                );
            }
            self.toggle_sidebar(cx);
            return Ok(did);
        }
        if !show {
            // A view closed opens again as it was; a shell's pane closing ends what runs in it,
            // which only the person decides.
            if region == "terminal" {
                return fail(
                    Code::Failed,
                    "closing a shell ends what runs in it; the person closes their shells",
                );
            }
            let panes: Vec<Pane> = self
                .panes_now(cx)
                .panes()
                .into_iter()
                .filter(|pane| in_region(*pane, region))
                .collect();
            if panes.iter().any(|pane| self.pane_holds_keyboard(*pane, window, cx)) {
                return fail(Code::Busy, format!("the person is working in {named}"));
            }
            for pane in panes {
                self.close_pane(pane, cx);
            }
            return Ok(did);
        }
        self.agent_chat_here(None, cx)?;
        match region {
            "files" => self.agent_beside(cx, |this, cx| {
                this.ensure_explorer(window, cx);
                this.open_view(RightSurface::Files, cx);
            }),
            "changes" => {
                if !self.space_git_detected(cx) {
                    return fail(Code::Unknown, "the chat's folder is not a git repository");
                }
                self.agent_beside(cx, |this, cx| this.show_changes(window, cx));
            }
            "inspector" => {
                self.agent_beside(cx, |this, cx| this.open_view(RightSurface::Inspector, cx));
            }
            _ => {
                if !self.splits_here() {
                    return fail(Code::Failed, "this window is too narrow to split");
                }
                self.agent_beside(cx, |this, cx| this.split_pane(None, cx));
            }
        }
        Ok(did)
    }

    /// `slopty ui chat`: a chat on screen, as a sidebar row's click puts it there, the keyboard
    /// left where it is.
    fn agent_chat(
        &mut self,
        key: Option<&str>,
        own: Option<&str>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Result<Option<String>, Failure> {
        let id = {
            let state = self.state.read(cx);
            let listed = state.chats.iter().filter(|row| !row.archived);
            match (key, own) {
                (Some(key), _) => {
                    chat_named(key, listed.map(|row| (row.id.as_str(), row.title.as_deref())))?
                }
                (None, Some(own)) => own.to_owned(),
                (None, None) => {
                    return fail(Code::Unknown, "say which chat: you run in none the window lists");
                }
            }
        };
        if id == self.active_chat && matches!(self.route, Route::Chat) {
            return Ok(None);
        }
        if matches!(self.route, Route::Settings(_)) {
            return fail(Code::Busy, "the person is in Settings; the window stays there");
        }
        if self.agent_sees_typing(window, cx) {
            return fail(
                Code::Busy,
                "the person is typing a draft in the chat on screen; the window stays on it",
            );
        }
        let title = self
            .state
            .read(cx)
            .chats
            .iter()
            .find(|row| row.id == id)
            .and_then(|row| row.title.clone())
            .unwrap_or_else(|| id.clone());
        self.command_palette = None;
        self.set_route(Route::Chat, cx);
        self.nav.push(NavEntry::Chat(id.clone()));
        self.state.update(cx, |state, cx| state.select_chat(Some(id), cx));
        Ok(Some(format!("put {title} on screen")))
    }

    /// `slopty ui split`: `target` opened beside the session, and the session's panes at even
    /// shares.
    fn agent_split(
        &mut self,
        target: Option<&Target>,
        device: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<Option<String>, Failure> {
        self.agent_chat_here(None, cx)?;
        let opened =
            target.map(|target| self.agent_open(target, device, window, cx)).transpose()?;
        if self.panes_now(cx).len() < 2 {
            return Ok(None);
        }
        self.panes_mut(cx).equalize();
        cx.notify();
        Ok(Some(match opened.flatten() {
            Some(opened) => format!("{opened} beside the session, the panes at even shares"),
            None => "put the session's panes at even shares".to_owned(),
        }))
    }

    /// `slopty ui terminal`: a shell in a pane beside the chat on screen, in `folder`, where there
    /// is most room; `answered` hears once its shell is up and `command` typed.
    fn agent_terminal(
        &mut self,
        folder: Option<String>,
        command: Option<String>,
        who: String,
        device: Option<&str>,
        answered: Answered,
        cx: &mut Context<Self>,
    ) {
        if let Err(failure) = self.agent_chat_here(folder.as_ref().and(device), cx) {
            return answered(Err(failure), cx);
        }
        if !self.splits_here() {
            return answered(fail(Code::Failed, "this window is too narrow to split"), cx);
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return answered(
                fail(Code::Failed, "the window is not connected to its machines yet"),
                cx,
            );
        };
        let chat = self.state.read(cx).panel_session_key();
        let target = self.state.read(cx).terminal_target_device(&chat);
        let state = self.state.clone();
        let panel = cx.new(|cx| TerminalPanel::new(state, cx));
        let key =
            panel.update(cx, |panel, cx| panel.reserve_tab_for_chat(chat.clone(), "Terminal", cx));
        let panes = self.panel_key(cx);
        let id = format!("terminal/{}", self.add_shell_beside(&panes, panel.clone(), cx));
        let mut params = serde_json::json!({ "chatId": chat });
        if let Some(object) = params.as_object_mut() {
            if let Some(folder) = &folder {
                object.insert("cwd".into(), serde_json::Value::String(folder.clone()));
            }
            if let Some(target) = &target {
                object.insert("targetDeviceId".into(), serde_json::Value::String(target.clone()));
            }
        }
        let did = match &folder {
            Some(folder) => format!("opened a terminal in {folder}"),
            None => "opened a terminal".to_owned(),
        };
        cx.spawn(async move |this, cx| {
            let (cols, rows) = TerminalPanel::grid(&panel.downgrade(), cx).await;
            if let Some(object) = params.as_object_mut() {
                object.insert("cols".into(), cols.into());
                object.insert("rows".into(), rows.into());
            }
            let opened =
                engine.client().call_as::<TerminalSession>(methods::OPEN_TERMINAL, params).await;
            let session = match opened {
                Ok(session) => session,
                Err(e) => {
                    let message = format!("the terminal did not open: {e}");
                    return cx.update(|cx| {
                        panel.update(cx, |panel, cx| {
                            panel.fail_reserved_tab(&chat, key, &message, cx);
                        });
                        answered(fail(Code::Failed, message), cx);
                    });
                }
            };
            let terminal = session.id.clone();
            let attached = cx.update(|cx| {
                panel.update(cx, |panel, cx| {
                    panel.attach_reserved_session(&chat, key, session, target, cx)
                })
            });
            if !attached {
                let message = "the terminal tab was closed as it opened";
                return cx.update(|cx| answered(fail(Code::Failed, message), cx));
            }
            // A fresh shell takes its command once its view is up.
            let line = command.as_deref().map(str::trim).filter(|line| !line.is_empty());
            let ran = match line {
                None => None,
                Some(line) => {
                    let since = cx.background_executor().now();
                    loop {
                        if cx.update(|cx| super::terminal_runs::send_line(&terminal, line, cx)) {
                            break Some(!super::terminal_runs::read_first(line));
                        }
                        if cx.background_executor().now().saturating_duration_since(since)
                            > SHELL_WITHIN
                        {
                            break Some(false);
                        }
                        cx.background_executor().timer(ASK_EVERY).await;
                    }
                }
            };
            let did = match (ran, line) {
                (Some(true), Some(line)) => format!("{did} and ran {line}"),
                (Some(false), _) => format!("{did}; its command waits for you to run it"),
                _ => did,
            };
            cx.update(|cx| {
                crate::toast::info(cx, format!("{who}: {did}"));
                let window = window_state(&this, cx);
                let made = window.tabs.iter().find(|tab| tab.id == id).cloned().map(|mut tab| {
                    tab.terminal = Some(terminal);
                    tab
                });
                answered(Ok(Done { did: Some(did), made, ran, window }), cx);
            });
        })
        .detach();
    }
}

/// What the served window shows, from a task outside the shell's own update.
fn window_state(shell: &WeakEntity<Shell>, cx: &mut App) -> WindowState {
    let window = cx.try_global::<Served>().map(|served| served.window);
    shell
        .upgrade()
        .zip(window)
        .and_then(|(shell, window)| {
            window.update(cx, |_, window, cx| shell.read(cx).agent_window(window, cx)).ok()
        })
        .unwrap_or_default()
}

/// A file link as the mark names it: its path, without the scheme and the line.
fn shown_file(link: &str) -> String {
    let path = link.strip_prefix("file://").unwrap_or(link);
    let path = path.split('#').next().unwrap_or(path);
    path.replace("%20", " ").replace("%23", "#").replace("%3F", "?").replace("%25", "%")
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use gpui::{Entity, TestAppContext, VisualTestContext, px};

    use super::super::*;
    use super::*;

    type Heard = Rc<RefCell<Vec<Result<Done, Failure>>>>;

    /// A shell on two chats of this machine, `a` on screen, its window serving agents' asks.
    fn setup(
        cx: &mut TestAppContext,
    ) -> (tempfile::TempDir, Entity<Shell>, &mut VisualTestContext) {
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            crate::app_menus::init(cx);
            settings::init(UiSettings::default(), dir.path(), cx);
        });
        let (shell, cx) = cx.add_window_view(|_, cx| {
            let state = cx.new(|_| {
                let mut state = AppState::new();
                state.no_project = true;
                state.local_device_id = Some("studio".into());
                state.selected_chat = Some("a1".into());
                state.chats = [("a1", "Tidy the greeting"), ("b2", "Fix the build")]
                    .into_iter()
                    .map(|(id, title)| {
                        serde_json::from_value(serde_json::json!({
                            "id": id, "title": title, "deviceId": "studio", "archived": false,
                            "createdAt": Utc::now(),
                        }))
                        .unwrap()
                    })
                    .collect();
                state
            });
            let boot = EngineBootConfig {
                data_dir: dir.path().into(),
                default_harness: slopty_view_proto::HarnessId::MOCK,
            };
            let mut shell = Shell::new(state, boot, cx);
            shell.debug_gate = Some(GatePhase::Ready);
            shell.splash = SplashPhase::Gone;
            shell.active_chat = "a1".into();
            shell
        });
        cx.simulate_resize(gpui::size(px(1600.0), px(900.0)));
        cx.update(|window, cx| shell.update(cx, |_, cx| serve(window, cx)));
        cx.run_until_parked();
        (dir, shell, cx)
    }

    fn ask(act: Act) -> Ask {
        Ask { act, chat: Some("a1".into()), device: Some("studio".into()), who: "Agent".into() }
    }

    /// Ask `act` of the window as the workspace does, from outside any update: its answer.
    fn asked(cx: &mut VisualTestContext, ask: Ask) -> Result<Done, Failure> {
        let heard: Heard = Rc::default();
        let kept = Rc::clone(&heard);
        cx.cx
            .update(|cx| act(ask, Box::new(move |outcome, _| kept.borrow_mut().push(outcome)), cx));
        cx.run_until_parked();
        let mut heard = heard.borrow_mut();
        assert_eq!(heard.len(), 1, "answered once: {heard:?}");
        heard.remove(0)
    }

    fn region(done: &Done, name: &str) -> Region {
        done.window.regions.iter().find(|r| r.name == name).cloned().unwrap_or_default()
    }

    fn code(outcome: &Result<Done, Failure>) -> Option<Code> {
        outcome.as_ref().err().map(|failure| failure.code)
    }

    /// With no shell window up, an ask hears that there is none, once.
    #[gpui::test]
    fn with_no_window_an_ask_hears_there_is_none(cx: &mut TestAppContext) {
        let heard: Heard = Rc::default();
        let kept = Rc::clone(&heard);
        cx.update(|cx| {
            act(ask(Act::State), Box::new(move |outcome, _| kept.borrow_mut().push(outcome)), cx);
        });
        let heard = heard.borrow();
        assert_eq!(heard.len(), 1);
        assert_eq!(code(&heard[0]), Some(Code::NoWindow), "{heard:?}");
    }

    /// The state names the page, the chat on screen with its machine, every region and whether
    /// the person is typing, and changes nothing.
    #[gpui::test]
    fn the_state_says_what_the_window_shows(cx: &mut TestAppContext) {
        let (_dir, _shell, cx) = setup(cx);
        let done = asked(cx, ask(Act::State)).unwrap();
        assert_eq!(done.did, None);
        assert_eq!(done.window.page, "chat");
        let chat = done.window.chat.clone().unwrap();
        assert_eq!((chat.id.as_str(), chat.title.as_deref()), ("a1", Some("Tidy the greeting")));
        let names: Vec<&str> = done.window.regions.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, REGIONS);
        assert!(!done.window.typing);
    }

    /// What an agent does in the window is toasted only when its chat is not the one
    /// on screen, which already shows the call and the change beside it.
    #[gpui::test]
    fn an_agent_on_screen_is_not_toasted_over_its_own_change(cx: &mut TestAppContext) {
        let (_dir, _shell, cx) = setup(cx);
        let hide = |chat: &str| Ask {
            chat: Some(chat.into()),
            ..ask(Act::Region { region: "sidebar".into(), change: Change::Toggle })
        };
        assert!(asked(cx, hide("a1")).unwrap().did.is_some());
        assert_eq!(
            cx.update(|_, cx| crate::toast::said(cx)),
            Vec::<SharedString>::new(),
            "a1 is on screen"
        );
        assert!(asked(cx, hide("b2")).unwrap().did.is_some());
        let said = cx.update(|_, cx| crate::toast::said(cx));
        assert_eq!(said.len(), 1, "b2 is not: {said:?}");
        assert!(said[0].starts_with("Agent: "), "{said:?}");
    }

    /// Each region shows, hides, toggles, expands and collapses by its name or the window's own
    /// word for it: a view stands beside the session while the keyboard stays where it was, a
    /// change already made changes nothing, a shell is the person's to close, and a name the
    /// window does not know is answered with the ones it does.
    #[gpui::test]
    fn regions_show_hide_expand_and_collapse(cx: &mut TestAppContext) {
        let (_dir, shell, cx) = setup(cx);
        let change = |region: &str, change| Act::Region { region: region.into(), change };
        let done = asked(cx, ask(change("sidebar", Change::Hide))).unwrap();
        assert_eq!(done.did.as_deref(), Some("hid the sidebar"));
        assert!(!region(&done, "sidebar").shown);
        assert_eq!(asked(cx, ask(change("sidebar", Change::Collapse))).unwrap().did, None);
        let done = asked(cx, ask(change("Side bar", Change::Toggle))).unwrap();
        assert!(region(&done, "sidebar").shown, "{done:?}");

        let done = asked(cx, ask(change("trace", Change::Show))).unwrap();
        assert_eq!(done.did.as_deref(), Some("showed the inspector"));
        assert!(region(&done, "inspector").shown, "{done:?}");
        let tab = done.window.tabs.iter().find(|tab| tab.kind == "inspector").cloned().unwrap();
        assert_eq!(
            (tab.id.as_str(), tab.region.as_str(), tab.selected),
            ("pane/inspector/0", "inspector", false)
        );
        assert_eq!(
            shell.read_with(cx, |shell, cx| shell.panes_now(cx).focused()),
            Pane::Session,
            "the keyboard stays in the session"
        );
        assert_eq!(
            asked(cx, ask(change("inspector", Change::Expand))).unwrap().did,
            None,
            "shown already"
        );
        let done = asked(cx, ask(change("inspector", Change::Collapse))).unwrap();
        assert!(!region(&done, "inspector").shown);
        assert!(done.window.tabs.is_empty(), "{done:?}");

        // A shell splits beside the session; hiding it would end what runs in it.
        let done = asked(cx, ask(change("shells", Change::Show))).unwrap();
        assert!(region(&done, "terminal").shown, "{done:?}");
        let kept = asked(cx, ask(change("terminal", Change::Hide)));
        assert_eq!(code(&kept), Some(Code::Failed), "only the person closes a shell");

        let unknown = asked(cx, ask(change("dock", Change::Show)));
        assert_eq!(code(&unknown), Some(Code::Unknown));
        let message = unknown.unwrap_err().message;
        assert!(message.contains("sidebar, files, changes, inspector, terminal"), "{message}");
    }

    /// A chat goes on screen by its id, a prefix of it or its title, or the agent's own; a name
    /// for several or none is answered with what it names.
    #[gpui::test]
    fn a_chat_goes_on_screen_by_id_prefix_or_title(cx: &mut TestAppContext) {
        let (_dir, shell, cx) = setup(cx);
        let chat = |key: Option<&str>| Act::Chat(key.map(str::to_owned));
        let done = asked(cx, ask(chat(Some("fix the build")))).unwrap();
        assert_eq!(done.did.as_deref(), Some("put Fix the build on screen"));
        assert_eq!(shell.read_with(cx, |shell, _| shell.active_chat.clone()), "b2");
        assert_eq!(done.window.chat.map(|c| c.id).as_deref(), Some("b2"));
        let done = asked(cx, ask(chat(None))).unwrap();
        assert_eq!(done.window.chat.map(|c| c.id).as_deref(), Some("a1"), "the agent's own");
        assert_eq!(asked(cx, ask(chat(Some("a")))).unwrap().did, None, "already on screen");
        assert_eq!(code(&asked(cx, ask(chat(Some("nope"))))), Some(Code::Unknown));
        let several = chat_named("", [("a1", Some("x")), ("a2", None)].into_iter());
        assert!(several.unwrap_err().message.contains("x (a1), untitled (a2)"));
        let mut lost = ask(chat(None));
        lost.chat = None;
        assert_eq!(code(&asked(cx, lost)), Some(Code::Unknown), "no chat of its own to name");
    }

    /// A path names a file on the agent's machine, so it opens only beside a chat there; a page
    /// and the diff need a chat on screen; a terminal needs the window's link to its machines.
    #[gpui::test]
    fn what_opens_needs_its_chat_and_its_machine(cx: &mut TestAppContext) {
        let (_dir, shell, cx) = setup(cx);
        let file = Act::Open(Target::File("file:///w/app/src/main.rs#L3".into()));
        let mut elsewhere = ask(file);
        elsewhere.device = Some("laptop".into());
        let refused = asked(cx, elsewhere);
        assert_eq!(code(&refused), Some(Code::Elsewhere), "{refused:?}");
        let terminal = |folder: Option<&str>| Act::Terminal {
            folder: folder.map(str::to_owned),
            command: Some("cargo test".into()),
        };
        let mut far = ask(terminal(Some("/w/app")));
        far.device = Some("laptop".into());
        assert_eq!(code(&asked(cx, far)), Some(Code::Elsewhere));
        assert_eq!(code(&asked(cx, ask(terminal(None)))), Some(Code::Failed), "no link");
        shell.update(cx, |shell, _| shell.active_chat.clear());
        let page = Act::Open(Target::Page("https://example.com/".into()));
        assert_eq!(code(&asked(cx, ask(page))), Some(Code::Elsewhere), "no chat on screen");
    }

    /// Split puts the session's panes at even shares; with nothing beside the session there is
    /// nothing to share.
    #[gpui::test]
    fn split_puts_the_panes_at_even_shares(cx: &mut TestAppContext) {
        let (_dir, shell, cx) = setup(cx);
        assert_eq!(asked(cx, ask(Act::Split(None))).unwrap().did, None, "the session alone");
        let inspector = Act::Region { region: "inspector".into(), change: Change::Show };
        asked(cx, ask(inspector)).unwrap();
        let done = asked(cx, ask(Act::Split(None))).unwrap();
        assert_eq!(done.did.as_deref(), Some("put the session's panes at even shares"));
        let panes = shell.read_with(cx, Shell::panes_now);
        assert_eq!(panes.focused(), Pane::Session, "the keyboard stays in the session");
        assert_eq!(panes, {
            let mut even = panes.clone();
            even.equalize();
            even
        });
    }

    /// A file link is named by its path, unescaped.
    #[test]
    fn a_file_is_marked_by_its_path() {
        assert_eq!(shown_file("file:///w/a%20b/c%23.rs#L12"), "/w/a b/c#.rs");
        assert_eq!(shown_file("/w/x.rs"), "/w/x.rs");
    }
}
