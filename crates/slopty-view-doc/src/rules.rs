//! slopty: an agent's permission rules, as Settings → Permissions and an approval's "always
//! allow" edit them (Slopty's own; not in zeron).
//!
//! Served on `slopty.WatchRules` ([`crate::calls::WATCH_RULES`]) for a chat, or for a folder on
//! a machine (the newest thread there whose agent reads its rules); changed by
//! `slopty.ChangeRule`. The agent's own settings files hold them, with its own precedence: the
//! worker writes the person's (user), the project's (project, shared with the repository) and
//! this machine's for the project (local), and never anything else.

use serde::{Deserialize, Serialize};

/// An agent's permission rules.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionRules {
    /// The chat they were read through, which `slopty.ChangeRule` may name.
    pub chat_id: String,
    /// `false` until the agent has said them.
    pub known: bool,
    /// The rules, in the agent's order.
    pub rules: Vec<PermissionRuleRow>,
    /// The folders it may work in beside its own.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub directories: Vec<String>,
    /// The scopes a rule can be added to, the narrowest first.
    pub scopes: Vec<RuleScope>,
}

/// One permission rule.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionRuleRow {
    /// `allow`, `deny` or `ask`.
    pub behavior: String,
    /// The rule as the agent writes it (`Bash(cargo test:*)`).
    pub rule: String,
    /// Where it comes from, as the agent names it (`localSettings`, `session`, `cliArg`).
    pub source: String,
    /// Where it comes from, for people ("This project, this machine", "This session").
    pub source_label: String,
    /// What it means, when the agent says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// It can be removed from here: it lives in one of [`PermissionRules::scopes`].
    pub editable: bool,
    /// It is in effect, not overridden by another.
    pub in_effect: bool,
}

/// A settings scope a rule can be written to.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleScope {
    /// The agent's name for it, as `slopty.ChangeRule`'s `scope` takes it.
    pub id: String,
    /// For people ("This project, this machine").
    pub label: String,
    /// The file it is, relative to the project or the home folder.
    pub file: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rules travel in the shell's words, camel-cased.
    #[test]
    fn rules_travel_camel_cased() {
        let rules = PermissionRules {
            chat_id: "c".into(),
            known: true,
            rules: vec![PermissionRuleRow {
                behavior: "allow".into(),
                rule: "Bash(cargo test:*)".into(),
                source: "localSettings".into(),
                source_label: "This project, this machine".into(),
                description: None,
                editable: true,
                in_effect: true,
            }],
            directories: Vec::new(),
            scopes: Vec::new(),
        };
        let json = serde_json::to_value(&rules).expect("serializes");
        assert_eq!(json["rules"][0]["sourceLabel"], "This project, this machine");
        assert_eq!(json["rules"][0]["inEffect"], true);
        let back: PermissionRules = serde_json::from_value(json).expect("reads back");
        assert_eq!(back, rules);
    }
}
