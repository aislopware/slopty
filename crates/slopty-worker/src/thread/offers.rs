//! What a new thread of each agent can be started with here ([`Offers`]), for the agents a
//! worker lists ([`slopty_proto::server::InstalledAgent::offers`]).
//!
//! A client composes a thread's first message before the thread exists, with the same model,
//! mode and effort menus a running thread's composer has. Each list is the newest thread's of
//! the agent that published one, since the agent's own catalogue moves with its version and the
//! person's account; where no thread here ever published it, it is what the agent's adapter
//! knows without asking the agent ([`seed`]).

use std::collections::{BTreeMap, HashMap};

use slopty_proto::thread::{AgentId, Offers, ThreadId};

use super::Host;

/// What `agent`'s adapter knows a start of it can choose before any thread of it ran.
///
/// Claude Code's models, modes and efforts from its CLI, with the mode its settings here start
/// a session in; Codex's approval policies; nothing for the agents that say theirs only once a
/// session is open.
#[must_use]
pub fn seed(agent: &AgentId) -> Offers {
    if agent.is(AgentId::CLAUDE_CODE) {
        let home = slopty_platform::dirs::home();
        Offers {
            mode: Some(slopty_agent::resume::starting_mode(&home)),
            ..slopty_agent::resume::offers()
        }
    } else if agent.is(AgentId::CODEX) {
        slopty_agent::codex::shared::offers()
    } else {
        Offers::default()
    }
}

/// Which of a thread's lists are not empty, and when it began.
#[derive(Clone, Copy)]
struct Held {
    thread: ThreadId,
    created_ms: u64,
    lists: [bool; 4],
}

/// What the newest thread of each agent held by `host` published, list by list: an agent's
/// models from the newest thread that has any, its modes from the newest that has any, and so on.
#[must_use]
pub fn seen(host: &Host) -> BTreeMap<AgentId, Offers> {
    let held = host.visit(|state| {
        let meta = &state.meta;
        let lists = [
            !meta.models.is_empty(),
            !meta.modes.is_empty(),
            !meta.efforts.is_empty(),
            !state.commands.is_empty(),
        ];
        lists.contains(&true).then(|| {
            let held = Held { thread: meta.id, created_ms: meta.created_ms.as_millis(), lists };
            (meta.agent.clone(), held)
        })
    });
    // The newest thread for each list of each agent.
    let mut newest: BTreeMap<AgentId, [Option<Held>; 4]> = BTreeMap::new();
    for (agent, held) in held {
        let slots = newest.entry(agent).or_default();
        for (slot, has) in slots.iter_mut().zip(held.lists) {
            if has && slot.is_none_or(|was| was.created_ms < held.created_ms) {
                *slot = Some(held);
            }
        }
    }
    let wanted: HashMap<ThreadId, Vec<(AgentId, usize)>> =
        newest.iter().fold(HashMap::new(), |mut wanted, (agent, slots)| {
            for (list, slot) in slots.iter().enumerate() {
                if let Some(held) = slot {
                    wanted.entry(held.thread).or_default().push((agent.clone(), list));
                }
            }
            wanted
        });
    let mut seen: BTreeMap<AgentId, Offers> = BTreeMap::new();
    let lists = host.visit(|state| {
        let lists = wanted.get(&state.meta.id)?;
        Some(
            lists
                .iter()
                .map(|(agent, list)| {
                    let mut offers = Offers::default();
                    match list {
                        0 => offers.models.clone_from(&state.meta.models),
                        1 => offers.modes.clone_from(&state.meta.modes),
                        2 => offers.efforts.clone_from(&state.meta.efforts),
                        _ => offers.commands.clone_from(&state.commands),
                    }
                    (agent.clone(), offers)
                })
                .collect::<Vec<_>>(),
        )
    });
    for (agent, offers) in lists.into_iter().flatten() {
        let into = seen.entry(agent).or_default();
        into.models.extend(offers.models);
        into.modes.extend(offers.modes);
        into.efforts.extend(offers.efforts);
        into.commands.extend(offers.commands);
    }
    seen
}

/// `seed` with each list `seen` has in place of its own. The mode a start begins in stays the
/// seed's: a thread that ran says which mode it was in, not which a new one starts in.
#[must_use]
pub fn merged(seed: Offers, seen: Option<&Offers>) -> Offers {
    let Some(seen) = seen else { return seed };
    Offers {
        models: pick(seed.models, &seen.models),
        modes: pick(seed.modes, &seen.modes),
        mode: seed.mode,
        efforts: pick(seed.efforts, &seen.efforts),
        commands: pick(seed.commands, &seen.commands),
    }
}

/// `seen` where it says anything, else `own`.
fn pick<T: Clone>(own: Vec<T>, seen: &[T]) -> Vec<T> {
    if seen.is_empty() { own } else { seen.to_vec() }
}

#[cfg(test)]
mod tests {
    use slopty_core::WallMs;
    use slopty_proto::thread::{Action, Command, Drive, Effort, Model, ThreadMeta};

    use super::*;
    use crate::thread::log::Limits;

    fn meta(agent: &str, created: u64, models: &[&str], efforts: &[&str]) -> ThreadMeta {
        ThreadMeta {
            id: ThreadId::new(),
            agent: AgentId::named(agent),
            agent_version: String::new(),
            native: format!("{agent}-{created}"),
            cwd: "/w".to_owned(),
            title: String::new(),
            terminal: None,
            parent: None,
            origin: ThreadMeta::PERSON.to_owned(),
            forked_from: None,
            drive: Drive::named(Drive::SHARED),
            caps: Vec::new(),
            models: models
                .iter()
                .map(|m| Model { id: (*m).to_owned(), label: (*m).to_owned() })
                .collect(),
            modes: Vec::new(),
            efforts: efforts
                .iter()
                .map(|e| Effort { id: (*e).to_owned(), label: (*e).to_owned(), description: None })
                .collect(),
            facts: BTreeMap::new(),
            created_ms: WallMs::from_millis(created),
        }
    }

    /// Each list a start offers is the newest thread's of the agent that has one, list by list,
    /// over what its adapter knows beforehand: Codex's newest models with an older thread's
    /// efforts and its own approval policies, Claude Code's own modes and efforts with the
    /// models and commands its thread heard; an agent no thread here ran keeps its seed.
    #[test]
    fn a_start_offers_the_newest_lists_over_what_the_adapter_knows() {
        let dir = tempfile::tempdir().unwrap();
        let host = Host::open(dir.path(), Limits::default()).unwrap();
        let claude = meta(AgentId::CLAUDE_CODE, 3, &["opus[1m]"], &[]);
        for meta in [
            meta(AgentId::CODEX, 1, &["gpt-a"], &["low"]),
            meta(AgentId::CODEX, 2, &["gpt-b"], &[]),
            claude.clone(),
        ] {
            host.create(meta).unwrap();
        }
        let review = Command {
            name: "code-review".to_owned(),
            description: String::new(),
            argument_hint: None,
            source: "built-in".to_owned(),
        };
        host.apply(claude.id, vec![Action::CommandsSet(vec![review.clone()])]);

        let seen = seen(&host);
        let codex = AgentId::named(AgentId::CODEX);
        let offered = merged(seed(&codex), seen.get(&codex));
        let ids = |models: &[Model]| models.iter().map(|m| m.id.clone()).collect::<Vec<_>>();
        assert_eq!(ids(&offered.models), ["gpt-b"]);
        assert_eq!(offered.efforts.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), ["low"]);
        assert_eq!(offered.modes, seed(&codex).modes);

        let code = AgentId::named(AgentId::CLAUDE_CODE);
        let offered = merged(seed(&code), seen.get(&code));
        assert_eq!(ids(&offered.models), ["opus[1m]"]);
        assert_eq!((offered.modes.clone(), offered.commands), (seed(&code).modes, vec![review]));
        assert!(offered.modes.iter().any(|m| m.id == "plan"), "{:?}", offered.modes);
        assert_eq!(offered.efforts, seed(&code).efforts);

        let pi = AgentId::named(AgentId::PI);
        assert_eq!(merged(seed(&pi), seen.get(&pi)), Offers::default());
    }
}
