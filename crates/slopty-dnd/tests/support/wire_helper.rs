//! The worker's drag helper itself, `slopty_dnd::helper`, as a test app: what the daemon runs as
//! `slopty-worker dnd`, spoken to over its stdin and stdout by `tests/roles.rs`.

#[cfg(target_os = "macos")]
fn main() -> std::process::ExitCode {
    slopty_dnd::helper::run()
}

#[cfg(not(target_os = "macos"))]
fn main() {}
