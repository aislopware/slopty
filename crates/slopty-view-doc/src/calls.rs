//! slopty: the calls Slopty's view model answers beyond zeron's (`slopty_view_rpc::methods`),
//! for the dock, the tray, the transcript's own actions and its pages. Each names its
//! parameters and its answer; a chat is named by `chatId` as zeron's calls name it.
//!
//! Every verb is the agent's own, through its door: a control the chat's agent cannot do is not
//! offered ([`crate::dock::DockCan`]), and one asked anyway is answered with why it failed.

/// The chat's background work: `{ chatId }` → a watch of [`crate::work::WorkTray`].
pub const WATCH_WORK: &str = "slopty.WatchWork";

/// The chat's dock: `{ chatId }` → a watch of [`crate::dock::ChatDock`].
pub const WATCH_DOCK: &str = "slopty.WatchDock";

/// An agent's permission rules: `{ chatId }`, or `{ deviceId, cwd? }` for the newest thread on
/// that machine (in that folder) whose agent reads them → a watch of
/// [`crate::rules::PermissionRules`].
pub const WATCH_RULES: &str = "slopty.WatchRules";

/// Stop one row of the tray: `{ chatId, rowId }` (a [`crate::work::WorkRow::id`]) → `{}` once
/// the agent took it, or why not.
pub const STOP_WORK: &str = "slopty.StopWork";

/// Stop every live row of the tray, one stop each: `{ chatId }` → `{ stopped }`, how many were
/// asked.
pub const STOP_ALL_WORK: &str = "slopty.StopAllWork";

/// Move a running call to the background, the turn going on past it: `{ chatId, partId? }` (the
/// call's tool part; every blocking call when absent) → `{}`.
pub const BACKGROUND: &str = "slopty.Background";

/// Run a `!` command in the chat's folder on its machine; its output goes to the agent as the
/// person's next message: `{ chatId, command }` → `{ commandId }`.
pub const RUN_SHELL: &str = "slopty.RunShell";

/// Ask the chat's agent a side question (`/btw`): answered from the conversation so far, without
/// tools, never entering it and never stopping the turn under way: `{ chatId, question }` → `{}`.
/// The question and its answer come in the dock ([`crate::dock::ChatDock::side`]).
pub const ASK_SIDE: &str = "slopty.AskSide";

/// Go back to before a message of the person's: `{ chatId, messageId, code, conversation }`.
/// `code` puts the files back as they were before that message's turn; `conversation` branches
/// a new chat sharing the turns before it. → `{ prompt, chatId?, undo?, noUndo? }`: the
/// message's words, for the composer; the new chat when one was branched; and what undoes the
/// rewind ([`crate::dock::RewindUndo`]), for the dock's "Undo rewind" toast, when all of it can
/// be: the files the worker kept before a code rewind, the branched chat. A code rewind whose
/// files were not kept has no undo, and `noUndo` says why.
pub const REWIND: &str = "slopty.Rewind";

/// Undo a rewind: `{ undo }` (the [`crate::dock::RewindUndo`] the rewind answered with) →
/// `{ chatId }`, the chat to show again, once its folder is put back as it was before the
/// rewind. Then the branched chat is deleted, as its own delete does; the chat it branched from
/// kept its conversation. Refused, deleting nothing, when a later rewind or turn came since: only
/// the last undoes.
pub const UNDO_REWIND: &str = "slopty.UndoRewind";

/// Add or remove a permission rule: `{ chatId, add, behavior, rule, scope }` (a
/// [`crate::rules::RuleScope::id`]) → `{}`.
pub const CHANGE_RULE: &str = "slopty.ChangeRule";

/// Allow an approval once and add a rule, as edited, so its like is allowed from now on:
/// `{ chatId, requestId, rule, scope }` → `{}`.
pub const ALLOW_WITH_RULE: &str = "slopty.AllowWithRule";

/// The person acted in a mod's surface: `{ chatId, surfaceId, element, event, value? }`, `event`
/// one of `press`, `input`, `select`, `close` → `{}`.
pub const SURFACE_EVENT: &str = "slopty.SurfaceEvent";

/// Clear the chat's goal (`/goal clear`): `{ chatId }` → `{ commandId }`.
pub const CLEAR_GOAL: &str = "slopty.ClearGoal";

/// Switch the chat's permission mode: `{ chatId, mode }` (a
/// [`crate::dock::ModeChoice::id`]) → `{}`.
pub const SET_MODE: &str = "slopty.SetMode";

/// How a page stands and what it asks: `{ page }` (a [`crate::page_card::PageCard::page`]) → a
/// watch of [`crate::page_card::PageState`].
pub const WATCH_PAGE: &str = "slopty.WatchPage";

/// The person's answer to what a page asked: `{ page, request, allow }` → `{}`.
pub const ANSWER_PAGE: &str = "slopty.AnswerPage";

/// Show a page in the right pane, or back inline: `{ page, to }`, `to` `pane` or `inline` →
/// `{}`. The page keeps its web view and its state; it is laid out to fill the pane
/// ([`crate::page_card::PageState::display_mode`] says `pane`), and the card draws it there.
pub const MOVE_PAGE: &str = "slopty.MovePage";

/// Save a page's document in its chat's folder on its machine, as `{title}-{call}.html`:
/// `{ page }` → the write's outcome, as `WriteWorkspaceFile` answers it (its `file.path`).
pub const SAVE_PAGE: &str = "slopty.SavePage";

/// Copy a page's last picture to the clipboard as a PNG: `{ page }` → `{ width, height }`, in
/// pixels; refused while it has none.
pub const COPY_PAGE_IMAGE: &str = "slopty.CopyPageImage";

/// Let a page reach the network its app declared: `{ page }` → `{}`.
pub const GRANT_PAGE_NETWORK: &str = "slopty.GrantPageNetwork";
