//! The dock around the composer and the requests it asks, on a real stack: a stand-in Claude
//! Code (`slopty-stub-claude`) opens each of Claude Code's doors beside its messages (background
//! work, a monitor, a goal, dialogs and forms, file checkpoints, a `!` command, a question only
//! the person at its card answers), and the app shows and answers them through the agent's own
//! doors. Each case is judged by the accessibility tree, what the stand-in heard and the files
//! its folder holds.

use std::path::PathBuf;
use std::time::Duration;

use slopty_e2e::harness::ServedApp;
use slopty_e2e::{A11yNode, Driver, Dump};

use crate::served::{
    SETTLE, STEP, TURN, all_holding, asking, click, control_requests, goldens, heard, holding,
    hover, middle, node, open_thread, says, seen, start, tell, thread_until, type_in_composer,
};

/// The worker's name, as the CLI names the machine.
const MACHINE: &str = "dock";

/// A stack whose stand-in writes what it heard to `root/heard.json`; its folder `root/repo`.
async fn stack() -> (ServedApp, PathBuf, PathBuf) {
    let stack = ServedApp::launch_rooted(MACHINE, &[], |root| {
        vec![("STUB_RECORD".to_owned(), root.join("heard.json").to_string_lossy().into_owned())]
    })
    .await
    .unwrap();
    let (repo, record) = (stack.path("repo"), stack.path("heard.json"));
    (stack, repo, record)
}

/// The labels of `role` in `dump`, for a failure's message.
fn labels(dump: &Dump, role: &str) -> Vec<String> {
    dump.a11y.iter().filter(|n| n.role == role).filter_map(|n| n.label.clone()).collect()
}

/// The first node `role` named `label` after `after` in reading order.
fn after<'a>(dump: &'a Dump, after: &A11yNode, role: &str, label: &str) -> Option<&'a A11yNode> {
    let at = dump.a11y.iter().position(|n| n == after)?;
    dump.a11y.iter().skip(at).find(|n| n.role == role && n.label.as_deref() == Some(label))
}

/// The tray's rows of `noun` ("command", "monitor", "subagent"…).
fn rows<'a>(dump: &'a Dump, noun: &str) -> Vec<&'a A11yNode> {
    let lead = format!("{noun} · ");
    dump.a11y
        .iter()
        .filter(|n| {
            n.role == "ListItem" && n.label.as_deref().is_some_and(|l| l.starts_with(&lead))
        })
        .collect()
}

/// Open the Background tray from its header, unless it is open.
async fn open_tray(drv: &mut Driver) -> Dump {
    let dump = seen(drv, "the Background tray", STEP, |d| {
        node(d, "Button", "Expand background work").is_some()
            || node(d, "Button", "Collapse background work").is_some()
    })
    .await;
    if node(&dump, "Button", "Expand background work").is_some() {
        click(drv, "Button", "Expand background work").await;
    }
    seen(drv, "the tray open", STEP, |d| node(d, "Button", "Collapse background work").is_some())
        .await
}

/// The Background tray lists a command left running and a monitor that ended, and stops the
/// command through the agent's door: a stand-in runs `sleep 600` in the background, then a
/// monitor whose stream ends. Its header counts the one live command; a row says its kind, its
/// title and its state; the ended monitor folds under "Finished". The command's ✕ asks Claude
/// Code to stop that task (`stop_task` with its id), and the row ends, the header's count with it.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn the_background_tray_lists_work_and_stops_a_command() {
    let (mut stack, repo, record) = stack().await;
    let agent = start(&stack, MACHINE, &repo, "background sleep 600").await;
    thread_until(&stack, &agent, "left a command running", |r| {
        says(r, "it runs in the background")
    })
    .await;
    tell(&mut stack, "monitor").await;
    thread_until(&stack, &agent, "heard the monitor end", |r| says(r, "the build log ended")).await;

    let renders = stack.path("");
    let drv = &mut stack.driver;
    open_thread(drv, 1).await;
    let tray = open_tray(drv).await;
    let commands = rows(&tray, "command");
    assert_eq!(commands.len(), 1, "one command row: {:?}", labels(&tray, "ListItem"));
    let header = node(&tray, "Button", "Collapse background work").cloned().unwrap();
    let words = header.value.clone().unwrap_or_default();
    assert!(
        words.contains("1 command") && !words.contains("1 commands"),
        "the header counts the one live command: {words:?}; the rows: {:?}",
        labels(&tray, "ListItem")
    );
    let command = commands[0].clone();
    let said = command.label.clone().unwrap_or_default();
    assert!(said.contains("sleep 600") || said.contains("Run sleep"), "its title: {said:?}");

    // The monitor ended, so it waits under "Finished" until that opens.
    if rows(&tray, "monitor").is_empty() {
        let finished = holding(&tray, "Button", "Finished · ").cloned();
        let finished = finished.unwrap_or_else(|| {
            panic!("the ended monitor folds under Finished: {:?}", labels(&tray, "Button"))
        });
        let (x, y) = middle(&finished);
        drv.click(x, y).await.unwrap();
    }
    let tray = seen(drv, "the monitor's row", STEP, |d| !rows(d, "monitor").is_empty()).await;
    let monitor = rows(&tray, "monitor")[0].label.clone().unwrap_or_default();
    assert!(monitor.contains("Watch the build log"), "the monitor's title: {monitor:?}");
    tokio::time::sleep(SETTLE).await;
    // The rows' times run with the clock: they stay out of the golden.
    goldens(drv, &renders, "dock-tray", &["main · ", "command · ", "monitor · "]).await;

    // ✕ on the command's row: Claude Code is asked to stop that task, and the row ends.
    let tray = drv.dump().await.unwrap();
    let command = rows(&tray, "command")[0].clone();
    hover(drv, &command).await;
    let stop = after(&tray, &command, "Button", "Stop").cloned();
    let stop = stop.unwrap_or_else(|| panic!("the row's Stop: {:?}", labels(&tray, "Button")));
    let (x, y) = middle(&stop);
    drv.click(x, y).await.unwrap();
    let stopped = seen(drv, "the command ended", STEP, |d| {
        let header = node(d, "Button", "Collapse background work")
            .or_else(|| node(d, "Button", "Expand background work"));
        header.and_then(|h| h.value.as_deref()).is_none_or(|v| !v.contains("1 command"))
    })
    .await;
    let asked = control_requests(&heard(&record), "stop_task");
    assert_eq!(asked.len(), 1, "one stop_task: {asked:?}");
    let task = asked[0]["request"]["task_id"].as_str().unwrap_or_default().to_owned();
    assert!(task.starts_with('b'), "the background command's task: {task:?}");
    assert!(
        all_holding(&stopped, "Button", "Stop").iter().all(|n| n.label.as_deref() != Some("Stop")),
        "nothing live is left to stop: {:?}",
        labels(&stopped, "Button")
    );
    stack.shutdown().await;
}

/// The footer under the composer says the mode and the goal: a stand-in with a command running
/// sets a goal (`signals`). The footer reads the mode the agent is in and "/goal active", and
/// leaves the live command to the Background tray; ⇧Tab moves to the next mode through Claude
/// Code's own door (`set_permission_mode`), and the footer follows.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn the_footer_says_the_mode_and_the_goal() {
    let (mut stack, repo, record) = stack().await;
    let agent = start(&stack, MACHINE, &repo, "background sleep 600").await;
    thread_until(&stack, &agent, "left a command running", |r| {
        says(r, "it runs in the background")
    })
    .await;
    tell(&mut stack, "signals").await;
    thread_until(&stack, &agent, "signalled", |r| says(r, "Done: signalled.")).await;

    let renders = stack.path("");
    let drv = &mut stack.driver;
    open_thread(drv, 1).await;
    let footer = seen(drv, "the footer", STEP, |d| {
        holding(d, "Button", "Mode: ").is_some() && holding(d, "Button", "/goal active").is_some()
    })
    .await;
    assert!(
        holding(&footer, "Button", "1 command").is_none(),
        "the tray counts the command, not the footer"
    );
    let mode = holding(&footer, "Button", "Mode: ").and_then(|n| n.label.clone()).unwrap();
    println!("MEASURE footer: {mode}");
    tokio::time::sleep(SETTLE).await;
    // The goal's age runs with the clock; its words stay out of the golden's text.
    goldens(drv, &renders, "dock-footer", &["/goal active"]).await;

    // ⇧Tab from the composer: the next mode, asked of Claude Code, and said by the footer.
    type_in_composer(drv, "").await;
    drv.keys("shift-tab").await.unwrap();
    let moved = seen(drv, "the next mode", STEP, |d| {
        holding(d, "Button", "Mode: ").and_then(|n| n.label.as_deref()) != Some(mode.as_str())
    })
    .await;
    let now = holding(&moved, "Button", "Mode: ").and_then(|n| n.label.clone()).unwrap();
    let asked = control_requests(&heard(&record), "set_permission_mode");
    assert!(!asked.is_empty(), "Claude Code was asked to change its mode");
    let to = asked.last().unwrap()["request"]["mode"].as_str().unwrap_or_default().to_owned();
    println!("MEASURE footer: ⇧Tab asked for {to}, the footer reads {now}");
    stack.shutdown().await;
}

/// A dialog of Claude Code's own and an MCP server's form are each answered at their card: the
/// dialog's options are its payload's, a click on one answers it with that option, and the
/// form, drawn from its schema, sends what was typed into its field.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn a_dialog_and_a_form_are_answered_at_their_cards() {
    let (mut stack, repo, _record) = stack().await;
    let agent = start(&stack, MACHINE, &repo, "dialog remote_callout").await;
    thread_until(&stack, &agent, "asks the dialog", asking).await;

    let renders = stack.path("");
    let drv = &mut stack.driver;
    open_thread(drv, 1).await;
    seen(drv, "the dialog's options", STEP, |d| {
        node(d, "Button", "Yes").is_some() && node(d, "Button", "No").is_some()
    })
    .await;
    tokio::time::sleep(SETTLE).await;
    goldens(drv, &renders, "dock-dialog", &[]).await;
    click(drv, "Button", "Yes").await;
    let read =
        thread_until(&stack, &agent, "answered the dialog", |r| says(r, "Dialog remote_callout:"))
            .await;
    assert!(says(&read, "yes"), "the dialog took the option clicked: {read}");

    tell(&mut stack, "elicit").await;
    thread_until(&stack, &agent, "asks the form", asking).await;
    let drv = &mut stack.driver;
    let form = seen(drv, "the form", STEP, |d| {
        holding(d, "Form", "").is_some() && holding(d, "Group", "Ticket").is_some()
    })
    .await;
    tokio::time::sleep(SETTLE).await;
    goldens(drv, &renders, "dock-form", &[]).await;
    // The field under its group: the first text input after it.
    let group = holding(&form, "Group", "Ticket").cloned().unwrap();
    let at = form.a11y.iter().position(|n| *n == group).unwrap_or_default();
    let field = form
        .a11y
        .iter()
        .skip(at)
        .find(|n| n.role.contains("TextInput") && n.role != "MultilineTextInput")
        .or_else(|| form.a11y.iter().skip(at).find(|n| n.role.contains("TextInput")))
        .cloned()
        .unwrap_or_else(|| panic!("the field: {}", slopty_e2e::frame_text(&form.a11y)));
    let (x, y) = middle(&field);
    drv.click(x, y).await.unwrap();
    drv.type_text("SLO-42").await.unwrap();
    // What the person types goes into the field and answers nothing: its digits are no option's
    // keys while the field has the keyboard.
    let typed = drv.dump().await.unwrap();
    let read = stack.slopty(&["agent", "read", "--thread", &agent]).await.unwrap();
    assert!(asking(&read), "typing in the field answered the form: {read}");
    assert!(
        typed.a11y.iter().any(|n| n.value.as_deref() == Some("SLO-42")),
        "the field holds what was typed:\n{}",
        slopty_e2e::frame_text(&typed.a11y)
    );
    let drv = &mut stack.driver;
    let send = ["Submit form", "Submit", "Send", "Done", "OK"]
        .into_iter()
        .find(|label| node(&form, "Button", label).is_some())
        .unwrap_or_else(|| panic!("the form's send: {:?}", labels(&form, "Button")));
    click(drv, "Button", send).await;
    let read = thread_until(&stack, &agent, "answered the form", |r| says(r, "Elicitation:")).await;
    assert!(says(&read, "accept") && says(&read, "SLO-42"), "the form sent its field: {read}");
    stack.shutdown().await;
}

/// The toast's "Undo rewind": by its name, else the nameless button of the toast that says the
/// rewind ("Rewound …"), whose action the toaster draws without one.
fn undo_rewind(dump: &Dump) -> Option<&A11yNode> {
    node(dump, "Button", "Undo rewind").or_else(|| {
        let toast = holding(dump, "Status", "Rewound ")?;
        let [tx, ty, tw, th] = toast.bounds;
        dump.a11y.iter().find(|n| {
            let (x, y) = middle(n);
            n.role == "Button"
                && n.label.as_deref().is_none_or(str::is_empty)
                && x >= tx
                && x <= tx + tw
                && y >= ty
                && y <= ty + th
        })
    })
}

/// A user message rewinds and the rewind undoes: a stand-in writes `notes.txt` in its turn.
/// The message's "Rewind…" offers what Claude Code can take back; "Code and conversation" puts
/// the folder back as it was before the message (the file is gone) and returns the message's
/// words to the composer, and the toast's "Undo rewind" puts the file back.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn a_rewind_takes_the_turn_back_and_undo_returns_it() {
    let (mut stack, repo, _record) = stack().await;
    let prompt = "write notes.txt first words";
    let agent = start(&stack, MACHINE, &repo, prompt).await;
    thread_until(&stack, &agent, "wrote the file", |r| says(r, "Done: wrote notes.txt.")).await;
    let notes = repo.join("notes.txt");
    assert_eq!(std::fs::read_to_string(&notes).unwrap_or_default(), "first words\n");

    let renders = stack.path("");
    let drv = &mut stack.driver;
    let open = open_thread(drv, 1).await;
    // The person's message carries "Rewind…" on its hover strip. The message is no node of the
    // tree, so the pointer goes down the transcript's right side, where a person's message
    // sits, until the strip shows.
    let column = open
        .a11y
        .iter()
        .find(|n| {
            n.role == "Status" && n.label.as_deref().is_some_and(|l| l.contains("Worked for"))
        })
        .map_or([420.0, 0.0, 736.0, 0.0], |n| n.bounds);
    let x = column[0] + column[2] - 40.0;
    let mut strip = None;
    for step in 0..40_u8 {
        let y = f32::from(step).mul_add(14.0, 50.0);
        drv.ok(&slopty_e2e::Command::Move { x, y }).await.unwrap();
        let shown = drv.dump().await.unwrap();
        if let Some(rewind) = node(&shown, "Button", "Rewind…") {
            strip = Some(rewind.clone());
            println!("MEASURE rewind: the strip shows with the pointer at {x:.0},{y:.0}");
            break;
        }
    }
    let rewind = strip.unwrap_or_else(|| {
        panic!("no Rewind… over the person's message: {}", slopty_e2e::frame_text(&open.a11y))
    });
    let (x, y) = middle(&rewind);
    drv.click(x, y).await.unwrap();
    let menu = seen(drv, "the rewind menu", STEP, |d| {
        node(d, "Menu", "Rewind to before this message").is_some()
    })
    .await;
    let offered: Vec<String> =
        menu.a11y.iter().filter(|n| n.role == "MenuItem").filter_map(|n| n.label.clone()).collect();
    assert!(
        offered.iter().any(|l| l == "Code and conversation"),
        "the menu offers both: {offered:?}"
    );
    goldens(drv, &renders, "dock-rewind-menu", &[]).await;
    click(drv, "MenuItem", "Code and conversation").await;

    let since = std::time::Instant::now();
    while notes.exists() {
        assert!(since.elapsed() < STEP, "the rewind took notes.txt back");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let drv = &mut stack.driver;
    let rewound = seen(drv, "the words back in the composer and the undo", STEP, |d| {
        let draft = d.a11y.iter().any(|n| {
            n.role == "MultilineTextInput" && n.value.as_deref().is_some_and(|v| v.contains(prompt))
        });
        draft && undo_rewind(d).is_some()
    })
    .await;
    let undo = undo_rewind(&rewound).cloned().unwrap();
    if undo.label.as_deref() != Some("Undo rewind") {
        println!("MEASURE rewind: the toast's Undo rewind has no accessible name");
    }
    let (x, y) = middle(&undo);
    drv.click(x, y).await.unwrap();
    let since = std::time::Instant::now();
    while std::fs::read_to_string(&notes).unwrap_or_default() != "first words\n" {
        assert!(since.elapsed() < STEP, "the undo put notes.txt back");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    stack.shutdown().await;
}

/// A draft that starts with `!` is a command: the composer says "Shell mode", Return runs it on
/// the chat's machine through Claude Code's door, and its output goes to the agent as words it
/// is not asked to answer (`shouldQuery: false`); the person's row keeps the `!` mark.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn a_bang_draft_runs_a_command_for_the_agent() {
    let (mut stack, repo, record) = stack().await;
    let agent = start(&stack, MACHINE, &repo, "Hello there").await;
    thread_until(&stack, &agent, "answered", |r| says(r, "Done: Hello there")).await;

    let renders = stack.path("");
    let drv = &mut stack.driver;
    open_thread(drv, 1).await;
    type_in_composer(drv, "!echo bang-$((6*7))").await;
    seen(drv, "shell mode", STEP, |d| node(d, "Status", "Shell mode").is_some()).await;
    goldens(drv, &renders, "dock-shell-mode", &[]).await;
    drv.keys("enter").await.unwrap();

    // The command ran on the worker: its output, 42 worked out by the shell, reached Claude Code.
    let since = std::time::Instant::now();
    loop {
        let record = heard(&record);
        let told = record["received"].as_array().is_some_and(|lines| {
            lines.iter().any(|line| line["type"] == "user" && line.to_string().contains("bang-42"))
        });
        if told {
            break;
        }
        assert!(since.elapsed() < TURN, "Claude Code heard the command's output: {record}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let drv = &mut stack.driver;
    let row = seen(drv, "the person's row with its mark", STEP, |d| {
        d.a11y.iter().any(|n| n.label.as_deref().is_some_and(|l| l.contains("! echo bang-")))
    })
    .await;
    assert!(node(&row, "Status", "Shell mode").is_none(), "the composer is words again");
    stack.shutdown().await;
}

/// A question only the person at its card answers (Claude Code's `requires_user_interaction`)
/// says so on its card, and a note's press, answered through the server with no card, is
/// refused there: the request still waits, and the card's own click answers it.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn a_request_that_needs_the_person_is_refused_from_a_note() {
    let (mut stack, repo, record) = stack().await;
    let agent = start(&stack, MACHINE, &repo, "ask").await;
    let read = thread_until(&stack, &agent, "asks", asking).await;
    let ask = read["requests"][0]["ask"].as_str().unwrap_or_default().to_owned();
    let thread = read["thread"].as_str().unwrap_or(&agent).to_owned();

    let renders = stack.path("");
    let drv = &mut stack.driver;
    open_thread(drv, 1).await;
    seen(drv, "the card that needs the person", STEP, |d| {
        node(d, "Note", "Needs you at the card").is_some() && node(d, "Button", "Red").is_some()
    })
    .await;

    let mut refusals = Vec::new();
    for action in ["allow", "deny", "pick.0"] {
        let info = [("thread".to_owned(), thread.clone()), ("ask".to_owned(), ask.clone())];
        let pressed = drv.press_note(action, info.into_iter().collect()).await;
        let refused = pressed.as_ref().err().map(|e| format!("{e:#}")).unwrap_or_default();
        assert!(pressed.is_err(), "a note's {action} answered nothing: {pressed:?}");
        println!("MEASURE needs-presence: a note's {action} was refused: {refused}");
        refusals.push(refused);
    }
    assert!(
        refusals.iter().all(|r| r.contains("Needs you at the card")),
        "the worker said why it refused each press: {refusals:?}"
    );
    let read = stack.slopty(&["agent", "read", "--thread", &agent]).await.unwrap();
    assert!(asking(&read), "the request still waits on the person: {read}");
    let drv = &mut stack.driver;
    tokio::time::sleep(SETTLE).await;
    goldens(drv, &renders, "dock-needs-presence", &[]).await;

    let drv = &mut stack.driver;
    click(drv, "Button", "Red").await;
    let since = std::time::Instant::now();
    loop {
        let read = stack.slopty(&["agent", "read", "--thread", &agent]).await.unwrap();
        if !asking(&read) {
            break;
        }
        // A pick that waits for its send goes with Return.
        if since.elapsed() > Duration::from_secs(3) {
            stack.driver.keys("enter").await.unwrap();
        }
        assert!(since.elapsed() < STEP, "the card's click answered it: {read}");
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    thread_until(&stack, &agent, "took the card's answer", |r| says(r, "Done: noted.")).await;
    // The answer reaches Claude Code as the permission's allow, the question's answers in its
    // input; the thread's read keeps the turn's words, not a tool's result.
    let heard = heard(&record);
    let answered = heard["received"].as_array().into_iter().flatten().any(|line| {
        let line = match line {
            serde_json::Value::String(text) => serde_json::from_str(text).unwrap_or_default(),
            other => other.clone(),
        };
        line["type"] == "control_response"
            && line.pointer("/response/response/updatedInput/answers").is_some_and(|answers| {
                answers.as_object().is_some_and(|a| a.values().any(|v| v == "Red"))
            })
    });
    assert!(answered, "Claude Code heard the card's answer, Red: {heard}");
    stack.shutdown().await;
}
