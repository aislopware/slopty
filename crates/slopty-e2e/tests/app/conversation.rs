//! The conversation face of an agent's terminal, rendered by the app over recorded Claude Code
//! sessions that reach it the way a live one would: the transcripts laid out as Claude Code
//! keeps them, and `SessionStart`, the status line and `PermissionRequest` played through the
//! real relay (`slopty hook`, a child of the test). The worker decodes them, streams the
//! conversation to the app once ⌘J shows the face, and holds the permission prompt for it.
//! Nothing is typed into a shell and no agent runs.
//!
//! What the model writes before the transcript has it comes from Slopty's Claude Code mod: its
//! recorded events are posted to the worker's mod socket as the mod posts them.
//!
//! Goldens: the face with a held prompt over an edit's diff, light and dark; a subagent's own
//! thread; the face on a phone-width window, where it is the default; a step the model is
//! still writing; and the work beyond words (a pasted picture, a plan, the task list, a build
//! in the background), folded and in Verbose.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use slopty_e2e::{Command, Dump, Stack};

use crate::gallery::{STEP, first_shell, gated, golden};

/// The face's renders: room for the list, the approval card and the header's chips.
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

/// An agent working through an edit asks to run a command: the face shows the turn it is on,
/// the edit's diff in it, and the prompt in the composer's place with its three answers; the
/// header says how many lines changed, how full the context is and which model it is. The
/// same, dark.
#[tokio::test]
async fn the_face_holds_a_prompt_over_the_turn_it_interrupts() {
    if !gated() {
        return;
    }
    let mut stack = Stack::launch("e2e-worker").await.unwrap();
    let dir = stack.dir.path().to_path_buf();
    stack.driver.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    let dump = first_shell(&mut stack.driver).await;
    let session = dump.terminals[0].session.clone();
    let transcript = start_recorded(&stack, &session, "edit").await;
    stack.driver.keys("cmd-j").await.unwrap();
    stack
        .driver
        .wait_for("the conversation", STEP, |d| {
            has(d, "Group", "Conversation") && !labels(d, "Article").is_empty()
        })
        .await
        .unwrap();
    let _held = stack.relay_hook(&session, &[], &permission_request(&transcript)).unwrap();
    let drv = &mut stack.driver;
    let dump = drv
        .wait_for("the prompt held for the face", STEP, |d| {
            has(d, "AlertDialog", "Claude wants to run a command")
        })
        .await
        .unwrap();
    for answer in ["Allow once", "Always allow", "Deny"] {
        assert!(labels(&dump, "Button").iter().any(|l| l == answer), "{answer}: {:#?}", dump.a11y);
    }
    golden(drv, &dir, "conversation").await;
    stack.set_appearance("dark").unwrap();
    let drv = &mut stack.driver;
    drv.wait_for("the dark theme", STEP, |d| d.dark).await.unwrap();
    golden(drv, &dir, "conversation-dark").await;
    stack.shutdown().await;
}

/// A settled turn reads as its prompt, one line of what it did and its answer, with the files
/// it changed; a subagent's card opens its own thread, under a bar that names it and leads
/// back.
#[tokio::test]
async fn a_subagent_has_a_thread_of_its_own() {
    if !gated() {
        return;
    }
    let mut stack = Stack::launch("e2e-worker").await.unwrap();
    let dir = stack.dir.path().to_path_buf();
    stack.driver.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    let dump = first_shell(&mut stack.driver).await;
    let session = dump.terminals[0].session.clone();
    start_recorded(&stack, &session, "tools").await;
    let drv = &mut stack.driver;
    drv.keys("cmd-j").await.unwrap();
    drv.wait_for("the conversation", STEP, |d| has(d, "Group", "Conversation")).await.unwrap();
    // Settled: the prompt, the turn folded to its figures, the answer and the files it changed.
    drv.wait_for("the settled turn", STEP, |d| {
        labels(d, "Button").iter().any(|l| l.starts_with("Worked for 35 s"))
            && has(d, "List", "Changed files")
    })
    .await
    .unwrap();
    golden(drv, &dir, "conversation-settled").await;
    // Every step shown: the settled turn opens, the subagent's card with it, above the tail.
    drv.keys("ctrl-o ctrl-o").await.unwrap();
    drv.wait_for("every step", STEP, |d| any_label(d, "Updated a task")).await.unwrap();
    let card_shows = |d: &Dump| {
        d.a11y
            .iter()
            .any(|n| n.label.as_deref().is_some_and(|l| l.starts_with("Subagent Count lines")))
    };
    let dump = scroll_up_until(drv, "the subagent's card", card_shows).await;
    let card = dump
        .a11y
        .iter()
        .find(|n| n.label.as_deref().is_some_and(|l| l.starts_with("Subagent Count lines")))
        .unwrap();
    // The scroll can leave the card partly under the tile's header: click its part in the list.
    let list = dump
        .a11y
        .iter()
        .find(|n| n.role == "Group" && n.label.as_deref() == Some("Conversation"))
        .unwrap();
    let [x, y, w, h] = card.bounds;
    let (top, bottom) = (y.max(list.bounds[1]), (y + h).min(list.bounds[1] + list.bounds[3]));
    drv.click(x + w / 2.0, f32::midpoint(top, bottom)).await.unwrap();
    drv.wait_for("the subagent's thread", STEP, |d| has(d, "Navigation", "Subagent Count lines"))
        .await
        .unwrap();
    golden(drv, &dir, "conversation-subagent").await;
    stack.shutdown().await;
}

/// A file dropped on the face while it shows is attached to the draft: its chip waits over the
/// composer's field, saying how far it got and offering to stop it, and only the chip says so.
#[tokio::test]
async fn a_file_dropped_on_the_face_waits_in_the_composer() {
    if !gated() {
        return;
    }
    let mut stack = Stack::launch("e2e-worker").await.unwrap();
    let dir = stack.dir.path().to_path_buf();
    // Sparse: big enough to still be on its way when the frame is drawn, free to make.
    let source = stack.path("screen-recording.mov");
    std::fs::File::create(&source).unwrap().set_len(2 << 30).unwrap();
    stack.driver.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    let dump = first_shell(&mut stack.driver).await;
    let session = dump.terminals[0].session.clone();
    start_recorded(&stack, &session, "tools").await;
    let drv = &mut stack.driver;
    drv.keys("cmd-j").await.unwrap();
    let dump = drv
        .wait_for("the settled conversation", STEP, |d| {
            has(d, "Group", "Conversation") && has(d, "List", "Changed files")
        })
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
    // Drawn at once, as the transfers golden is: the upload is still near its start.
    drv.ok(&Command::Move { x: 1.0, y: 1.0 }).await.unwrap();
    let frame = drv.render(&dir.join("conversation-attachment.png")).await.unwrap();
    slopty_e2e::snapshot::assert_matches(
        "conversation-attachment",
        &frame,
        slopty_e2e::snapshot::MAC_TOLERANCE,
        &slopty_e2e::harness::artifacts_dir(),
    )
    .unwrap();
    let stop = drv.dump().await.unwrap();
    let stop = stop.a11y_node("Button", Some("Remove screen-recording.mov")).map(|n| n.bounds);
    let [x, y, w, h] = stop.expect("the chip's way to stop it");
    drv.click(x + w / 2.0, y + h / 2.0).await.unwrap();
    drv.wait_for("the chip gone", STEP, |d| !any_label(d, "Attaching screen-recording.mov"))
        .await
        .unwrap();
    stack.shutdown().await;
}

/// Scroll the conversation up a few lines at a time until `shows` holds, as a reader looking
/// for something above the tail would.
async fn scroll_up_until(
    drv: &mut slopty_e2e::Driver,
    what: &str,
    shows: impl Fn(&Dump) -> bool,
) -> Dump {
    for _ in 0..60 {
        let dump = drv.dump().await.unwrap();
        if shows(&dump) {
            return dump;
        }
        let list = dump.a11y_node("Group", Some("Conversation")).expect("the conversation");
        let [x, y, w, h] = list.bounds;
        // The wheel's delta is in lines: a few at a time pass nothing by.
        drv.scroll(x + w / 2.0, y + h / 2.0, 0.0, 5.0).await.unwrap();
    }
    drv.wait_for(what, STEP, shows).await.unwrap()
}

/// On a phone-width window an agent's tile opens on its conversation, with no key pressed,
/// and a prompt the agent asks takes the composer's place there too.
#[tokio::test]
async fn a_phone_opens_on_the_conversation() {
    if !gated() {
        return;
    }
    let mut stack = Stack::launch("e2e-worker").await.unwrap();
    let dir = stack.dir.path().to_path_buf();
    stack.driver.ok(&Command::Resize { width: PHONE.0, height: PHONE.1 }).await.unwrap();
    let dump = first_shell(&mut stack.driver).await;
    let session = dump.terminals[0].session.clone();
    let transcript = start_recorded(&stack, &session, "edit").await;
    stack
        .driver
        .wait_for("the conversation, unasked", STEP, |d| {
            has(d, "Group", "Conversation") && !labels(d, "Article").is_empty()
        })
        .await
        .unwrap();
    let _held = stack.relay_hook(&session, &[], &permission_request(&transcript)).unwrap();
    let drv = &mut stack.driver;
    drv.wait_for("the prompt held for the face", STEP, |d| {
        has(d, "AlertDialog", "Claude wants to run a command")
    })
    .await
    .unwrap();
    golden(drv, &dir, "conversation-phone").await;
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

/// While the model writes a step, the face shows it after the thread's last entry in a lighter
/// tone: the answer as it grows and the tool call being prepared. When the step stops and the
/// transcript has its entries, they take the live blocks' place.
#[tokio::test]
async fn a_step_being_written_shows_live_until_the_transcript_settles_it() {
    if !gated() {
        return;
    }
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
    stack.driver.keys("cmd-j").await.unwrap();
    stack
        .driver
        .wait_for("the conversation", STEP, |d| has(d, "Group", "Conversation"))
        .await
        .unwrap();

    // The first step as the model writes it: its answer, then the Bash call's input.
    let (batches, records) = recorded_mod("bash", &session);
    let stop = |b: &Value| b["events"].as_array().unwrap().iter().any(|e| e["kind"] == "stop");
    let first_stop = batches.iter().position(stop).unwrap();
    for batch in &batches[..first_stop] {
        assert_eq!(stack.post_mod(batch).await.unwrap(), 204, "{batch}");
    }
    let writing = "Writing: Let me run it.";
    let drv = &mut stack.driver;
    drv.wait_for("the live step", STEP, |d| {
        has(d, "Article", writing) && has(d, "Status", "Preparing Bash")
    })
    .await
    .unwrap();
    assert!(labels(&drv.dump().await.unwrap(), "Article").iter().all(|l| l == writing));
    golden(drv, &dir, "conversation-live").await;

    // The step stops and the transcript gets the answer and the call: both settle, into the
    // turn's fold now that the agent has no turn going.
    assert_eq!(stack.post_mod(&batches[first_stop]).await.unwrap(), 204);
    let result = records.iter().position(|l| l.contains(r#""type":"tool_result""#)).unwrap();
    let mut transcript = records[..result].join("\n");
    transcript.push('\n');
    std::fs::write(&main, transcript).unwrap();
    let dump = stack
        .driver
        .wait_for("the step settled", STEP, |d| {
            // The fold carries the turn's figures from the transcript: what it wrote. Its model
            // is the one the composer names, so the fold leaves it out.
            labels(d, "Button").iter().any(|l| l == "Worked \u{b7} 1 step \u{b7} 20 tokens")
                && !has(d, "Article", writing)
                && !has(d, "Status", "Preparing Bash")
        })
        .await
        .unwrap();
    assert!(
        !labels(&dump, "Article").iter().any(|l| l.starts_with("Writing: ")),
        "{:#?}",
        dump.a11y
    );
    stack.shutdown().await;
}

/// A screenshot made up for the session: a window's bar over a header whose chips run into
/// the title, as a person would paste to show it clipping.
fn screenshot() -> Vec<u8> {
    let (w, h) = (640_u32, 400_u32);
    let picture = image::RgbImage::from_fn(w, h, |x, y| {
        let chip = |x0: u32| (x0..x0.saturating_add(90)).contains(&x) && (58..82).contains(&y);
        if y < 28 {
            image::Rgb([232, 232, 236])
        } else if chip(300) || chip(400) || chip(500) {
            image::Rgb([96, 120, 220])
        } else if (24..420).contains(&x) && (62..78).contains(&y) {
            image::Rgb([60, 60, 70])
        } else if (40..100).contains(&y) {
            image::Rgb([248, 248, 250])
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

/// A turn's work beyond its words: the pasted screenshot on the prompt, the plan in view when
/// the turn folds, the task list over the composer, and the build run in the background with
/// its last line, following the file it writes as it grows and ending when its notice comes.
/// In Verbose, the thinking opens and the screenshot the `Read` returned shows on it.
#[tokio::test]
async fn the_face_shows_the_work_beyond_words() {
    if !gated() {
        return;
    }
    let mut stack = Stack::launch("e2e-worker").await.unwrap();
    let dir = stack.dir.path().to_path_buf();
    stack.driver.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    let dump = first_shell(&mut stack.driver).await;
    let session = dump.terminals[0].session.clone();
    let (main, output) = work_session(&stack);
    start(&stack, &session, &main).await;
    let drv = &mut stack.driver;
    drv.keys("cmd-j").await.unwrap();
    let building = "Build the release binary: Running";
    drv.wait_for("the build in the background", STEP, |d| {
        has(d, "List", "In the background")
            && labels(d, "Button").iter().any(|l| l.starts_with(building))
            && has(d, "Article", "Plan: Let the header's chips give way, Approved")
    })
    .await
    .unwrap();
    // The build prints on: the tray follows its file.
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
        labels(d, "Button").iter().any(|l| l == "Build the release binary: Done \u{b7} 1m 5s")
    })
    .await
    .unwrap();
    golden(drv, &dir, "conversation-work").await;
    drv.keys("ctrl-o ctrl-o").await.unwrap();
    // Every step shows, the build's call among them, so its finished row leaves the tray.
    drv.wait_for("every step", STEP, |d| {
        any_label(d, "Updated the tasks")
            && any_label(d, "Picture, 640 \u{d7} 400")
            && !labels(d, "Button").iter().any(|l| l.starts_with("Build the release binary: "))
    })
    .await
    .unwrap();
    golden(drv, &dir, "conversation-work-verbose").await;
    // Above the tail: the thinking, open in Verbose, and how long it took.
    scroll_up_until(drv, "the thinking", |d| {
        labels(d, "Button").iter().any(|l| l == "Thought for 9 s")
    })
    .await;
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

/// The face over a long conversation while the model writes an answer, the frame-time case
/// behind `docs/MEASUREMENTS.md` ("the conversation face under a streaming answer"). Runs only
/// with `SLOPTY_SMOOTH_E2E=1`, alone, since other tests' frames would be counted.
///
/// (h) the list following the tail while an answer grows by a piece every 16 ms, as Slopty's
/// Claude Code mod reports it; (i) the same with the reader panning the history at 120
/// events per second.
#[tokio::test]
async fn the_face_draws_a_streaming_answer_within_a_frame() {
    if std::env::var_os("SLOPTY_SMOOTH_E2E").is_none() {
        eprintln!("skipped: set SLOPTY_SMOOTH_E2E=1 (and SLOPTY_APP_E2E=1)");
        return;
    }
    let run = std::time::Duration::from_secs(5);
    let mut stack = Stack::launch("e2e-worker").await.unwrap();
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
    stack.driver.keys("cmd-j").await.unwrap();
    stack
        .driver
        .wait_for("the conversation", STEP, |d| {
            has(d, "Group", "Conversation") && labels(d, "Article").len() > 4
        })
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
            labels(d, "Article").iter().any(|l| l.starts_with("Writing: "))
        })
        .await
        .unwrap();

    let region = stack
        .driver
        .dump()
        .await
        .unwrap()
        .a11y_node("Group", Some("Conversation"))
        .expect("the face")
        .bounds;
    let (x, y) = (region[0] + region[2] / 2.0, region[1] + region[3] / 2.0);
    for (scenario, pan) in [
        ("(h) face, following a streaming answer", false),
        ("(i) face, panning while it streams", true),
    ] {
        stack.driver.frames_reset().await.unwrap();
        let begin = tokio::time::Instant::now();
        let mut n = 0_usize;
        while begin.elapsed() < run {
            let word = words[n % words.len()];
            assert_eq!(stack.post_mod(&piece(word)).await.unwrap(), 204);
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
        let frames = stack.driver.dump().await.unwrap().frames;
        println!("MEASURE {scenario}: {}", frames.row());
        assert!(frames.frames >= 100, "too few frames to judge: {frames:?}");
    }
    stack.shutdown().await;
}
