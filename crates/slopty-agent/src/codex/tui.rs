//! Codex run as its own TUI in a terminal (`docs/decisions/agents.md`, "TUI-first"), with
//! Slopty's tools through its config door: `-c mcp_servers.slopty.…` for that run alone.

use serde_json::Value;

use super::RESUME;

/// What a start chose for Codex, each only when named.
#[derive(Clone, Copy, Debug, Default)]
pub struct Choices<'a> {
    /// The model (`-m`).
    pub model: Option<&'a str>,
    /// The approval policy (`-a`): `untrusted`, `on-request`, `never`.
    pub approval: Option<&'a str>,
    /// The reasoning effort (`model_reasoning_effort`).
    pub effort: Option<&'a str>,
}

/// The name Slopty's tools go by: their tools are `mcp__slopty__…`.
const TOOLS: &str = "slopty";

/// What of the terminal's environment Slopty's tools need: the terminal they speak as, its
/// proof, the server and the worker.
const TOOL_ENV: [&str; 4] = [
    slopty_proto::ctl::SESSION_ENV,
    slopty_proto::ctl::SESSION_TOKEN_ENV,
    slopty_proto::ctl::SERVER_ENV,
    "SLOPTY_WORKER_SOCKET",
];

/// The arguments of Codex's TUI, its first prompt last, after `--`.
///
/// Thread `resumed` is taken up again (`codex resume <id>`), else a new one starts; the start's
/// `choices` follow, then Slopty's tools through `<cli> mcp` with the terminal's own variables
/// `env` when `tools` names them.
#[must_use]
pub fn tui(
    resumed: Option<&str>,
    choices: &Choices<'_>,
    tools: Option<(&str, &[(String, String)])>,
    prompt: Option<&str>,
) -> Vec<String> {
    let mut args = Vec::new();
    if let Some(id) = resumed {
        args.extend([RESUME.to_owned(), id.to_owned()]);
    }
    let given = |v: Option<&str>| v.map(str::trim).filter(|v| !v.is_empty()).map(str::to_owned);
    if let Some(model) = given(choices.model) {
        args.extend(["-m".to_owned(), model]);
    }
    if let Some(approval) = given(choices.approval) {
        args.extend(["-a".to_owned(), approval]);
    }
    if let Some(effort) = given(choices.effort) {
        args.extend(config("model_reasoning_effort", &Value::String(effort)));
    }
    if let Some((cli, env)) = tools {
        // Codex hands an MCP server only the variables its configuration names.
        let vars: serde_json::Map<String, Value> = env
            .iter()
            .filter(|(name, _)| TOOL_ENV.contains(&name.as_str()))
            .map(|(n, v)| (n.clone(), Value::String(v.clone())))
            .collect();
        args.extend(config(
            &format!("mcp_servers.{TOOLS}.command"),
            &Value::String(cli.to_owned()),
        ));
        args.extend(config(&format!("mcp_servers.{TOOLS}.args"), &serde_json::json!(["mcp"])));
        args.extend(config(&format!("mcp_servers.{TOOLS}.env"), &Value::Object(vars)));
    }
    if let Some(prompt) = prompt.filter(|p| !p.trim().is_empty()) {
        args.extend(["--".to_owned(), prompt.to_owned()]);
    }
    args
}

/// `-c key=value`, the value in TOML.
fn config(key: &str, value: &Value) -> [String; 2] {
    ["-c".to_owned(), format!("{key}={}", toml(value))]
}

/// `value` as TOML: a JSON string is a TOML basic string, an array the same, and an object an
/// inline table of bare keys (the variables' names are letters, digits and `_`).
fn toml(value: &Value) -> String {
    match value {
        Value::Array(items) => {
            format!("[{}]", items.iter().map(toml).collect::<Vec<_>>().join(", "))
        }
        Value::Object(fields) => {
            let fields: Vec<String> =
                fields.iter().map(|(key, value)| format!("{key} = {}", toml(value))).collect();
            format!("{{ {} }}", fields.join(", "))
        }
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Codex TUI takes the choices made, Slopty's tools as TOML its `-c` reads, the terminal's
    /// session among their variables and nothing else of its environment, and its prompt last.
    #[test]
    fn codex_runs_with_slopty_tools_through_its_config() {
        let env = vec![
            ("SLOPTY_SESSION".to_owned(), "s1".to_owned()),
            ("HOME".to_owned(), "/h".to_owned()),
        ];
        let choices = Choices { model: Some("gpt-5"), approval: Some("never"), effort: None };
        let args = tui(None, &choices, Some(("/bin/slopty", &env)), Some("-fix it"));
        assert_eq!(&args[..4], ["-m", "gpt-5", "-a", "never"]);
        assert!(
            args.contains(&r#"mcp_servers.slopty.command="/bin/slopty""#.to_owned()),
            "{args:?}"
        );
        assert!(args.contains(&r#"mcp_servers.slopty.args=["mcp"]"#.to_owned()), "{args:?}");
        assert!(
            args.contains(&r#"mcp_servers.slopty.env={ SLOPTY_SESSION = "s1" }"#.to_owned()),
            "{args:?}"
        );
        assert!(args.ends_with(&["--".to_owned(), "-fix it".to_owned()]));
        assert_eq!(tui(Some("019a"), &Choices::default(), None, None), ["resume", "019a"]);
    }
}
