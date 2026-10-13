// Derived from zeron (MIT, github.com/zeronsh/zeron @ 053ce2e), see THIRD_PARTY_NOTICES.md
//! Repository listing, history and search on the host device.
//!
//! slopty: an excerpt — the history match the interface shares with the engine. The rest is
//! the engine's, which is Slopty's.

use slopty_view_proto::GitHistoryCommit;

/// Case-insensitive subsequence score. Lower is better: adjacent and earlier
/// characters win, while still allowing `cmp rs` to find `composer.rs`.
fn fuzzy_score(query: &str, candidate: &str) -> Option<usize> {
    let candidate = candidate.to_lowercase();
    query
        .split_whitespace()
        .map(str::to_lowercase)
        .try_fold(0usize, |total, term| {
            let mut at = 0;
            let mut score = 0usize;
            let mut previous_end = None;
            for needle in term.chars() {
                let found = candidate[at..].find(needle)? + at;
                score += found;
                if previous_end == Some(found) {
                    score = score.saturating_sub(2);
                }
                at = found + needle.len_utf8();
                previous_end = Some(at);
            }
            Some(total.saturating_add(score))
        })
}

/// Match a history commit using the same semantics as the history UI.
///
/// This remains the shared entry point so RPC filtering and client-side
/// filtering cannot disagree about Unicode case normalization.
pub fn git_history_matches(query: &str, commit: &GitHistoryCommit) -> bool {
    let query = query.trim();
    if query.is_empty() {
        return true;
    }
    let normalized = query.to_ascii_lowercase();
    commit.sha.to_ascii_lowercase().starts_with(&normalized)
        || fuzzy_score(query, &format!("{} {}", commit.sha, commit.subject)).is_some()
}
