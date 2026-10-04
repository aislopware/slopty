//! The screens an agent drives, named in its thread from the tool calls the host hears: calls
//! are applied as an adapter applies them, over a world of windows the test keeps.

#[cfg(test)]
mod screens {
    use std::sync::Arc;
    use std::time::Duration;

    use parking_lot::Mutex;
    use slopty_agent::observed::{Observed, Out};
    use slopty_core::{DisplayId, WallMs, WindowId};
    use slopty_proto::screen::{CaptureTarget, DisplayInfo, WindowInfo};
    use slopty_proto::thread::detail::McpDetail;
    use slopty_proto::thread::{
        Action, AgentScreen, Changed, Clipped, Item, ItemBody, ItemId, ThreadId, ThreadState,
        ToolCall, ToolDetail, ToolState, Turn, TurnId, TurnState, Usage, kind,
    };
    use slopty_worker::thread::Host;
    use slopty_worker::thread::log::Limits;
    use slopty_worker::thread::screens::{Screens, World};

    /// Long enough for a loaded machine; the tests judge what comes.
    const BOUND: Duration = Duration::from_secs(30);

    struct Rig {
        host: Host,
        thread: ThreadId,
        _screens: tokio::task::JoinHandle<()>,
    }

    impl Rig {
        fn new(dir: &std::path::Path, world: Arc<Mutex<World>>) -> Self {
            let host = Host::open(&dir.join("threads"), Limits::default()).unwrap();
            let mut observed = Observed::new("s1", "", None, "/work", WallMs::ZERO);
            let Some(Out::Begin(meta)) = observed.drain().into_iter().next() else { panic!() };
            let thread = meta.id;
            host.create(*meta).unwrap();
            let screens = Screens::among(host.clone(), world).spawn();
            Self { host, thread, _screens: screens }
        }

        fn apply(&self, action: Action) {
            self.host.apply(self.thread, vec![action]);
        }

        fn begin(&self, turn: u32) {
            self.apply(Action::TurnStarted(Turn {
                id: TurnId(turn),
                input: None,
                state: TurnState::Active,
                started_ms: WallMs::ZERO,
                ended_ms: None,
                usage: Usage::default(),
                models: Vec::new(),
                changed: Changed::default(),
                before: None,
                after: None,
            }));
        }

        /// The agent's computer-use call `id` in `turn` on window `window`.
        fn click(&self, id: &str, turn: u32, window: u32) {
            let call = ToolCall {
                name: "mcp__computer-use__app_click".to_owned(),
                kind: kind::MCP.to_owned(),
                title: "computer-use: app_click".to_owned(),
                input: Clipped::whole(&format!(r#"{{"window_id":{window},"x":3,"y":4}}"#)),
                state: ToolState::Running,
                output: None,
                images: Vec::new(),
                detail: Some(ToolDetail::Mcp(McpDetail {
                    server: "computer-use".to_owned(),
                    tool: "app_click".to_owned(),
                })),
                child: None,
                ended_ms: None,
            };
            self.apply(Action::ItemStarted(Item {
                id: ItemId(id.to_owned()),
                turn: TurnId(turn),
                at_ms: WallMs::ZERO,
                body: ItemBody::Tool(Box::new(call)),
            }));
        }

        async fn until(&self, done: impl Fn(&ThreadState) -> bool) -> ThreadState {
            let mut feed = self.host.watch(self.thread).unwrap();
            let waited = tokio::time::timeout(BOUND, async {
                loop {
                    let state = self.host.state(self.thread).unwrap().0;
                    if done(&state) {
                        return state;
                    }
                    let _batch = feed.recv().await;
                }
            });
            waited.await.expect("the thread came to the state awaited")
        }
    }

    fn window(id: u32, title: &str) -> WindowInfo {
        WindowInfo {
            id: WindowId(id),
            app: "Notes".to_owned(),
            bundle_id: Some("com.apple.Notes".to_owned()),
            title: title.to_owned(),
            x: 0.0,
            y: 0.0,
            w: 600.0,
            h: 400.0,
            display: DisplayId(1),
            on_screen: true,
        }
    }

    fn targets(state: &ThreadState) -> Vec<CaptureTarget> {
        state.screens.iter().map(|s| s.target).collect()
    }

    /// A call in the thread's last turn names its window there, the latest first; a call the
    /// log tells again from an earlier turn names nothing.
    #[tokio::test]
    async fn the_agents_calls_name_the_windows_it_drives() {
        let dir = tempfile::tempdir().unwrap();
        let world = Arc::new(Mutex::new(World {
            windows: vec![window(10, "Groceries"), window(11, "Recipes"), window(12, "Old")],
            displays: vec![DisplayInfo {
                id: DisplayId(1),
                w: 1512.0,
                h: 982.0,
                scale: 2.0,
                hz: 60.0,
            }],
            ..World::default()
        }));
        let rig = Rig::new(dir.path(), world);
        rig.begin(1);
        rig.begin(2);
        rig.click("toolu_old", 1, 12);
        rig.click("toolu_1", 2, 10);
        let state = rig.until(|s| !s.screens.is_empty()).await;
        let first = state.screens.first().unwrap();
        assert_eq!(first.target, CaptureTarget::Window(WindowId(10)));
        assert_eq!(first.kind, AgentScreen::APP);
        assert_eq!(first.label, "Notes \u{2014} Groceries");
        rig.click("toolu_2", 2, 11);
        let state = rig.until(|s| s.screens.len() == 2).await;
        assert_eq!(
            targets(&state),
            [CaptureTarget::Window(WindowId(11)), CaptureTarget::Window(WindowId(10))]
        );
        assert!(
            !targets(&state).contains(&CaptureTarget::Window(WindowId(12))),
            "an earlier turn's call is history"
        );
    }
}
