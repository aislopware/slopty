//! The thread view of an agent's terminal, rendered by the app over recorded Claude Code
//! sessions that reach it the way a live one would: the transcripts laid out as Claude Code
//! keeps them, and `SessionStart`, the status line and `PermissionRequest` played through the
//! real relay (`slopty hook`, a child of the test). The worker maps them onto its thread model,
//! its table names the terminal, and the tile opens on the thread with no key pressed. Nothing
//! is typed into a shell and no agent runs.
//!
//! What the model writes before the transcript has it comes from Slopty's Claude Code mod: its
//! recorded events are posted to the worker's mod socket as the mod posts them.
//!
//! Goldens: the thread with a request over the composer, light and dark; its questions; a settled
//! turn and a subagent's own thread; a file attached by a drop; the thread on a phone-width window;
//! a step the model is still writing; and the work beyond words (a pasted picture, the plan, a
//! build in the background), folded and with every step open.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use slopty_e2e::{Command, Driver, Dump, Stack};

use crate::gallery::{STEP, first_shell, golden};

/// The thread's renders: room for the list, the request card and the header's chips.
const WINDOW: (f32, f32) = (1000.0, 720.0);
/// An iPhone's portrait width in points, with the height of one.
const PHONE: (f32, f32) = (393.0, 852.0);

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../slopty-agent/tests/fixtures/conversation")
        .join(name)
}

/// Lay the recorded session `name` under the run's directory as Claude Code keeps it (the
/// main transcript and its subagents' beside it), then start it in `session` through the
/// relay and give it a status line (the model and the context in use). Returns the main
/// transcript's path.
async fn start_recorded(stack: &Stack, session: &str, name: &str) -> String {
    let main = stack.path("projects").join("s1.jsonl");
    let subagents = slopty_agent::conversation::subagents_dir(&main);
    std::fs::create_dir_all(&subagents).unwrap();
    std::fs::copy(fixture(name).join("transcript.jsonl"), &main).unwrap();
    if let Ok(agents) = std::fs::read_dir(fixture(name).join("subagents")) {
        for agent in agents {
            let agent = agent.unwrap().path();
            std::fs::copy(&agent, subagents.join(agent.file_name().unwrap())).unwrap();
        }
    }
    start(stack, session, &main).await
}

/// Start the session whose main transcript is `main` in `session` through the relay, and give
/// it a status line (the model and the context in use). Returns the transcript's path.
async fn start(stack: &Stack, session: &str, main: &Path) -> String {
    let transcript = main.to_string_lossy().into_owned();
    let start = json!({
        "hook_event_name": "SessionStart", "source": "startup", "session_id": "s1",
        "transcript_path": transcript, "cwd": stack.path("home"),
    });
    let done = stack.relay_hook(session, &[], &start).unwrap().wait().await.unwrap();
    assert!(done.success(), "the relay ran");
    let status = json!({
        "session_id": "s1", "transcript_path": transcript,
        "model": { "id": "claude-opus-5-5", "display_name": "Opus 5.5" },
        "context_window": { "context_window_size": 200_000, "used_percentage": 34 },
    });
    let line = stack.relay_hook(session, &["statusline", "--command", "true"], &status).unwrap();
    assert!(line.wait_with_output().await.unwrap().status.success(), "the status line ran");
    transcript
}

/// The first `PermissionRequest` of the recorded `permission` session, asked in the session
/// whose transcript is `transcript`.
fn permission_request(transcript: &str) -> Value {
    let hooks = std::fs::read_to_string(fixture("permission").join("hooks.jsonl")).unwrap();
    let mut ask = hooks
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .map(|l| l["input"].clone())
        .find(|input| input["hook_event_name"] == "PermissionRequest")
        .unwrap();
    ask["transcript_path"] = Value::String(transcript.to_owned());
    ask["session_id"] = Value::String("s1".to_owned());
    ask
}

/// The labels of the nodes with `role`.
fn labels(d: &Dump, role: &str) -> Vec<String> {
    d.a11y.iter().filter(|n| n.role == role).filter_map(|n| n.label.clone()).collect()
}

/// Whether a node of any role has a label starting with `prefix`.
fn any_label(d: &Dump, prefix: &str) -> bool {
    d.a11y.iter().any(|n| n.label.as_deref().is_some_and(|l| l.starts_with(prefix)))
}

fn has(d: &Dump, role: &str, label: &str) -> bool {
    d.a11y_node(role, Some(label)).is_some()
}

/// Whether the tile shows its thread.
fn thread_shows(d: &Dump) -> bool {
    has(d, "Group", "Thread")
}

/// Whether a button's label starts with `prefix`.
fn button_starts(d: &Dump, prefix: &str) -> bool {
    labels(d, "Button").iter().any(|l| l.starts_with(prefix))
}

/// An agent's tile opens on its thread, with no key pressed, once the worker's thread table
/// names its terminal: the thread drawn from the worker's own model of it, its settled turns
/// folded, and the request the agent holds stacked over the composer with the answers Claude
/// Code takes. The same, dark.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn an_agent_tile_opens_on_its_thread() {
    let mut stack = Stack::launch("e2e-worker").await.unwrap();
    let dir = stack.dir.path().to_path_buf();
    stack.driver.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    let dump = first_shell(&mut stack.driver).await;
    let session = dump.terminals[0].session.clone();
    let transcript = start_recorded(&stack, &session, "edit").await;
    stack
        .driver
        .wait_for("the thread, face-first", STEP, |d| thread_shows(d) && any_label(d, "Worked"))
        .await
        .unwrap();
    let _held = stack.relay_hook(&session, &[], &permission_request(&transcript)).unwrap();
    let drv = &mut stack.driver;
    let dump = drv
        .wait_for("the request over the composer", STEP, |d| {
            d.a11y.iter().any(|n| n.role == "Dialog")
        })
        .await
        .unwrap();
    let asks = labels(&dump, "Dialog");
    assert!(asks.iter().any(|l| l.starts_with("Allow Bash")), "{asks:?}");
    for answer in ["Allow", "Deny"] {
        assert!(labels(&dump, "Button").iter().any(|l| l == answer), "{answer}: {:#?}", dump.a11y);
    }
    golden(drv, &dir, "thread").await;
    stack.set_appearance("dark").unwrap();
    let drv = &mut stack.driver;
    drv.wait_for("the dark theme", STEP, |d| d.dark).await.unwrap();
    golden(drv, &dir, "thread-dark").await;
    stack.shutdown().await;
}

/// Claude Code asks two questions through `AskUserQuestion`: the request opens over the
/// composer as a questionnaire, one question at a time under its header, each answer with
/// what it means, a field for one's own. A pick and a word of one's own answer them, and the
/// relay hands Claude Code the call's input with the answers keyed by each question's text,
/// as its own dialog would. The questionnaire takes the keyboard from the empty composer.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn an_agents_questions_are_answered_in_the_thread() {
    let mut stack = Stack::launch("e2e-worker").await.unwrap();
    let dir = stack.dir.path().to_path_buf();
    stack.driver.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    let dump = first_shell(&mut stack.driver).await;
    let session = dump.terminals[0].session.clone();
    let transcript = start_recorded(&stack, &session, "edit").await;
    stack
        .driver
        .wait_for("the thread, face-first", STEP, |d| thread_shows(d) && any_label(d, "Worked"))
        .await
        .unwrap();
    let questions = json!([{
        "question": "Which layout should the review use?", "header": "Layout",
        "multiSelect": false,
        "options": [
            { "label": "Split", "description": "Old and new side by side" },
            { "label": "Unified", "description": "One column, changes inline" }
        ]
    }, {
        "question": "Which panes stay open?", "header": "Panes", "multiSelect": true,
        "options": [{ "label": "Files" }, { "label": "Terminal" }]
    }]);
    let ask = json!({
        "hook_event_name": "PermissionRequest", "session_id": "s1",
        "transcript_path": transcript, "cwd": stack.path("home"),
        "tool_name": "AskUserQuestion", "tool_input": { "questions": questions },
    });
    let held = stack.relay_hook(&session, &[], &ask).unwrap();
    let drv = &mut stack.driver;
    let dump = drv
        .wait_for("the questions over the composer", STEP, |d| has(d, "RadioButton", "Unified"))
        .await
        .unwrap();
    assert!(has(&dump, "TextInput", "Other"), "a field for one's own: {:#?}", dump.a11y);
    let split = dump.a11y_node("RadioButton", Some("Split")).unwrap();
    assert!(split.focused, "the first answer has the keyboard: {:#?}", dump.a11y);
    golden(drv, &dir, "thread-questions").await;
    stack.set_appearance("dark").unwrap();
    let drv = &mut stack.driver;
    drv.wait_for("the dark theme", STEP, |d| d.dark).await.unwrap();
    golden(drv, &dir, "thread-questions-dark").await;
    stack.set_appearance("light").unwrap();
    let drv = &mut stack.driver;
    drv.wait_for("the light theme", STEP, |d| !d.dark).await.unwrap();

    click(drv, "RadioButton", "Unified").await;
    // ⌘↵ goes on at once; a newer gpui-kit also goes on by itself a moment after the pick.
    drv.keys("cmd-enter").await.unwrap();
    drv.wait_for("the second question", STEP, |d| has(d, "CheckBox", "Terminal")).await.unwrap();
    click(drv, "CheckBox", "Terminal").await;
    click(drv, "TextInput", "Other").await;
    drv.type_text("Logs").await.unwrap();
    click(drv, "Button", "Submit").await;

    let answered = tokio::time::timeout(STEP, held.wait_with_output()).await.unwrap().unwrap();
    let decision: Value = serde_json::from_slice(&answered.stdout).unwrap();
    assert_eq!(
        decision["hookSpecificOutput"]["decision"]["updatedInput"]["answers"],
        json!({
            "Which layout should the review use?": "Unified",
            "Which panes stay open?": "Terminal, Logs"
        }),
        "{decision:#}"
    );
    stack.shutdown().await;
}

/// Click the middle of the node with `role` and `label`.
async fn click(drv: &mut Driver, role: &str, label: &str) {
    let dump = drv.dump().await.unwrap();
    let node = dump.a11y_node(role, Some(label)).unwrap_or_else(|| panic!("{role} {label}"));
    let [x, y, w, h] = node.bounds;
    drv.click(w.mul_add(0.5, x), h.mul_add(0.5, y)).await.unwrap();
}

/// A settled turn reads as its prompt, one line of what it did and its answer, with the files
/// it changed over the composer; ⌃O opens every step, the subagent's call among them, which
/// opens the subagent's own thread under a bar that names it and leads back.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn a_subagent_has_a_thread_of_its_own() {
    let mut stack = Stack::launch("e2e-worker").await.unwrap();
    let dir = stack.dir.path().to_path_buf();
    stack.driver.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    let dump = first_shell(&mut stack.driver).await;
    let session = dump.terminals[0].session.clone();
    start_recorded(&stack, &session, "tools").await;
    let drv = &mut stack.driver;
    drv.wait_for("the settled turn", STEP, |d| {
        thread_shows(d)
            && button_starts(d, "Worked")
            && labels(d, "Group").iter().any(|l| l.starts_with("Edits"))
    })
    .await
    .unwrap();
    golden(drv, &dir, "thread-settled").await;
    drv.keys("ctrl-o").await.unwrap();
    let card_shows = |d: &Dump| button_starts(d, "Subagent ");
    let dump = scroll_up_until(drv, "the subagent's call", card_shows).await;
    let card = dump
        .a11y
        .iter()
        .find(|n| {
            n.role == "Button" && n.label.as_deref().is_some_and(|l| l.starts_with("Subagent "))
        })
        .unwrap();
    // The scroll can leave the call partly under the tile's header: click its part in the list.
    let list = dump.a11y_node("Group", Some("Thread")).unwrap();
    let [x, y, w, h] = card.bounds;
    let (top, bottom) = (y.max(list.bounds[1]), (y + h).min(list.bounds[1] + list.bounds[3]));
    drv.click(x + w / 2.0, f32::midpoint(top, bottom)).await.unwrap();
    drv.wait_for("the subagent's thread", STEP, |d| {
        labels(d, "Navigation").iter().any(|l| l.starts_with("Subagent "))
            && !labels(d, "Article").is_empty()
    })
    .await
    .unwrap();
    golden(drv, &dir, "thread-subagent").await;
    drv.keys("escape").await.unwrap();
    drv.wait_for("back on the thread", STEP, |d| {
        labels(d, "Navigation").iter().all(|l| !l.starts_with("Subagent ")) && card_shows(d)
    })
    .await
    .unwrap();
    stack.shutdown().await;
}

/// A file dropped on the thread while it shows is attached to the draft: its chip waits over
/// the composer's field, saying how far it got and offering to stop it, and only the chip says
/// so.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn a_file_dropped_on_the_thread_waits_in_the_composer() {
    // Held before its first byte, so the chip reads 0% however fast this machine is.
    let held = [(slopty_e2e::HOLD_UPLOADS_ENV, "1")];
    let mut stack = Stack::launch_with("e2e-worker", &held).await.unwrap();
    let dir = stack.dir.path().to_path_buf();
    // Sparse: a recording's size, free to make.
    let source = stack.path("screen-recording.mov");
    std::fs::File::create(&source).unwrap().set_len(2 << 30).unwrap();
    stack.driver.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    let dump = first_shell(&mut stack.driver).await;
    let session = dump.terminals[0].session.clone();
    start_recorded(&stack, &session, "tools").await;
    let drv = &mut stack.driver;
    let dump = drv
        .wait_for("the settled thread", STEP, |d| thread_shows(d) && button_starts(d, "Worked"))
        .await
        .unwrap();
    let tile = dump.item("terminal").unwrap().bounds;
    drv.drop_files(&[source.as_path()], tile[0] + tile[2] / 2.0, tile[1] + tile[3] / 2.0)
        .await
        .unwrap();
    let dump = drv
        .wait_for("the attachment's chip", STEP, |d| any_label(d, "Attaching screen-recording.mov"))
        .await
        .unwrap();
    assert!(
        !has(&dump, "Button", "Cancel upload"),
        "the header says nothing the chip says: {:#?}",
        dump.a11y
    );
    drv.ok(&Command::Move { x: 1.0, y: 1.0 }).await.unwrap();
    golden(drv, &dir, "thread-attachment").await;
    let stop = drv.dump().await.unwrap();
    let stop = stop.a11y_node("Button", Some("Remove screen-recording.mov")).map(|n| n.bounds);
    let [x, y, w, h] = stop.expect("the chip's way to stop it");
    drv.click(x + w / 2.0, y + h / 2.0).await.unwrap();
    drv.wait_for("the chip gone", STEP, |d| !any_label(d, "Attaching screen-recording.mov"))
        .await
        .unwrap();
    stack.shutdown().await;
}

/// Scroll the thread up a few lines at a time until `shows` holds, as a reader looking for
/// something above the tail would.
async fn scroll_up_until(drv: &mut Driver, what: &str, shows: impl Fn(&Dump) -> bool) -> Dump {
    for _ in 0..60 {
        let dump = drv.dump().await.unwrap();
        if shows(&dump) {
            return dump;
        }
        let list = dump.a11y_node("Group", Some("Thread")).expect("the thread");
        let [x, y, w, h] = list.bounds;
        // The wheel's delta is in lines: a few at a time pass nothing by.
        drv.scroll(x + w / 2.0, y + h / 2.0, 0.0, 5.0).await.unwrap();
    }
    drv.wait_for(what, STEP, shows).await.unwrap()
}

/// On a phone-width window an agent's tile opens on its thread too, and a request the agent
/// asks stacks over the composer there.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn a_phone_opens_on_the_thread() {
    let mut stack = Stack::launch("e2e-worker").await.unwrap();
    let dir = stack.dir.path().to_path_buf();
    stack.driver.ok(&Command::Resize { width: PHONE.0, height: PHONE.1 }).await.unwrap();
    let dump = first_shell(&mut stack.driver).await;
    let session = dump.terminals[0].session.clone();
    let transcript = start_recorded(&stack, &session, "edit").await;
    stack
        .driver
        .wait_for("the thread, unasked", STEP, |d| thread_shows(d) && any_label(d, "Worked"))
        .await
        .unwrap();
    let _held = stack.relay_hook(&session, &[], &permission_request(&transcript)).unwrap();
    let drv = &mut stack.driver;
    drv.wait_for("the request over the composer", STEP, |d| {
        d.a11y.iter().any(|n| n.role == "Dialog")
    })
    .await
    .unwrap();
    golden(drv, &dir, "thread-phone").await;
    stack.shutdown().await;
}

/// The recorded mod session `name`: its batches, each put in `session`, and its transcript's
/// records with their stamps taken out, since an entry stamped long before the follower saw
/// the block it settles is an older one.
fn recorded_mod(name: &str, session: &str) -> (Vec<Value>, Vec<String>) {
    let dir =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../slopty-agent/tests/fixtures/mod").join(name);
    let batches = std::fs::read_to_string(dir.join("events.jsonl"))
        .unwrap()
        .lines()
        .map(|line| {
            let mut batch: Value = serde_json::from_str(line).unwrap();
            batch["session"] = Value::String(session.to_owned());
            batch
        })
        .collect();
    let records = std::fs::read_to_string(dir.join("transcript.jsonl"))
        .unwrap()
        .lines()
        .map(|line| {
            let mut record: Value = serde_json::from_str(line).unwrap();
            record.as_object_mut().unwrap().remove("timestamp");
            record.to_string()
        })
        .collect();
    (batches, records)
}

/// While the model writes a step, the thread shows it after its last item: the answer as it
/// grows and the tool call being prepared. When the step stops and the transcript has its
/// entries, they take the live items' place.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn a_step_being_written_shows_live_until_the_transcript_settles_it() {
    let mut stack = Stack::launch("e2e-worker").await.unwrap();
    let dir = stack.dir.path().to_path_buf();
    stack.driver.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    let dump = first_shell(&mut stack.driver).await;
    let session = dump.terminals[0].session.clone();
    let main = stack.path("projects").join("s1.jsonl");
    std::fs::create_dir_all(main.parent().unwrap()).unwrap();
    std::fs::write(&main, "").unwrap();
    let start = json!({
        "hook_event_name": "SessionStart", "source": "startup", "session_id": "s1",
        "transcript_path": main, "cwd": stack.path("home"),
    });
    let done = stack.relay_hook(&session, &[], &start).unwrap().wait().await.unwrap();
    assert!(done.success(), "the relay ran");
    stack.driver.wait_for("the thread", STEP, thread_shows).await.unwrap();

    // Claude Code writes the prompt before the model answers it; then the first step as the
    // model writes it: its answer, then the Bash call's input.
    let (batches, records) = recorded_mod("bash", &session);
    let prompt = records.iter().position(|l| l.contains(r#""role":"user""#)).unwrap();
    let mut asked = records[..=prompt].join("\n");
    asked.push('\n');
    std::fs::write(&main, asked).unwrap();
    let stop = |b: &Value| b["events"].as_array().unwrap().iter().any(|e| e["kind"] == "stop");
    let first_stop = batches.iter().position(stop).unwrap();
    for batch in &batches[..first_stop] {
        assert_eq!(stack.post_mod(batch).await.unwrap(), 204, "{batch}");
    }
    let writing = "Let me run it.";
    let drv = &mut stack.driver;
    drv.wait_for("the live step", STEP, |d| {
        has(d, "Article", writing) && has(d, "Button", "Preparing Bash")
    })
    .await
    .unwrap();
    golden(drv, &dir, "thread-live").await;

    // The step stops and the transcript gets the answer and the call: both settle, the call
    // by the name the transcript gives it.
    assert_eq!(stack.post_mod(&batches[first_stop]).await.unwrap(), 204);
    let result = records.iter().position(|l| l.contains(r#""type":"tool_result""#)).unwrap();
    let mut transcript = records[..result].join("\n");
    transcript.push('\n');
    std::fs::write(&main, transcript).unwrap();
    stack
        .driver
        .wait_for("the step settled", STEP, |d| {
            has(d, "Button", "Say hi")
                && has(d, "Article", writing)
                && !has(d, "Button", "Preparing Bash")
        })
        .await
        .unwrap();
    stack.shutdown().await;
}

/// A screenshot made up for the session: a window's bar over a header whose chips run into
/// the title, cropped to the header as a person would paste to show it clipping, with the
/// first lines of the page under it.
fn screenshot() -> Vec<u8> {
    let (w, h) = (600_u32, 200_u32);
    let picture = image::RgbImage::from_fn(w, h, |x, y| {
        let chip = |x0: u32| (x0..x0.saturating_add(80)).contains(&x) && (52..84).contains(&y);
        let line =
            |y0: u32, x1: u32| (24..x1).contains(&x) && (y0..y0.saturating_add(10)).contains(&y);
        if y < 28 {
            image::Rgb([232, 232, 236])
        } else if chip(330) || chip(420) || chip(510) {
            image::Rgb([96, 120, 220])
        } else if (24..400).contains(&x) && (60..76).contains(&y) {
            image::Rgb([60, 60, 70])
        } else if (40..96).contains(&y) {
            image::Rgb([248, 248, 250])
        } else if line(120, 520) || line(144, 470) || line(168, 360) {
            image::Rgb([200, 200, 208])
        } else {
            image::Rgb([255, 255, 255])
        }
    });
    let mut png = std::io::Cursor::new(Vec::new());
    picture.write_to(&mut png, image::ImageFormat::Png).unwrap();
    png.into_inner()
}

/// A session made up to show a turn's work beyond its words, laid out under the run's
/// directory as Claude Code keeps it: a prompt with a pasted screenshot, the model's
/// thinking, a plan the person approved, a task list under way, a `Read` of the screenshot,
/// and a build run in the background whose output file the test writes and whose notice says
/// it finished. Returns the main transcript and the build's output file.
fn work_session(stack: &Stack) -> (PathBuf, PathBuf) {
    let main = stack.path("projects").join("s1.jsonl");
    std::fs::create_dir_all(main.parent().unwrap()).unwrap();
    let tasks = stack.path("claude-tmp").join("s1").join("tasks");
    std::fs::create_dir_all(&tasks).unwrap();
    let output = tasks.join("b1.output");
    std::fs::write(&output, "   Compiling slopty-theme v0.1.0\n   Compiling slopty-ui v0.1.0\n")
        .unwrap();
    let data = data_encoding::BASE64.encode(&screenshot());
    let picture = json!({
        "type": "image", "source": { "type": "base64", "media_type": "image/png", "data": data },
    });
    let mut parent = Value::Null;
    let mut out = String::new();
    let mut push = |uuid: &str, at: &str, mut record: Value| {
        if !uuid.is_empty() {
            record["uuid"] = json!(uuid);
            record["parentUuid"] = parent.clone();
            parent = json!(uuid);
        }
        record["timestamp"] = json!(format!("2026-09-27T04:{at}.000Z"));
        record["sessionId"] = json!("s1");
        out.push_str(&record.to_string());
        out.push('\n');
    };
    let assistant = |content: Value| {
        json!({ "type": "assistant", "message": {
            "role": "assistant", "model": "claude-opus-5-5", "content": [content],
            "usage": { "input_tokens": 12, "cache_read_input_tokens": 41_000, "output_tokens": 420 },
        }})
    };
    let result = |id: &str, content: Value, structured: Value| {
        json!({ "type": "user", "message": { "role": "user", "content": [{
            "type": "tool_result", "tool_use_id": id, "content": content,
        }]}, "toolUseResult": structured })
    };
    push(
        "u1",
        "00:00",
        json!({ "type": "user", "permissionMode": "acceptEdits", "message": {
            "role": "user", "content": [
                picture,
                { "type": "text", "text": "The header clips its chips on a narrow window, as here." },
            ],
        }}),
    );
    push(
        "a1",
        "00:09",
        assistant(json!({ "type": "thinking", "thinking":
        "The chips never shrink, so the title takes the squeeze. The header should give way \
         from the right: the model first, then the context, and the changes last." })),
    );
    let plan = "# Let the header's chips give way\n\n\
                1. Measure the chips before the title, at the tile's width.\n\
                2. Drop the model chip first, then the context.\n\
                3. Keep the changes chip to the last, since it opens the diff.\n\n\
                The title keeps at least a third of the header.";
    push(
        "a2",
        "00:12",
        assistant(json!({
            "type": "tool_use", "id": "t1", "name": "ExitPlanMode", "input": { "plan": plan },
        })),
    );
    push(
        "r1",
        "00:30",
        result("t1", json!("User has approved your plan."), json!({ "plan": plan })),
    );
    let todos = json!([
        { "content": "Measure the chips", "activeForm": "Measuring the chips", "status": "completed" },
        { "content": "Drop chips from the right", "activeForm": "Dropping chips from the right",
          "status": "in_progress" },
        { "content": "Check a phone-width window", "activeForm": "Checking a phone-width window",
          "status": "pending" },
    ]);
    push(
        "a3",
        "00:31",
        assistant(json!({
            "type": "tool_use", "id": "t2", "name": "TodoWrite", "input": { "todos": todos },
        })),
    );
    push("r2", "00:31", result("t2", json!("Todos have been modified successfully."), json!({})));
    push(
        "a4",
        "00:40",
        assistant(json!({
            "type": "tool_use", "id": "t3", "name": "Read",
            "input": { "file_path": "/work/shots/header.png" },
        })),
    );
    push(
        "r3",
        "00:41",
        result(
            "t3",
            json!([picture]),
            json!({
                "type": "image", "file": { "base64": data, "type": "image/png" },
            }),
        ),
    );
    push(
        "a5",
        "00:50",
        assistant(json!({
            "type": "tool_use", "id": "t4", "name": "Bash", "input": {
                "command": "cargo build --release", "description": "Build the release binary",
                "run_in_background": true,
            },
        })),
    );
    let started = format!(
        "Command running in background with ID: b1. Output is being written to: {}. You will be \
         notified when it completes.",
        output.display()
    );
    push(
        "r4",
        "00:51",
        result(
            "t4",
            json!(started),
            json!({
                "stdout": "", "stderr": "", "interrupted": false, "backgroundTaskId": "b1",
            }),
        ),
    );
    push(
        "a6",
        "00:55",
        assistant(json!({ "type": "text", "text":
        "The chips now give way from the right, the model first. The release build runs in the \
         background; I will check a phone-width window once it is done." })),
    );
    std::fs::write(&main, out).unwrap();
    (main, output)
}

/// The notice Claude Code queues when the background build ends, as the transcript keeps it.
fn build_finished() -> String {
    let notice = "<task-notification>\n<task-id>b1</task-id>\n<tool-use-id>t4</tool-use-id>\n\
                  <status>completed</status>\n<summary>Background command \"Build the release \
                  binary\" completed (exit code 0)</summary>\n</task-notification>";
    let record = json!({
        "type": "queue-operation", "operation": "enqueue", "content": notice,
        "timestamp": "2026-09-27T04:01:55.000Z", "sessionId": "s1",
    });
    format!("{record}\n")
}

/// A turn's work beyond its words: the pasted screenshot on the prompt, the plan over the
/// composer, and the build run in the background with its last line, following the file it
/// writes as it grows and ending when its notice comes. With every step open (⌃O) the thinking
/// shows, and the screenshot the `Read` returned shows on its call.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn the_thread_shows_the_work_beyond_words() {
    let mut stack = Stack::launch("e2e-worker").await.unwrap();
    let dir = stack.dir.path().to_path_buf();
    stack.driver.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    let dump = first_shell(&mut stack.driver).await;
    let session = dump.terminals[0].session.clone();
    let (main, output) = work_session(&stack);
    start(&stack, &session, &main).await;
    let drv = &mut stack.driver;
    let building = "Build the release binary: Running";
    let picture = "Picture, 600 \u{d7} 200";
    // The agent lists its background work: a chip says one runs, and opens the panel of it.
    drv.wait_for("the background chip", STEP, |d| has(d, "Button", "1 running")).await.unwrap();
    click(drv, "Button", "1 running").await;
    drv.wait_for("the build in the background", STEP, |d| {
        labels(d, "Status").iter().any(|l| l == building)
            && labels(d, "Button").iter().any(|l| l.starts_with("Plan, 1 of 3 done"))
            && has(d, "Button", picture)
    })
    .await
    .unwrap();
    // The build prints on: the bar follows its file.
    let mut file = std::fs::OpenOptions::new().append(true).open(&output).unwrap();
    std::io::Write::write_all(
        &mut file,
        b"   Compiling slopty v0.1.0\n    Finished `release` profile [optimized] target(s) in 1m 04s\n",
    )
    .unwrap();
    std::fs::OpenOptions::new()
        .append(true)
        .open(&main)
        .and_then(|mut f| std::io::Write::write_all(&mut f, build_finished().as_bytes()))
        .unwrap();
    drv.wait_for("the build done", STEP, |d| {
        labels(d, "Status").iter().any(|l| l.starts_with("Build the release binary: Completed"))
    })
    .await
    .unwrap();
    golden(drv, &dir, "thread-work").await;
    drv.keys("ctrl-o").await.unwrap();
    // Every step open: the screenshot the `Read` returned on its call, and above the plan's
    // card the thinking.
    drv.wait_for("every step", STEP, |d| {
        has(d, "Button", "Read /work/shots/header.png") && has(d, "Button", picture)
    })
    .await
    .unwrap();
    golden(drv, &dir, "thread-work-open").await;
    // "Thought for 4 s", or "Thought for a moment": how long it thought, where it says.
    scroll_up_until(drv, "the thinking", |d: &Dump| button_starts(d, "Thought")).await;
    // Above the steps: the prompt, with the screenshot pasted on it.
    let prompt = |d: &Dump| {
        labels(d, "Article").iter().any(|l| l.starts_with("You: ")) && has(d, "Button", picture)
    };
    scroll_up_until(drv, "the prompt", prompt).await;
    stack.shutdown().await;
}

/// A long session made up for measuring: `turns` turns, each a prompt, an answer in Markdown
/// with a list and a fenced block, a command with its output, an edit with its diff and a
/// read, every assistant record carrying a model and its usage as Claude Code writes them.
fn synthetic_session(turns: usize) -> String {
    struct Log {
        out: String,
        n: u64,
        parent: Option<String>,
    }
    impl Log {
        fn push(&mut self, mut record: Value) -> String {
            self.n = self.n.saturating_add(1);
            let uuid = format!("00000000-0000-4000-9000-{:012}", self.n);
            let secs = self.n.saturating_mul(2);
            let stamp = format!(
                "2026-09-27T{:02}:{:02}:{:02}.000Z",
                4_u64.saturating_add(secs / 3_600),
                (secs / 60) % 60,
                secs % 60
            );
            record["uuid"] = Value::String(uuid.clone());
            record["parentUuid"] = self.parent.clone().map_or(Value::Null, Value::String);
            record["timestamp"] = Value::String(stamp);
            record["sessionId"] = Value::String("s1".to_owned());
            record["isSidechain"] = Value::Bool(false);
            self.out.push_str(&record.to_string());
            self.out.push('\n');
            self.parent = Some(uuid.clone());
            uuid
        }

        fn assistant(&mut self, content: &Value) {
            let usage = json!({
                "input_tokens": 12, "cache_read_input_tokens": 41_000,
                "cache_creation_input_tokens": 800, "output_tokens": 420,
            });
            self.push(json!({
                "type": "assistant",
                "message": {
                    "id": format!("msg_{}", self.n), "role": "assistant",
                    "model": "claude-opus-5-5", "content": [content], "usage": usage,
                    "stop_reason": "tool_use",
                },
            }));
        }
    }
    let mut log = Log { out: String::new(), n: 0, parent: None };
    for turn in 0..turns {
        log.push(json!({
            "type": "user",
            "message": { "role": "user", "content": format!(
                "Step {turn}: tighten the parser's error path and run the tests again"
            )},
        }));
        log.assistant(&json!({ "type": "thinking", "thinking": format!(
            "The parser returns early on turn {turn}; the error path loses the span."
        )}));
        log.assistant(&json!({ "type": "text", "text": format!(
            "Turn {turn}. The failure comes from **two** places:\n\n\
             - `parse_header` drops the span when the line is empty\n\
             - `recover` retries without resetting the cursor\n\n\
             ```rust\nfn recover(&mut self) -> Result<(), Error> {{\n    self.cursor = self.mark;\n    self.next()\n}}\n```\n\n\
             I will fix both and run the suite."
        )}));
        let bash = format!("toolu_b{turn}");
        log.assistant(&json!({
            "type": "tool_use", "id": bash, "name": "Bash",
            "input": { "command": "cargo test -p parser", "description": "Run the parser tests" },
        }));
        let stdout = (0..30).map(|i| format!("test case_{i:02} ... ok")).collect::<Vec<_>>();
        log.push(json!({
            "type": "user",
            "message": { "role": "user", "content": [{
                "type": "tool_result", "tool_use_id": bash, "content": stdout.join("\n"),
                "is_error": false,
            }]},
            "toolUseResult": { "stdout": stdout.join("\n"), "stderr": "", "interrupted": false },
        }));
        let edit = format!("toolu_e{turn}");
        log.assistant(&json!({
            "type": "tool_use", "id": edit, "name": "Edit",
            "input": {
                "file_path": "/work/src/parser.rs",
                "old_string": "self.next()", "new_string": "self.cursor = self.mark;\nself.next()",
            },
        }));
        log.push(json!({
            "type": "user",
            "message": { "role": "user", "content": [{
                "type": "tool_result", "tool_use_id": edit, "content": "The file was updated.",
            }]},
            "toolUseResult": {
                "filePath": "/work/src/parser.rs",
                "structuredPatch": [{
                    "oldStart": 40, "oldLines": 6, "newStart": 40, "newLines": 8,
                    "lines": [
                        "     fn recover(&mut self) -> Result<(), Error> {",
                        "-        self.next()",
                        "+        self.cursor = self.mark;",
                        "+        self.next()",
                        "     }",
                        " ",
                        "+    /// The span of the line being read.",
                        "     fn span(&self) -> Span {",
                    ],
                }],
            },
        }));
        let read = format!("toolu_r{turn}");
        log.assistant(&json!({
            "type": "tool_use", "id": read, "name": "Read",
            "input": { "file_path": "/work/src/lexer.rs" },
        }));
        log.push(json!({
            "type": "user",
            "message": { "role": "user", "content": [{
                "type": "tool_result", "tool_use_id": read, "content": "1\tuse std::str;",
            }]},
            "toolUseResult": { "file": { "startLine": 1, "numLines": 120, "totalLines": 120 } },
        }));
        log.assistant(&json!({ "type": "text", "text": format!(
            "Done with turn {turn}: the tests pass and the span survives an empty line."
        )}));
    }
    log.out
}

/// Frame-time measurements, run by `cargo xtask e2e smooth` alone and never beside the app's other
/// tests, whose frames would be counted.
mod frame_time {
    use super::*;

    /// The thread view over a long thread while the model writes an answer, the frame-time case
    /// behind `docs/MEASUREMENTS.md` ("the thread view under a streaming answer").
    ///
    /// (h) the list following the tail while an answer grows by a piece every 16 ms, as Slopty's
    /// Claude Code mod reports it; (i) the same with the reader panning the history at 120
    /// events per second; (j) every step open and panned, nothing streaming; (k) how long a
    /// streamed word takes from the mod's post to a frame that shows it.
    #[tokio::test]
    #[ignore = "live: cargo xtask e2e smooth"]
    async fn the_thread_draws_a_streaming_answer_within_a_frame() {
        let run = std::time::Duration::from_secs(5);
        // The thread keeps GPUI's motion, which the self-test otherwise holds still.
        let mut stack =
            Stack::launch_with("e2e-worker", &[("SLOPTY_E2E_MOTION", "1")]).await.unwrap();
        stack.driver.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
        let dump = first_shell(&mut stack.driver).await;
        let session = dump.terminals[0].session.clone();
        let main = stack.path("projects").join("s1.jsonl");
        std::fs::create_dir_all(main.parent().unwrap()).unwrap();
        std::fs::write(&main, synthetic_session(80)).unwrap();
        let start = json!({
            "hook_event_name": "SessionStart", "source": "startup", "session_id": "s1",
            "transcript_path": main, "cwd": stack.path("home"),
        });
        let done = stack.relay_hook(&session, &[], &start).unwrap().wait().await.unwrap();
        assert!(done.success(), "the relay ran");
        stack
            .driver
            .wait_for("the thread", STEP, |d| thread_shows(d) && labels(d, "Article").len() > 2)
            .await
            .unwrap();

        let (batches, _) = recorded_mod("bash", &session);
        for batch in &batches[..3] {
            assert_eq!(stack.post_mod(batch).await.unwrap(), 204, "{batch}");
        }
        let turn = batches[1]["events"][0]["turnId"].clone();
        let piece = |text: &str| {
            json!({ "session": session, "events": [{
                "kind": "text", "block": 0, "step": 0, "turnId": turn,
                "model": "claude-haiku-4-5-20251001", "text": text,
            }]})
        };
        let words = [
            "The ",
            "parser ",
            "now ",
            "keeps ",
            "the ",
            "span ",
            "through ",
            "`recover`, ",
            "and ",
            "an ",
            "empty ",
            "line ",
            "reads ",
            "as ",
            "one.\n\n",
            "- ",
            "fixed ",
            "`parse_header`\n",
            "- ",
            "reset ",
            "the ",
            "cursor\n\n",
        ];
        assert_eq!(stack.post_mod(&piece("Writing ")).await.unwrap(), 204);
        stack
            .driver
            .wait_for("the live answer", STEP, |d| {
                labels(d, "Article").iter().any(|l| l.starts_with("Writing"))
            })
            .await
            .unwrap();

        // (k) a streamed word to the frame that shows it: each word a token of its own, added to
        // the answer while the list follows its tail, timed from its post to the first dump
        // whose tree has it, so the dump's own round trip, timed alone, is in every sample.
        let ms = |d: std::time::Duration| d.as_secs_f64() * 1e3;
        let mut stale = 0_usize;
        let mut alone = Vec::new();
        for _ in 0..40 {
            let begin = tokio::time::Instant::now();
            drop(moving_dump(&mut stack.driver, &mut stale).await);
            alone.push(ms(begin.elapsed()));
        }
        let mut shown = Vec::new();
        for n in 0..60 {
            let token = format!("t{n:02}");
            let word = json!({ "session": session, "events": [{
                "kind": "text", "block": 0, "step": 0, "turnId": turn,
                "model": "claude-haiku-4-5-20251001", "text": format!("{token} "),
            }]});
            let begin = tokio::time::Instant::now();
            assert_eq!(stack.post_mod(&word).await.unwrap(), 204);
            loop {
                let dump = moving_dump(&mut stack.driver, &mut stale).await;
                let answers = labels(&dump, "Article");
                if answers.iter().any(|l| l.contains(&token)) {
                    break;
                }
                assert!(begin.elapsed() < STEP, "{token} never showed: {answers:?}");
            }
            shown.push(ms(begin.elapsed()));
            tokio::time::sleep(std::time::Duration::from_millis(16)).await;
        }
        let at = |v: &mut Vec<f64>, q: f64| {
            v.sort_by(f64::total_cmp);
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                clippy::cast_precision_loss,
                reason = "a quantile's index in a few dozen samples"
            )]
            let i = ((v.len() as f64 - 1.0) * q).round() as usize;
            v.get(i).copied().unwrap_or_default()
        };
        println!(
            "MEASURE (k) a streamed word to its frame: {:.1} / {:.1} / {:.1} ms p50 / p95 / max \
             ({} words) · a dump alone {:.1} / {:.1} ms p50 / p95 · {stale} dumps a step behind",
            at(&mut shown, 0.5),
            at(&mut shown, 0.95),
            at(&mut shown, 1.0),
            shown.len(),
            at(&mut alone, 0.5),
            at(&mut alone, 0.95),
        );

        let region = stack
            .driver
            .dump_moving()
            .await
            .unwrap()
            .a11y_node("Group", Some("Thread"))
            .expect("the thread")
            .bounds;
        let (x, y) = (region[0] + region[2] / 2.0, region[1] + region[3] / 2.0);
        for (scenario, pan, streams) in [
            ("(h) thread, following a streaming answer", false, true),
            ("(i) thread, panning while it streams", true, true),
            ("(j) thread, every step open, panning", true, false),
        ] {
            if !streams {
                stack.driver.keys("ctrl-o").await.unwrap();
            }
            stack.driver.frames_reset().await.unwrap();
            let begin = tokio::time::Instant::now();
            let mut n = 0_usize;
            while begin.elapsed() < run {
                let word = words[n % words.len()];
                if streams {
                    assert_eq!(stack.post_mod(&piece(word)).await.unwrap(), 204);
                }
                if pan {
                    let dy = if (n / 60).is_multiple_of(2) { 40.0 } else { -40.0 };
                    stack.driver.scroll(x, y, 0.0, dy).await.unwrap();
                    tokio::time::sleep(std::time::Duration::from_millis(8)).await;
                    stack.driver.scroll(x, y, 0.0, dy).await.unwrap();
                    tokio::time::sleep(std::time::Duration::from_millis(8)).await;
                } else {
                    tokio::time::sleep(std::time::Duration::from_millis(16)).await;
                }
                n = n.saturating_add(1);
            }
            let frames = stack.driver.dump_moving().await.unwrap().frames;
            println!("MEASURE {scenario}: {}", frames.row());
            assert!(frames.frames >= 100, "too few frames to judge: {frames:?}");
        }

        stack.shutdown().await;
    }

    /// A dump while the motion runs, counting in `stale` one whose frame a stepped mark moved
    /// on from between its draw and the one from scratch beside it.
    async fn moving_dump(drv: &mut Driver, stale: &mut usize) -> Dump {
        let dump = drv.dump_moving().await.unwrap();
        if dump.stale.is_some() {
            *stale = stale.saturating_add(1);
        }
        dump
    }
}

/// A git repository at `dir` holding one source file, with Claude Code's `code-review` among
/// its project's commands, so the agent there reviews through its own door.
fn review_repo(dir: &Path) {
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=Mira", "-c", "user.email=mira@localhost"])
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    };
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::create_dir_all(dir.join(".claude/commands")).unwrap();
    std::fs::write(dir.join(".claude/commands/code-review.md"), "Review the diff\n").unwrap();
    std::fs::write(dir.join("src/refresh.rs"), REFRESH_BEFORE).unwrap();
    git(&["init", "-q", "-b", "main"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "refresh tokens"]);
}

const REFRESH_BEFORE: &str = "pub fn refresh(client: &Client, token: &Token) -> Result<Token> {\n    let fresh = client.post(\"/refresh\", token)?;\n    Ok(fresh)\n}\n";

const REFRESH_AFTER: &str = "pub fn refresh(client: &Client, token: &Token) -> Result<Token> {\n    let key = IdempotencyKey::new();\n    let fresh = retry(3, || client.post_with(\"/refresh\", token, &key))?;\n    store.save(&fresh)?;\n    Ok(fresh)\n}\n";

/// The ref names under `repo`'s private refs that end with `end`.
fn refs_ending(repo: &Path, end: &str) -> Vec<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["for-each-ref", "--format=%(refname) %(objectname)", "refs/slopty/"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| l.split(' ').next().is_some_and(|r| r.ends_with(end)))
        .map(|l| l.split(' ').nth(1).unwrap_or_default().to_owned())
        .collect()
}

/// Wait until `repo` holds a private ref ending with `end`, and give its commit.
async fn private_ref(repo: &Path, end: &str) -> String {
    let started = tokio::time::Instant::now();
    loop {
        if let Some(found) = refs_ending(repo, end).into_iter().next() {
            return found;
        }
        assert!(started.elapsed() < STEP, "no ref ending {end}");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

/// One record of the session's transcript, as Claude Code writes it.
fn record(uuid: &str, parent: Option<&str>, at: &str, body: Value) -> String {
    let mut record = body;
    record["uuid"] = json!(uuid);
    record["parentUuid"] = parent.map_or(Value::Null, |p| json!(p));
    record["timestamp"] = json!(format!("2026-10-04T09:{at}.000Z"));
    record["sessionId"] = json!("s1");
    record["isSidechain"] = json!(false);
    format!("{record}\n")
}

/// The record Claude Code closes a turn with.
fn turn_ended() -> Value {
    json!({ "type": "system", "subtype": "turn_duration", "durationMs": 30_000, "level": "info" })
}

fn said_by_claude(text: &str) -> Value {
    json!({ "type": "assistant", "message": {
        "role": "assistant", "model": "claude-opus-5-5", "content": [{ "type": "text", "text": text }],
        "stop_reason": "end_turn",
        "usage": { "input_tokens": 12, "cache_read_input_tokens": 41_000, "output_tokens": 420 },
    }})
}

fn append(path: &Path, text: &str) {
    use std::io::Write as _;
    let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    file.write_all(text.as_bytes()).unwrap();
}

/// Claude Code's own review from the review tile: the agent changes a file in a turn, the
/// review tile shows it, and "Review with Claude Code" sends its `/code-review` over the
/// change as the person's turn, typed into the agent's terminal (a stand-in that only keeps
/// what it is given). Its answer, played into the transcript as Claude Code writes one, puts
/// a finding on its line in the diff and keeps one about a file not on show as a note above
/// it. Light and dark.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
#[expect(clippy::too_many_lines, reason = "one review, from the turn to its findings")]
async fn the_agents_own_review_puts_its_findings_on_the_diff() {
    let mut stack = Stack::launch("e2e-worker").await.unwrap();
    let dir = stack.dir.path().to_path_buf();
    stack.driver.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    let repo = stack.path("repo");
    review_repo(&repo);
    first_shell(&mut stack.driver).await;
    let drv = &mut stack.driver;
    let before: Vec<String> =
        drv.dump().await.unwrap().terminals.into_iter().map(|t| t.session).collect();
    drv.open(&["sh", "-c", "printf '\\033]0;Retry the refresh\\007'; exec cat"], 1).await.unwrap();
    let dump = drv
        .wait_for("the agent's terminal", STEP, |d| {
            d.terminals.iter().any(|t| !before.contains(&t.session))
        })
        .await
        .unwrap();
    let session =
        dump.terminals.iter().find(|t| !before.contains(&t.session)).unwrap().session.clone();
    drv.reveal(&session).await.unwrap();

    let main = stack.path("projects").join("s1.jsonl");
    std::fs::create_dir_all(main.parent().unwrap()).unwrap();
    let transcript = main.to_string_lossy().into_owned();
    let hook = |event: &str, more: Value| {
        let mut payload = json!({
            "hook_event_name": event, "session_id": "s1", "transcript_path": transcript,
            "cwd": repo,
        });
        if let (Some(payload), Some(more)) = (payload.as_object_mut(), more.as_object()) {
            payload.extend(more.clone());
        }
        payload
    };
    let relay = async |stack: &Stack, payload: Value| {
        let done = stack.relay_hook(&session, &[], &payload).unwrap().wait().await.unwrap();
        assert!(done.success(), "the relay ran");
    };
    std::fs::write(&main, "").unwrap();
    relay(&stack, hook("SessionStart", json!({ "source": "startup" }))).await;

    // The agent's turn: snapshotted as it begins, the file changed, and snapshotted as it ends.
    let prompt = "Make the refresh retry with one idempotency key";
    relay(&stack, hook("UserPromptSubmit", json!({ "prompt": prompt }))).await;
    append(
        &main,
        &record(
            "u1",
            None,
            "00:00",
            json!({ "type": "user", "message": { "role": "user", "content": prompt } }),
        ),
    );
    private_ref(&repo, "/1-before").await;
    std::fs::write(repo.join("src/refresh.rs"), REFRESH_AFTER).unwrap();
    let file = repo.join("src/refresh.rs").to_string_lossy().into_owned();
    let (old, new) = (
        REFRESH_BEFORE.lines().nth(1).unwrap(),
        REFRESH_AFTER.lines().skip(1).take(3).collect::<Vec<_>>().join("\n"),
    );
    append(
        &main,
        &record(
            "e1",
            Some("u1"),
            "00:10",
            json!({ "type": "assistant", "message": {
                "role": "assistant", "model": "claude-opus-5-5", "stop_reason": "tool_use",
                "content": [{ "type": "tool_use", "id": "toolu_e1", "name": "Edit",
                    "input": { "file_path": file, "old_string": old, "new_string": new } }],
                "usage": { "input_tokens": 12, "cache_read_input_tokens": 41_000, "output_tokens": 220 },
            }}),
        ),
    );
    let patch: Vec<String> =
        std::iter::once(format!(" {}", REFRESH_BEFORE.lines().next().unwrap()))
            .chain(std::iter::once(format!("-{old}")))
            .chain(new.lines().map(|l| format!("+{l}")))
            .chain(REFRESH_BEFORE.lines().skip(2).map(|l| format!(" {l}")))
            .collect();
    append(
        &main,
        &record(
            "r1",
            Some("e1"),
            "00:11",
            json!({ "type": "user",
                "message": { "role": "user", "content": [{ "type": "tool_result",
                    "tool_use_id": "toolu_e1", "content": "The file has been updated." }] },
                "toolUseResult": { "filePath": file, "oldString": old, "newString": new,
                    "originalFile": REFRESH_BEFORE, "replaceAll": false, "userModified": false,
                    "structuredPatch": [{ "oldStart": 1, "oldLines": 4, "newStart": 1,
                        "newLines": 6, "lines": patch }] } }),
        ),
    );
    append(
        &main,
        &record(
            "a1",
            Some("r1"),
            "00:30",
            said_by_claude(
                "The refresh now retries three times with one key, and saves the token.",
            ),
        ),
    );
    append(&main, &record("d1", Some("a1"), "00:31", turn_ended()));
    relay(&stack, hook("Stop", json!({ "stop_hook_active": false }))).await;
    private_ref(&repo, "/1-after").await;

    let drv = &mut stack.driver;
    drv.wait_for("the thread", STEP, |d| thread_shows(d) && button_starts(d, "Review"))
        .await
        .unwrap();
    click(drv, "Button", "Review").await;
    drv.wait_for("the review tile with its door", STEP, |d| {
        d.items.iter().any(|i| i.kind == "review") && has(d, "Button", "Review with Claude Code")
    })
    .await
    .unwrap();
    click(drv, "Button", "Review with Claude Code").await;
    drv.wait_for("the review running", STEP, |d| any_label(d, "Claude Code is reviewing"))
        .await
        .unwrap();

    // What the composer typed is Claude Code's own command over the change, as two commits.
    let (base, head) =
        (private_ref(&repo, "/review-base").await, private_ref(&repo, "/review-head").await);
    let range = format!("{base}...{head}");
    let typed = format!("/code-review {range}");
    stack
        .driver
        .wait_for("the command in the agent's terminal", STEP, |d| {
            d.terminal(&session).is_some_and(|t| t.rows.concat().contains(&typed))
        })
        .await
        .unwrap();
    relay(&stack, hook("UserPromptSubmit", json!({ "prompt": typed }))).await;
    let command = format!(
        "<command-name>/code-review</command-name>\n<command-message>code-review</command-message>\n<command-args>{range}</command-args>"
    );
    append(
        &main,
        &record(
            "u2",
            Some("d1"),
            "01:00",
            json!({ "type": "user", "message": { "role": "user", "content": command } }),
        ),
    );
    let findings = "I reviewed the change and found two things to fix.\n\n\
        1. **The store is not in scope** (`src/refresh.rs:4`)\n   `store` is never passed in, so this does not build. Take it as a parameter.\n\n\
        2. **The README still says refresh never retries** (`README.md:12`)\n   Say that it retries three times with one key.";
    append(&main, &record("a2", Some("u2"), "01:40", said_by_claude(findings)));
    append(&main, &record("d2", Some("a2"), "01:41", turn_ended()));
    relay(&stack, hook("Stop", json!({ "stop_hook_active": false }))).await;

    let drv = &mut stack.driver;
    let dump = drv
        .wait_for("the findings", STEP, |d| any_label(d, "Claude Code raised 2 findings"))
        .await
        .unwrap();
    assert!(
        any_label(&dump, "The README still says refresh never retries"),
        "a note above the diff: {:#?}",
        dump.a11y
    );
    assert!(has(&dump, "Button", "Send 2 comments"), "{:#?}", labels(&dump, "Button"));
    drv.ok(&Command::Move { x: 1.0, y: 1.0 }).await.unwrap();
    golden(drv, &dir, "review-agent").await;
    stack.set_appearance("dark").unwrap();
    let drv = &mut stack.driver;
    drv.wait_for("the dark theme", STEP, |d| d.dark).await.unwrap();
    golden(drv, &dir, "review-agent-dark").await;
    stack.shutdown().await;
}
