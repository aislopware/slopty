// Derived from zeron (MIT, github.com/zeronsh/zeron @ c02739d), see THIRD_PARTY_NOTICES.md
//! Workspace lifecycle types shared by the engine and its clients.

use serde::{Deserialize, Serialize};

/// The fixed data boundary selected when an engine runtime is assembled.
///
/// Authentication can change while a runtime is alive, but its workspace scope
/// cannot. Switching scopes requires assembling a new runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkspaceScope {
    /// This device's own data.
    Local,
    /// An account's data, synced between its devices.
    Synced,
    /// A development account's data.
    Development,
}

/// Stable information about the engine runtime reached by a client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineInfo {
    /// The device the runtime runs on.
    pub device_id: String,
    /// The data it holds.
    pub workspace_scope: WorkspaceScope,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_scope_uses_wire_safe_names() {
        for (scope, encoded) in [
            (WorkspaceScope::Local, "\"local\""),
            (WorkspaceScope::Synced, "\"synced\""),
            (WorkspaceScope::Development, "\"development\""),
        ] {
            assert_eq!(serde_json::to_string(&scope).unwrap(), encoded);
            assert_eq!(serde_json::from_str::<WorkspaceScope>(encoded).unwrap(), scope);
        }
    }

    #[test]
    fn engine_info_uses_camel_case_fields() {
        let info =
            EngineInfo { device_id: "device-1".into(), workspace_scope: WorkspaceScope::Local };
        assert_eq!(
            serde_json::to_value(&info).unwrap(),
            serde_json::json!({ "deviceId": "device-1", "workspaceScope": "local" })
        );
    }
}
