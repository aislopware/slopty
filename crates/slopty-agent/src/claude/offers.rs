//! What a Claude Code thread can be started with: the models, permission modes and efforts it
//! takes, and the mode a new session starts in on this machine.

use std::path::Path;

use serde_json::Value;
use slopty_proto::thread::{Effort, Mode, Model, Offers};

/// The modes `--permission-mode` takes (`claude --help`, 2.1.283). Manual's config value,
/// `default`, is none of them: a session in it is started without the flag.
const PERMISSION_MODES: [&str; 6] =
    ["acceptEdits", "auto", "bypassPermissions", "manual", "dontAsk", "plan"];

/// Whether a start may ask for permission mode `mode`: one `--permission-mode` takes.
///
/// Skipping every permission is among them: how freely an agent acts is the person's setting
/// (`docs/decisions/agents.md`, "How freely an agent acts").
#[must_use]
pub fn startable_mode(mode: &str) -> bool {
    PERMISSION_MODES.contains(&mode)
}

/// The levels `--effort` takes (`claude --help`, 2.1.291), each with its name for people.
const EFFORTS: [(&str, &str); 5] = [
    ("low", "Low"),
    ("medium", "Medium"),
    ("high", "High"),
    ("xhigh", "Extra high"),
    ("max", "Max"),
];

/// Whether a start may ask for effort `effort`: a level `--effort` takes.
#[must_use]
pub fn startable_effort(effort: &str) -> bool {
    EFFORTS.iter().any(|(id, _)| *id == effort)
}

/// What a new Claude Code thread can be started with before any ran here.
///
/// The models `/model` takes by alias ([`crate::claude::history::MODELS`]), its own default
/// among them ([`DEFAULT_MODEL`]), the modes a start may ask for ([`startable_mode`]) and the
/// levels `--effort` takes. Its commands come from a thread that ran.
#[must_use]
pub fn offers() -> Offers {
    let mode = |id: &str| {
        let (label, description) = match id {
            "acceptEdits" => ("Accept edits", Some("Edits files without asking")),
            "auto" => ("Auto", Some("Asks only about what looks risky")),
            "dontAsk" => ("Don't ask", Some("Denies what is not allowed beforehand")),
            "plan" => ("Plan", Some("Plans first and changes nothing")),
            "manual" => ("Manual", Some("Asks before edits, commands and the network")),
            "bypassPermissions" => ("Bypass permissions", Some("Asks nothing")),
            other => (other, None),
        };
        Mode {
            id: id.to_owned(),
            label: label.to_owned(),
            description: description.map(str::to_owned),
        }
    };
    Offers {
        models: crate::claude::history::MODELS
            .iter()
            .map(|(id, label)| Model {
                id: (*id).to_owned(),
                label: (*label).to_owned(),
                options: Vec::new(),
            })
            .collect(),
        model: Some(DEFAULT_MODEL.to_owned()),
        modes: PERMISSION_MODES.iter().filter(|m| startable_mode(m)).map(|m| mode(m)).collect(),
        // The machine's settings say it ([`starting_mode`]).
        mode: None,
        efforts: EFFORTS
            .iter()
            .map(|(id, label)| Effort {
                id: (*id).to_owned(),
                label: (*label).to_owned(),
                description: None,
            })
            .collect(),
        commands: Vec::new(),
    }
}

/// Claude Code's name for the model it runs on unless one is chosen: the one its account is
/// given, which `/model` lists first as recommended.
pub const DEFAULT_MODEL: &str = "default";

/// The built-in mode a session starts in when nothing chooses one: Claude Code 2.1.283 on, in
/// a terminal (code.claude.com/docs/en/permission-modes, "Which mode a session starts in").
const BUILT_IN_MODE: &str = "auto";

/// The mode a new session on this machine starts in when its start asks for none, by its id in
/// [`offers`].
///
/// That is `permissions.defaultMode` from the managed settings, else from the person's
/// `~/.claude/settings.json` under `home`, else the built-in auto mode. Auto turned
/// off (`disableAutoMode`) starts it in Manual instead.
///
/// A folder's own `.claude/settings.json` can choose another, short of `auto`; the machine
/// cannot say for every folder.
#[must_use]
pub fn starting_mode(home: &Path) -> String {
    starting_mode_under(&crate::managed::ManagedSettings::read(home), home)
}

/// [`starting_mode`] under the `managed` settings, with the person's own read under `home`.
fn starting_mode_under(managed: &crate::managed::ManagedSettings, home: &Path) -> String {
    let mut user = crate::managed::ManagedSettings::default();
    let doc = std::fs::read_to_string(home.join(".claude").join("settings.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok());
    if let Some(doc) = doc {
        user.add(&doc);
    }
    starting_mode_of(managed, &user)
}

/// [`starting_mode`] from what the `managed` and the person's `user` settings say.
fn starting_mode_of(
    managed: &crate::managed::ManagedSettings,
    user: &crate::managed::ManagedSettings,
) -> String {
    let set = managed.default_mode.as_deref().or(user.default_mode.as_deref());
    // Manual's config value is `default`; `--permission-mode` and the offers name it `manual`.
    let mode = match set {
        Some("default") => "manual",
        Some(mode) if PERMISSION_MODES.contains(&mode) => mode,
        _ => BUILT_IN_MODE,
    };
    if mode == "auto" && (managed.disable_auto_mode || user.disable_auto_mode) {
        "manual".to_owned()
    } else {
        mode.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_offered_mode_says_what_it_does() {
        let modes = offers().modes;
        let manual = modes.iter().find(|m| m.id == "manual").expect("Manual is offered");
        assert_eq!(manual.label, "Manual");
        assert!(modes.iter().all(|m| m.description.is_some()), "{modes:?}");
    }

    /// A start that asks for no mode begins where the settings say, the managed ones first,
    /// else in auto mode; `default` is Manual, and auto turned off starts it in Manual.
    #[test]
    fn a_start_with_no_mode_begins_where_the_settings_say() {
        use crate::managed::ManagedSettings;
        let of = |doc: Value| {
            let mut settings = ManagedSettings::default();
            settings.add(&doc);
            settings
        };
        let none = ManagedSettings::default();
        assert_eq!(starting_mode_of(&none, &none), "auto", "the built-in default");
        let plan = of(serde_json::json!({ "permissions": { "defaultMode": "plan" } }));
        let manual = of(serde_json::json!({ "permissions": { "defaultMode": "default" } }));
        assert_eq!(starting_mode_of(&none, &plan), "plan", "the person's own");
        assert_eq!(starting_mode_of(&none, &manual), "manual");
        assert_eq!(starting_mode_of(&manual, &plan), "manual", "the managed one wins");
        let unknown = of(serde_json::json!({ "permissions": { "defaultMode": "yolo" } }));
        assert_eq!(starting_mode_of(&none, &unknown), "auto", "not a mode");
        let off = of(serde_json::json!({ "permissions": { "disableAutoMode": "disable" } }));
        assert_eq!(starting_mode_of(&off, &none), "manual", "auto turned off");
        assert_eq!(starting_mode_of(&off, &plan), "plan");

        let home = tempfile::tempdir().expect("a home");
        let claude = home.path().join(".claude");
        std::fs::create_dir_all(&claude).expect("~/.claude");
        assert_eq!(starting_mode_under(&none, home.path()), "auto", "no settings file");
        std::fs::write(claude.join("settings.json"), r#"{"permissions":{"defaultMode":"plan"}}"#)
            .expect("settings");
        assert_eq!(starting_mode_under(&none, home.path()), "plan", "the person's settings");
    }
}
