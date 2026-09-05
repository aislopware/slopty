//! Where the daemon keeps things.

use std::path::PathBuf;

use anyhow::{Context as _, Result};
use slopty_core::HostId;

/// `$SLOPTY_DATA_DIR`, else `~/Library/Application Support/Slopty`.
pub fn data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("SLOPTY_DATA_DIR") {
        return PathBuf::from(dir);
    }
    let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("/tmp"), PathBuf::from);
    home.join("Library").join("Application Support").join("Slopty")
}

/// `$SLOPTY_HOSTD_SOCKET`, else `$TMPDIR/slopty/hostd.sock`.
pub fn ctl_socket() -> PathBuf {
    if let Some(p) = std::env::var_os("SLOPTY_HOSTD_SOCKET") {
        return PathBuf::from(p);
    }
    std::env::temp_dir().join("slopty").join("hostd.sock")
}

/// The stable host id, created on first run.
pub fn host_id(data_dir: &std::path::Path) -> Result<HostId> {
    let path = data_dir.join("host-id");
    if let Ok(text) = std::fs::read_to_string(&path) {
        let uuid = text.trim().parse().with_context(|| format!("parse {}", path.display()))?;
        return Ok(HostId::from_uuid(uuid));
    }
    let id = HostId::new();
    std::fs::write(&path, id.as_uuid().to_string())
        .with_context(|| format!("write {}", path.display()))?;
    Ok(id)
}

/// `$SLOPTY_HOST_NAME`, else the machine's computer name (two daemons on one Mac, as in a
/// test, would otherwise be indistinguishable in a client's host switcher).
pub fn host_name() -> String {
    if let Some(name) = std::env::var_os("SLOPTY_HOST_NAME") {
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
        .unwrap_or_else(|| "host".to_owned())
}
