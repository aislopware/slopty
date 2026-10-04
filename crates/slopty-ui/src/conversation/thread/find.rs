//! Finding words in a thread: which of its items hold them, and the row that shows each.
//!
//! The find bar reads what the worker's search reads (`ThreadRequest::Search`): the person's
//! messages, the agent's answers and reasoning, its calls' titles and its notices. The items
//! the client holds are matched here, at once, as the person types. Turns older than those
//! are the worker's to search: its hits for this thread ([`older`]) stand before the rest, and
//! going to one pages the thread back to it.

use slopty_proto::thread::wire::{ItemHit, ThreadHits};
use slopty_proto::thread::{Item, ItemBody, ItemId, ThreadId, ThreadState, TurnId};

use super::rows::Row;

/// Where a match is: an item the client holds, or one in an older turn the worker found.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Found {
    /// The item.
    pub item: ItemId,
    /// Its turn, to page back to when it is not held.
    pub turn: TurnId,
}

/// What an item says, as find reads it; nothing for an item with no words of its own.
fn words(item: &Item) -> Option<String> {
    match &item.body {
        ItemBody::User(message) => Some(match &message.command {
            Some(command) => format!("/{command} {}", message.text.text),
            None => message.text.text.clone(),
        }),
        ItemBody::Text(text) | ItemBody::Reasoning(text) => Some(text.text.clone()),
        ItemBody::Tool(call) => Some(call.title.clone()),
        ItemBody::Notice(notice) => Some(notice.text.text.clone()),
        ItemBody::Compaction(_) | ItemBody::Review { .. } | ItemBody::Extra { .. } => None,
    }
}

/// Whether `text` holds every word of `query`, ignoring case, as the worker's search does.
fn holds(text: &str, query: &[String]) -> bool {
    let text = text.to_lowercase();
    query.iter().all(|word| text.contains(word.as_str()))
}

/// The words of `query`, lowered; none for a blank one.
fn query_words(query: &str) -> Vec<String> {
    query.split_whitespace().map(str::to_lowercase).collect()
}

/// The items of `state` that hold every word of `query`, in the thread's order.
#[must_use]
pub fn matches(state: &ThreadState, query: &str) -> Vec<Found> {
    let query = query_words(query);
    if query.is_empty() {
        return Vec::new();
    }
    state
        .items
        .iter()
        .filter(|item| words(item).is_some_and(|w| holds(&w, &query)))
        .map(|item| Found { item: item.id.clone(), turn: item.turn })
        .collect()
}

/// The worker's hits in `thread` that the client does not hold: older turns, oldest first.
/// How many more it left out comes second.
#[must_use]
pub fn older(hits: &ThreadHits, thread: ThreadId, state: &ThreadState) -> (Vec<Found>, u32) {
    let Some(hit) = hits.threads.iter().find(|t| t.thread == thread) else {
        return (Vec::new(), 0);
    };
    let held = |h: &ItemHit| state.items.iter().any(|i| i.id == h.item);
    let mut found: Vec<&ItemHit> = hit.hits.iter().filter(|h| !held(h)).collect();
    found.sort_by_key(|h| (h.turn, h.at_ms));
    let found = found.into_iter().map(|h| Found { item: h.item.clone(), turn: h.turn }).collect();
    (found, hit.more)
}

/// Where the item at `at` (its place in the thread's items) shows: its own row, or else the
/// fold or group that holds it, folded.
#[must_use]
pub fn row_of(rows: &[Row], spans: &[std::ops::Range<usize>], at: usize) -> Option<usize> {
    let holds = |ix: &usize| spans.get(*ix).is_some_and(|span| span.contains(&at));
    let own = (0..rows.len())
        .filter(holds)
        .find(|ix| !matches!(rows.get(*ix), Some(Row::Fold { .. } | Row::Group { .. })));
    own.or_else(|| (0..rows.len()).find(holds))
}

#[cfg(test)]
mod tests {
    use slopty_core::WallMs;
    use slopty_proto::thread::wire::{ItemHit, ThreadHit, ThreadHits};
    use slopty_proto::thread::{Clipped, Item, ItemBody, ItemId, TurnId};

    use super::{Found, matches, older, row_of};
    use crate::conversation::thread::fixtures;
    use crate::conversation::thread::rows::Row;

    fn item(id: &str, turn: u32, body: ItemBody) -> Item {
        Item { id: ItemId(id.to_owned()), turn: TurnId(turn), at_ms: WallMs::ZERO, body }
    }

    /// Every word must be there, in any case and any order, as the worker's search has it;
    /// an item with no words of its own is never a match.
    #[test]
    fn every_word_is_found_in_any_case() {
        let mut state = fixtures::empty();
        state.items = vec![
            item("a", 1, ItemBody::Text(Clipped::whole("The Parser fails on tabs"))),
            item("b", 1, ItemBody::Reasoning(Clipped::whole("tabs again"))),
            item("c", 2, ItemBody::Text(Clipped::whole("fixed the parser"))),
        ];
        let ids = |q: &str| matches(&state, q).into_iter().map(|f| f.item.0).collect::<Vec<_>>();
        assert_eq!(ids("parser tabs"), ["a"]);
        assert_eq!(ids("TABS"), ["a", "b"]);
        assert!(ids("  ").is_empty(), "nothing for no words");
    }

    /// The worker's hits for the thread that the client does not hold come oldest first, with
    /// what it left out; another thread's are not this one's.
    #[test]
    fn older_hits_are_the_ones_not_held() {
        let mut state = fixtures::empty();
        let thread = state.meta.id;
        state.items = vec![item("held", 9, ItemBody::Text(Clipped::whole("parser")))];
        let hit = |id: &str, turn: u32| ItemHit {
            item: ItemId(id.to_owned()),
            turn: TurnId(turn),
            said: ItemHit::AGENT.to_owned(),
            text: "parser".to_owned(),
            spans: Vec::new(),
            cut_before: false,
            cut_after: false,
            at_ms: WallMs::ZERO,
        };
        let hits = ThreadHits {
            query: "parser".to_owned(),
            threads: vec![ThreadHit {
                thread,
                hits: vec![hit("held", 9), hit("late", 5), hit("early", 2)],
                more: 4,
            }],
            more: 0,
        };
        let (found, more) = older(&hits, thread, &state);
        let ids: Vec<&str> = found.iter().map(|f| f.item.0.as_str()).collect();
        assert_eq!((ids.as_slice(), more), (["early", "late"].as_slice(), 4));
        let other = slopty_proto::thread::ThreadId::new();
        assert_eq!(older(&hits, other, &state), (Vec::<Found>::new(), 0));
    }

    /// An item shows in its own row, or in the folded fold over it.
    #[test]
    fn an_item_shows_in_its_row_or_its_fold() {
        let turn = TurnId(1);
        let rows = [
            Row::User { item: ItemId("u".to_owned()) },
            Row::Fold { turn, part: 0, open: false },
            Row::Text { item: ItemId("t".to_owned()) },
        ];
        let spans = [0..1, 0..3, 2..3];
        assert_eq!(row_of(&rows, &spans, 0), Some(0), "its own row before the fold");
        assert_eq!(row_of(&rows, &spans, 1), Some(1), "folded work: the fold");
        assert_eq!(row_of(&rows, &spans, 2), Some(2));
        assert_eq!(row_of(&rows, &spans, 3), None);
    }
}
