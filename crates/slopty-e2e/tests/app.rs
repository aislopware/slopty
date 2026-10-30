//! The real app, driven from inside: the shell on a real stack, its live surfaces and settings,
//! rendered with the app's own renderer and compared with the goldens.
//!
//! Every test is live (`#[ignore]`, run by `cargo xtask e2e app`), since it launches the app. None
//! needs a permission from the machine: a remote window streams from the worker's drawn screen
//! (`SLOPTY_SYNTHETIC_SCREEN`). Cases in a `screen_recording` module would, and run with
//! `--screen-recording`.

#[cfg(test)]
#[path = "app/agent_ui.rs"]
mod agent_ui;

#[cfg(test)]
#[path = "app/clipboard.rs"]
mod clipboard;

#[cfg(test)]
#[path = "app/conversation.rs"]
mod conversation;

#[cfg(test)]
#[path = "app/devices.rs"]
mod devices;

#[cfg(test)]
#[path = "app/hangs.rs"]
mod hangs;

#[cfg(test)]
#[path = "app/interface.rs"]
mod interface;

#[cfg(test)]
#[path = "app/liveness.rs"]
mod liveness;

#[cfg(test)]
#[path = "app/media.rs"]
mod media;

#[cfg(test)]
#[path = "app/pair.rs"]
mod pair;

#[cfg(test)]
#[path = "app/past_sessions.rs"]
mod past_sessions;

#[cfg(test)]
#[path = "app/planted.rs"]
mod planted;

#[cfg(test)]
#[path = "app/prompt_box.rs"]
mod prompt_box;

#[cfg(test)]
#[path = "app/resume.rs"]
mod resume;

#[cfg(test)]
#[path = "app/terminal_keys.rs"]
mod terminal_keys;

#[cfg(test)]
#[path = "app/through_server.rs"]
mod through_server;

#[cfg(test)]
#[path = "app/served.rs"]
mod served;

#[cfg(test)]
#[path = "app/session.rs"]
mod session;

#[cfg(test)]
#[path = "app/sidebar_paging.rs"]
mod sidebar_paging;

#[cfg(test)]
#[path = "app/surfaces_files.rs"]
mod surfaces_files;

#[cfg(test)]
#[path = "app/surfaces_live.rs"]
mod surfaces_live;

#[cfg(test)]
#[path = "app/transcript.rs"]
mod transcript;

#[cfg(test)]
#[path = "app/walk.rs"]
mod walk;
