use slopty_core::{DisplayId, WallMs, WindowId};
use slopty_proto::screen::{CaptureTarget, DisplayInfo, WindowInfo};
use slopty_proto::thread::detail::{ExecDetail, ExecStatus, McpDetail};
use slopty_proto::thread::{AgentScreen, Clipped, ToolCall, ToolDetail, ToolState, kind};

use super::{Clue, Device, KEPT, STALE, World, booted, clue, driven, kept, resolve, worth_telling};

fn call(name: &str, detail: Option<ToolDetail>, input: &str, output: Option<&str>) -> ToolCall {
    ToolCall {
        name: name.to_owned(),
        kind: kind::MCP.to_owned(),
        title: name.to_owned(),
        input: Clipped::whole(input),
        state: ToolState::Completed,
        output: output.map(Clipped::whole),
        images: Vec::new(),
        detail,
        child: None,
        ended_ms: None,
    }
}

/// A call of MCP server `server`'s `tool`, as Codex and Claude Code both give it.
fn mcp(server: &str, tool: &str, input: &str, output: Option<&str>) -> ToolCall {
    let detail = ToolDetail::Mcp(McpDetail { server: server.to_owned(), tool: tool.to_owned() });
    call(&format!("mcp__{server}__{tool}"), Some(detail), input, output)
}

fn exec(command: &str) -> ToolCall {
    let detail = ToolDetail::Exec(ExecDetail {
        command: Clipped::whole(command),
        description: None,
        cwd: None,
        background: false,
        task: None,
        status: ExecStatus::Done,
        exit_code: Some(0),
        stderr: None,
        duration_ms: None,
    });
    ToolCall { kind: kind::EXEC.to_owned(), ..call("Bash", Some(detail), "{}", None) }
}

fn window(id: u32, app: &str, bundle: &str, title: &str, size: (f32, f32)) -> WindowInfo {
    WindowInfo {
        id: WindowId(id),
        app: app.to_owned(),
        bundle_id: Some(bundle.to_owned()),
        title: title.to_owned(),
        x: 0.0,
        y: 0.0,
        w: size.0,
        h: size.1,
        display: DisplayId(1),
        on_screen: true,
    }
}

fn display(id: u32) -> DisplayInfo {
    DisplayInfo { id: DisplayId(id), w: 1512.0, h: 982.0, scale: 2.0, hz: 60.0 }
}

/// A Mac with Notes, the person's Chrome, Playwright's Chrome for Testing, two simulators and
/// one display.
fn world() -> World {
    World {
        windows: vec![
            window(10, "Notes", "com.apple.Notes", "Groceries", (800.0, 600.0)),
            window(11, "Notes", "com.apple.Notes", "", (200.0, 100.0)),
            window(20, "Google Chrome", "com.google.Chrome", "Inbox", (1200.0, 800.0)),
            window(
                30,
                "Google Chrome for Testing",
                "com.google.chrome.for.testing",
                "Example Domain",
                (1280.0, 720.0),
            ),
            window(40, "Simulator", "com.apple.iphonesimulator", "iPhone 17", (400.0, 860.0)),
            window(41, "Simulator", "com.apple.iphonesimulator", "iPhone 17 Pro", (400.0, 880.0)),
        ],
        owners: [(WindowId(10), 501), (WindowId(11), 501)].into_iter().collect(),
        displays: vec![display(1)],
        booted: vec![
            Device { udid: "AAAA-1".to_owned(), name: "iPhone 17".to_owned() },
            Device { udid: "BBBB-2".to_owned(), name: "iPhone 17 Pro".to_owned() },
        ],
    }
}

fn found(call: &ToolCall, world: &World) -> Option<(CaptureTarget, String, String)> {
    let clue = clue(call)?;
    resolve(&clue, world).map(|s| (s.target, s.kind, s.label))
}

fn win(id: u32) -> CaptureTarget {
    CaptureTarget::Window(WindowId(id))
}

/// Computer use names its window outright, by its id in the input or in what it captured; by
/// an application's process or name, it is that application's largest window; acting on the
/// whole screen, it is the display. Asking for access drives nothing.
#[test]
fn computer_use_names_a_window_an_application_or_the_display() {
    let world = world();
    let named = mcp("computer-use", "app_click", r#"{"window_id":10,"x":4,"y":5}"#, None);
    assert_eq!(
        found(&named, &world),
        Some((win(10), AgentScreen::APP.to_owned(), "Notes \u{2014} Groceries".to_owned()))
    );
    let captured =
        mcp("computer-use", "app_screenshot", r#"{"app":"Notes"}"#, Some("Captured window_id 11"));
    assert_eq!(clue(&captured), Some(Clue::Window(WindowId(11))));
    let by_name = mcp("computer-use", "app_type", r#"{"app":"Notes","text":"milk"}"#, None);
    assert_eq!(found(&by_name, &world).map(|f| f.0), Some(win(10)), "the larger Notes window");
    let by_process = mcp("cua-computer-use", "click", r#"{"pid":501,"x":1,"y":1}"#, None);
    assert_eq!(found(&by_process, &world).map(|f| f.0), Some(win(10)));
    let whole = mcp("computer-use", "left_click", r#"{"coordinate":[10,20]}"#, None);
    assert_eq!(
        found(&whole, &world),
        Some((
            CaptureTarget::Display(DisplayId(1)),
            AgentScreen::DESKTOP.to_owned(),
            "Desktop".to_owned()
        ))
    );
    let two = World { displays: vec![display(1), display(5)], ..world.clone() };
    let second = mcp("computer-use", "switch_display", r#"{"display":5}"#, None);
    assert_eq!(found(&second, &two).map(|f| f.2), Some("Display 2".to_owned()));
    let asking = mcp("computer-use", "request_access", r#"{"apps":["Notes"]}"#, None);
    assert_eq!(clue(&asking), None);
    let gone = mcp("computer-use", "app_click", r#"{"window_id":99}"#, None);
    assert_eq!(found(&gone, &world), None, "a window no longer listed is no screen");
}

/// A name with no detail is read as Claude Code writes it.
#[test]
fn a_bare_tool_name_is_read_as_its_server_and_tool() {
    let bare = call("mcp__computer-use__screenshot", None, "{}", None);
    assert_eq!(clue(&bare), Some(Clue::Display(None)));
    assert_eq!(clue(&call("Read", None, "{}", None)), None);
}

/// A browser tool's page is the browser window titled with it; with no title, the one window of
/// a browser only automation runs, never the person's own.
#[test]
fn a_browser_is_the_window_showing_the_page_the_tool_reported() {
    let world = world();
    let navigated = mcp(
        "playwright",
        "browser_navigate",
        r#"{"url":"https://example.com"}"#,
        Some("### Page state\n- Page URL: https://example.com/\n- Page Title: Example Domain\n"),
    );
    assert_eq!(
        found(&navigated, &world),
        Some((
            win(30),
            AgentScreen::BROWSER.to_owned(),
            "Google Chrome for Testing \u{2014} Example Domain".to_owned()
        ))
    );
    let tab = mcp(
        "claude-in-chrome",
        "tabs_context_mcp",
        "{}",
        Some(r#"{"tabs":[{"tabId":7,"title":"Inbox","url":"https://mail.example"}]}"#),
    );
    assert_eq!(found(&tab, &world).map(|f| f.0), Some(win(20)), "the person's own Chrome");
    let untitled = mcp("chrome-devtools", "click", r#"{"uid":"1_4"}"#, Some("Clicked"));
    assert_eq!(found(&untitled, &world).map(|f| f.0), Some(win(30)));
    let mut two = world;
    two.windows.push(window(31, "Chromium", "org.chromium.Chromium", "", (900.0, 700.0)));
    assert_eq!(found(&untitled, &two), None, "two automation browsers: no guess");
    assert_eq!(clue(&exec("agent-browser open https://example.com")), Some(Clue::Browser(None)));
}

/// A simulator call means the simulator it names, by id or by name, or the only one booted;
/// another platform's device is no simulator.
#[test]
fn a_simulator_is_the_booted_one_the_call_names() {
    let world = world();
    let booted_sim = mcp("XcodeBuildMCP", "launch_app_sim", r#"{"simulatorUuid":"BBBB-2"}"#, None);
    assert_eq!(
        found(&booted_sim, &world),
        Some((
            win(41),
            AgentScreen::SIMULATOR.to_owned(),
            "Simulator \u{2014} iPhone 17 Pro".to_owned()
        ))
    );
    let built = exec(
        "xcodebuild test -scheme App -destination 'platform=iOS Simulator,name=iPhone 17 Pro'",
    );
    assert_eq!(found(&built, &world).map(|f| f.0), Some(win(41)), "the longest name within");
    let shot = exec("xcrun simctl io booted screenshot shot.png");
    assert_eq!(found(&shot, &world), None, "two booted and none named");
    let one = World { booted: world.booted[..1].to_vec(), ..world.clone() };
    assert_eq!(found(&shot, &one).map(|f| f.0), Some(win(40)));
    let android = mcp("agent-device", "press", r#"{"platform":"android","x":1}"#, None);
    assert_eq!(clue(&android), None);
    let ios = mcp("ios-simulator", "ui_tap", r#"{"x":1,"y":2}"#, None);
    assert!(matches!(clue(&ios), Some(Clue::Simulator(_))));
    assert_eq!(clue(&mcp("radiostudios", "tap", "{}", None)), None, "ios only as a word");
}

/// `simctl`'s answer gives the booted devices only.
#[test]
fn simctl_lists_the_booted_devices() {
    let json = r#"{"devices":{
        "com.apple.CoreSimulator.SimRuntime.iOS-26-5":[
            {"udid":"AAAA-1","name":"iPhone 17","state":"Booted"},
            {"udid":"CCCC-3","name":"iPad Air","state":"Shutdown"}],
        "com.apple.CoreSimulator.SimRuntime.watchOS-26-5":[]}}"#;
    assert_eq!(booted(json), [Device { udid: "AAAA-1".to_owned(), name: "iPhone 17".to_owned() }]);
    assert_eq!(booted("not json"), []);
}

fn screen(id: u32, used_ms: u64) -> AgentScreen {
    AgentScreen {
        target: win(id),
        kind: AgentScreen::APP.to_owned(),
        label: format!("App {id}"),
        used_ms: WallMs::from_millis(used_ms),
    }
}

/// The screen driven last goes first, once, and only the last [`KEPT`] stay; one goes when its
/// window closes or long after it was driven, and driving the first one again within the minute
/// tells nothing new.
#[test]
fn the_latest_screen_leads_and_closed_or_stale_ones_go() {
    let at = WallMs::from_millis(10_000_000);
    let after = |ms: u64| WallMs::from_millis(at.as_millis().saturating_add(ms));
    let list = driven(&[screen(1, 0), screen(2, 0), screen(3, 0)], screen(2, 0), at);
    let ids: Vec<CaptureTarget> = list.iter().map(|s| s.target).collect();
    assert_eq!(ids, [win(2), win(1), win(3)]);
    assert_eq!(list.first().map(|s| s.used_ms), Some(at));
    let more = driven(&list, screen(4, 0), at);
    assert_eq!(more.len(), KEPT);
    assert_eq!(more.last().map(|s| s.target), Some(win(1)), "the oldest went");

    assert!(!worth_telling(&list, &driven(&list, screen(2, 0), after(30_000))));
    assert!(worth_telling(&list, &driven(&list, screen(2, 0), after(61_000))));
    assert!(worth_telling(&list, &driven(&list, screen(1, 0), at)), "another order");

    let open = World {
        windows: vec![window(2, "A", "a", "", (1.0, 1.0)), window(1, "B", "b", "", (1.0, 1.0))],
        ..World::default()
    };
    let later = after(u64::try_from(STALE.as_millis()).unwrap_or(u64::MAX).saturating_sub(1));
    let left = kept(&[screen(2, 10_000_000), screen(1, 1), screen(3, 10_000_000)], &open, later);
    assert_eq!(left, [screen(2, 10_000_000)], "1 went stale, 3 closed");
}
