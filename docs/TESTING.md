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
   **Retained frames.** GPUI draws only the views that heard of a change, so a view left out
   keeps showing the old state while every assertion on state passes. `retained::stale` is the
   oracle: it takes what the window last painted (every quad and sprite with its bounds, clip
   and colour, as sorted lines), draws the same state again with every view built from scratch,
   and names the lines only one side holds. When two scratch draws differ (something moves),
   the lines both agree on must still be in the frame shown. `workspace/tests/retained.rs` runs it after each
   step of an echo, a command starting (in a shell on the strip and in one scrolled off it), a
   spring, a trackpad scroll, a hover, a window resize and the overview around a stream, and a
   page tile whose page fails as it is drawn, with the workspace's clock held so a frame of motion and its scratch twin fall on
   the same instant; its own test proves it catches a view changed without a notify and that
   scratch after scratch agrees. The oracle compares quads and sprites, so what they cannot
   show is asserted beside it: a stream's picture laid out at the bounds its view was drawn in,
   a view drawn again after a change (`renders()`), a shell not built again when only its strip
   was. In the app self-test every `dump` carries the same check
   (`Dump::stale`), and `render` answers with an error, the scratch frame saved beside the
   render, when the frame the app drew is stale: the goldens are the frames the app draws
   through its notifies, never a forced full redraw. The check runs once the frame's
   next-frame callbacks have run, so an animation's own notify is in the frame it judges. A
   notify raised while a frame is drawn (in a paint, or in a focus listener) wakes nothing,
   and the check sees what it left out. A
   live app cannot hold the clock, so what moves on it paints differently between the frame
   and its scratch twin a few milliseconds later: the self-test runs under GPUI's Reduce Motion
   (the frame-time runs set `SLOPTY_E2E_MOTION=1` to keep it), and a frame whose two scratch
   draws disagree is in motion and not judged.
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
   Remote windows and displays stream for real here too. With `SLOPTY_SYNTHETIC_SCREEN=1` the
   worker serves its drawn screen (`slopty_worker::screen::synthetic::Synthetic`) in place of
   the Mac's: one display and two windows, the same on every Mac, whose pictures are drawn
   (`slopty_capture::synthetic::Canvas`: a still desktop, a strip of blocks, and a page of glyphs
   scrolling in the middle half) and then encoded by VideoToolbox, sent and decoded as any other
   worker's. Nothing is captured, so no grant is asked for. `tests/app/stream.rs` holds each
   tile to its numbers (frames, arrival → present, loss, NACKs, the worker's own counters). Its
   goldens mask the scrolling page and hold it to the golden's mix of luma instead
   (`snapshot::luma_distance`), so the chrome, the overlay and the still parts of the picture are
   compared pixel for pixel while the page moves. Its `frame_time` case, under
   `cargo xtask e2e smooth`, times drawn frames from their capture stamp to the paint that
   shows them.
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

**Fuzzing.** Every decoder a peer's bytes reach has a libFuzzer target in `fuzz/`, a crate
outside the workspace (nightly, AddressSanitizer, debug assertions). The control stream both
ways, the server's links and the unidirectional streams go through the real framing
(`codec::try_take`, as `FramedRecv` reads it) in pieces of a size the fuzzer picks. The
datagrams are covered too: the client's datagram, the terminal copy with its frame head, and the
media header, the cursor and the pasteboard origin. The reassembler gets real packetized
fragments and Reed–Solomon parity, reordered, lost, repeated or damaged, with NACKs answered from
the packetizer. The worker is fuzzed on NACKs and receiver reports. The worker's control socket
gets its JSON line. A target panics on a crash and on a broken invariant: a decoded message must
encode back to the same bytes, a frame head must agree with its frame, and an undamaged run must
deliver every frame byte for byte. The decoding targets also count the heap a decode takes
(`slopty_testkit::alloc::Counting`) and fail past 64 bytes per input byte, 48 per budgeted
cell and 2 MiB, so a small message that asks for a large allocation is a crash too
(docs/decisions/terminal.md, "Decoded lines are bounded"). The fuzz targets and the replay run on `cargo xtask fuzz` and
`deep fuzz`, never in the gate. A crash is minimised into `fuzz/regressions/<target>/`, and
`fuzz/tests/regressions.rs` replays every such input on an ordinary build
(`cargo xtask fuzz --replay`). The fix for a crash still gets its unit test in the crate it
fixes.

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
checks (the fuzz smoke among them), each with a JSON summary under `target/nightly/<date>/`.
`docs/DEV.md` has the commands.
