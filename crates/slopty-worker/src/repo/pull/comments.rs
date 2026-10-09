//! A pull request's review still open, read with the person's own `gh` for its agent to
//! address ([`PullComments`]).
//!
//! - **Threads.** Each review thread not resolved, on its file and line, its notes in order. gh
//!   asks GitHub's GraphQL API, the one place that says whether a thread is resolved, under the
//!   repository gh resolves from the checkout (`{owner}`, `{repo}`).
//! - **Reviewers' own words.** Each reviewer's latest review, when it asked for changes or
//!   commented, and said something over its threads. An approval, or words a later review of theirs
//!   replaced, ask nothing more.
//! - **Bounds.** At most [`COMMENTS_MAX`] threads, [`NOTES_MAX`] notes each, [`NOTE_MAX`] bytes a
//!   note: the rest is counted, or cut at a character's edge.

use std::path::Path;

use serde::Deserialize;
use slopty_proto::git::{
    COMMENTS_MAX, GitOutcome, NOTE_MAX, NOTES_MAX, PullComments, PullNote, PullThread,
};

use super::{REMOTE, run};

/// What GitHub is asked: the pull request's latest reviews and its review threads, each with
/// its first notes.
const QUERY: &str = "query($owner: String!, $name: String!, $number: Int!) { \
     repository(owner: $owner, name: $name) { pullRequest(number: $number) { \
     reviews(last: 50) { nodes { author { login } body state url } } \
     reviewThreads(first: 100) { totalCount nodes { isResolved isOutdated path line \
     comments(first: 20) { totalCount nodes { author { login } body url } } } } } } }";

#[derive(Debug, Deserialize)]
struct Answer {
    data: Data,
}

#[derive(Debug, Deserialize)]
struct Data {
    repository: Repository,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Repository {
    pull_request: Option<Request>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Request {
    reviews: Nodes<Review>,
    review_threads: Counted<Thread>,
}

#[derive(Debug, Deserialize)]
struct Nodes<T> {
    nodes: Vec<T>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Counted<T> {
    total_count: u32,
    nodes: Vec<T>,
}

#[derive(Debug, Deserialize)]
struct Author {
    login: String,
}

#[derive(Debug, Deserialize)]
struct Review {
    author: Option<Author>,
    #[serde(default)]
    body: String,
    /// `APPROVED`, `CHANGES_REQUESTED`, `COMMENTED`, `DISMISSED`, `PENDING`.
    #[serde(default)]
    state: String,
    url: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Thread {
    is_resolved: bool,
    #[serde(default)]
    is_outdated: bool,
    path: Option<String>,
    line: Option<u32>,
    comments: Counted<Comment>,
}

#[derive(Debug, Deserialize)]
struct Comment {
    author: Option<Author>,
    #[serde(default)]
    body: String,
    url: Option<String>,
}

/// The review still open on pull request `number`, asked of `gh` in the checkout at `root`.
///
/// # Errors
/// gh says something other than its answer, or the repository has no such pull request.
pub(super) async fn read(gh: &Path, root: &Path, number: u32) -> Result<PullComments, GitOutcome> {
    let number_field = format!("number={number}");
    let query = format!("query={QUERY}");
    let args = [
        "api",
        "graphql",
        "-F",
        "owner={owner}",
        "-F",
        "name={repo}",
        "-F",
        &number_field,
        "-f",
        &query,
    ];
    let out = run(gh, root, &args, None, REMOTE).await?;
    parse(&out, number)
}

/// gh's answer for pull request `number`, as the review still open.
///
/// # Errors
/// It is not the answer asked for, or names no such pull request.
pub(super) fn parse(out: &str, number: u32) -> Result<PullComments, GitOutcome> {
    let answer: Answer = serde_json::from_str(out)
        .map_err(|e| GitOutcome::Failed { said: format!("gh answered no review: {e}") })?;
    let request = answer.data.repository.pull_request.ok_or_else(|| GitOutcome::Refused {
        why: format!("the repository has no pull request #{number}"),
    })?;
    let mut latest: Vec<Review> = Vec::new();
    for review in request.reviews.nodes {
        let author = login(review.author.as_ref());
        latest.retain(|r| login(r.author.as_ref()) != author);
        latest.push(review);
    }
    let said = latest.into_iter().filter(|r| {
        matches!(r.state.as_str(), "CHANGES_REQUESTED" | "COMMENTED") && !r.body.trim().is_empty()
    });
    let mut threads: Vec<PullThread> = said
        .map(|r| PullThread {
            path: None,
            line: None,
            outdated: false,
            url: r.url,
            notes: vec![note(login(r.author.as_ref()), &r.body)],
        })
        .collect();
    let unread =
        request.review_threads.total_count.saturating_sub(count(&request.review_threads.nodes));
    threads.extend(request.review_threads.nodes.into_iter().filter(|t| !t.is_resolved).map(|t| {
        let url = t.comments.nodes.first().and_then(|c| c.url.clone());
        PullThread {
            path: t.path,
            line: t.line,
            outdated: t.is_outdated,
            url,
            notes: t
                .comments
                .nodes
                .iter()
                .map(|c| note(login(c.author.as_ref()), &c.body))
                .collect(),
        }
    }));
    Ok(bounded(number, threads, unread))
}

/// Who wrote it: the forge's name for them, or "someone" for an account gone.
fn login(author: Option<&Author>) -> &str {
    author.map_or("someone", |a| a.login.as_str())
}

fn count<T>(items: &[T]) -> u32 {
    u32::try_from(items.len()).unwrap_or(u32::MAX)
}

/// A note, its body cut to [`NOTE_MAX`] bytes at a character's edge.
pub(super) fn note(author: &str, body: &str) -> PullNote {
    let body = body.trim();
    let mut end = body.len().min(NOTE_MAX);
    while !body.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    PullNote { author: author.to_owned(), body: body.get(..end).unwrap_or_default().to_owned() }
}

/// `threads` held to the bounds, with `unread` more the forge did not list.
pub(super) fn bounded(number: u32, mut threads: Vec<PullThread>, unread: u32) -> PullComments {
    let past = count(&threads).saturating_sub(u32::try_from(COMMENTS_MAX).unwrap_or(u32::MAX));
    threads.truncate(COMMENTS_MAX);
    for thread in &mut threads {
        thread.notes.truncate(NOTES_MAX);
    }
    PullComments { number, threads, more: past.saturating_add(unread) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// GitHub's answer, as gh prints it: two reviews from one reviewer (the later one asks for
    /// changes), an approval with words, a comment-only review with none, a resolved thread and
    /// an open one with a reply.
    const ANSWER: &str = r#"{"data":{"repository":{"pullRequest":{
"reviews":{"nodes":[
 {"author":{"login":"ada"},"body":"Looks fine.","state":"COMMENTED","url":"https://github.com/o/r/pull/7#pullrequestreview-1"},
 {"author":{"login":"ada"},"body":"Split the parser out first.","state":"CHANGES_REQUESTED","url":"https://github.com/o/r/pull/7#pullrequestreview-2"},
 {"author":{"login":"lin"},"body":"Ship it.","state":"APPROVED","url":"https://github.com/o/r/pull/7#pullrequestreview-3"},
 {"author":{"login":"sam"},"body":"","state":"COMMENTED","url":"https://github.com/o/r/pull/7#pullrequestreview-4"}]},
"reviewThreads":{"totalCount":3,"nodes":[
 {"isResolved":true,"isOutdated":false,"path":"src/a.rs","line":3,"comments":{"totalCount":1,"nodes":[
  {"author":{"login":"sam"},"body":"Typo.","url":"https://github.com/o/r/pull/7#discussion_r1"}]}},
 {"isResolved":false,"isOutdated":true,"path":"src/lib.rs","line":42,"comments":{"totalCount":2,"nodes":[
  {"author":{"login":"sam"},"body":"This unwrap panics on an empty file.","url":"https://github.com/o/r/pull/7#discussion_r2"},
  {"author":null,"body":"Agreed.","url":"https://github.com/o/r/pull/7#discussion_r3"}]}}]}}}}}"#;

    /// Each reviewer's latest word that asks something comes first, then each thread still
    /// open with its notes in order; a resolved thread, an approval, an empty review and an
    /// earlier review the reviewer replaced ask nothing. A thread the forge did not list is
    /// counted.
    #[test]
    fn only_the_review_still_open_is_read() {
        type Said<'a> = (Option<&'a str>, Vec<(&'a str, &'a str)>);
        let read = parse(ANSWER, 7).expect("read");
        assert_eq!(read.number, 7);
        let said: Vec<Said<'_>> = read
            .threads
            .iter()
            .map(|t| {
                let notes = t.notes.iter().map(|n| (n.author.as_str(), n.body.as_str())).collect();
                (t.path.as_deref(), notes)
            })
            .collect();
        assert_eq!(
            said,
            [
                (None, vec![("ada", "Split the parser out first.")]),
                (
                    Some("src/lib.rs"),
                    vec![("sam", "This unwrap panics on an empty file."), ("someone", "Agreed.")]
                ),
            ]
        );
        let open = &read.threads[1];
        assert_eq!((open.line, open.outdated), (Some(42), true));
        assert_eq!(open.url.as_deref(), Some("https://github.com/o/r/pull/7#discussion_r2"));
        assert_eq!(read.more, 1, "one thread past the page");
    }

    /// A pull request the repository does not have is a refusal; an answer that is not one says
    /// so in gh's place.
    #[test]
    fn no_such_pull_request_and_no_answer_say_so() {
        let none = r#"{"data":{"repository":{"pullRequest":null}}}"#;
        assert!(matches!(parse(none, 9), Err(GitOutcome::Refused { why }) if why.contains("#9")));
        assert!(matches!(parse("{}", 9), Err(GitOutcome::Failed { .. })));
    }

    /// The bounds hold: threads past the most are counted, a long note is cut at a character's
    /// edge, and a thread keeps its first notes.
    #[test]
    fn the_review_is_held_to_its_bounds() {
        let long = "é".repeat(NOTE_MAX);
        let cut = note("ada", &long);
        assert!(cut.body.len() <= NOTE_MAX && cut.body.chars().all(|c| c == 'é'));
        let thread = |notes: usize| PullThread {
            path: None,
            line: None,
            outdated: false,
            url: None,
            notes: vec![note("ada", "again"); notes],
        };
        let many = vec![thread(NOTES_MAX + 2); COMMENTS_MAX + 3];
        let held = bounded(7, many, 2);
        assert_eq!((held.threads.len(), held.more), (COMMENTS_MAX, 5));
        assert!(held.threads.iter().all(|t| t.notes.len() == NOTES_MAX));
    }
}
