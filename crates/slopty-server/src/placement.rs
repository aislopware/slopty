//! Where a task's start goes (`docs/decisions/projects.md`): on the worker its orchestrator
//! names, and otherwise the least busy worker, beside a clone of the project's repository when
//! the task named no directory and one has a clone. Wherever it goes, the worker is online and
//! has the agent installed, named or not. The orchestrator reads every worker's facts with
//! `list_workers` and names the worker itself; nothing here judges rules over them.

use slopty_core::WorkerId;
use slopty_proto::project::{Fact, Facts};

/// Most items one list or map fact holds, as a worker reports it; the rest are dropped.
pub(crate) const FACT_ITEMS_MAX: usize = 1024;
/// Most facts one worker reports, counting those inside lists and maps.
pub(crate) const FACTS_MAX: usize = 4096;
/// Longest text fact, in bytes; a longer one is cut.
pub(crate) const FACT_TEXT_MAX: usize = 4096;
/// Most bytes of one worker's facts, names and texts together; what is past it is dropped.
/// It keeps every worker's facts within one link frame, many workers to an answer.
pub(crate) const FACTS_BYTES_MAX: usize = 64 * 1024;

/// A worker as placement sees it.
#[derive(Clone, Debug)]
pub(crate) struct Candidate {
    /// Which.
    pub worker: WorkerId,
    /// Its name.
    pub name: String,
    /// Whether it is online now.
    pub online: bool,
    /// Whether it has reported its own facts yet: until it has, only the agents it registered
    /// with are known to be installed.
    pub reported: bool,
    /// Its facts, the server's and its own.
    pub facts: Facts,
    /// How many agents run on it in all, and are being started there.
    pub fleet_live: u16,
    /// Whether it has a clone of the project's repository.
    pub clone: bool,
}

/// An agent a worker must have installed, as its facts name it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct Installed {
    /// The facts it is under: `agents` by its program, `acp` by the registry's name.
    pub map: &'static str,
    /// Its name there.
    pub name: String,
}

impl Installed {
    /// The agent whose program is `program`, under `agents`.
    pub(crate) fn program(program: &str) -> Self {
        Self { map: "agents", name: program.to_owned() }
    }

    /// `agent`, by the thread model's id: an ACP agent under `acp` by the registry's name,
    /// Claude Code as `claude`, any other by its own name under `agents`.
    pub(crate) fn of(agent: &slopty_proto::thread::AgentId) -> Self {
        use slopty_proto::thread::AgentId;
        match agent.acp_name() {
            Some(name) => Self { map: "acp", name: name.to_owned() },
            None if agent.is(AgentId::CLAUDE_CODE) => Self::program("claude"),
            None => Self::program(&agent.0),
        }
    }
}

/// What a start asks of the worker it goes to.
#[derive(Clone, Debug, Default)]
pub(crate) struct Wanted {
    /// The worker the orchestrator or the person named.
    pub pin: Option<WorkerId>,
    /// The agent it runs, as a worker's facts name it: a worker that does not report it
    /// installed does not fit, named or not.
    pub agent: Option<Installed>,
    /// Whether a worker with a clone of the project's repository goes first.
    pub clone: bool,
}

/// The worker to start on: the named one when it fits, and otherwise, of those that fit, one
/// with a clone when one is wanted, then the one running the fewest agents, then the least
/// busy, then by name.
///
/// # Errors
/// Why no worker fits, in words, naming each with why.
pub(crate) fn choose(candidates: &[Candidate], wanted: &Wanted) -> Result<WorkerId, String> {
    let named = |c: &&Candidate| wanted.pin.is_none_or(|pin| pin == c.worker);
    let best =
        candidates.iter().filter(named).filter(|c| unfit(c, wanted).is_none()).min_by(|a, b| {
            (wanted.clone && b.clone)
                .cmp(&(wanted.clone && a.clone))
                .then(a.fleet_live.cmp(&b.fleet_live))
                .then(busy(&a.facts).total_cmp(&busy(&b.facts)))
                .then_with(|| a.name.cmp(&b.name))
                .then_with(|| a.worker.cmp(&b.worker))
        });
    if let Some(best) = best {
        return Ok(best.worker);
    }
    if let Some(pin) = wanted.pin
        && !candidates.iter().any(|c| c.worker == pin)
    {
        return Err(format!("it is pinned to worker {pin}, which the server does not know"));
    }
    if candidates.is_empty() {
        return Err("the server knows no worker".to_owned());
    }
    let why: Vec<String> = candidates
        .iter()
        .filter(named)
        .filter_map(|c| Some(format!("{}: {}", c.name, unfit(c, wanted)?)))
        .collect();
    Err(why.join("; "))
}

/// Why `c` cannot take the start; `None` when it can.
fn unfit(c: &Candidate, wanted: &Wanted) -> Option<String> {
    if !c.online {
        return Some("not online".to_owned());
    }
    let Installed { map, name: agent } = wanted.agent.as_ref()?;
    let has = matches!(c.facts.get(*map), Some(Fact::Map(installed))
        if installed.contains_key(agent));
    match (has, c.reported) {
        (true, _) => None,
        (false, true) => Some(format!("{agent} is not installed")),
        (false, false) => Some(format!("has not said yet whether {agent} is installed")),
    }
}

/// A worker's facts as the server keeps them: lists and maps cut to [`FACT_ITEMS_MAX`] items,
/// texts to [`FACT_TEXT_MAX`] bytes, and [`FACTS_MAX`] facts and [`FACTS_BYTES_MAX`] bytes in
/// all, so every worker's facts fit a frame. What is past a bound is dropped, in name order.
pub(crate) fn bounded(facts: Facts) -> Facts {
    let mut budget = Budget { facts: FACTS_MAX, bytes: FACTS_BYTES_MAX };
    facts
        .into_iter()
        .map_while(|(name, fact)| {
            budget.take_bytes(name.len())?;
            Some((name, cut(fact, &mut budget)?))
        })
        .collect()
}

/// What is left of a worker's bounds on its facts.
struct Budget {
    facts: usize,
    bytes: usize,
}

impl Budget {
    fn take_bytes(&mut self, n: usize) -> Option<()> {
        self.bytes = self.bytes.checked_sub(n.saturating_add(8))?;
        Some(())
    }
}

fn cut(fact: Fact, budget: &mut Budget) -> Option<Fact> {
    budget.facts = budget.facts.checked_sub(1)?;
    Some(match fact {
        Fact::Text(t) => {
            let mut end = t.len().min(FACT_TEXT_MAX);
            while !t.is_char_boundary(end) {
                end = end.saturating_sub(1);
            }
            budget.take_bytes(end)?;
            Fact::Text(t.get(..end).unwrap_or_default().to_owned())
        }
        Fact::List(items) => Fact::List(
            items.into_iter().take(FACT_ITEMS_MAX).map_while(|f| cut(f, budget)).collect(),
        ),
        Fact::Map(parts) => Fact::Map(
            parts
                .into_iter()
                .take(FACT_ITEMS_MAX)
                .map_while(|(k, f)| {
                    budget.take_bytes(k.len())?;
                    Some((k, cut(f, budget)?))
                })
                .collect(),
        ),
        other => {
            budget.take_bytes(0)?;
            other
        }
    })
}

/// Load per cpu, for a tie between otherwise equal workers; unknown sorts last.
fn busy(facts: &Facts) -> f64 {
    let load = match facts.get("load") {
        Some(Fact::Float(l)) => *l,
        #[expect(clippy::cast_precision_loss, reason = "a load average is small")]
        Some(Fact::Int(l)) => *l as f64,
        _ => return f64::MAX,
    };
    let cpus = match facts.get("cpus") {
        #[expect(clippy::cast_precision_loss, reason = "a cpu count is small")]
        Some(Fact::Int(n)) if *n > 0 => *n as f64,
        _ => 1.0,
    };
    load / cpus
}

#[cfg(test)]
mod tests;
