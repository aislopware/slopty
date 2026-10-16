use serde_json::{Value, json};
use slopty_proto::thread::{Rule, RuleChange};

use super::{answers_bang, change_rule, setting, status_command, status_line};

fn change(add: bool, behavior: &str, rule: &str, scope: &str) -> RuleChange {
    RuleChange {
        add,
        behavior: behavior.to_owned(),
        rule: rule.to_owned(),
        scope: scope.to_owned(),
    }
}

/// A rule goes into its scope's file and out again, every other member of the file kept; a
/// change Claude Code does not take is refused and writes nothing.
#[test]
fn a_rule_goes_into_its_scopes_file_and_out_again() {
    let (home, cwd) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let local = cwd.path().join(".claude/settings.local.json");
    std::fs::create_dir_all(local.parent().unwrap()).unwrap();
    std::fs::write(&local, r#"{"model":"opus","permissions":{"allow":["Read"]}}"#).unwrap();
    let rule = "Bash(cargo test:*)";
    change_rule(home.path(), cwd.path(), &change(true, Rule::ALLOW, rule, Rule::LOCAL)).unwrap();
    change_rule(home.path(), cwd.path(), &change(true, Rule::ALLOW, rule, Rule::LOCAL)).unwrap();
    let doc: Value = serde_json::from_str(&std::fs::read_to_string(&local).unwrap()).unwrap();
    assert_eq!(doc["permissions"]["allow"], json!(["Read", rule]), "added once");
    assert_eq!(doc["model"], "opus", "the rest kept");
    change_rule(home.path(), cwd.path(), &change(false, Rule::ALLOW, rule, Rule::LOCAL)).unwrap();
    let doc: Value = serde_json::from_str(&std::fs::read_to_string(&local).unwrap()).unwrap();
    assert_eq!(doc["permissions"]["allow"], json!(["Read"]), "removed");
    change_rule(home.path(), cwd.path(), &change(true, Rule::DENY, "Bash(rm:*)", Rule::USER))
        .unwrap();
    let user = std::fs::read_to_string(home.path().join(".claude/settings.json")).unwrap();
    let user: Value = serde_json::from_str(&user).unwrap();
    assert_eq!(user["permissions"]["deny"], json!(["Bash(rm:*)"]), "the person's own file");
    for refused in [
        change(true, "maybe", rule, Rule::LOCAL),
        change(true, Rule::ALLOW, rule, "policySettings"),
        change(true, Rule::ALLOW, " ", Rule::LOCAL),
    ] {
        change_rule(home.path(), cwd.path(), &refused).unwrap_err();
    }
    std::fs::write(&local, "not json").unwrap();
    change_rule(home.path(), cwd.path(), &change(true, Rule::ASK, rule, Rule::LOCAL)).unwrap_err();
    assert_eq!(std::fs::read_to_string(&local).unwrap(), "not json", "a broken file is left");
}

/// A setting is read from the most local scope that sets it.
#[test]
fn settings_are_read_most_local_first() {
    let (home, cwd) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    assert!(answers_bang(home.path(), cwd.path()), "on unless set off");
    std::fs::create_dir_all(home.path().join(".claude")).unwrap();
    std::fs::write(home.path().join(".claude/settings.json"), r#"{"respondToBashCommands":false}"#)
        .unwrap();
    assert!(!answers_bang(home.path(), cwd.path()));
    std::fs::create_dir_all(cwd.path().join(".claude")).unwrap();
    std::fs::write(
        cwd.path().join(".claude/settings.json"),
        r#"{"respondToBashCommands":true,"statusLine":{"type":"command","command":"echo hi"}}"#,
    )
    .unwrap();
    assert!(answers_bang(home.path(), cwd.path()), "the project's over the person's");
    assert_eq!(status_command(home.path(), cwd.path()).as_deref(), Some("echo hi"));
    assert_eq!(setting(home.path(), cwd.path(), "nothing"), None);
}

/// The status line command reads its JSON on stdin and its words, colour kept, are the line; one
/// that fails says nothing.
#[tokio::test]
async fn the_status_line_reads_its_json_and_says_its_line() {
    let cwd = tempfile::tempdir().unwrap();
    let script = cwd.path().join("line.sh");
    std::fs::write(
        &script,
        "#!/bin/sh\nread input\nmodel=$(printf '%s' \"$input\" | sed 's/.*\"id\":\"\\([^\"]*\\)\".*/\\1/')\nprintf '\\033[32m%s\\033[0m\\n' \"$model\"\n",
    )
    .unwrap();
    let input = json!({ "model": { "id": "claude-opus-5-5" } });
    let said = status_line(&format!("sh {}", script.display()), &input, cwd.path()).await;
    assert_eq!(said.as_deref(), Some("\u{1b}[32mclaude-opus-5-5\u{1b}[0m"));
    assert_eq!(status_line("exit 3", &input, cwd.path()).await, None, "a failure says nothing");
}
