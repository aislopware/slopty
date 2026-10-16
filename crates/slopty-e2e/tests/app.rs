//! The real app, driven from inside: the shell on a real stack, its live surfaces and settings,
//! rendered with the app's own renderer and compared with the goldens.
//!
//! Every test is live (`#[ignore]`, run by `cargo xtask e2e app`), since it launches the app. Only
//! the cases in `screen_recording` need a permission from the machine; they run with
//! `--screen-recording`.

#[cfg(test)]
#[path = "app/clipboard.rs"]
mod clipboard;

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
#[path = "app/pair.rs"]
mod pair;

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
#[path = "app/surfaces_files.rs"]
mod surfaces_files;

#[cfg(test)]
#[path = "app/surfaces_live.rs"]
mod surfaces_live;

#[cfg(test)]
#[path = "app/transcript.rs"]
mod transcript;
