//! Where the daemon keeps things.

use std::path::PathBuf;

use anyhow::{Context as _, Result};
use slopty_core::WorkerId;

/// `$SLOPTY_DATA_DIR`, else `~/Library/Application Support/Slopty`.
pub fn data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("SLOPTY_DATA_DIR") {
        return PathBuf::from(dir);
    }
    let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("/tmp"), PathBuf::from);
    home.join("Library").join("Application Support").join("Slopty")
}

/// `$SLOPTY_WORKER_SOCKET`, else `$TMPDIR/slopty/worker.sock`.
pub fn ctl_socket() -> PathBuf {
    if let Some(p) = std::env::var_os("SLOPTY_WORKER_SOCKET") {
        return PathBuf::from(p);
    }
    std::env::temp_dir().join("slopty").join("worker.sock")
}

/// The stable worker id, created on first run.
pub fn worker_id(data_dir: &std::path::Path) -> Result<WorkerId> {
    let path = data_dir.join("worker-id");
    if let Ok(text) = std::fs::read_to_string(&path) {
        let uuid = text.trim().parse().with_context(|| format!("parse {}", path.display()))?;
        return Ok(WorkerId::from_uuid(uuid));
    }
    let id = WorkerId::new();
    std::fs::write(&path, id.as_uuid().to_string())
        .with_context(|| format!("write {}", path.display()))?;
    Ok(id)
}

/// `$SLOPTY_WORKER_NAME`, else the machine's computer name (two daemons on one Mac, as in a
/// test, would otherwise be indistinguishable in a client's worker switcher).
pub fn worker_name() -> String {
    if let Some(name) = std::env::var_os("SLOPTY_WORKER_NAME") {
        let name = name.to_string_lossy().trim().to_owned();
        if !name.is_empty() {
            return name;
        }
    }
    std::process::Command::new("scutil")
        .args(["--get", "ComputerName"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "worker".to_owned())
}
