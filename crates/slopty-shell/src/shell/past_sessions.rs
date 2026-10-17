//! slopty: Claude Code's past sessions, seen and taken up again (`docs/decisions/agents.md`,
//! "Past Claude Code sessions are seen and continued in the GUI").
//!
//! - **History.** Each project group of the sidebar ends with a quiet "History" disclosure, shut
//!   at first; a sidebar organised by status has one History for every project (the filtered
//!   one's alone while the sidebar is filtered), above Archived, each row naming its project. Opened, it asks the project's machines for the Claude Code sessions kept in its
//!   folders (the TUI's and `claude -p`'s alike) that are no chat here yet, newest first: each
//!   row titled by its first prompt, with how long ago and its branch where the machine says.
//!   It lists a page and pages on with "Show more"; nothing is asked while it is shut, and the
//!   rows already listed stand while it asks again.
//! - **Resume a session…** (the palette, and its shortcut) searches the person's past prompts
//!   on the chat's machine, in its project or in every folder (⇥), and shows the prompt that
//!   matched with the words lit.
//! - **Opening** a session the machine keeps a chat of opens that chat; any other is taken up by
//!   Claude Code's own `--resume` in its folder, and the chat opens with its whole conversation
//!   once its machine has started it. Its machine's words say why one does not open.
//!
//! The view model answers both calls (`slopty_ui`'s `view_model::past`) from the session's own
//! machine, so a remote project reads as a local one.

use std::collections::HashMap;
use std::time::Duration;

use gpui::Focusable as _;
use gpui::prelude::*;
use gpui::{
    AnyElement, Context, Entity, FocusHandle, Pixels, SharedString, Subscription, Task, Window,
    div, px,
};
use serde::Deserialize;

use super::Shell;
use super::command_palette::{
    command_key_hint, palette_card, palette_empty, palette_footer, palette_header,
    palette_overlay, palette_results_fade, palette_results_height,
};
use crate::composer::{ComposerInput, ComposerInputEvent};
use crate::icons::{self, icon};
use crate::popover;
use crate::settings::{ShortcutId, SidebarOrganization};
use crate::state::format_time_ago;
use crate::theme::Theme;

/// What the shell asks the view model.
mod calls {
    pub(super) const LIST: &str = "slopty.PastSessions";
    pub(super) const OPEN: &str = "slopty.OpenPastSession";
}

/// The History disclosure's label.
pub(super) const HISTORY: &str = "History";
/// The key of the History of every project, in a sidebar organised by status.
const EVERY_PROJECT: &str = "*";
/// What a History row is called before its title, for a screen reader.
pub(super) const PAST_SESSION: &str = "Past session:";
/// The palette's entry, and the picker's title.
pub(super) const RESUME: &str = "Resume a session\u{2026}";

/// Rows History lists when opened, and how many more each "Show more" adds.
const FIRST_PAGE: usize = 8;
const PAGE: usize = 20;
/// Sessions the picker lists.
const PICKER_LIMIT: usize = 50;
/// The pause in typing before the machine is asked for the words.
const ASK_AFTER: Duration = Duration::from_millis(150);
/// How long an opened session's chat is waited for in the sidebar's list.
const CHAT_WITHIN: Duration = Duration::from_secs(10);
/// How often it is looked for meanwhile.
const CHAT_POLL: Duration = Duration::from_millis(40);

/// The History header's height, and a state row's (loading, empty, failed, Show more).
const HEADER_HEIGHT: f32 = 28.0;
const LINE_HEIGHT: f32 = 28.0;
/// A session row's height: one line, or two with its branch.
const ROW_HEIGHT: f32 = 30.0;
const ROW_BRANCH_HEIGHT: f32 = 44.0;
/// The space between History's rows, as the sidebar's list keeps.
const GAP: f32 = 2.0;
/// The space above History's first row.
const BODY_INSET: f32 = 2.0;
/// The width of a row's time slot, which "Opening…" and "Running" take without moving the title.
const TIME_SLOT: f32 = 64.0;
/// History's clock, which its rows' words start past.
const HISTORY_ICON: f32 = 13.0;

/// A prompt as the machine found it: its words, where the asked words are in it, and whether
/// it was cut.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(super) struct PromptLine {
    pub(super) text: String,
    pub(super) spans: Vec<[u32; 2]>,
    pub(super) cut_before: bool,
    pub(super) cut_after: bool,
}

/// One past session, as the view model lists it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(super) struct PastRow {
    /// Its machine and id, unique across machines.
    pub(super) key: String,
    pub(super) device_id: String,
    pub(super) device: Option<String>,
    pub(super) native: String,
    /// Its first prompt, when the person typed one.
    pub(super) title: Option<String>,
    /// The prompt that matched the words, else its last prompt.
    pub(super) prompt: Option<PromptLine>,
    pub(super) cwd: Option<String>,
    pub(super) space_id: Option<String>,
    /// When it last changed, in ms since the epoch.
    pub(super) updated_at: Option<i64>,
    pub(super) branch: Option<String>,
    /// How a live Claude Code elsewhere holds it, when one does.
    pub(super) running: Option<String>,
    /// The chat it already is here.
    pub(super) chat_id: Option<String>,
    /// The words that take it up again.
    pub(super) resume: Vec<String>,
}

impl PastRow {
    /// What it is called: its first prompt, else its last, else its id.
    pub(super) fn label(&self) -> String {
        let said = self
            .title
            .as_deref()
            .or(self.prompt.as_ref().map(|p| p.text.as_str()))
            .map(crate::transcript::single_line)
            .filter(|t| !t.trim().is_empty());
        said.unwrap_or_else(|| {
            format!("Session {}", self.native.chars().take(8).collect::<String>())
        })
    }

    /// How long ago it last changed.
    fn ago(&self, now: chrono::DateTime<chrono::Utc>) -> Option<String> {
        let at = chrono::DateTime::from_timestamp_millis(self.updated_at?)?;
        Some(format_time_ago(at, now))
    }

    /// What its time slot says while a live Claude Code elsewhere holds it.
    fn running_words(&self) -> Option<&'static str> {
        self.running.as_deref().map(|how| match how {
            "interactive" => "In a terminal",
            _ => "Running",
        })
    }

    /// Why it cannot be taken up now, in words, while a live Claude Code elsewhere holds it.
    pub(super) fn held_elsewhere(&self) -> Option<String> {
        let how = self.running.as_deref()?;
        let place = if how == "interactive" { "in a terminal" } else { "in the background" };
        Some(format!(
            "Claude Code is running this session {place} on {}. Close it there to continue it here.",
            self.device.as_deref().filter(|d| !d.is_empty()).unwrap_or("its machine")
        ))
    }
}

/// A machine that could not answer, and why.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(super) struct Failure {
    pub(super) device_id: String,
    pub(super) device: Option<String>,
    pub(super) words: String,
}

/// What the view model answered.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(super) struct Listed {
    pub(super) sessions: Vec<PastRow>,
    pub(super) failures: Vec<Failure>,
    pub(super) more: bool,
    pub(super) cut: Option<String>,
}

impl Listed {
    /// The sessions that are no chat here yet: History's rows.
    fn unopened(&self) -> impl Iterator<Item = &PastRow> {
        self.sessions.iter().filter(|row| row.chat_id.is_none())
    }

    /// Why nothing could be listed, when every machine failed and none listed anything.
    fn failed(&self) -> Option<String> {
        (self.sessions.is_empty() && !self.failures.is_empty()).then(|| {
            let words: Vec<&str> = self.failures.iter().map(|f| f.words.as_str()).collect();
            words.join("; ")
        })
    }
}

/// Where a listing stands.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) enum Load {
    /// Never asked.
    #[default]
    Unasked,
    /// Asked; what was listed before stands meanwhile.
    Loading(Option<Listed>),
    Listed(Listed),
    /// The call failed, in its words.
    Failed(String),
}

impl Load {
    /// What is listed now, whether or not it is being asked again.
    fn listed(&self) -> Option<&Listed> {
        match self {
            Self::Loading(had) => had.as_ref(),
            Self::Listed(listed) => Some(listed),
            Self::Unasked | Self::Failed(_) => None,
        }
    }

    /// Ask again, keeping what is listed.
    fn reload(&mut self) {
        *self = Self::Loading(self.listed().cloned());
    }
}

/// One project group's History.
#[derive(Default)]
struct History {
    open: bool,
    /// The rows it lists.
    shown: usize,
    load: Load,
    /// Bumped per ask: an older answer is dropped.
    asked: u64,
    task: Option<Task<()>>,
}

/// What the picker searches: the chat's machine, and its project unless every folder.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Scope {
    device_id: String,
    space_id: Option<String>,
    /// The project's name, for the scope's tab.
    project: Option<String>,
}

/// The "Resume a session…" picker.
struct Picker {
    search: Entity<ComposerInput>,
    focus: FocusHandle,
    previous_focus: Option<FocusHandle>,
    focus_pending: bool,
    scope: Scope,
    /// Every folder of the machine, not the project's alone.
    every: bool,
    active: usize,
    scroll: gpui::ScrollHandle,
    load: Load,
    asked: u64,
    task: Option<Task<()>>,
    /// The words of the listing shown, which lights them.
    words: String,
    /// The open that failed, in its machine's words.
    refused: Option<(String, String)>,
    _events: Subscription,
}

/// The past sessions' state, one per shell.
#[derive(Default)]
pub(super) struct PastSessions {
    /// By project group.
    groups: HashMap<String, History>,
    picker: Option<Picker>,
    /// The session being opened, by its key, and the open on its way.
    opening: Option<(String, Task<()>)>,
}

impl PastSessions {
    /// Whether the picker is up: it holds the keyboard.
    pub(super) fn picker_open(&self) -> bool {
        self.picker.is_some()
    }

    fn opening(&self, key: &str) -> bool {
        self.opening.as_ref().is_some_and(|(k, _)| k == key)
    }
}

/// How tall History stands: its header, then while open its rows, "Show more" or the line that
/// says where it stands.
fn history_height(history: Option<&History>) -> f32 {
    let Some(history) = history.filter(|h| h.open) else { return HEADER_HEIGHT };
    let body = match history.load.listed() {
        Some(listed) => {
            let rows: Vec<&PastRow> = listed.unopened().take(shown(history)).collect();
            // A line says there are none, or that a machine failed beside the rows listed.
            let said = rows.is_empty() || !listed.failures.is_empty();
            let lines = rows.len() + usize::from(said) + usize::from(more(history));
            let heights: f32 = rows.iter().map(|row| row_height(row)).sum::<f32>()
                + if said { LINE_HEIGHT } else { 0.0 }
                + if more(history) { LINE_HEIGHT } else { 0.0 };
            heights + GAP * lines.saturating_sub(1) as f32
        }
        None => LINE_HEIGHT,
    };
    HEADER_HEIGHT + BODY_INSET + body
}

/// How many rows History shows.
fn shown(history: &History) -> usize {
    history.shown.max(FIRST_PAGE)
}

/// Whether History has more to show than it does.
fn more(history: &History) -> bool {
    history.load.listed().is_some_and(|listed| {
        listed.more || listed.unopened().count() > shown(history)
    })
}

fn row_height(row: &PastRow) -> f32 {
    if row.branch.is_some() { ROW_BRANCH_HEIGHT } else { ROW_HEIGHT }
}

impl Shell {
    /// The spaces of the project group `key`: every checkout of it, this device's first. For
    /// [`EVERY_PROJECT`], the filtered project's, else every project's.
    fn history_spaces(&self, key: &str, cx: &gpui::App) -> Vec<String> {
        let state = self.state.read(cx);
        let space = if key == EVERY_PROJECT {
            let Some(filter) = self.settings.space_filter.as_deref() else {
                return state.spaces.iter().map(|s| s.id.clone()).collect();
            };
            state.spaces.iter().find(|space| space.id == filter)
        } else {
            state.spaces.iter().find(|space| slopty_view_proto::view::project_key(space) == key)
        };
        let Some(space) = space else { return Vec::new() };
        state.project_members(space).into_iter().map(|s| s.id.clone()).collect()
    }

    /// History of every project, above Archived, in a sidebar organised by status, where no
    /// group is a project's.
    pub(super) fn render_sidebar_history(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if self.settings.sidebar_organization == SidebarOrganization::ByProject
            || self.state.read(cx).spaces.is_empty()
        {
            return None;
        }
        let open = self.past_sessions.groups.get(EVERY_PROJECT).is_some_and(|h| h.open);
        // slopty: a key reads as everywhere else (`keys::combo_text`).
        let shortcut = crate::keys::combo_text(self.settings.keymap.get(ShortcutId::ResumeSession));
        let chevron = icon(icons::ALT_ARROW_RIGHT)
            .size(px(12.0))
            .flex_none()
            .text_color(super::spaces::sidebar_header_glyph(theme))
            .when(open, |chevron| {
                chevron.with_transformation(gpui::Transformation::rotate(gpui::percentage(0.25)))
            });
        let header = super::spaces::sidebar_disclosure_header(
            theme,
            None,
            HISTORY.into(),
            None,
            None,
            chevron.into_any_element(),
        )
        .id("history-toggle")
        .debug_selector(|| "history-toggle".into())
        .role(gpui::Role::Button)
        .aria_label(SharedString::from(if open {
            "History, open: past Claude Code sessions"
        } else {
            "History: past Claude Code sessions"
        }))
        .tooltip(crate::settings::widgets::text_tooltip(format!(
            "Past Claude Code sessions \u{00B7} {RESUME} {shortcut}"
        )))
        .on_click(cx.listener(|this, _, _, cx| this.toggle_history(EVERY_PROJECT, cx)));
        let mut section = div().flex().flex_col().pt(px(12.0)).child(header);
        if open {
            section = section.child(self.render_history_body(EVERY_PROJECT, theme, cx));
        }
        Some(section.into_any_element())
    }

    /// History, the last row of the project group `key`: its key, its height (with the list's
    /// gap above it) and its element. Only a project's group has one.
    pub(super) fn project_history(
        &mut self,
        key: &str,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<(String, f32, AnyElement)> {
        if self.settings.sidebar_organization != SidebarOrganization::ByProject
            || key.starts_with("section:")
        {
            return None;
        }
        if self.history_spaces(key, cx).is_empty() {
            return None;
        }
        let height = history_height(self.past_sessions.groups.get(key));
        let element = self.render_history(key, theme, cx);
        Some((format!("history:{key}"), height + super::SIDEBAR_LIST_GAP, element))
    }

    /// Open or shut the project group `key`'s History; opened, it asks again.
    fn toggle_history(&mut self, key: &str, cx: &mut Context<Self>) {
        let history = self.past_sessions.groups.entry(key.to_owned()).or_default();
        history.open = !history.open;
        history.shown = FIRST_PAGE;
        if history.open {
            self.load_history(key, cx);
        } else {
            history.task = None;
            if matches!(history.load, Load::Loading(_)) {
                history.load = history.load.listed().cloned().map_or(Load::Unasked, Load::Listed);
            }
        }
        cx.notify();
    }

    /// Ask the project group `key`'s machines for its sessions, as many as it shows and one
    /// more, keeping the rows listed meanwhile.
    fn load_history(&mut self, key: &str, cx: &mut Context<Self>) {
        let spaces = self.history_spaces(key, cx);
        let engine = self.state.read(cx).engine().cloned();
        let Some(history) = self.past_sessions.groups.get_mut(key) else { return };
        let Some(engine) = engine else {
            history.load = Load::Failed("Not connected".to_owned());
            return;
        };
        history.asked = history.asked.wrapping_add(1);
        let asked = history.asked;
        history.load.reload();
        let params = serde_json::json!({
            "spaceIds": spaces,
            "query": "",
            // A session that became a chat here is left out: ask past the page for it.
            "limit": shown(history).saturating_add(PAGE),
        });
        let key = key.to_owned();
        history.task = Some(cx.spawn(async move |this, cx| {
            let listed = engine.client().call_as::<Listed>(calls::LIST, params).await;
            let _gone = this.update(cx, |this, cx| {
                let Some(history) = this.past_sessions.groups.get_mut(&key) else { return };
                if history.asked != asked {
                    return;
                }
                history.task = None;
                history.load = match listed {
                    Ok(listed) => match listed.failed() {
                        Some(why) => Load::Failed(why),
                        None => Load::Listed(listed),
                    },
                    Err(error) => Load::Failed(error.to_string()),
                };
                cx.notify();
            });
        }));
    }

    /// Every open History asks again: a session just opened is a chat now.
    fn reload_histories(&mut self, cx: &mut Context<Self>) {
        let open: Vec<String> = self
            .past_sessions
            .groups
            .iter()
            .filter(|(_, history)| history.open)
            .map(|(key, _)| key.clone())
            .collect();
        for key in open {
            self.load_history(&key, cx);
        }
    }

    /// History's header and, while open, its rows.
    fn render_history(&mut self, key: &str, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let open = self.past_sessions.groups.get(key).is_some_and(|h| h.open);
        // slopty: a key reads as everywhere else (`keys::combo_text`).
        let shortcut = crate::keys::combo_text(self.settings.keymap.get(ShortcutId::ResumeSession));
        let toggle = key.to_owned();
        let header = div()
            .id(SharedString::from(format!("history-toggle-{key}")))
            .debug_selector({
                let key = key.to_owned();
                move || format!("history-toggle-{key}")
            })
            .h(px(HEADER_HEIGHT))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(8.0))
            .px(px(Theme::SPACE_SM))
            .rounded(px(6.0))
            .cursor_pointer()
            .role(gpui::Role::Button)
            .aria_label(SharedString::from(if open {
                "History, open: past Claude Code sessions"
            } else {
                "History: past Claude Code sessions"
            }))
            .text_size(crate::typography::ui_rems(12.0))
            .text_color(theme.text_muted.opacity(0.55))
            .hover(|s| s.bg(theme.glass_hover()).text_color(theme.text_muted))
            .tooltip(crate::settings::widgets::text_tooltip(format!(
                "Past Claude Code sessions in this project \u{00B7} {RESUME} {shortcut}"
            )))
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.toggle_history(&toggle, cx);
            }))
            .child(icon(icons::CLOCK_CIRCLE).size(px(HISTORY_ICON)).flex_none().text_color(theme.text_faint))
            .child(div().flex_1().min_w_0().truncate().child(HISTORY))
            .child(
                icon(icons::ALT_ARROW_RIGHT)
                    .size(px(12.0))
                    .flex_none()
                    .text_color(theme.text_faint)
                    .when(open, |chevron| {
                        chevron.with_transformation(gpui::Transformation::rotate(
                            gpui::percentage(0.25),
                        ))
                    }),
            );
        let mut column = div().w_full().flex().flex_col().child(header);
        if open {
            column = column.child(self.render_history_body(key, theme, cx));
        }
        column.into_any_element()
    }

    /// History's rows, or the line that says where it stands.
    fn render_history_body(&mut self, key: &str, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let now = chrono::Utc::now();
        let view = cx.entity_id();
        let Some(history) = self.past_sessions.groups.get(key) else {
            return div().into_any_element();
        };
        let mut body = div().w_full().flex().flex_col().pt(px(BODY_INSET)).gap(px(GAP));
        let Some(listed) = history.load.listed() else {
            let line = match &history.load {
                Load::Failed(why) => self.failure_line(key, "history-failed", why.clone(), theme, cx),
                _ => state_line(theme, "history-loading", "Reading Claude Code\u{2019}s sessions\u{2026}".into(), false)
                    .child(crate::loaders::mini_mono_spinner(
                        format!("history-spinner-{key}"),
                        2.0,
                        theme.text_muted,
                        view,
                        cx,
                    )),
            };
            return body.child(line).into_any_element();
        };
        let rows: Vec<PastRow> = listed.unopened().take(shown(history)).cloned().collect();
        let has_more = more(history);
        // A machine that failed beside the rows the others listed.
        let partial = (!rows.is_empty()).then(|| listed.failures.first().map(|f| f.words.clone())).flatten();
        if rows.is_empty() {
            let line = match listed.failures.first() {
                Some(failure) => self.failure_line(key, "history-empty", failure.words.clone(), theme, cx),
                None => state_line(theme, "history-empty", "No past Claude Code sessions here".to_owned(), false),
            };
            body = body.child(line);
        }
        for row in rows {
            let element = self.render_history_row(&row, key == EVERY_PROJECT, now, theme, cx);
            body = body.child(element);
        }
        if let Some(words) = partial {
            body = body.child(self.failure_line(key, "history-partial", words, theme, cx));
        }
        if has_more {
            let page = key.to_owned();
            body = body.child(
                state_line(theme, "history-more", "Show more".to_owned(), false)
                    .cursor_pointer()
                    .hover(|s| s.bg(theme.glass_hover()).text_color(theme.text))
                    .tooltip(crate::settings::widgets::text_tooltip(
                        "List older Claude Code sessions",
                    ))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(history) = this.past_sessions.groups.get_mut(&page) {
                            history.shown = shown(history).saturating_add(PAGE);
                        }
                        this.load_history(&page, cx);
                        cx.notify();
                    })),
            );
        }
        body.into_any_element()
    }

    /// Why History of `key` is missing sessions, in the machine's words, with Try again: a
    /// click asks once more.
    fn failure_line(
        &self,
        key: &str,
        id: &'static str,
        why: String,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let retry = key.to_owned();
        state_line(theme, id, why.clone(), true)
            .tooltip(crate::settings::widgets::text_tooltip(format!("{why}\nClick to try again")))
            .cursor_pointer()
            .hover(|s| s.bg(theme.glass_hover()))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.load_history(&retry, cx);
                cx.notify();
            }))
            .child(div().flex_none().text_color(theme.text_muted).child("Try again"))
    }

    /// One past session in History: its title, how long ago (or that it is opening, or held
    /// elsewhere) in a slot of its own, and under it its project (in the History of every
    /// project) and its branch, where known.
    fn render_history_row(
        &mut self,
        row: &PastRow,
        every: bool,
        now: chrono::DateTime<chrono::Utc>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let opening = self.past_sessions.opening(&row.key);
        let label = row.label();
        let corner: SharedString = if opening {
            "Opening\u{2026}".into()
        } else if let Some(words) = row.running_words() {
            words.into()
        } else {
            row.ago(now).unwrap_or_default().into()
        };
        let running = row.running.is_some();
        let tooltip = match row.held_elsewhere() {
            Some(held) => format!("{label}\n{held}"),
            None => format!("{label}\nOpen and continue this Claude Code session"),
        };
        let target = row.clone();
        let title = div()
            .flex()
            .items_center()
            .gap(px(8.0))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(theme.text_muted)
                    .child(SharedString::from(label.clone())),
            )
            .child(
                div()
                    .flex_none()
                    .w(px(TIME_SLOT))
                    .flex()
                    .justify_end()
                    .text_size(crate::typography::ui_rems(11.0))
                    .text_color(if running { theme.warning } else { theme.text_faint })
                    .child(corner.clone()),
            );
        let project = every.then(|| self.row_project(row, cx)).flatten();
        let meta = (project.is_some() || row.branch.is_some()).then(|| {
            div()
                .flex()
                .items_center()
                .gap(px(4.0))
                .text_size(crate::typography::ui_rems(11.0))
                .text_color(theme.text_faint)
                .when_some(project.clone(), |el, project| {
                    el.child(div().flex_none().max_w(px(120.0)).truncate().child(SharedString::from(project)))
                })
                .when_some(row.branch.clone(), |el, branch| {
                    el.child(icon(icons::GIT_BRANCH).size(px(11.0)).flex_none().text_color(theme.text_faint))
                        .child(div().min_w_0().truncate().child(SharedString::from(branch)))
                })
        });
        let height = if meta.is_some() { ROW_BRANCH_HEIGHT } else { ROW_HEIGHT };
        div()
            .id(SharedString::from(format!("history-row-{}", row.key)))
            .debug_selector({
                let key = row.key.clone();
                move || format!("history-row-{key}")
            })
            .h(px(height))
            .flex_none()
            .flex()
            .flex_col()
            .justify_center()
            .gap(px(2.0))
            // Under History's words, past its clock: the rows read as History's own.
            .pl(px(Theme::SPACE_SM + HISTORY_ICON + 8.0))
            .pr(px(Theme::SPACE_SM))
            .rounded(px(6.0))
            .cursor_pointer()
            .role(gpui::Role::Button)
            .aria_label(SharedString::from(format!("{PAST_SESSION} {label}, {corner}")))
            .text_size(crate::typography::ui_rems(13.0))
            .hover(|s| s.bg(theme.glass_hover()))
            .tooltip(crate::settings::widgets::text_tooltip(tooltip))
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.open_past_session(target.clone(), cx);
            }))
            .child(title)
            .children(meta)
            .into_any_element()
    }

    /// The project `row` ran in, by its name here, else its folder's.
    fn row_project(&self, row: &PastRow, cx: &gpui::App) -> Option<String> {
        let state = self.state.read(cx);
        let named = row.space_id.as_deref().and_then(|id| state.spaces.iter().find(|s| s.id == id));
        named.map(|space| space.display_name().to_owned()).or_else(|| {
            let cwd = row.cwd.as_deref()?.trim_end_matches('/');
            Some(cwd.rsplit('/').next().unwrap_or(cwd).to_owned())
        })
    }

    /// Open `row`: the chat it already is, else Claude Code takes it up again on its machine and
    /// its chat opens once started. Why it does not open is said in its machine's words.
    pub(super) fn open_past_session(&mut self, row: PastRow, cx: &mut Context<Self>) {
        if let Some(chat) = row.chat_id.clone() {
            self.close_resume_picker(None, cx);
            self.open_chat(chat, cx);
            return;
        }
        if self.past_sessions.opening(&row.key) {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.show_notice(crate::toast::ToastKind::Error, "Couldn\u{2019}t reach the Slopty engine. Wait for it to connect, then try again.", cx);
            return;
        };
        let params = serde_json::json!({
            "deviceId": row.device_id,
            "cwd": row.cwd,
            "resume": row.resume,
        });
        let key = row.key.clone();
        let task = cx.spawn(async move |this, cx| {
            let opened = engine.client().call(calls::OPEN, params).await;
            let chat = match opened {
                Ok(answer) => answer.get("chatId").and_then(|c| c.as_str()).map(str::to_owned),
                Err(error) => {
                    let _gone = this.update(cx, |this, cx| this.past_refused(&key, &error.to_string(), cx));
                    return;
                }
            };
            let Some(chat) = chat else {
                let _gone =
                    this.update(cx, |this, cx| this.past_refused(&key, "It did not say its chat", cx));
                return;
            };
            // The chat opens once the sidebar's list has it.
            let since = std::time::Instant::now();
            loop {
                let listed = this
                    .update(cx, |this, cx| this.state.read(cx).chats.iter().any(|c| c.id == chat))
                    .unwrap_or(true);
                if listed || since.elapsed() >= CHAT_WITHIN {
                    break;
                }
                cx.background_executor().timer(CHAT_POLL).await;
            }
            let _gone = this.update(cx, |this, cx| {
                this.past_sessions.opening = None;
                this.close_resume_picker(None, cx);
                this.open_chat(chat, cx);
                this.reload_histories(cx);
            });
        });
        self.past_sessions.opening = Some((row.key, task));
        cx.notify();
    }

    /// The session `key` did not open, for `why`: said where it was asked, the picker's footer
    /// or a notice.
    fn past_refused(&mut self, key: &str, why: &str, cx: &mut Context<Self>) {
        self.past_sessions.opening = None;
        match self.past_sessions.picker.as_mut() {
            Some(picker) => picker.refused = Some((key.to_owned(), why.to_owned())),
            None => self.show_notice(
                crate::toast::ToastKind::Error,
                format!("Couldn\u{2019}t open the Claude Code session. Try again. ({why})"),
                cx,
            ),
        }
        cx.notify();
    }

    // ---- the picker ----

    /// Where the picker searches: the open chat's machine and project, else the new chat's
    /// project, else the machine the new chat targets.
    fn resume_scope(&self, cx: &gpui::App) -> Option<Scope> {
        let state = self.state.read(cx);
        let project = |space: &str| {
            state.spaces.iter().find(|s| s.id == space).map(|s| s.display_name().to_owned())
        };
        let chat = state
            .selected_chat
            .as_deref()
            .and_then(|id| state.chats.iter().find(|chat| chat.id == id));
        if let Some(chat) = chat {
            let space_id = chat.space_id.clone();
            let project = space_id.as_deref().and_then(project);
            return Some(Scope { device_id: chat.device_id.clone(), space_id, project });
        }
        if let Some(space) = state.selected_space_row() {
            return Some(Scope {
                device_id: space.device_id.clone(),
                space_id: Some(space.id.clone()),
                project: Some(space.display_name().to_owned()),
            });
        }
        let device = state.effective_device_id().or_else(|| state.devices.first().map(|d| d.id.clone()))?;
        Some(Scope { device_id: device, space_id: None, project: None })
    }

    /// "Resume a session…": the picker, or shut it when it is up.
    pub(super) fn toggle_resume_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.past_sessions.picker.is_some() {
            self.close_resume_picker(Some(window), cx);
            return;
        }
        let Some(scope) = self.resume_scope(cx) else {
            self.show_notice(crate::toast::ToastKind::Error, "Couldn\u{2019}t list past sessions with no machine connected. Add one in Settings \u{2192} Devices.", cx);
            return;
        };
        self.command_palette = None;
        let search = cx.new(|cx| {
            ComposerInput::with_context("Search what you asked Claude Code\u{2026}", "PaletteSearch", cx)
        });
        let events = cx.subscribe(&search, |this, _, event, cx| {
            if matches!(event, ComposerInputEvent::Edited) {
                this.ask_picker(true, cx);
            }
        });
        let every = scope.space_id.is_none();
        self.past_sessions.picker = Some(Picker {
            search,
            focus: cx.focus_handle(),
            previous_focus: window.focused(cx),
            focus_pending: true,
            scope,
            every,
            active: 0,
            scroll: gpui::ScrollHandle::new(),
            load: Load::Unasked,
            asked: 0,
            task: None,
            words: String::new(),
            refused: None,
            _events: events,
        });
        self.ask_picker(false, cx);
        cx.notify();
    }

    /// Shut the picker, the keyboard back where it was.
    fn close_resume_picker(&mut self, window: Option<&mut Window>, cx: &mut Context<Self>) {
        let Some(picker) = self.past_sessions.picker.take() else { return };
        if let (Some(window), Some(focus)) = (window, picker.previous_focus) {
            window.focus(&focus, cx);
        }
        cx.notify();
    }

    /// Ask the machine for what the picker's words find, after a pause in typing when `typed`.
    fn ask_picker(&mut self, typed: bool, cx: &mut Context<Self>) {
        let engine = self.state.read(cx).engine().cloned();
        let Some(picker) = self.past_sessions.picker.as_mut() else { return };
        let Some(engine) = engine else {
            picker.load = Load::Failed("Not connected".to_owned());
            return;
        };
        let words = picker.search.read(cx).text().trim().to_owned();
        picker.asked = picker.asked.wrapping_add(1);
        picker.refused = None;
        let asked = picker.asked;
        picker.load.reload();
        let mut params = serde_json::json!({ "query": words, "limit": PICKER_LIMIT });
        match (&picker.scope.space_id, picker.every) {
            (Some(space), false) => params["spaceIds"] = serde_json::json!([space]),
            _ => {
                params["deviceId"] = serde_json::json!(picker.scope.device_id);
                params["allFolders"] = serde_json::json!(true);
            }
        }
        picker.task = Some(cx.spawn(async move |this, cx| {
            if typed {
                cx.background_executor().timer(ASK_AFTER).await;
            }
            let listed = engine.client().call_as::<Listed>(calls::LIST, params).await;
            let _gone = this.update(cx, |this, cx| {
                let Some(picker) = this.past_sessions.picker.as_mut() else { return };
                if picker.asked != asked {
                    return;
                }
                picker.task = None;
                picker.words = words;
                picker.active = 0;
                picker.scroll.set_offset(gpui::point(px(0.0), px(0.0)));
                picker.load = match listed {
                    Ok(listed) => match listed.failed() {
                        Some(why) => Load::Failed(why),
                        None => Load::Listed(listed),
                    },
                    Err(error) => Load::Failed(error.to_string()),
                };
                cx.notify();
            });
        }));
    }

    /// Search the project alone, or every folder of the machine.
    fn toggle_picker_scope(&mut self, cx: &mut Context<Self>) {
        let Some(picker) = self.past_sessions.picker.as_mut() else { return };
        if picker.scope.space_id.is_none() {
            return;
        }
        picker.every = !picker.every;
        self.ask_picker(false, cx);
        cx.notify();
    }

    fn picker_rows(&self) -> Vec<PastRow> {
        self.past_sessions
            .picker
            .as_ref()
            .and_then(|p| p.load.listed())
            .map(|listed| listed.sessions.clone())
            .unwrap_or_default()
    }

    fn move_picker(&mut self, down: bool, cx: &mut Context<Self>) {
        let count = self.picker_rows().len();
        let Some(picker) = self.past_sessions.picker.as_mut() else { return };
        if count == 0 {
            return;
        }
        picker.active = if down {
            (picker.active + 1) % count
        } else {
            (picker.active + count - 1) % count
        };
        picker.scroll.scroll_to_item(picker.active);
        cx.notify();
    }

    fn open_picked(&mut self, cx: &mut Context<Self>) {
        let rows = self.picker_rows();
        let Some(row) = self.past_sessions.picker.as_ref().and_then(|p| rows.get(p.active)).cloned()
        else {
            return;
        };
        self.open_past_session(row, cx);
    }

    /// The picker over the window, while it is up.
    pub(super) fn render_resume_picker(
        &mut self,
        viewport: gpui::Size<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let theme = Theme::of(cx).for_popup();
        // slopty: a key reads as everywhere else (`keys::combo_text`).
        let shortcut = crate::keys::combo_text(self.settings.keymap.get(ShortcutId::ResumeSession));
        let now = chrono::Utc::now();
        let view = cx.entity_id();
        let opening = self.past_sessions.opening.as_ref().map(|(key, _)| key.clone());
        let picker = self.past_sessions.picker.as_mut()?;
        if std::mem::take(&mut picker.focus_pending) {
            window.focus(&picker.search.focus_handle(cx), cx);
        }
        let search = picker.search.clone();
        let focus = picker.focus.clone();
        let scroll = picker.scroll.clone();
        let active = picker.active;
        let every = picker.every;
        let words = picker.words.clone();
        let refused = picker.refused.clone();
        let scope = picker.scope.clone();
        let load = picker.load.clone();
        let mut children: Vec<AnyElement> = Vec::new();
        let rows = load.listed().map(|l| l.sessions.clone()).unwrap_or_default();
        for (ix, row) in rows.iter().enumerate() {
            let target = row.clone();
            let opening = opening.as_deref() == Some(row.key.as_str());
            let refusal = refused.as_ref().filter(|(key, _)| *key == row.key).map(|(_, why)| why.clone());
            let element = picker_row(row, ix, ix == active, opening, refusal, every, &words, now, &theme)
                .on_click(cx.listener(move |this, _, _, cx| this.open_past_session(target.clone(), cx)))
                .on_mouse_move(cx.listener(move |this, _: &gpui::MouseMoveEvent, _, cx| {
                    if let Some(picker) = this.past_sessions.picker.as_mut()
                        && picker.active != ix
                    {
                        picker.active = ix;
                        cx.notify();
                    }
                }));
            children.push(div().px(px(8.0)).child(element).into_any_element());
        }
        let status: Option<AnyElement> = match &load {
            Load::Unasked | Load::Loading(None) => Some(
                div()
                    .py(px(24.0))
                    .flex()
                    .justify_center()
                    .child(crate::loaders::mini_mono_spinner("picker-spinner", 2.5, theme.text_muted, view, cx))
                    .into_any_element(),
            ),
            Load::Failed(why) => Some(
                palette_empty(&theme, "Couldn\u{2019}t read Claude Code\u{2019}s sessions", why.clone())
                    .into_any_element(),
            ),
            Load::Listed(listed) | Load::Loading(Some(listed)) if listed.sessions.is_empty() => {
                let hint = match (words.is_empty(), every) {
                    (true, false) => "Claude Code kept no sessions in this project. Press \u{21E5} for every folder.",
                    (true, true) => "Claude Code kept no sessions on this machine.",
                    (false, false) => "No prompt here says that. Press \u{21E5} to search every folder.",
                    (false, true) => "No prompt on this machine says that.",
                };
                Some(palette_empty(&theme, "No past sessions", hint).into_any_element())
            }
            _ => None,
        };
        let body = div()
            .id("resume-results")
            .min_h_0()
            .max_h(px(palette_results_height(viewport)))
            .overflow_y_scroll()
            .track_scroll(&scroll)
            .flex()
            .flex_col()
            .gap(px(super::SIDEBAR_LIST_GAP))
            .pt(px(8.0))
            .pb(px(8.0))
            .children(children)
            .children(status);
        let body = palette_results_fade(body, &scroll);
        let has_project = scope.space_id.is_some();
        let project_label: SharedString = scope.project.clone().unwrap_or_else(|| "This project".to_owned()).into();
        let tabs = div()
            .flex_none()
            .px(px(12.0))
            .py(px(6.0))
            .flex()
            .items_center()
            .gap(px(4.0))
            .border_b_1()
            .border_color(crate::theme::hairline(0.06))
            .when(has_project, |tabs| {
                tabs.child(
                    crate::surface_chrome::tab("resume-scope-project", !every, &theme)
                        .px(px(8.0))
                        .role(gpui::Role::Tab)
                        .aria_label(project_label.clone())
                        .tooltip(crate::settings::widgets::text_tooltip("Search this project \u{00B7} \u{21E5}"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            if this.past_sessions.picker.as_ref().is_some_and(|p| p.every) {
                                this.toggle_picker_scope(cx);
                            }
                        }))
                        .child(project_label.clone()),
                )
            })
            .child(
                crate::surface_chrome::tab("resume-scope-every", every, &theme)
                    .px(px(8.0))
                    .role(gpui::Role::Tab)
                    .aria_label("Every folder")
                    .tooltip(crate::settings::widgets::text_tooltip("Search every folder on this machine \u{00B7} \u{21E5}"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        if this.past_sessions.picker.as_ref().is_some_and(|p| !p.every) {
                            this.toggle_picker_scope(cx);
                        }
                    }))
                    .child("Every folder"),
            )
            .child(div().flex_1())
            .when_some(load.listed().and_then(|l| l.cut.clone()), |tabs, cut| {
                tabs.child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(crate::typography::ui_rems(11.0))
                        .text_color(theme.text_faint)
                        .child(SharedString::from(cut)),
                )
            });
        let card = palette_card("resume-picker", &focus, viewport, &theme)
            .key_context("ResumePicker")
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                match event.keystroke.key.as_str() {
                    "up" | "down" => this.move_picker(event.keystroke.key == "down", cx),
                    "enter" => this.open_picked(cx),
                    "tab" => this.toggle_picker_scope(cx),
                    "escape" => this.close_resume_picker(Some(window), cx),
                    _ => return,
                }
                cx.stop_propagation();
            }))
            .on_mouse_down_out(cx.listener(|this, _, window, cx| this.close_resume_picker(Some(window), cx)))
            .child(palette_header(&theme, search.into_any_element(), popover::kbd_hint(&theme, &shortcut)))
            .child(tabs)
            .child(body)
            .child(
                palette_footer()
                    .child(command_key_hint(&theme, "\u{2191} \u{2193}", "Navigate"))
                    .child(command_key_hint(&theme, "\u{21B5}", "Open"))
                    .when(has_project, |footer| footer.child(command_key_hint(&theme, "Tab", "Scope")))
                    .child(command_key_hint(&theme, "Esc", "Close")),
            );
        Some(palette_overlay(viewport, card))
    }
}

/// A state line of History: what it is doing, or why it has nothing, one line tall.
fn state_line(theme: &Theme, id: &'static str, words: String, failed: bool) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .h(px(LINE_HEIGHT))
        .flex_none()
        .flex()
        .items_center()
        .gap(px(8.0))
        .px(px(Theme::SPACE_SM))
        .rounded(px(6.0))
        .text_size(crate::typography::ui_rems(12.0))
        .text_color(if failed { theme.danger.opacity(0.8) } else { theme.text_muted.opacity(0.55) })
        .role(gpui::Role::Status)
        .aria_label(SharedString::from(words.clone()))
        .child(div().flex_1().min_w_0().truncate().child(SharedString::from(words)))
}

/// One session in the picker: its title with the words lit, the prompt that matched (or its
/// last) with the words lit, and where and when; why it did not open, when it did not.
#[expect(clippy::too_many_arguments, reason = "one row's facts, drawn together")]
fn picker_row(
    row: &PastRow,
    ix: usize,
    active: bool,
    opening: bool,
    refused: Option<String>,
    every: bool,
    words: &str,
    now: chrono::DateTime<chrono::Utc>,
    theme: &Theme,
) -> gpui::Stateful<gpui::Div> {
    let label = row.label();
    // The prompt that matched is its own line unless it is the title, already lit above.
    let prompt = row.prompt.as_ref().filter(|p| crate::transcript::single_line(&p.text) != label);
    let prompt_line = prompt.map(|p| prompt_text(p, theme));
    let mut place: Vec<String> = Vec::new();
    if every && let Some(cwd) = row.cwd.as_deref() {
        place.push(cwd.rsplit('/').find(|part| !part.is_empty()).unwrap_or(cwd).to_owned());
    }
    if let Some(branch) = &row.branch {
        place.push(branch.clone());
    }
    let corner: SharedString = if opening {
        "Opening\u{2026}".into()
    } else if let Some(words) = row.running_words() {
        words.into()
    } else {
        row.ago(now).unwrap_or_default().into()
    };
    let second: Option<AnyElement> = match (&refused, prompt_line) {
        (Some(why), _) => Some(
            div()
                .min_w_0()
                .truncate()
                .text_size(crate::typography::ui_rems(12.0))
                .text_color(theme.danger)
                .child(SharedString::from(why.clone()))
                .into_any_element(),
        ),
        (None, Some(line)) => Some(line),
        (None, None) => None,
    };
    let label_text: SharedString = label.clone().into();
    let tooltip = row.held_elsewhere().unwrap_or_else(|| "Open and continue \u{00B7} \u{21B5}".to_owned());
    popover::menu_row(theme, active, format!("resume-row-{ix}"))
        .id(("resume-row", ix))
        .rounded(px(popover::PALETTE_ITEM_RADIUS))
        .role(gpui::Role::Button)
        .aria_label(SharedString::from(format!(
            "{label}{}, {corner}",
            prompt.map(|p| format!(", {}", p.text)).unwrap_or_default()
        )))
        .min_h(px(44.0))
        .py(px(5.0))
        .tooltip(crate::settings::widgets::text_tooltip(tooltip))
        .child(icon(icons::CLAUDE_MARK).size(px(16.0)).flex_none().text_color(crate::icons::claude_brand()))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(2.0))
                .child(div().truncate().child(popover::search_highlight(label_text, Some(words), theme)))
                .children(second),
        )
        .child(
            div()
                .flex_none()
                .flex()
                .flex_col()
                .items_end()
                .gap(px(2.0))
                .text_size(crate::typography::ui_rems(11.0))
                .child(
                    div()
                        .w(px(TIME_SLOT))
                        .flex()
                        .justify_end()
                        .text_color(if row.running.is_some() { theme.warning } else { theme.text_faint })
                        .child(corner),
                )
                .when(!place.is_empty(), |el| {
                    el.child(
                        div()
                            .max_w(px(160.0))
                            .truncate()
                            .text_color(theme.text_faint)
                            .child(SharedString::from(place.join(" \u{00B7} "))),
                    )
                }),
        )
}

/// A prompt on one line with the asked words lit: `…` where it was cut.
fn prompt_text(prompt: &PromptLine, theme: &Theme) -> AnyElement {
    let (line, ranges) = prompt_line(prompt);
    div()
        .min_w_0()
        .truncate()
        .text_size(crate::typography::ui_rems(12.0))
        .text_color(theme.text_faint)
        .child(popover::search_highlight_ranges(line.into(), ranges, theme))
        .into_any_element()
}

/// `prompt` on one line, `…` before and after where it was cut, and where the words are in it.
/// A line break becomes a space, byte for byte, so the spans still land; a span off the text's
/// character boundaries is dropped.
pub(super) fn prompt_line(prompt: &PromptLine) -> (String, Vec<std::ops::Range<usize>>) {
    const ELLIPSIS: &str = "\u{2026}";
    let lead = if prompt.cut_before { ELLIPSIS.len() } else { 0 };
    let mut line = String::with_capacity(prompt.text.len() + 2 * ELLIPSIS.len());
    if prompt.cut_before {
        line.push_str(ELLIPSIS);
    }
    line.extend(prompt.text.chars().map(|c| if matches!(c, '\n' | '\r' | '\t') { ' ' } else { c }));
    if prompt.cut_after {
        line.push_str(ELLIPSIS);
    }
    let ranges = prompt
        .spans
        .iter()
        .filter_map(|[start, end]| {
            let (start, end) = (usize::try_from(*start).ok()?, usize::try_from(*end).ok()?);
            (start < end
                && end <= prompt.text.len()
                && prompt.text.is_char_boundary(start)
                && prompt.text.is_char_boundary(end))
            .then(|| start + lead..end + lead)
        })
        .collect();
    (line, ranges)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(native: &str) -> PastRow {
        PastRow { key: format!("d:{native}"), native: native.to_owned(), ..PastRow::default() }
    }

    /// A session is called by its first prompt, else its last, else its id; on one line.
    #[test]
    fn a_session_is_named_by_what_was_asked_in_it() {
        let mut past = row("0123456789abcdef");
        assert_eq!(past.label(), "Session 01234567");
        past.prompt = Some(PromptLine { text: "the last\nprompt".to_owned(), ..PromptLine::default() });
        assert_eq!(past.label(), "the last prompt");
        past.title = Some("Fix the build".to_owned());
        assert_eq!(past.label(), "Fix the build");
    }

    /// The view model's answer reads whole, and a field it leaves out reads as none.
    #[test]
    fn an_answer_reads_as_the_view_model_writes_it() {
        let answer = serde_json::json!({
            "sessions": [{
                "key": "dev:abc", "deviceId": "dev", "device": "Studio", "native": "abc",
                "title": "Fix it", "prompt": { "text": "fix it", "spans": [[0, 3]],
                "cutBefore": false, "cutAfter": true }, "cwd": "/work", "spaceId": "dev:/work",
                "updatedAt": 1_700_000_000_000_i64, "branch": "main", "running": null,
                "chatId": null, "resume": ["--resume", "abc"],
            }],
            "failures": [{ "deviceId": "other", "device": "Laptop", "words": "Laptop is out of reach" }],
            "more": true,
            "cut": null,
        });
        let listed: Listed = serde_json::from_value(answer).unwrap();
        assert_eq!(listed.sessions[0].branch.as_deref(), Some("main"));
        assert_eq!(listed.sessions[0].prompt.as_ref().unwrap().spans, vec![[0, 3]]);
        assert!(listed.more);
        assert_eq!(listed.failed(), None, "a listing with sessions is no failure");
        let empty: Listed =
            serde_json::from_value(serde_json::json!({ "failures": [{ "words": "unreadable" }] }))
                .unwrap();
        assert_eq!(empty.failed().as_deref(), Some("unreadable"));
    }

    /// The asked words stay lit after the line is made one line and marked where it was cut, and
    /// a span off a character boundary is dropped.
    #[test]
    fn the_asked_words_stay_lit_on_one_line() {
        let prompt = PromptLine {
            text: "fix\nthe é build".to_owned(),
            spans: vec![[4, 7], [9, 10], [0, 99]],
            cut_before: true,
            cut_after: true,
        };
        let (line, ranges) = prompt_line(&prompt);
        assert_eq!(line, "\u{2026}fix the é build\u{2026}");
        assert_eq!(ranges.len(), 1, "{ranges:?}");
        assert_eq!(&line[ranges[0].clone()], "the");
    }

    /// History's height is its rows' and lines' exactly, so the group it closes never clips it.
    #[test]
    fn history_stands_as_tall_as_what_it_shows() {
        assert_eq!(history_height(None), HEADER_HEIGHT);
        let mut history = History { open: true, ..History::default() };
        assert_eq!(history_height(Some(&history)), HEADER_HEIGHT + BODY_INSET + LINE_HEIGHT);
        let mut branched = row("b");
        branched.branch = Some("main".to_owned());
        let mut chat = row("c");
        chat.chat_id = Some("chat".to_owned());
        history.load = Load::Listed(Listed {
            sessions: vec![row("a"), branched, chat],
            more: true,
            ..Listed::default()
        });
        assert_eq!(
            history_height(Some(&history)),
            HEADER_HEIGHT + BODY_INSET + ROW_HEIGHT + ROW_BRANCH_HEIGHT + LINE_HEIGHT + 2.0 * GAP,
            "a chat here is no row; Show more is a line"
        );
        history.load = Load::Listed(Listed::default());
        assert_eq!(history_height(Some(&history)), HEADER_HEIGHT + BODY_INSET + LINE_HEIGHT);
        history.load = Load::Listed(Listed {
            sessions: vec![row("a")],
            failures: vec![Failure { words: "Laptop is out of reach".to_owned(), ..Failure::default() }],
            ..Listed::default()
        });
        assert_eq!(
            history_height(Some(&history)),
            HEADER_HEIGHT + BODY_INSET + ROW_HEIGHT + LINE_HEIGHT + GAP,
            "a machine that failed beside the rows says so in a line"
        );
    }

    /// A session held by a live Claude Code elsewhere says so, naming its machine.
    #[test]
    fn a_session_held_elsewhere_says_where() {
        let mut past = row("a");
        assert_eq!(past.held_elsewhere(), None);
        past.running = Some("interactive".to_owned());
        past.device = Some("Studio".to_owned());
        let said = past.held_elsewhere().unwrap();
        assert!(said.contains("in a terminal on Studio"), "{said}");
        assert_eq!(past.running_words(), Some("In a terminal"));
    }
}
