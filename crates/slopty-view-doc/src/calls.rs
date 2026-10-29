//! slopty: the calls Slopty's view model answers beyond zeron's (`slopty_view_rpc::methods`),
//! for the dock, the tray, the transcript's own actions and its pages. Each names its
//! parameters and its answer; a chat is named by `chatId` as zeron's calls name it.
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

/// slopty: the terminal each session is (`docs/decisions/ui.md`, "A session is its terminal"):
/// `{}` → a watch of `{ chatId: terminalId }`, an agent's session by the terminal its TUI runs
/// in now, and a plain terminal (`terminal:<id>`) by its own.
pub const WATCH_SESSION_TERMINALS: &str = "slopty.WatchSessionTerminals";

/// slopty: start a session in a folder: `{ deviceId, cwd, agent }`, `agent` an agent's id
/// (`claude-code`, `codex`, `pi`) for its own TUI, or `terminal` for a shell → `{ chatId }` once
/// its machine started it, or the machine's words for why not.
pub const START_SESSION: &str = "slopty.StartSession";

/// slopty: what [`START_SESSION`] names a plain shell by, beside the agents' ids.
pub const TERMINAL_AGENT: &str = "terminal";
