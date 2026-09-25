# Testing

## Four layers, fastest first
1. **Unit**, in every crate: pure logic behind traits with fakes (`slopty_input::Recorder`
   for the injector, `Layout`/`ItemDoc` in `slopty-client`). Runs under `cargo gate`.
2. **Headless GPUI**, `#[gpui::test]` in `slopty-ui`: a `VisualTestContext` window with real
   layout, `simulate_keystrokes`/`simulate_click`, a channel for the worker. Read the UI like
   a DOM: `cx.debug_bounds("item-<uuid>")` (set with `.debug_selector`), `window.painted_quads()`
   for colours and borders, view accessors (`rows()`, `zoom()`, `active_item()`),
   `slopty_ui::a11y::tree(window)` after `window.set_a11y_active(true)` for roles, labels and
   the focused node. No pixels, no process, runs under `cargo gate`. Every workspace/terminal
   behaviour gets a test here first.
3. **App self-test**, `cargo xtask e2e app` (`crates/slopty-e2e`, gate `SLOPTY_APP_E2E`):
   launches ptyd + the worker + the app (built with `--features slopty/e2e`) in a temp dir, pairs
   them, and drives the app over its own control socket (`SLOPTY_TEST_SOCKET`: keys, clicks,
   dump, render). `dump` is the structured state (items, focus, zoom, terminal rows, and
   `a11y`: the accessibility tree as role/label/value/focused/bounds in reading order,
   from the frame that painted the state in the same dump);
   `render` is GPUI drawing its own window to a PNG, compared numerically with
   `crates/slopty-e2e/golden` (`--accept` writes missing and failing goldens, `--accept-all` rewrites every golden). The temp dir
   is named for its test, not at random, because a golden draws the paths under it. Nothing touches another app.
   `cargo xtask e2e ios [--sim iphone|ipad]` (gate `SLOPTY_IOS_E2E`) is the same socket with
   the app in the simulator: the way to check anything on the phone or the tablet. There the
   socket also takes `ui_key_press` / `ui_touch` / `ui_pinch` / `ui_insert_text` /
   `ui_delete_backward` (`Driver::ui_key`, `ui_tap`, `ui_pan`, `ui_pinch`, `ui_insert_text`),
   delivered at the UIKit boundary by the fork (`tests/ios_uikit.rs`): use them for anything
   about how the phone's own keyboard, fingers or key bar reach the app.
   `cargo xtask e2e pair` (gate `SLOPTY_PAIR_E2E`, serial) runs two app processes on one worker,
   each on its own socket and data dir, to prove many-clients-one-server: a terminal opened on
   one shows on the other, typing on both is serialised, attention badges both, a client dying
   leaves the other streaming and reattaches on relaunch, closing and notes propagate. The
   display scenario also needs `SLOPTY_SCREEN_E2E`. `cargo xtask e2e pair-ios [--sim iphone|ipad]`
   (gate `SLOPTY_PAIR_IOS_E2E`) puts the second client in the simulator: the Mac and the phone
   on one worker. `cargo xtask e2e workers` (gate `SLOPTY_WORKER2_E2E`,
   `SLOPTY_WORKER2=<ssh name>`, serial) is one client and two workers on two machines: ptyd +
   `slopty-worker` on a second Mac over ssh (temp root, private HOME, torn down after), to prove
   cross-worker attention — the pill sums both workers, a banner routes to the worker holding
   its session, and a hook is played only through `slopty hook` over ssh, never a real agent.
   `cargo xtask e2e server` (gate `SLOPTY_SERVER_E2E`, about 7 s) is the server with a real
   worker: `slopty-server`, then ptyd + `slopty-worker` registered with it through `--server`,
   on ports of their own under a temp root (`harness::ServerStack`). It drives them only
   through the `slopty` binary with `--json` and MCP over HTTP. It covers the directory with
   capabilities, a shell typed to, waited on and read back, files both ways (binary too), a
   listener found by `ports`, a close, and a shell that exits on its own. A killed worker must
   turn unreachable and come back online under the same id.
4. **Live desktop**, `cargo xtask e2e worker|screen|input|all` (gates `SLOPTY_SCREEN_E2E`,
   `SLOPTY_INPUT_E2E`): real capture and real event posting, own data dir under `target/e2e/`.
   The assertions live inside those tests.

## Beyond the layers: the deep checks
`cargo xtask deep <check>` (and the weekly `Deep` workflow) runs what no layer above can see
in the gate's budget: Miri over the pure crates (undefined behaviour under the interpreter),
ThreadSanitizer/AddressSanitizer builds of the daemons and the codec, `cargo hack
--each-feature` (every feature alone and together), `cargo llvm-cov` line coverage per crate,
and `cargo mutants` on one crate to find the lines no test would notice changing. A finding
there becomes a test in the layer that can hold it. `docs/DEV.md` has the commands.
