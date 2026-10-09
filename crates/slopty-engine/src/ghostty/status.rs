//! The program status protocol (`OSC 7501`): what a program says it is doing, record by record.
//!
//! libghostty checks each report against the specification and hands it over; it keeps none.
//! The engine keeps the records as the specification says
//! (<https://www.superlogical.com/rex/docs/build/program-status>):
//!
//! - A report replaces its record whole, by its id (a `/` path; empty is the program itself).
//! - A `clear` removes the record and every record beneath it, and with no id every record. A full
//!   reset (RIS) sends one with no id.
//! - A new prompt (`OSC 133;A`) ends `working`, `blocked` and `idle`: the shell has the terminal
//!   back. `done` and `error` stay, for the person to see.
//! - At most [`RECORDS`], the one updated longest ago making room.
//!
//! Reports are queued in stream order with the prompt marks, so a report written after a prompt
//! in the same read outlives that prompt. A title or a message is a program's untrusted text:
//! the characters that reorder or hide text are taken out before anything shows it.

use std::cell::RefCell;
use std::rc::Rc;

use libghostty_vt::Terminal;
use libghostty_vt::terminal::{ProgramNeed as VtNeed, ProgramState as VtState};
use slopty_proto::terminal::{ProgramState, ProgramStatus};

use super::Pending;
use crate::EngineError;

/// The most records a terminal keeps: the specification's least, so the set the viewers are
/// sent whole after each change stays small.
const RECORDS: usize = ProgramStatus::RECORDS;

/// One report, as the engine applies it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Report {
    /// The record with this id is now this.
    Put(ProgramStatus),
    /// Remove the record with this id and those beneath it; every record for an empty id.
    Clear(String),
}

/// Queue each report libghostty passes on, among the prompt marks of the same write.
pub(super) fn install(
    term: &mut Terminal<'static, 'static>,
    marks: &Rc<RefCell<Vec<Pending>>>,
) -> Result<(), EngineError> {
    let marks = Rc::clone(marks);
    term.on_program_status(move |_, report| {
        let id = String::from_utf8_lossy(report.id()).into_owned();
        let state = match report.state() {
            Ok(VtState::Clear) => {
                marks.borrow_mut().push(Pending::Status(Report::Clear(id)));
                return;
            }
            Ok(VtState::Idle) => ProgramState::Idle,
            Ok(VtState::Working) => ProgramState::Working,
            Ok(VtState::Done) => ProgramState::Done,
            Ok(VtState::Blocked) => ProgramState::Blocked,
            Ok(VtState::Error) => ProgramState::Error,
            // A state a later libghostty adds.
            _ => return,
        };
        let need = match report.need() {
            VtNeed::Permission => Some(ProgramStatus::PERMISSION),
            VtNeed::Question => Some(ProgramStatus::QUESTION),
            VtNeed::Auth => Some(ProgramStatus::AUTH),
            _ => None,
        };
        let status = ProgramStatus {
            id,
            state,
            need: need.filter(|_| state == ProgramState::Blocked).map(str::to_owned),
            progress: report.progress().filter(|p| *p <= 100),
            app: shown(report.app()),
            title: shown(report.title()),
            message: shown(report.message()),
        };
        marks.borrow_mut().push(Pending::Status(Report::Put(status)));
    })?;
    Ok(())
}

/// A program's words as they may be shown: UTF-8, with the characters that reorder text (the
/// direction marks, embeddings, overrides and isolates) or hide it (the zero-width ones) taken
/// out, so a message cannot pass for another's.
fn shown(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).chars().filter(|c| !hides(*c)).collect()
}

/// Whether `c` reorders or hides the text around it.
const fn hides(c: char) -> bool {
    matches!(
        c,
        '\u{061C}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{2069}'
            | '\u{FEFF}'
    )
}

/// The records a terminal keeps, each with when it was last put, oldest first.
#[derive(Debug, Default)]
pub(super) struct Records {
    kept: Vec<ProgramStatus>,
}

impl Records {
    /// Apply `report`; `true` when the records changed.
    pub(super) fn apply(&mut self, report: Report) -> bool {
        match report {
            Report::Put(status) => {
                let at = self.kept.iter().position(|r| r.id == status.id);
                let was = at.map(|at| self.kept.remove(at));
                if was.is_none() && self.kept.len() >= RECORDS {
                    self.kept.remove(0);
                }
                // The same again changes nothing, but still counts as the latest update.
                let changed = was.as_ref() != Some(&status);
                self.kept.push(status);
                changed
            }
            Report::Clear(id) => {
                let before = self.kept.len();
                self.kept.retain(|r| !(id.is_empty() || beneath(&r.id, &id)));
                self.kept.len() != before
            }
        }
    }

    /// A prompt started: the shell has the terminal back, so what said it was at work, waiting
    /// or at rest has ended. `true` when a record went.
    pub(super) fn prompt(&mut self) -> bool {
        let before = self.kept.len();
        self.kept.retain(|r| matches!(r.state, ProgramState::Done | ProgramState::Error));
        self.kept.len() != before
    }

    /// Every record, by id.
    pub(super) fn all(&self) -> Vec<ProgramStatus> {
        let mut all = self.kept.clone();
        all.sort_by(|a, b| a.id.cmp(&b.id));
        all
    }

    /// The reports that put every record back, oldest first, for a checkpoint.
    pub(super) fn replay(&self, out: &mut Vec<u8>) {
        for record in &self.kept {
            out.extend_from_slice(b"\x1b]7501;state=");
            out.extend_from_slice(state_word(record.state).as_bytes());
            if let Some(need) = &record.need {
                out.extend_from_slice(b":kind=");
                out.extend_from_slice(need.as_bytes());
            }
            if let Some(progress) = record.progress {
                out.extend_from_slice(format!(":progress={progress}").as_bytes());
            }
            // An id, an app and a need are names: no `:` or `;` in them, as the specification's
            // grammar has them.
            for (key, value) in [("id", &record.id), ("app", &record.app)] {
                if !value.is_empty() {
                    out.extend_from_slice(format!(":{key}={value}").as_bytes());
                }
            }
            for (key, value) in [("title", &record.title), ("msg", &record.message)] {
                if !value.is_empty() {
                    let encoded = data_encoding::BASE64.encode(value.as_bytes());
                    out.extend_from_slice(format!(":{key}={encoded}").as_bytes());
                }
            }
            out.extend_from_slice(b"\x1b\\");
        }
    }
}

/// Whether the record `id` is `parent` or beneath it (`build/test` is beneath `build`).
fn beneath(id: &str, parent: &str) -> bool {
    id.strip_prefix(parent).is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// A state as a report spells it.
const fn state_word(state: ProgramState) -> &'static str {
    match state {
        ProgramState::Idle => "idle",
        ProgramState::Working => "working",
        ProgramState::Done => "done",
        ProgramState::Blocked => "blocked",
        ProgramState::Error => "error",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put(id: &str, state: ProgramState) -> Report {
        Report::Put(ProgramStatus {
            id: id.to_owned(),
            state,
            need: None,
            progress: None,
            app: String::new(),
            title: String::new(),
            message: String::new(),
        })
    }

    fn ids(records: &Records) -> Vec<String> {
        records.all().into_iter().map(|r| r.id).collect()
    }

    /// A clear takes its record and those beneath it, not a sibling whose name it begins.
    #[test]
    fn a_clear_takes_the_record_and_those_beneath_it() {
        let mut records = Records::default();
        for id in ["build", "build/test", "builder", "deploy"] {
            assert!(records.apply(put(id, ProgramState::Working)));
        }
        assert!(records.apply(Report::Clear("build".to_owned())));
        assert_eq!(ids(&records), ["builder", "deploy"]);
        assert!(!records.apply(Report::Clear("build".to_owned())), "nothing left to take");
        assert!(records.apply(Report::Clear(String::new())));
        assert_eq!(records.all(), [], "no id: every record");
    }

    /// Past the limit, the record updated longest ago makes room; the same report again is
    /// no change, but counts as an update.
    #[test]
    fn the_record_updated_longest_ago_makes_room() {
        let mut records = Records::default();
        for n in 0..RECORDS {
            records.apply(put(&format!("r{n:02}"), ProgramState::Working));
        }
        assert!(!records.apply(put("r00", ProgramState::Working)), "the same again");
        assert!(records.apply(put("new", ProgramState::Working)));
        let kept = ids(&records);
        assert_eq!(kept.len(), RECORDS);
        assert!(kept.contains(&"r00".to_owned()), "updated again, so kept");
        assert!(!kept.contains(&"r01".to_owned()), "the oldest went");
    }

    /// A prompt ends what was at work, waiting or at rest; a result stays to be seen.
    #[test]
    fn a_prompt_ends_all_but_the_results() {
        let mut records = Records::default();
        records.apply(put("a", ProgramState::Working));
        records.apply(put("b", ProgramState::Blocked));
        records.apply(put("c", ProgramState::Idle));
        records.apply(put("d", ProgramState::Done));
        records.apply(put("e", ProgramState::Error));
        assert!(records.prompt());
        assert_eq!(ids(&records), ["d", "e"]);
        assert!(!records.prompt(), "nothing more to end");
    }

    /// The characters that reorder or hide text are taken out of a program's words.
    #[test]
    fn a_programs_words_lose_what_reorders_or_hides_them() {
        assert_eq!(shown("Apply\u{202E}?\u{200B}".as_bytes()), "Apply?");
        assert_eq!(shown("ok \u{2067}x\u{2069}".as_bytes()), "ok x");
        assert_eq!(shown(b"bad \xff byte"), "bad \u{FFFD} byte");
    }
}
