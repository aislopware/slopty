//! The prompt search over agents' records written into a temporary home: what a later search
//! reads, how a line that is not a prompt is passed over, and what a bounded search says it
//! skipped. The last test measures a search over this machine's own records, by hand.

#[cfg(test)]
mod history {
    use std::fs;
    use std::io::Write as _;
    use std::path::Path;
    use std::time::{Duration, Instant};

    use slopty_proto::thread::AgentId;
    use slopty_worker::thread::history::{Ask, Found, History, Limits, Stores};

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
