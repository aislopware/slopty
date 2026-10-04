//! The windows and displays each thread's agent drives ([`AgentScreen`]), named from the agent's
//! own tool calls, so the person can watch one beside the thread and take it over.
//!
//! Every adapter already maps its agent's calls into the thread model: an MCP tool as its server
//! and tool ([`McpDetail`]) with its input, a command as its text ([`ExecDetail`]). The host
//! tells each call that runs or ran in a thread's last turn ([`Host::tools`]); [`clue`] reads
//! what it says of a screen, and [`resolve`] finds that screen among what the worker can stream
//! now ([`World`]):
//!
//! * A computer-use tool that names a window (Claude Code's `app_*` tools, `cua-driver`'s) is that
//!   window; one that names an application's process or name is that application's largest window;
//!   one that acts on the whole screen is the display.
//! * A simulator's tools (an MCP server for simulators or devices, `simctl` or a simulator
//!   destination in a command) mean the booted simulator the call names, or the only one booted, as
//!   `simctl` lists them: its window in Simulator.
//! * A browser's tools (Claude in Chrome, Playwright, Chrome `DevTools`, `agent-browser`) mean the
//!   browser window titled with the page the tool reported; with no title, the one window of a
//!   browser only automation runs (Chrome for Testing, Chromium).
//!
//! Nothing here reads a screen or acts on one: the calls are the agent's own words, and the
//! windows are the worker's listing. A thread keeps up to [`KEPT`] screens, the latest first;
//! one goes once its window closes or [`STALE`] after the agent last drove it.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde_json::Value;
use slopty_core::{DisplayId, WallMs, WindowId};
use slopty_proto::screen::{CaptureTarget, DisplayInfo, ScreenEvent, WindowInfo};
use slopty_proto::thread::detail::{ExecDetail, McpDetail};
use slopty_proto::thread::{Action, AgentScreen, ThreadId, ThreadState, ToolCall, ToolDetail};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

use super::Host;
use super::host::ToolSeen;

/// Screens a thread keeps at most.
pub const KEPT: usize = 3;
/// How long after the agent last drove a screen it is still offered.
pub const STALE: Duration = Duration::from_mins(15);
/// How often the kept screens are checked for windows that closed.
const SWEEP: Duration = Duration::from_secs(10);
/// How long one look at the worker's windows answers for: an agent's calls come in bursts.
const FRESH: Duration = Duration::from_secs(1);
/// How long `simctl` may take to list the booted simulators.
const SIMCTL_WAIT: Duration = Duration::from_secs(5);
/// A screen driven again is told again only after this long, so a burst of clicks is one action.
const RETOLD: Duration = Duration::from_secs(60);

/// What a tool call says of the screen it drives.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Clue {
    /// A window, by the worker's own id for it.
    Window(WindowId),
    /// An application's window, by the application's process.
    Process(i32),
    /// An application's window, by the application's name or bundle id.
    App(String),
    /// A whole display: the one named, else the main one.
    Display(Option<DisplayId>),
    /// A booted simulator: the one whose id or name is among these words or within them, else
    /// the only one booted.
    Simulator(Vec<String>),
    /// A browser window: the one showing the page titled so, when the tool said.
    Browser(Option<String>),
}

/// A booted simulator, as `simctl` lists it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Device {
    /// Its id.
    pub udid: String,
    /// Its name, which its window in Simulator is titled with.
    pub name: String,
}

/// What the worker can stream now, and what a clue needs to find its screen there.
#[derive(Clone, Debug, Default)]
pub struct World {
    /// The windows.
    pub windows: Vec<WindowInfo>,
    /// Their owners' processes, where a clue needed them.
    pub owners: HashMap<WindowId, i32>,
    /// The displays, the main one first.
    pub displays: Vec<DisplayInfo>,
    /// The booted simulators, where a clue needed them.
    pub booted: Vec<Device>,
}

/// The bundle id of Simulator.
const SIMULATOR_APP: &str = "com.apple.iphonesimulator";
/// Browsers, by bundle id.
const BROWSERS: [&str; 14] = [
    "com.google.Chrome",
    "com.google.Chrome.canary",
    "com.google.Chrome.beta",
    "com.google.chrome.for.testing",
    "org.chromium.Chromium",
    "com.brave.Browser",
    "com.microsoft.edgemac",
    "company.thebrowser.Browser",
    "com.apple.Safari",
    "com.apple.SafariTechnologyPreview",
    "org.mozilla.firefox",
    "org.mozilla.nightly",
    "com.vivaldi.Vivaldi",
    "com.operasoftware.Opera",
];
/// Browsers only automation runs, by bundle id: with no page title to go by, the agent's
/// browser is the one window of these.
const AUTOMATION_BROWSERS: [&str; 3] =
    ["com.google.chrome.for.testing", "org.chromium.Chromium", "org.mozilla.nightly"];

/// What `call` says of the screen it drives; `None` for a call that drives none.
#[must_use]
pub fn clue(call: &ToolCall) -> Option<Clue> {
    let input = serde_json::from_str::<Value>(&call.input.text).ok();
    let output = call.output.as_ref().map_or("", |o| o.text.as_str());
    match &call.detail {
        Some(ToolDetail::Mcp(McpDetail { server, tool })) => {
            mcp_clue(server, tool, input.as_ref(), output)
        }
        Some(ToolDetail::Exec(ExecDetail { command, .. })) => exec_clue(&command.text),
        _ => match mcp_name(&call.name) {
            Some((server, tool)) => mcp_clue(server, tool, input.as_ref(), output),
            None => None,
        },
    }
}

/// An MCP tool's server and tool from the name an agent gives it: `mcp__server__tool` (Claude
/// Code), `server.tool` (Codex).
fn mcp_name(name: &str) -> Option<(&str, &str)> {
    name.strip_prefix("mcp__").and_then(|rest| rest.split_once("__"))
}

/// A server's name folded for matching: lower case, letters and digits only.
fn folded(server: &str) -> String {
    server.chars().filter(char::is_ascii_alphanumeric).map(|c| c.to_ascii_lowercase()).collect()
}

fn mcp_clue(server: &str, tool: &str, input: Option<&Value>, output: &str) -> Option<Clue> {
    let ios =
        server.split(|c: char| !c.is_ascii_alphanumeric()).any(|w| w.eq_ignore_ascii_case("ios"));
    let server = folded(server);
    let has = |words: &[&str]| words.iter().any(|w| server.contains(w));
    if has(&["computeruse"]) || server.starts_with("cua") {
        return computer_clue(tool, input, output);
    }
    if has(&["chrome", "playwright", "browser", "puppeteer", "devtools", "webdriver", "selenium"]) {
        return Some(Clue::Browser(page_title(output)));
    }
    if ios || has(&["simulator", "xcodebuild", "mobile", "device", "appium"]) {
        let platform = input.and_then(|i| i.get("platform")).and_then(Value::as_str);
        let apple = platform.is_none_or(|p| {
            let p = p.to_ascii_lowercase();
            ["ios", "ipados", "tvos", "visionos", "watchos"].contains(&p.as_str())
        });
        return apple.then(|| Clue::Simulator(input.map(strings).unwrap_or_default()));
    }
    None
}

/// The tools of a computer-use server that only ask, and drive no screen.
const ASKING: [&str; 6] = [
    "request_access",
    "list_granted_applications",
    "list_apps",
    "read_clipboard",
    "write_clipboard",
    "release_full_control",
];

fn computer_clue(tool: &str, input: Option<&Value>, output: &str) -> Option<Clue> {
    if ASKING.contains(&tool) {
        return None;
    }
    let field = |names: &[&str]| {
        names.iter().find_map(|n| input.and_then(|i| i.get(*n))).and_then(Value::as_i64)
    };
    if let Some(id) = field(&["window_id", "windowId"]).and_then(|id| u32::try_from(id).ok()) {
        return Some(Clue::Window(WindowId(id)));
    }
    if let Some(id) = captured_window(output) {
        return Some(Clue::Window(id));
    }
    if let Some(pid) = field(&["pid"]).and_then(|pid| i32::try_from(pid).ok()) {
        return Some(Clue::Process(pid));
    }
    if tool.starts_with("app_") || tool == "open_application" {
        let app = ["app", "application", "bundle_id", "bundleId", "name"]
            .iter()
            .find_map(|n| input.and_then(|i| i.get(*n)).and_then(Value::as_str));
        return app.map(|app| Clue::App(app.to_owned()));
    }
    let display = field(&["display", "display_id", "displayId"])
        .and_then(|id| u32::try_from(id).ok())
        .map(DisplayId);
    Some(Clue::Display(display))
}

/// The window a computer-use tool says it captured (`Captured window_id 123`).
fn captured_window(output: &str) -> Option<WindowId> {
    let at = output.find("window_id")?;
    let digits: String = output
        .get(at.saturating_add("window_id".len())..)?
        .trim_start_matches([' ', ':', '='])
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok().map(WindowId)
}

/// The page title a browser tool reported: Playwright's `Page Title:` line, else the first
/// `"title"` its JSON gives.
fn page_title(output: &str) -> Option<String> {
    let line = output.lines().find_map(|line| {
        let line = line.trim_start_matches(|c: char| c == '-' || c == '*' || c.is_whitespace());
        line.strip_prefix("Page Title:").map(str::trim)
    });
    let title = line.map(str::to_owned).or_else(|| {
        let at = output.find("\"title\"")?;
        let rest = output.get(at.saturating_add("\"title\"".len())..)?;
        let rest = rest.trim_start().strip_prefix(':')?.trim_start().strip_prefix('"')?;
        let end = rest.find('"')?;
        rest.get(..end).map(str::to_owned)
    })?;
    (!title.is_empty()).then_some(title)
}

/// Every string in `value`, nested or not: the ids and names a simulator call may give.
fn strings(value: &Value) -> Vec<String> {
    match value {
        Value::String(s) => vec![s.clone()],
        Value::Array(items) => items.iter().flat_map(strings).collect(),
        Value::Object(fields) => fields.values().flat_map(strings).collect(),
        _ => Vec::new(),
    }
}

fn exec_clue(command: &str) -> Option<Clue> {
    let words: Vec<&str> =
        command.split(|c: char| c.is_whitespace() || "'\",=".contains(c)).collect();
    let simulator = words.contains(&"simctl")
        || command.contains("iOS Simulator")
        || command.contains("open -a Simulator");
    if simulator {
        return Some(Clue::Simulator(vec![command.to_owned()]));
    }
    let browser = words.iter().any(|w| matches!(*w, "agent-browser" | "playwright-cli"));
    browser.then_some(Clue::Browser(None))
}

/// The screen `clue` names in `world`: what a stream of it opens, its kind and its label.
#[must_use]
pub fn resolve(clue: &Clue, world: &World) -> Option<AgentScreen> {
    let window = |w: &WindowInfo| Some(of_window(w));
    match clue {
        Clue::Window(id) => world.windows.iter().find(|w| w.id == *id).and_then(window),
        Clue::Process(pid) => {
            largest(world.windows.iter().filter(|w| world.owners.get(&w.id) == Some(pid)))
                .and_then(window)
        }
        Clue::App(app) => largest(world.windows.iter().filter(|w| {
            w.app.eq_ignore_ascii_case(app)
                || w.bundle_id.as_deref().is_some_and(|b| b.eq_ignore_ascii_case(app))
        }))
        .and_then(window),
        Clue::Display(id) => {
            let at = match id {
                Some(id) => world.displays.iter().position(|d| d.id == *id),
                None => (!world.displays.is_empty()).then_some(0),
            }?;
            let display = world.displays.get(at)?;
            let label = if world.displays.len() > 1 {
                format!("Display {}", at.saturating_add(1))
            } else {
                "Desktop".to_owned()
            };
            Some(screen(CaptureTarget::Display(display.id), AgentScreen::DESKTOP, label))
        }
        Clue::Simulator(named) => simulator(named, world).and_then(window),
        Clue::Browser(title) => browser(title.as_deref(), world).and_then(window),
    }
}

fn screen(target: CaptureTarget, kind: &str, label: String) -> AgentScreen {
    AgentScreen { target, kind: kind.to_owned(), label, used_ms: WallMs::ZERO }
}

/// `w` as a screen: its kind by its application, its label the application and its title.
fn of_window(w: &WindowInfo) -> AgentScreen {
    let bundle = w.bundle_id.as_deref().unwrap_or_default();
    let kind = if bundle == SIMULATOR_APP {
        AgentScreen::SIMULATOR
    } else if BROWSERS.contains(&bundle) {
        AgentScreen::BROWSER
    } else {
        AgentScreen::APP
    };
    let label = match (w.app.is_empty(), w.title.is_empty()) {
        (_, true) => w.app.clone(),
        (true, false) => w.title.clone(),
        (false, false) => format!("{} \u{2014} {}", w.app, w.title),
    };
    screen(CaptureTarget::Window(w.id), kind, label)
}

/// The largest of `windows` on screen, else the largest of them.
fn largest<'a>(windows: impl Iterator<Item = &'a WindowInfo>) -> Option<&'a WindowInfo> {
    windows.max_by(|a, b| a.on_screen.cmp(&b.on_screen).then((a.w * a.h).total_cmp(&(b.w * b.h))))
}

/// The window of the simulator `named` names, or of the only one booted; with nothing booted
/// that `simctl` said, the only window Simulator has.
fn simulator<'a>(named: &[String], world: &'a World) -> Option<&'a WindowInfo> {
    let windows: Vec<&WindowInfo> =
        world.windows.iter().filter(|w| w.bundle_id.as_deref() == Some(SIMULATOR_APP)).collect();
    let exact = world.booted.iter().find(|d| named.iter().any(|n| *n == d.udid || *n == d.name));
    let within = || {
        world
            .booted
            .iter()
            .filter(|d| named.iter().any(|n| n.contains(&d.udid) || n.contains(&d.name)))
            .max_by_key(|d| d.name.len())
    };
    let only = match world.booted.as_slice() {
        [only] => Some(only),
        _ => None,
    };
    let device = exact.or_else(within).or(only);
    match device {
        Some(device) => windows.into_iter().find(|w| w.title.starts_with(&device.name)),
        None => match windows.as_slice() {
            [only] => Some(*only),
            _ => None,
        },
    }
}

/// The browser window showing the page titled `title`; with no title, the one window of a
/// browser only automation runs.
fn browser<'a>(title: Option<&str>, world: &'a World) -> Option<&'a WindowInfo> {
    let is = |w: &&WindowInfo, of: &[&str]| w.bundle_id.as_deref().is_some_and(|b| of.contains(&b));
    let browsers = || world.windows.iter().filter(|w| is(w, &BROWSERS));
    if let Some(title) = title {
        return browsers()
            .find(|w| w.title == title)
            .or_else(|| browsers().find(|w| w.title.starts_with(title)));
    }
    let automated: Vec<&WindowInfo> = browsers().filter(|w| is(w, &AUTOMATION_BROWSERS)).collect();
    match automated.as_slice() {
        [only] => Some(*only),
        _ => None,
    }
}

/// The booted simulators in `simctl list devices booted -j`'s answer.
#[must_use]
pub fn booted(json: &str) -> Vec<Device> {
    let Ok(listed) = serde_json::from_str::<Value>(json) else { return Vec::new() };
    let Some(runtimes) = listed.get("devices").and_then(Value::as_object) else {
        return Vec::new();
    };
    runtimes
        .values()
        .filter_map(Value::as_array)
        .flatten()
        .filter(|d| d.get("state").and_then(Value::as_str) == Some("Booted"))
        .filter_map(|d| {
            let text = |key: &str| d.get(key).and_then(Value::as_str).map(str::to_owned);
            Some(Device { udid: text("udid")?, name: text("name")? })
        })
        .collect()
}

/// `screens` with `now` driven at `at`: first, once, and no more than [`KEPT`] of them.
#[must_use]
pub fn driven(screens: &[AgentScreen], now: AgentScreen, at: WallMs) -> Vec<AgentScreen> {
    let first = AgentScreen { used_ms: at, ..now };
    std::iter::once(first.clone())
        .chain(screens.iter().filter(|s| s.target != first.target).cloned())
        .take(KEPT)
        .collect()
}

/// `screens` without those `world` no longer lists, or that the agent last drove [`STALE`]
/// before `at`.
#[must_use]
pub fn kept(screens: &[AgentScreen], world: &World, at: WallMs) -> Vec<AgentScreen> {
    screens
        .iter()
        .filter(|s| at.since(s.used_ms) < STALE)
        .filter(|s| match s.target {
            CaptureTarget::Window(id) => world.windows.iter().any(|w| w.id == id),
            CaptureTarget::Display(id) => world.displays.iter().any(|d| d.id == id),
        })
        .cloned()
        .collect()
}

/// Whether `next` is worth telling over `was`: another screen, another order or label, or the
/// first one driven again long after it was last told.
fn worth_telling(was: &[AgentScreen], next: &[AgentScreen]) -> bool {
    let same = |a: &AgentScreen, b: &AgentScreen| {
        a.target == b.target && a.kind == b.kind && a.label == b.label
    };
    let changed = was.len() != next.len() || was.iter().zip(next).any(|(a, b)| !same(a, b));
    let stale = match (was.first(), next.first()) {
        (Some(a), Some(b)) => b.used_ms.since(a.used_ms) >= RETOLD,
        _ => false,
    };
    changed || stale
}

/// Names the screens of every thread's agent as its calls come ([`Screens::spawn`]).
#[derive(Clone, Debug)]
pub struct Screens {
    host: Host,
    /// The last look at the worker's windows, and when it was taken.
    seen: Arc<Mutex<Option<(Instant, World)>>>,
    /// A world the caller keeps in place of the worker's own, where one is.
    kept: Option<Arc<Mutex<World>>>,
}

impl Screens {
    /// The screens of `host`'s threads, among what this worker can stream.
    #[must_use]
    pub fn new(host: Host) -> Self {
        Self { host, seen: Arc::default(), kept: None }
    }

    /// The screens of `host`'s threads among `world`, which the caller changes as it likes: a
    /// stand-in for the worker's windows, where a test sets them.
    #[must_use]
    pub fn among(host: Host, world: Arc<Mutex<World>>) -> Self {
        Self { host, seen: Arc::default(), kept: Some(world) }
    }

    /// Follow every thread's tool calls from now on, and let go of screens that closed.
    #[must_use]
    pub fn spawn(&self) -> JoinHandle<()> {
        let this = self.clone();
        let mut tools = self.host.tools();
        tokio::spawn(async move {
            let mut sweep = tokio::time::interval(SWEEP);
            sweep.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    seen = tools.recv() => match seen {
                        Ok(seen) => this.heard(&seen).await,
                        Err(broadcast::error::RecvError::Lagged(missed)) => {
                            tracing::debug!(missed, "screens missed tool calls");
                        }
                        Err(broadcast::error::RecvError::Closed) => return,
                    },
                    _ = sweep.tick() => this.sweep().await,
                }
            }
        })
    }

    /// A tool call ran: the screen it drives goes first in its thread's.
    async fn heard(&self, seen: &ToolSeen) {
        let Some(clue) = clue(&seen.call) else { return };
        let world = self.world(&clue).await;
        let Some(screen) = resolve(&clue, &world) else { return };
        let now = WallMs::now();
        self.host.update(seen.thread, |state| {
            let next = driven(&state.screens, screen, now);
            let actions = if worth_telling(&state.screens, &next) {
                vec![Action::ScreensSet(next)]
            } else {
                vec![]
            };
            (actions, ())
        });
    }

    /// Let go of the screens that closed or went stale.
    async fn sweep(&self) {
        let held: Vec<ThreadId> =
            self.host.visit(|s: &ThreadState| (!s.screens.is_empty()).then_some(s.meta.id));
        if held.is_empty() {
            return;
        }
        let world = self.world(&Clue::Display(None)).await;
        let now = WallMs::now();
        for thread in held {
            self.host.update(thread, |state| {
                let next = kept(&state.screens, &world, now);
                let actions =
                    if next == state.screens { vec![] } else { vec![Action::ScreensSet(next)] };
                (actions, ())
            });
        }
    }

    /// The worker's windows and displays, with what `clue` needs beside them.
    async fn world(&self, clue: &Clue) -> World {
        if let Some(kept) = &self.kept {
            return kept.lock().clone();
        }
        let fresh = self
            .seen
            .lock()
            .as_ref()
            .filter(|(at, _)| at.elapsed() < FRESH)
            .map(|(_, world)| world.clone());
        let mut world = if let Some(world) = fresh {
            world
        } else {
            let world = match crate::screen::listing().await {
                Ok(ScreenEvent::Listing { windows, displays }) => {
                    World { windows, displays, ..World::default() }
                }
                Ok(_) | Err(_) => World::default(),
            };
            *self.seen.lock() = Some((Instant::now(), world.clone()));
            world
        };
        match clue {
            Clue::Simulator(_) => world.booted = booted_now().await,
            Clue::Process(_) => world.owners = owners(&world.windows),
            _ => {}
        }
        world
    }
}

/// The simulators booted now, as `xcrun simctl` lists them; none where it cannot.
async fn booted_now() -> Vec<Device> {
    let run = tokio::process::Command::new("xcrun")
        .args(["simctl", "list", "devices", "booted", "-j"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();
    match tokio::time::timeout(SIMCTL_WAIT, run).await {
        Ok(Ok(out)) if out.status.success() => booted(&String::from_utf8_lossy(&out.stdout)),
        _ => Vec::new(),
    }
}

/// The process that owns each of `windows`.
fn owners(windows: &[WindowInfo]) -> HashMap<WindowId, i32> {
    #[cfg(target_os = "macos")]
    {
        windows
            .iter()
            .filter_map(|w| slopty_capture::window_owner_pid(w.id).map(|pid| (w.id, pid)))
            .collect()
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = windows;
        HashMap::new()
    }
}

#[cfg(test)]
mod tests;
