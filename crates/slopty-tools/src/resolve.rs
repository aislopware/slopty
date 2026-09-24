//! Turning what a person or a model types into ids: a worker by id, name or id prefix, and a
//! terminal as `worker/session` with a session-id prefix, or a session alone.
//!
//! Full ids cost no round trip; anything else asks for the directory or the terminal list, the
//! directory at most once per resolver.

use slopty_core::{SessionId, WorkerId};
use slopty_proto::orchestration::{ErrorCode, Outcome, TermRef, Verb};
use slopty_proto::server::{Liveness, WorkerInfo};
use slopty_proto::terminal::SessionSummary;

use crate::{Dispatch, ToolError};

/// Resolves names against one dispatch, remembering the directory it fetched.
#[derive(Debug)]
pub struct Resolver<'a, D> {
    dispatch: &'a D,
    workers: Option<Vec<WorkerInfo>>,
}

impl<'a, D: Dispatch> Resolver<'a, D> {
    /// A resolver on `dispatch`.
    pub const fn new(dispatch: &'a D) -> Self {
        Self { dispatch, workers: None }
    }

    /// Where it resolves.
    pub const fn dispatch(&self) -> &'a D {
        self.dispatch
    }

    /// The directory, fetched once.
    pub async fn workers(&mut self) -> Result<&[WorkerInfo], ToolError> {
        if self.workers.is_none() {
            match self.dispatch.call(Verb::ListWorkers).await {
                Outcome::Workers(list) => self.workers = Some(list),
                other => return Err(ToolError::unexpected(other)),
            }
        }
        Ok(self.workers.as_deref().unwrap_or_default())
    }

    /// Terminals, on one worker or all.
    pub async fn terminals(
        &self,
        worker: Option<WorkerId>,
    ) -> Result<Vec<(WorkerId, SessionSummary)>, ToolError> {
        match self.dispatch.call(Verb::ListTerminals { worker }).await {
            Outcome::Terminals(list) => Ok(list),
            other => Err(ToolError::unexpected(other)),
        }
    }

    /// A worker: its id, its name, a unique id prefix, or, when `needle` is `None`, the only
    /// worker online.
    pub async fn worker(&mut self, needle: Option<&str>) -> Result<WorkerId, ToolError> {
        if let Some(id) = needle.and_then(|n| n.trim().parse::<WorkerId>().ok()) {
            return Ok(id);
        }
        pick_worker(self.workers().await?, needle)
    }

    /// A worker when `needle` names one; `None` stays `None` (every worker).
    pub async fn some_worker(
        &mut self,
        needle: Option<&str>,
    ) -> Result<Option<WorkerId>, ToolError> {
        match needle {
            Some(w) => self.worker(Some(w)).await.map(Some),
            None => Ok(None),
        }
    }

    /// A terminal from `worker/session` or a bare session, each part an id or a prefix and the
    /// worker part also a name.
    pub async fn term(&mut self, needle: &str) -> Result<TermRef, ToolError> {
        let (worker, session) = split_term(needle)?;
        let worker = self.some_worker(worker).await?;
        if let (Some(worker), Ok(session)) = (worker, session.parse::<SessionId>()) {
            return Ok(TermRef { worker, session });
        }
        pick_session(&self.terminals(worker).await?, session)
    }
}

/// `worker/session` split at its last `/`; a needle without one is a session alone.
pub fn split_term(needle: &str) -> Result<(Option<&str>, &str), ToolError> {
    let needle = needle.trim();
    let (worker, session) = match needle.rsplit_once('/') {
        Some((w, s)) => (Some(w.trim()), s.trim()),
        None => (None, needle),
    };
    if session.is_empty() || worker.is_some_and(str::is_empty) {
        return Err(ToolError::invalid(format!(
            "{needle:?} is not a terminal; write it as worker/session"
        )));
    }
    Ok((worker, session))
}

/// The worker `needle` names: an exact id, then an exact name, then a case-blind name, then a
/// unique id prefix. Without a needle, the only worker online.
pub fn pick_worker(workers: &[WorkerInfo], needle: Option<&str>) -> Result<WorkerId, ToolError> {
    let Some(needle) = needle.map(str::trim) else {
        let online: Vec<_> = workers.iter().filter(|w| w.liveness == Liveness::Online).collect();
        return match online.as_slice() {
            [one] => Ok(one.worker),
            [] => Err(ToolError::new(ErrorCode::WorkerUnreachable, "no worker is online")),
            many => Err(ToolError::invalid(format!(
                "{} workers are online ({}); name one as the worker",
                many.len(),
                names(many)
            ))),
        };
    };
    let lower = needle.to_lowercase();
    let tiers: [&dyn Fn(&WorkerInfo) -> bool; 4] = [
        &|w| w.worker.to_string() == lower,
        &|w| w.name == needle,
        &|w| w.name.eq_ignore_ascii_case(needle),
        &|w| w.worker.to_string().starts_with(&lower),
    ];
    for matches in tiers {
        let hits: Vec<_> = workers.iter().filter(|w| matches(w)).collect();
        match hits.as_slice() {
            [] => {}
            [one] => return Ok(one.worker),
            many => {
                return Err(ToolError::invalid(format!(
                    "{needle:?} matches {} workers: {}",
                    many.len(),
                    names(many)
                )));
            }
        }
    }
    Err(ToolError::new(ErrorCode::UnknownWorker, format!("no worker is called {needle:?}")))
}

fn names(workers: &[&WorkerInfo]) -> String {
    workers.iter().map(|w| format!("{} ({})", w.name, w.worker)).collect::<Vec<_>>().join(", ")
}

/// The one terminal whose session id starts with `prefix`.
pub fn pick_session(
    terminals: &[(WorkerId, SessionSummary)],
    prefix: &str,
) -> Result<TermRef, ToolError> {
    let prefix = prefix.to_lowercase();
    let hits: Vec<_> =
        terminals.iter().filter(|(_, s)| s.id.to_string().starts_with(&prefix)).collect();
    match hits.as_slice() {
        [(worker, s)] => Ok(TermRef { worker: *worker, session: s.id }),
        [] => Err(ToolError::new(
            ErrorCode::UnknownTerminal,
            format!("no terminal matches {prefix:?}"),
        )),
        many => Err(ToolError::invalid(format!(
            "{prefix:?} matches {} terminals; give more of the id: {}",
            many.len(),
            many.iter().map(|(w, s)| format!("{w}/{}", s.id)).collect::<Vec<_>>().join(", ")
        ))),
    }
}

#[cfg(test)]
mod tests {
    use slopty_proto::server::{Os, WorkerCaps};
    use slopty_proto::terminal::SessionState;

    use super::*;

    fn info(id: &str, name: &str, liveness: Liveness) -> WorkerInfo {
        WorkerInfo {
            worker: id.parse().unwrap(),
            name: name.to_owned(),
            address: "100.64.0.1:45550".to_owned(),
            liveness,
            caps: WorkerCaps {
                os: Os::MacOs,
                os_version: "26.5".to_owned(),
                arch: "aarch64".to_owned(),
                cpus: 8,
                memory: 1,
                encoders: Vec::new(),
                displays: Vec::new(),
                agents: Vec::new(),
                can_capture: false,
                can_inject: false,
                load: 0.0,
                version: "0".to_owned(),
            },
            last_seen_ms: 0,
        }
    }

    const STUDIO: &str = "0199a000-0000-7000-8000-000000000001";
    const LAPTOP: &str = "0199b000-0000-7000-8000-000000000002";

    fn fleet() -> Vec<WorkerInfo> {
        vec![info(STUDIO, "mac-studio", Liveness::Online), info(LAPTOP, "MacBook", Liveness::Gone)]
    }

    #[test]
    fn a_worker_is_found_by_id_name_or_prefix() {
        let w = fleet();
        let studio: WorkerId = STUDIO.parse().unwrap();
        let laptop: WorkerId = LAPTOP.parse().unwrap();
        assert_eq!(pick_worker(&w, Some(STUDIO)).unwrap(), studio);
        assert_eq!(pick_worker(&w, Some(&STUDIO.to_uppercase())).unwrap(), studio);
        assert_eq!(pick_worker(&w, Some("mac-studio")).unwrap(), studio);
        assert_eq!(pick_worker(&w, Some("macbook")).unwrap(), laptop, "names are case-blind");
        assert_eq!(pick_worker(&w, Some("0199b")).unwrap(), laptop);
        let err = pick_worker(&w, Some("0199")).unwrap_err();
        assert!(err.message.contains("matches 2 workers"), "{err}");
        assert_eq!(err.code, ErrorCode::Invalid);
        let err = pick_worker(&w, Some("pi")).unwrap_err();
        assert!(err.message.contains("no worker is called"), "{err}");
        assert_eq!(err.code, ErrorCode::UnknownWorker);
    }

    #[test]
    fn an_exact_name_beats_a_case_blind_one() {
        let w = vec![info(STUDIO, "box", Liveness::Online), info(LAPTOP, "Box", Liveness::Online)];
        assert_eq!(pick_worker(&w, Some("Box")).unwrap(), LAPTOP.parse().unwrap());
        let err = pick_worker(&w, Some("BOX")).unwrap_err();
        assert!(err.message.contains("matches 2 workers"), "{err}");
    }

    #[test]
    fn no_worker_named_means_the_only_one_online() {
        assert_eq!(pick_worker(&fleet(), None).unwrap(), STUDIO.parse().unwrap());
        let both = vec![
            info(STUDIO, "mac-studio", Liveness::Online),
            info(LAPTOP, "MacBook", Liveness::Online),
        ];
        let err = pick_worker(&both, None).unwrap_err();
        assert!(err.message.contains("name one as the worker"), "{err}");
        let err = pick_worker(&[], None).unwrap_err();
        assert!(err.message.contains("no worker is online"), "{err}");
    }

    #[test]
    fn a_term_splits_at_its_last_slash() {
        assert_eq!(split_term("studio/0199").unwrap(), (Some("studio"), "0199"));
        assert_eq!(split_term(" 0199 ").unwrap(), (None, "0199"));
        split_term("studio/").unwrap_err();
        split_term("/0199").unwrap_err();
        assert_eq!(split_term("").unwrap_err().code, ErrorCode::Invalid);
    }

    fn term(worker: &str, session: &str) -> (WorkerId, SessionSummary) {
        let summary = SessionSummary {
            id: session.parse().unwrap(),
            title: String::new(),
            cwd: None,
            repo: None,
            cols: 80,
            rows: 24,
            state: SessionState::Running,
            viewers: 0,
            command: Vec::new(),
            agent: None,
        };
        (worker.parse().unwrap(), summary)
    }

    #[test]
    fn a_session_prefix_must_be_unique() {
        let list = [
            term(STUDIO, "0199a1b2-c3d4-7000-8000-00000000aaaa"),
            term(LAPTOP, "0199a1b2-c3d5-7000-8000-00000000bbbb"),
        ];
        let hit = pick_session(&list, "0199A1B2-C3D5").unwrap();
        assert_eq!(hit.worker, LAPTOP.parse().unwrap());
        let err = pick_session(&list, "0199a1b2").unwrap_err();
        assert!(err.message.contains("matches 2 terminals"), "{err}");
        let err = pick_session(&list, "ffff").unwrap_err();
        assert!(err.message.contains("no terminal matches"), "{err}");
        assert_eq!(err.code, ErrorCode::UnknownTerminal);
    }
}
