//! The starts made on this device (`starts.json` in the client's data).
//!
//! It keeps the last start that went, and what each agent's last start chose on its chips.
//! They are the person's own defaults, kept apart from the layout, so a relaunch begins where the
//! last run left off: the steps list the last agent, machine and folder first, a new worktree first
//! when the last start made one, and a draft's chips begin where that agent's last start set them.
//!
//! A chip begins only on what the machine offers the agent now ([`Starts::seed`]): a model
//! another machine has, or one since gone, leaves the chip at the agent's default.

use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};
use slopty_core::WallMs;
use slopty_proto::thread::{AgentId, Offers};

use crate::layout::WorkerKey;

/// The file under the client's data directory.
pub const FILE: &str = "starts.json";

/// The starts made here.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct Starts {
    /// The last start that went.
    last: Option<LastStart>,
    /// Each agent's chips as its last start went, one entry per agent.
    chosen: Vec<Chosen>,
}

/// A start that went: its agent, its machine, its folder, and whether in a new worktree of it.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct LastStart {
    /// Its agent.
    pub agent: AgentId,
    /// Its machine.
    pub worker: WorkerKey,
    /// Its folder: where the agent stood, or the one its new worktree was made from.
    pub cwd: String,
    /// In a new worktree of that folder's repository.
    pub worktree: bool,
    /// When it went, by this device's clock.
    pub at: WallMs,
}

/// A draft's chips, by the agent's own ids; the agent's default where `None`.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct Chips {
    /// The model.
    pub model: Option<String>,
    /// The permission mode.
    pub mode: Option<String>,
    /// The effort.
    pub effort: Option<String>,
}

/// One agent's chips as its last start went.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
struct Chosen {
    agent: AgentId,
    chips: Chips,
}

impl Starts {
    /// The last start that went.
    #[must_use]
    pub const fn last(&self) -> Option<&LastStart> {
        self.last.as_ref()
    }

    /// `start` went, with `chips` when it was drafted: it is the last start, and `chips` are
    /// its agent's from now on. A start with no draft (a project's orchestrator) leaves the
    /// agent's chips as they were.
    pub fn went(&mut self, start: LastStart, chips: Option<Chips>) {
        if let Some(chips) = chips {
            let agent = start.agent.clone();
            match self.chosen.iter_mut().find(|c| c.agent == agent) {
                Some(chosen) => chosen.chips = chips,
                None => self.chosen.push(Chosen { agent, chips }),
            }
        }
        self.last = Some(start);
    }

    /// `agent`'s chips as its last start went.
    #[must_use]
    pub fn chips(&self, agent: &AgentId) -> Option<&Chips> {
        self.chosen.iter().find(|c| c.agent == *agent).map(|c| &c.chips)
    }

    /// Where a draft of `agent` begins its chips, on a machine that `offers` what it does: each
    /// as the agent's last start set it, where the machine offers that choice; else its
    /// default.
    #[must_use]
    pub fn seed(&self, agent: &AgentId, offers: &Offers) -> Chips {
        let Some(last) = self.chips(agent) else { return Chips::default() };
        Chips {
            model: offered(last.model.as_deref(), offers.models.iter().map(|m| m.id.as_str())),
            mode: offered(last.mode.as_deref(), offers.modes.iter().map(|m| m.id.as_str())),
            effort: offered(last.effort.as_deref(), offers.efforts.iter().map(|e| e.id.as_str())),
        }
    }

    /// The starts kept at `path`: none when there is no file. One that does not read (another
    /// build's, or broken) is said and dropped; the next start writes it again.
    #[must_use]
    pub fn read(path: &Path) -> Self {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Self::default(),
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "starts read");
                return Self::default();
            }
        };
        serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            tracing::warn!(path = %path.display(), error = %e, "starts parse: dropped");
            Self::default()
        })
    }

    /// Keep them at `path`, whole (`slopty_platform::fs::replace`), so a crash leaves the old
    /// file or the new one.
    ///
    /// # Errors
    ///
    /// The write's own, or the serialiser's.
    pub fn write(&self, path: &Path) -> io::Result<()> {
        let bytes = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        slopty_platform::fs::replace(path, &bytes)
    }
}

/// `choice`, when it is one of the `ids` offered.
fn offered<'a>(choice: Option<&str>, mut ids: impl Iterator<Item = &'a str>) -> Option<String> {
    choice.filter(|c| ids.any(|id| id == *c)).map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use slopty_proto::thread::{Effort, Mode, Model};

    use super::*;

    fn claude() -> AgentId {
        AgentId::named(AgentId::CLAUDE_CODE)
    }

    fn start(worktree: bool) -> LastStart {
        LastStart {
            agent: claude(),
            worker: WorkerKey::new(7),
            cwd: "/w/atlas".to_owned(),
            worktree,
            at: WallMs::from_millis(1_000),
        }
    }

    fn chips(model: &str, mode: &str, effort: &str) -> Chips {
        Chips {
            model: Some(model.to_owned()),
            mode: Some(mode.to_owned()),
            effort: Some(effort.to_owned()),
        }
    }

    /// What a start keeps comes back whole from the file, and no file reads as no starts; a
    /// file that does not read is dropped rather than failing the app.
    #[test]
    fn the_starts_come_back_from_their_file() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join(FILE);
        assert_eq!(Starts::read(&path), Starts::default(), "no file, no starts");

        let mut starts = Starts::default();
        starts.went(start(true), Some(chips("opus", "plan", "high")));
        starts.write(&path).expect("written");
        let back = Starts::read(&path);
        assert_eq!(back, starts);
        assert_eq!(back.last().map(|l| l.worktree), Some(true), "the worktree choice too");

        std::fs::write(&path, b"{").expect("broken");
        assert_eq!(Starts::read(&path), Starts::default(), "broken, dropped");
    }

    /// A draft begins on the agent's last chips only where the machine offers them now; a
    /// start with no draft changes the last start and leaves the chips.
    #[test]
    fn a_draft_begins_on_the_last_chips_the_machine_offers() {
        let mut starts = Starts::default();
        let offers = Offers {
            models: vec![Model { id: "opus".to_owned(), label: "Opus".to_owned() }],
            modes: vec![Mode {
                id: "plan".to_owned(),
                label: "Plan".to_owned(),
                description: None,
            }],
            mode: None,
            efforts: vec![Effort {
                id: "low".to_owned(),
                label: "Low".to_owned(),
                description: None,
            }],
            commands: Vec::new(),
        };
        assert_eq!(starts.seed(&claude(), &offers), Chips::default(), "no start, defaults");

        starts.went(start(false), Some(chips("opus", "plan", "high")));
        let seeded = starts.seed(&claude(), &offers);
        assert_eq!(
            seeded,
            Chips { model: Some("opus".to_owned()), mode: Some("plan".to_owned()), effort: None },
            "an effort the machine does not offer stays at the default"
        );
        let codex = AgentId::named(AgentId::CODEX);
        assert_eq!(starts.seed(&codex, &offers), Chips::default(), "each agent its own");
        assert_eq!(starts.seed(&claude(), &Offers::default()), Chips::default(), "none offered");

        let orchestrator = LastStart { cwd: "/w/other".to_owned(), ..start(false) };
        starts.went(orchestrator, None);
        assert_eq!(starts.last().map(|l| l.cwd.as_str()), Some("/w/other"));
        assert_eq!(starts.chips(&claude()), Some(&chips("opus", "plan", "high")), "kept");
    }
}
