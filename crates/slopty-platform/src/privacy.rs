//! The panes of System Settings ▸ Privacy & Security a worker needs switched on, opened where
//! the person switches them.
//!
//! Permissions belong to the process that asks, and on a worker that is `slopty-worker` under
//! launchd, not the app: the person turns on `slopty-worker` in each pane
//! (`docs/decisions/platform.md`, "This Mac as a worker").

/// A pane of Privacy & Security.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pane {
    /// Screen & System Audio Recording: windows and the desktop can be streamed.
    ScreenRecording,
    /// Accessibility: a remote window's clicks and keys are delivered.
    Accessibility,
}

/// Open `pane` in System Settings. macOS only; nothing happens elsewhere.
pub fn open(pane: Pane) {
    // `x-apple.systempreferences:` names the Security & Privacy extension and, after `?`, the
    // anchor of the list to show.
    let url = match pane {
        Pane::ScreenRecording => {
            "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture"
        }
        Pane::Accessibility => {
            "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility"
        }
    };
    if cfg!(target_os = "macos") {
        crate::open_url(url);
    }
}
