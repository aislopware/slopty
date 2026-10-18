//! slopty: an agent drives the person's window (`slopty ui`, the `ui_*` tools): it opens a
//! terminal tab, a file, a page or the diff in the side pane, shows, hides, expands or collapses
//! a region, puts a chat on screen, lays the chat and the side pane side by side, and reads what
//! the window shows. Slopty's workspace hands each ask here through [`act`] once the shell's
//! window is up ([`serve`]), and sends what [`act`] answers back to the agent.
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

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use gpui::{AnyWindowHandle, App, Context, FocusHandle, Focusable as _, Global, WeakEntity, Window};
use slopty_view_proto::TerminalSession;
use slopty_view_rpc::methods;

use super::{NavEntry, RightSurface, Route, Shell, WidthTween};
use crate::motion::{self, RESIZE};

/// How long a fresh terminal's shell is waited for before its command is given up on.
const SHELL_WITHIN: Duration = Duration::from_secs(10);

/// How often a terminal still coming up is asked again to take its command.
const ASK_EVERY: Duration = Duration::from_millis(50);

/// The regions an agent names, as [`WindowState::regions`] lists them.
pub const REGIONS: [&str; 4] = ["sidebar", "pane", "files", "terminal"];

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
    /// A terminal tab for the chat on screen.
    Terminal {
        /// The folder it starts in, absolute on the agent's machine; the chat's own when `None`.
        folder: Option<String>,
        /// A command line to type once its shell is up.
        command: Option<String>,
        /// `terminal` (the drawer, the default) or `pane`.
        region: Option<String>,
    },
    /// Something in the side pane.
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
    /// The chat and the side pane side by side at even widths, `target` shown there first.
    Split(Option<Target>),
}

/// What the side pane shows for an agent.
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
    /// As large as it goes: the side pane over the chat; any other region shown.
    Expand,
    /// Back to its share: the side pane beside the chat; any other region hidden.
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
    /// The chat's tabs: the side pane's, then the terminal drawer's.
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
    /// Taking the chat's room.
    pub expanded: bool,
}

/// One tab of the window.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tab {
    /// `pane/<kind>/<n>` or `terminal/<n>`.
    pub id: String,
    /// What it shows.
    pub kind: String,
    /// Its title.
    pub title: String,
    /// Its region.
    pub region: String,
    /// The one its region shows.
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
        "pane" | "side pane" | "right" | "right pane" | "surfaces" => "pane",
        "files" | "files panel" | "explorer" => "files",
        "terminal" | "drawer" | "terminal drawer" | "bottom" => "terminal",
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
                Some(fail(Code::Unknown, format!("{key} names several chats: {}", named.join(", "))))
            }
        }
    };
    let prefixed = chats.clone().filter(|(id, _)| id.starts_with(key)).collect();
    if let Some(found) = by(prefixed) {
        return found;
    }
    let titled =
        chats.filter(|(_, title)| title.is_some_and(|t| t.trim().eq_ignore_ascii_case(key))).collect();
    by(titled).unwrap_or_else(|| fail(Code::Unknown, format!("no chat is called {key}")))
}

/// What a pane tab is, by its surface: its kind and number.
const fn surface_kind(surface: RightSurface) -> Option<(&'static str, u64)> {
    Some(match surface {
        RightSurface::Picker => return None,
        RightSurface::File(n) => ("file", n),
        RightSurface::Browser(n) => ("browser", n),
        RightSurface::Diff(n) => ("diff", n),
        RightSurface::Terminal(n) => ("terminal", n),
        RightSurface::SideChat(n) => ("chat", n),
        RightSurface::Screen(n) => ("screen", n),
        RightSurface::ModPane(n) => ("mod", n),
        RightSurface::Page(n) => ("page", n),
    })
}

impl Shell {
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
        let did = match act {
            Act::State => Ok(None),
            Act::Terminal { folder, command, region } => {
                let device = device.as_deref();
                self.agent_terminal(folder, command, region, who, device, answered, cx);
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
        if let Some(did) = &did {
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
        let fit = self.horizontal_fit();
        let region = |name: &str, shown: bool, expanded: bool| Region {
            name: name.to_owned(),
            shown,
            expanded,
        };
        let regions = vec![
            region("sidebar", !self.settings.sidebar_collapsed && fit.sidebar, false),
            region("pane", self.right_pane_shown(cx), self.right_pane_expanded),
            region("files", self.files_panel_open(cx) && fit.files, false),
            region("terminal", self.terminal_open(cx), false),
        ];
        let key = self.panel_key(cx);
        let active = self.resolved_right_active(cx);
        let mut tabs: Vec<Tab> = self
            .right_surface_rows(cx)
            .into_iter()
            .filter_map(|(surface, title, _, target)| {
                let (kind, n) = surface_kind(surface)?;
                let terminal = match surface {
                    RightSurface::Terminal(tab) => self
                        .right_terminal
                        .as_ref()
                        .and_then(|panel| panel.read(cx).terminal_id(&key, tab).map(str::to_owned)),
                    _ => None,
                };
                Some(Tab {
                    id: format!("pane/{kind}/{n}"),
                    kind: kind.to_owned(),
                    title: title.to_string(),
                    region: "pane".to_owned(),
                    selected: surface == active,
                    terminal,
                    target: target.map(|t| t.to_string()),
                })
            })
            .collect();
        if let Some(panel) = &self.terminal {
            let panel = panel.read(cx);
            let drawer = state.panel_session_key();
            tabs.extend(panel.tab_summaries(cx).into_iter().map(|(n, title, _)| Tab {
                id: format!("terminal/{n}"),
                kind: "terminal".to_owned(),
                title: title.to_string(),
                region: "terminal".to_owned(),
                selected: false,
                terminal: panel.terminal_id(&drawer, n).map(str::to_owned),
                target: None,
            }));
        }
        WindowState { page, chat, regions, tabs, typing: self.agent_sees_typing(window, cx) }
    }

    /// Whether the person is typing a draft: the keyboard in a composer that holds one.
    fn agent_sees_typing(&self, window: &Window, cx: &App) -> bool {
        std::iter::once(&self.composer)
            .chain(self.side_chats.values().map(|side| &side.composer))
            .any(|composer| {
                composer.focus_handle(cx).contains_focused(window, cx)
                    && composer.read(cx).has_draft(cx)
            })
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

    /// `slopty ui open`: a file, a page or the diff in the side pane.
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
                if !self.open_workspace_file_link(&chat, link, window, cx) {
                    return fail(Code::Unknown, format!("the window cannot open {link}"));
                }
                Ok(Some(format!("opened {}", shown_file(link))))
            }
            Target::Page(url) => {
                self.agent_chat_here(None, cx)?;
                self.set_surfaces_open(true, cx);
                self.add_browser_surface(Some(url.clone()), window, cx);
                Ok(Some(format!("opened {url}")))
            }
            Target::Diff => {
                self.agent_chat_here(None, cx)?;
                if !self.space_git_detected(cx) {
                    return fail(Code::Unknown, "the chat's folder is not a git repository");
                }
                self.set_surfaces_open(true, cx);
                self.add_diff_surface(window, cx);
                Ok(Some("opened the diff".to_owned()))
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
        let busy = |what: &str| fail(Code::Busy, format!("the person is typing in the {what}"));
        match region {
            "sidebar" => {
                if show == now.shown {
                    return Ok(None);
                }
                if show && !self.settings.sidebar_collapsed {
                    self.promote_hidden_panel(super::panel_layout::AuxiliaryPanel::Sidebar, cx);
                } else if !show && !now.shown {
                    self.settings.sidebar_collapsed = true;
                } else {
                    self.toggle_sidebar(cx);
                }
                Ok(Some(format!("{} the sidebar", if show { "showed" } else { "hid" })))
            }
            "pane" => {
                // Collapsing the pane puts it back beside the chat; it stays as shown as it was.
                let show = if matches!(change, Change::Collapse) { now.shown } else { show };
                if show {
                    self.agent_chat_here(None, cx)?;
                }
                let expand = match change {
                    Change::Expand => Some(true),
                    Change::Collapse if now.shown => Some(false),
                    _ => None,
                };
                if !show && self.navigation_focus.in_right(window, cx) {
                    return busy("side pane");
                }
                if expand == Some(true) && !now.expanded && state.typing {
                    return fail(
                        Code::Busy,
                        "the person is typing a draft, which the expanded side pane would cover",
                    );
                }
                let mut did = None;
                if show != now.shown {
                    self.set_surfaces_open(show, cx);
                    did = Some(if show { "showed the side pane" } else { "hid the side pane" });
                }
                if let Some(expand) = expand
                    && self.right_pane_expanded != expand
                    && self.right_pane_shown(cx)
                {
                    self.toggle_right_pane_expand(cx);
                    did = Some(if expand { "expanded the side pane" } else { "put the side pane beside the chat" });
                }
                Ok(did.map(str::to_owned))
            }
            "files" => {
                if show == now.shown {
                    return Ok(None);
                }
                if show {
                    self.agent_chat_here(None, cx)?;
                    self.add_files_surface(window, cx);
                } else {
                    self.close_files_panel(cx);
                }
                Ok(Some(format!("{} the files panel", if show { "showed" } else { "hid" })))
            }
            _ => {
                if show == now.shown {
                    return Ok(None);
                }
                let typing_there = self
                    .terminal
                    .as_ref()
                    .is_some_and(|panel| panel.read(cx).focus_handle().contains_focused(window, cx));
                if !show && typing_there {
                    return busy("terminal");
                }
                self.agent_drawer(show, cx);
                Ok(Some(format!("{} the terminal", if show { "showed" } else { "hid" })))
            }
        }
    }

    /// Open or close the terminal drawer as Cmd+J does, the keyboard left where it is.
    fn agent_drawer(&mut self, open: bool, cx: &mut Context<Self>) {
        if self.terminal_open(cx) == open {
            return;
        }
        let from = self.terminal_geometry.get().height;
        let key = self.panel_key(cx);
        self.panels.toggle_terminal(&key);
        self.terminal_tween = Some(WidthTween::new(from, self.terminal_target(cx), self.now()));
        let panel = self.terminal_panel(cx);
        panel.update(cx, |panel, cx| panel.set_open(open, cx));
        self.terminal_tween_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(RESIZE.total().mul_f32(motion::speed_scale()) + Duration::from_millis(30))
                .await;
            this.update(cx, |shell, cx| {
                shell.terminal_tween = None;
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
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
                    return fail(
                        Code::Unknown,
                        "say which chat: you run in none the window lists",
                    );
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

    /// `slopty ui split`: `target` in the side pane, and the pane beside the chat at an even
    /// share of the width.
    fn agent_split(
        &mut self,
        target: Option<&Target>,
        device: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<Option<String>, Failure> {
        self.agent_chat_here(None, cx)?;
        let opened = target.map(|target| self.agent_open(target, device, window, cx)).transpose()?;
        self.set_surfaces_open(true, cx);
        if self.right_pane_expanded && self.right_pane_shown(cx) {
            self.toggle_right_pane_expand(cx);
        }
        let before = self.painted_panels(cx);
        self.resplit_panels(before, None, cx);
        Ok(Some(match opened.flatten() {
            Some(opened) => format!("{opened} beside the chat"),
            None => "put the side pane beside the chat".to_owned(),
        }))
    }

    /// `slopty ui terminal`: a terminal tab for the chat on screen, in `folder`, in the drawer or
    /// the side pane; `answered` hears once its shell is up and `command` typed.
    #[expect(clippy::too_many_arguments, reason = "an ask's parts, each its own")]
    fn agent_terminal(
        &mut self,
        folder: Option<String>,
        command: Option<String>,
        region: Option<String>,
        who: String,
        device: Option<&str>,
        answered: Answered,
        cx: &mut Context<Self>,
    ) {
        let in_pane = match region.as_deref().map(region_named) {
            None | Some(Some("terminal")) => false,
            Some(Some("pane")) => true,
            Some(_) => {
                let message = format!(
                    "a terminal opens in the terminal drawer or the side pane, not {}",
                    region.unwrap_or_default()
                );
                return answered(fail(Code::Unknown, message), cx);
            }
        };
        if let Err(failure) = self.agent_chat_here(folder.as_ref().and(device), cx) {
            return answered(Err(failure), cx);
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return answered(fail(Code::Failed, "the window is not connected to its machines yet"), cx);
        };
        let chat = self.state.read(cx).panel_session_key();
        let target = self.state.read(cx).terminal_target_device(&chat);
        let panel = if in_pane { self.right_terminal_panel(cx) } else { self.terminal_panel(cx) };
        let key = panel
            .update(cx, |panel, cx| panel.reserve_tab_for_chat(chat.clone(), "Terminal", cx));
        if in_pane {
            panel.update(cx, |panel, cx| panel.set_open(true, cx));
            let pane = self.panel_key(cx);
            self.right_tabs.entry(pane).or_default().push(RightSurface::Terminal(key));
            self.set_surfaces_open(true, cx);
            self.set_right_active(RightSurface::Terminal(key), cx);
        } else {
            self.agent_drawer(true, cx);
            panel.update(cx, |panel, cx| panel.select_tab_by_key(key, cx));
        }
        cx.notify();
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
        let id = format!("{}/{key}", if in_pane { "pane/terminal" } else { "terminal" });
        cx.spawn(async move |this, cx| {
            let (cols, rows) =
                crate::terminal::panel::TerminalPanel::grid(&panel.downgrade(), cx).await;
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
                        if cx.background_executor().now().saturating_duration_since(since) > SHELL_WITHIN {
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
    fn setup(cx: &mut TestAppContext) -> (tempfile::TempDir, Entity<Shell>, &mut VisualTestContext) {
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            crate::app_menus::init(cx);
            let mut config = UiSettings::default();
            config.session_info_hidden = true;
            settings::init(config, dir.path(), cx);
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
        cx.cx.update(|cx| act(ask, Box::new(move |outcome, _| kept.borrow_mut().push(outcome)), cx));
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

    /// Each region shows, hides, toggles, expands and collapses by its name or the window's own
    /// word for it, a change already made changes nothing, and a name the window does not know
    /// is answered with the ones it does.
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

        let done = asked(cx, ask(change("side-pane", Change::Show))).unwrap();
        assert!(region(&done, "pane").shown, "{done:?}");
        let done = asked(cx, ask(change("pane", Change::Expand))).unwrap();
        assert!(region(&done, "pane").expanded);
        assert_eq!(done.did.as_deref(), Some("expanded the side pane"));
        let done = asked(cx, ask(change("pane", Change::Collapse))).unwrap();
        assert!(region(&done, "pane").shown && !region(&done, "pane").expanded, "{done:?}");
        let done = asked(cx, ask(change("pane", Change::Hide))).unwrap();
        assert!(!region(&done, "pane").shown);

        let done = asked(cx, ask(change("terminal", Change::Show))).unwrap();
        assert!(region(&done, "terminal").shown);
        assert!(shell.read_with(cx, |shell, cx| shell.terminal_open(cx)));
        let done = asked(cx, ask(change("drawer", Change::Hide))).unwrap();
        assert!(!region(&done, "terminal").shown);

        let unknown = asked(cx, ask(change("dock", Change::Show)));
        assert_eq!(code(&unknown), Some(Code::Unknown));
        let message = unknown.unwrap_err().message;
        assert!(message.contains("sidebar, pane, files, terminal"), "{message}");
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

    /// While the person types a draft, an ask that would move the chat or cover the draft is
    /// refused, the window says they are typing, and an ask that is done leaves the keyboard in
    /// the draft.
    #[gpui::test]
    fn a_draft_being_typed_keeps_the_keyboard_and_the_chat(cx: &mut TestAppContext) {
        let (_dir, shell, cx) = setup(cx);
        cx.update(|window, cx| {
            shell.update(cx, |shell, cx| {
                shell.composer.update(cx, |composer, cx| {
                    composer.input.update(cx, |input, cx| input.set_text("half a thought", cx));
                });
                window.focus(&shell.composer.focus_handle(cx), cx);
            });
        });
        cx.run_until_parked();
        let moved = asked(cx, ask(Act::Chat(Some("b2".into()))));
        assert_eq!(code(&moved), Some(Code::Busy), "{moved:?}");
        assert_eq!(shell.read_with(cx, |shell, _| shell.active_chat.clone()), "a1");
        let covered = Act::Region { region: "pane".into(), change: Change::Expand };
        assert_eq!(code(&asked(cx, ask(covered))), Some(Code::Busy));
        let done = asked(cx, ask(Act::Region { region: "pane".into(), change: Change::Show }));
        assert!(done.as_ref().is_ok_and(|done| done.window.typing), "{done:?}");
        let focused = cx.update(|window, cx| {
            shell.read(cx).composer.focus_handle(cx).contains_focused(window, cx)
        });
        assert!(focused, "the keyboard stays in the draft");
    }

    /// Once an agent has put another chat on screen, the person's click in the composer and the
    /// keys after it land in that chat's draft, however long after the ask they come.
    #[gpui::test]
    fn keys_after_a_chat_put_on_screen_land_in_its_draft(cx: &mut TestAppContext) {
        let (_dir, shell, cx) = setup(cx);
        shell.update(cx, |shell, cx| shell.open_chat("a1".into(), cx));
        cx.run_until_parked();
        // What the self-test's `dump` does between steps: the frame judged against two drawn from
        // scratch (`slopty_kit::retained::stale`).
        let dump = |cx: &mut VisualTestContext| {
            cx.run_until_parked();
            cx.update(|window, cx| {
                for _ in 0..2 {
                    window.refresh();
                    window.draw(cx).clear(cx);
                }
            });
        };
        dump(cx);
        let done = asked(cx, ask(Act::Chat(Some("b2".into())))).unwrap();
        assert_eq!(done.window.chat.map(|c| c.id).as_deref(), Some("b2"));
        dump(cx);
        dump(cx);
        let input = shell.read_with(cx, |shell, cx| shell.composer.read(cx).input.clone());
        let kept = cx.update(|window, cx| input.read(cx).focus_handle(cx).is_focused(window));
        assert!(kept, "the keyboard stayed in the composer");
        // The click and the keys come together, as the self-test's socket brings them, with no
        // frame drawn between.
        let at = input.read_with(cx, |input, _| input.drawn_bounds()).expect("the composer drawn").center();
        cx.update(|window, cx| {
            use gpui::{Modifiers, MouseButton, MouseDownEvent, MouseUpEvent, PlatformInput};
            let down = MouseDownEvent {
                button: MouseButton::Left,
                position: at,
                modifiers: Modifiers::default(),
                click_count: 1,
                first_mouse: false,
            };
            let up = MouseUpEvent { button: MouseButton::Left, position: at, modifiers: Modifiers::default(), click_count: 1 };
            let _down = window.dispatch_event(PlatformInput::MouseDown(down), cx);
            let _up = window.dispatch_event(PlatformInput::MouseUp(up), cx);
            for ch in "half a thought".chars() {
                let keystroke = gpui::Keystroke {
                    modifiers: Modifiers::default(),
                    key: ch.to_string(),
                    key_char: Some(ch.to_string()),
                };
                window.dispatch_keystroke(keystroke, cx);
            }
        });
        cx.run_until_parked();
        cx.simulate_input(", finished");
        assert_eq!(input.read_with(cx, |input, _| input.text().to_owned()), "half a thought, finished");
        let focused = cx.update(|window, cx| input.read(cx).focus_handle(cx).is_focused(window));
        assert!(focused, "the composer holds the keyboard the person gave it");
        // A screen reader, and the self-test reading the window, find the words in the field.
        let read = cx.update(|window, cx| {
            window.set_a11y_active(true);
            window.refresh();
            window.draw(cx).clear(cx);
            window.a11y_tree().is_some_and(|update| {
                update.nodes.iter().any(|(_, node)| {
                    node.role() == gpui::Role::MultilineTextInput && node.value() == Some("half a thought, finished")
                })
            })
        });
        assert!(read, "the composer's words are its value in the accessibility tree");
    }

    /// A path names a file on the agent's machine, so it opens only beside a chat there; a page
    /// and the diff need a chat on screen; a terminal needs the window's link to its machines
    /// and a place it opens in.
    #[gpui::test]
    fn what_opens_needs_its_chat_and_its_machine(cx: &mut TestAppContext) {
        let (_dir, shell, cx) = setup(cx);
        let file = Act::Open(Target::File("file:///w/app/src/main.rs#L3".into()));
        let mut elsewhere = ask(file);
        elsewhere.device = Some("laptop".into());
        let refused = asked(cx, elsewhere);
        assert_eq!(code(&refused), Some(Code::Elsewhere), "{refused:?}");
        let terminal = |folder: Option<&str>, region: Option<&str>| Act::Terminal {
            folder: folder.map(str::to_owned),
            command: Some("cargo test".into()),
            region: region.map(str::to_owned),
        };
        let mut far = ask(terminal(Some("/w/app"), None));
        far.device = Some("laptop".into());
        assert_eq!(code(&asked(cx, far)), Some(Code::Elsewhere));
        assert_eq!(code(&asked(cx, ask(terminal(None, Some("sidebar"))))), Some(Code::Unknown));
        assert_eq!(code(&asked(cx, ask(terminal(None, None)))), Some(Code::Failed), "no link");
        shell.update(cx, |shell, _| shell.active_chat.clear());
        let page = Act::Open(Target::Page("https://example.com/".into()));
        assert_eq!(code(&asked(cx, ask(page))), Some(Code::Elsewhere), "no chat on screen");
    }

    /// Split lays the side pane beside the chat at an even share: shown, and out of full width.
    #[gpui::test]
    fn split_puts_the_pane_beside_the_chat(cx: &mut TestAppContext) {
        let (_dir, _shell, cx) = setup(cx);
        let expand = Act::Region { region: "pane".into(), change: Change::Expand };
        assert!(region(&asked(cx, ask(expand)).unwrap(), "pane").expanded);
        let done = asked(cx, ask(Act::Split(None))).unwrap();
        let pane = region(&done, "pane");
        assert!(pane.shown && !pane.expanded, "{done:?}");
        assert_eq!(done.did.as_deref(), Some("put the side pane beside the chat"));
    }

    /// A file link is named by its path, unescaped.
    #[test]
    fn a_file_is_marked_by_its_path() {
        assert_eq!(shown_file("file:///w/a%20b/c%23.rs#L12"), "/w/a b/c#.rs");
        assert_eq!(shown_file("/w/x.rs"), "/w/x.rs");
    }
}
