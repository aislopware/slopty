//! Finding sessions: by their titles, projects, branches and machines
//! ([`Shell::thread_matches`]), and by what was said in them (`SearchChats`, [`ChatWordsQuery`]).
//! The palette lists both today; the session picker is to take them up.

use super::*;
use crate::state::EngineHandle;

// What the chats said is searched once typing pauses on two characters or more
// (`SearchChats`), below the title matches.
/// Shorter queries match nearly every chat's words; they are not sent.
pub(super) const MIN_CHAT_WORDS_CHARS: usize = 2;
/// The pause in typing before the chats' words are searched.
pub(super) const CHAT_WORDS_DEBOUNCE: Duration = Duration::from_millis(200);
/// How many matches one search asks for.
pub(super) const CHAT_WORDS_LIMIT: usize = 50;

// One place a chat's words matched (`SearchChats`): the chat, who said it, a line around
// the match and the match's byte range in that line.
/// A chat whose words match the query.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ChatWordsMatch {
    pub(super) chat_id: String,
    /// The agent said it; else the person did.
    pub(super) agent: bool,
    /// The words around the match, on one line.
    pub(super) excerpt: String,
    /// The match's byte range in `excerpt`; empty when the answer's range did not fit it.
    pub(super) range: std::ops::Range<usize>,
}

impl ChatWordsMatch {
    /// One answer's entry, or `None` when it names no chat. A range that is not on the
    /// excerpt's character boundaries is dropped, not the match.
    fn parse(value: &serde_json::Value) -> Option<Self> {
        let chat_id = value.get("chatId")?.as_str().filter(|id| !id.is_empty())?.to_owned();
        let agent = value.get("role").and_then(serde_json::Value::as_str) != Some("user");
        // One line: line breaks and tabs become spaces, each one byte for one, so the range
        // still lands.
        let excerpt: String = value
            .get("excerpt")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .chars()
            .map(|c| if matches!(c, '\n' | '\r' | '\t') { ' ' } else { c })
            .collect();
        let offset = |key: &str| {
            value.get(key).and_then(serde_json::Value::as_u64).and_then(|v| usize::try_from(v).ok())
        };
        let range = match (offset("matchStart"), offset("matchEnd")) {
            (Some(start), Some(end))
                if start < end
                    && end <= excerpt.len()
                    && excerpt.is_char_boundary(start)
                    && excerpt.is_char_boundary(end) =>
            {
                start..end
            }
            _ => 0..0,
        };
        Some(Self { chat_id, agent, excerpt, range })
    }

    /// An answer's matches, first per chat, in the answer's order; anything else answers none.
    pub(super) fn parse_all(answer: &serde_json::Value) -> Vec<Self> {
        let list = answer.as_array().or_else(|| answer.get("matches").and_then(|m| m.as_array()));
        let mut seen = std::collections::HashSet::new();
        list.into_iter()
            .flatten()
            .filter_map(Self::parse)
            .filter(|found| seen.insert(found.chat_id.clone()))
            .collect()
    }
}

// The chats' words search's request state: an answer to an older query is dropped, and
// a failure (an older view model, a server that does not search) shows no matches.
#[derive(Default)]
pub(super) struct ChatWordsQuery {
    /// Bumped per request; an answer to an older one is dropped.
    generation: u64,
    pub(super) loading: bool,
    pub(super) results: Vec<ChatWordsMatch>,
    task: Option<Task<()>>,
}

impl ChatWordsQuery {
    /// Forget the results, cancelling any request in flight.
    pub(super) fn clear(&mut self) {
        self.generation += 1;
        self.loading = false;
        self.results.clear();
        self.task = None;
    }

    /// Search the chats' words for `query` once typing pauses on [`MIN_CHAT_WORDS_CHARS`] or
    /// more, through `engine`; with none, or a shorter query, there are no matches. `query_of`
    /// finds this query in the shell again when the answer comes, which settles it there.
    pub(super) fn search(
        &mut self,
        engine: Option<EngineHandle>,
        query: String,
        query_of: fn(&mut Shell) -> Option<&mut Self>,
        cx: &mut Context<Shell>,
    ) {
        let Some(engine) = engine.filter(|_| query.chars().count() >= MIN_CHAT_WORDS_CHARS) else {
            self.clear();
            return;
        };
        let generation = self.begin();
        let params = serde_json::json!({ "query": query, "limit": CHAT_WORDS_LIMIT });
        self.task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(CHAT_WORDS_DEBOUNCE).await;
            let answer = engine.client().call(methods::SEARCH_CHATS, params).await;
            this.update(cx, |shell, cx| {
                if query_of(shell).is_some_and(|query| query.finish(generation, answer)) {
                    cx.notify();
                }
            })
            .ok();
        }));
    }

    /// Start a request; the returned generation identifies its answer. Rows from the last
    /// answer stay up meanwhile.
    fn begin(&mut self) -> u64 {
        self.generation += 1;
        self.loading = true;
        self.generation
    }

    /// Settle request `generation` with its answer; `false` when a newer one superseded it.
    fn finish(
        &mut self,
        generation: u64,
        answer: Result<serde_json::Value, slopty_view_rpc::RpcError>,
    ) -> bool {
        if self.generation != generation {
            return false;
        }
        self.loading = false;
        self.results = match answer {
            Ok(answer) => ChatWordsMatch::parse_all(&answer),
            Err(error) => {
                tracing::debug!(%error, "palette chat words search failed");
                Vec::new()
            }
        };
        true
    }
}

// A chat words match's second line: who said it, then the words in `text_faint` with
// the match in `text`, one line, cut at the row's end.
/// "Agent: …the words…", the match lit.
pub(super) fn chat_words_line(who: &'static str, found: &ChatWordsMatch, theme: &Theme) -> gpui::Div {
    let lead = who.len() + 1;
    let line: SharedString = format!("{who} {}", found.excerpt).into();
    let mut highlights = vec![(
        0..who.len(),
        gpui::HighlightStyle { color: Some(theme.text_muted), ..Default::default() },
    )];
    if !found.range.is_empty() {
        highlights.push((
            found.range.start + lead..found.range.end + lead,
            gpui::HighlightStyle {
                color: Some(theme.text),
                font_weight: Some(gpui::FontWeight::MEDIUM),
                ..Default::default()
            },
        ));
    }
    div()
        .min_w_0()
        .truncate()
        .text_size(crate::typography::ui_rems(12.0))
        .text_color(theme.text_faint)
        .child(gpui::StyledText::new(line).with_highlights(highlights))
}

impl Shell {
    /// Top-level chats matching every word of `query`, best match first
    /// (see `thread_search`).
    pub(super) fn thread_matches(&self, query: &str, limit: usize, cx: &App) -> Vec<String> {
        let state = self.state.read(cx);
        let terms = thread_search::search_terms(query);
        // Global history deliberately ignores the sidebar's project filter and
        // collapsed groups. Archived conversations remain searchable too.
        // Like the sidebar, only list top-level sessions, not side chats or MCP workers.
        let scored = state
            .chats
            .iter()
            .filter(|chat| chat.is_top_level())
            .filter_map(|chat| {
                let fields = thread_search::ThreadFields {
                    title: chat.title.as_deref().unwrap_or("New session"),
                    project: state
                        .space_for_chat(chat)
                        .map(|s| s.display_name())
                        .unwrap_or(""),
                    branch: super::sidebar_status::branch_of(chat).unwrap_or(""),
                    device: state.device_name(&chat.device_id).unwrap_or(""),
                    archived: chat.archived,
                };
                let score = thread_search::score_thread(&fields, &terms)?;
                let active = chat.last_message_at.unwrap_or(chat.created_at);
                Some((chat.id.clone(), score, active.timestamp_millis()))
            })
            .collect();
        thread_search::rank(scored, limit)
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    // The chats' words: a match each, after the pause, from the answer's shape.
    pub(in crate::shell) fn words(chat: &str, role: &str, excerpt: &str, start: usize, end: usize) -> serde_json::Value {
        serde_json::json!({
            "chatId": chat, "role": role, "excerpt": excerpt,
            "matchStart": start, "matchEnd": end,
        })
    }

    #[test]
    fn a_chat_words_answer_is_read_defensively() {
        let found = ChatWordsMatch::parse_all(&serde_json::json!([
            words("a", "agent", "both tests\npass now", 11, 15),
            words("a", "user", "a second match of a", 0, 1),
            words("b", "user", "naïve pass", 3, 5),
            { "role": "agent", "excerpt": "no chat" },
            words("c", "agent", "short", 2, 99),
        ]));
        assert_eq!(found.len(), 3, "one per chat, none without a chat: {found:?}");
        assert!(found[0].agent);
        assert_eq!(found[0].excerpt, "both tests pass now", "one line");
        assert_eq!(&found[0].excerpt[found[0].range.clone()], "pass");
        assert!(!found[1].agent);
        assert!(found[1].range.is_empty(), "inside a character: no highlight, still a match");
        assert!(found[2].range.is_empty(), "past the excerpt");
        let wrapped =
            ChatWordsMatch::parse_all(&serde_json::json!({ "matches": [words("d", "agent", "x", 0, 1)] }));
        assert_eq!(wrapped.len(), 1);
        assert!(ChatWordsMatch::parse_all(&serde_json::json!({})).is_empty());
    }
}
