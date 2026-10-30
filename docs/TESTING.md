# Testing

Tests make no sound on this Mac: the drawn screen's worker sounds only under `SLOPTY_SYNTHETIC_SOUND=1`, and a test that plays sound keeps its player muted or off the output device.

## Four layers, fastest first
1. **Unit**, in every crate: pure logic behind traits with fakes (`slopty_input::Recorder`
   for the injector, `Layout`/`ItemDoc` in `slopty-client`). Runs under `cargo gate`.
2. **Headless GPUI**, `#[gpui::test]` in `slopty-ui` and `slopty-shell`: a `VisualTestContext`
   window with real layout, `simulate_keystrokes`/`simulate_click`, a channel for the worker.
   The view model (`slopty_ui::workspace::view_model::tests`) is driven through the shell's own
   calls and read back as the shell reads it; the shell's tests read its views like a DOM
   (`cx.debug_bounds`, `window.painted_quads()`, `slopty_kit::a11y::tree(window)` after
   `window.set_a11y_active(true)`). No pixels, no process, runs under `cargo gate`.
   **Retained frames.** GPUI draws only the views that heard of a change, so a view left out
   keeps showing the old state while every assertion on state passes. `slopty_kit::retained::stale`
   is the oracle: it takes what the window last painted (every quad and sprite with its bounds,
   clip and colour, as sorted lines), draws the same state again with every view built from
   scratch, and names the lines only one side holds. When two scratch draws differ (something
   moves), the lines both agree on must still be in the frame shown. In the app self-test every
   `dump` carries the same check (`Dump::stale`), and `render` answers with an error, the
   scratch frame saved beside the render, when the frame the app drew is stale: the goldens are
   the frames the app draws through its notifies, never a forced full redraw. The check runs
   once the frame's next-frame callbacks have run, so an animation's own notify is in the frame
   it judges, and only after a frame that painted nothing new (at most eight): a value measured
   while drawing reaches the views that read it a frame later (ARCHITECTURE.md, "Drawing under
   retention"), as when a narrower window moves the workspace's width inset and the views laid
   out against it then move with it. What changed after that frame and is not drawn yet (an agent's
   toast pushed while the dump waited) is drawn first, as the next frame would draw it, so the
   frame judged never predates the state the scratch draws see. The scratch draws show the
   moment the frame shown showed
   (`Window::repeat_frame_time`): every GPUI animation of a frame and the shell's own tweens read
   the frame's one moment (`App::frame_time`), so a fade a hair short of its end in the frame
   shown is there in the frames from scratch too, though they are drawn after its end. What the
   shared pulse drives (loaders, the title shimmer, the stream's dissolve, rolling labels) is
   notified at 30 Hz and drawn from its last paint between ticks, so it shows the moment of the
   tick it last drew on (`motion::pulse_moment`), in a frame from scratch too. The shell reads
   no other clock: `crate::clock` is its one door, and clippy refuses a wall-clock read anywhere
   else in it (docs/decisions/testing.md, "Every clock in the shell is the frame's"). What
   still reads the wall clock moves between draws: the self-test holds GPUI's Reduce Motion
   (`motion::hold`), and a frame whose two scratch draws disagree is judged only on what holds
   still, at whatever depth what moves leaves it. The headless tests name such a read: two
   frames from scratch at one moment, the wall clock moved on between them, must agree
   (`retention_tests::wobble`).
   **Fuzzed whole.** `shell::fuzz_tests` takes the whole shell through seeded sequences of what
   a person and the engine do, on a Mac's window, a phone's at 3 × and an iPad's, and checks
   every step against both oracles, the model's selection and the accessibility tree. The suite
   runs four seeds of eight steps, about a second here, and names a failure at once with the
   run that shrinks it; the ignored long run shrinks a failure to the fewest steps and prints
   the variables that rerun it.
   **Every frame, as it is drawn.** The oracle judges the frames a test or a dump stops at, so a
   view that is stale for a few frames between them goes unseen. GPUI's retained check
   (`Window::set_check_retained`, gpui-fast's `fast::check_retained`) judges every frame, as
   Chromium's under-invalidation check (`checkUnderInvalidation`) does. Each frame that drew
   something again from the last one (a view, a stretch under a key, a scroll layer's tiles or a
   list layer's rows) is drawn a second time right after it, from scratch at the same moment,
   before any effect or event. Both frames see the same state. A difference inside what was drawn
   again is reported with the view's type, its entity, its element id path (or the layer's
   container), and the primitives each frame painted there. The frame from scratch is drawn in a
   shadow and leaves no trace: the window shows the frame it drew, so a checked app paints what an
   unchecked one paints, frame for frame, and the goldens hold with the check on. A frame can find
   too late what it drew again: one that asked for the next frame (a hover found too late, the
   focus moving while it drew), or a place that read what the frame's own drawing wrote after it
   (a row widening a column the rows before it were drawn at). What differs there waits for the
   next frame, and is reported only if that frame draws the place again and it still differs.
   What a frame built is judged when its window comes to rest: a view notified while the frame
   drew asks for no next frame, so a window whose last frame notified something while drawing,
   and asks for no frame, is compared then with a frame from scratch in the views notified, the
   views inside them and the views that read them; the report names what was notified and
   suggests `cx.defer`. Other builds are still the oracle's, on settled frames. The headless retention tests
   (`shell::retention_tests`) panic on a finding. `retained::check_every_frame` turns the check
   on for a window, and `retained::stale` reports what it kept ahead of its own comparison. The
   self-test runs with it on (`GPUI_CHECK_RETAINED=record` for the Mac app and the simulator's):
   a finding fails the next `dump` or `render`, named by its view. Proof:
   `fast::tests::check_retained` in gpui-fast catches an un-notified view and a list layer
   composited over an untold change, passes a notified view and a view beside an animation, and
   `a_checked_window_draws_every_frame_as_an_unchecked_one` holds a checked window to an
   unchecked one through the oracle's random histories, finding nothing.
   `retained::tests::a_view_changed_untold_is_named_by_the_frame_check` covers the same in Slopty.
   **With the tree kept on.** The oracle draws its frames from scratch into the window, and a
   dump turns the accessibility tree on and off, each a full build: every view is built again
   before the next input, so a view that acts on what it was last built with (a listener, a key
   context, a hitbox) never misbehaves under a dump. A Mac with an assistive app running (a
   launcher, a window manager, a screen reader) asks for the tree from the first frame and
   keeps it, and GPUI then keeps view retention but not element retention.
   `SLOPTY_E2E_A11Y=always` on `cargo xtask remote … --e2e app` runs the app so: the tree is on
   from the first frame, and a dump or render reads it and the per-frame check's findings
   without drawing anything from scratch. `walk::*` walks a person's inputs one after another
   in either mode.
3. **App self-test**, `cargo xtask e2e app` (`crates/slopty-e2e`):
   launches a server, ptyd, the worker registered with the server, and the app (built with
   `--features slopty/e2e`) following the server, in a temp dir, so the app reaches the worker
   through the directory as every app does (`harness::Stack`), and drives the app over its own
   control socket (`SLOPTY_TEST_SOCKET`: keys, clicks, dump, render). `dump` is the structured
   state (the workspace's items, each with its kind, worker and session, the live terminals
   and screens with their rows and sizes, which one holds the keyboard, and `a11y`: the
   accessibility tree as role/label/value/focused/bounds in reading order, from the frame that
   painted the state in the same dump); `render` is GPUI drawing its own window to a PNG,
   composited over one slate backdrop, compared numerically with `crates/slopty-e2e/golden`,
   and what the same frame says in words (its accessibility tree's roles, labels and values, a
   line a node) compared exactly with the `.txt` beside it, so a changed word fails with the
   lines that changed where the pixel tolerance cannot see it (`--accept` writes every golden
   its frame differs from by one pixel or one word, within the tolerance too, so no golden
   keeps drawing a word or a shade the app has since changed; `--accept-all` rewrites every
   golden, and `--review` writes none and fails on none, leaving each changed frame's render
   and diff in the artifacts, so a design review
   sees every golden in one run; it also draws every render again at 2x beside the 1x ones, as
   `target/e2e/artifacts/<name>@2x.png`, the set shown to the person, since a 1x render is not
   what a Retina Mac shows). `--filter <filterset>` (nextest's `-E`, such as
   `test(the_terminal_dock_holds_a_shell_of_the_worker)`) runs only the tests it picks, and
   `--accept`/`--review` then touch only the goldens those tests render. `--no-build` reruns
   what the last run built without calling cargo; an edit since then is not in it. The temp dir
   is named for its test, not at random, because a golden draws the paths under it. Nothing
   touches another app.
   Remote windows and displays stream for real here too. With `SLOPTY_SYNTHETIC_SCREEN=1` the
   worker serves its drawn screen (`slopty_worker::screen::synthetic::Synthetic`) in place of
   the Mac's: one display and two windows, the same on every Mac, whose pictures are drawn
   (`slopty_capture::synthetic::Canvas`) and then encoded by VideoToolbox, sent and decoded as
   any other worker's. Nothing is captured, so no grant is asked for. Every app the harness
   starts takes GPUI's presented handler as the glass when the display reports no scan-out
   (`GPUI_PRESENTED_AT_CALLBACK=1`): this Mac is reached over Parsec, whose virtual display
   gives every frame a zero presentation time.
   Two cases in the `app` suite stand for how the app is used beyond one window. `app::pair`
   runs two app processes on one worker, each on its own socket and data dir
   (`Stack::launch_pair`): the dock's shell of the same place is one shell on both, what either
   types runs there once and in order, and one client dying leaves the other's shell live.
   `app::through_server` is the app at its first run (`harness::ServerFleet`): a server with two
   workers registered, one on loopback and one behind a relay shaped like the tailnet
   (`harness::TAILNET`: 8 to 12 ms round trip, 3 % loss), which the directory lists at the
   relay's address. The server's address is typed into Settings → Devices' Add device, and both
   workers must come from the directory, connected. The far worker is killed and restarted
   twice, as soon as it shows down and again after the server has called it unreachable; it
   must show down within 5 s and be connected within 2 s of each restart.
   `cargo xtask e2e ios [--sim iphone|ipad]` is the same socket with the app in the simulator.
   `cargo xtask e2e server` (about 7 s) is the server with a real
   worker: `slopty-server`, then ptyd + `slopty-worker` registered with it through `--server`,
   on ports of their own under a temp root (`harness::ServerStack`). It drives them only
   through the `slopty` binary with `--json` and MCP over HTTP. It covers the directory with
   capabilities, a shell typed to, waited on and read back, files both ways (binary too), a
   listener found by `ports`, a close, and a shell that exits on its own. A killed worker must
   turn unreachable and come back online under the same id.
   `cargo xtask linux e2e` (serial) is a terminal-only Linux worker.
   ptyd, the worker and the `slopty` CLI are cross-built for `aarch64-unknown-linux-gnu`
   and run in a Debian container on Docker Desktop, as an account with bash for its shell,
   on the paths a Linux install takes. The worker's UDP port is published on this Mac's
   loopback, and its `[network] allow` admits the bridge gateway its packets come from. From
   this Mac, `tests/linux.rs` connects with the client core's link and a `TermState`. The
   greeting must say Linux, aarch64 and Debian, with no capture, input, encoder or display.
   The login shell from passwd must echo a typed command and run it. A folder made there must
   list through `ListFolder`, and a file in it must read through `ReadFile`. It also times 300 keystrokes into `cat` to their echo and
   prints the percentiles. The container is capped at two CPUs and removed at the end.
4. **Live desktop**, `cargo xtask e2e worker|screen|input|all`: real capture and real event
   posting, own data dir under `target/e2e/`. The assertions live inside those tests.

The tests of layers 3 and 4 are live: each carries `#[ignore = "live: <the command that runs
it>"]`, so `cargo gate` lists it as skipped rather than passing it without running it, and
its xtask command runs it with nextest's `--run-ignored only`. A live test reads no variable to
decide whether to run. One that needs the Screen Recording grant sits in its target's
`screen_recording` module and runs only with `--screen-recording`. The live tests of
`slopty-capture` and `slopty-input` still return early without `SLOPTY_SCREEN_E2E` or
`SLOPTY_INPUT_E2E`, which the xtask sets for them.

## Agents: recordings and stand-ins
No test runs a signed-in agent. Every agent runs as its own TUI in a terminal
(`docs/decisions/agents.md`, "TUI-first"), so the stand-in is a TUI too: Claude Code is played by
`slopty-stub-claude` (`slopty_testkit::claude_tui`) in a real terminal of a real ptyd and worker.
It keeps the records Claude Code keeps (its registry entry and its transcript, a new session on
`/clear`, and the waiting state in its registry record), runs the hooks it is given and waits
for each, and records what it was typed, every read with its time, and each hook it ran
(`STUB_RECORD`). Its prompts script it: `ask <question>` waits on the person,
`hold` keeps a turn at work until Esc, and `STUB_TURNS` names the calls of Slopty's tools a
prompt makes through the `slopty mcp` it is handed. `apps/slopty-worker/tests/threads.rs` proves
the TUI-first worker one behaviour per test against it, and `crates/slopty-client/tests/threads.rs`
the client's table and follow over the same stand-in. The conversation fixtures
(`crates/slopty-agent/tests/fixtures/conversation/`) are recorded from a real session and hold
the transcript decoder. The app's e2e reads them too: `tests/app/planted.rs` lays recordings,
and turns a test writes, end to end as one past session in the worker's Claude Code store, an
hour old whatever day it runs, and takes it up from History, so the stand-in `--resume`s it in a
terminal while the inspector beside it reads what Claude Code wrote. A fixture no recording can reach,
such as an agent asking to be signed in, is written to the schema by hand, and its test says so.

## Where the gate runs
Layers 1 and 2 and every lint make up `cargo gate`, split between this Mac and
GitHub Actions. Before a commit, `cargo gate` runs the cheap lanes on the staged tree: fmt,
taplo, deny, hakari, shear, typos, `committed` (on the history, and on the pending message given
with `-m`) and host clippy. It takes about a minute with a warm build. `cargo xtask land` then
pushes the commit to the `gate` branch. CI runs every lane there, the tests and clippy for iOS
and Linux and rustdoc among them, and fast-forwards main to the commit only when all of them
pass. So main holds only commits the full gate passed. A red run's summary names the failed lane
and each failed test, read from nextest's JUnit report. The fix lands as a new commit on top,
and its run supersedes the red one. The tests a runner's virtual Mac cannot run (missing
hardware) are listed in nextest's `ci` profile and run in every local `cargo gate --full`.
A test that fails and then passes on a retry fails its run (`flaky-result = "fail"`): a pass on
a retry is a flake, which a green must not hide. The one exception is the few tests named in
the `gate` and `warm` profiles, which time themselves against the machine's load and are
retried twice by their exact names, never by a pattern, until virtual time replaces their
clocks (a test in `xtask/src/deep.rs` holds the config to that). `docs/DEV.md` ("Gate") has
the flags and the workflow.

### VideoToolbox
A runner's virtual Mac shares its host's media engine, and at times its encoder stops ("No real
codec", `docs/decisions/video.md`). The tests that code then hang in the driver, past any kill.
So CI runs the `videotoolbox` test group on its own after the rest, under a 5-minute deadline,
and asks the encoder first and after a failure with one real frame
(`encoder::tests::the_encoder_codes_one_frame`, 30 s). An encoder that does not answer before
the run skips it with a warning. A failed run on a runner whose encoder stopped answering, or
logged that it stopped, passes with a warning. A failure while the encoder still codes fails the
lane, since the hang is then ours. So a change to the coding path (`slopty-codec`,
`slopty-capture`, the worker's `screen`) runs the group on this Mac's real encoder before it
lands:
`cargo nextest run -p slopty-codec -p slopty-worker -E 'group(videotoolbox)'`.

## Live lane in a VM
A test that moves the real pointer, posts HID events, locks the screen, reaches the login window
or needs a TCC grant runs in a macOS guest, never on this Mac (someone works on it over Parsec)
and never on the other one. `cargo xtask vm live -p <crate> -- <nextest args>` clones a clean
guest from the base, deploys this tree's worker into it with `slopty worker deploy`, and runs the
crates' tests there from a nextest archive. They run in the guest's logged-in session, where
Accessibility, Screen Recording and `PostEvent` are granted, one at a time since there is one
pointer, with `SLOPTY_INPUT_E2E`, `SLOPTY_SCREEN_E2E`, `SLOPTY_DND_E2E` and `SLOPTY_VM` set. The
reports and `target/e2e` come back under `target/vm/<guest>/`. Such a test is written as any
live test is: `#[ignore = "live: cargo xtask vm live …"]`, or gated by its variable. It posts
only in the guest, and its pass or fail comes from what its own processes report.

**A skip is a failure in the guest.** A live test that finds something missing (its variable
unset, a grant, a display, a shell) calls `slopty_testkit::live::skip("<why>")` and returns.
Anywhere else that prints `skipped: <why>` and the test passes unrun; under `SLOPTY_VM` the call
fails the test instead, since a guest lacks nothing a live test needs. No test prints its own
skip line, so the lane reads no output to decide. A host test reaches the guest's worker over the
VM network at `SLOPTY_VM_WORKER` (`crates/slopty-e2e/tests/vm.rs`). `cargo xtask vm e2e` is the
lane's own proof: that host test, then a real HID pointer move posted in the guest and read
back there. Why a guest and why tart: `docs/decisions/testing.md` ▸ **Live tests that drive the
desktop run in a macOS guest under tart**. Commands: `docs/DEV.md` ▸ "Live lane in a VM".

## Beyond the layers: the deep checks
`cargo xtask deep <check>` runs what no layer above can see in the gate's budget: Miri over the
pure crates (undefined behaviour under the interpreter), ThreadSanitizer/AddressSanitizer builds
of every crate with a share of the workspace's `unsafe` (the daemons, the codec, capture,
platform, the virtual display, input, drag and drop, the crash reporter, the testkit and the
tailnet reader), `cargo hack --each-feature` (every feature alone and together), `cargo
llvm-cov` line coverage per crate, and `cargo mutants` on the code a change touches, to find the
lines no test would notice changing (below, "Mutation in the diff"). A finding there becomes a
test in the layer that can hold it. `docs/DEV.md` has the commands.

They run on GitHub Actions, not on this Mac, where someone works over Parsec while other
sessions gate (`docs/decisions/testing.md`, "The deep checks run on GitHub Actions"). The `Deep`
workflow runs every night, each check a job of its own: on macOS the sanitizers, loom, `leaks`,
the fuzz targets, the property tests at 4 096 cases, the GPUI scheduler seeds, the features and
coverage; on Ubuntu, Miri. Mutation testing of the core crates runs weekly on Ubuntu, the wire
crate split four ways. Each job uploads its report (the log, the tests' JUnit, `leaks`' reports,
the fuzzers' crashes), and a failed night opens or adds to the "Deep checks failing" issue,
which the next clean night closes. Two checks stay here: `deep metal` needs a GPU a hosted
runner lacks, and the soak and bench measure this Mac. Here a check runs one at a time at
`nice -n 19` on four cores, and `cargo xtask nightly` refuses a full run without `--all-here`.

Four deep checks see what the rest cannot:
- **Sanitizers and signals.** A sanitizer's runtime installs its own fatal-signal handlers, and
  `slopty-crash` chains to the handler it finds, so its tests that die of a signal run apart with
  the runtime's handlers off (`deep.rs`, `OWN_SIGNALS`). `ThreadSanitizer` runs a signal's
  handler at once only on a thread inside a call it treats as blocking, and otherwise holds it
  until that thread next calls a function it intercepts. The kernel hands a child's `SIGCHLD` to
  the test harness's main thread, which waits for the tests in a call the runtime does not
  intercept and wakes only for its 60-second notice. So every test that waited on a child
  through `SIGCHLD` (`tokio::process`, a PTY's child) waited a minute, and the session actor's and
  the review's tests failed or timed out for that alone. Every binary `sanitize thread` builds
  links a pump (`xtask/src/deep/fixtures/sigchld.rs`): before `main` it blocks `SIGCHLD` on the
  main thread, which every later thread inherits, and starts one thread that waits for it in
  `sigsuspend`, whose interceptor runs the handler at once. A child waits 5 ms for it, not 60 s.
- **Loom models** (`deep loom`). A lock-free structure whose correctness rests on its orderings
  gets a model beside it, behind `cfg(slopty_loom)`, which swaps its atomics for loom's. Loom
  then explores every interleaving, with loads free to read stale stores. A model is proven by
  planting the bug it guards against (an ordering weakened, a check dropped) and watching it
  fail. The audio ring is the first (`docs/decisions/tooling.md`, "The audio ring is
  model-checked with loom").
- **Metal validation** (`deep metal`). GPUI's headless tests draw on no GPU, so the app
  self-test is the one layer where the renderer runs. This check runs it with Metal's API and
  shader validation on. An API error asserts, and a shader fault aborts the app, which fails the
  test that drew it. A warning (a redundant or unused binding) is logged in the app's output.
- **Leaks in the tests' own paths** (`deep leaks`). The daemons' and the wire's test binaries run
  one test at a time under `leaks --atExit`. It reads the heap once they are done, and a binary
  fails on memory left unreachable. This is macOS's own tool with no instrumented build: Apple's
  sanitizer runtime has no `LeakSanitizer` here. A binary still running after ten minutes is
  killed and fails.
  A test that drives a shell on a PTY cannot run under it. `leaks` turns on `MallocStackLogging`
  in the process it launches, and libmalloc's notice then reaches the terminal the test reads.
  Such targets are left out by name (`deep.rs`, `LEAKS_CANNOT_RUN`), and so is `slopty-pty`, whose
  terminal tests hang under it. The soak reads ptyd's heap live with `leaks <pid>`.

**Fuzzing.** Every decoder a peer's bytes or an agent's records reach has a target in `fuzz/`,
a crate outside the workspace, each built for libFuzzer (cargo-fuzz: nightly, AddressSanitizer,
debug assertions) and for AFL++ (ziggy over cargo-afl) from one entry point
(`slopty_fuzz::target!`). The control stream both ways, the server's links and the
unidirectional streams go through the real framing (`codec::try_take`, as `FramedRecv` reads
it) in pieces of a size the fuzzer picks. The datagrams are covered too: the client's datagram,
the terminal copy with its frame head, and the media header, the cursor and the pasteboard
origin. The terminal target writes a program's output through the worker's engine, libghostty's
VT parser built `ReleaseSafe` so a parser fault traps. Every viewer applying the frames it was
sent must show what a second engine fed the same bytes shows, and a scrollback page holds no
more lines than asked. A checkpoint replayed into a fresh engine must show the same lines, each
with its cells as they draw, its prompt mark, its links and its soft wrap, and the same cursor.
Two metamorphic relations hold it where no oracle says what a run should show: the second
engine is fed the bytes one at a time, so every sequence is split at every point a PTY read may
split it, and must show the same screen, cursor and modes and report the same session state
after each write; and on the primary screen a resize keeps the logical lines (soft wraps
joined) and the cursor's place in them. Its seeds are captures of real programs' output
(`fuzz/seeds/terminal`) and its dictionary the VT tokens (`fuzz/dicts/terminal.dict`). The
reassembler gets real packetized fragments and Reed–Solomon parity, reordered, lost, repeated
or damaged, with NACKs answered from the packetizer. The worker is fuzzed on NACKs and receiver
reports, and its control socket on its JSON line. Claude Code's transcript goes through the
conversation decoder in reads of every length, over the main file and its subagents' (seeded
from `crates/slopty-agent/tests/fixtures`), its hook payloads and its record of itself
(`sessions/<pid>.json`) through theirs, an answer's markdown through the streaming parser a
delta at a time, which must give the one-shot parse after each, and a visual's Luau script,
images, models and shaders through the sandbox, whose memory cap and time budget must hold. A
target panics on a crash and on a broken invariant: a decoded message must encode back to the
same bytes, a frame head must agree with its frame, an undamaged run must deliver every frame
byte for byte, and a field no Claude Code writes changes nothing it decodes. The decoding
targets also count the heap a decode takes (`slopty_testkit::alloc::Counting`) and fail past 64
bytes per input byte, 48 per budgeted cell and 2 MiB, so a small message that asks for a large
allocation is a crash too (docs/decisions/terminal.md, "Decoded lines are bounded").
What the fuzzers reached stays held in the gate (docs/decisions/testing.md, "Fuzz corpora are
regression tests"). Each target's corpus, the fewest bytes (at most 2 MiB) that reach every edge
the grown one reaches (`cargo xtask fuzz --cmin`), is committed under `fuzz/corpus/<target>`,
and a crash, minimised, under `fuzz/regressions/<target>/`;
`fuzz/tests/regressions.rs` replays both through each target on a plain, optimised build, the
inputs shared over every core (`cargo xtask fuzz --replay`). Fuzzing itself runs on `cargo xtask fuzz`
and nightly on Deep's arm runner (`deep fuzz --ziggy`): libFuzzer, then AFL++ from the
committed corpus, each for a minute a target on four cores, with what AFL++ queued added to the
corpus libFuzzer grows from night to night. The fix for a crash still gets its unit test in the
crate it fixes.

## Mutation in the diff
Line coverage says a test ran a line; a mutant says whether any test would notice it changing.
`cargo xtask deep mutants` runs `cargo mutants` (configured by `.cargo/mutants.toml`, nextest's
`mutants` profile) on the code a diff touches, by tier:
- **Tier A, the pure kernels** (core, grid, predict, proto, media, math, settings, theme,
  markdown, syntax, the view crates, tools, and the pure modules of net, client, pty and the
  agent's conversation): every mutant of a change must be caught, or the change ships code no
  test reads. A missed one fails the run.
- **Tier B, the daemons and their links** (worker, workerd, server, serverd, the client's link,
  cli, ptyd, deploy, tailnet): missed mutants are listed as work and fail nothing, until the
  tier has its baseline from a run under the simulated world.

Each missed mutant is a line of `target/deep/mutants/lane-work.md` (and of the job's summary), a
task a lane takes: write the test that fails on that change, or, for a change no test could tell
apart (an equivalent mutant), add it to `.cargo/mutants.toml` or mark the function
`#[mutants::skip]` with the reason. `Debug` and `Display` bodies are never mutated.

It runs beside the gate and not in its way, on each push the gate takes
(`.github/workflows/mutants.yml`): tier A's mutants in the diff from main, the unmutated tree
untested (the gate tests the same commit), at most 30 of them. A change that makes more runs an
even sample, every k-th.

```sh
cargo xtask deep mutants --in-diff origin/main            # tier A in this branch's change
cargo xtask deep mutants --since "24 hours ago" --tier a --tier b
cargo xtask deep mutants -p slopty-grid --shard 0/4       # a crate, a quarter of it
```

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
  `leaks <pid>` at the end. Beside the terminals, a stream lane opens, decodes and closes the
  worker's drawn screen (`SLOPTY_SYNTHETIC_SCREEN`) one stream after another. The streams take
  the display and both windows in turn, at three capture scales, so every open builds the
  capture, the encoder and the decoder at another size. The soak fails on a stream that did not
  open or decoded nothing, on a decode error, and on frames lost or dropped past their budgets.

`cargo xtask nightly` runs the soak, the bench's wall times, the property tests at
`PROPTEST_CASES` cases, `slopty-ui`'s tests under `ITERATIONS` scheduler seeds and the deep
checks (the fuzz smoke among them), each with a JSON summary under `target/nightly/<date>/`.
`docs/DEV.md` has the commands.
