# Testing

## Four layers, fastest first
1. **Unit**, in every crate: pure logic behind traits with fakes (`slopty_input::Recorder`
   for the injector, `Layout`/`ItemDoc` in `slopty-client`). Runs under `cargo gate`.
2. **Headless GPUI**, `#[gpui::test]` in `slopty-ui`: a `VisualTestContext` window with real
   layout, `simulate_keystrokes`/`simulate_click`, a channel for the worker. Read the UI like
   a DOM: `cx.debug_bounds("item-<uuid>")` (set with `.debug_selector`), `window.painted_quads()`
   for colours and borders, view accessors (`rows()`, `active_item()`, a stream's `zoom()`),
   `slopty_ui::a11y::tree(window)` after `window.set_a11y_active(true)` for roles, labels and
   the focused node. No pixels, no process, runs under `cargo gate`. Every workspace/terminal
   behaviour gets a test here first.
3. **App self-test**, `cargo xtask e2e app` (`crates/slopty-e2e`):
   launches ptyd + the worker + the app (built with `--features slopty/e2e`) in a temp dir, pairs
   them, and drives the app over its own control socket (`SLOPTY_TEST_SOCKET`: keys, clicks,
   dump, render). `dump` is the structured state (tiles, focus, whether the overview is open,
   terminal rows, and `a11y`: the accessibility tree as role/label/value/focused/bounds in
   reading order, from the frame that painted the state in the same dump);
   `render` is GPUI drawing its own window to a PNG, compared numerically with
   `crates/slopty-e2e/golden` (`--accept` writes missing and failing goldens, `--accept-all` rewrites every golden, and `--review` writes none and fails on none, leaving each changed frame's render and diff in the artifacts, so a design review sees every golden in one run). `--filter <filterset>` (nextest's `-E`, such
   as `test(a_folder_tile_browses_the_worker)`) runs only the tests it picks, in any case, and
   `--accept`/`--review` then touch only the goldens those tests render. `--no-build` reruns
   what the last run built without calling cargo, so a rerun right after a build takes seconds
   instead of waiting on the other sessions' builds in `target/`; an edit since then is not in it.
   The temp dir
   is named for its test, not at random, because a golden draws the paths under it. Nothing touches another app.
   `cargo xtask e2e ios [--sim iphone|ipad]` is the same socket with
   the app in the simulator: the way to check anything on the phone or the tablet. There the
   socket also takes `ui_key_press` / `ui_touch` / `ui_pinch` / `ui_insert_text` /
   `ui_delete_backward` (`Driver::ui_key`, `ui_tap`, `ui_pan`, `ui_pinch`, `ui_insert_text`),
   delivered at the UIKit boundary by the fork (`tests/ios_uikit.rs`): use them for anything
   about how the phone's own keyboard, fingers or key bar reach the app.
   `cargo xtask e2e pair` (serial) runs two app processes on one worker,
   each on its own socket and data dir, to prove many-clients-one-server: a terminal opened on
   one shows on the other, typing on both is serialised, attention badges both, a client dying
   leaves the other streaming and reattaches on relaunch, closing and notes propagate. The
   display scenario also needs `--screen-recording`. `cargo xtask e2e pair-ios [--sim iphone|ipad]`
   puts the second client in the simulator: the Mac and the phone
   on one worker. `cargo xtask e2e workers` (serial)
   is one client and two workers on this Mac: a second ptyd + `slopty-worker` under a root of
   its own with a private HOME (`harness::SecondWorker`), which the app reaches only through a
   `slopty-shape` relay shaped like the tailnet path to another Mac (`harness::TAILNET`: 8 to
   12 ms round trip, 3 % loss). It proves cross-worker attention: the pill sums both workers, a
   banner routes to the worker holding its session, a hook is played only by spawning
   `slopty hook` with the payload on its stdin, never a real agent, and a killed worker shows
   down and reattaches on restart while the other keeps streaming.
   `cargo xtask e2e server` (about 7 s) is the server with a real
   worker: `slopty-server`, then ptyd + `slopty-worker` registered with it through `--server`,
   on ports of their own under a temp root (`harness::ServerStack`). It drives them only
   through the `slopty` binary with `--json` and MCP over HTTP. It covers the directory with
   capabilities, a shell typed to, waited on and read back, files both ways (binary too), a
   listener found by `ports`, a close, and a shell that exits on its own. A killed worker must
   turn unreachable and come back online under the same id.
   `cargo xtask e2e through-server` (serial, about 20 s) is
   the app as it is normally used (`harness::ServerFleet`). A `slopty-server` has two workers
   registered with it: one on loopback and one behind the tailnet-shaped relay, which the
   directory lists at the relay's address. The app starts at its first run, and the server's
   address is typed into the panel and entered. Both workers must then come from the directory
   with a shell each that answers a command, and a hook played on the far one through
   `slopty hook` must badge the pill. The far worker is killed and restarted twice, as soon as
   it shows down and again after the server has called it unreachable. It must show down within
   5 s and be connected within 2 s of each restart. Golden `through-server`.
   `cargo xtask linux e2e` (serial) is a terminal-only Linux worker.
   ptyd, the worker and the `slopty` relay are cross-built for `aarch64-unknown-linux-gnu`
   and run in a Debian container on Docker Desktop, as an account with bash for its shell,
   on the paths a Linux install takes. The worker's UDP port is published on this Mac's
   loopback, and its `[worker] allow` admits the bridge gateway its packets come from. From
   this Mac, `tests/linux.rs` connects with the client core's link and a `TermState`. The
   greeting must say Linux, aarch64 and Debian, with no capture, input, encoder or display.
   The login shell from passwd must echo a typed command and run it. A folder made there must
   list through `ListFolder`, and a file in it must read through `ReadFile`. A
   `UserPromptSubmit` hook played through the Linux `slopty hook` inside the container must
   reach the client as `Working`. It also times 300 keystrokes into `cat` to their echo and
   prints the percentiles. The container is capped at two CPUs and removed at the end.
4. **Live desktop**, `cargo xtask e2e worker|screen|input|all`: real capture and real event
   posting, own data dir under `target/e2e/`. The assertions live inside those tests.

The tests of layers 3 and 4 are live: each carries `#[ignore = "live: <the command that runs
it>"]`, so `cargo gate` lists it as skipped rather than passing it without running it, and
its xtask command runs it with nextest's `--run-ignored only`. A live test reads no variable to
decide whether to run. One that needs the Screen Recording grant sits in its target's
`screen_recording` module and runs only with `--screen-recording`. The app's frame-time
measurement sits in `frame_time` and runs under `smooth`, alone. The live tests of
`slopty-capture` and `slopty-input` still return early without `SLOPTY_SCREEN_E2E` or
`SLOPTY_INPUT_E2E`, which the xtask sets for them.

## Beyond the layers: the deep checks
`cargo xtask deep <check>` (and the weekly `Deep` workflow) runs what no layer above can see
in the gate's budget: Miri over the pure crates (undefined behaviour under the interpreter),
ThreadSanitizer/AddressSanitizer builds of the daemons and the codec, `cargo hack
--each-feature` (every feature alone and together), `cargo llvm-cov` line coverage per crate,
and `cargo mutants` on one crate to find the lines no test would notice changing. A finding
there becomes a test in the layer that can hold it. `docs/DEV.md` has the commands.

## Budgets: allocations, instructions, footprint
Wall time is not a pass or fail on a machine other sessions load, so the budgets are counts:
- **Allocations**, in the gate. `tests/allocs.rs` in `slopty-media`, `slopty-engine` and
  `slopty-grid` install `slopty_testkit::alloc::Counting` as the test binary's allocator and
  count, per thread, what a hot path allocates in steady state. Covered: cutting a frame into
  datagrams and putting it back together, a keystroke's echo through the engine and its diff,
  an Enter at a bottom prompt, the echo frame's encoding, a row applied on the client, and one
  frame or echo handed to eight viewers against one. Each asserts blocks (and bytes where they
  say something) against a number in the test. Fan-out asserts equality: eight viewers cost
  exactly what one does.
- **Retired instructions**, in `cargo xtask bench`. The `*_cost` measurements report each
  series' instructions per operation (`proc_pid_rusage`, `ri_instructions`), which
  `xtask/budgets.toml` holds within 5 %. Their wall times are kept only by the nightly run, as a
  trend.
- **Footprint, descriptors, threads and leaks**, in `cargo xtask soak`: the real daemons under
  a scripted load, sampled for `ri_phys_footprint`, open descriptors and threads, with
  `leaks <pid>` at the end.

`cargo xtask nightly` runs the soak, the bench's wall times, the property tests at
`PROPTEST_CASES` cases, `slopty-ui`'s tests under `ITERATIONS` scheduler seeds and the deep
checks, each with a JSON summary under `target/nightly/<date>/`. `docs/DEV.md` has the commands.
