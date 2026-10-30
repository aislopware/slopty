//! The calls Slopty's view model answers beyond `slopty_view_rpc::methods`.
//!
//! They serve the dock, the tray, the transcript's own actions and its pages. Each names its
//! parameters and its answer; a chat is named by `chatId`, as in `slopty_view_rpc::methods`.
//!
//! None of them asks an agent anything: an agent's TUI answers in its own terminal, so these
//! read what a session did and drive the pages it showed.

/// The chat's background work: `{ chatId }` → a watch of [`crate::work::WorkTray`].
pub const WATCH_WORK: &str = "slopty.WatchWork";

/// The chat's dock: `{ chatId }` → a watch of [`crate::dock::ChatDock`].
pub const WATCH_DOCK: &str = "slopty.WatchDock";

/// How a page stands and what it asks: `{ page }` (a [`crate::page_card::PageCard::page`]) → a
/// watch of [`crate::page_card::PageState`].
pub const WATCH_PAGE: &str = "slopty.WatchPage";

/// The person's answer to what a page asked: `{ page, request, allow }` → `{}`.
pub const ANSWER_PAGE: &str = "slopty.AnswerPage";

/// Show a page in the right pane, or back inline: `{ page, to }` → `{}`.
///
/// `to` is `pane` or `inline`. The page keeps its web view and its state; it is laid out to fill
/// the pane ([`crate::page_card::PageState::display_mode`] says `pane`), and the card draws it
/// there.
pub const MOVE_PAGE: &str = "slopty.MovePage";

/// Save a page's document in its chat's folder on its machine, as `{title}-{call}.html`:
/// `{ page }` → the write's outcome, as `WriteWorkspaceFile` answers it (its `file.path`).
pub const SAVE_PAGE: &str = "slopty.SavePage";

/// Copy a page's last picture to the clipboard as a PNG: `{ page }` → `{ width, height }`, in
/// pixels; refused while it has none.
pub const COPY_PAGE_IMAGE: &str = "slopty.CopyPageImage";

/// Let a page reach the network its app declared: `{ page }` → `{}`.
pub const GRANT_PAGE_NETWORK: &str = "slopty.GrantPageNetwork";

/// The terminal each session is: `{}` → a watch of `{ chatId: terminalId }`.
///
/// An agent's session names the terminal its TUI runs in now, and a plain terminal
/// (`terminal:<id>`) its own (`docs/decisions/ui.md`, "A session is its terminal").
pub const WATCH_SESSION_TERMINALS: &str = "slopty.WatchSessionTerminals";

/// Start a session in a folder: `{ deviceId, cwd, agent }` → `{ chatId }`.
///
/// `agent` is an agent's id (`claude-code`, `codex`, `pi`) for its own TUI, or `terminal` for a
/// shell. The answer comes once its machine started it, or with the machine's words for why
/// not.
pub const START_SESSION: &str = "slopty.StartSession";

/// What [`START_SESSION`] names a plain shell by, beside the agents' ids.
pub const TERMINAL_AGENT: &str = "terminal";
