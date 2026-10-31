//! The shell drawn in the test process as the self-test's app draws it, and held to its goldens.
//!
//! A test here opens the shell's window as [`crate::start`] opens it (the same install, fonts,
//! theme and window options) in a GPUI test window that draws real pixels: Core Text through
//! `gpui_platform::text_system`, the app's assets, and gpui-fast's headless Metal renderer, the
//! `render_frame` the app's window draws with. The state is set on the shell's `AppState` as the
//! view model sets it; no app process, daemon or guest runs, and a frame takes milliseconds.
//!
//! A frame is laid over the self-test's backdrop and held to its golden by the comparator both
//! kinds of golden share (`slopty_testkit::golden`), and what it says (its whole accessibility
//! tree, as the self-test's words) and what it paints (its scene, [`scene::lines`]) are insta
//! snapshots, so a changed word, place or colour fails as text in a lane's quick check and the
//! picture judges only what only pixels say. A golden scoped to its surface holds that surface's
//! picture and scene alone, and the whole window's words, below the fold too.
//!
//! Dark is a check, not a picture: the dark frame says what the light one says, places every node
//! and lays out every primitive where the light one does ([`scene::moved`]), neither frame paints
//! a colour only the other theme has ([`scene::foreign`]), and dark sets no text fainter than
//! light does under [`CONTRAST`] ([`scene::fainter`]); a dark picture is kept only where a page
//! shows both themes' colours itself.
//! A relation that mapped each light colour to its dark one was tried and dropped: the shell works
//! colours out in code as well as from the theme (a switch's knob, a hover's opacity), two of its
//! colours alike in light part in dark, and a glyph's raster is a pixel wider or narrower on the
//! other ground, so no mapping held that a correct page kept.
//!
//! The shell's tests run only on Macs, and every Mac has a Metal device, so a machine without one
//! fails here rather than pass unseen (`docs/decisions/testing.md`, "Goldens render in the test
//! process").

mod scene;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui::{Modifiers, TestAppContext, TestDispatcher, VisualTestContext, point, size};
use image::RgbaImage;
use slopty_testkit::golden::{self, Accept, Goldens, MAC_SHARE, Spoken};
use slopty_testkit::stack::{PINNED_MS, SERVER};
use slopty_view_proto::{
    Connectivity, ConnectivityState, Device, DeviceAction, ServerInvite, ServerPresence,
};

use super::*;
use crate::appearance::AppearanceMode;

/// The self-test's window: 1320 × 880 points at one device pixel to the point, as the guest's
/// display draws it.
const WINDOW: (f32, f32) = (1320.0, 880.0);

/// The id this app knows itself by: one no device it lists has, so the worker reads as another
/// machine, as it does to the self-test's app.
const CLIENT: &str = "this-app";

/// The contrast (WCAG's ratio) dark's text stands at against its ground where light sets it at as
/// much or more ([`scene::fainter`]): WCAG's floor for large text and a control's parts, which the
/// quietest live words in dark clear (a field's placeholder at 3.6, a menu's check at 3.3), and
/// well above a colour left near the dark ground.
const CONTRAST: f32 = 3.0;

/// The goldens' directory, the self-test's: a golden is one file whichever test draws it.
fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../slopty-e2e/golden")
}

/// Where a frame's render, its diff and its words go for a review: `SLOPTY_GOLDEN_ARTIFACTS`, or
/// the target directory's `goldens`.
fn artifacts_dir() -> PathBuf {
    std::env::var_os("SLOPTY_GOLDEN_ARTIFACTS").map_or_else(
        || Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/goldens"),
        PathBuf::from,
    )
}

/// What a frame shows: its picture laid over the self-test's backdrop, of `rect` of the window
/// (x, y, width, height in points), its whole accessibility tree and its scene's lines.
struct Frame {
    image: RgbaImage,
    rect: [f32; 4],
    nodes: Vec<Node>,
    scene: Vec<String>,
}

/// A node of a frame's accessibility tree, in reading order.
#[derive(Clone, Debug)]
struct Node {
    role: String,
    label: Option<String>,
    value: Option<String>,
    focused: bool,
    /// Where it is, in points: x, y, width, height.
    bounds: [f32; 4],
}

impl Frame {
    /// What the frame says, as the self-test's words say it.
    fn words(&self) -> String {
        Self::spoken(&self.nodes)
    }

    /// What `nodes` say, as the self-test's words say it.
    fn spoken(nodes: &[Node]) -> String {
        golden::words(nodes.iter().map(|node| Spoken {
            role: &node.role,
            label: node.label.as_deref(),
            value: node.value.as_deref(),
            focused: node.focused,
        }))
    }

    /// The frame scoped to `rect` (x, y, width, height in points, a pixel each): its picture cut
    /// to it and the scene's lines that reach into it. Its words stay the whole window's, as the
    /// self-test's were, those below the fold and beside the surface too.
    fn within(self, rect: [f32; 4]) -> Self {
        let [x, y, w, h] = rect;
        let reaches =
            |[bx, by, bw, bh]: [f32; 4]| bx < x + w && bx + bw > x && by < y + h && by + bh > y;
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "pixels")]
        let image = image::imageops::crop_imm(&self.image, x as u32, y as u32, w as u32, h as u32)
            .to_image();
        let scene =
            self.scene.into_iter().filter(|line| scene::place(line).is_none_or(reaches)).collect();
        Self { image, rect, nodes: self.nodes, scene }
    }

    /// The frame as [`scene::fainter`] reads it.
    fn drawn(&self) -> scene::Drawn<'_> {
        let [left, top, ..] = self.rect;
        scene::Drawn::new(&self.scene, &self.image, [left, top])
    }
}

/// The nodes of `window`'s accessibility tree in reading order, depth first, as the self-test
/// reads them (`slopty_kit::a11y::tree`).
fn nodes(window: &Window) -> Vec<Node> {
    let Some(update) = window.a11y_tree() else { return Vec::new() };
    let by_id: HashMap<_, _> = update.nodes.iter().map(|(id, node)| (*id, node)).collect();
    let Some(root) = update.tree.as_ref().map(|tree| tree.root) else { return Vec::new() };
    let scale = window.scale_factor().max(0.01);
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        let Some(node) = by_id.get(&id) else { continue };
        let bounds = node.bounds().map_or([0.0; 4], |r| {
            #[expect(clippy::cast_possible_truncation, reason = "window points")]
            let p = |v: f64| (v as f32) / scale;
            [p(r.x0), p(r.y0), p(r.x1 - r.x0), p(r.y1 - r.y0)]
        });
        out.push(Node {
            role: format!("{:?}", node.role()),
            label: node.label().map(str::to_owned),
            value: node.value().map(str::to_owned),
            focused: id == update.focus,
            bounds,
        });
        stack.extend(node.children().iter().rev().copied());
    }
    out
}

/// The shell's main window opened as the app opens it, in a test context that draws real
/// pixels, as the self-test's app first shows it: light, on opaque surfaces, motion reduced and
/// the clock pinned.
struct Drawing {
    app: TestAppContext,
    cx: VisualTestContext,
    shell: Entity<Shell>,
    _data: tempfile::TempDir,
    /// The runtime a stand-in engine answers on ([`Self::engine`]), driven between the app's
    /// turns on this thread, so nothing wakes the app's executor from elsewhere.
    runtime: Option<tokio::runtime::Runtime>,
}

impl Drawing {
    /// The window, scheduled by `seed`.
    ///
    /// # Panics
    ///
    /// On a machine with no Metal device to draw with: the shell's tests run on Macs (this one,
    /// the gate's hosted runners), each of which has one, so one without is a broken runner, not
    /// a test to pass.
    fn open(seed: u64) -> Self {
        let renderer = gpui_platform::current_headless_renderer;
        assert!(
            matches!(renderer(), Ok(Some(_))),
            "no Metal device to draw the window with: every Mac the shell's tests run on has one"
        );
        let mut app = TestAppContext::build_with_renderer(
            TestDispatcher::new(seed),
            None,
            gpui_platform::text_system(),
            Arc::new(icons::Assets),
            renderer,
        );
        // The settings the self-test writes before its app starts
        // (`slopty_e2e::harness::pin_interface`), read as the app reads them.
        let data = tempfile::tempdir().expect("a settings directory");
        std::fs::write(
            data.path().join("ui-settings.json"),
            serde_json::json!({ "appearance": "light", "surface": "opaque" }).to_string(),
        )
        .expect("the settings written");
        let dir = data.path().to_path_buf();
        app.update(|cx| {
            crate::install(UiSettings::load(&dir), dir.clone(), cx);
            // As the self-test's socket asks before its first command.
            motion::hold(true, cx);
            crate::clock::pin(Some(PINNED_MS), cx);
        });
        let boot = EngineBootConfig {
            data_dir: dir,
            default_harness: slopty_view_proto::HarnessId::CLAUDE_CODE,
        };
        let window = app
            .update(|cx| {
                let bounds = gpui::Bounds::centered(None, size(px(WINDOW.0), px(WINDOW.1)), cx);
                let options = crate::main_window_options(bounds, None, cx);
                cx.open_window(options, move |window, cx| {
                    crate::prepare_main_window(window, cx);
                    let state = cx.new(|_| AppState::new());
                    cx.new(|cx| {
                        let mut shell = Shell::new(state, boot, cx);
                        // The gate and the splash are the engine's to end; it is ready.
                        shell.debug_gate = Some(GatePhase::Ready);
                        shell.splash = SplashPhase::Gone;
                        shell
                    })
                })
            })
            .expect("the window opens");
        app.update(crate::appearance::reapply_window_background);
        let shell = window.root(&mut app).expect("the shell");
        let mut cx = VisualTestContext::from_window(window.into(), &app);
        cx.simulate_scale_factor_change(1.0);
        cx.simulate_resize(size(px(WINDOW.0), px(WINDOW.1)));
        // GPUI checks every frame that draws a view again against one from scratch.
        cx.update(|window, _| window.set_check_retained(Some(gpui::RetainedCheck::Panic)));
        let mut drawing = Self { app, cx, shell, _data: data, runtime: None };
        drawing.following(vec![worker()]);
        drawing
    }

    /// The state the self-test's stack gives the shell: this app follows [`SERVER`], online and
    /// offering its code for a phone, with `devices` listed (its worker, as the window opens) and
    /// no project yet.
    fn following(&mut self, devices: Vec<Device>) {
        self.state(|state| {
            state.connection = ConnectionStatus::Ready;
            state.workspace_scope = Some(WorkspaceScope::Local);
            state.local_device_id = Some(CLIENT.into());
            state.devices = devices;
            state.connectivity =
                Connectivity { state: ConnectivityState::Connected, server: Some(server()) };
            state.chats_synced = true;
            state.spaces_synced = true;
        });
    }

    /// Change the shell's state as the view model would, and let the window take it in.
    fn state(&mut self, change: impl FnOnce(&mut AppState)) {
        self.shell.update(&mut self.cx, |shell, cx| {
            shell.state.update(cx, |state, cx| {
                change(state);
                cx.notify();
            });
        });
        self.cx.run_until_parked();
    }

    /// Have the shell's engine answer as `service` does, through the RPC client the app reaches its
    /// view model by; its answers come in as [`Self::settle`] drives them.
    fn engine(&mut self, service: Arc<dyn slopty_view_rpc::RpcService>) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime for the engine");
        let engine = {
            let _entered = runtime.enter();
            crate::state::EngineHandle::from_test_client(slopty_view_rpc::memory_client(service))
        };
        self.state(|state| state.set_test_engine(engine));
        self.runtime = Some(runtime);
    }

    /// Let the engine answer and the window take its answers in until the window's tree holds
    /// what `done` asks, or panic naming `what`.
    fn settle(&mut self, what: &str, done: impl Fn(&[Node]) -> bool) {
        for _ in 0..200 {
            self.cx.run_until_parked();
            if let Some(runtime) = &self.runtime {
                runtime.block_on(async {
                    for _ in 0..20 {
                        tokio::task::yield_now().await;
                    }
                });
            }
            self.cx.run_until_parked();
            if done(&self.tree()) {
                return;
            }
        }
        panic!("{what} never showed:\n{}", self.frame().words());
    }

    /// The window's accessibility tree as a frame drawn now shows it.
    fn tree(&mut self) -> Vec<Node> {
        self.cx.update(|window, cx| {
            window.set_a11y_active(true);
            window.draw(cx).clear(cx);
            let nodes = nodes(window);
            window.set_a11y_active(false);
            nodes
        })
    }

    /// Light or dark, as the self-test's `Appearance` command sets it.
    fn appearance(&mut self, dark: bool) {
        self.cx.update(|window, cx| {
            crate::appearance::set_mode(
                if dark { AppearanceMode::Dark } else { AppearanceMode::Light },
                cx,
            );
            window.refresh();
        });
        self.cx.run_until_parked();
    }

    /// Frosted or opaque surfaces, as the self-test's `Surface` command sets them.
    fn frosted(&mut self, frosted: bool) {
        let surface = if frosted {
            slopty_shell_theme::SurfacePreference::Frosted
        } else {
            slopty_shell_theme::SurfacePreference::Opaque
        };
        self.cx.update(|window, cx| {
            crate::appearance::set_surface(surface, cx);
            window.refresh();
        });
        self.cx.run_until_parked();
    }

    /// Click the middle of the node `role` named `label`, leaving the pointer there, as the
    /// self-test's driver clicks.
    fn click(&mut self, role: &str, label: &str) {
        let [x, y, w, h] = self.bounds(role, label);
        let at = point(px(w.mul_add(0.5, x)), px(h.mul_add(0.5, y)));
        self.cx.simulate_click(at, Modifiers::none());
        self.cx.run_until_parked();
    }

    /// Where the node `role` named `label` is drawn now, in points.
    fn bounds(&mut self, role: &str, label: &str) -> [f32; 4] {
        let tree = self.tree();
        let found =
            tree.iter().find(|node| node.role == role && node.label.as_deref() == Some(label));
        match found {
            Some(node) => node.bounds,
            None => panic!("no {role} {label} in:\n{}", Frame::spoken(&tree)),
        }
    }

    /// The frame the window shows now, drawn as the app draws it and rendered as the self-test
    /// renders it, with its tree read from a frame drawn again with the tree asked for.
    fn frame(&mut self) -> Frame {
        self.cx.run_until_parked();
        self.cx.update(|window, cx| {
            window.draw(cx).clear(cx);
            let mut image = window.render_to_image().expect("the window renders");
            golden::over_backdrop(&mut image);
            let scene = scene::lines(&window.painted_primitives());
            window.set_a11y_active(true);
            window.draw(cx).clear(cx);
            let nodes = nodes(window);
            window.set_a11y_active(false);
            Frame { image, rect: [0.0, 0.0, WINDOW.0, WINDOW.1], nodes, scene }
        })
    }

    /// The theme's colours in light and in dark, the settings' own surface's too.
    fn themes(&mut self) -> Themes {
        let mut colours = |dark: bool| {
            self.appearance(dark);
            self.cx.update(|_, cx| {
                let theme = Theme::of(cx);
                let mut colours = scene::theme_colours(&format!("{theme:?}"));
                colours
                    .extend(scene::theme_colours(&format!("{:?}", theme.for_settings_surface())));
                colours
            })
        };
        let (light, dark) = (colours(false), colours(true));
        self.appearance(false);
        Themes { light, dark }
    }
}

/// The colours of the theme in light and in dark, as `#rrggbbaa` reads them.
struct Themes {
    light: Vec<[u8; 4]>,
    dark: Vec<[u8; 4]>,
}

impl Drop for Drawing {
    fn drop(&mut self) {
        self.cx.run_until_parked();
        self.app.quit();
    }
}

/// The server as the self-test's stack shows it: online on this Mac, with the code a phone
/// scans carrying the Mac's tailnet name, made as the app makes it (`ServerInvite::reaching`).
fn server() -> ServerPresence {
    ServerPresence {
        name: SERVER.into(),
        online: true,
        actions: Vec::new(),
        invite: Some(ServerInvite::reaching("mac-studio.tail1234.ts.net:7373")),
    }
}

/// The goldens of a run: the self-test's directory, this run's artifacts, insta's accept
/// (`cargo xtask remote --accept-snapshots`) for the pictures too, and a Mac frame's share.
fn goldens<'a>(dir: &'a Path, artifacts: &'a Path) -> Goldens<'a> {
    Goldens { dir, artifacts, accept: Accept::from_insta_env(), share: MAC_SHARE }
}

/// Hold `image` to the golden `name`.
fn held(name: &str, image: &RgbaImage) -> golden::Diff {
    let (dir, artifacts) = (golden_dir(), artifacts_dir());
    goldens(&dir, &artifacts).pixels(name, image, &[]).unwrap_or_else(|error| panic!("{error:#}"))
}

/// What a golden's picture and scene hold: the whole window, the page beside the settings'
/// sections, or a dialog over it, with the room its shadow takes.
#[derive(Clone, Copy)]
enum Scope {
    Window,
    Page,
    Dialog(&'static str),
}

/// The room a dialog's shadow takes round it, in points.
const SHADOW: f32 = 40.0;

impl Scope {
    /// Where this scope is in `drawing`'s window now, in points.
    fn rect(self, drawing: &mut Drawing) -> [f32; 4] {
        match self {
            Self::Window => [0.0, 0.0, WINDOW.0, WINDOW.1],
            Self::Page => {
                let left = drawing.cx.update(|_, cx| settings::widgets::sidebar_width(cx));
                [left, 0.0, WINDOW.0 - left, WINDOW.1]
            }
            Self::Dialog(label) => {
                let [x, y, w, h] = drawing.bounds("Dialog", label);
                let (left, top) = ((x - SHADOW).max(0.0).floor(), (y - SHADOW).max(0.0).floor());
                let right = (x + w + SHADOW).min(WINDOW.0).ceil();
                let bottom = (y + h + SHADOW).min(WINDOW.1).ceil();
                [left, top, right - left, bottom - top]
            }
        }
    }
}

/// Snapshot `text` as `name` beside these tests (`goldens/snapshots`).
fn snapshot(name: &str, text: &str) {
    insta::with_settings!({
        snapshot_path => "goldens/snapshots",
        prepend_module_to_snapshot => false,
        omit_expression => true,
    }, {
        insta::assert_snapshot!(name, text);
    });
}

/// Hold the state the window shows to its goldens, `scope`d: in light its picture
/// (`<name>-light.png`), its words (`<name>`) and its scene (`<name>-scene`), and in dark what
/// [`dark_check`] holds.
fn hold(
    drawing: &mut Drawing,
    name: &str,
    scope: Scope,
    themes: &Themes,
    fixed: &[[f32; 4]],
    dark: Dark,
) {
    let rect = scope.rect(drawing);
    let light = drawing.frame().within(rect);
    held(&format!("{name}-light"), &light.image);
    snapshot(name, &light.words());
    snapshot(&format!("{name}-scene"), &light.scene.join("\n"));
    dark_check(drawing, name, &light, themes, fixed, dark);
}

/// The state drawn in dark, the rect `light` holds, held to `light`: the same words, the
/// same nodes in the same places and the same layout ([`scene::moved`]), and no text fainter
/// than light sets it under [`CONTRAST`] ([`scene::fainter`]); then its picture (`<name>-dark.png`)
/// where `dark` keeps one, a page that shows both themes' colours itself, or else no colour only
/// the other theme has in either frame ([`scene::foreign`]) outside `fixed` (rects in points whose
/// colours stay by design, a code a camera reads).
fn dark_check(
    drawing: &mut Drawing,
    name: &str,
    light: &Frame,
    themes: &Themes,
    fixed: &[[f32; 4]],
    dark: Dark,
) {
    drawing.appearance(true);
    let darkened = drawing.frame().within(light.rect);
    drawing.appearance(false);
    assert_eq!(darkened.words(), light.words(), "{name}: dark says what light says");
    let places = |frame: &Frame| -> Vec<String> {
        frame.nodes.iter().map(|node| format!("{} {:?}", node.role, node.bounds)).collect()
    };
    let (light_places, dark_places) = (places(light), places(&darkened));
    assert_eq!(light_places.len(), dark_places.len(), "{name}: dark has the nodes light has");
    let misplaced: Vec<String> = light_places
        .iter()
        .zip(&dark_places)
        .filter(|(light, dark)| light != dark)
        .map(|(light, dark)| format!("light {light}, dark {dark}"))
        .collect();
    assert!(misplaced.is_empty(), "{name}: dark places nodes apart:\n{}", misplaced.join("\n"));
    let moved = scene::moved(&light.scene, &darkened.scene);
    assert!(moved.is_empty(), "{name}: dark lays out apart:\n{}", moved.join("\n"));
    let faint = scene::fainter(&light.drawn(), &darkened.drawn(), CONTRAST);
    assert!(faint.is_empty(), "{name}: text fainter in dark:\n{}", faint.join("\n"));
    if dark == Dark::Picture {
        held(&format!("{name}-dark"), &darkened.image);
        return;
    }
    let outside = |frame: &Frame| -> Vec<String> {
        let fixed_by_design = |[x, y, w, h]: [f32; 4]| {
            fixed
                .iter()
                .any(|&[fx, fy, fw, fh]| x >= fx && y >= fy && x + w <= fx + fw && y + h <= fy + fh)
        };
        let kept = |line: &&String| scene::place(line).is_none_or(|at| !fixed_by_design(at));
        frame.scene.iter().filter(kept).cloned().collect()
    };
    let stray = [
        scene::foreign(&outside(light), &themes.light, &themes.dark),
        scene::foreign(&outside(&darkened), &themes.dark, &themes.light),
    ]
    .concat();
    assert!(stray.is_empty(), "{name}: a colour of the other theme:\n{}", stray.join("\n"));
}

/// How a golden holds its dark frame: by the colours it paints, or, for a page that shows both
/// themes' colours itself, by a picture of its own.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Dark {
    Colours,
    Picture,
}

/// The self-test's machine as Settings asks it: Claude Code installed on it, Codex and pi not,
/// its agents on Auto; a look on the tailnet that finds no server; and a run over SSH to a
/// machine no resolver knows, which fails. It keeps what it was asked.
#[derive(Default)]
struct Machine {
    asked: parking_lot::Mutex<Vec<(String, serde_json::Value)>>,
}

/// A machine no resolver knows (`.invalid` is reserved for that).
const NO_HOST: &str = "nohost.invalid";

/// What looking up [`NO_HOST`] says on a Mac: the standard library's words round the system's
/// (`gai_strerror(EAI_NONAME)`), the line a run that fails shows under its title.
const NO_SUCH_NAME: &str =
    "failed to lookup address information: nodename nor servname provided, or not known";

#[async_trait::async_trait]
impl slopty_view_rpc::RpcService for Machine {
    async fn handle(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<slopty_view_rpc::RpcReply, slopty_view_rpc::RpcError> {
        use slopty_view_rpc::{RpcReply, methods};
        self.asked.lock().push((method.to_owned(), params));
        let agent = |id: &str, name: &str, installed: bool| {
            serde_json::json!({
                "id": id, "name": name, "supportsSteering": true, "steeringMode": "step-boundary",
                "reasoningLevels": [], "installed": installed, "canInstall": true,
                "enabled": installed,
            })
        };
        let once =
            |item: serde_json::Value| RpcReply::Stream(Box::pin(futures::stream::iter([item])));
        Ok(match method {
            methods::LIST_HARNESSES => RpcReply::Value(serde_json::json!([
                agent("claude-code", "Claude Code", true),
                agent("codex", "Codex", false),
                agent("pi", "pi", false),
            ])),
            methods::GET_AGENT_SETTINGS => {
                RpcReply::Value(serde_json::json!({ "autonomy": "auto" }))
            }
            methods::LOOK_ON_TAILNET => once(serde_json::json!({ "running": true })),
            methods::ADD_DEVICE => {
                // In the words a deploy says it in, as the app does where it installs nothing.
                let failure = slopty_deploy::Failure::unnamed(NO_HOST, vec![NO_SUCH_NAME.into()]);
                once(serde_json::json!({
                    "run": 1,
                    "failure": {
                        "title": failure.title, "hint": failure.hint, "lines": failure.lines,
                    },
                }))
            }
            methods::MUTATE => RpcReply::Value(serde_json::json!({})),
            other => {
                return Err(slopty_view_rpc::RpcError::Failed(format!("Settings asked {other}")));
            }
        })
    }
}

/// The self-test's worker, as the server lists it: `surfaces`, on macOS, online.
fn worker() -> Device {
    Device {
        id: "f90c4d49078400a418850c6d7ec25b6b".into(),
        name: "surfaces".into(),
        platform: "macos".into(),
        last_seen_at: DateTime::from_timestamp_millis(PINNED_MS.cast_signed()),
        created_at: None,
        version: Some("0.1.0".into()),
        actions: vec![DeviceAction { id: "reveal".into(), label: "Show in Finder".into() }],
        ssh_host: None,
        turned_away: None,
    }
}

/// Settings opened as the self-test opens it (⌘,), on the window following the self-test's
/// server with its worker and the machine answering for it.
fn settings(seed: u64) -> (Drawing, Themes, Arc<Machine>) {
    let mut drawing = Drawing::open(seed);
    let machine = Arc::new(Machine::default());
    drawing.engine(Arc::<Machine>::clone(&machine));
    let themes = drawing.themes();
    drawing.cx.simulate_keystrokes("cmd-,");
    drawing.cx.run_until_parked();
    (drawing, themes, machine)
}

/// The window as it first opens, on its opaque surfaces and frosted over the backdrop: the
/// self-test's own first frames (`interface`, `interface-frosted`), which this test writes and
/// the self-test on a real stack is held to without writing them, so the two drawing apart fails
/// there. Their words are the self-test's `.txt` beside them; their dark is checked here, and its
/// picture is the self-test's.
#[test]
fn the_interface_draws_as_its_goldens() {
    let mut drawing = Drawing::open(0);
    let (dir, artifacts) = (golden_dir(), artifacts_dir());
    for (name, frosted) in [("interface", false), ("interface-frosted", true)] {
        drawing.frosted(frosted);
        let themes = drawing.themes();
        let light = drawing.frame();
        let goldens = goldens(&dir, &artifacts);
        goldens.pixels(name, &light.image, &[]).unwrap_or_else(|error| panic!("{error:#}"));
        goldens.text(name, &light.words()).unwrap_or_else(|error| panic!("{error:#}"));
        snapshot(&format!("{name}-scene"), &light.scene.join("\n"));
        dark_check(&mut drawing, name, &light, &themes, &[], Dark::Colours);
    }
}

/// Appearance: the whole window once, Settings' sections beside the page, in light and in dark
/// (the page shows the appearance itself, so its dark is a picture of its own); a row off its
/// default (the self-test's opaque glass) offers Reset, and only that row, the ones below the
/// fold too; and the light theme's menu, the default marked.
#[test]
fn settings_appearance_draws_as_its_goldens() {
    let (mut drawing, themes, _machine) = settings(1);
    drawing.click("Tab", "Providers");
    drawing.click("Tab", "Appearance");
    hold(&mut drawing, "settings-appearance", Scope::Window, &themes, &[], Dark::Picture);
    let tree = drawing.tree();
    let label = |node: &Node| node.label.clone().unwrap_or_default();
    assert!(
        tree.iter().any(|node| label(node) == "Conversation width: Default"),
        "the last row, below the fold, is in the tree:\n{}",
        Frame::spoken(&tree)
    );
    let resets: Vec<String> =
        tree.iter().map(label).filter(|label| label.starts_with("Reset ")).collect();
    assert_eq!(resets, ["Reset Glass to default"], "Reset only off the default");

    drawing.click("Button", "Light theme: Slopty Light");
    drawing.settle("the theme menu", |tree| {
        tree.iter().any(|node| {
            node.role == "MenuItemRadio" && node.label.as_deref() == Some("Slopty Light")
        })
    });
    hold(&mut drawing, "settings-appearance-defaults", Scope::Page, &themes, &[], Dark::Picture);
}

/// Shortcuts: every binding by what it does.
#[test]
fn settings_shortcuts_draws_as_its_golden() {
    let (mut drawing, themes, _machine) = settings(2);
    drawing.click("Tab", "Shortcuts");
    hold(&mut drawing, "settings-shortcuts", Scope::Page, &themes, &[], Dark::Colours);
}

/// Providers: the machine's agents, the one installed switched on and the others offering an
/// install, and how freely they act.
#[test]
fn settings_providers_draws_as_its_golden() {
    let (mut drawing, themes, _machine) = settings(3);
    drawing.click("Tab", "Providers");
    drawing.settle("the agents and their autonomy", |tree| {
        tree.iter().any(|node| node.label.as_deref() == Some("Autonomy: Auto"))
            && tree.iter().any(|node| node.label.as_deref() == Some("Install Codex"))
    });
    hold(&mut drawing, "settings-agents", Scope::Page, &themes, &[], Dark::Colours);
}

/// Devices: the server with its code for a phone, the worker; Add device's dialog, its look on
/// the tailnet under "A server", and a run over SSH that fails, which names the machine and
/// offers to try again; and the server's code.
#[test]
fn settings_devices_draw_as_their_goldens() {
    let (mut drawing, themes, _machine) = settings(4);
    drawing.click("Tab", "Devices");
    hold(&mut drawing, "settings-devices", Scope::Page, &themes, &[], Dark::Colours);

    drawing.click("Button", "Add a device");
    let dialog = Scope::Dialog("Add device");
    hold(&mut drawing, "settings-devices-add", dialog, &themes, &[], Dark::Colours);

    drawing.click("Tab", "A server");
    drawing.settle("the look on the tailnet, which finds no server", |tree| {
        tree.iter().any(|node| {
            node.role == "Status"
                && node.label.as_deref() == Some("No Slopty server found on your tailnet yet.")
        })
    });
    hold(&mut drawing, "settings-devices-add-server", dialog, &themes, &[], Dark::Colours);

    drawing.click("Tab", "Over SSH");
    drawing.cx.simulate_input(NO_HOST);
    drawing.click("Button", "Install");
    drawing.settle("the run's failure", |tree| {
        tree.iter().any(|node| node.role == "Alert")
            && tree.iter().any(|node| node.label.as_deref() == Some("Try again"))
    });
    hold(&mut drawing, "settings-devices-add-failed", dialog, &themes, &[], Dark::Colours);
    drawing.cx.simulate_keystrokes("escape");
    drawing.cx.run_until_parked();

    drawing.click("Button", "Connect a phone or iPad");
    let code = drawing.bounds("Image", "The server's code");
    hold(
        &mut drawing,
        "settings-devices-invite",
        Scope::Dialog("Connect a phone or iPad"),
        &themes,
        &[code],
        Dark::Colours,
    );
}

/// Archived: a session on the worker archived at the pinned moment, so its age reads "now" as
/// the self-test's did, with Unarchive, which asks the machine to take it out of the archive.
#[test]
fn settings_archived_draws_as_its_golden() {
    let (mut drawing, themes, machine) = settings(5);
    let at = DateTime::from_timestamp_millis(PINNED_MS.cast_signed()).expect("a moment");
    let chat: slopty_view_proto::Chat = serde_json::from_value(serde_json::json!({
        "id": "tidy", "title": "Tidy the greeting", "deviceId": worker().id, "archived": true,
        "createdAt": at, "cwd": "/Users/e2e/repo", "lastMessageAt": at, "lastSeenAt": at,
    }))
    .expect("a chat");
    drawing.state(|state| state.chats = vec![chat]);
    drawing.click("Tab", "Archived sessions");
    hold(&mut drawing, "settings-archived", Scope::Page, &themes, &[], Dark::Colours);
    drawing.click("Button", "Unarchive");
    let asked = |machine: &Machine| {
        machine
            .asked
            .lock()
            .iter()
            .find(|(method, _)| method == methods::MUTATE)
            .map(|(_, params)| params.clone())
    };
    drawing.settle("Unarchive asked", |_| asked(&machine).is_some());
    assert_eq!(
        asked(&machine),
        Some(serde_json::json!({ "op": "setChatArchived", "chatId": "tidy", "archived": false })),
        "Unarchive takes the session out of the archive"
    );
}
