// Derived from zeron (MIT, github.com/zeronsh/zeron @ 053ce2e), see THIRD_PARTY_NOTICES.md
//! The browser's live preview list: the ports a session's machine serves that reach this client.
use serde::{Deserialize, Serialize};

/// One served port a browser tab can open.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PreviewService {
    /// Stable while the port is served: the machine and the port, never a process ID.
    pub id: String,
    /// The serving machine's name, shown beside its port.
    pub device_name: String,
    /// Where this client reaches the port: its own forward, `localhost:<local>`.
    pub hostname: String,
    /// The process listening on the port.
    pub name: String,
    /// The port on the serving machine.
    pub port: u16,
}

impl PreviewService {
    /// The address a browser tab loads for this preview.
    #[must_use]
    pub fn url(&self) -> String {
        format!("http://{}", self.hostname)
    }
}

/// What a session's browser tab can preview, sent again whenever it changes.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PreviewSnapshot {
    /// The served ports, by port.
    pub services: Vec<PreviewService>,
    /// Whether the ports are a worker's forwards; false until the first list arrives.
    #[serde(default)]
    pub remote: bool,
}
