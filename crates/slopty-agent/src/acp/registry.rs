//! The ACP agents a worker can run, by name: an open list.
//!
//! The known ones are here, each with the command line that serves ACP on stdio, as the public
//! ACP registry (`agentclientprotocol/registry`) launches it, but run as the person's own
//! installed program rather than fetched. The person adds their own, or puts a known one on
//! another command line, in their settings (`[worker.acp]`, a name and its command line); an
//! empty command line takes a known one away. Claude Code, Codex and pi are not here: each has
//! an adapter of its own over a richer protocol than ACP.

use std::collections::BTreeMap;

/// An ACP agent: its name, and the command line that serves ACP on stdio.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Agent {
    /// Its name, as its thread's agent is named (`acp:<name>`).
    pub name: String,
    /// The program, looked for on the `PATH` when it has no directory.
    pub program: String,
    /// What it is started with.
    pub args: Vec<String>,
}

/// A known agent, as [`KNOWN`] lists it.
#[derive(Clone, Copy, Debug)]
pub struct Known {
    /// Its name: the registry's id.
    pub name: &'static str,
    /// Its program, as its package installs it.
    pub program: &'static str,
    /// What makes it serve ACP on stdio.
    pub args: &'static [&'static str],
}

impl Known {
    const fn new(name: &'static str, program: &'static str, args: &'static [&'static str]) -> Self {
        Self { name, program, args }
    }
}

/// The agents known without being named in the settings, by the registry's id.
pub const KNOWN: [Known; 26] = [
    Known::new("amp-acp", "amp-acp", &[]),
    Known::new("auggie", "auggie", &["--acp"]),
    Known::new("cline", "cline", &["--acp"]),
    Known::new("codebuddy-code", "codebuddy", &["--acp"]),
    Known::new("cortex-code", "cortex", &["acp", "serve"]),
    Known::new("crow-cli", "crow-cli", &["acp"]),
    Known::new("cursor", "cursor-agent", &["acp"]),
    Known::new("deepagents", "deepagents-acp", &[]),
    Known::new("devin", "devin", &["acp"]),
    Known::new("dimcode", "dimcode", &["acp"]),
    Known::new("dirac", "dirac", &["--acp"]),
    Known::new("factory-droid", "droid", &["exec", "--output-format", "acp-daemon"]),
    Known::new("gemini", "gemini", &["--acp"]),
    Known::new("github-copilot-cli", "copilot", &["--acp"]),
    Known::new("goose", "goose", &["acp"]),
    Known::new("grok-build", "grok", &["agent", "stdio"]),
    Known::new("harn", "harn", &["serve", "acp"]),
    Known::new("kilo", "kilo", &["acp"]),
    Known::new("kimchi", "kimchi", &["--mode", "acp"]),
    Known::new("kimi", "kimi", &["acp"]),
    Known::new("mistral-vibe", "vibe-acp", &[]),
    Known::new("opencode", "opencode", &["acp"]),
    Known::new("qoder", "qodercli", &["--acp"]),
    Known::new("qwen-code", "qwen", &["--acp"]),
    Known::new("stakpak", "stakpak", &["acp"]),
    Known::new("vtcode", "vtcode", &["acp"]),
];

impl From<&Known> for Agent {
    fn from(known: &Known) -> Self {
        Self {
            name: known.name.to_owned(),
            program: known.program.to_owned(),
            args: known.args.iter().map(|&a| a.to_owned()).collect(),
        }
    }
}

/// Every agent: the person's own (`own`, a name and its command line), then the known ones they
/// do not name, sorted by name. A command line that is empty takes its name away.
#[must_use]
pub fn registry(own: &BTreeMap<String, Vec<String>>) -> Vec<Agent> {
    let mut agents: BTreeMap<String, Agent> =
        KNOWN.iter().map(|known| (known.name.to_owned(), Agent::from(known))).collect();
    for (name, line) in own {
        match line.split_first() {
            Some((program, args)) if !name.is_empty() && !program.is_empty() => {
                let agent =
                    Agent { name: name.clone(), program: program.clone(), args: args.to_vec() };
                agents.insert(name.clone(), agent);
            }
            _ => {
                agents.remove(name);
            }
        }
    }
    agents.into_values().collect()
}

/// The agent named `name`, among the person's own (`own`) and the known ones.
#[must_use]
pub fn find(name: &str, own: &BTreeMap<String, Vec<String>>) -> Option<Agent> {
    registry(own).into_iter().find(|agent| agent.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_persons_own_agents_add_to_replace_and_take_away_known_ones() {
        let own = BTreeMap::from([
            ("mine".to_owned(), vec!["/opt/mine/bin/agent".to_owned(), "--stdio".to_owned()]),
            ("gemini".to_owned(), vec!["gemini".to_owned(), "--experimental-acp".to_owned()]),
            ("goose".to_owned(), Vec::new()),
        ]);
        let agents = registry(&own);
        let mine = agents.iter().find(|a| a.name == "mine").unwrap();
        assert_eq!(
            (mine.program.as_str(), mine.args.as_slice()),
            ("/opt/mine/bin/agent", &["--stdio".to_owned()][..])
        );
        assert_eq!(find("gemini", &own).unwrap().args, ["--experimental-acp"]);
        assert_eq!(find("goose", &own), None, "an empty command line takes it away");
        assert_eq!(find("opencode", &own).unwrap().args, ["acp"]);
        assert_eq!(agents.len(), KNOWN.len(), "one added, one taken away");
        assert!(agents.windows(2).all(|w| w[0].name < w[1].name), "sorted, each once");
    }

    #[test]
    fn the_known_agents_are_named_once_and_none_has_its_own_adapter() {
        let mut names: Vec<&str> = KNOWN.iter().map(|k| k.name).collect();
        names.dedup();
        assert_eq!(names.len(), KNOWN.len());
        assert!(names.windows(2).all(|w| w[0] < w[1]), "kept sorted");
        for native in ["claude-acp", "codex-acp", "pi-acp"] {
            assert!(!names.contains(&native), "{native} has an adapter of its own");
        }
    }
}
