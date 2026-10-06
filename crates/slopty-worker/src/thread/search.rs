//! What was said in the threads this worker holds, searched
//! ([`ThreadRequest::Search`](slopty_proto::thread::wire::ThreadRequest::Search)).
//!
//! **What is searched.** Each thread's state as its log keeps it: the person's messages, the
//! agent's answers and reasoning, its calls' titles and its notices. A text the log keeps clipped
//! is searched as far as it is kept; the rest is the agent's own session's, which a search does
//! not read. Every thread held is searched, a subagent's and one asleep among them.
//!
//! **The match** is the prompt search's ([`super::history`]): every word asked for must be in
//! the text, as written, case ignored unless a word has a capital, accents ignored. A whole word,
//! or one at a word's start, scores above one inside another word. An item's score is its
//! match's; a thread's is its best item's. Threads go best first, then by their newest match;
//! each carries its best few items, cut round their first match.
//!
//! **The cost.** The host's lock is held for one thread at a time ([`Host::visit`]), and a
//! search runs on the blocking pool, so no adapter waits on a whole search.

use nucleo_matcher::pattern::{AtomKind, CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use slopty_core::WallMs;
use slopty_proto::thread::wire::{
    HITS_PER_THREAD, ITEM_HIT_BYTES, ItemHit, SEARCH_THREADS, ThreadHit, ThreadHits,
};
use slopty_proto::thread::{ItemBody, ThreadState};

use super::Host;
use super::history::excerpt;

/// How much of an item a hit shows before its first match, in bytes.
const LEAD_BYTES: usize = 48;

/// The threads in `host` where every word of `query` was said, at most `limit` of them (and
/// at most [`SEARCH_THREADS`]). Nothing is found for no words. Blocks on the host's lock, a
/// thread at a time.
#[must_use]
pub fn search(host: &Host, query: &str, limit: u32) -> ThreadHits {
    let pattern =
        Pattern::new(query, CaseMatching::Smart, Normalization::Smart, AtomKind::Substring);
    if pattern.atoms.is_empty() {
        return ThreadHits { query: query.to_owned(), threads: Vec::new(), more: 0 };
    }
    let mut matcher = Matcher::new(Config::DEFAULT);
    let mut found = host.visit(|state| thread_hits(state, &pattern, &mut matcher));
    found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
    let limit = usize::try_from(limit.clamp(1, SEARCH_THREADS)).unwrap_or(usize::MAX);
    let more = u32::try_from(found.len().saturating_sub(limit)).unwrap_or(u32::MAX);
    found.truncate(limit);
    let threads = found.into_iter().map(|(_, _, hit)| hit).collect();
    ThreadHits { query: query.to_owned(), threads, more }
}

/// `state`'s items that match `pattern`, its best first, with the thread's best score and its
/// newest match's time; `None` when none matches.
fn thread_hits(
    state: &ThreadState,
    pattern: &Pattern,
    matcher: &mut Matcher,
) -> Option<(u32, WallMs, ThreadHit)> {
    let mut chars = Vec::new();
    let mut scored: Vec<(u32, usize)> = Vec::new();
    for (at, item) in state.items.iter().enumerate() {
        let Some((_, text)) = said(&item.body) else { continue };
        if let Some(score) = pattern.score(Utf32Str::new(text, &mut chars), matcher) {
            scored.push((score, at));
        }
    }
    let best = scored.iter().map(|(score, _)| *score).max()?;
    let newest = scored
        .iter()
        .filter_map(|(_, at)| state.items.get(*at).map(|item| item.at_ms))
        .max()
        .unwrap_or(WallMs::ZERO);
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
    let more = u32::try_from(scored.len().saturating_sub(HITS_PER_THREAD)).unwrap_or(u32::MAX);
    let hits = scored
        .into_iter()
        .take(HITS_PER_THREAD)
        .filter_map(|(_, at)| {
            let item = state.items.get(at)?;
            let (kind, text) = said(&item.body)?;
            let cut = excerpt(pattern, matcher, text, ITEM_HIT_BYTES, LEAD_BYTES);
            Some(ItemHit {
                item: item.id.clone(),
                turn: item.turn,
                said: kind.to_owned(),
                text: cut.text,
                spans: cut.spans,
                cut_before: cut.cut_before,
                cut_after: cut.cut_after,
                at_ms: item.at_ms,
            })
        })
        .collect();
    Some((best, newest, ThreadHit { thread: state.meta.id, hits, more }))
}

/// The words an item says, and what kind they are; `None` for one a search passes over.
fn said(body: &ItemBody) -> Option<(&'static str, &str)> {
    match body {
        ItemBody::User(message) => Some((ItemHit::PERSON, &message.text.text)),
        ItemBody::Text(text) => Some((ItemHit::AGENT, &text.text)),
        ItemBody::Reasoning(text) => Some((ItemHit::REASONING, &text.text)),
        ItemBody::Tool(call) => Some((ItemHit::TOOL, &call.title)),
        ItemBody::Notice(notice) => Some((ItemHit::NOTICE, &notice.text.text)),
        ItemBody::Compaction(_) | ItemBody::Review { .. } | ItemBody::Extra { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use slopty_proto::search::Span;
    use slopty_proto::thread::detail::Clipped;
    use slopty_proto::thread::{
        Action, AgentId, Drive, Item, ItemId, Notice, ThreadId, ThreadMeta, ToolCall, ToolState,
        TurnId, UserMessage,
    };

    use super::*;
    use crate::thread::log::Limits;

    fn meta(title: &str) -> ThreadMeta {
        ThreadMeta {
            id: ThreadId::new(),
            agent: AgentId::named(AgentId::CODEX),
            agent_version: String::new(),
            native: title.to_owned(),
            cwd: "/w".to_owned(),
            title: title.to_owned(),
            terminal: None,
            parent: None,
            origin: ThreadMeta::PERSON.to_owned(),
            forked_from: None,
            drive: Drive::named(Drive::SHARED),
            caps: Vec::new(),
            models: Vec::new(),
            modes: Vec::new(),
            efforts: Vec::new(),
            facts: std::collections::BTreeMap::new(),
            created_ms: WallMs::ZERO,
        }
    }

    fn item(id: &str, turn: u32, at: u64, body: ItemBody) -> Action {
        Action::ItemCompleted(Item {
            id: ItemId(id.to_owned()),
            turn: TurnId(turn),
            at_ms: WallMs::from_millis(at),
            body,
        })
    }

    fn person(text: &str) -> ItemBody {
        ItemBody::User(UserMessage {
            text: Clipped::whole(text),
            images: Vec::new(),
            command: None,
            intent: None,
        })
    }

    fn tool(title: &str) -> ItemBody {
        ItemBody::Tool(Box::new(ToolCall {
            name: "Bash".to_owned(),
            kind: slopty_proto::thread::kind::EXEC.to_owned(),
            title: title.to_owned(),
            input: Clipped::whole("{}"),
            state: ToolState::Completed,
            output: None,
            images: Vec::new(),
            detail: None,
            child: None,
            ended_ms: None,
        }))
    }

    fn host(threads: Vec<(ThreadMeta, Vec<Action>)>) -> (tempfile::TempDir, Host, Vec<ThreadId>) {
        let dir = tempfile::tempdir().unwrap();
        let host = Host::open(dir.path(), Limits::default()).unwrap();
        let mut ids = Vec::new();
        for (meta, actions) in threads {
            ids.push(meta.id);
            host.create(meta.clone()).unwrap();
            host.apply(meta.id, actions);
        }
        (dir, host, ids)
    }

    fn found(hits: &ThreadHits) -> Vec<(ThreadId, Vec<(&str, &str)>)> {
        hits.threads
            .iter()
            .map(|t| {
                (t.thread, t.hits.iter().map(|h| (h.item.0.as_str(), h.said.as_str())).collect())
            })
            .collect()
    }

    /// Every word must be said in one item, whatever its kind: the person's words, the agent's
    /// answer and reasoning, a call's title, a notice. A whole word ranks a thread above one
    /// where it is inside another word; among equals the newer match goes first, in a thread as
    /// across threads. A text the search passes over (a compaction) finds nothing.
    #[test]
    fn every_word_is_found_in_what_was_said_the_best_and_newest_first() {
        let (_dir, host, ids) = host(vec![
            (
                meta("whole"),
                vec![
                    item("u1", 1, 10, person("Fix the flaky login test")),
                    item("t1", 1, 20, ItemBody::Text(Clipped::whole("The login test is flaky."))),
                    item("r1", 1, 30, ItemBody::Reasoning(Clipped::whole("login flows race"))),
                    item("c1", 1, 40, tool("Run cargo test login")),
                    item(
                        "n1",
                        1,
                        50,
                        ItemBody::Notice(Notice::new(Notice::INFO, Clipped::whole("login said"))),
                    ),
                ],
            ),
            (meta("inside"), vec![item("u2", 1, 99, person("Relogin again"))]),
            (meta("elsewhere"), vec![item("u3", 1, 5, person("Nothing to see"))]),
        ]);
        let hits = search(&host, "login", 10);
        assert_eq!(hits.query, "login");
        let [whole, inside, _] = ids[..] else { panic!() };
        assert_eq!(
            found(&hits),
            [
                (
                    whole,
                    vec![
                        ("n1", ItemHit::NOTICE),
                        ("c1", ItemHit::TOOL),
                        ("r1", ItemHit::REASONING)
                    ]
                ),
                (inside, vec![("u2", ItemHit::PERSON)]),
            ],
            "a whole word first; the newest of equals first"
        );
        assert_eq!(hits.threads[0].more, 2, "two more of its items matched");
        assert_eq!(hits.more, 0);
        let both = search(&host, "flaky TEST", 10);
        assert!(both.threads.is_empty(), "a capital is matched as written");
        let both = search(&host, "flaky test", 10);
        let items: Vec<&str> = both.threads[0].hits.iter().map(|h| h.item.0.as_str()).collect();
        assert_eq!(items, ["t1", "u1"], "each word in the one item");
        assert!(search(&host, "   ", 10).threads.is_empty(), "no words find nothing");
    }

    /// A hit shows the part of its item round the first match, cut on characters' boundaries,
    /// with each match marked in what it shows; past the limit, the threads left out are
    /// counted.
    #[test]
    fn a_hit_shows_its_match_and_the_limit_counts_what_it_left_out() {
        let long = format!("{}é needle {}", "a".repeat(1_000), "b".repeat(1_000));
        let (_dir, host, ids) = host(vec![
            (meta("long"), vec![item("u1", 2, 10, person(&long))]),
            (meta("short"), vec![item("u2", 1, 5, person("a needle here"))]),
        ]);
        let hits = search(&host, "needle", 1);
        assert_eq!(hits.threads.len(), 1);
        assert_eq!(hits.more, 1, "one thread left out");
        let hit = &hits.threads[0].hits[0];
        assert_eq!(hits.threads[0].thread, ids[0], "the newer of equal matches");
        assert_eq!(hit.turn, TurnId(2));
        assert!(hit.cut_before && hit.cut_after);
        assert!(hit.text.len() <= ITEM_HIT_BYTES);
        let [Span { start, end }] = hit.spans[..] else { panic!("{:?}", hit.spans) };
        let marked = hit.text.get(usize::try_from(start).unwrap()..usize::try_from(end).unwrap());
        assert_eq!(marked, Some("needle"));
        let hits = search(&host, "needle", 0);
        assert_eq!(hits.threads.len(), 1, "a limit of none is one");
    }

    /// What a search costs the worker over a day's threads: 100 threads of 40 turns, each a
    /// message, an answer of about 300 bytes and three calls, searched for a frequent word, a
    /// two-word phrase and a word found nowhere. The palette's Threads section asks once the
    /// field rests (`find::ASK_AFTER` in slopty-ui); this is what each ask costs.
    #[test]
    #[ignore = "measurement: cargo test -p slopty-worker --lib search_cost -- --ignored --nocapture"]
    fn search_cost() {
        let words = ["parser", "error", "span", "cursor", "reset", "token", "lexer", "build"];
        let threads = (0..100_u32)
            .map(|t| {
                let actions = (0..40_u32)
                    .flat_map(|turn| {
                        let at = u64::from(t * 1_000 + turn * 10);
                        let w = |k: u32| words[usize::try_from((t + turn + k) % 8).unwrap()];
                        let answer = (0..6)
                            .map(|k| {
                                format!(
                                    "The {} keeps its {} and the {} moves on.",
                                    w(k),
                                    w(k + 1),
                                    w(k + 2)
                                )
                            })
                            .collect::<Vec<_>>()
                            .join(" ");
                        let id = |kind: &str| format!("{kind}{t}.{turn}");
                        vec![
                            item(
                                &id("u"),
                                turn,
                                at,
                                person(&format!("Fix the {} in the {}", w(0), w(3))),
                            ),
                            item(&id("c1"), turn, at + 1, tool(&format!("cargo test {}", w(1)))),
                            item(&id("c2"), turn, at + 2, tool("cargo clippy")),
                            item(&id("c3"), turn, at + 3, tool(&format!("rg {}", w(2)))),
                            item(&id("a"), turn, at + 4, ItemBody::Text(Clipped::whole(&answer))),
                        ]
                    })
                    .collect();
                (meta(&format!("thread {t}")), actions)
            })
            .collect();
        let (_dir, host, _ids) = host(threads);
        for query in ["pa", "parser error", "zebra"] {
            let mut took: Vec<std::time::Duration> = std::iter::repeat_with(|| {
                let started = std::time::Instant::now();
                let hits = search(&host, query, SEARCH_THREADS);
                let took = started.elapsed();
                std::hint::black_box(hits);
                took
            })
            .take(100)
            .collect();
            took.sort();
            println!("{query:>14}: p50 {:?}, p95 {:?}, max {:?}", took[49], took[94], took[99]);
        }
    }
}
