//! The prompt search over agents' records written into a temporary home: what a later search
//! reads, how a line that is not a prompt is passed over, and what a bounded search says it
//! skipped. The last test measures a search over this machine's own records, by hand.

#[cfg(test)]
mod history {
    use std::fs;
    use std::io::Write as _;
    use std::path::Path;
    use std::time::Duration;

    use slopty_proto::thread::AgentId;
    use slopty_worker::thread::history::{Ask, Found, History, Limits, Stores};
    use tokio::time::Instant;

    fn line(text: &str, session: &str, at: u64) -> String {
        let line = serde_json::json!({"display": text, "pastedContents": {}, "timestamp": at,
            "project": "/w", "sessionId": session});
        format!("{line}\n")
    }

    fn claude_only(home: &Path) -> Stores {
        Stores { claude: Some(home.join(".claude")), codex: None, pi: None }
    }

    fn search(history: &History, query: &str) -> Found {
        history.search(&Ask { agent: None, cwd: None, query, limit: 50 })
    }

    fn sessions(found: &Found) -> Vec<&str> {
        let mut names: Vec<&str> = found.sessions.iter().map(|s| s.native.as_str()).collect();
        names.sort_unstable();
        names
    }

    fn append(path: &Path, text: &str) {
        let mut file = fs::OpenOptions::new().append(true).open(path).unwrap();
        file.write_all(text.as_bytes()).unwrap();
    }

    /// A line that is not JSON, or cut, is passed over and the lines after it are read; a later
    /// search reads what was appended once its line is whole; a file replaced at the same path
    /// is read again from its start.
    #[test]
    fn a_record_is_read_on_from_where_it_stopped_and_again_when_replaced() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join(".claude");
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("history.jsonl");
        let corrupt = "{\"display\":\"alpha broken\",\"pastedC\n\u{fffd}\u{0}garbage\n";
        fs::write(
            &file,
            [line("alpha one", "s-1", 1), corrupt.to_owned(), line("alpha two", "s-2", 2)].concat(),
        )
        .unwrap();
        let history = History::new(claude_only(home.path()), Limits::default());
        let found = search(&history, "alpha");
        assert_eq!(sessions(&found), ["s-1", "s-2"], "the lines round the corrupt ones are read");
        assert_eq!((found.absent, found.cut), (None, None));

        let whole = line("beta three", "s-3", 3);
        let (head, tail) = whole.split_at(20);
        append(&file, head);
        assert_eq!(sessions(&search(&history, "beta")), Vec::<&str>::new(), "a line not yet whole");
        append(&file, tail);
        assert_eq!(sessions(&search(&history, "beta")), ["s-3"], "read on once it is");
        assert_eq!(sessions(&search(&history, "alpha")), ["s-1", "s-2"], "kept from before");

        let replaced = dir.join("history.new");
        fs::write(&replaced, line("gamma four", "s-4", 4)).unwrap();
        fs::rename(&replaced, &file).unwrap();
        assert_eq!(sessions(&search(&history, "alpha")), Vec::<&str>::new());
        assert_eq!(sessions(&search(&history, "gamma")), ["s-4"]);
    }

    /// With no words, the sessions come prompted last first, each with its last prompt; with
    /// words, the best match first, its matches marked.
    #[test]
    fn sessions_rank_by_their_best_prompt() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join(".claude");
        fs::create_dir_all(&dir).unwrap();
        let lines = [
            line("deploy the docs", "old", 1),
            line("look at the redeployment", "mid", 2),
            line("deploy staging", "new", 3),
            line("and then lunch", "old", 4),
        ];
        fs::write(dir.join("history.jsonl"), lines.concat()).unwrap();
        let history = History::new(claude_only(home.path()), Limits::default());

        let recent = search(&history, "");
        let names: Vec<&str> = recent.sessions.iter().map(|s| s.native.as_str()).collect();
        assert_eq!(names, ["old", "new", "mid"]);
        let last = &recent.sessions[0];
        assert_eq!(last.prompts.len(), 1);
        assert_eq!(last.prompts[0].text, "and then lunch");
        assert_eq!(last.title.as_deref(), Some("deploy the docs"), "named by its first prompt");

        let found = search(&history, "deploy");
        let names: Vec<&str> = found.sessions.iter().map(|s| s.native.as_str()).collect();
        assert_eq!(names, ["new", "old", "mid"], "a word's start beats inside a word, then newer");
        let hit = &found.sessions[1].prompts[0];
        assert_eq!(hit.text, "deploy the docs");
        let span = hit.spans[0];
        assert_eq!(hit.text.get(span.start as usize..span.end as usize), Some("deploy"));
        let only = history.search(&Ask { agent: None, cwd: None, query: "deploy", limit: 1 });
        assert_eq!(only.sessions.len(), 1);
        let elsewhere = history.search(&Ask { agent: None, cwd: Some("/x"), query: "", limit: 9 });
        assert!(elsewhere.sessions.is_empty(), "a folder no prompt was sent in");
    }

    /// A file larger than its share is read from its tail, and a search out of bytes or time
    /// says what it skipped; the next search reads on and the answer comes whole.
    #[test]
    fn a_bounded_search_says_what_it_skipped_and_reads_on() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join(".claude");
        fs::create_dir_all(&dir).unwrap();
        let lines: Vec<String> =
            (0..100).map(|n| line(&format!("prompt {n}"), &format!("s-{n}"), n)).collect();
        let size = u64::try_from(lines.iter().map(String::len).sum::<usize>()).unwrap();
        fs::write(dir.join("history.jsonl"), lines.concat()).unwrap();

        let tail = Limits { file_bytes: size / 2, ..Limits::default() };
        let found = search(&History::new(claude_only(home.path()), tail), "prompt");
        assert!(found.sessions.len() < 55 && found.sessions.len() > 45, "{}", found.sessions.len());
        assert!(found.sessions.iter().any(|s| s.native == "s-99"), "the newest are read");
        assert!(found.cut.as_deref().is_some_and(|cut| cut.contains("oldest")), "{:?}", found.cut);

        let bytes = Limits { scan_bytes: size / 4, ..Limits::default() };
        let history = History::new(claude_only(home.path()), bytes);
        let first = search(&history, "prompt");
        assert!(first.sessions.len() < 30, "{}", first.sessions.len());
        assert!(first.cut.is_some());
        let mut found = first;
        for _ in 0..4 {
            found = search(&history, "prompt");
        }
        assert_eq!(found.sessions.len(), 50, "read on to the end, the limit kept");
        assert_eq!(found.cut, None);

        let time = Limits { scan_time: Duration::ZERO, ..Limits::default() };
        let found = search(&History::new(claude_only(home.path()), time), "prompt");
        assert_eq!(found.sessions, []);
        assert!(found.cut.as_deref().is_some_and(|cut| cut.contains("time")), "{:?}", found.cut);
    }

    /// An agent with no record here, or none Slopty reads, says so instead of finding nothing.
    #[test]
    fn an_agent_without_a_record_says_why() {
        let home = tempfile::tempdir().unwrap();
        let history = History::new(claude_only(home.path()), Limits::default());
        assert!(search(&history, "x").absent.is_some(), "no agent's directory is here");
        let codex = AgentId::named(AgentId::CODEX);
        let ask = Ask { agent: Some(&codex), cwd: None, query: "x", limit: 5 };
        assert!(history.search(&ask).absent.is_some());
        let other = AgentId::named("gemini");
        let ask = Ask { agent: Some(&other), cwd: None, query: "x", limit: 5 };
        assert!(history.search(&ask).absent.is_some_and(|why| why.contains("gemini")));
    }

    /// A Codex session found by its prompts is taken up again by its id while its rollout, Codex's
    /// own record of it, is here: one whose rollout went is found with no start to give.
    #[test]
    fn a_codex_session_is_taken_up_again_only_while_its_rollout_is_here() {
        let home = tempfile::tempdir().unwrap();
        let codex = home.path().join(".codex");
        let kept = "01a1009b-ce5e-7d91-b77c-80127c630fd6";
        let gone = "01a1009b-ce5e-7d91-b77c-80127c630fd7";
        let day = codex.join("sessions").join("2026").join("10").join("03");
        fs::create_dir_all(&day).unwrap();
        let meta = serde_json::json!({"timestamp": "2026-10-03T10:00:00.000Z",
            "type": "session_meta", "payload": {"id": kept, "cwd": "/w/api", "source": "cli"}});
        let rollout = day.join(format!("rollout-2026-10-03T14-12-53-{kept}.jsonl"));
        fs::write(rollout, format!("{meta}\n")).unwrap();
        let said = |session: &str, text: &str| {
            let line =
                serde_json::json!({"session_id": session, "ts": 1_780_559_712, "text": text});
            format!("{line}\n")
        };
        let prompts = [said(kept, "add a retry"), said(gone, "add a retry too")];
        fs::write(codex.join("history.jsonl"), prompts.concat()).unwrap();
        let stores = Stores { claude: None, codex: Some(codex), pi: None };
        let found = search(&History::new(stores, Limits::default()), "retry");
        let resume = |native: &str| {
            found.sessions.iter().find(|s| s.native == native).map(|s| s.resume.clone())
        };
        assert_eq!(resume(kept), Some(slopty_agent::codex::resume_args(kept)));
        assert_eq!(resume(gone), Some(Vec::new()), "found, with nothing to take up");
    }

    /// A session found by its prompts says when it was last active as its folder's listing and
    /// its open thread do: its transcript's last record's time, not its last prompt's; one with
    /// no transcript here says its last prompt's.
    #[test]
    fn a_found_sessions_time_is_its_last_records() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join(".claude");
        let project = dir.join("projects").join("-w");
        fs::create_dir_all(&project).unwrap();
        let lines = [line("fix the parser", "s-1", 1_000), line("and the docs", "s-2", 2_000)];
        fs::write(dir.join("history.jsonl"), lines.concat()).unwrap();
        let answered = serde_json::json!({
            "type": "assistant", "sessionId": "s-1", "timestamp": "2026-10-15T10:01:00.000Z",
            "message": { "role": "assistant", "content": [{ "type": "text", "text": "Done." }] },
        });
        fs::write(project.join("s-1.jsonl"), format!("{answered}\n")).unwrap();
        let history = History::new(claude_only(home.path()), Limits::default());
        let found = search(&history, "");
        let at = |native: &str| {
            found.sessions.iter().find(|s| s.native == native).and_then(|s| s.updated_ms)
        };
        let last = slopty_agent::claude::sessions::last_active(&project.join("s-1.jsonl"));
        assert!(last.is_some());
        assert_eq!(at("s-1"), last, "its transcript's last record");
        assert_eq!(at("s-2"), Some(slopty_core::WallMs::from_millis(2_000)), "its last prompt");
    }

    /// A cold search and a warm one over this machine's own records: how long each takes and
    /// how much they hold, as counts only; no prompt is printed.
    ///
    /// `cargo test --release -p slopty-worker --test history -- --ignored --nocapture measure`.
    #[test]
    #[ignore = "measurement over the person's own records, run by hand"]
    fn measure_a_search_of_this_machines_prompts() {
        let size = |path: &Path| fs::metadata(path).map_or(0, |m| m.len());
        let stores = Stores::here();
        let claude = stores.claude.as_deref().map_or(0, |d| size(&d.join("history.jsonl")));
        let codex = stores.codex.as_deref().map_or(0, |d| size(&d.join("history.jsonl")));
        eprintln!("history files: claude {claude} B, codex {codex} B");
        for query in ["", "the", "login test", "zzqxj"] {
            let history = History::new(Stores::here(), Limits::default());
            let ask = Ask { agent: None, cwd: None, query, limit: 50 };
            let cold = Instant::now();
            let found = history.search(&ask);
            let cold = cold.elapsed();
            let mut warm = Vec::new();
            for _ in 0..5 {
                let at = Instant::now();
                let again = history.search(&ask);
                warm.push(at.elapsed());
                assert_eq!(again.sessions.len(), found.sessions.len());
            }
            warm.sort_unstable();
            let hits: usize = found.sessions.iter().map(|s| s.prompts.len()).sum();
            eprintln!(
                "query {query:?}: cold {cold:?}, warm median {:?} max {:?}; {} sessions, \
                 {hits} prompts shown; cut {}",
                warm[2],
                warm[4],
                found.sessions.len(),
                found.cut.is_some(),
            );
        }
    }
}
