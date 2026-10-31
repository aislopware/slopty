//! Sessions planted in Claude Code's own store on a stack's worker, as its TUI leaves them, and
//! taken up from the session picker (⌘K): the recordings `slopty-agent`'s tests hold
//! (`tests/fixtures/conversation`), turns answered in words alone, or records a test writes, laid
//! end to end as one session. The stand-in TUI (`slopty-stub-claude`, `--resume <id>`) takes the
//! session up in a terminal, and the inspector beside it reads what Claude Code wrote.

use std::path::{Path, PathBuf};

use serde_json::Value;
use slopty_e2e::harness::ServedApp;
use slopty_e2e::{A11yNode, Command, Driver, Dump};

use crate::served::{
    STEP, WINDOW, epoch_ms, escaped, holding, middle, rfc3339, says, seen, start, thread_until,
};

/// What the live session that brings the project into the sidebar is asked.
const LIVE: &str = "Say hello";

/// The session id every recording is normalised to.
const RECORDED: &str = "00000000-0000-4000-8000-000000000001";

/// A turn's records, and its subagents' by their transcripts' names.
type Played = (Vec<Value>, Vec<(String, Vec<Value>)>);

/// One turn of a planted session: a recording of `slopty-agent`'s by name, or a prompt answered
/// in words alone.
pub enum Turn {
    Recorded(&'static str),
    Said(&'static str, &'static str),
    /// Records written for the test, as Claude Code writes them, the session's folder `/work`.
    Records(Vec<Value>),
}

/// The recordings `slopty-agent`'s tests hold.
fn recordings() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../slopty-agent/tests/fixtures/conversation")
}

/// The records of the transcript at `path`, each line's JSON.
fn read_records(path: &Path) -> Vec<Value> {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    text.lines().filter_map(|line| serde_json::from_str::<Value>(line).ok()).collect()
}

/// The words of the last assistant record of the transcript at `path`.
fn last_words(path: &Path) -> Option<String> {
    read_records(path)
        .into_iter()
        .filter(|record| record["type"] == "assistant")
        .filter_map(|record| record["message"]["content"].as_array().cloned())
        .flatten()
        .filter(|block| block["type"] == "text")
        .filter_map(|block| block["text"].as_str().map(str::to_owned))
        .next_back()
}

/// What the recording `name` last answers: the words of its last assistant record, as Claude Code
/// wrote them, so a test waits for the end of whichever recording the fixtures hold.
pub fn last_answer(name: &str) -> String {
    last_words(&recordings().join(name).join("transcript.jsonl"))
        .unwrap_or_else(|| panic!("the {name} recording answers in words"))
}

/// What the one subagent the recording `name` ran answered, and the tools it used, as the
/// recording has them: the first line of its last words in its own transcript (`subagents/`),
/// a heading's marks and strong marks left out, and the tool uses the parent's record of the
/// call counts. A test reads them here rather than copying a recording's answer, which a new
/// recording moves.
pub fn subagent_answer(name: &str) -> (String, u64) {
    let dir = recordings().join(name);
    let runs = std::fs::read_dir(dir.join("subagents")).unwrap();
    let runs: Vec<PathBuf> = runs
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
        .collect();
    let [run] = runs.as_slice() else {
        panic!("the {name} recording runs one subagent: {runs:?}");
    };
    let answer = last_words(run)
        .and_then(|words| {
            words
                .lines()
                .map(|line| line.trim_start_matches('#').replace("**", "").trim().to_owned())
                .find(|line| !line.is_empty())
        })
        .unwrap_or_else(|| panic!("the {name} recording's subagent answers in words"));
    let tools = read_records(&dir.join("transcript.jsonl"))
        .iter()
        .find_map(|record| record["toolUseResult"]["totalToolUseCount"].as_u64())
        .unwrap_or_else(|| panic!("the {name} recording counts its subagent's tool uses"));
    (answer, tools)
}

/// Whether the thread, as read (`slopty agent read`), holds an answer of the agent's that says
/// `words` whole: a prompt that asks for those words is not the answer.
pub fn answered(read: &Value, words: &str) -> bool {
    read["turns"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|turn| turn["entries"].as_array())
        .flatten()
        .any(|entry| {
            entry["type"] == "agent" && entry["text"].as_str().map(str::trim) == Some(words.trim())
        })
}

/// The records of turn `n` of a session `id` in `folder`: its ids made its own (`n` in each, so
/// no two turns share one), the session's id and folder put in, the paths under the folder it
/// was recorded in (`/work`) moved under `folder` with it.
fn records(turn: &Turn, n: usize, id: &str, folder: &str) -> Played {
    let own = |text: &str| {
        text.replace(RECORDED, id)
            .replace(
                "00000000-0000-4000-8000-",
                &format!("00000000-0000-4000-{:04x}-", 0x8001_usize.saturating_add(n)),
            )
            .replace("toolu_", &format!("toolu_{n}x"))
            .replace("\"msg_", &format!("\"msg_{n}x"))
            .replace("\"req_", &format!("\"req_{n}x"))
            .replace("/work/", &format!("{folder}/"))
    };
    let parsed = |text: &str| -> Vec<Value> {
        own(text)
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .map(|mut record| {
                if record.get("cwd").is_some() {
                    record["cwd"] = folder.into();
                }
                record
            })
            .collect()
    };
    match turn {
        Turn::Records(lines) => {
            let text = lines.iter().map(Value::to_string).collect::<Vec<_>>().join("\n");
            (parsed(&text), Vec::new())
        }
        Turn::Recorded(name) => {
            let dir = recordings().join(name);
            let main = parsed(&std::fs::read_to_string(dir.join("transcript.jsonl")).unwrap());
            let subagents = std::fs::read_dir(dir.join("subagents"))
                .into_iter()
                .flatten()
                .flatten()
                .map(|entry| {
                    let text = std::fs::read_to_string(entry.path()).unwrap();
                    (entry.file_name().to_string_lossy().into_owned(), parsed(&text))
                })
                .collect();
            (main, subagents)
        }
        Turn::Said(prompt, answer) => {
            let uuid = |k: u32| {
                format!(
                    "00000000-0000-4000-{:04x}-0000000000{k:02}",
                    0x8001_usize.saturating_add(n)
                )
            };
            let common = |k: u32, parent: Option<u32>, kind: &str, message: Value| {
                serde_json::json!({
                    "cwd": folder, "entrypoint": "cli", "gitBranch": "main", "isSidechain": false,
                    "message": message, "parentUuid": parent.map(uuid), "sessionId": id,
                    "timestamp": format!("2026-10-19T02:00:{k:02}.000Z"), "type": kind,
                    "userType": "external", "uuid": uuid(k), "version": "2.1.301",
                })
            };
            let mut said = common(
                2,
                Some(1),
                "assistant",
                serde_json::json!({
                    "content": [{ "type": "text", "text": answer }], "id": format!("msg_{n}x01"),
                    "model": "claude-opus-5-5", "role": "assistant", "stop_reason": "end_turn",
                    "type": "message", "usage": { "input_tokens": 2, "output_tokens": 120 },
                }),
            );
            said["requestId"] = format!("req_{n}x01").into();
            let asked =
                common(1, None, "user", serde_json::json!({ "role": "user", "content": prompt }));
            (vec![asked, said], Vec::new())
        }
    }
}

/// Plant `turns` as Claude Code's session `id` in `folder`, in the store under `home`, as the TUI
/// leaves one: one transcript, each turn following the last (its first record's parent the
/// previous turn's last), its moments laid end to end and ending an hour before the moment the
/// stack pins the app's clock to ([`slopty_e2e::harness::PINNED_MS`]), its subagents'
/// transcripts beside it, and its first prompt in the prompt history.
pub fn plant(home: &Path, folder: &Path, id: &str, turns: &[Turn]) {
    let cwd = folder.to_string_lossy().into_owned();
    let project = home.join(".claude").join("projects").join(escaped(folder));
    let mut played: Vec<Played> =
        turns.iter().enumerate().map(|(n, turn)| records(turn, n, id, &cwd)).collect();
    // Each turn's moments moved to start a minute after the last ended, then all of them to
    // end an hour before the pinned now, so the session reads as one an hour old, at the same
    // times of day, in every run.
    let moment = |record: &Value| record["timestamp"].as_str().and_then(epoch_ms);
    let mut cursor: u64 = 0;
    let mut shifts = Vec::new();
    for (main, _) in &played {
        let first = main.iter().filter_map(moment).min().unwrap_or(0);
        let last = main.iter().filter_map(moment).max().unwrap_or(first);
        let shift = i128::from(cursor).saturating_sub(i128::from(first));
        shifts.push(shift);
        cursor = u64::try_from(i128::from(last).saturating_add(shift))
            .unwrap_or(cursor)
            .saturating_add(60_000);
    }
    let end = slopty_e2e::harness::PINNED_MS.saturating_sub(3_600_000);
    let base = i128::from(end).saturating_sub(i128::from(cursor));
    let moved = |record: &mut Value, shift: i128| {
        if let Some(at) = moment(record) {
            let at = u64::try_from(i128::from(at).saturating_add(shift).saturating_add(base))
                .unwrap_or(at);
            record["timestamp"] = rfc3339(at).into();
        }
    };
    let mut lines = Vec::new();
    let mut last: Option<String> = None;
    let mut first_prompt: Option<String> = None;
    for ((main, subagents), shift) in played.iter_mut().zip(shifts) {
        for record in main.iter_mut() {
            moved(record, shift);
            let sidechain = record["isSidechain"] == true;
            if record.get("uuid").is_some() && record["parentUuid"].is_null() && !sidechain {
                record["parentUuid"] = last.clone().map_or(Value::Null, Value::from);
            }
            if let Some(uuid) = record["uuid"].as_str().filter(|_| !sidechain) {
                last = Some(uuid.to_owned());
            }
            if first_prompt.is_none() && record["type"] == "user" {
                first_prompt = record["message"]["content"].as_str().map(str::to_owned);
            }
            lines.push(record.to_string());
        }
        for (name, records) in subagents.iter_mut() {
            let dir = project.join(id).join("subagents");
            std::fs::create_dir_all(&dir).unwrap();
            let text = records.iter_mut().fold(String::new(), |mut text, record| {
                moved(record, shift);
                text.push_str(&record.to_string());
                text.push('\n');
                text
            });
            std::fs::write(dir.join(name), text).unwrap();
        }
    }
    std::fs::create_dir_all(&project).unwrap();
    let mut transcript = lines.join("\n");
    transcript.push('\n');
    std::fs::write(project.join(format!("{id}.jsonl")), transcript).unwrap();
    let history = home.join(".claude").join("history.jsonl");
    let mut prompts = std::fs::read_to_string(&history).unwrap_or_default();
    let line = serde_json::json!({
        "display": first_prompt.unwrap_or_default(), "pastedContents": {}, "project": cwd,
        "sessionId": id, "timestamp": end.saturating_sub(60_000),
    });
    prompts.push_str(&line.to_string());
    prompts.push('\n');
    std::fs::write(history, prompts).unwrap();
}

/// A stack whose worker's Claude Code store holds the sessions `plant` leaves in `root/work`
/// (holding the recordings' `main.rs` and `notes.txt`), with a live session there that brings
/// the project into the sidebar; the folder, by its real path.
pub async fn stack(machine: &str, plant_in: impl FnOnce(&Path, &Path)) -> (ServedApp, PathBuf) {
    let stack = ServedApp::launch_rooted(machine, &[], |root| {
        // The pinned look (opaque, in the harness's appearance): a settings file of its own
        // replaces the harness's pin, whose frost would draw the machine.
        slopty_e2e::harness::pin_interface(&root.join("app"), slopty_e2e::harness::APPEARANCE)
            .unwrap();
        for dir in ["home", "work"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        let home = std::fs::canonicalize(root.join("home")).unwrap();
        let work = std::fs::canonicalize(root.join("work")).unwrap();
        std::fs::write(work.join("main.rs"), "fn main() {\n    println!(\"hello\");\n}\n").unwrap();
        std::fs::write(work.join("notes.txt"), "alpha\nbeta\ngamma\ndelta\n").unwrap();
        plant_in(&home, &work);
        Vec::new()
    })
    .await
    .unwrap();
    let work = std::fs::canonicalize(stack.path("work")).unwrap();
    let live = start(&stack, machine, &work, LIVE).await;
    thread_until(&stack, &live, "answered", |read| says(read, &format!("Done: {LIVE}"))).await;
    (stack, work)
}

/// The thread a Claude Code session `id` is, as the worker names it and `slopty agent read`
/// reads it.
pub fn thread_of(id: &str) -> String {
    slopty_proto::thread::ThreadId::derived(&["claude-code session", id]).to_string()
}

/// Take up the past session first asked `title`, found by typing it in the session picker
/// (⌘K): its terminal is the main column, the inspector shown beside it, and the pointer off the
/// window.
pub async fn resume(drv: &mut Driver, title: &str) -> Dump {
    drv.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    drv.keys("cmd-k").await.unwrap();
    seen(drv, "the session picker", STEP, |d| holding(d, "Dialog", "Sessions").is_some()).await;
    drv.type_text(title).await.unwrap();
    let row = format!("Past session: {title}");
    let listed =
        seen(drv, "the session in the picker", STEP, |d| holding(d, "Button", &row).is_some())
            .await;
    let (x, y) = middle(holding(&listed, "Button", &row).unwrap());
    drv.click(x, y).await.unwrap();
    // Picked, the card goes and leaves the pointer over what lay under it, whose hover and
    // tooltip would show in whatever frame is read next: the pointer leaves the window.
    drv.ok(&Command::Move { x: -1.0, y: -1.0 }).await.unwrap();
    seen(drv, "the session in its terminal", STEP, |d| {
        holding(d, "Dialog", "Sessions").is_none() && d.a11y.iter().any(|n| n.role == "Terminal")
    })
    .await;
    crate::served::show_inspector(drv).await
}

/// Where the session's terminal ends on the right: what lies past it is the inspector's.
pub fn terminal_right(dump: &Dump) -> f32 {
    dump.a11y.iter().find(|n| n.role == "Terminal").map_or(0.0, |n| n.bounds[0] + n.bounds[2])
}

/// The inspector's part of the frame: the nodes past the terminal.
pub fn inspector(dump: &Dump) -> Vec<&A11yNode> {
    let edge = terminal_right(dump) - 1.0;
    dump.a11y.iter().filter(|n| n.bounds[0] >= edge && n.role != "Terminal").collect()
}

/// The inspector's words, one node a line.
pub fn inspector_words(dump: &Dump) -> String {
    let nodes: Vec<A11yNode> = inspector(dump).into_iter().cloned().collect();
    slopty_e2e::frame_text(&nodes)
}
