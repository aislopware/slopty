//! An agent drives the person's window, on a real stack: a stand-in Claude Code
//! (`slopty-stub-claude`) calls Slopty's `ui_*` tools through the `slopty` MCP server the worker
//! hands it (`STUB_TURNS`), and `slopty ui`, the same verbs for an agent's shell, asks through the
//! worker's control socket. Each ask reaches the window in front of the worker's machine
//! (`docs/decisions/agents.md`, "Agents drive the person's window through open verbs"); what it
//! changed is judged on the app's accessibility tree, and what it said on its JSON.

use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};
use slopty_e2e::Dump;
use slopty_e2e::harness::{ServedApp, bin_dir};

use crate::served::{
    STEP, heard, holding, node, open_thread, says, seen, start, tell, thread_until,
    type_in_terminal,
};

/// The worker's name, as the CLI names the machine.
const MACHINE: &str = "steer";

/// What the stand-in is first asked, a plain turn.
const BEGIN: &str = "Read the greeting";
/// The turn whose tool hides the sidebar.
const HIDE: &str = "Hide the sidebar";
/// The turn whose tools show it again, open a terminal and a file, and say the layout.
const ARRANGE: &str = "Arrange the window";
/// The file the agent opens.
const README: &str = "# Steer\n\nA folder an agent works in.\n";

/// The answers of the tool calls in the stand-in's record, each the JSON its text holds, with
/// `error` set on a failure.
fn answers(record: &Path) -> Vec<Value> {
    heard(record)["mcp"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|answer| {
            let text = answer.pointer("/content/0/text").and_then(Value::as_str).unwrap_or("");
            let mut said = serde_json::from_str(text).unwrap_or_else(|_| json!({ "text": text }));
            if answer["isError"] == true && said.get("error").is_none() {
                said["error"] = json!(text);
            }
            said
        })
        .collect()
}

/// The region `name` of a window's state.
fn region<'a>(window: &'a Value, name: &str) -> &'a Value {
    window["regions"]
        .as_array()
        .and_then(|all| all.iter().find(|r| r["name"] == name))
        .unwrap_or(&Value::Null)
}

/// The terminals drawn: the session's, and each shell split beside it.
fn terminals(dump: &Dump) -> usize {
    dump.a11y.iter().filter(|n| n.role == "Terminal").count()
}

/// Whether any node, of any role, says `words`.
fn says_anywhere(dump: &Dump, words: &str) -> bool {
    dump.a11y.iter().any(|n| {
        n.label.as_deref().is_some_and(|l| l.contains(words))
            || n.value.as_deref().is_some_and(|v| v.contains(words))
    })
}

/// `slopty ui <args>` in `cwd`, as an agent's shell on the stack's worker runs it: through the
/// worker's control socket, outside any session. Whether it succeeded, and the JSON it printed.
async fn ui(stack: &ServedApp, cwd: &Path, args: &[&str]) -> (bool, Value) {
    let data = stack.path("cli");
    let mut command = tokio::process::Command::new(bin_dir().unwrap().join("slopty"));
    slopty_testkit::env::scrub(command.as_std_mut(), &data.join("home"));
    let run = command
        .arg("--data-dir")
        .arg(&data)
        .arg("ui")
        .args(args)
        .env("SLOPTY_WORKER_SOCKET", stack.worker.ctl_socket())
        .current_dir(cwd)
        .kill_on_drop(true)
        .output();
    let out = tokio::time::timeout(Duration::from_secs(90), run).await.unwrap().unwrap();
    let said = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "slopty ui {args:?} printed no JSON ({e}): {}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    });
    (out.status.success(), said)
}

/// An agent's tools drive the window: the stand-in's `ui_region` hides the sidebar (the toggle
/// then offers to show it), and in its next turn `ui_region` expands it again, `ui_terminal`
/// opens a shell in its folder in a pane beside the session, `ui_open` shows its README in a pane
/// of its own, each change shown in place with no notice over it, since the agent's chat is the
/// one on screen (a chat off screen is marked with one, `shell::agent_ui::tests`), and `ui_state`
/// reports that layout: the sidebar and the shells shown, the README's pane and the shell's, with
/// its session, and nobody typing.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn an_agents_tools_lay_out_the_window() {
    let turns = json!({
        BEGIN: [],
        HIDE: [{ "name": "ui_region", "arguments": { "region": "sidebar", "change": "hide" } }],
        ARRANGE: [
            { "name": "ui_region", "arguments": { "region": "sidebar", "change": "expand" } },
            { "name": "ui_terminal", "arguments": {} },
            { "name": "ui_open", "arguments": { "file": "README.md", "line": 3 } },
            { "name": "ui_state", "arguments": {} },
        ],
    })
    .to_string();
    let mut stack = ServedApp::launch_rooted(MACHINE, &[], |root| {
        vec![
            ("STUB_TURNS".to_owned(), turns),
            ("STUB_RECORD".to_owned(), root.join("heard.json").to_string_lossy().into_owned()),
        ]
    })
    .await
    .unwrap();
    let (repo, record) = (stack.path("repo"), stack.path("heard.json"));
    std::fs::write(repo.join("README.md"), README).unwrap();
    let agent = start(&stack, MACHINE, &repo, BEGIN).await;
    thread_until(&stack, &agent, "took its first turn", |r| says(r, "Done: Read the greeting"))
        .await;
    seen(&mut stack.driver, "the sidebar", STEP, |d| node(d, "Button", "Hide sidebar").is_some())
        .await;

    // The person in front of the agent's chat asks; the agent hides the sidebar.
    tell(&mut stack, HIDE).await;
    thread_until(&stack, &agent, "hid the sidebar", |r| says(r, "Done: Hide the sidebar")).await;
    let hidden = seen(&mut stack.driver, "the sidebar hidden", STEP, |d| {
        node(d, "Button", "Show sidebar").is_some()
    })
    .await;
    let hid = answers(&record);
    assert_eq!(hid.len(), 1, "one call: {hid:?}");
    assert_eq!(hid[0]["did"], "hid the sidebar", "{hid:?}");
    assert_eq!(region(&hid[0]["window"], "sidebar")["shown"], false, "{hid:?}");
    assert!(node(&hidden, "Button", "Hide sidebar").is_none());

    // Then it lays the window out.
    tell(&mut stack, ARRANGE).await;
    thread_until(&stack, &agent, "arranged the window", |r| says(r, "Done: Arrange the window"))
        .await;
    let laid = seen(
        &mut stack.driver,
        "the sidebar, a shell and the README beside the session",
        STEP,
        |d| {
            node(d, "Button", "Hide sidebar").is_some()
                && terminals(d) >= 2
                && node(d, "Group", "README.md").is_some()
        },
    )
    .await;
    // The chat on screen shows what its agent did; no notice says it again over the pane.
    let readme_mark = format!(": opened {}", repo.join("README.md").display());
    assert!(!says_anywhere(&laid, &readme_mark), "no notice over the README");

    let said = answers(&record);
    assert_eq!(said.len(), 5, "five calls: {said:?}");
    for answer in &said[1..] {
        assert!(answer.get("error").is_none(), "each call did its part: {answer}");
    }
    assert_eq!(said[1]["did"], "showed the sidebar", "{said:?}");
    let opened = &said[2];
    assert_eq!(opened["made"]["region"], "terminal", "{opened}");
    assert!(opened["made"]["terminal"].as_str().is_some_and(|t| !t.is_empty()), "{opened}");
    assert!(opened["did"].as_str().is_some_and(|d| d.starts_with("opened a terminal")), "{opened}");
    assert!(said[3]["did"].as_str().is_some_and(|d| d.contains("README.md")), "{said:?}");
    let window = &said[4]["window"];
    assert_eq!(window["page"], "chat", "{window}");
    assert_eq!(window["typing"], false, "{window}");
    assert_eq!(window["chat"]["machine"], MACHINE, "{window}");
    for name in ["sidebar", "terminal"] {
        assert_eq!(region(window, name)["shown"], true, "{name} shows: {window}");
    }
    let tabs = window["tabs"].as_array().cloned().unwrap_or_default();
    let readme = tabs.iter().find(|t| t["kind"] == "file").unwrap_or(&Value::Null);
    assert_eq!(readme["region"], "pane", "{window}");
    assert_eq!(readme["selected"], true, "{window}");
    assert!(readme["target"].as_str().is_some_and(|t| t.contains("README.md")), "{window}");
    let terminal = tabs.iter().find(|t| t["id"] == opened["made"]["id"]).unwrap_or(&Value::Null);
    assert_eq!(terminal["terminal"], opened["made"]["terminal"], "its tab, by its id: {window}");

    stack.shutdown().await;
}

/// The person stays in charge, and every refusal says why: from an agent's shell, `slopty ui
/// chat` puts the other chat on screen while nobody types (its terminal holding the keyboard is
/// someone watching); once the person types a draft in the session's terminal, the same ask
/// is busy, the chat and the keyboard stay, and an ask that moves nothing the person is in
/// (`show terminal`, a shell split beside the session) goes through with the keyboard still the
/// session's. A region, a file or a chat the window does not know is unknown, its message naming
/// what it does know; with the app gone, no window is reached.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn the_window_stays_the_persons_and_says_why_it_refuses() {
    let mut stack = ServedApp::launch_with(MACHINE, &[], &[]).await.unwrap();
    let repo = stack.path("repo");
    let first = start(&stack, MACHINE, &repo, "Tidy the greeting").await;
    thread_until(&stack, &first, "took its turn", |r| says(r, "Done: Tidy the greeting")).await;
    let second = start(&stack, MACHINE, &repo, "Fix the build").await;
    thread_until(&stack, &second, "took its turn", |r| says(r, "Done: Fix the build")).await;
    open_thread(&mut stack.driver, 1).await;

    // Nobody types: the agent puts the other chat on screen, and says so.
    let (ok, state) = ui(&stack, &repo, &["state"]).await;
    assert!(ok, "{state}");
    let shown = state["window"]["chat"]["title"].as_str().unwrap_or_default().to_owned();
    let other = if shown.contains("greeting") { "Fix the build" } else { "Tidy the greeting" };
    let (ok, moved) = ui(&stack, &repo, &["chat", other]).await;
    assert!(ok, "{moved}");
    assert_eq!(moved["window"]["chat"]["title"], other, "{moved}");
    seen(&mut stack.driver, "the mark", STEP, |d| says_anywhere(d, &format!("put {other}"))).await;

    // The person types a draft in the session's terminal: the chat and the keyboard are theirs.
    type_in_terminal(&mut stack.driver, "half a thought").await;
    let typing =
        seen(&mut stack.driver, "the draft", STEP, |d| d.focused.starts_with("terminal:")).await;
    let session = typing.focused.clone();
    let (ok, busy) = ui(&stack, &repo, &["chat", &shown]).await;
    assert!(!ok, "{busy}");
    assert_eq!(busy["error"]["code"], "busy", "{busy}");
    let (ok, typing) = ui(&stack, &repo, &["state"]).await;
    assert!(ok && typing["window"]["typing"] == true, "{typing}");
    assert_eq!(typing["window"]["chat"]["title"], other, "the chat stayed: {typing}");
    let (ok, split) = ui(&stack, &repo, &["show", "terminal"]).await;
    assert!(ok, "{split}");
    seen(&mut stack.driver, "a shell beside the session", STEP, |d| terminals(d) >= 2).await;
    let kept = stack.driver.dump().await.unwrap();
    assert_eq!(kept.focused, session, "the session's terminal kept the keyboard");

    // What the window does not know.
    let (ok, region) = ui(&stack, &repo, &["show", "toolbar"]).await;
    assert!(!ok && region["error"]["code"] == "unknown", "{region}");
    let listed = region["error"]["message"].as_str().unwrap_or_default();
    assert!(listed.contains("sidebar") && listed.contains("terminal"), "{region}");
    let (ok, file) = ui(&stack, &repo, &["open", "gone.rs"]).await;
    assert!(!ok && file["error"]["code"] == "unknown", "{file}");
    let (ok, chat) = ui(&stack, &repo, &["chat", "No such chat"]).await;
    assert!(!ok && chat["error"]["code"] == "unknown", "{chat}");
    let frame = stack.driver.dump().await.unwrap();
    assert_eq!(frame.focused, session, "the keyboard is still the session's");
    assert!(holding(&frame, "Button", "README").is_none(), "nothing opened");

    // The app gone, no window answers.
    let _quit = stack.driver.call(&slopty_e2e::Command::Quit).await;
    let _gone = tokio::time::timeout(STEP, stack.app.wait()).await;
    let (ok, none) = ui(&stack, &repo, &["state"]).await;
    assert!(!ok && none["error"]["code"] == "no_window", "{none}");

    stack.shutdown().await;
}
