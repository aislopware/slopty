//! A live Claude Code's record of itself, `<config>/sessions/<pid>.json`, as the worker reads it
//! (`slopty_agent::claude::sessions::live`).
//!
//! Any Claude Code on the machine writes one, and rewrites it as what it does changes: its
//! session, folder, status and what a wait is for, in its own words. The input is the file's
//! bytes. What must hold, beside no panic: a record read names a process and a session Claude
//! Code would name, says how it runs, and a wait it records reads as one with words to show,
//! of one of the kinds a wait is.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use slopty_agent::claude::sessions::{CONFIG_DIR_ENV, LIVE_DIR, live};

/// The wait kinds a record's wait may be said as (`slopty_proto::thread::Wait`).
const KINDS: [&str; 3] = ["permission", "question", "input"];

/// Run one input.
pub fn run(data: &[u8]) {
    // With Claude Code's own directory moved, `live` reads that one, which may be the
    // person's: nothing is written there, and this target has no record to read.
    if std::env::var_os(CONFIG_DIR_ENV).is_some() {
        return;
    }
    let home = home();
    let record = home.join(".claude").join(LIVE_DIR).join("4242.json");
    std::fs::write(&record, data).expect("a record written");
    for found in live(home) {
        assert!(found.pid > 0, "a record of pid 0");
        assert!(slopty_agent::claude::is_session_id(&found.session), "{:?}", found.session);
        assert!(!found.kind.is_empty(), "a record that runs no way");
        for said in [&found.version, &found.name, &found.status].into_iter().flatten() {
            assert!(!said.trim().is_empty(), "a blank word kept: {said:?}");
        }
        let waiting = found.waiting();
        assert_eq!(waiting.is_some(), found.status.as_deref() == Some("waiting"), "{found:?}");
        if let Some(wait) = waiting {
            assert!(KINDS.contains(&wait.kind.as_str()), "a wait of kind {:?}", wait.kind);
            assert!(!wait.text.trim().is_empty(), "a wait with no words: {wait:?}");
        }
    }
}

/// A home of this process's own, its Claude Code's sessions directory made once.
fn home() -> &'static Path {
    static HOME: OnceLock<PathBuf> = OnceLock::new();
    HOME.get_or_init(|| {
        let home =
            std::env::temp_dir().join(format!("slopty-fuzz-registry-{}", std::process::id()));
        std::fs::create_dir_all(home.join(".claude").join(LIVE_DIR)).expect("a sessions directory");
        home
    })
}
