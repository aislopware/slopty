//! Claude Code's past sessions on a real stack: sessions a TUI left in Claude Code's own store on
//! the worker (a transcript under `~/.claude/projects/<folder>/` and the prompts in
//! `~/.claude/history.jsonl`, as Claude Code writes them) are found by what was asked in them,
//! typed in the session picker (⌘K), and taken up again: the stand-in Claude Code TUI
//! (`slopty-stub-claude`) starts on the same session (`--resume <id>`) in a terminal that is the
//! session's main column, and hears what is typed there. Each case is judged by the accessibility
//! tree and by what the stand-ins heard (`STUB_RECORD`, one file per stand-in), and the worker's
//! words in a thread by its read.

use std::path::{Path, PathBuf};

use slopty_e2e::Dump;
use slopty_e2e::harness::ServedApp;

use crate::served::{
    STEP, TURN, all_holding, click, escaped, goldens, heard, holding, rfc3339, says, seen, start,
    thread_until,
};

/// The worker's name, as the CLI names the machine.
const MACHINE: &str = "past";

/// A session the person ran in the TUI in the project: its id, its prompts and its answers.
const PARSER: &str = "6f1c9a52-3b7e-4c1d-9a0e-2f5b8c7d1e40";
const PARSER_ASKED: [&str; 2] = ["Explain the parser", "Now add tests for it"];
const PARSER_SAID: [&str; 2] =
    ["The parser reads tokens one by one.", "Tests added for the parser."];

/// Another, found by a word asked in its second prompt, not its first.
const LEXER: &str = "0b7d4e19-8c2a-4f6b-b3d1-7e9a5c0f2b68";
const LEXER_ASKED: [&str; 2] = ["Rename the lexer module", "Then point every import at scanner"];
const LEXER_SAID: [&str; 2] =
    ["Renamed lexer.rs to scanner.rs.", "Every import points at scanner."];

/// A session a Claude Code still runs in a terminal of the worker's machine.
const HELD: &str = "9d2e7a10-5c4b-4e8f-a1b2-c3d4e5f60718";
const HELD_ASKED: [&str; 1] = ["Profile the startup path"];
const HELD_SAID: [&str; 1] = ["Startup spends most of its time loading fonts."];

/// What the live chat that brings the project into the sidebar is asked.
const LIVE: &str = "Say hello";

/// When the TUI's sessions were first asked, in ms since the epoch: a day and an hour before the
/// moment the stack pins the app's clock to ([`slopty_e2e::harness::PINNED_MS`]), so the picker
/// says "1d" of them, at the same times of day, in every run.
const fn asked_at() -> u64 {
    slopty_e2e::harness::PINNED_MS.saturating_sub(25 * 3_600_000)
}

/// Claude Code's own records of session `id` in `folder`, under `home`: its transcript, one
/// prompt and answer per turn a minute apart from `at` (ms since the epoch), on `main`, and the
/// prompts in its prompt history, as the TUI writes them.
fn seed(home: &Path, folder: &Path, id: &str, at: u64, asked: &[&str], said: &[&str]) {
    let claude = home.join(".claude");
    let project = claude.join("projects").join(escaped(folder));
    std::fs::create_dir_all(&project).unwrap();
    let cwd = folder.to_string_lossy();
    let mut lines = Vec::new();
    let mut parent = serde_json::Value::Null;
    let turn_at =
        |turn: usize| at.saturating_add(u64::try_from(turn).unwrap().saturating_mul(60_000));
    for (turn, (prompt, answer)) in asked.iter().zip(said).enumerate() {
        let common = |uuid: &str, parent: &serde_json::Value| {
            serde_json::json!({
                "uuid": uuid, "parentUuid": parent, "sessionId": id, "cwd": cwd,
                "timestamp": rfc3339(turn_at(turn)), "isSidechain": false,
                "userType": "external", "entrypoint": "cli", "version": "2.1.301",
                "gitBranch": "main",
            })
        };
        let (user, assistant) = (format!("{id}-u{turn}"), format!("{id}-a{turn}"));
        let mut record = common(&user, &parent);
        record["type"] = "user".into();
        record["message"] = serde_json::json!({ "role": "user", "content": prompt });
        lines.push(record.to_string());
        let mut record = common(&assistant, &serde_json::Value::String(user));
        record["type"] = "assistant".into();
        record["requestId"] = format!("req_{id}_{turn}").into();
        record["message"] = serde_json::json!({
            "id": format!("msg_{id}_{turn}"), "type": "message", "role": "assistant",
            "model": "claude-opus-5-5", "content": [{ "type": "text", "text": answer }],
            "stop_reason": "end_turn", "stop_sequence": null,
            "usage": { "input_tokens": 12, "output_tokens": 9 },
        });
        lines.push(record.to_string());
        parent = serde_json::Value::String(assistant);
    }
    let mut transcript = lines.join("\n");
    transcript.push('\n');
    std::fs::write(project.join(format!("{id}.jsonl")), transcript).unwrap();
    let mut history = std::fs::read_to_string(claude.join("history.jsonl")).unwrap_or_default();
    for (turn, prompt) in asked.iter().enumerate() {
        let line = serde_json::json!({
            "display": prompt, "pastedContents": {}, "timestamp": turn_at(turn), "project": cwd,
            "sessionId": id,
        });
        history.push_str(&line.to_string());
        history.push('\n');
    }
    std::fs::write(claude.join("history.jsonl"), history).unwrap();
}

/// Mark session `id` in `folder` as running in a terminal of the machine, as Claude Code marks
/// its live sessions (`~/.claude/sessions/<pid>.json`), held by the live process `pid`.
fn mark_running(home: &Path, folder: &Path, id: &str, pid: u32) {
    let sessions = home.join(".claude").join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    let record = serde_json::json!({
        "pid": pid, "sessionId": id, "cwd": folder.to_string_lossy(), "kind": "interactive",
        "entrypoint": "cli", "version": "2.1.301", "status": "idle",
    });
    std::fs::write(sessions.join(format!("{pid}.json")), record.to_string()).unwrap();
}

/// A stack whose worker's Claude Code store holds the TUI's sessions of `root/repo`, and whose
/// stand-ins each write what they heard to `root/heard-<pid>.json`.
/// The project's folder, by its real path, and the worker's home.
async fn stack() -> (ServedApp, PathBuf, PathBuf) {
    let stack = ServedApp::launch_rooted(MACHINE, &[], |root| {
        // The pinned look (opaque, in the harness's appearance): a settings file of its own
        // replaces the harness's pin, whose frost would draw the machine.
        slopty_e2e::harness::pin_interface(&root.join("app"), slopty_e2e::harness::APPEARANCE)
            .unwrap();
        std::fs::create_dir_all(root.join("home")).unwrap();
        std::fs::create_dir_all(root.join("repo")).unwrap();
        let home = std::fs::canonicalize(root.join("home")).unwrap();
        let repo = std::fs::canonicalize(root.join("repo")).unwrap();
        let at = asked_at();
        seed(&home, &repo, PARSER, at, &PARSER_ASKED, &PARSER_SAID);
        seed(&home, &repo, LEXER, at, &LEXER_ASKED, &LEXER_SAID);
        seed(&home, &repo, HELD, at, &HELD_ASKED, &HELD_SAID);
        let record = root.join(concat!("heard-{", "pid", "}.json"));
        vec![("STUB_RECORD".to_owned(), record.to_string_lossy().into_owned())]
    })
    .await
    .unwrap();
    let repo = std::fs::canonicalize(stack.path("repo")).unwrap();
    let home = std::fs::canonicalize(stack.path("home")).unwrap();
    (stack, repo, home)
}

/// A live chat in `folder`, its turn over: what brings the project into the sidebar.
async fn live_chat(stack: &ServedApp, folder: &Path, prompt: &str) {
    let agent = start(stack, MACHINE, folder, prompt).await;
    thread_until(stack, &agent, "answered", |read| read.to_string().contains("Done:")).await;
}

/// The picker's row of the past session first asked `title`.
fn past<'a>(dump: &'a Dump, title: &str) -> Option<&'a slopty_e2e::A11yNode> {
    holding(dump, "Button", &format!("Past session: {title}"))
}

/// The window's words, one node a line.
fn words(dump: &Dump) -> String {
    slopty_e2e::frame_text(&dump.a11y)
}

/// The sidebar's row of the chat titled `title`, exactly: not the picker's row of a past session
/// or of a live one, which hold the same words and more.
fn chat<'a>(dump: &'a Dump, title: &str) -> Option<&'a slopty_e2e::A11yNode> {
    dump.a11y.iter().find(|n| n.role == "Button" && n.label.as_deref() == Some(title))
}

/// Open the session picker (⌘K) and type `typed` into it.
async fn pick(drv: &mut slopty_e2e::Driver, typed: &str) {
    drv.keys("cmd-k").await.unwrap();
    seen(drv, "the session picker", STEP, |d| holding(d, "Dialog", "Sessions").is_some()).await;
    drv.type_text(typed).await.unwrap();
}

/// The pointer away from what the goldens draw, on the terminal's top right, clear of the
/// picker's card and the chrome: no tooltip or hover over a render.
async fn rest(drv: &mut slopty_e2e::Driver) {
    drv.ok(&slopty_e2e::Command::Move { x: 1300.0, y: 60.0 }).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
}

/// The thread a Claude Code session `id` is, as the worker names it (`ThreadId::derived`) and
/// `slopty agent read` reads it.
fn thread_of(id: &str) -> String {
    slopty_proto::thread::ThreadId::derived(&["claude-code session", id]).to_string()
}

/// The record of the stand-in started on session `id` (`--resume <id>`), once it took a line
/// holding `words` (any line, for none); each stand-in writes its own, `heard-<pid>.json`.
async fn resumed_on(root: &Path, id: &str, words: Option<&str>) -> serde_json::Value {
    let since = std::time::Instant::now();
    loop {
        if let Some(record) = resumed(root, id).filter(|record| {
            words.is_none_or(|words| record["prompts"].to_string().contains(words))
        }) {
            return record;
        }
        assert!(
            since.elapsed() < TURN,
            "no stand-in on {id} took {words:?}: {:?}",
            resumed(root, id)
        );
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
}

/// The record of a stand-in started on session `id`, when one was.
fn resumed(root: &Path, id: &str) -> Option<serde_json::Value> {
    std::fs::read_dir(root).ok()?.flatten().find_map(|entry| {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with("heard-") {
            return None;
        }
        let record = heard(&entry.path());
        let args: Vec<&str> =
            record["args"].as_array()?.iter().filter_map(serde_json::Value::as_str).collect();
        let resumed = args.windows(2).any(|pair| pair == ["--resume", id]);
        resumed.then_some(record)
    })
}

/// A session the person ran in Claude Code's TUI is found in the session picker (⌘K) by a word
/// asked in it, titled by its first prompt, beside where and when it ran, and not the live chat's
/// own; picked, Claude Code takes it up again in a terminal in its folder (`--resume <id>`), that
/// terminal the session's main column, its whole conversation the thread's, and a message typed
/// there goes on with it. A word asked only in a later prompt finds another, that prompt shown
/// under its title with the word lit, and ↓ ⏎ takes it up past the row that would start one.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn a_tui_session_is_found_opened_and_continued() {
    let (mut stack, repo, _home) = stack().await;
    live_chat(&stack, &repo, LIVE).await;
    let root = stack.path("");
    let renders = stack.path("");
    let drv = &mut stack.driver;
    drv.ok(&slopty_e2e::Command::Resize { width: 1320.0, height: 880.0 }).await.unwrap();

    // Typed, the picker finds the TUI's session by its first prompt, not the others.
    pick(drv, "parser").await;
    let found =
        seen(drv, "the session the word finds", STEP, |d| past(d, PARSER_ASKED[0]).is_some()).await;
    assert_eq!(
        all_holding(&found, "Button", "Past session: ").len(),
        1,
        "only the session the word finds:\n{}",
        words(&found)
    );
    let row = past(&found, PARSER_ASKED[0]).unwrap().label.clone().unwrap_or_default();
    assert!(
        row.contains(MACHINE) && row.contains("main") && row.contains("1d"),
        "where and when: {row}"
    );
    rest(drv).await;
    goldens(drv, &renders, "picker-past", &[]).await;

    // Picked, it is a session of the project: Claude Code taken up on it in a terminal, in its
    // folder (`--resume <id>`), that terminal the main column, the card gone.
    click(drv, "Button", &row).await;
    seen(drv, "the session open in its terminal", STEP, |d| {
        chat(d, PARSER_ASKED[0]).is_some()
            && holding(d, "Dialog", "Sessions").is_none()
            && d.a11y.iter().any(|n| n.role == "Terminal")
    })
    .await;
    let record = resumed_on(&root, PARSER, None).await;
    assert_eq!(record["cwd"].as_str().map(Path::new), Some(repo.as_path()), "in its folder");
    rest(drv).await;
    goldens(drv, &renders, "past-sessions-opened", &[]).await;
    // Its whole conversation is the thread's, read from Claude Code's transcript.
    let parser = thread_of(PARSER);
    thread_until(&stack, &parser, "held the whole conversation", |read| {
        PARSER_ASKED.iter().chain(&PARSER_SAID).all(|w| says(read, w))
    })
    .await;

    // Typed in its terminal, a message goes on with the same session, in the same thread.
    let drv = &mut stack.driver;
    crate::served::type_in_terminal(drv, "Keep going").await;
    drv.keys("enter").await.unwrap();
    resumed_on(&root, PARSER, Some("Keep going")).await;
    thread_until(&stack, &parser, "answered in the same thread", |read| {
        says(read, "Done: Keep going") && says(read, PARSER_SAID[1])
    })
    .await;
    let drv = &mut stack.driver;
    seen(drv, "the answer in its terminal", STEP, |d| {
        d.terminals.iter().any(|t| t.lines.iter().any(|line| line.contains("Done: Keep going")))
    })
    .await;

    // A word asked only in a later prompt: that prompt under the title, the row past the one
    // that starts a session named so, reached by ↓ and taken up by ⏎.
    pick(drv, "scanner").await;
    let found = seen(drv, "the session its later prompt finds", STEP, |d| {
        past(d, LEXER_ASKED[0]).is_some() && past(d, PARSER_ASKED[0]).is_none()
    })
    .await;
    let row = past(&found, LEXER_ASKED[0]).unwrap().label.clone().unwrap_or_default();
    assert!(row.contains(LEXER_ASKED[1]), "the prompt that matched is the row's: {row}");
    assert!(
        holding(&found, "Button", "\u{201c}scanner\u{201d}").is_some(),
        "the row that starts one comes first:\n{}",
        words(&found)
    );
    rest(drv).await;
    goldens(drv, &renders, "picker-past-prompt", &[]).await;
    drv.keys("down").await.unwrap();
    drv.keys("enter").await.unwrap();
    let opened = seen(drv, "the found session open", STEP, |d| {
        chat(d, LEXER_ASKED[0]).is_some() && holding(d, "Dialog", "Sessions").is_none()
    })
    .await;
    assert!(chat(&opened, "scanner").is_none(), "no session started named by the word");
    resumed_on(&root, LEXER, None).await;
    thread_until(&stack, &thread_of(LEXER), "held the found conversation", |read| {
        LEXER_SAID.iter().all(|w| says(read, w))
    })
    .await;

    stack.shutdown().await;
}

/// A machine whose Claude Code store cannot be read says so in the picker, in its words, where
/// the sessions would be; once readable, the next keystroke finds them.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn the_picker_says_why_it_found_no_past_session() {
    let (mut stack, repo, home) = stack().await;
    live_chat(&stack, &repo, LIVE).await;
    let store = home.join(".claude").join("projects").join(escaped(&repo));
    let locked = |mode: u32| {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&store, std::fs::Permissions::from_mode(mode)).unwrap();
    };
    locked(0o000);

    let drv = &mut stack.driver;
    drv.ok(&slopty_e2e::Command::Resize { width: 1320.0, height: 880.0 }).await.unwrap();
    pick(drv, "parser").await;
    let said = seen(drv, "why it found nothing", STEP, |d| {
        holding(d, "Status", "Permission denied").is_some()
    })
    .await;
    assert!(past(&said, PARSER_ASKED[0]).is_none(), "{}", words(&said));

    // Readable again, one more keystroke asks again and finds the session.
    locked(0o755);
    drv.type_text(" ").await.unwrap();
    let found =
        seen(drv, "the session once readable", STEP, |d| past(d, PARSER_ASKED[0]).is_some()).await;
    assert!(holding(&found, "Status", "Permission denied").is_none(), "{}", words(&found));

    stack.shutdown().await;
}

/// A session a Claude Code still runs in a terminal of its machine says so in the picker, and is
/// not taken up a second time: picking it says why in the card, and no second Claude Code starts
/// on it. Once that Claude Code ends, the next keystroke finds it free, and picking it takes it up.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn a_session_running_elsewhere_is_shown_not_taken() {
    let (mut stack, repo, home) = stack().await;
    let mut terminal = std::process::Command::new("sleep").arg("600").spawn().unwrap();
    mark_running(&home, &repo, HELD, terminal.id());
    live_chat(&stack, &repo, LIVE).await;
    let root = stack.path("");
    let drv = &mut stack.driver;
    drv.ok(&slopty_e2e::Command::Resize { width: 1320.0, height: 880.0 }).await.unwrap();

    pick(drv, "startup").await;
    let listed = seen(drv, "the held session marked", STEP, |d| {
        past(d, HELD_ASKED[0])
            .is_some_and(|n| n.label.as_deref().is_some_and(|l| l.contains("In a terminal")))
    })
    .await;
    let label = past(&listed, HELD_ASKED[0]).unwrap().label.clone().unwrap();
    click(drv, "Button", &label).await;
    seen(drv, "why it did not open", STEP, |d| {
        holding(d, "Status", "Close it there to continue it here").is_some()
            && holding(d, "Dialog", "Sessions").is_some()
    })
    .await;
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    assert!(resumed(&root, HELD).is_none(), "no second Claude Code took the session up");

    // Once the terminal's Claude Code ends, the session is free to take up.
    let _killed = terminal.kill();
    let _reaped = terminal.wait();
    let drv = &mut stack.driver;
    drv.type_text(" ").await.unwrap();
    let free = seen(drv, "the session free", STEP, |d| {
        past(d, HELD_ASKED[0])
            .is_some_and(|n| n.label.as_deref().is_some_and(|l| !l.contains("In a terminal")))
    })
    .await;
    let label = past(&free, HELD_ASKED[0]).unwrap().label.clone().unwrap();
    click(drv, "Button", &label).await;
    let record = resumed_on(&root, HELD, None).await;
    assert_eq!(record["cwd"].as_str().map(Path::new), Some(repo.as_path()), "in its folder");
    stack.shutdown().await;
}
