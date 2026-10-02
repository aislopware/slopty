//! What the face says in numbers: a turn's model and tokens, a model's name, a time of day,
//! and which files a turn or the whole session changed.
//!
//! Nothing here draws; the view sets these words where they belong (the fold's right edge, its
//! hint, the header's context chip, the changed-files list).

use std::collections::HashMap;
use std::time::Duration;

use slopty_core::WallMs;
use slopty_proto::conversation::{
    AgentRun, Body, Entry, Meters, ResultStatus, ShellStatus, ThreadId, ToolDetail, Turn,
};

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

/// A model as people say it, and the provider it is reached through.
///
/// The provider is the name's part before a `/`, when it is not the model's own:
/// `Anthropic/Claude Sonnet 4.5` is "Claude Sonnet 4.5" through Anthropic, `canned/canned-1` is
/// "canned-1" through canned, `Canned/Canned` is "Canned" alone, and `claude-opus-5-5` is
/// "Opus 5.5" ([`model_name`]).
#[must_use]
pub fn spoken_model(name: &str) -> (String, Option<String>) {
    let name = name.trim();
    let (provider, model) = name
        .rsplit_once('/')
        .map(|(p, m)| (p.trim(), m.trim()))
        .filter(|(p, m)| !p.is_empty() && !m.is_empty())
        .map_or((None, name), |(p, m)| (Some(p), m));
    let spoken = model_name(model);
    let provider = provider.filter(|p| !p.eq_ignore_ascii_case(&spoken)).map(str::to_owned);
    (spoken, provider)
}

/// The right edge of a turn's fold: the model and the tokens it wrote.
///
/// "Opus 5.5 · 3.1k tokens". `session_model` is the one the composer names; a turn answered by
/// it alone leaves the name out, since the composer says it.
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

/// A rate limit's share from which it is said in the warning tone.
pub const LIMIT_WARN_FROM: f64 = 80.0;

/// What the context popover says: how full the window is, what the session cost, and the
/// account's rate limits, each flagged from [`LIMIT_WARN_FROM`].
#[derive(Clone, PartialEq, Debug)]
pub struct ContextFigures {
    /// Share of the window in use, 0 to 100: the bar's fill.
    pub used_pct: f64,
    /// "68.0k of 200k · 34%", or the share alone when the sizes are not known.
    pub used: String,
    /// "$1.24 this session".
    pub cost: Option<String>,
    /// "5-hour limit 42% · resets 16:30", "7-day limit 11%", and whether each is past its
    /// threshold.
    pub limits: Vec<(String, bool)>,
}

/// The context popover's figures from the status line's `meters` and the last turn's context
/// size; `None` before the status line says how full the window is.
#[must_use]
pub fn context_figures(meters: &Meters, last_context: Option<u64>) -> Option<ContextFigures> {
    let used_pct = meters.context_used_pct?;
    let share = format!("{used_pct:.0}%");
    let used = match (last_context, meters.context_window) {
        (Some(tokens), Some(window)) => {
            format!("{} of {} \u{b7} {share}", tools::tokens(tokens), tools::tokens(window))
        }
        (None, Some(window)) => format!("{share} of {}", tools::tokens(window)),
        _ => format!("{share} used"),
    };
    let cost = meters.cost_usd.filter(|c| *c > 0.0).map(|c| format!("${c:.2} this session"));
    let limit = |name: &str, window: &slopty_proto::conversation::RateWindow| {
        let resets = window
            .resets_at
            .and_then(|secs| clock(WallMs::from_millis(secs.saturating_mul(1_000))))
            .map(|at| format!(" \u{b7} resets {at}"))
            .unwrap_or_default();
        (
            format!("{name} limit {:.0}%{resets}", window.used_pct),
            window.used_pct >= LIMIT_WARN_FROM,
        )
    };
    let limits = [("5-hour", meters.five_hour.as_ref()), ("7-day", meters.seven_day.as_ref())]
        .into_iter()
        .filter_map(|(name, window)| window.map(|w| limit(name, w)))
        .collect();
    Some(ContextFigures { used_pct, used, cost, limits })
}

/// A time of day from ms since the Unix epoch, in this machine's zone: "14:05". `None` for
/// a record with no stamp.
#[must_use]
pub fn clock(at: WallMs) -> Option<String> {
    if at.is_zero() {
        return None;
    }
    let secs = i64::try_from(at.as_millis() / 1_000).ok()?;
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

/// When an entry's record was last added to: a call's result, else its own stamp.
fn settled_ms(entry: &Entry) -> WallMs {
    match &entry.body {
        Body::Tool(call) => call.result.as_ref().map_or(entry.at_ms, |r| r.at_ms.max(entry.at_ms)),
        _ => entry.at_ms,
    }
}

/// How long the model thought before writing the thinking block at `at`: from what it was
/// answering (the entry before it) to the block. `None` when the records carry no time.
#[must_use]
pub fn thought_ms(entries: &[Entry], at: usize) -> Option<u64> {
    let this = entries.get(at)?.at_ms;
    let before = settled_ms(entries.get(at.checked_sub(1)?)?);
    (!before.is_zero() && this > before).then(|| this.millis_since(before))
}

/// When each task went in progress and when it was done, by task id, from the calls that
/// kept the list: a task update, or a whole list written at once.
#[must_use]
pub fn task_times(entries: &[Entry]) -> HashMap<String, (Option<WallMs>, Option<WallMs>)> {
    let mut times: HashMap<String, (Option<WallMs>, Option<WallMs>)> = HashMap::new();
    let mut mark = |id: &str, status: &str, at: WallMs| {
        let slot = times.entry(id.to_owned()).or_default();
        match status {
            "in_progress" => slot.0 = slot.0.or(Some(at)),
            "completed" => slot.1 = slot.1.or(Some(at)),
            _ => {}
        }
    };
    for entry in entries {
        let Body::Tool(call) = &entry.body else { continue };
        if call.result.as_ref().is_none_or(|r| r.status != ResultStatus::Ok) {
            continue;
        }
        let at = settled_ms(entry);
        match &call.detail {
            ToolDetail::TaskUpdate(update) => {
                if let Some(to) = &update.to {
                    mark(&update.task_id, to, at);
                }
            }
            ToolDetail::TodoWrite { todos } => {
                for todo in todos {
                    mark(&todo.id, &todo.status, at);
                }
            }
            _ => {}
        }
    }
    times
}

/// Work running in the background, or that finished since the person's last prompt: the
/// Bash calls and subagents started with `run_in_background`, oldest first.
#[must_use]
pub fn background(entries: &[Entry]) -> Vec<&Entry> {
    let since = entries
        .iter()
        .rev()
        .find(|e| matches!(e.body, Body::Prompt(_)))
        .map_or(WallMs::ZERO, |e| e.at_ms);
    entries
        .iter()
        .filter(|entry| {
            let Body::Tool(call) = &entry.body else { return false };
            match &call.detail {
                ToolDetail::Bash(bash) if bash.background && bash.task_id.is_some() => {
                    bash.status == ShellStatus::Running
                        || bash.finished_ms.unwrap_or(entry.at_ms) >= since
                }
                ToolDetail::Agent(agent) if agent.background => {
                    let ended = agent
                        .duration_ms
                        .map(|d| entry.at_ms.saturating_add(Duration::from_millis(d)));
                    agent.status == AgentRun::Running || ended.unwrap_or(entry.at_ms) >= since
                }
                _ => false,
            }
        })
        .collect()
}

/// The longest a title from a prompt runs, in characters, its ellipsis included.
pub const TITLE_CHARS: usize = 48;

/// What a session is about, as a tile with no title of its own is named: the first line of its
/// first prompt, cut at [`TITLE_CHARS`]. A slash command is not a task, so it is passed over.
#[must_use]
pub fn first_prompt(entries: &[Entry]) -> Option<String> {
    let line = entries.iter().find_map(|entry| match &entry.body {
        Body::Prompt(prompt) if prompt.command.is_none() => {
            prompt.text.text.lines().map(str::trim).find(|l| !l.is_empty())
        }
        _ => None,
    })?;
    if line.chars().count() <= TITLE_CHARS {
        return Some(line.to_owned());
    }
    let cut: String = line.chars().take(TITLE_CHARS.saturating_sub(1)).collect();
    Some(format!("{}\u{2026}", cut.trim_end()))
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
    use slopty_core::WallMs;
    use slopty_proto::conversation::Usage;

    use super::*;
    use crate::conversation::fixtures::scenario;

    /// A session is named by its first prompt's first line, cut short; a slash command
    /// before it names nothing.
    #[test]
    fn a_session_is_named_by_its_first_prompt() {
        let prompt = |id: &str, text: &str, command: Option<&str>| Entry {
            id: id.to_owned(),
            at_ms: WallMs::ZERO,
            body: Body::Prompt(slopty_proto::conversation::Prompt {
                text: slopty_proto::conversation::Clipped {
                    text: text.to_owned(),
                    lines: 1,
                    chars: 1,
                    full: None,
                },
                images: Vec::new(),
                command: command.map(str::to_owned),
            }),
        };
        assert_eq!(first_prompt(&[]), None);
        let short =
            [prompt("p0", "", Some("/clear")), prompt("p1", "\nFix the header\nmore", None)];
        assert_eq!(first_prompt(&short).as_deref(), Some("Fix the header"));
        let long = [prompt(
            "p1",
            "Let the header's chips give way from the right on a narrow window",
            None,
        )];
        let title = first_prompt(&long).unwrap_or_default();
        assert_eq!(title, "Let the header's chips give way from the right\u{2026}");
        assert_eq!(title.chars().count(), TITLE_CHARS - 1, "the cut drops its trailing space");
    }

    #[test]
    fn models_read_as_people_say_them() {
        assert_eq!(model_name("claude-opus-5-5"), "Opus 5.5");
        assert_eq!(model_name("claude-haiku-4-5-20251001"), "Haiku 4.5");
        assert_eq!(model_name("claude-sonnet-4-20250514"), "Sonnet 4");
        assert_eq!(model_name("claude-3-5-sonnet-20241022"), "Sonnet 3.5");
        assert_eq!(model_name("gpt-x"), "gpt-x");
        assert_eq!(model_name("<synthetic>"), "<synthetic>");
    }

    /// A model is one name, as people say it, whatever shape the agent gives it in: a
    /// provider's prefix goes beside it, and a provider named as its model says nothing.
    #[test]
    fn a_model_is_one_name_with_its_provider_beside_it() {
        let spoken = |name| spoken_model(name);
        assert_eq!(spoken("Canned/Canned"), ("Canned".to_owned(), None));
        assert_eq!(spoken("canned/canned-1"), ("canned-1".to_owned(), Some("canned".to_owned())));
        assert_eq!(
            spoken("Anthropic/Claude Sonnet 4.5"),
            ("Claude Sonnet 4.5".to_owned(), Some("Anthropic".to_owned()))
        );
        assert_eq!(spoken("claude-opus-5-5"), ("Opus 5.5".to_owned(), None));
        assert_eq!(spoken(" Canned "), ("Canned".to_owned(), None));
        assert_eq!(spoken("/odd/"), ("/odd/".to_owned(), None));
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

    /// The popover says how full the window is in tokens and as a share, the session's cost,
    /// and each rate limit with its reset, flagged from 80 %.
    #[test]
    fn the_context_figures_say_the_window_the_cost_and_the_limits() {
        use slopty_proto::conversation::RateWindow;
        let meters = Meters {
            context_used_pct: Some(34.0),
            context_window: Some(200_000),
            cost_usd: Some(1.237),
            five_hour: Some(RateWindow { used_pct: 84.0, resets_at: None }),
            seven_day: Some(RateWindow { used_pct: 11.2, resets_at: None }),
            ..Meters::default()
        };
        let figures = context_figures(&meters, Some(68_000)).expect("the share is known");
        assert_eq!(figures.used, "68.0k of 200k \u{b7} 34%");
        assert_eq!(figures.cost.as_deref(), Some("$1.24 this session"));
        assert_eq!(
            figures.limits,
            [("5-hour limit 84%".to_owned(), true), ("7-day limit 11%".to_owned(), false)]
        );
        let at = 1_790_000_000_u64;
        let resets = Meters {
            five_hour: Some(RateWindow { used_pct: 42.0, resets_at: Some(at) }),
            ..meters
        };
        let said = context_figures(&resets, None).expect("known").limits[0].0.clone();
        let clock = clock(WallMs::from_millis(at * 1_000)).expect("a time");
        assert_eq!(said, format!("5-hour limit 42% \u{b7} resets {clock}"));
        assert_eq!(context_figures(&resets, None).map(|f| f.used).as_deref(), Some("34% of 200k"));
        assert_eq!(context_figures(&Meters::default(), None), None);
    }

    /// Thinking took from what it answered to when it was written; a task's times come from
    /// the list that moved it; background work stays in view while it runs and after it ends
    /// until the next prompt.
    #[test]
    fn the_work_session_reads_its_times() {
        let dir = tempfile::tempdir().unwrap();
        let mut model = crate::conversation::model::Model::default();
        for event in crate::conversation::fixtures::work(dir.path()) {
            model.apply(event);
        }
        let entries = model.thread(&ThreadId::Main).unwrap().entries();
        let thinking = entries.iter().position(|e| matches!(e.body, Body::Thinking(_))).unwrap();
        assert_eq!(thought_ms(entries, thinking), Some(9_000));
        assert_eq!(thought_ms(entries, 0), None, "nothing before the first entry");
        let times = task_times(entries);
        assert_eq!(times.values().filter(|(start, _)| start.is_some()).count(), 1, "{times:?}");
        assert_eq!(times.values().filter(|(_, end)| end.is_some()).count(), 1, "{times:?}");
        let work: Vec<&str> = background(entries).iter().map(|e| e.id.as_str()).collect();
        assert_eq!(work, ["t4"]);
    }
}
