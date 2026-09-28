# Decisions — Testing

See `docs/DECISIONS.md` for the legend. Newest entries go at the end.

- ✅ **Four layers, each answering one question, fastest first** (2026-09-05). Unit tests for
  logic behind traits with fakes (`slopty_input::Recorder`); headless GPUI tests in
  `slopty-ui` (`#[gpui::test]`, `VisualTestContext`) for layout, focus, key bindings and what
  the scene paints; the app self-test (`crates/slopty-e2e`, `cargo xtask e2e app`) for the
  wiring of daemons + app + network + real PTY; the gated live desktop tests for capture
  and event posting. A behaviour is pinned at the lowest layer that can see it; the layers
  above only prove the seams. `CLAUDE.md` ▸ Tests is the operating guide.

- ✅ **The app tests itself over a control socket instead of being driven from outside**
  (2026-09-05). With `SLOPTY_TEST_SOCKET` set (feature `slopty/e2e`) the app serves JSON lines:
  `pair`, `keys`, `type`, `click`, `scroll`, `resize`, `dump`, `render`, `quit`. Input goes
  through `Window::dispatch_keystroke`/`dispatch_event`, so bindings, focus and listeners run
  exactly as for a user, and every command settles on the next frame before it answers.
  `dump` is the app's own model of its window (items with window bounds, focus owner, zoom,
  terminal rows); `render` is `Window::render_to_image` (fork, `test-support`), the app
  painting its own window, diffed numerically against a golden with a 24-value channel slack
  and a 1 % pixel tolerance. No external process ever posts events into the app or captures
  it, and nobody has to look at the images: the numbers and the `.diff.png` path are the
  verdict. Two real bugs surfaced on the first run (below). `dump` reads its state in one
  next-frame callback and the accessibility tree in the following one (2026-09-15): GPUI
  runs those callbacks before the frame's draw, so a dump read at once paired fresh state
  with the previous frame's tree, and the driven-agent test twice found an Allow row in the
  state but no Allow button in the tree. The draw after the first callback paints exactly
  the state it read, and the second callback sees that draw's tree.

- ✅ **The iOS app is tested through the same socket, in the simulator** (2026-09-05).
  `cargo xtask e2e ios [--sim iphone|ipad]` builds the app with the `e2e` feature, boots the
  simulator (created on first use), installs the bundle and runs `crates/slopty-e2e/tests/ios.rs`
  (gate `SLOPTY_IOS_E2E`): ptyd and the worker start on the Mac as for `e2e app`, the app is
  launched with `simctl launch` and `SIMCTL_CHILD_*` variables for its socket, data dir,
  `SLOPTY_PREDICT` and `SLOPTY_HARDWARE_KEYBOARD`, and binds the socket on the shared file system (a simulator process is a
  Mac process; the sandbox does not stop it). `Stack::launch_on_simulator` shares the daemon
  code with `launch`; shutdown sends `quit` and `simctl terminate`. The test pins the one
  behaviour that differs by screen: the worker places a desktop-sized terminal and the client
  fits it — a phone shrinks it to the viewport, an iPad keeps 720 pt — then types, reads the
  echo, ⌘N/⌘W. UIKit's own delivery (touches, presses) is still outside the test: the socket
  dispatches keystrokes at GPUI's level, so a fork bug in `pressesBegan` would not show here.

- ✅ **The simulator renders goldens too: `render_to_image` on iOS draws offscreen**
  (2026-09-05, fork commit `9463bf6`). `gpui_ios` had no `render_to_image`, so the simulator
  tests checked only dumps. The fork gives `gpui_ios` a `test-support` feature (wired into
  `gpui_platform`'s, which `slopty-app`'s `e2e` feature already turns on, so nothing changed
  in the app) whose `render_to_image` calls `gpui_apple`'s existing
  `render_scene_to_image` with the layer's `drawableSize`: the current scene drawn through
  the same Metal pipeline into a private BGRA8 texture, read back as `RgbaImage`. Offscreen
  rather than the macOS path's `nextDrawable`: the layer's three drawables belong to the
  display link and a simulator app that UIKit has throttled or backgrounded may have none
  to hand out, while a private target is always available and never disturbs presentation;
  iOS is unified memory, so `getBytes` reads the shared texture without a blit. Pixel size is
  the drawable's (points × screen scale, the same truncation the layout uses), so a golden is
  the full screen at 3× on the iPhone 17 Pro and 2× on the iPad Pro 13": several hundred KB of
  flat UI each, compressed well by PNG. Tolerance is the app self-test's 1 % of pixels with
  the 24-value channel slack (hinting, the cursor, the RTT readout); goldens are per device,
  `ios-phone-*.png` and `ios-pad-*.png`, chosen by the viewport width the dump reports
  (`device()` in `tests/ios.rs`, the same threshold that decides the terminal's fit). Both
  scenarios render: the fitted shell after its echo and the conversation view with the
  composer above the key bar. `foreground_fraction` moved into `slopty_e2e::snapshot` so the
  blank-frame guard is shared by the Mac and simulator tests (threshold 0.2 % on iOS: a fitted
  terminal's few lines are under 1 % of an iPad's 2064×2752). First finding: the phone's
  terminal golden differed by 1.7 % between two runs — one 140-pt band at the bottom and every
  text row. GameController reports the Mac's keyboard to the simulator about a second after
  launch, the poll hides the key bar and the terminal is refitted, so the frame depended on
  which side of that poll the render landed. The e2e build reads `SLOPTY_HARDWARE_KEYBOARD`
  (`0|1`) before asking the platform and the harness launches the simulator app with `0`:
  goldens are the glass-only layout, key bar in frame, whatever the simulator has attached.

- ✅ **Headless UI tests read the scene like a DOM** (2026-09-05). Item roots carry
  `.debug_selector("item-<uuid>")` and the canvas root `"canvas"`; `cx.debug_bounds` gives
  their laid-out bounds, `window.painted_quads()` the borders and colours the frame actually
  produced, and `simulate_click(bounds.center())`/`simulate_keystrokes` drive them. The fake
  host is an `mpsc` pair: the test asserts what the canvas sent and feeds back the
  `CanvasSync` deltas and `Frame`s a host would. The accessibility tree is readable the same
  way (see the accessibility ruling below).

- ✅ **Accessibility: one accesskit tree, read by VoiceOver and by the tests** (2026-09-05).
  *Tree exposure (fork `120daa1eef`):* GPUI builds its accesskit tree only while a screen
  reader is attached, so `Window::set_a11y_active(true)` (test-support) forces it and
  `Window::a11y_tree()` returns the last frame's `TreeUpdate`; the same call works on the
  test platform, the real macOS window under `test-support`, and `gpui_ios`. The self-test
  socket turns it on at start-up (`SLOPTY_TEST_SOCKET` set, `e2e` feature) and `dump.a11y`
  is the tree trimmed to `{role, label, value, focused, bounds}` per node, depth first in
  reading order, bounds in window points (GPUI stores them in device pixels; the walker
  divides by the scale factor). Headless tests read `slopty_ui::a11y::tree(window)`, the
  same shape.
  *iOS bridge (`gpui_ios/src/ios/a11y.rs`):* UIKit has no accesskit adapter, so the bridge
  mirrors the tree as `UIAccessibilityElement`s on the Metal view: one element per node
  with a label, a value or a click action (containers only lend their order), frames in
  the view's coordinate space, traits from the role (button, header, image, static text,
  search field, keyboard key, selected, not enabled, updates-frequently for terminals and
  status), `accessibilityActivate` → accesskit `Click` → GPUI's click listeners. The tree
  turns on when VoiceOver is running at launch or the first time UIKit asks the view for
  its elements; a `UIAccessibilityLayoutChangedNotification` follows a changed list, with
  the focused element when the focus moved. Constants come from the objc2 statics; every
  `unsafe` names its rule.
  *Roles and labels:* every button-like element is a `Role::Button` with an `aria_label`
  (title-bar pills "take over" / "mute" / "show chat", badge answers, the top bar's
  "shell" / "agent" / "note" / "window" / "fit", "N need you", the host switcher and its
  rows, pairing, the key bar keys by name: "Escape", "Control", "Command, armed"…); an
  item's title bar is a `Heading` labelled "`<kind> <title>`"; the agent pill and the
  conversation's attention row are `Status` with their text; conversation entries are
  `ListItem`s labelled "You: …" / "Claude: …" / "Tool Bash: …"; the composer, a note and
  the find field are the gpui-kit inputs with `aria_label`; the minimap and a remote window
  are `Image`s. The grid is `Role::Terminal` with the program title as its label and *the
  cursor row's text* as its value (`TerminalView::cursor_row_text`, computed only while the
  tree is active): a screen reader should hear the line being worked on, not 24 rows.
  *Focus order (macOS):* `slopty_ui::a11y::tab_stop` makes an element focusable at tab index
  0, so the ring is render order, which is reading order (top bar, then items by z, each
  title bar left to right, the conversation's attention row, the composer, its send button).
  Tab and ⇧Tab walk the ring only while a control is focused (a terminal's Tab is the
  shell's, a text field's is its own); `ctrl-tab` / `ctrl-shift-tab` (`FocusNext` /
  `FocusPrev`, "Canvas" context) enter it from anywhere. Enter and Space click (GPUI's
  keyboard click). A mouse press does not move the focus to a control (the pill's mouse-down
  stops propagation before GPUI's focus-on-click), so a click on "allow" leaves the keyboard
  in the terminal, as macOS buttons behave. The ring is `surfaces.accent` (the design
  system's focus-ring token) as a 1 px border under `focus_visible`: keyboard focus only,
  the mouse never paints it.
  *Tests:* `the_title_bar_is_read_and_tabbed_in_reading_order` (canvas: heading, status,
  pills in order, ⌃Tab then Tab, the ring quad, Enter answers),
  `the_attention_row_and_the_composer_are_read_and_tabbed`, `the_grid_reads_its_cursor_row`
  (slopty-ui); `dump.a11y` assertions in the app self-test (terminal item, top bar,
  conversation) and in both simulator tests (grid, conversation, key bar). Left out: no
  VoiceOver session was driven by hand (the bridge is checked by its compile and the tree it
  mirrors); gpui's own `a11y_tree_is_readable_once_forced_active` covers the fork.

- ✅ **`cargo xtask e2e` builds with `--bins`, never `--bin slopty-app`** (2026-09-05). A `--bin`
  filter applies to every `-p` on the command line, so the daemons and the CLI were not rebuilt
  and a stale `slopty-worker` answered `ProtocolVersion { worker: 9 }` to a protocol-11 app: every
  suite failed at pairing within a second. `--bins` builds each selected package's binaries.

- ✅ **Frame-time scenarios run through the self-test socket, and the harness draws differently
  from the product** (2026-09-05). `crates/slopty-e2e/tests/smooth.rs` (`cargo xtask e2e
  smooth`, gate `SLOPTY_SMOOTH_E2E`; `smooth-ios` in the simulator) opens N flooding shells
  (`open` with a command and a count), pans at 120 scroll events a second (a trackpad's report
  rate) for 5 s, pinch-zooms fit → 200 % → fit with ⌘-scroll, streams the first display beside
  five shells (`add_display`, needs `SLOPTY_SCREEN_E2E`), pans and zooms with a 2 000-line
file card beside five shells (`open_file`, 2026-09-12), and types 60 letters at 15 a second
  with `SLOPTY_PREDICT=never` and `always`, reading `dump.frames` and the terminal's key
  latency back. Two things about the harness to keep in mind when reading its numbers: the
  `e2e` feature is GPUI `test-support`, under which a dirty window is drawn synchronously
  inside `flush_effects` rather than from the display link, so "frames" are updates, not
  vsyncs, and the interval columns are not display cadence; and the machine is shared with
  other sessions' builds (`pgrep -fl "cargo|rustc" | wc -l` goes in the log). Draw-time
  percentiles are valid either way. The suite is serial (`--test-threads 1`) because two apps
  drawing at once halve each other's budget. The one assertion with a limit
  (`PAN_P95_LIMIT`) fails when the twenty-shell pan's draw p95 exceeds it on the Mac.

- ⚠️ **GPUI drops keystrokes when the focused element is not in the frame** (found by the
  app self-test 2026-09-05: ⌘W dead after ⌘1). Below `CARD_ZOOM` the terminals are drawn as
  cards without their views, so the focused `TerminalView` handle had no node in the
  dispatch tree and nothing, not even canvas bindings, ran. `CanvasView::keep_focus_rendered`
  moves focus to the canvas while in card mode and back to the active terminal when the
  grids return; the headless test `zooming_out_to_cards_hands_the_keyboard_to_the_canvas_and_back`
  pins it.

- ✅ **Playing an agent in the self-tests: hook JSON to the worker's control socket, never typed
  into the shell** (2026-09-05). The worker already takes `CtlRequest::Hook { session, payload }`
  on its control socket (what `slopty hook` relays), so `Stack::play_hook` writes the fixture
  transcript (`harness::TRANSCRIPT`, never a real `~/.claude/projects` file) under the run's
  temp dir and sends the payload there from the test process, with the session id read from
  the dump. A first version typed `printf … | slopty hook` into the shell under test and
  waited for the state to change: synthetic keys running a command in a shell and reading the
  result back is the surveillance-shaped pattern CLAUDE.md forbids, and it got that session
  flagged. Keys over the test socket drive the app's own UI only (⌘⇧L, the composer, the
  buttons); the one line the composer sends is a shell comment, so the shell echoes it and runs
  nothing. The same path drives the simulator, since the worker stays on the Mac.

- ✅ **The self-test plays the agent with a fake `claude` on ptyd's `PATH`, never by typing**
  (2026-09-05). `Stack::launch_with_fake_claude` gives ptyd and the worker a `HOME` and a `PATH` of
  their own and writes a small `claude` script into the run's temp directory; "+ agent"
  (⌘⇧T) starts it. It paints the spinning title, then the sparkle one, and writes the fixture
  JSONL into `$HOME/.claude/projects/<escaped cwd>` where the real one would, stepping from
  stage to stage when the test creates a marker file, so nothing depends on a sleep. This
  keeps the rule from the played-hook ruling above: the test drives the app's own UI and the
  harness's own environment, and never types a command into the shell under test.

- ✅ **The "hooks" pill is clicked end to end, through the accessibility tree** (2026-09-06),
  closing the gap the ruling above used to name. The pill's position *is* in the dump: it is a
  button with a label, so it carries bounds in `dump.a11y`, which is also how a screen reader
  reaches it — the test clicks the middle of those bounds with `Command::Click` and asserts
  the worker wrote Claude Code's settings with the relay and all 12 `HOOK_EVENTS`. The file asserted
  is `<run temp dir>/home/.claude/settings.json`, and the test asserts that path is **under the
  run's directory before asserting its content**, so a wiring mistake fails the test instead of
  editing the developer's `~/.claude`. The pill retires after one click, so a second *click* is
  not reachable through the UI; idempotence is asserted against the file the daemon actually
  wrote (`install_at` → `Unchanged`, bytes identical).

- ✅ **The two CoreAudio openers run one at a time** (2026-09-13). Gate 341's only failure
  was `a_worker_reassembles_nacks_reports_and_stops_with_the_connection` timing out after 20 s
  waiting for the player, in the same minute `slopty-codec`'s `player_starts_and_drains` took
  47 s: both had just opened CoreAudio's first client of the session, right after an iOS
  simulator run, in parallel. Ruling: a nextest test group `coreaudio` (`max-threads = 1`)
  holds the two tests that open a `Player`, with a 60 s slow timeout, and the worker test
  waits 45 s for the player. Neither test measures the open; the wait is for the machine.

- ✅ **The remote window's pointer is tested through a laid-out window, against the bounds it
  recorded** (2026-09-13). Line coverage put `slopty-ui::screen` at 65 %, with every pointer
  and scroll path untested at the headless layer: the mapping from a card point to a stream
  pixel needs the picture's bounds, which only a render records. Ruling: the test opens the view
  with `add_window_view`, resizes and parks so the canvas prepaint stores the bounds, then reads
  those bounds back and drives `simulate_mouse_*`/`simulate_event(ScrollWheelEvent)` at
  fractions of them, asserting stream pixels as fractions of the stream size. Expectations are
  never hard-coded window pixels: the letterboxed picture's size is the layout's business, and
  a test that pinned it would break on any chrome change without a behaviour change.



- ✅ **The repo's volume must be mounted with ownership on, or every VideoToolbox session costs
  ~29 s** (2026-09-15). `slopty-codec`'s `hevc_encode_then_decode` takes **108, 116 and 118 s** run
  from `/Volumes/Lacie/...`, at 9 % CPU throughout — waiting, not working — and **0.53 s** run from
  `/tmp`. The binary is 1.28 MB and reads at 1.2 GB/s, so it is neither size nor throughput.

  The cause is the mount flag, proven rather than inferred. The boot volume is mounted with
  ownership; `/Volumes/Lacie` is mounted `noowners`. A 200 MB APFS disk image **created on the
  external disk** and attached with `-owners on` runs the same binary in **0.50 s** — same physical
  hardware, same bytes, only the flag differs. macOS will not use the cached code-signature path
  for a process on a volume where ownership is ignored, so every `VTDecompressionSessionCreate`
  revalidates. Later sessions in one process are 4 ms, which fits: the cost is validation, not the
  codec.

  The fix is one command and needs no rebuild: `sudo diskutil enableOwnership /Volumes/Lacie`.
  `CARGO_TARGET_DIR` on the boot volume would also work but is the worse trade — `target/` is
  265 GB against 103 GB free, and it fixes only what it moves.

  **Two earlier conclusions were wrong and are withdrawn.** The first blamed the decoder (a 🔬 in
  `video.md` claiming `VTDecompressionSessionCreate` had regressed from 150 ms to 28 s). The second
  blamed cargo's `linker-signed` ad-hoc signature, on 108 s against 0.53 s for "the same binary
  re-signed" — but the re-signed copy had also been moved to `/tmp`, so the comparison moved two
  variables and credited the wrong one. Running the *unmodified linker-signed* binary from `/tmp`
  (0.56 s) is what isolated the volume; the disk image is what isolated the flag. An `xtask` change
  that re-signed every test binary before each run was written against that wrong cause and has
  been reverted. Change one variable per measurement, and a copy to another volume is a variable.

- ✅ **No test runs cargo, and only the audio test waits on CoreAudio** (2026-09-26). The
  worker's end-to-end tests built their idle-window fixture with `cargo build` on every run.
  Whenever another build held the lock, the test waited past its 120 s limit. The fixture now
  comes from the build that ran the tests. `cargo xtask e2e` builds every binary a suite
  spawns before it starts and names their directory in `SLOPTY_E2E_BIN_DIR`, and a workspace
  test build, like the gate's, puts the fixture beside the worker. The echo test runs without
  the window when the fixture was not built. Its window opens then fail at ScreenCaptureKit,
  which still loads the worker. The screen tests, gated on `SLOPTY_SCREEN_E2E`, require the
  fixture. The client's screen-worker test had covered cursor, reassembly, NACKs, the worker
  stopping with its connection, and audio, all in one. Every failure on record was its wait
  for the player: the Opus encoder took about 40 s in a loaded gate, and the player had not
  opened 45 s later. The audio half is now `audio_waits_for_the_player_without_holding_video_and_counts_gaps`,
  alone in the `coreaudio` group, and the video half no longer waits on audio. The waits on
  VideoToolbox and on CoreAudio are bounded at 100 s (`FOR_THE_MACHINE`). None of them
  measures the framework, so the bound only makes a stuck framework fail with the stats. The
  waits on the worker's own logic stay at 3 s.

- ✅ **A Mac golden allows 0.2%, measured, not 1%** (2026-09-27). The 1% every Mac golden
  allowed was set before the noise was known. At 1280×800 it let ten thousand pixels through, and
  `through-server.png` kept a washed row and a word of status text for a day. The e2e cursor now
  never blinks (`harness::pinned_settings`), so a frame no longer depends on the blink phase. Two
  runs of every golden then differed by 0.051% at most on the Mac (`browser`), 0.005% for
  `through-server` and 0.009% on the iPhone. `snapshot::MAC_TOLERANCE` is 0.2%, four times
  that, shared by every Mac case. `transfers` keeps 1%: its port is the one the OS picked and its
  upload moves (0.43% between runs). iOS went from 0.3% to 0.05%: the iPad's runs, once 0.06%
  apart with a blinking cursor, now differ by 0.002%.

- ✅ **The bulk-rate tests hold BBR3's model against the path, not a rate** (2026-09-27).
  `bulk_over_delay` and `upload_rate`'s large file asserted 80 Mbit/s and 3 s. That is a floor on
  the cores free, not on the congestion model. With another session's job on all eight
  performance cores they measured 18-27 Mbit/s and failed three gates on code that had passed an
  hour before. A window-over-bandwidth-delay ratio was tried next, and it failed too (0.73 at
  load 250): the delivery rate is a ten-second peak while BBR3 sizes by its current estimate, and
  the two drift apart under load. The bug these tests guard is `BBR.min_rtt` stuck at the 5 ms
  initial estimate on a 60 ms path. So they now assert exactly that. BBR3's `BBR.min_rtt`
  (exposed as vendored noq-proto patch 7, `Bbr3::min_rtt`, reported in
  `congestion::Snapshot::model_min_rtt`) must be at least half the path's own measured minimum.
  With the model right the two are equal: 0.9 ms to 61 ms at load 150, and they come from the same
  acknowledgements, so load moves neither. Stuck, the ratio is 0.08 at 60 ms and 0.25 at 10 ms
  each way. The short-trip test checks from 10 ms each way up. The vendored crate's own tests
  (patch 6's unit test) do not run in Slopty's gate. The rates stay printed as `MEASURE` lines.
  The many-small-files bound is now half the one-round-trip-per-file time (1 s), not a quarter:
  it read 0.58 s with every core taken, and the bug took 2.16 s. The nextest override that ran
  `bulk_over_delay` alone is gone.

- ✅ **`transfers` is deterministic and holds the Mac's 0.2%** (2026-09-27). Amends the 0.2%
  entry above, which let `transfers` keep 1%. Its runs differed by 0.43% to 2.8%, and the diff
  showed why: the long temporary path the test `cd`'d into wrapped differently from run to run,
  and the port was the one the OS picked. The test now runs with a home of its own
  (`Stack::launch_at_home`, the real path of `home` under the run's root), so the shells say
  `~/drop-here` and no temporary path of the machine's is in the render. It listens on the first
  pair of free ports from 47310 (`steady_port`; the app forwards a port its own machine holds on
  the next one), which is 47310 → 47311 on a quiet machine. The upload's progress needs no hold:
  a 2 GiB sparse file still reads 0% when the frame is taken. Three runs then differed from the
  golden by 0.008%, 0.008% and 0.009% (the `app` suite's `a_forwarded_port` test, run alone
  three times in `cargo xtask e2e app`'s environment; each run's `snapshot transfers` line), so
  it uses `snapshot::MAC_TOLERANCE` and `TRANSFERS_TOLERANCE` is gone.

- ✅ **The app under test reads a stand-in tailnet** (2026-09-27). The first-run panel now looks
  for servers and workers on the tailnet (`docs/decisions/ui.md`). An app under test on a Mac
  running Tailscale would find what this machine's tailnet answers, a worker of the developer's
  own included, and the `first-run` and `add-worker` goldens would show it or not by timing.
  In the e2e build the app reads the `Status` JSON named by `slopty_e2e::TAILNET_STATUS_ENV`
  instead of the `LocalAPI`, and `spawn_app` writes an empty running tailnet there, so the panel
  says "Nothing answered on your tailnet" at once; `first-run` waits for those words.

- ✅ **A live test is `#[ignore]`d, not gated on a variable** (2026-09-28, audit finding 60).
  A live test used to return early when its variable was unset, so `cargo gate` counted it as
  passed without running it. Now each one carries `#[ignore = "live: <the command that runs
  it>"]`: the gate lists it as skipped, and `cargo xtask e2e <case>` runs it with nextest's
  `--run-ignored only`. The six `fn gated()` copies in `slopty-e2e` are gone, and the tests
  read no variable to decide whether to run.
  - Within a live target, xtask picks tests by module. Tests that need the Screen Recording
    grant sit in the target's `screen_recording` module and run only with
    `--screen-recording`, the flag that replaces setting `SLOPTY_SCREEN_E2E` beside another
    case. The app's frame-time measurement sits in `frame_time`, which `app` leaves out and
    `smooth` runs alone.
  - `cargo xtask e2e screen` runs `screen_stream_over_quic` from the worker's target by
    default. `--filter` reaches the target's other live tests, which are measurements.
  - Still to convert: the live tests of `slopty-capture`, `slopty-input` and
    `slopty-vdisplay`, which still read `SLOPTY_SCREEN_E2E`/`SLOPTY_INPUT_E2E`. Their suites
    are `Kept::Env` in `xtask/src/e2e.rs` until then.
  - Test: `a_suites_filters_the_callers_and_the_capture_rule_all_apply` (`xtask`).

- ✅ **Allocation budgets gate the hot paths** (2026-09-29). Wall time cannot pass or fail a
  commit on a machine other sessions load, but the allocations a path makes are the same on
  every run. So `tests/allocs.rs` in `slopty-media`, `slopty-engine` and `slopty-grid` install
  a counting global allocator (`slopty_testkit::alloc::Counting`, about 40 lines over `System`)
  and hold each steady-state path to a number of blocks, and to bytes where they say something.
  The counts are per thread, so the harness's threads do not move them, and each test first
  checks that the counter is installed, so a budget never passes on zeros. The published
  counters were not taken: stats_alloc, allocation-counter, assert_no_alloc and cap have had no
  release since 2021-2023. dhat allows one profiler per process and counts every thread. It
  stays the tool for finding where the blocks come from, not for asserting them.
  - Held today: cutting a frame, 3 blocks at any size or parity; putting one back together,
    3; a key's byte into the engine, 0; its diff frame, 4; an Enter at a bottom prompt, 10 on
    any screen; the echo frame encoded, 8; a row applied on the client, 1; a line scrolling into
    a full history, 1.2 at most. A frame or an echo handed to eight viewers costs exactly what
    one does.
  - The first run found three extra costs, and all three are fixed. The packetizer grew its
    datagram list for the parity, 5 blocks a frame where 3 do. The engine read every row of the
    screen into a new line on a scroll, 30 blocks at 80×24 and 66 at 200×60. The client's
    scrollback rebuilt its index with `split_off` each time the oldest line moved: 6 blocks
    and 1.4 KiB a line. MEASUREMENTS, "allocation budgets", has the before and after.
  - Encoding an echo frame takes 8 blocks because `slopty_proto::codec::encode` grows its
    buffer from the 4-byte prefix. Sizing it first would make that 1. That is `slopty-proto`'s
    change, and the budget is lowered when it lands.
  - Not yet covered: the client's datagram reader, the worker session actor's fan-out, which
    adds a credit per viewer, and a steady terminal paint in `slopty-ui`. Each belongs in its
    own crate's `tests/allocs.rs`.
  - Tests: every test in those three files.

- ✅ **A measurement is held to retired instructions; wall time is a nightly trend**
  (2026-09-29, audit finding 61). The `*_cost` measurements printed percentiles they computed
  by hand, with the median code copied into each, and asserted nothing. Now they time their
  samples with `slopty_testkit::bench`. For each sample it also reads the process's retired
  instructions (`proc_pid_rusage`, `ri_instructions`: no root, no entitlement) and subtracts
  what the two readings cost. It then writes one JSON line per series. `cargo xtask bench` runs
  them in release and holds each series' median instructions per operation to
  `xtask/budgets.toml`, within 5 %.
  - Two runs back to back, the second with other sessions building, agreed within 0.1 % on
    most series and within 3 % on all 31. The 3 % was `frame_cost.write` at 861 instructions,
    where the calibrated overhead is a large share. MEASUREMENTS has the table.
  - darwin-kperf needs root or a private entitlement, and callgrind and gungraun have no
    Apple-silicon macOS port. criterion and divan measure wall time, which is the noise this
    avoids.
  - A series that times other threads (the audio render with the decoder pushing beside it) is
    `wall_only`: the instruction count is the whole process's.
  - Wall times go to `target/nightly/bench.jsonl` only with `--wall`, which the nightly run
    passes, and a move of more than 25 % since the last night is called out, never failed.
  - The bench is not in the gate: its release build with fat LTO is minutes. It runs nightly,
    and by hand after a change on the input, terminal or frame path.
  - Tests: `a_budget_holds_within_its_slack`,
    `the_measured_crates_are_the_ones_that_take_the_testkit`,
    `budgets_round_trip_through_their_file`, `the_checked_in_budgets_parse` (`xtask`), and
    `a_series_reports_per_operation_and_as_json` (`slopty-testkit`).

- ✅ **The daemons soak under a scripted load, and `leaks` reads them at the end**
  (2026-09-29). Nothing ran longer than a scenario, so growth in a daemon went unseen: a map
  never pruned, a descriptor or task that outlives its session. `cargo xtask soak` starts the
  server, ptyd and worker from a temporary HOME, the worker registered with the server. It
  drives cycles through the `slopty` CLI: open a quiet bash, flood it, play a hook through
  `slopty hook report`, read it back, close it. It samples each daemon's `ri_phys_footprint`,
  open descriptors and threads. It fails on footprint growth faster than 64 KiB a minute by
  least squares over the last two thirds of the load, on a peak over the daemon's budget, on
  descriptors or threads left after a 12 s settle, and on any leak `leaks <pid>` reports.
  - The footprint is what jetsam and Activity Monitor count, where the resident set is not.
  - `leaks` cannot read a hardened-runtime binary, and `cargo xtask sign` signs the dev daemons
    with the hardened runtime. So the soak copies them to `target/deep/soak/bin` and signs the
    copies ad hoc, without the runtime and with `get-task-allow`. The build tree's signatures,
    and the TCC grants tied to them, are untouched.
  - `MallocStackLogging` is off unless `--stacks` is given: it adds its log to the footprint
    the soak budgets.
  - No display stream runs. The worker has no switch that puts its synthetic capture behind a
    stream, and a real capture needs Screen Recording. Nor is the worker restarted mid-soak.
    Both are next, the first as a test switch in the worker daemon.
  - The first run found ptyd keeping a descriptor and about 145 KiB for every closed session,
    and a race in the control socket's client that fails a hook report now and then
    (MEASUREMENTS, "the first soak"). Those are ptyd's and the platform crate's to fix. The
    budgets stay where they are, and the soak fails until then.
  - Test: `the_slope_is_the_least_squares_fit` (`xtask`); the soak itself is the check.

- ✅ **A local nightly runs the heavy lanes** (2026-09-29). The weekly Deep workflow had never
  produced a result on its schedule, and the soak, the bench's wall times and amplified property
  tests need this Mac. `cargo xtask nightly` runs them one after another under `nice -n 10`:
  the soak for 20 minutes, the bench with `--wall`, the property tests at `PROPTEST_CASES=4096`,
  `slopty-ui`'s tests under `ITERATIONS=50` scheduler seeds, and `deep` miri, address and
  thread sanitizers, coverage and features. Each writes a log and a JSON result under
  `target/nightly/<date>/`, beside a summary. A check whose tool is missing is skipped with the
  reason, not failed. `cargo xtask nightly install` loads a LaunchAgent that runs it at 03:00 at
  background priority. It is installed only by that command.
  - `slopty-media`'s pipeline property test used `ProptestConfig::with_cases(256)`, which
    overrides `PROPTEST_CASES`. Its default config is the same 256 and reads the variable, so it
    now takes the default.
  - Tests: `the_launch_agent_runs_the_nightly_at_three` and
    `every_check_is_named_once_and_the_deep_ones_say_what_they_need` (`xtask`).

- ✅ **Closing anything in the workspace leaves no entity and no map entry behind**
  (2026-09-29). Views that outlived their tile, and entries that outlived their worker, lasted
  as long as the app and nothing could see them. `workspace/tests/leaks.rs` has
  `closes_clean(view, cx, cycle)`. It settles the workspace and records `footprint()`, the length
  of every map and list `WorkspaceView` keeps. Then it runs the open-and-close cycle once, so
  whatever is made once per app is made. It takes GPUI's `leak_detector_snapshot()` and runs the
  cycle again. After each run it lets every wait run out (the undo of a close, the toast, the
  palette's way out) and draws two frames. It then calls `assert_no_new_leaks` and checks that
  the footprint is back where it started. `LEAK_BACKTRACE=1` records where each handle was made,
  though the pinned fork's formatter matches mangled symbol names and prints no frames on macOS.
  - The cycles close a shell with output, a streaming window, a file with an unsaved edit, a page,
    a note and a folder. They open and dismiss the palette, the window picker and a tile's name
    field, and toggle the overview. They show, feed and hide an agent's face, then close its
    shell. They add and remove a second worker holding a tile of every kind.
  - Found: a dismissed palette was still held by a strong handle for every frame it had drawn.
    `cx.on_next_frame` keeps its entity until a frame runs, and a hidden window runs none, so
    the palette now schedules that check through a weak handle. Removing a worker left its items
    in the tile recency and its key in `given_shell`, `given_pending` and the navigator's folded
    set, so a worker added again was never given a shell. `remove_worker` now drops all of them.
  - A stream view took an `NSProcessInfo` activity that keeps the display awake. Under test it
    held this Mac's display on, and the first one in a fresh process took about 17 s, so every
    headless test that opened a stream took 17 to 32 s. Tests no longer take it; GPUI's own
    hold is what they assert. Those tests now run in about 50 ms.
  - Tests: `a_closed_terminal_leaves_nothing`, `a_closed_stream_leaves_nothing`,
    `a_closed_file_with_an_edit_leaves_nothing`, `a_closed_page_note_and_folder_leave_nothing`,
    `the_overlays_and_the_overview_leave_nothing`, `a_closed_agent_with_its_face_leaves_nothing`
    and `a_removed_worker_with_open_tiles_leaves_nothing` (`slopty-ui`).
