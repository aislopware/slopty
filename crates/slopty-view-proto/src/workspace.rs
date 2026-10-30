// Derived from zeron (MIT, github.com/zeronsh/zeron @ c02739d), see THIRD_PARTY_NOTICES.md
//! Workspace lifecycle types shared by the engine and its clients.

use serde::{Deserialize, Serialize};

/// The data an engine runtime holds. Slopty has no accounts, so it is always this device's own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkspaceScope {
    /// This device's own data.
    Local,
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
        let scope = WorkspaceScope::Local;
        assert_eq!(serde_json::to_string(&scope).unwrap(), "\"local\"");
        assert_eq!(serde_json::from_str::<WorkspaceScope>("\"local\"").unwrap(), scope);
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
