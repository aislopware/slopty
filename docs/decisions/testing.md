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
  (gate `SLOPTY_IOS_E2E`): ptyd and hostd start on the Mac as for `e2e app`, the app is
  launched with `simctl launch` and `SIMCTL_CHILD_*` variables for its socket, data dir,
  `SLOPTY_PREDICT` and `SLOPTY_HARDWARE_KEYBOARD`, and binds the socket on the shared file system (a simulator process is a
  Mac process; the sandbox does not stop it). `Stack::launch_on_simulator` shares the daemon
  code with `launch`; shutdown sends `quit` and `simctl terminate`. The test pins the one
  behaviour that differs by screen: the host places a desktop-sized terminal and the client
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
  and a stale `slopty-hostd` answered `ProtocolVersion { host: 9 }` to a protocol-11 app: every
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

- ✅ **Playing an agent in the self-tests: hook JSON to hostd's control socket, never typed
  into the shell** (2026-09-05). hostd already takes `CtlRequest::Hook { session, payload }`
  on its control socket (what `slopty hook` relays), so `Stack::play_hook` writes the fixture
  transcript (`harness::TRANSCRIPT`, never a real `~/.claude/projects` file) under the run's
  temp dir and sends the payload there from the test process, with the session id read from
  the dump. A first version typed `printf … | slopty hook` into the shell under test and
  waited for the state to change: synthetic keys running a command in a shell and reading the
  result back is the surveillance-shaped pattern CLAUDE.md forbids, and it got that session
  flagged. Keys over the test socket drive the app's own UI only (⌘⇧L, the composer, the
  buttons); the one line the composer sends is a shell comment, so the shell echoes it and runs
  nothing. The same path drives the simulator, since hostd stays on the Mac.

- ✅ **The self-test plays the agent with a fake `claude` on ptyd's `PATH`, never by typing**
  (2026-09-05). `Stack::launch_with_fake_claude` gives ptyd and hostd a `HOME` and a `PATH` of
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
  hostd wrote Claude Code's settings with the relay and all 12 `HOOK_EVENTS`. The file asserted
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
