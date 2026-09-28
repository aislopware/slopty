//! Where the daemon keeps things.

use std::path::PathBuf;

use anyhow::{Context as _, Result};
use slopty_core::WorkerId;

/// `$SLOPTY_WORKER_SOCKET`, else `worker.sock` in the platform's socket directory
/// (`slopty_platform::dirs::runtime_dir`: `$TMPDIR/slopty` on macOS).
pub fn ctl_socket() -> PathBuf {
    std::env::var_os("SLOPTY_WORKER_SOCKET")
        .map_or_else(|| slopty_platform::dirs::runtime_dir().join("worker.sock"), PathBuf::from)
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

/// `$SLOPTY_WORKER_NAME`, else the machine's name (`slopty_platform::computer_name`); two
/// daemons on one machine, as in a test, would otherwise be indistinguishable in a client's
/// worker switcher.
pub fn worker_name() -> String {
    if let Some(name) = std::env::var_os("SLOPTY_WORKER_NAME") {
        let name = name.to_string_lossy().trim().to_owned();
        if !name.is_empty() {
            return name;
        }
    }
    slopty_platform::computer_name().unwrap_or_else(|| "worker".to_owned())
}
