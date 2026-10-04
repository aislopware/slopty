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



- ❌ **Superseded 2026-09-30 by `tooling.md`, "A test binary's directory, not the volume".** The
  isolation below moved two variables, the mount flag and the binary's directory, and credited the
  flag. The cost follows the number of entries in the executable's directory, so
  `enableOwnership` would not help. The xtask test runner's hard links fix it. The record of the
  original entry follows.

  **The repo's volume must be mounted with ownership on, or every VideoToolbox session costs
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
  opened 45 s later. The audio half is now `audio_waits_for_the_player_and_counts_gaps` (on the worker's one
  sound since 2026-10-01), alone in the `coreaudio` group, and the video half no longer waits on
  audio. The waits on
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
  the next one), which is 47310 → 47311 on a quiet machine. The upload's progress did not
  seem to need a hold, since a 2 GiB sparse file read 0% when the frame was taken; it later read
  2% on another run (see "An upload a golden draws is held"). Three runs then differed from the
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

- ✅ **The app under test is told its workers' macOS grants** (2026-09-30). A worker reports
  whether this machine granted its binary Screen Recording and Accessibility, and the navigator
  says "Screen Recording off" under a worker without it. The navigator and conversation goldens
  had been accepted on a machine that had not granted them, and failed on one that had: the line
  moved every row below it by 12 pt. In the e2e build the app sets both grants in each worker's
  caps (the hello and every later `Caps`) from `slopty_e2e::WORKER_GRANTS_ENV`, and `spawn_app`
  names both, the state a working Mac is in. A test that wants a grant missing names fewer
  through `Stack::launch_with`. The warning itself is covered headless (`workspace::tests::facts`).

- ✅ **An upload a golden draws is held** (2026-09-30). How far an upload got by a given frame is
  up to the machine: `conversation-attachment` read "↑ 0%" on one run and "↑ 2%" on another.
  `xfer::Table::hold_uploads` holds every upload on a link before its next chunk, and a cancel
  still stops a held one. The e2e build's app holds each link's uploads when
  `slopty_e2e::HOLD_UPLOADS_ENV` is set, and the attachment test launches with it, so the chip
  reads 0% on every run. `slopty-client`'s `upload_hold` tests show a held upload sends no byte
  until it is released, and that a cancelled one stops.

- ✅ **A golden of a prompt waits for the caret the shell sets there** (2026-09-30). Slopty's
  zsh integration writes the prompt (`133;A` to `133;B`) and then, from `zle-line-init`, the
  bar (`ESC[5 q`), after the block `preexec` set for the command. The `terminal` golden once
  held the block: 119 pixels. It was not a timing effect. When the bar came in a write of its
  own, the engine sent no frame for it, because libghostty dirties no row for a caret changed
  in place. The app then drew the block at the prompt until something else changed.
  `slopty-engine` now remembers the caret its last frame carried and sends a frame when it
  changes (`a_caret_changed_alone_sends_a_frame`; MEASUREMENTS, "a caret changed alone").
  The harness still waits rather than widening the tolerance. The dump carries each
  terminal's `cursor_shape` (the program's DECSCUSR) and `at_prompt` (the cursor's row carries
  a prompt mark and no command runs). The `terminal` golden waits for
  `TerminalInfo::reads_a_line` (at the prompt, under the bar). Every other golden helper
  (`gallery::settled`, `tiles::golden`, the `note`, `server-unreachable` and `through-server`
  renders) waits for `Dump::prompts_settled`: no shell stands at a prompt without its bar.
  Each wait fails after `STEP` with the last dump, which is how the engine bug surfaced.

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

- ✅ **Every decoder a peer reaches is fuzzed, and only a named test is retried** (2026-09-29).
  A peer's bytes reach postcard decoders, the zerocopy media parsers and the reassembler's
  Reed–Solomon path. Nothing had fed them anything but well-formed messages. `fuzz/` holds
  twelve libFuzzer targets: `client_msg`, `worker_msg`, `server_msg` and `uni_stream` through
  the real framing in fuzzer-sized pieces, then `client_datagram`, `term_datagram`,
  `media_header`, `cursor`, `origin`, `reassemble`, `feedback` (the worker's NACKs and reports)
  and `ctl` (the worker's socket line). `reassemble` and `feedback` are structure-aware
  (`arbitrary`). The first is a script of real packetized frames that the wire reorders, drops,
  repeats and damages, and its NACKs are answered from the packetizer. A fuzzer guessing headers
  would not get past the header check. Every target also asserts an invariant, not only the
  absence of a crash. A decoded value re-encodes to the same bytes, and `frame_head` agrees with
  the frame it heads. An undamaged run delivers each frame byte for byte and in order. A
  retransmission is a data fragment of the frame asked for. The bitrate stays between the floor
  and the ceiling, and the parity ratio within its bounds.
  - The crate sits outside the workspace. Its instrumented build is nightly, so the gate never
    builds it and gets no slower. The gate still checks its formatting, which takes 0.1 s. Each
    target is a library function, so `fuzz/tests/regressions.rs` replays `fuzz/regressions/`
    on a plain build. `cargo xtask fuzz` builds with ASan and runs each target in fork mode under
    `nice`, seeded from the 190 wire goldens. `deep fuzz` (30 s each) runs in the nightly and in
    the weekly Deep workflow, which uploads what it finds.
  - First run, 60 s per target on this Mac (about 70 s each with fork start-up): no crash, leak,
    timeout or broken invariant. Coverage reached `worker_msg` 6 319 edges, `server_msg` 5 962,
    `client_msg` 4 047, `uni_stream` 3 774, `ctl` 3 329, `reassemble` 2 867 (3 252 exec/s),
    `term_datagram` 2 454, `feedback` 2 068 and `client_datagram` 1 983. The fixed-layout
    parsers have almost nothing to reach (`cursor` 43, `media_header` 49, `origin` 162).
  - Found by reading what the targets decode, not by a crash: `Line`'s `Deserialize`
    (`crates/slopty-grid/src/line.rs`, `cells.resize(cols, …)`) pads every line to the `cols`
    the peer names. 100 lines of 65 535 blank columns are 703 bytes on the wire and 6.5 million
    48-byte cells once decoded (314 MB), so one 16 MB frame of them asks for terabytes. It is
    open, for the owner of `slopty-grid` and `slopty-proto`.
  - The `ci` nextest profile retried every test twice, against the rule that only a named test
    gets retries. It now inherits `gate` (`inherits = "gate"`), whose three session-actor tests
    are the only ones retried, and keeps its longer timeouts and JUnit report. A throwaway crate
    checked that nextest 0.9.146 carries a parent's per-test overrides into the child.
  - `deep sanitize` also covers `slopty-platform` and `slopty-capture`, which hold most of the
    `unsafe` (309 blocks between them, against 140 in the codec). First run: ASan and TSan
    each pass all 79 tests (4 live ones skipped), in 1.1 s and 8.4 s.
  - CI's release job gets the sccache the rustc wrapper needs; without it every tag build
    would fail as the gate did before 7b0e09a0. The gate job runs `--in-place`, as the gate's
    own help says CI should. `rust-cache` keeps only the registry and git checkouts, since
    sccache already holds the compiled units.
  - Tests: `every_target_has_its_entry_point`, `every_regression_input_replays_clean` and
    `the_seeds_run_through_every_target` (`fuzz`), plus
    `a_hex_golden_seeds_its_stream_and_a_line_the_control_socket`,
    `every_golden_parses_and_every_seeded_target_exists` and
    `the_smoke_runs_in_fork_mode_within_its_time` (`xtask`).

- ✅ **A worker under test serves a drawn screen, and the app self-test streams it**
  (2026-09-29). Remote desktop is the first priority, yet no app self-test streamed a picture:
  a worker started from a shell has no Screen Recording grant, the Mac's real screen belongs to
  whoever uses it, and the drawn `Canvas` was reachable only from in-process measurements. With
  `SLOPTY_SYNTHETIC_SCREEN=1` (a test knob read once, in the style of `SLOPTY_WINDOW_CAPTURE`)
  the worker serves every stream, its listing, its warm-up and its window resizes from
  `slopty_worker::screen::synthetic::Synthetic`: `Studio`, a capture that lists one display
  (id 1, 1512 × 982 points at 2×, 60 Hz) and two windows (7001 "Synthetic editor", 1280 × 800;
  7002 "Synthetic terminal", 800 × 500) that take a resize, each drawn as a `Canvas` of its own
  size; the real VideoToolbox encoder; and `Poke` for input.
  - The daemon picks the platform per stream (`screens::run`), so the product path stays
    `Pipeline<MacOs>` with every call direct, and the drawn one is a second monomorphisation.
    `StreamControl` lost its platform parameter for it: it is `Arc<dyn Controlled>`, as
    `StatsHandle` already was, a virtual call on the feedback path only. A display made for the
    client is never made on a drawn worker; it streams the drawn display as the physical one.
  - Goldens of a moving picture: `Canvas` scrolls its page in the middle half of the picture
    and keeps the desktop and the strip still, so a golden masks the page and holds the rest
    to its pixels (`snapshot::assert_matches_masked`), and holds the page to the golden's mix
    of luma instead (`snapshot::luma_distance`, the share of pixels in another sixteenth of
    the luma range: 0.006–0.015 between runs, a black or misplaced picture is near 1). The
    tile header's health word follows the last second's pacing, which the machine's load
    moves, so it is masked too and asserted through the accessibility tree.
  - Every live readout is masked by its accessibility node, in pixels and in words: the status
    bar's rate and frame time, and the stats overlay's figures (2026-10-03). The overlay's line
    was a run of text with no node, so it was compared pixel for pixel, and "59 fps" against a
    golden's "60 fps" failed `stream-window-stats` (0.485 % and 0.589 % against 0.4 %). It is
    a `Label` now, which a screen reader also reads. The 0.4 % had also hidden goldens a day
    behind the chrome's icons (0.34 % of every stream golden); they were retaken, and the
    stream goldens hold to the chrome's 0.2 %, since the still desktop's codec rendering moves
    them by 0 to 0.033 % (`docs/MEASUREMENTS.md`, "the stream goldens and the late beats").
  - The harness runs the binaries from a copy in the temporary directory (`harness::bin_dir`).
    A process whose executable sits in a directory of ~100k entries (`target/…/deps`) pays for
    it in every VideoToolbox session it opens (`tooling.md`, "A test binary's directory, not the
    volume"): the drawn-stream unit tests timed
    out at 120 s from `target/`, and passed in 0.54 s copied to `/tmp`. The measurement
    client is a binary (`slopty-glass`) for the same reason: a test binary is not copied.
  - What the app cannot show here: presentation. Under a remote session to this Mac the app's
    window is covered, the window server reports no frame presented, and arrival → present
    has no sample; the tests say so and time the path with `slopty-glass` instead. Zoom (a
    pinch) and trackpad mode are the touch UI's, and the Mac's test socket has no pinch and its
    dump no toggled state, so neither is asserted here.
  - Tests: `a_drawn_window_streams_into_its_tile`, `the_drawn_display_streams_into_its_tile`
    and `frame_time::drawn_frames_reach_the_glass_on_loopback` (`tests/app/stream.rs`);
    `only_a_one_switches_the_drawn_screen_on`,
    `the_drawn_screen_lists_its_windows_and_takes_a_resize` and
    `a_drawn_window_streams_at_its_own_size` (`slopty-worker`); `a_masked_pixel_is_neither_compared_nor_counted`
    and `a_scrolled_mix_is_near_and_a_blank_picture_is_far` (`slopty-e2e`).

- ✅ **Live tests that drive the desktop run in a macOS guest under tart** (2026-09-30). The
  person at this Mac works on it over Parsec, so no test here may move the real pointer, post
  HID events, lock the screen, raise a prompt or reach the login window, and none may run on the
  other Mac. A local macOS guest takes all of that: `cargo xtask vm` (`xtask/src/vm.rs`),
  `docs/DEV.md` ▸ "Live lane in a VM". Verified against the primary sources on this date.
  - **Tool: tart** (now `openai/tart`, `brew install openai/tools/tart`, 2.38.0, Developer ID
    signed by Cirrus Labs, 89 MB with Softnet). Its licence is FSL-1.1-ALv2, whose permitted
    purposes include internal use, which a test lane on one Mac is. What decided it:
    - Cirrus's `macos-tahoe-base` (26.6.2) and `macos-golden-gate-base` (27.0) images come set
      up: an `admin` account that logs in at boot, SSH, no sleep, screen saver or lock, Gatekeeper
      and **SIP off**, and the tart guest agent as a LaunchAgent in the logged-in session with
      Accessibility, Screen Recording and `PostEvent` granted in both TCC databases
      (`macos-image-templates/scripts/update-tcc-database.sh`). `tart exec` runs a command
      through that agent, so it sits in the session's window server with those grants, which is
      where HID posting and an AppKit child app need to be. An SSH shell is outside the session.
    - `tart clone` is an APFS clone: a clean guest per run in 30 ms, no disk until it writes.
    - `TART_HOME` puts every image and guest on the external disk.
  - **Rejected: lume** (trycua, MIT, 0.5.3). It can set a Tahoe guest up from an IPSW unattended
    and turn SIP off through Recovery over VNC. But it has no macOS 27 preset, reaches a guest
    only over SSH (outside the logged-in session, where cua's own GUI tests say not to run),
    grants nothing in TCC, and sends telemetry unless told not to. Everything the base images
    already are would be ours to build and keep working.
  - **Rejected: Virtualization.framework from Rust** (objc2). It fits the pure-Rust rule, and
    the entitlement is no obstacle (`com.apple.security.virtualization` works on an ad-hoc
    signature; only bridged networking's is restricted). But a macOS 26 guest from an IPSW
    stops at Setup Assistant with SIP on. `VZMacGuestProvisioningOptions` makes the account and
    SSH only for guests from 27 on. Getting through Setup Assistant and Recovery means typing
    into the guest's screen, which is what Cirrus's Packer templates do. Owning that for a test
    lane is the wrong place for the effort; the images are rebuilt from Apple's IPSWs by public
    templates and pinned here by digest.
  - **Not from an IPSW here.** An IPSW install (19.8 GB for 26.6.2, 26.6 GB for 27.0.1) gives a
    guest nobody has logged in to, SIP on, nothing granted: no HID test can run in it. The
    pinned image is the same install with those steps done. ipsw.me lists the 26.x virtual Mac
    IPSWs as no longer signed. A new image is taken on purpose, by changing the digest in
    `Macos::image`, never by a tag moving.
  - **TCC in the guest.** With SIP off, `create` writes rows for the installed worker's path
    (`~/Library/Application Support/Slopty/bin/slopty-worker`) into the system and user
    databases: Accessibility, Screen Recording, `PostEvent`, with no code requirement, so a
    rebuilt worker keeps them. Under launchd the worker is its own responsible process and needs
    its own rows. On 27 the user database sits in a ProtectedSystem container that tccd holds
    open (found with `lsof`, as Cirrus does). A PPPC profile cannot grant Screen Recording and
    needs MDM, and `tccutil` only resets, so neither is used.
  - **Deploy is `slopty worker deploy`, unchanged.** The CLI takes an `ssh` program but no ssh
    options. So xtask passes itself as that program and turns the CLI's `xtask vm <script>`
    into `ssh` with the guest's key and options. Depending on `slopty-deploy` from xtask would
    pull slopty-platform (objc2-app-kit, objc2-web-kit) into the gate's own binary, and would
    break `cargo gate` whenever another lane has slopty-proto half-edited. A `--ssh-option`
    flag on `slopty worker deploy` would retire the stand-in.
  - **Network.** The guest sits on tart's shared NAT (`192.168.64.0/24`), and its worker's
    `[worker] allow` admits this Mac's address as the guest sees it (`SSH_CONNECTION`). The
    host-only network (`--net-host`) needs Softnet under passwordless sudo, which the lane does
    not install.
  - **Bounds.** Each guest has 4 of the 10 cores and 8 GB of the 32. The macOS licence allows two
    more macOS instances, and Virtualization.framework refuses a third running guest. Spotlight
    and scheduled software updates are off in the base.
  - **What a run proves** (`cargo xtask vm e2e`): this Mac reaches the guest's worker over the
    VM network, and the greeting says macOS 26 with capture and input granted, and a shell
    echoes (`slopty-e2e`, `tests/vm.rs`). A real HID pointer move posted in the guest is read
    back there (`slopty-input`,
    `moves_the_real_pointer_on_a_display_stream`). A live test that would skip itself fails in
    the guest (`slopty_testkit::live::skip` under `SLOPTY_VM`), because nothing is missing there.
    That is decided in the test, not by reading its output: a scan for `skipped:` missed other
    wordings and failed on stats fields named `skipped`.
  - **Runs clean up after themselves, and only after themselves.** A run's guest carries its
    pid (`-run-<pid>-`); it goes when the run ends or on SIGINT, SIGTERM or SIGHUP, and a run
    killed outright leaves it for the next `live`, `e2e` or `prune`, which remove only guests
    whose pid is gone (a zero signal). Run guests share two fixed MACs under a lock, so the
    host's DHCP pool (a day's leases) is not spent a lease per run.
  - **Next.** The drag-and-drop P0 spikes (`audio.md`, "live lane") run here with
    `SLOPTY_DND_E2E=1`, which the lane sets. Screen lock and the login window are reachable
    (`pmset displaysleepnow`, a fast user switch) but no test drives them yet. The 27 guest is
    `vm create --macos 27` (a 34 GB pull).

- ✅ **The tailnet's live tests run against real Go daemons that xtask drives on loopback**
  (2026-09-30). Fakes of the `LocalAPI` (`slopty-tailnet`'s `fake`) prove what Slopty reads, but
  not what Headscale and `tailscaled` actually write: whether a grant's `app` value reaches the
  destination's whois verbatim, which path a ping reports, what the IPN bus sends. So
  `cargo xtask tailnet up` runs a real tailnet on this Mac's loopback: Headscale 0.29.4 and two
  userspace `tailscaled` nodes, `worker` (`tag:slopty-worker`) and `ci` (`tag:ci`), with a policy
  that opens the tailnet and grants `ci` Slopty's capability with `roles: ["agent"]` on `worker`.
  - **Go binaries are test fixtures, and no Go is written here.** Headscale is its release
    binary, pinned by version and SHA-256. `tailscale` and `tailscaled` 1.102.5 are built once
    with the local Go (`go install`, which checks every module against Go's checksum database)
    and cached under `target/tailnet/tools`. Only the xtask, which is Rust, drives them. Without
    Go it fails and says to install it.
  - **Nothing leaves the Mac.** Headscale's HTTP, gRPC, metrics and embedded DERP/STUN listen on
    free loopback ports, written to the fixture file, and the DERP map has no other region. The
    nodes reach that DERP over HTTP (`TS_DEBUG_USE_DERP_HTTP`) and then each other directly over
    127.0.0.1, and log uploads are off.
  - **The user's own Tailscale is never reached.** Every call names its node's socket, which
    also turns off the CLI's search for the macOS app's port. A node must answer as a fresh
    daemon before login, and its control URL must be the fixture's after it. `TS_LOGS_DIR` keeps
    `tailscaled` out of `/Library/Tailscale`. The build leaves out DNS (`ts_omit_dns`), because
    at start a macOS `tailscaled` cleans up the system DNS that a root `tailscaled` would have set.
  - **No daemon outlives `up`.** `up` holds everything in the foreground until Ctrl-C, SIGTERM,
    SIGHUP or `down`. Each daemon runs under a guard, a hidden xtask subcommand in its own
    process group, which stops the daemon when `up`'s end of a pipe closes. That holds even when
    `up` is killed outright (checked with `kill -9`). `down` also stops any process whose command
    line names the run dir, and nothing else.
  - **Tests.** A test reads the JSON at `SLOPTY_TAILNET_FIXTURE` and skips through
    `slopty_testkit::live::skip` when it is unset or the file is gone
    (`crates/slopty-tailnet/tests/fixture.rs`). The first test proves that the worker's
    `LocalApi::status` lists `ci` online with its tag, that `whois` from both of `ci`'s addresses
    yields exactly the granted roles, and that nothing is granted the other way. It also holds
    the fixture's capability equal to `policy::CAP`. The grant roles, ping path and IPN bus
    items of `.research/tailscale-services-2026-09-30.md` build on this fixture.

- ✅ **The encode engines and the GPU are measured from IOReport, without root** (2026-09-30,
  M1 Max, macOS 27.0). `powermetrics` needs root, and a measurement run from a test has none,
  so until now which engine a session ran on, and what the GPU did, were guessed from timings.
  `slopty_testkit::soc` subscribes to the IOReport channels `powermetrics` reads, which any
  process may read through `libIOReport.dylib`, and reports a span: each encode engine's
  interrupts (`Interrupt Statistics (by index)`, `ave0 0` and `ave1 0`, about four a coded
  frame) and DRAM traffic (`AMC Stats`, `VENC0` and `VENC1`), and the GPU's active share
  (`GPU Stats`, `GPUPH` out of `OFF`) and energy (`Energy Model`, `GPU Energy`). The counters are
  the whole Mac's, so a measurement reads an idle second first and reports what it added.
  - Left out because they say nothing here: the encode block's energy (`Energy Model`, `AVE0`
    reads 0 mJ over any span) and its power state (`SoC Stats`, `AVEMSR`, `ACT` idle or not).
  - IOReport is private: no SDK header declares it, so the signatures are the ones root-free
    monitors (macmon) call, and the reader lives in the test kit only, never in a shipped
    binary. Off macOS it opens nothing. Test: `a_span_reads_back`. First use:
    MEASUREMENTS "large streams on the encode engines", where it showed a session never spans
    two engines and put another app's encoder on `ave1` while the Mac was otherwise idle.

- ✅ **Every daemon, CLI and agent a test starts runs in a clean environment** (2026-10-01).
  - A test runs inside the developer's shell. That shell may hold their Claude Code settings
    and credentials, API keys, the Slopty terminal it runs in (`SLOPTY_SESSION`,
    `SLOPTY_SESSION_TOKEN`), and a `PATH` that finds the real `claude`. Everything a daemon
    inherits reaches the shells and agents it starts, and the stub `claude` recorded such
    variables, some of them keys.
  - `slopty_testkit::env::scrub` clears the environment and keeps only what the machine needs:
    the locale, `TERM`, the user, `TMPDIR`, and what the Rust toolchain reads (logging,
    backtraces, coverage). `PATH` becomes the system's own directories, with no program the
    person installed, and `HOME` a directory of the test's own. A test sets anything beyond
    that itself.
  - It is the one place every harness starts from: `slopty-worker`'s `e2e`, `handoff` and
    `server_link`, the CLI's tests, ptyd's, and `slopty-e2e`'s harness for its daemons and
    CLI. The app and the helper windows are left as they were.
  - Tests: `a_scrubbed_process_sees_only_the_kept_variables_and_its_own_home`
    (`slopty-testkit`), and
    `an_agent_the_worker_starts_inherits_nothing_of_the_test_s_environment` (`server_link`).
    The stub records each variable's name with a digest of its value (never the value), and the
    test fails on any variable of its own that reached the agent unchanged. Before the scrub it
    listed everything beyond the kept set, API keys and every `CARGO_*` variable among them.
- ✅ **A soak's extra worker thread was the blocking pool, kept warm by an idle tick**
  (2026-10-01, MEASUREMENTS "the worker's blocking pool after a soak"). Two soaks in eleven
  failed with the worker holding one thread more after the load than before (18 → 19).
  It was no leak. By name and stack the worker held 10 tokio workers, the main thread, its
  daemon thread, one or two system dispatch threads, and 3 to 5 idle threads of tokio's
  blocking pool. No thread was busy, and no session's thread outlived its session. The pool
  grows to the most blocking tasks run at once, and tokio ends an idle thread after 10 s. But
  the agents tick (`apps/slopty-worker/src/agents.rs`, every 750 ms) sent an empty task to
  the pool even with no agent working. An lldb breakpoint on `spawn_blocking` in an idle worker
  caught 32 calls in 8 s, all from `keep_awake`. Each call woke one idle thread, so none
  reached the keep-alive and the pool kept its high-water mark for good. A load that ran one
  more blocking task at once than the fill had raised the mark by one, which the baseline, 12 s
  after the fill, did not have.
  - **The fix is in the worker.** `agents::sample` goes to the blocking pool only when a turn is
    paused, since only a paused turn's commands have their processor time read. Otherwise it
    maps the agents in place. An idle worker now hands the pool nothing, and the pool empties
    10 s after its last task. Test: `only_a_paused_turn_takes_the_blocking_pool` (a
    current-thread runtime starts a thread only for the pool: none for no agent or a working
    one, one for a paused turn; it fails when every tick takes the pool).
  - **The soak's check stays strict, and it now names what grew.** No allowance was added,
    since after the fix nothing is left to allow for. At the baseline and at the end the soak
    reads each daemon's threads by name (`PROC_PIDLISTTHREADS`, then `PROC_PIDTHREADINFO`), with
    a session's `session-<uuid>` counted as `session-*` and a thread without a name as
    `(unnamed)`. A failure reads, for example, `worker: 19 threads after the load, 18 before
    (+1 tokio-rt-worker)`, and `summary.json` keeps both lists. The worker's tokio workers are a
    fixed number, so a grown `tokio-rt-worker` count after the settle means blocking work is
    still arriving. Tests: `a_process_s_threads_read_by_name` and
    `a_thread_named_for_a_session_counts_under_its_stem` (`xtask`).
  - *Not settled here.* The system's dispatch threads (`(unnamed)`) are the kernel's to start
    and end. They held level from baseline to end in every soak here, but a run that caught one
    in between would fail and name it. In three soaks in a row the server held one thread more
    after the load (11 → 12). That is the server's to look at, and the named check will say
    which thread.

- ✅ **The soak judges footprint growth by Theil–Sen, over 900 s of load after its first 300**
  (2026-10-01, MEASUREMENTS "the soak's footprint slope against its window"). Short soaks read
  ptyd growing by up to 1 980 KiB/min, the server by up to 204 and the worker by 930 once. None
  of it was a leak or a store still filling. `leaks` found nothing of ours in any daemon.
  `heap` every minute of a 15-minute load held ptyd's live heap at about 630 blocks and the
  worker's at about 51 000, each flat. Call trees under `MallocStackLogging`, 60 s and 620 s
  into a load, named no allocating site that grew. The differences were the terminals open at
  that moment (ghostty pages in the worker, a session's bounded ring in ptyd) and an 8 KiB
  sleep assertion. The footprint moves because the allocator takes and returns regions in
  steps of 16 KiB to 2.5 MiB, because ptyd wanders by 1 MiB, and because the worker holds 0 to
  35 MiB of `IOSurface` as streams open and close. A least-squares line follows every step and
  spike, so the shorter the window, the larger the slope it reads.
  - **With a stream lane the worker's floor climbs, then holds.** Its small-allocation pages
    (`footprint`) rose from 10–15 MiB to 24–26 MiB over the fill and the first 400 s of load.
    They then held for the remaining 25 minutes of two 30-minute soaks, while the live heap did
    not grow. That is the allocator keeping the high-water mark of the most streams and
    sessions alive at once, so the slope leaves out the load's first 300 s.
  - **The judgement is now robust, and the budget is unchanged at 64 KiB/min.** The slope is the
    Theil–Sen estimate, the median of the slopes between every pair of samples. A spike spans
    almost no pairs, and a single step spans more than half of them only within `1/(2√n)` of
    the window's middle, while growth spread over the window moves every pair. Across seven
    soaks, most of them run three at once, the largest slope of any window starting 300 s or
    more into the load was 179 over 600 s (the worker with a stream lane), and 47 over 900 s
    and 32 over 1 200 s in the two 30-minute soaks. Least squares read up to 189 over 600 s on quieter loads. So the slope is judged
    only once the load past its first 300 s spans 900 s, and the default load is 1 200 s, the
    nightly's. A shorter `--seconds` still prints the slope, marked as not judged. Tests:
    `a_climb_spikes_and_a_held_step_read_as_no_growth_and_growth_reads_its_rate` (6 MiB in the
    first 300 s, 17 MiB spikes and 1 MiB held read under 1 KiB/min; the climb judged reads over
    the budget; 128 KiB/min of growth under it all reads within a tenth) and
    `the_slope_is_the_median_of_the_pairwise_slopes` (`xtask`).
  - **The settle is 20 s.** Tokio ends a blocking thread after 10 s idle, and those exits can
    hand the system's dispatch threads work. Dispatch threads end about 5 s after they go idle:
    eight started at once in a probe, seven had ended by 5.5 s, and one stays as the pool's.
    At 12 s, two soaks of seven ended with one more unnamed thread than at the baseline, and
    the baseline's own unnamed thread was the dispatch pool's (`start_wqthread`). At 20 s a
    16-minute soak alone held 13 → 13 threads, but two of three soaks run at once still each
    caught one. So the settle covers a soak run alone, and a machine loaded that hard can
    still trip it. *Not settled:* naming a dispatch thread apart from one of ours, so the check
    can wait for those alone.
  - **Staging the daemons no longer kills another soak.** The soak copied its binaries into
    `target/deep/soak/bin` in place and then signed them. A file written in place keeps its
    inode, so a second soak started beside a first rewrote the code the first was running, and
    the first's CLI was killed (`SIGKILL`) mid-fill in two runs. Each binary is now copied
    beside the old one, signed, and renamed over it, which leaves the running file whole. Test:
    `a_staged_binary_replaces_the_path_and_leaves_the_running_file_whole`.
  - **A leak's verdict names it.** `leaks` writes `1 leak for` in the singular, and the soak
    looked only for `leaks for`, so a single leak failed as `no verdict line`. The verdict now
    reads the `total leaked bytes` line and adds the first root leak, skipping the stack
    headers `MallocStackLogging` adds. Test:
    `a_leaks_verdict_reads_in_the_singular_and_names_the_root`.
  - *Not settled here: three `leaks` reports that are no growth.* (1) With a stream lane, the
    default, every soak's worker ends with `1 leak for 128 total leaked bytes`, a root
    `NSPasteboard`. Its stack runs from `clip::watch` on the blocking pool through
    `MacBoard::change_count` to `+[NSPasteboard _pasteboardWithName:]`. The soak gives the
    worker a named pasteboard, and `MacBoard` looks it up by name on every call. A standalone
    probe reproduces it outside Slopty: the first lookup of a named pasteboard off the main
    thread leaves one such object per name (50 names, 50 objects), and every later lookup
    returns that same object. The general pasteboard, a first lookup on the main thread, or a
    strong reference kept by the caller each read 0. The fix belongs to `slopty-input`'s
    `MacBoard`: keep the named board's `Retained<NSPasteboard>` rather than looking it up every
    50 ms. (2) One 30-minute soak's worker reported 5.5 MiB in 41 546 blocks, rooted in one
    1.4 MiB block. That block is the idempotency ledger's `HashMap` table. With 4 096 keys and
    their churn, hashbrown grew it to 16 384 buckets, and the map points into its middle, at
    the control bytes. `leaks` does not follow such a pointer for a block that large. In a
    probe, a held `HashMap` of 4 096 entries read 0 leaks, and one of 14 000 (1.5 MiB) read as
    a root leak. The table is bounded, and a run whose memory happens to hold its start
    address reads clean. The fix belongs to the ledger (`orchestrate/idempotency.rs`): a
    structure without an interior-pointer root, or a table that stays below the size. (3) Once,
    under `--stacks`, 32 bytes leaked inside HIToolbox's input-source cache
    (`InitializeInputSourceCache`, reached from `slopty_platform::input_source::current`).
    That one is Apple's.

- ✅ **A test that sets a process-wide fake holds it until it ends** (2026-10-01). Two worker
  tests, `screens::made::a_made_display_is_streamed_and_released_with_the_stream` and
  `…an_outgrown_display_is_remade_and_the_stream_follows_it`, failed under `cargo test` on this
  Mac and passed in the gate. They are neither ignored nor gated, and they need no Screen
  Recording grant and no `CGVirtualDisplay`: the whole platform is the `screens::fake` one. The
  cause was a race between tests. The four `made` tests each wrote the displays the fake
  enumeration lists into one process-wide static and then read them back through the stream, so
  under `cargo test`, whose tests run as threads of one process, one test's listing replaced
  another's before it was read. nextest runs every test as its own process, which hid it. Run
  one at a time they passed three runs in three, and run together they failed three in three.
  - **The fix is in the fake.** `fake::listing(displays)` takes an async lock and returns its
    guard, and the listing is set only through it, so a test holds its listing for as long as it
    runs. The four `made` tests now pass together five runs in five, and the worker's binary
    passes all 35 tests three runs in three.
  - **Their unwraps say what went wrong.** An empty `unwrap` in these tests now names what did
    not happen: the stream did not open, no display was made (the stream fell back to a
    physical one), the stream ended before the resize, no display event came within 5 s, or
    the serving task panicked.

- ✅ **A crate's integration tests stay one binary per file** (2026-10-01). Merging each crate's
  `tests/*.rs` into one binary would save about a CI-minute, and it would cost churn out of
  proportion. Rebuilding a crate's integration binaries against rebuilding only its smallest one
  prices one more binary at 1.5–6 s of CPU on this Mac: 4.4 s in `slopty-net` (8 binaries,
  49–71 s together), 1.5 s in `slopty-worker` (7, 32 s), 5.8 s in `slopty-proto` (5, 30 s), 3.6 s
  in `slopty-client` (5, 22 s), 1.4 s in `slopty-cli` (5, 31 s). Outside `slopty-e2e` there are
  74 integration binaries in 23 crates, so merging saves 51 links and 1.5–5 CPU-minutes, a minute
  or so of a three-core runner's 26-minute build. Most of the test build is each library compiled
  a second time for its unit tests, which merging leaves as it is. The price: every
  `binary(…)` filter becomes a `test(…)` path (`.config/nextest.toml`'s groups, timeouts and the
  gate's retry list, `nightly`'s proptest filter, `vm`'s `binary(inject)`, the e2e suites' and
  `tailnet`'s `--test` names), the crash tests that re-run their own binary by test name change
  their names, and the `.proptest-regressions` files move with their sources. MEASUREMENTS
  2026-10-01, "the tests lane on a hosted runner, minute by minute".
  - What was taken instead: `slopty-e2e`'s live targets, all `#[ignore]`d, build only with its
    `live` feature, which `cargo xtask e2e` and host clippy pass: twelve units and about 92 s of
    CPU out of every test build.

- ✅ **The deep checks run on GitHub Actions** (2026-10-01). Miri, the sanitizers, loom, `leaks`,
  the fuzz targets, the long property tests, the GPUI scheduler seeds, the feature matrix,
  coverage and mutation testing run in the `Deep` workflow: nightly, a job per check, mutation
  testing weekly. This Mac runs one check at a time, and only to prove a change. An audit of what
  had actually run before this showed almost nothing had. The nightly had run once, in part (its
  property tests, 2026-09-29), because its LaunchAgent was never installed. The `Deep` workflow
  had run three times and failed each in about 90 s, setting up sccache. The fuzz crate had not
  compiled since the stripes change, which nothing noticed because the gate checks only its
  formatting. A local run of the checks then drove this Mac's load to 101 beside a gate and the
  soaks, while someone worked on it over Parsec. The repository is public, so hosted minutes,
  macOS ones included, cost nothing.
  - **Which runner.** A check that builds crates calling Apple's frameworks (the sanitized
    crates, loom's CoreAudio ring, `leaks`, the whole-workspace checks) runs on `macos-26`. Miri
    and mutation testing read the pure crates, which build on Linux, so they run on Ubuntu, which
    has shorter queues. xtask itself had to build there: the input-source module and a `statfs`
    field are now macOS only.
  - **Loud.** Each job uploads its report. A failed night opens the "Deep checks failing" issue,
    or comments on it, and the next clean night closes it. A missed mutant is a finding to rank,
    not a failure, so the mutation jobs never fail the run.
  - **What stays here.** Metal validation needs a GPU, and a hosted runner's virtual Mac has
    none. The soak and the bench measure this machine. Locally, `cargo xtask nightly` refuses a
    full run without `--all-here`, and each check it runs gets `nice -n 19`, four build jobs and
    four test threads (MEASUREMENTS 2026-10-01, "a nightly that shares the Mac").
  - **The sanitizers' exclusions, each with its reason.** The testkit's instruction-count test
    reads retired instructions, which ThreadSanitizer's instrumentation multiplies
    (`TSAN_COUNTS_INSTRUCTIONS`). Under both sanitizers, 5 of `slopty-crash`'s 35 tests fail: in
    the instrumented build, the frame they look for by name resolves to the `FnOnce::call_once`
    shim of the thread's closure, in `function.rs`. That is a fragility of those tests, reported
    to their owner, not hidden. Under ThreadSanitizer one worker test timed out
    (`session_actor`, a viewer whose queue was dropped) and one PTY test hit the 120 s limit
    (`a_typed_ssh_goes_through_the_cli`). Neither raised a race report.
  - **Miri** runs through nextest with a 20-minute limit per test and without the
    allocation-counting binaries: run locally, `slopty-grid`'s `tests/allocs.rs` held a run for
    55 minutes under the interpreter. The audio ring stays loom's alone: it is safe atomics, and
    its one `unsafe` is the FFI render callback, which Miri cannot run.

- ✅ **Terminal output is fuzzed through the engine** (2026-10-01). A program's output is the
  widest input a peer controls. Any program in a session writes it, a remote host's included, and
  the worker's engine parses it with libghostty and turns it into frames. The `terminal` fuzz
  target drives a `GhosttyEngine` with a script of writes, resizes, viewers joining, scrollback
  pages and checkpoints. libghostty is built `ReleaseSafe`, so a fault in the parser traps
  instead of passing silently. Seeded from nine captures of real programs and a dictionary of VT
  tokens, five minutes reached 4 750 coverage points with no parser trap, no engine panic and no
  viewer diverging (MEASUREMENTS 2026-10-01, "the terminal fuzz target").
  - **The oracles.** Every viewer, applying the frames it was sent by absolute line, shows what a
    second engine fed the same bytes shows whole. Every frame comes back from the wire as it
    went. A scrollback page holds no more lines than asked. A checkpoint replayed into a fresh
    engine shows the same cells and cursor.
  - **What the checkpoint oracle holds a replay to.** Each line's cells as they draw, its
    prompt mark, its hyperlinks and its soft wrap, and the cursor. Only what draws nothing may
    differ: a cell never written comes back as a space, and a space's pen (a foreground colour,
    bold, blink) shows nothing. During a synchronized update (mode 2026) the check waits, since
    the replay holds its screen too.
  - **What it found, and where each was fixed.** A checkpoint lost a lot more than its cells.
    Each fix was made where the loss was. Most were in libghostty's VT formatter, fixed in our
    fork (aislopware/ghostty#1 and #2, carried through aislopware/libghostty-rs#1 and #2).
    None duplicates an open ghostty-org pull request.
    - Blanks between and after styled cells came back in the pen of the cell before them, so
      the gap a tab left between struck-through cells came back struck through. The formatter
      now closes the style before the spaces that stand for them. Regression:
      `fuzz/regressions/terminal/checkpoint-struck-blanks`.
    - Hyperlinks were written for HTML only. VT output now opens each with OSC 8, keeping its
      explicit id, and closes it where it ends.
    - No OSC 133 state at all, so every row came back as output. The formatter's new
      `semantic_prompt` option writes every row's prompt flag and every cell's content. The
      engine then gives the cursor back the content it writes with, and carries its own prompt
      starts, statuses and command blocks in an OSC of its own after each screen (`OSC 6973`,
      `slopty-engine`'s `ghostty/carried.rs`). Replaying a checkpoint counts none of the marks
      the formatter writes, so nothing waiting on a command's end is woken.
    - Soft wraps never came back, since the formatter ended every row with a newline. The engine
      now formats with `unwrap`, which leaves a wrapped row for the replay to wrap. Three gaps in
      that path are fixed in the formatter. A wrapped row followed by one with no text shifted
      every later row up by one. The semantic prompt state carries across a wrap, with a
      newline where a wrap would not bring the next row back as it is. Wraparound and insert
      mode were set before the contents, so a row never wrapped and text was inserted rather
      than written.
    - Trailing blank rows were left out, and the engine padded them back by counting the
      formatter's newlines, which a wrapped row no longer writes. The formatter's new
      `trailing_rows` option writes them.
    - On the alternate screen, the primary's character sets and pen were carried in. A DEC
      special-graphics designation (`ESC ( 0`) made the alternate screen's text come back as
      line drawing. The engine now resets the pen, the character sets and the cursor's content
      before the alternate screen's contents. Regression:
      `fuzz/regressions/terminal/checkpoint-lost-dec-graphics-glyph`.
    - Tab stops a program set were left out. They were dropped because the formatter once left
      the cursor at the last stop, and it now homes after them.
  - **What it found next, with every line held to the oracle.** A second round of fixes in the
    fork (aislopware/ghostty#3 and #4, carried through aislopware/libghostty-rs#3), and two in
    the engine.
    - A replay's wrap gives the new row the prompt flag the wrap does, not the one the row has.
      The formatter now sets both rows' flags right after the wrap. On a single column there
      is no column to step back to, so it prints that column again.
    - A row the replay wrapped into that held only a prompt flag was never entered. As the
      last row it was lost, and the newline after it took off the soft wrap before it. The
      formatter now enters it by its end. Regression: `checkpoint-lost-flagged-empty-row`.
    - A wrap that scrolls fills the new row with the pen's background (background colour
      erase), and the blanks the row ended in kept that colour. Such a row now ends with an
      erase under the default pen. Regression: `checkpoint-wrap-scroll-coloured-blanks`.
    - Two kinds of cell were written as plain blanks and lost. One holds only a background (an
      erase under a coloured pen). The other is empty under a hyperlink: a wide character that
      a single column or a right margin cannot hold prints as one. Regression:
      `checkpoint-lost-link-on-empty-cell`.
    - A resize without reflow that cut a wide character at the new edge left half of it.
    - The scrolling region belongs to the terminal, not to a screen, so the primary's region
      scrolled the alternate screen's rows away as they replayed. The engine resets it before
      them. Regression: `checkpoint-alt-screen-scrolled-by-primary-region`.
  - **Stale frames, found through the checkpoint.** The mirror's frames are the truth, so a
    frame that went stale shows up as a checkpoint that does not match.
    - libghostty left a row clean when a zero-width mark attached to the cell before it without
      clustering, and when the row's prompt flag changed (OSC 133, a newline or a wrap in a
      prompt, output at the first column). The render state copies only dirty rows, so it kept
      the old text or flag. Upstream lets the prompt flag go stale on purpose, since its
      renderer draws nothing of it, but the engine's marks are drawn. The fork now dirties a row
      on both, and its render state tests hold the flag to that. Regression:
      `frame-stale-prompt-flag`.
    - The engine drew a cell that holds only a background as default, since it read colours
      from cell styles alone. Such a cell has no style, so no row flag said a row held one, and
      a check of every cell put a keystroke's frame up by 4 to 5 %. The fork now flags the row
      (`Row.background`, read as `GHOSTTY_ROW_DATA_BACKGROUND`), and the engine reads those
      colours on a flagged row only (aislopware/ghostty#4).
    - `WRAPPED` was read from libghostty's `wrap_continuation`, a cache that a scrolling region,
      an inserted or deleted line and history eviction leave stale. It is now the row above's
      own wrap flag, which reflow and selection use. A line's `WRAPPED` therefore changes when
      only the row above it changes. A frame sends the clean row below a changed one again
      with only the flag changed, and a changed row whose row above is clean takes its wrap
      from its own line as last sent. A first version walked every row and looked the row
      above up in the grid, and cost a keystroke's frame about 1 %.
  - **Memory, not a leak.** Under AddressSanitizer the target's resident memory grows by about
    0.5 MB per execution. Plain `ReleaseFast` and `ReleaseSafe` builds of the same inputs stay
    flat at 8 to 12 MB, and LeakSanitizer reports no leak. The terminal target therefore gets an
    8 GiB RSS limit, the others 2 GiB, and every target a 2 GiB limit on any one allocation.

- ✅ **The tests lane counts pseudo-terminals while it runs; a refused `/dev/ptmx` is opened
  again** (2026-10-02). CI's tests lane twice had an open of `/dev/ptmx` refused with ENXIO,
  which read as all 511 pairs in use, and a `lsof` after the lane found none held. Counted
  every 200 ms while the lane ran, the tests held 25 at most (18 with a runner's three
  threads), and no process kept one past its test (`docs/MEASUREMENTS.md`, 2026-10-02). The
  refusal is XNU's: its table of pairs grows 16 at a time, and an open that finds it full
  fails when another pair is closed at that moment (`bsd/kern/tty_ptmx.c`). A freshly booted
  runner meets it, as a guest on a fresh boot did 12 and 14 times in 400 opens. So:
  - `slopty-pty` opens again, up to 16 times, an open refused with ENXIO, and only one still
    refused is reported as exhaustion. The guest then refused none.
  - The tests lane runs a sampler beside nextest (`xtask/src/ptys.rs`, also `cargo xtask ptys
    -- <command>`). It lists this user's processes and their character devices, counts the
    pairs held under the gate or by processes that left it with the run's
    `NEXTEST_WORKSPACE_ROOT`, and names each holder by the `NEXTEST_BINARY_ID` and
    `NEXTEST_TEST_NAME` it inherited, or by its parent's when it is one of Apple's programs,
    whose environment no other process may read. It opens one master after each count, whose
    minor, the lowest free, bounds what the count cannot see. The lane fails, naming the tests,
    on a process holding a pair 2 s after its test ended, on more than 128 at once (a quarter
    of the limit, five times the suite's peak), or on the system refusing an open 16 times in a
    row. The samples go beside the `JUnit` report, which CI keeps, in place of the `lsof` step.
  - The pairs are counted, never timed. No nextest test group bounds the PTY tests: none holds
    more than 16, and a group would only stretch the run.

- ✅ **A golden is held in words too** (2026-10-02). The pixel tolerance (0.2 %) cannot see a
  changed word: `workspace-navigator.png` kept "Workspace 1" in its title bar a day after the
  app said "e2e-worker" there, and passed. Every app golden now has a `.txt` beside its PNG:
  what the same frame says, a line per node of its accessibility tree in reading order (role,
  label, value, and the node with the keyboard), with no bounds, compared exactly. A changed
  word fails with the lines that changed, the golden's and the frame's (`similar`'s unified
  diff), under the same `--accept`, `--accept-all` and `--review` as the picture. A masked
  region's nodes are left out, since what a moving picture's header says moves with it.
  - **Cost.** The app reads the tree from one more frame of the same state, drawn with the tree
    asked for, right after the frame it renders: 0.7 to 1.7 ms a golden (25 to 60 nodes) over
    the ten goldens of `a_workspace_of_columns_in_both_themes`, against a render and PNG write
    of tens of milliseconds.
  - **What it cannot see.** Text that is in no node, for a screen reader or for this check:
    a plain run of text with no role. That is an accessibility gap to close where it is found,
    not a reason to read glyphs back.
  - **No machine in a golden.** A path in the run's scratch directory (the system temporary
    root, raw or resolved, and the directory the run made in it, whose name is random) is
    spelled `$SCRATCH` as the text is made (`snapshot::scrub_scratch`), so a file tile's
    `File $SCRATCH/project/main.rs` reads the same on every machine and in every run, and no
    user name or temporary path lands in a golden. A port after a loopback host is `$PORT`
    (`snapshot::scrub_ports`), since the tests' servers bind what the system gives them. Both
    are scrubbed in code, never by hand.
  - Tests: `snapshot::tests::a_changed_word_is_a_changed_line`, `a_masked_node_says_nothing`,
    `a_scratch_path_reads_the_same_everywhere`, `a_loopback_port_reads_the_same_in_every_run`.

- ✅ **A run of app tests keeps the binaries it started with** (2026-10-02). Tests in the app
  suite failed at random with "slopty-app did not create app.sock within 30s"
  (`a_folder_tile_browses_the_worker_and_opens_a_file_beside_it`,
  `a_page_on_a_host_only_the_worker_names_loads_through_its_proxy`,
  `a_multi_item_copy_arrives_as_its_items`, the thread's frame-time run), always in a row and
  while other builds ran on the machine. Not a slow machine: each of those apps logged
  "notifications off: not an app bundle", which only `notify::System::new` says, and only an app
  built without the `e2e` feature makes that one, since the self-test makes `notify::Memory`.
  Such an app never opens the test socket. `apps/slopty/tests/crash.rs` runs the app binary, so
  any build of the workspace's tests (a gate's, another session's `cargo nextest run`) builds
  `target/debug/slopty-app` again without the feature, and every test process staged
  whatever was there when it started. `target/debug/deps` held the evidence: a plain build of
  the app finished at 11:04:45, the thread's frame-time run failed at 11:07, and the e2e build
  that replaced it came at 11:17:48.
  - Under nextest each test is a process, so the run's first test pins the binaries it staged
    (`harness::pinned`): hard links to the staged copies in a directory named by
    `NEXTEST_RUN_ID`, which every later test of the run reads. Staging replaces a copy by
    renaming a new file over it, so a link keeps the file the run began with. A pin older than
    two hours is removed as a new one is made.
  - The pin alone left the gap between `cargo xtask e2e`'s app build and the run's first test,
    which holds the suites' own test builds: minutes on a busy machine, and a rerun of the
    thread's frame-time test lost its app there at 13:06. So the tests now start
    `slopty-app-e2e` (`harness::APP`), a second bin target of `apps/slopty` on the same
    `main.rs` with `required-features = ["e2e"]`: only a build with the self-test makes it,
    and a plain build of the workspace's tests never touches it.
  - Timeouts were not raised: 30 s already covers a cold start on a loaded machine many times.
  - Test: `harness::tests::a_run_keeps_the_binaries_its_first_test_pinned`.

- ✅ **A test waits on the event it means, and a hang is the runner's to time out**
  (2026-10-02). CI run 37019453215, on a change to the client's icons alone, failed six worker
  and server tests that pass here. None was a product fault; each waited on a clock where it
  meant an event, or held the machine to a number only a quiet machine keeps.
  - The processes the four thread tests started stalled for about 40 s together: a ptyd's
    `tic`, a shell's `stty`, a worker that never printed its address. Two of them failed their
    20 s step and the two that passed ended within 0.2 s of each other once the stall cleared,
    20 and 35 times their usual second. A per-step wall budget only moves where such a stall
    fails, so `apps/slopty-worker/tests/threads.rs` has none: each step waits for its message
    or file, a child that exits fails at once, and a test that never gets there is ended by
    nextest's slow-timeout (`.config/nextest.toml`), as `next_or_stopped` already leaves
    waiting on the machine to the runner. Holding a test's ptyd stopped for 25 s failed the
    old file at 20 s; the new one passes at 26 s.
  - A worker registers before it has looked for its agents, and placement reads that silence
    as "has not said yet whether claude is installed". The clone test placed its task on a
    worker that was merely online; it now waits for Claude Code in that worker's
    capabilities, as `fleet` did for the first. A `claude --version` one second slower
    reproduced the failure exactly.
  - The echo test typed at the attach, before the shell had started: the tty's own echo went
    into the first diff after the attach, which carries every row and so is never an echo, and
    the shell's reply came past the 50 ms window on a loaded runner. It now answers the
    shell's first `read`, waits for the shell to say it is reading again, and judges the frame
    after that Enter, the kernel's echo. A shell that starts 200 ms late reproduced the old
    failure.
  - The dropped search was judged by wall time against a walk timed earlier in another load.
    It is now judged by the process's CPU time, the work done, with the future dropped where
    it is polled: under 1 ms after the drop against 0.7–1.6 s for a whole walk here, and a
    search whose drop stopped nothing fails at once.
  - The stuck-submit test held the stream to exactly one give-up. A hosted virtual Mac's
    VideoToolbox can hold a replacement's turn past the patience too, and the beat rightly
    gives that up as well. The test now times each turn of the replacement sessions and allows
    one more give-up for each that stayed inside at least half the patience (a healthy turn
    takes milliseconds), with one session built for each. A replacement turn made to wait 2.6 s
    passes with three sessions; the same wait hidden from the timing fails.

- ✅ **A stand-in agent's transcript is named for its session** (2026-10-05). Claude Code writes
  `<session id>.jsonl`, and the worker takes a session's id from a hook's `session_id` or from
  the transcript's file name, whichever reaches the thread first. Every stand-in now names its
  transcript that way: the relay's and the stack's played hooks, the fake `claude`, which uses
  the `--session-id` Slopty pinned, and the server e2e. One named `agent.jsonl` made two ids
  for one agent. A held `PermissionRequest` landing before its status then began a thread under
  the file's name and ended it when the status named the other, so the through-server golden
  showed "Claude exited" in some runs and the message box in others.

- ✅ **A golden that shows a time pins the readouts' clock** (2026-10-05). The `agent-screen`
  golden drifted between 0.191 % and 0.207 % against its 0.200 % tolerance. Its working row
  shows how long the turn has run, counted from the transcript's fixed 2026-10-04 stamps to the
  wall clock, so it read "6h 38m" when the golden was taken and "11h 26m" a few hours later,
  with digits of other widths each hour. Every readout of a time (a turn's elapsed time, a
  record's stamp, a limit's reset, an author's age, a project's time at work, an agent's age in
  the navigator) now reads `slopty_ui::clock::now`. That is the system clock unless
  `Command::PinClock` pins it, and the test pins it a minute after its transcript's first record.
  What is kept or sent (a backup's time, a visit counted) still reads the system clock. The
  same pin holds the working marks still: under Reduce Motion a working mark and companion
  breathe in opacity by the spin clock, which left 130 pixels varying with the moment the frame
  was taken. Pinned, every mark shows the moment it stands upright and whole
  (`icons::PINNED_STEPS`) and wakes nothing, so `agent-screen` matches its golden to the pixel
  run after run. The same drift hid a real change: the composer's help button is gone, and every
  composer golden still drew it under the tolerance. They are taken again.

