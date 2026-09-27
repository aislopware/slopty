//! What the face says in numbers: a turn's model and tokens, a model's name, a time of day,
//! and which files a turn or the whole session changed.
//!
//! Nothing here draws; the view sets these words where they belong (the fold's right edge, its
//! hint, the header's context chip, the changed-files list).

use std::collections::HashMap;

use slopty_proto::conversation::{Body, Entry, Meters, ResultStatus, ThreadId, ToolDetail, Turn};

use super::{rows, tools};

/// A model's id as a person says it: `claude-opus-5-5` is "Opus 5.5",
/// `claude-haiku-4-5-20251001` is "Haiku 4.5". An id of another shape stays as it is.
#[must_use]
pub fn model_name(id: &str) -> String {
    let Some(rest) = id.strip_prefix("claude-") else { return id.to_owned() };
    let mut family = None;
    let mut version: Vec<&str> = Vec::new();
    for part in rest.split(['-', '_']) {
        let is_number = !part.is_empty() && part.chars().all(|c| c.is_ascii_digit());
        match (is_number, family) {
            // A date stamp (`20251001`) ends the version.
            (true, _) if part.len() >= 6 => break,
            (true, _) => version.push(part),
            (false, None) => family = Some(part),
            (false, Some(_)) => {}
        }
    }
    let Some(family) = family.filter(|f| f.chars().all(|c| c.is_ascii_alphabetic())) else {
        return id.to_owned();
    };
    let mut name: String = family.chars().take(1).flat_map(char::to_uppercase).collect();
    name.push_str(family.get(1..).unwrap_or_default());
    if !version.is_empty() {
        name.push(' ');
        name.push_str(&version.join("."));
    }
    name
}

/// The right edge of a turn's fold: the model and the tokens it wrote.
///
/// "Opus 5.5 · 3.1k tokens". `session_model` is the status line's; a turn answered by it alone
/// leaves the name out, since the header says it.
#[must_use]
pub fn turn_meta(turn: &Turn, session_model: Option<&str>) -> Option<String> {
    let names: Vec<String> = turn.models.iter().map(|m| model_name(m)).collect();
    let same = names.len() == 1 && names.first().map(String::as_str) == session_model;
    let mut parts = Vec::new();
    if !names.is_empty() && !same {
        parts.push(names.join(", "));
    }
    if turn.usage.output > 0 {
        parts.push(format!("{} tokens", tools::tokens(turn.usage.output)));
    }
    (!parts.is_empty()).then(|| parts.join(" \u{b7} "))
}

/// The whole of a turn's figures, one fact a line, for the fold's hint.
#[must_use]
pub fn turn_detail(turn: &Turn, window: Option<u64>) -> String {
    let mut lines = Vec::new();
    if !turn.models.is_empty() {
        let names: Vec<String> = turn.models.iter().map(|m| model_name(m)).collect();
        lines.push(names.join(", "));
    }
    if turn.requests > 0 {
        lines.push(tools::count(u64::from(turn.requests), "request", "requests"));
    }
    let u = turn.usage;
    let read = u.context();
    if read > 0 {
        lines.push(format!(
            "{} tokens read, {} from the cache",
            tools::tokens(read),
            tools::tokens(u.cache_read)
        ));
    }
    if u.output > 0 {
        let thinking = if u.thinking > 0 {
            format!(", {} of it thinking", tools::tokens(u.thinking))
        } else {
            String::new()
        };
        lines.push(format!("{} tokens written{thinking}", tools::tokens(u.output)));
    }
    if let Some(context) = turn.context_tokens {
        lines.push(match window.filter(|w| *w > 0) {
            Some(window) => format!(
                "Context {} of {} ({}%)",
                tools::tokens(context),
                tools::tokens(window),
                percent(context, window)
            ),
            None => format!("Context {}", tools::tokens(context)),
        });
    }
    if let Some(mode) = turn.mode.as_deref().filter(|m| *m != "default") {
        lines.push(format!("{} mode", super::approval::mode_label(mode)));
    }
    if let Some(why) = turn.stop.as_deref().and_then(stop_reason) {
        lines.push(why.to_owned());
    }
    lines.join("\n")
}

/// Why the model stopped, when it is worth saying: not at the end of its answer, nor for a
/// tool.
#[must_use]
pub fn stop_reason(stop: &str) -> Option<&'static str> {
    match stop {
        "max_tokens" => Some("Stopped at the output limit"),
        "refusal" => Some("The model declined"),
        "pause_turn" => Some("Paused by the model"),
        "model_context_window_exceeded" => Some("Stopped at the context limit"),
        _ => None,
    }
}

/// `part` of `whole`, in whole percent.
fn percent(part: u64, whole: u64) -> u64 {
    part.saturating_mul(100).checked_div(whole).unwrap_or(0)
}

/// The header's context hint: "Context 34% used · 68k of 200k tokens · $1.24 this session".
#[must_use]
pub fn context_hint(meters: &Meters, last_context: Option<u64>) -> Option<String> {
    let used = meters.context_used_pct?;
    let mut parts = vec![format!("Context {used:.0}% used")];
    match (last_context, meters.context_window) {
        (Some(tokens), Some(window)) => {
            parts.push(format!("{} of {} tokens", tools::tokens(tokens), tools::tokens(window)));
        }
        (None, Some(window)) => parts.push(format!("{} window", tools::tokens(window))),
        _ => {}
    }
    if let Some(cost) = meters.cost_usd.filter(|c| *c > 0.0) {
        parts.push(format!("${cost:.2} this session"));
    }
    Some(parts.join(" \u{b7} "))
}

/// A time of day from ms since the Unix epoch, in this machine's zone: "14:05". `None` for
/// a record with no stamp.
#[must_use]
pub fn clock(ms: u64) -> Option<String> {
    if ms == 0 {
        return None;
    }
    let secs = i64::try_from(ms / 1_000).ok()?;
    let local = secs.checked_add(utc_offset(secs))?;
    let day = local.rem_euclid(86_400);
    Some(format!("{:02}:{:02}", day / 3_600, (day % 3_600) / 60))
}

/// Seconds this machine's zone is ahead of UTC at `secs` past the Unix epoch, kept per hour:
/// a list asks for every visible prompt's time on every frame.
fn utc_offset(secs: i64) -> i64 {
    use core_foundation::date::CFDate;
    use core_foundation::timezone::CFTimeZone;
    thread_local! {
        static HOUR: std::cell::Cell<Option<(i64, i64)>> = const { std::cell::Cell::new(None) };
    }
    /// The Unix epoch in Core Foundation's absolute time, which counts from 2001-01-01.
    const CF_EPOCH: i64 = 978_307_200;
    let hour = secs.div_euclid(3_600);
    if let Some((at, offset)) = HOUR.get()
        && at == hour
    {
        return offset;
    }
    #[expect(clippy::cast_precision_loss, reason = "seconds since 2001, well within f64")]
    let at = CFDate::new(secs.saturating_sub(CF_EPOCH) as f64);
    #[expect(clippy::cast_possible_truncation, reason = "a zone offset in whole seconds")]
    let offset = CFTimeZone::system().seconds_from_gmt(at).round() as i64;
    HOUR.set(Some((hour, offset)));
    offset
}

/// A file one or more edits and writes changed.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FileChange {
    /// Its path as the calls named it.
    pub path: String,
    /// Lines added over its edits.
    pub added: u32,
    /// Lines removed.
    pub removed: u32,
    /// Edits and writes that changed it, oldest first: their thread and entry id.
    pub edits: Vec<(ThreadId, String)>,
    /// A write made it.
    pub created: bool,
}

/// The files `entries` changed, in the order each was first changed. A call that failed or was
/// refused changed nothing.
#[must_use]
pub fn files<'a>(entries: impl IntoIterator<Item = (&'a ThreadId, &'a Entry)>) -> Vec<FileChange> {
    let mut out: Vec<FileChange> = Vec::new();
    let mut at: HashMap<String, usize> = HashMap::new();
    for (thread, entry) in entries {
        let Body::Tool(call) = &entry.body else { continue };
        if call.result.as_ref().is_none_or(|r| r.status != ResultStatus::Ok) {
            continue;
        }
        let (path, created) = match &call.detail {
            ToolDetail::Edit(edit) => (&edit.path, false),
            ToolDetail::Write(write) => {
                (&write.path, write.kind == slopty_proto::conversation::WriteKind::Create)
            }
            _ => continue,
        };
        let (added, removed) = rows::entry_changes(entry);
        let ix = *at.entry(path.clone()).or_insert_with(|| {
            out.push(FileChange {
                path: path.clone(),
                added: 0,
                removed: 0,
                edits: Vec::new(),
                created: false,
            });
            out.len().saturating_sub(1)
        });
        if let Some(file) = out.get_mut(ix) {
            file.added = file.added.saturating_add(added);
            file.removed = file.removed.saturating_add(removed);
            file.created |= created;
            file.edits.push((thread.clone(), entry.id.clone()));
        }
    }
    out
}

/// The directory part of `path` as a list shows it beside the name: the last two components,
/// "src/conversation" rather than "/home/user/work/crates/slopty-ui/src/conversation".
#[must_use]
pub fn short_dir(path: &str) -> String {
    let dir = path.trim_end_matches('/').rsplit_once('/').map_or("", |(dir, _)| dir);
    let parts: Vec<&str> = dir.split('/').filter(|p| !p.is_empty()).collect();
    let keep = parts.len().saturating_sub(2);
    parts.get(keep..).unwrap_or_default().join("/")
}

#[cfg(test)]
mod tests {
    use slopty_proto::conversation::Usage;

    use super::*;
    use crate::conversation::fixtures::scenario;

    #[test]
    fn models_read_as_people_say_them() {
        assert_eq!(model_name("claude-opus-5-5"), "Opus 5.5");
        assert_eq!(model_name("claude-haiku-4-5-20251001"), "Haiku 4.5");
        assert_eq!(model_name("claude-sonnet-4-20250514"), "Sonnet 4");
        assert_eq!(model_name("claude-3-5-sonnet-20241022"), "Sonnet 3.5");
        assert_eq!(model_name("gpt-x"), "gpt-x");
        assert_eq!(model_name("<synthetic>"), "<synthetic>");
    }

    /// A turn's edge names its model only when it is not the session's, and always says what it
    /// wrote; the hint says the rest, the context against the window included.
    #[test]
    fn a_turn_says_its_model_and_tokens() {
        let turn = Turn {
            prompt: "p".to_owned(),
            models: vec!["claude-haiku-4-5-20251001".to_owned()],
            requests: 6,
            usage: Usage {
                input: 50,
                cache_read: 125_375,
                cache_write: 2_344,
                output: 1_942,
                thinking: 1_359,
            },
            context_tokens: Some(22_814),
            mode: Some("plan".to_owned()),
            stop: Some("max_tokens".to_owned()),
            ..Turn::default()
        };
        assert_eq!(
            turn_meta(&turn, Some("Opus 5.5")).as_deref(),
            Some("Haiku 4.5 \u{b7} 1.9k tokens")
        );
        assert_eq!(turn_meta(&turn, Some("Haiku 4.5")).as_deref(), Some("1.9k tokens"));
        assert_eq!(turn_meta(&Turn::default(), None), None);
        assert_eq!(
            turn_detail(&turn, Some(200_000)),
            "Haiku 4.5\n6 requests\n128k tokens read, 125k from the cache\n\
             1.9k tokens written, 1.4k of it thinking\nContext 22.8k of 200k (11%)\n\
             Plan mode\nStopped at the output limit"
        );
    }

    /// The session's files: an edit and then a write to the same file count as one file with
    /// both calls; a failed call changed nothing.
    #[test]
    fn files_add_up_per_path() {
        let model = scenario("edit");
        let main = model.thread(&ThreadId::Main).unwrap();
        let changed = files(main.entries().iter().map(|e| (&ThreadId::Main, e)));
        assert_eq!(changed.len(), 1, "{changed:?}");
        let file = &changed[0];
        assert!(file.path.ends_with("notes.txt"));
        assert_eq!((file.added, file.removed, file.edits.len()), (2, 1, 2));
        assert_eq!(short_dir("/home/user/work/src/view/parts.rs"), "src/view");
        assert_eq!(short_dir("notes.txt"), "");
    }

    #[test]
    fn the_context_hint_says_the_share_the_size_and_the_cost() {
        let meters = Meters {
            context_used_pct: Some(34.0),
            context_window: Some(200_000),
            cost_usd: Some(1.237),
            ..Meters::default()
        };
        assert_eq!(
            context_hint(&meters, Some(68_000)).as_deref(),
            Some("Context 34% used \u{b7} 68.0k of 200k tokens \u{b7} $1.24 this session")
        );
        assert_eq!(context_hint(&Meters::default(), None), None);
    }
}
