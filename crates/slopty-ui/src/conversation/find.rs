//! Finding words in a thread: which entries hold a query, and the row that shows each.
//!
//! A search reads what the reader can read: prompts, answers, a call's title, a note, and
//! thinking where the density shows it. A match inside a folded turn or a closed group names
//! the key that opens it, so the find bar can bring it into view.

use slopty_proto::conversation::{Body, Entry};

use super::model::Thread;
use super::rows::{self, Row};
use super::tools;

/// The entries of `thread` whose words hold `query`, ignoring case, in the thread's order.
/// An empty query finds nothing.
#[must_use]
pub fn matches(thread: &Thread, query: &str, thinking: bool) -> Vec<String> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return Vec::new();
    }
    thread
        .entries()
        .iter()
        .filter(|entry| {
            words(thread, entry, thinking).is_some_and(|w| w.to_lowercase().contains(&query))
        })
        .map(|entry| entry.id.clone())
        .collect()
}

/// What an entry says, as the find bar reads it.
fn words(thread: &Thread, entry: &Entry, thinking: bool) -> Option<String> {
    match &entry.body {
        Body::Prompt(prompt) => Some(match &prompt.command {
            Some(command) => format!("{command} {}", prompt.text.text),
            None => prompt.text.text.clone(),
        }),
        Body::Text(text) => Some(text.text.clone()),
        Body::Thinking(text) => thinking.then(|| text.text.clone()),
        Body::Tool(call) => {
            let title = tools::title(call, thread.tasks());
            Some(match title.subject {
                Some(subject) => format!("{} {subject}", title.verb),
                None => title.verb,
            })
        }
        Body::Note(note) => Some(note.text.text.clone()),
        Body::Compact(_) | Body::Interrupted { .. } | Body::Rewound { .. } => None,
    }
}

/// The row that shows entry `id`: its own, or the group holding it.
#[must_use]
pub fn row_of(rows: &[Row], id: &str) -> Option<usize> {
    rows.iter().position(|row| match row {
        Row::Prompt { id: at } | Row::Answer { id: at, .. } | Row::Entry { id: at, .. } => at == id,
        Row::Group { ids, .. } => ids.iter().any(|at| at == id),
        Row::Fold { .. }
        | Row::Changes { .. }
        | Row::File { .. }
        | Row::Edit { .. }
        | Row::Live { .. }
        | Row::Working
        | Row::Pending { .. } => false,
    })
}

/// The key that opens the fold hiding entry `id`: its turn's prompt's, when `id` is work
/// (not the prompt itself).
#[must_use]
pub fn fold_over(thread: &Thread, id: &str) -> Option<String> {
    let entries = thread.entries();
    let at = entries.iter().position(|e| e.id == id)?;
    let prompt = entries.get(..at)?.iter().rev().find(|e| matches!(e.body, Body::Prompt(_)))?;
    Some(rows::fold_key(&prompt.id))
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use slopty_proto::conversation::ThreadId;

    use super::*;
    use crate::conversation::fixtures::scenario;
    use crate::conversation::model::Model;
    use crate::conversation::rows::{Density, Input, build};

    fn main(model: &Model) -> &Thread {
        model.thread(&ThreadId::Main).expect("main")
    }

    /// A query finds prompts, answers and call titles, whatever their case, and nothing when
    /// it is blank.
    #[test]
    fn a_query_finds_what_the_reader_can_read() {
        let model = scenario("tools");
        let thread = main(&model);
        assert!(matches(thread, "   ", true).is_empty());
        let all: Vec<String> = thread.entries().iter().map(|e| e.id.clone()).collect();
        let first_prompt = thread
            .entries()
            .iter()
            .find_map(|e| match &e.body {
                Body::Prompt(p) => Some(p.text.text.clone()),
                _ => None,
            })
            .expect("a prompt");
        let word = first_prompt.split_whitespace().next().expect("a word").to_uppercase();
        let found = matches(thread, &word, true);
        assert!(!found.is_empty(), "{word} in {all:?}");
        assert!(found.iter().all(|id| all.contains(id)));
    }

    /// A match in a folded turn is behind its fold's key; once opened, a row shows it.
    #[test]
    fn a_folded_match_names_the_fold_that_opens_it() {
        let model = scenario("tools");
        let thread = main(&model);
        let work =
            thread.entries().iter().find(|e| matches!(e.body, Body::Tool(_))).expect("a call");
        let key = fold_over(thread, &work.id).expect("a fold");
        let input = |toggled| Input {
            thread,
            id: &ThreadId::Main,
            live_turn: false,
            waiting: false,
            density: Density::Normal,
            toggled,
            live: &[],
            pending: &[],
        };
        let closed = HashSet::new();
        assert_eq!(row_of(&build(input(&closed)), &work.id), None);
        let open: HashSet<String> = [key].into();
        assert!(row_of(&build(input(&open)), &work.id).is_some());
    }
}
