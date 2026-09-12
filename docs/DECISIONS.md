# Decisions

One entry per ruling. **Verified** means checked against a primary source (SDK header on this
machine, upstream source, crates.io, or our own measurement) on the date given. Claims inherited
from slop-desk are *not* trusted until verified here; where slop-desk was wrong it is noted.

Status: ✅ decided · 🔬 measure before relying on it · ⏸ deferred.

## Platform

- ✅ **Floor macOS 26.5 / iOS 26.5, Apple silicon only.** User decision 2026-09-04. No
  availability checks, no fallbacks for older OS.
- ✅ **Pure Rust.** Scripts are `xtask`. The only non-Rust files are the iOS `main.m` shim (a
  UIKit bootstrap that can't be avoided until `UIApplicationMain` is driven from Rust in the GPUI
  fork), Metal shaders, and the XcodeGen spec that xtask generates.

## UI

- ✅ **GPUI from a fork, pinned.** crates.io `gpui` 0.2.2 is 10 months stale (verified crates.io
  2026-09-04). Fork `aislopware/zed`, branch `slopty`, first base `801c087` — the commit gpui-kit
  0.6.0's `gpui-pre 0.3.1` snapshots (verified: crates.io description "snapshot of zed@801c087").
  Current base: `xtask/upstream.toml` (rebased 2026-09-05 onto upstream main `5a9b9558db` of
  2026-09-04, 35 commits; upstream touched gpui only in `Hitbox::is_hovered_at` and the test
  platform, so the five fork commits replayed without a conflict; fork head `d9980cc4ae`).
- ✅ **gpui-kit from a fork.** `longbridge/gpui-kit` v0.6.0 (renamed from gpui-component; Apache-2.0,
  LICENSE file verified). It depends on `gpui-pre` from crates.io; Cargo `[patch]` cannot rename
  packages (tested 2026-09-04: a `package =` key in `[patch]` is silently ignored), so a one-commit
  fork re-points its deps at our zed fork. Upstream sync = rebase that commit.
  Done 2026-09-04: `aislopware/zed` branch `slopty` = 801c087; `aislopware/gpui-kit` branch `slopty`
  = v0.6.0 + one commit (`gpui`, `gpui_platform`, `gpui_web`, `gpui_macros`, `reqwest_client`,
  `sum_tree` → git deps on the zed fork; `reqwest` → zed's `zed-reqwest` git fork).
  2026-09-05: rebased onto gpui-kit main `d59d1a16` (v0.6.0 + 32 commits, no newer tag; upstream
  still pins `gpui-pre 0.3.1` = zed 801c087, and nothing in gpui's API moved between 801c087 and
  our zed base, so the pair still agrees). Only `Cargo.lock` conflicted — it always will, since
  the fork commit rewrites every `gpui-pre-*` entry — and the resolution is mechanical: take
  upstream's lock, `cargo update -w` re-adds the git sources against the freshly pushed zed
  fork. `cargo xtask upstream sync` does exactly that and stops on any other conflicted file.
  2026-09-11: rebased onto zed main `d12e456be7` (2026-09-11, 64 commits) and gpui-kit main
  `84f57fdfcb` (2026-09-12, 57 commits, tag v0.6.1). One API break: upstream added
  `Platform::prevent_idle_sleep(&self, reason) -> Task<Result<ActivityGuard>>` (the macOS
  backend wraps `NSProcessInfo` activities); `gpui_ios` now implements it with UIKit's one
  process-wide `UIApplication.idleTimerDisabled` behind a hold count, released on the main
  queue since a guard may drop on any thread (fork commit `c4a311d623`). Sync gotcha fixed in
  `xtask`: when a build-check fails after the rebase, the fix lands on the already-rebased local
  branch, and the next `sync` used to refuse it as "diverged" (the fork head is no longer an
  ancestor). The check is now patch-based (`git cherry`): a local branch that carries every
  fork patch is "ahead", not diverged, and the sync continues from it.
  After the sync: gate green, app self-test 6/7 with `conversation.png` at 1.046 % vs the
  1 % tolerance — gpui-kit's markdown moved (inline code now rounded in the mono font, #2949;
  heading/list layout, #3038; descender clipping, #3023), so the golden was re-accepted;
  every structural assertion in that scenario passed. Also in this gpui-kit range: headless UI
  testing (#3005) and a mobile-application layer (#3045), both worth reading for the iOS app.
  2026-09-12: rebased onto zed main `a9cdfc9936` (2026-09-11, 6 commits; gpui-kit and
  libghostty-rs already at upstream head, `vendor/ghostty` at ghostty main). One conflict:
  upstream #64099 wrapped the headless Metal render paths in `objc2::rc::autoreleasepool`
  (temporary command buffers and pass descriptors accumulated across a long headless run),
  and our iOS commit picks `MTLStorageMode::Shared` for the offscreen target on unified memory
  (iOS has no `Managed`). Resolution: our storage-mode choice inside their pool (fork head
  `8ffbc6e145`). Second sync gotcha fixed in `xtask`: a conflict resolved by hand rewrites the
  patch, so `git cherry` could not vouch for it either and the re-run still said "diverged";
  the check now also accepts a local branch that sits on the new upstream and replays the
  fork's patches by subject, in order. Stable 1.98.1, every tool floor and the objc2 family
  were already at their latest; nightly rustfmt moved to 2026-09-11.
- ✅ **Upstream sync is `cargo xtask upstream check|sync`, run at least weekly** (user standing
  order 2026-09-05: gpui and gpui-kit move fast, keep pulling). `xtask/upstream.toml` records,
  per fork, the upstream and fork URLs, branches, the checkout under the main clone's
  `.research/` (shared by every worktree, cloned blobless on first use) and the `base` commit
  + date the fork was last rebased onto, so drift is visible in git history. `check` fetches
  both upstreams and prints commits and tags since each base and the date of the `Cargo.lock`
  pin; the gate prints a warning line (never a failure) when a base is older than 7 days.
  `sync` goes zed first, then gpui-kit (its lock resolves against the zed fork): fetch, refuse
  a dirty or mid-rebase checkout, `git rebase <upstream> <local branch>`, resolve a
  `Cargo.lock`-only conflict as above and stop with the file list on anything else (the
  checkout stays mid-rebase for a person or an agent to read upstream's change before choosing
  a side), build-check the fork (`cargo check -p gpui -p gpui_ios --target aarch64-apple-ios-sim`
  and the host `gpui` + `gpui_platform`; `gpui-kit` with `component,assets`), push with
  `--force-with-lease` against the fork head it fetched, then `cargo update -p gpui -p gpui-kit`
  in the workspace (one git source moves as a unit: every `aislopware/zed` package follows) and
  rewrite the base lines. What it does not do: the gate, the e2e runs and the DECISIONS entry —
  those stay with whoever ran it. Commits in the checkouts are signed like ours, so
  `SSH_AUTH_SOCK` must point at the signing agent before `sync`.
- ✅ **Forks are git dependencies, not submodules.** `gpui = { git = "…/aislopware/zed", rev = … }`
  pinned by rev; cargo caches the fetch. A zed submodule would put a multi-GB checkout in every
  clone and CI run for four crates we build. To hack on the fork: clone it next to this repo and
  add a `[patch."https://github.com/aislopware/zed"]` block locally (never committed).
- ✅ **iOS backend = zed PR #63068's `gpui_ios` crate on top of upstream `801c087`, with the
  PR's gpui-core edits dropped** (2026-09-04, fork branch `slopty`). The cherry-pick conflicted
  in 5 files because upstream had meanwhile absorbed everything the PR changed in `gpui` itself
  (`WindowInsets`/`on_insets_changed`, soft keyboard + `TextInputStateChange`, app lifecycle,
  memory warnings, `PlatformGestures`) and replaced its `TouchGestureArena` with
  `TouchGestureRecognizer` (#63373), which synthesises mouse events from taps so ordinary
  `on_click` handlers work under touch. Ruling: `git checkout HEAD` for every `crates/gpui/src`
  file, keep `gpui_ios`, `gpui_apple` (iOS SDK selection in `build.rs`, iOS-safe atlas and
  renderer) and `gpui_platform` (`cfg(target_os = "ios")` → `gpui_ios::current_platform`). Two
  fixes were needed for the newer API: `Platform::on_quit` takes `FnMut() -> bool`, and
  `TouchEvent` gained `predicted_position`, filled from
  `UIEvent.predictedTouchesForTouch:` for moved touches. Shaders: iOS builds use
  `gpui_apple/runtime_shaders` (compiled by `MTLDevice` at launch) so the pipeline has no
  Metal-toolchain dependency; the toolchain is installed on the build Mac anyway
  (`xcodebuild -downloadComponent MetalToolchain`). Reference implementation for behaviour
  only: `tanlethanh/zed@feat/gpui-mobile` (zedra), cloned under `.research/`.
- ✅ **Zero-copy video in GPUI.** `gpui::surface(CVPixelBuffer)` → `paint_surface` →
  `CVMetalTextureCache` in `gpui_apple/metal_renderer.rs` (verified by reading source). Upstream
  gates it to macOS and stubs `draw_surfaces` on iOS; fork commit `a7cc3f4` lifts every gate to
  `any(macos, ios)` (`core-video` 0.5 builds for both), so the phone paints the decoder's
  `CVPixelBuffer` the same zero-copy way. Checked on macOS, `aarch64-apple-ios-sim` and
  `aarch64-apple-ios`.
- ✅ **Hardware keyboards and pointers on iOS live in the fork, not the app** (2026-09-05,
  fork commit `dfc149e`). `gpui_ios` only implemented `UIKeyInput` (`insertText:` /
  `deleteBackward`), so an iPad with a Magic Keyboard had no arrows, escape, ⌃C or ⌘ chords:
  UIKit delivers physical keys as `pressesBegan:`/`pressesEnded:` on the responder chain and
  nothing handled them. Ruling: the metal view handles presses (and is first responder whenever
  the text input view is not, so canvas-level shortcuts work with nothing focused); the
  `UIKey` → `Keystroke` mapping (`hardware_keyboard.rs`) copies `gpui_macos`' rules — a shifted
  letter is `shift-a`, a shifted symbol is `!` with shift dropped, chords carry no `key_char`,
  `charactersIgnoringModifiers` falls back to the HID usage's ASCII name for non-Latin
  layouts — so Slopty's keymaps and `slopty_ui::keys` need nothing iOS-specific. Plain text
  while a text input is first responder is forwarded to `super` (UIKit's text system types it,
  IME and marked text intact); everything else is consumed so the text system does not also
  insert "\n" for Enter. UIKit does not auto-repeat presses (verified: no repeated
  `pressesBegan` for a held arrow), so the window runs its own repeat (400 ms / 50 ms, GCD
  main-queue timer, generation-checked so a release cancels it) and delivers `is_held`, which
  `TerminalView::key_down` already turns into repeat key events. Modifier keys (HID
  `0xE0..=0xE7`) update `Window::modifiers()` and emit `ModifiersChanged`; caps lock is tracked
  from `alphaShift`. Pointer: `UIHoverGestureRecognizer` → `MouseMove`,
  `UIPanGestureRecognizer` with `allowedScrollTypesMask = all` and `maximumNumberOfTouches = 0`
  (indirect scrolls only, so gpui core's touch recognizer keeps one-finger drags) →
  `ScrollWheel` with pixel deltas and phases; `UIApplicationSupportsIndirectInputEvents` in
  the plist so trackpad clicks are pointer touches. Bundle: `TARGETED_DEVICE_FAMILY 1,2`, all
  iPad orientations (`UIRequiresFullScreen` is deprecated and ignored from iOS 26, so nothing
  opts out of Split View / Stage Manager). Verified: the mapping's unit tests (letters
  keep shift, symbols drop it, chords have no `key_char`, named keys, modifiers ignored) run on
  the host through a shim crate because `gpui_ios` is `cfg(target_os = "ios")`; the app builds
  for `aarch64-apple-ios-sim` against the new pin. The app hides the key bar while
  `slopty_platform::hardware_keyboard_attached()` (GameController's coalesced keyboard, polled
  on the settings tick — GameController posts connect/disconnect notifications, but a poll that
  already runs costs nothing and needs no observer lifetime) is true; `key_bar_visible` is the
  pure rule with its table test. The simulator-driven layer exists: `cargo xtask e2e ios`
  drives the app in the simulator over its socket (`crates/slopty-e2e/tests/ios.rs`); the
  UIKit delivery has its own injection layer (next ruling).
- ✅ **The simulator self-test delivers at the UIKit boundary, from descriptions (2026-09-06,
  fork commit `2360f99d`).** `Command::Keys` / `Click` enter GPUI's dispatch, so nothing in
  the fork's UIKit plumbing (`pressesBegan:` on the metal view, `touchesBegan:` phases, the
  pinch recognizer, `insertText:` / `deleteBackward` on the text input view) was under test.
  UIKit's objects cannot be made: `UIPress`, `UITouch` and `UIKey` have no public
  initialiser, subclassing them would still leave `UIEvent` private and the touch phase
  read-only, and posting through `UIApplication.sendEvent:` needs a `UIEvent` nobody can
  construct. Ruling: the callbacks are split at the point where they have read their object
  into plain values (`UiKey` = usage + flags + `characters` + `charactersIgnoringModifiers`;
  touch id + `UITouchPhase` + point; recognizer state + scale + point; a string) and hand
  those to one delivery function per kind (`handle_hardware_key`, `deliver_touch`,
  `deliver_pinch`, `insert_text`, `handle_delete_backward`); `gpui_ios::inject(DescribedInput)`
  (fork feature `test-support`, so the bundle has none of it) enters the same functions from
  a *description*. Everything after the unpacking is the phone's real path: the modifier
  state machine, the key repeat, `is_plain_text` deciding whether the text system types the
  key (the injector then plays UIKit's part and calls `insert_text`, which is what the
  responder chain would do), gpui core's touch recognizer, the canvas's pinch handler. The
  unpacking itself is covered by unit tests of the pure mappings, which moved out of
  `cfg(target_os = "ios")` so they run on the host (`cargo test -p gpui_ios` in the fork):
  HID usage + flags → `Keystroke`, raw `UITouchPhase` / `UIGestureRecognizerState` →
  `TouchPhase`, and the US-layout stand-in that fills `UIKey.characters` for a described
  press (a described event has no layout engine; the stand-in is what a US keyboard reports,
  Control folded to the C0 code, Command emptying `characters`). The socket grew
  `UiKeyPress { usage, modifiers, phase }`, `UiTouch { touches, phase }` (a whole
  `touches…:withEvent:` set), `UiPinch { scale, x, y, phase }`, `UiInsertText`,
  `UiDeleteBackward`; the app hands them to `inject` from its async task with no window
  leased, because the delivery re-enters GPUI's dispatch the way a UIKit callback does
  (inside `update_window` it would find the window taken); macOS answers an error. The dump
  stays the only truth (rows, focus, zoom, bounds, a11y). Verified on the iPhone and iPad
  simulators (`crates/slopty-e2e/tests/ios_uikit.rs`): plain presses are typed by the text
  system and named ones consumed (`cat -v` shows `^[`, `^[[A`, `^[[C`, `^C`), ⌘⇧L from a
  press toggles the conversation view, a cancelled press releases the chord, with two shells
  fitted (⌘N, ⌘1) a tap on the first activates it, a pan with a second resting finger moves
  the camera by the first finger's travel on its locked axis and flings nothing after a
  stopped finger, a pinch multiplies the zoom by the product of its steps about the point
  under the fingers, a `UIKeyInput` composition (`a`, delete, `â`) leaves exactly `â`
  in `cat -v`, and the key bar's Escape, Up arrow and ⌃ buttons work when tapped at their
  a11y bounds. Found on the way: (1) the real `insertText:` path held the input handler's
  `RefCell` borrow across `replace_text_in_range`, which re-enters GPUI, whose
  `take_input_handler` borrows the same cell — every soft-keyboard character would have
  panicked the phone the moment a terminal took the keyboard; the first `UiInsertText` hit it
  and the fork now takes the handler (and the input callback) out of the cell for the call,
  as the macOS backend does; (2) a real two-finger drag on the device belongs to
  `UIPinchGestureRecognizer` (`cancelsTouchesInView`), whose translation the canvas ignores,
  so two fingers zoom and never pan; one finger pans; (3) a tap on bare canvas cannot leave
  the keyboard on the canvas: `keep_focus_rendered` hands it back to the active terminal on
  the next frame, so the test asserts activation (the headless twins,
  `a_finger_tap_activates_what_it_lands_on` and
  `a_finger_pan_on_bare_canvas_moves_the_content_with_the_finger`, go through gpui's touch
  recognizer); (4) a finger that keeps reporting after it stops (`UITouchPhaseStationary`
  events, which the fork maps to `Moved`) makes gpui core's quadratic velocity fit bend
  backwards and fling the content the wrong way; a real still finger sends nothing, and the
  40 ms silence is what the recognizer reads as "stopped", so the driver's `ui_pan` and the
  finger test rest 100 ms before lifting instead of sending stationary reports (chosen over
  changing the fit in gpui core: the device never produces the input that trips it);
  (5) the US stand-in combined Caps Lock and Shift with OR; a keyboard reverses one with the
  other on letters (`capslock-shift-a` is `a`), fixed with its unit test in fork `2360f99d`.
  Not modelled: `UITextInput` marked text (the fork's text input
  view is `UIKeyInput` only, so iOS composes by delete + insert, which is what the test
  plays), touch force and prediction (a described touch has none), non-US layouts.
- ✅ **iOS link and launch quirks (verified on the iOS 26.5 simulator, 2026-09-04).** Two extra
  things beyond the framework list: `Network.framework` (iroh's `netdev` uses `nw_path_monitor`
  on iOS), and stand-ins for three CGL symbols (`CGLErrorString`, `CGLGetCurrentContext`,
  `CGLTexImageIOSurface2D`) that the `io-surface` crate references from `bind_to_gl_texture`.
  `-Wl,-U,<sym>` lets the link pass but dyld's chained fixups bind at load and the app dies
  with "symbol not found in flat namespace", so `apps/slopty-ios` defines them as `extern "C"`
  functions that `unreachable!()`; nothing on iOS calls them. Dead-code stripping does not
  remove the references. `gpui-kit` widgets default to the light theme: the app forces
  `ThemeMode::Dark`. Phones have no CLI, so `slopty-app` shows a pairing panel (ticket field,
  "Paste & pair" reading `UIPasteboard`, which iOS gates behind a paste-permission alert) and
  the connect loop waits on it; the same panel serves macOS first runs. A phone joining a
  desktop-made canvas would open onto empty space (items sit at desktop coordinates), so the
  first snapshot schedules `fit_when_painted`. Safe area / keyboard come from the fork's new
  `Window::insets()` (`a140f32`). Touch: gpui core's portable recognizer turns one finger into
  taps / pans (scroll events) but has no pinch and ignores a second finger, so a pinch on the
  simulator moved the item instead of zooming; the fork adds a native
  `UIPinchGestureRecognizer` on the Metal view that emits GPUI `PinchEvent`s with incremental
  deltas, the same event the macOS trackpad produces, so the canvas's `capture_pinch` handler
  serves both. Soft keyboard: iOS raises it only for a focused element that registered a text
  input handler, so `TerminalView` implements `EntityInputHandler` (empty ranges, no
  composition preview; committed text goes to the PTY as `TermRequest::Raw`) and
  `TerminalElement::paint` registers it; keys still arrive as key events on both platforms
  (gpui_ios turns `insertText:` into `KeyDown`). Items created from this client are
  `Camera::reveal`ed on the next frame (pan if they fit, else zoom out no further than
  `CARD_ZOOM` and anchor top-left), because the host's `free_slot` places them to the right of
  everything, off a phone's screen.
- ✅ **Mouse selection, ⌘C, ⌘V.** Left-drag selects cells; the selection is stored as
  absolute line indices (`Selection { anchor, head }`, both inclusive) so it survives scrolling
  and is painted as a quad under the text from the same prepaint pass. When the program has
  mouse tracking on, clicks are reported to it instead and ⇧-drag forces a selection (what
  every terminal does). ⌘C copies the selected cells with trailing blanks trimmed per line
  (lines missing from the scrollback cache copy as empty); ⌘V sends the clipboard as
  `TermRequest::Paste`, which the host brackets when the program asked for it. Any key, a
  resize or an epoch change (reflow, alt screen) clears the selection. Bindings live in the
  "Terminal" key context (`slopty_ui::terminal::key_bindings`). Double-click selects the word
  (run of non-blank cells), triple-click the line. On touch a plain drag pans the canvas (GPUI
  recognises it as a scroll), so the terminal element claims GPUI's *long press* instead
  (`window.prevent_default()` at `Started`): hold to select the word under the finger, keep
  holding and move to extend, as iOS text views do.

- ✅ **Input methods in terminals.** In the fork, `prefers_ime_for_printable_keys` follows
  `accepts_text_input` (true for `TerminalView`), so with the input handler installed macOS
  routes printable keys through the active input method. `TerminalView` keeps the marked text
  (`replace_and_mark_text_in_range`/`unmark_text`), the element draws it underlined at the
  cursor over the host's cells and hides the block cursor meanwhile (what Terminal.app does),
  and the commit arrives through `replace_text_in_range` as raw bytes. Covered by the GPUI test
  `composition_is_previewed_then_committed_as_raw_bytes` (gpui `test-support`). A live Telex
  run was not possible from the automation session (see `cargo xtask ime` below).
- ✅ **`cargo xtask ime [id] [--all]`** lists/selects macOS input sources through HIToolbox's
  `TISSelectInputSource` (Carbon), for input-method testing. Caveat found 2026-09-05: the
  agent's shell runs in a `Background` launchd session (`launchctl managername`), where TIS
  refuses to select (`paramErr -50`) and synthetic ⌃Space never reaches the hotkey; the tool
  works from a real Terminal in the Aqua session.
- ✅ **Phone-sized terminals.** The host places every new terminal at `TERMINAL_SIZE`
  (720×440). When the item is one *we* opened, `CanvasView::fit_to_viewport` immediately
  proposes a `Place` that clamps it to the viewport minus `GAP` at zoom 1, so a phone gets a
  phone-wide terminal and, being its driver, a PTY of that width; desktop viewports exceed the
  default and are untouched. Terminals opened elsewhere are still viewed zoomed out; the
  driver/viewer hand-off is the "take" pill below.
  Verified on the iOS 26.5 simulator 2026-09-05: "+ shell" on the phone opens a 43×22 PTY
  shown at 100 % across the phone's width. Landscape (both directions in the spec) rotates the
  GPUI window with the keyboard; nothing else was needed.
- ✅ **The opener drives.** Every client attaches to a new terminal the moment the
  `SessionOpened` broadcast reaches it, so the phone could become driver of a shell the desktop
  just opened (observed 2026-09-05: a fresh desktop shell wearing the "take" pill). hostd calls
  `SessionHandle::reserve_driver(opener)` before broadcasting; the first attach no longer
  decides. Test `reserved_driver_beats_the_first_attach`.
- ✅ **Driver hand-off is explicit: the "take" pill.** A terminal whose PTY follows another
  client's size shows "take" in its title bar (`TerminalState::driving` is false).
  `CanvasView::take_over` sizes the item for this viewport (clamped between the default
  `TERMINAL_SIZE` and the viewport minus `GAP`), sends `TermRequest::Drive`, focuses and
  reveals it. Item geometry is shared canvas state, so the other client sees the terminal
  shrink or grow: that *is* the signal that someone else drives it, and their own "take"
  brings it back. Rejected: tmux-style "latest input drives" (a phone typing one command would
  keep shrinking the desktop's terminal) and per-client geometry (a second document model).

- ✅ **iOS key bar.** The soft keyboard has no esc/tab/ctrl/arrows, so the iOS app
  (`KEY_BAR = cfg!(target_os = "ios")`) draws a 40 pt row above the keyboard with esc, tab, ⌃,
  ←↑↓→, `-`, `/`, `|`, `~`. Keys go through `TerminalView::press`, the same path as hardware
  key events; ⌃ is *sticky*: it arms `set_sticky_control` and the next key (bar or typed
  character, including text arriving through `replace_text_in_range`) is sent with the control
  modifier, then it disarms. Covered by `sticky_control_applies_to_the_next_key_only`; verified
  on the simulator 2026-09-05 (⌃ + `c` interrupts `sleep 30`, arrows recall history). The bar
  only renders while a terminal is the canvas's active item. Its last key is the clipboard:
  "copy" while the terminal has a selection (long-press selects; a plain drag pans the
  canvas), "paste" otherwise, since the phone has no ⌘C/⌘V.
  A remote window gets its own bar (`SCREEN_BAR_KEYS`, `CanvasView::active_key_target`):
  esc, tab, sticky ⌃ and ⌘, arrows, `/`, copy, paste. `ScreenView` implements
  `EntityInputHandler` too, so the soft keyboard rises over a window; each committed character
  becomes a press + release `ScreenInput::Key` carrying the character as `text` (a modified key
  carries none, or the host would insert the letter as well as run the chord). Composition
  (marked text) is held and only the committed string is sent. Windows on a phone were
  view-and-touch only before this (2026-09-05). Found on the way: a press on a window never
  made it the *active* item, because `ScreenView::mouse_down` stops propagation (so the canvas
  does not pan) before the item container's activate handler runs; the view now emits
  `ScreenViewEvent::Pressed` and the canvas activates from that. The workspace also observes
  the canvas entity, since the key bar follows the active item and nothing else re-rendered
  the chrome when it changed. A long press on a window is a right click on the host, which
  exposed a second host bug: a right click posted with `CGEventPostToPid` reaches
  `rightMouseDown` but AppKit's context-menu tracking never opens the menu (Ghostty, macOS
  26.5; a real click opens it). Right-button events now go through the HID tap even for
  window streams — the host pointer moves for those, the price of a menu — and Ghostty's
  menu opens from the Mac app and from a phone long press (verified 2026-09-05).
- ✅ **Stream stats overlay** (2026-09-05). Parsec's overlay is how people judge a remote
  desktop, so ⌘⇧I draws one per window from the client's own counters (`ScreenStats` gained
  `bytes`; rates are deltas over ~1 s, computed in `ScreenView::hud_text` on render since a
  window re-renders on every frame anyway). Global, not per window, and it carries over to
  windows opened later. First reading on loopback: a 450×250 @0.50 idle Ghostty window ran
  54 fps at 0.11 Mb/s, RTT 1.7 ms, zero loss.

- ✅ **Notes are shared text, last writer wins.** `ItemKind::Note { text }` was already in the
  document; the client now edits it in place with gpui-kit's `TextareaState` (`NoteView`).
  Edits go to the host as an `Upsert` of the whole item after a 400 ms typing pause and on
  blur; a remote change is applied only while this client is not editing. No merge: notes are
  short and the canvas has one author at a time in practice. ⌘⇧N / "+ note" places a 320×240
  note in the next free slot and reveals + focuses it at once: the document applies our op
  optimistically, and keying that off the host's echo failed on the phone, where a frame
  rendered (and created the editor) before the echo arrived. Zoomed-out cards show the first
  non-empty line. Verified on macOS and the iOS simulator 2026-09-05 (text persisted in
  `canvas.json`).

- ✅ **Minimap is a painted overlay, not elements.** A 160×100 box in the bottom-right
  corner (`render_minimap`) fits the union of every item rect and the viewport, paints each
  item as a block (`fill`, accent for the active one) and the viewport as an `outline`, all in
  one GPUI `canvas` element: zero layout cost per item, and it scales to hundreds of items.
  Mouse-down/drag inside it centres the camera on the pointer (`Drag::Minimap`); the box's
  mapping from the last prepaint is kept for hit-testing. Hidden while the canvas is empty.
  Verified on macOS 2026-09-05.

- ✅ **Continuous redraw model.** GPUI is reactive; video surfaces call
  `Window::request_animation_frame()` every frame, as zed's GIF and LiveKit views do.
- ✅ **Fonts are bundled, never system-resolved.** JetBrains Mono 2.304 (OFL) + Symbols Nerd Font
  Mono (MIT) ship inside `slopty-ui` and are registered with `TextSystem::add_fonts` at startup;
  every run carries a `FontFallbacks` chain (symbols → Menlo → Apple Color Emoji). Evidence
  2026-09-04: "JetBrains Mono" was not installed on the dev Mac and GPUI silently resolved a
  proportional face, producing broken cell alignment. Bundling also makes iOS identical.
- ✅ **Terminal zoom scales paint, not the grid.** `TerminalElement::zoom` derives cols×rows from
  the *unscaled* item rect and paints at `font_size × zoom`; zooming never resizes the PTY. Shaped
  rows are cached per (content, focus, font size).
- ✅ **Remote windows are `surface` elements, verified 2026-09-04.** `slopty-ui::screen::ScreenView`
  wraps the decoder's `CVPixelBuffer` with `core_video::CVPixelBuffer::wrap_under_get_rule` (own
  retain, so the frame `Arc` may drop first) and paints `gpui::surface(buffer).object_fit(Fill)`.
  A Ghostty window streamed from the host rendered pixel-exact through the Metal path with no
  copy. The view keeps only the newest frame; a pump task awaits the client `watch` channels for
  frames and cursor and calls `cx.notify()`.
- ✅ **Stream quality follows painted size.** `ScreenView::set_painted_width` (called from the
  canvas at paint time with the item's on-screen width in device pixels) quantises the wanted
  scale to quarter steps and sends `SetQuality` at most every 400 ms, so zooming the canvas
  re-encodes at a matching resolution instead of downscaling a full-size stream. Video paints
  at every zoom (a thumbnail costs a thumbnail-sized stream); only terminals collapse to cards
  below `CARD_ZOOM`. Changed 2026-09-04 when the phone fitted a desktop layout at 28 % and
  showed "2151 frames" instead of the picture.
- ✅ **Picker is a modal overlay, ⌘O.** `slopty-ui::picker::WindowPicker` lists on-screen windows
  (sorted app → title) and displays from a `Listing`; Escape or a backdrop click dismisses, a row
  click picks. Bound to ⌘O because Raycast owns ⌘⇧N system-wide on the dev Mac (evidence: the
  first binding opened Raycast's clipboard history instead).
- ✅ **Design tokens: one system for every surface (Warp-class pass, 2026-09-05).** The UI
  grew title bars, pills, badges, separators, a composer, an attention row, a minimap, a HUD,
  a key bar and a picker on six tokens (`canvas`, `panel`, `border`, `text`, `text_muted`,
  `accent`, radius 8, space 8). The audit (`target/design-audit.md`, scratch) found nine
  padding ratios, six radii, ten type sizes, four tint alphas and status colours borrowed
  from the terminal's ANSI palette. Ruling, modelled on Warp's visible system (restrained
  neutral ladder, one accent, 4/8 pt spacing, hairlines over shadows, quiet hover/active
  states, status colour only where it carries meaning, mono for terminal surfaces and the
  system sans for chrome): every element in `slopty-ui` and the app chrome draws from this
  table and nothing else, except geometry that content dictates (cell metrics, item rects,
  the terminal grid, canvas zoom multipliers).

  | Token | Dark | Light | Used for |
  |---|---|---|---|
  | `surfaces.canvas` (0) | `0A0B0E` | `F4F5F7` | window, canvas, bars |
  | `surfaces.panel` (1) | `14161B` | `FFFFFF` | title bars, panels, popovers, composer |
  | `surfaces.raised` (2) | `1B1E25` | `EEF0F3` | key caps, inputs, hovered rows |
  | `surfaces.overlay` (3) | `23272F` | `E4E7EC` | pressed rows, pill fills, HUD |
  | `surfaces.border` | `24272E` | `D8DBE1` | every hairline (1 pt) |
  | `surfaces.text` | `E6E6E6` | `1D1D1F` | primary |
  | `surfaces.text_secondary` | `B4B9C3` | `4B4F58` | labels, tool summaries, counts |
  | `surfaces.text_muted` | `8B919C` | `6E6E73` | hints, timestamps, folds, inactive titles |
  | `surfaces.accent` | `8AB4F8` | `2F6FDB` | focus ring, active border, primary action, links |
  | `surfaces.accent_fg` | `0A0B0E` | `FFFFFF` | text on an accent fill |
  | `surfaces.success` | `98C379` | `1A7F37` | connected, agent done |
  | `surfaces.warn` | `E5C07B` | `9A6700` | agent waiting, "N need you", muted, reconnecting |
  | `surfaces.error` | `F06C75` | `CF222E` | failed result, failed command, pairing error |
  | `radii.xs / sm / md` | 4 / 6 / 8 | same | pills and inline buttons / buttons, inputs, key caps / panels, items, popovers |
  | `spacing.xxs … xl` | 2 / 4 / 8 / 12 / 16 / 24 | same | the only paddings and gaps in chrome |
  | `typography.ui_size` + `caption()/small()/title()` | 13 → 10 / 12 / 15 | same | chrome type scale (settings move the base) |
  | `typography.mono_size` @ `mono_line_height` | 13 @ 1.0 | same | terminal grid, code in the conversation (the multiplier is ghostty's `adjust-cell-height`; 1.0 = the font's own) |
  | `typography.markdown_line_height` | 1.5 | same | assistant turns |
  | `alpha::TINT_FAINT / TINT / TINT_STRONG / TINT_PRESSED` | 0.08 / 0.12 / 0.25 / 0.4 | same | selected row / pill fills, hover / answer buttons, the human's bubble / a strong tint under the pointer |
  | `alpha::HOVER` | 0.08 | same | hover wash of `text` on a bare button |
  | `alpha::SCRIM` | 0.6 | same | modal backdrop |
  | `alpha::SEPARATOR / SEPARATOR_ERROR` | 0.18 / 0.7 | same | command-block hairline (terminal fg / `error`) |
  | `alpha::HUD / MINIMAP / MINIMAP_ITEM` | 0.85 / 0.92 / 0.7 | same | translucent panels over video and the canvas |

  Rules that follow (as shipped): status tones are chrome tokens now — the terminal palette
  stays the terminal's; the chrome tones share its hues so nothing jars, and the light table
  has its own set. The agent badge uses the accent for both busy states (thinking and a tool;
  the label says which), `warn` while blocked, `success` when done. Shadows go except on
  floating layers (picker, host switcher, search bar, "↓ latest" pill), and those use
  `shadow_sm`. Focus is one accent hairline: the focused item's frame, the search bar while it
  has the caret, the composer while it has the caret. Hover is a `HOVER` wash or a step up
  the surface ladder, pressed one step further, never opacity. Pills: `radii.xs`,
  `spacing.sm`/`spacing.xxs` padding, `small()` type, `TINT` fill of their tone with the tone
  as text. Buttons: `radii.sm`, `spacing.md`/`spacing.xs` padding in a dialog and
  `spacing.sm`/`spacing.xs` in a bar; the one primary action per surface is an accent fill
  with `accent_fg` text, a secondary one is `raised` → `overlay`. Key caps are `raised` on the
  `panel` bar, the accent when armed. Markdown in the conversation gets
  `markdown_line_height`, paragraph gaps of one base unit, headings stepping down from
  `title()` to the base, code in the terminal mono at `small()` on `raised`. gpui-kit's own
  theme (what its inputs, the composer and `TextView` read) is rewritten from the same tokens
  by `slopty_ui::kit::sync` on every theme change, instead of a second palette. The only
  literal geometry left is content-driven: the picker's 560×520 frame, the 420 pt pairing
  panel, the 320 pt host switcher, the 180 pt search field and its 40 pt counter, the list's
  512 pt overdraw, and the canvas zoom multipliers. Headless tests read the tokens back
  through `painted_quads()`: accent vs hairline item frame, the `warn` outline of a blocked
  agent in both variants over a light-canvas fill, the `error` separator tone, the composer's
  focus ring and the warn-tinted attention row, and gpui-kit's colours after a sync. The app
  goldens (`terminal`, `note`, `conversation`, `conversation-permission`) were re-accepted for
  this ruling; the track report names each with its `differing/total`.
- ✅ **Frame time is measured by the app itself, not inferred** (2026-09-05). `slopty_ui::frames`
  is a pure probe (ring of 1024 frames, nearest-rank percentiles, unit-tested with hand-made
  instants): `begin` at the top of `Workspace::render`, `end` in a zero-size element painted
  last, so a sample is the whole draw (layout, prepaint, paint) and nothing else. It reports
  draw p50/p95/p99/max, the interval between draws, how many draws ran over the nominal frame
  (1/60 s on the Mac, 1/120 s on iOS; `SLOPTY_FRAME_HZ` overrides) and how many frame slots
  those long draws swallowed (`dropped` = Σ⌊draw/nominal⌋, not interval gaps, because a
  pause in typing is not a dropped frame). It is the fourth line of the ⌘⇧I overlay and the
  `frames` block of the self-test `dump`; `frames_reset` starts a window. Keystroke → paint is
  measured beside it (`terminal::latency`): a key's sequence number is stamped on press and
  matched at the first paint that shows the local echo (predicted) and the first paint after
  the host's `input_ack` covers it (echoed); `dump` carries both per terminal. GPUI's own
  `FrameTiming` sits behind the `profiler` feature (hdrhistogram) and is not built.
- ✅ **The link applies host events once per frame, in one update** (2026-09-05). The app's
  link loop used to run a GPUI update per event; a burst of frames from twenty sessions
  queued a foreground task each and, in the self-test build, drew the window for each (GPUI's
  `test-support` draws every dirty window inside `flush_effects`). Now the first event after a
  quiet spell is applied at once and, while events keep arriving, the loop drains up to 256 of
  them and applies the batch in one `cx.update` at most once per nominal frame. A keystroke on
  an idle link is not delayed; a flood costs one update and one notify per frame.
- ✅ **A session sends at most 125 frames a second** (2026-09-05). The host coalesced PTY output
  for 2 ms and then sent a frame, so a flooding shell produced 500 frames/s per session; twenty
  of them overran every client's 256-frame sink and hostd detached the client ("cannot keep
  up") within seconds. `slopty_host::session::MIN_FRAME_INTERVAL` (8 ms) paces a session's
  frames after the first: a flood leaves every 8 ms. No display shows more than 120 Hz; the
  prediction path is unaffected (the client's local echo does not wait for a frame).
  Amended 2026-09-06: the 2 ms `COALESCE` window that a burst after a quiet spell used to wait
  is gone. With the pacer in place it bought nothing (a flood is bounded by the 8 ms interval
  either way) and it was the largest fixed cost on a keystroke's echo: `bench echo` on
  loopback sat at QUIC rtt + ~3 ms, 2 ms of which was this window. Output after a quiet spell
  is now framed at once; bytes already readable when the timer fires still land in the same
  frame because the actor's select reads the master first. Before/after in MEASUREMENTS
  2026-09-06 "leading-edge frame".
  Amended again 2026-09-06, later: "framed at once" was framed on the next timer tick. The read
  arm set `frame_due = now` and left the flush to the select's timer arm, and a tokio deadline
  of "now" is rounded up to the timer wheel's next millisecond and waited for by the driver's
  park: 1.4 ms per keystroke, measured stage by stage (MEASUREMENTS.md "the keystroke path,
  stage by stage"; the engine itself is 2 µs). The read arm now calls `flush_frame` inline when
  `frame_due_after` says the frame is due, and arms the timer only when the previous frame is
  younger than `MIN_FRAME_INTERVAL`. Quiet-machine echo: p50 3.1–3.9 → **0.65 ms**. Rule for
  the whole codebase that falls out of it: **never express "now" as a tokio timer** — a
  `sleep_until(now)` or `interval` tick is a millisecond, and on this path a millisecond is
  the budget.
- ✅ **The keystroke path carries permanent trace stamps** (2026-09-06). `trace!` lines at
  every stage of one echo — `term input received` and `frame sent` in hostd's connection,
  `pty input written` (with `queued_us`, `write_us`), `echo read` (`echo_us`) and `frame
  flushed` (`read_to_frame_us`, `input_to_frame_us`) in the session actor, `frame received`
  in the client link and `bench send` / `bench frame` in the bench — with microsecond
  timestamps from one clock when host and client share a machine, so a log pair splits the
  round trip without a profiler. Cost when off: an `Instant` in `Cmd::Request` and two
  `Option` writes per keystroke. This is how the 1.4 ms above was found after two rounds of
  guessing (parser, coalescing) and it is the first thing to reach for when the echo number
  moves; the loaded-machine rows in the same MEASUREMENTS entry show it telling scheduling
  noise from a regression. Not a wire change: `ClientSink` still carries bare `TermEvent`s,
  so the sink → forwarder hop is measured from the flush stamp to `frame sent` rather than
  stamped itself.
- ✅ **The QUIC legs are quinn's and iroh's own, and stay as they are** (2026-09-06). With
  the frame flushed inline, a same-machine echo is 0.73 ms p50 on a quiet host, of which
  everything Slopty does is 0.1 ms and the two QUIC legs are 0.6 (client → hostd 0.42, hostd →
  client 0.19; MEASUREMENTS.md "the keystroke path, stage by stage"). The suspicion that the
  legs were tokio wakes of parked workers came from a loaded machine, where
  `TOKIO_WORKER_THREADS=1` looked 0.1–0.3 ms faster per leg; quiet, the medians are identical
  and only the tails differ (p90 0.8 against 1.5–2.8 ms). Not changed: the median is the path
  through iroh's socket task and quinn's endpoint and connection drivers, the same on any
  worker count, and a smaller pool for the tail is a decision for the flap harness, since the
  video path packetizes and pumps on the same runtime. The next step on this path, if one is
  ever needed, is a profile of iroh's send and receive path, not another runtime knob.
- ✅ **Only items in the viewport are drawn** (2026-09-05). `CanvasView::draws` culls an item whose
  screen rectangle is outside the viewport, except the active item and one being dragged or
  resized (their views must stay in the frame for focus and the gesture). The minimap still
  reads every item's rectangle, so the culled items stay visible there. Headless test:
  `items_outside_the_viewport_are_not_drawn_unless_active`.
- ✅ **Terminal text is shaped per word and cached across frames and views** (2026-09-05). The
  element used to shape each row as one line and cache it by the row's content hash; under
  streaming output every row is new every frame, so the cache never hit and a stack sample of
  the app under twenty flooding shells put 48 % of the main thread in `shape_line`. Rows are
  now split at plain spaces (blank, narrow, no underline or strikethrough: a background is a
  quad, not a glyph) and every plain digit stands alone; each piece is shaped on its own with
  the forced cell width and cached by (text, styles, size, family, palette, focus) as an
  `Rc<ShapedLine>`, then painted at its start column. Pixel-identical to whole-row shaping:
  under the forced width every glyph sits at cluster index × cell width, kerning is off, and
  no coding font ligates across a space or between digits (a decorated digit stays in its
  word so the underline is one piece). The cache sweeps once per frame (stamped with
  `frames::index`), not once per element: with four terminals in view the per-element sweep
  evicted each terminal's rows before it came round again. The `~` filler is shaped once per
  prepaint. The row loop reads the view's rows in place (no `Line` clones per frame) and the
  cell is measured once per (family, size, scale), not once per frame. Tests: row splitting
  (`segments`), column-independent keys, once-per-frame sweep, and the headless
  `words_are_shaped_once_across_rows_and_frames` (three rows of two words shape two entries; a
  frame with the same words elsewhere shapes nothing new).
- ✅ **A cell's text clones as a copy** (2026-09-05). `slopty_grid::CellText` was a
  `SmallVec<[u8; 8]>`; the client copies every row it receives into its line cache (screen and
  scrollback both hold it) and `Vec<Cell>::clone` was 16 % of the main thread under the flood.
  It is now an inline `[u8; 22]` with a length, or a `Box<str>` past that (a ZWJ emoji
  sequence): still 24 bytes, same wire form (a string), equality and hash by content because
  the representation is canonical. `smallvec` left the workspace with it.
- ✅ **Words are shaped at the base size only and painted glyph by glyph at the zoom**
  (2026-09-06, fork commit `876a96f`). A `ShapedLine` is tied to the size it was shaped at, so
  every zoom step re-shaped every visible word and the card → grid flip at `CARD_ZOOM` shaped
  twenty grids in one frame (the p99 / max of scenario (b) above). Now the word cache is keyed
  without the zoom (`hash_base(focused, base_size, family, palette)`), `shape_cells` shapes at
  the theme's size with the base cell width forced, and `paint` walks each word's glyph runs
  (`ShapedLine::layout()`, the fork's accessor; `LineLayout` / `ShapedRun` / `ShapedGlyph` were
  already public) and calls `Window::paint_glyph` / `paint_emoji` at the zoomed font size, at
  `origin + shaped position × zoom` on the derived baseline (`glyph_origin`, unit-tested). Sound
  because, under the forced cell width, a glyph's shaped x is cluster index × base cell and the
  zoomed grid is the base grid × zoom, so the positions are the grid's either way; the glyph
  ids are the font's and do not depend on the size. All the glyphs go in **one `paint_layer`**
  per element: a primitive painted outside a layer takes a bounds-tree insert of its own for
  its order (`Scene::insert_primitive`), which is why GPUI gives every line it paints a layer;
  the first cut painted bare and cost 0.5 ms p50 per pan frame at zoom 1 (1.2 → 1.7 ms in
  every run, load or not; `BoundsTree::insert` 12 % of the main thread in the sample), the
  layer gives it back. The word carries its per-byte colours
  (`Word::colors`, `color_at`, unit-tested) since GPUI's decoration runs are `pub(crate)`;
  every underline, the curly one included, is a `Decoration` drawn from the cells (the wave
  through `Window::paint_underline` at the font's underline position, where it used to sit at
  GPUI's `baseline + 0.618 · descent`), so segmentation no longer has a curly special case.
  Pixel-identical at zoom 1 (goldens: terminal 1576 / 540000 in the prompt rows, the run-to-run
  variance; the rest 0). Rasterisation at the zoomed size is what GPUI did already, per size
  and subpixel variant, so the atlas grows with the number of distinct sizes a pinch passes
  through exactly as before; the sample says it is 0.6 % of the main thread and the ruling is
  **no size quantisation** (the brief's fallback of shaping at the nearest whole-pixel cell
  is not needed and would blur less than it would cost in code). What the main thread does
  during the zoom cycle now (`sample`, MEASUREMENTS "zoom hitch"): shaping 2.5 % (was 41–48 %),
  `paint_glyph` 12 % (all scene insertion), "prepaint's per-cell loops" 26 % (a misread of
  mangled symbols: the demangled sample of the next ruling puts the per-cell loop under
  0.5 % and that share on the font walk), applying the
  flood 33 %. Draw numbers before / after in MEASUREMENTS: p50 pan 1.2–1.4 → 1.0–1.1 ms,
  zoom 1.1–1.5 → 1.1–1.3 ms; the p99 / max of the zoom cycle stayed over 16.7 ms on every tree
  (prepaint over twenty visible grids plus the flood, on a machine that was never idle), so
  the smooth guard gains **no zoom limit** for now.
- ✅ **A line is shared between the screen and the scrollback** (2026-09-06, measured). Both hold
  `Arc<Line>`; applying a row makes one allocation and puts it in both, so a row that scrolls off
  the screen into history is moved, never copied. Every accessor still hands out `&Line`, so the
  painter and the wire are untouched. `Screen::resize` is the one place that has to diverge, and
  it uses `Arc::make_mut`, which copies only a row history also holds and only on a resize.
  Measured with 20 flooding shells (MEASUREMENTS, same date): applying host frames went from
  **14.8 % to 4.2 %** of the main thread and `Vec<Cell>::clone` from **6.8 % to 0 %**. The same
  change fixed a second copy nobody had measured: `Scrollback::set_extent` ran `split_off` on
  every frame, rebuilding the map whether or not the host had dropped anything; it now splits
  only when `oldest` actually advances.
- ❌ **A `VecDeque` ring instead of the scrollback's `BTreeMap`** (2026-09-06, measured, not
  taken). Written and measured: a ring of `capacity` slots over `[base, base + capacity)` with
  holes for what has not arrived. Two paired 10-second samples put it inside the map's own
  spread — apply 4.2 % / 6.2 % with the map against 7.3 % / 5.0 % with the ring — because the
  container was never the cost. What the profile actually shows inside `insert_shared` is
  `Arc::drop_slow` freeing the evicted line's `Vec<Cell>`, which both containers pay identically;
  the "B-tree churn" in the earlier note was that same drop, attributed to the `pop_first` frame
  it happened under. Against no win the ring costs behaviour: the map holds two disjoint regions
  (the live tail and a history range the user scrolled to and fetched), and a single contiguous
  ring has to re-base and drop one of them. `missing()` exists precisely because the cache is
  sparse.
- 🔬 **What is left of the apply path** (2026-09-06): freeing an evicted line, ~4 % of the main
  thread under a 20-shell flood; a line-buffer pool would be the next step and is not worth it
  at that size. The two earlier 🔬 items here (the shared line, size-independent glyph
  painting) are the ✅ rulings above.
- ✅ **The installed fonts are listed once per app, and prepaint builds only the rows the clip
  shows** (2026-09-06, `e4613cf` + `f0e7007`). The zoom cycle's worst frame was not the
  element's row loop at all. The canvas culls off-screen items, so at zoom 1 only the
  viewport's few grids ever draw; ⌘1 drops everything below `CARD_ZOOM`; the first step back
  over it draws twenty grids for the first time in one frame, and each first frame resolved
  the monospace family by listing every installed font (`TextSystem::all_font_names`, a
  synchronous XPC round trip to fontd, ~36 ms): 32 % of the main thread in a demangled
  `sample` of scenario (b), the 700–1300 ms max frames of every zoom run so far, and a 36 ms
  hitch on every ⌘N. The resolved family is now memoised per theme list on the `ShapeCache`
  global (the per-view memo stays, so a frame costs neither the walk nor the lookup); a
  headless test opens three shells and counts one walk. Rows a grid has below or above the
  window's content mask are not built (no quads, no words, no hashes; paint walks only the
  prepared rows); a headless test counts every row of a grid on screen and under half of one
  hanging off the bottom edge. Numbers (MEASUREMENTS "the zoom hitch, second look"): the zoom
  max 356 / 1006 ms on main → 109 / 91 ms, p99 75 / 280 → 86 / 34, on a machine other sessions
  were building on (noise 5–120 processes); `TerminalElement::prepaint` 38 % → 4 % of the main
  thread. Not done, with the number that rules it out: a per-row cache of quads and
  decorations and skipping link/overlay work during a flight — the per-cell loop they would
  save is ≈ 0.4 % of the thread.
- 🔬 **What the zoom p99 is now, and whose**: with the element at 4 % (prepaint) + 12 % (paint,
  all `paint_glyph`), the after-sample's draw is 48 % paint (terminal glyphs 28 %, the items'
  titles and pills 7 %), 29 % GPUI layout (`Div::request_layout` and taffy over twenty item
  trees whose every size changes per zoom step) and 7 % scene + Metal; between draws the
  flood's apply takes 19 % of the thread (claude/applyfast's `Vec<Cell>` copies 8 %). A zoom
  frame that follows an apply is the p99. The element has no more to give on the main thread;
  the next cuts are the item chrome's layout at zoom (a fixed-size card whose contents scale
  as one transform instead of a re-laid-out tree) and the apply. No zoom guard: B1 and B2 ran
  at 120 and 42 build processes and still beat main's quiet runs, which is the load telling
  the guard what it would flap on.
- ✅ **Text in motion paints from a raster ladder; chrome labels are shaped once** (2026-09-06,
  fork `9fa23ead`, `perf(ui): paint zooming text from a raster ladder, shape chrome labels
  once`). The third demangled look at a zoom-step draw (MEASUREMENTS "third look") split the
  "29 % layout" honestly: 6 % was the chrome's text shaped again at every step (GPUI's text
  element shapes at the painted size and every step is a new size), 11 % taffy over the whole
  window's tree and the rest the per-frame tree build — GPUI rebuilds elements and the taffy
  tree every frame regardless (`layout_engine.clear()` in `Window::draw`), so nothing is laid
  out *because* bounds change, and there is no scaled-layer transform to hand a cached layout
  to (only SVG paths take a `TransformationMatrix`). What the same look found instead:
  **glyph rasterisation at 14 % of the draw** — every step a new font size, every glyph a fresh
  raster and atlas upload, and an atlas that never evicts growing with the number of steps.
  The sprite shader maps an atlas tile onto arbitrary bounds, so the fork gained
  `Window::paint_glyph_scaled(origin, font, glyph, raster_size, font_size, color)` (rasterise
  at one size, stretch to another, grayscale AA). The canvas marks a frame **in motion** while
  the camera zoom differs from the one drawn last frame and until an 80 ms settle timer that
  every change restarts has fired (`SETTLE`, the note's generation pattern); in motion, the
  terminal glyphs and the chrome labels paint from the nearest rung of an
  eight-per-octave size ladder (`fonts::raster_rung`, within ±4.4 %), stretched — a pinch
  from 30 % to 200 % rasterises ~22 sizes instead of one per step — and the settled frame
  paints exact. Asking for the settle frame right after each frame in motion was tried first
  and doubled a gesture's frames (one settle per step, each rasterising exact at an
  intermediate size: zoom p99 32 ms against main's 9); a frame the flood asks for between two
  reports must stay on the ladder too, or it rasterises an intermediate size exact. The item
  chrome's text (title, pills, badge, block headings) is a `ChromeText` element: shaped once
  at its base size in a per-app cache swept per frame, sized by arithmetic (`shaped width ×
  k`, no taffy measure callback), painted glyph by glyph at `base × k` with GPUI's own
  baseline and x-advance arithmetic — identical at `k = 1` — and its own ellipsis. Numbers
  (draw ms p50/p95/p99/max · dropped, alternated against main on the same machine, noise 0–36):
  zoom **7.3–9.8 ms p99** in all four runs of the tree against main's 13.2–56.7 (main's best,
  on an idle machine, 9.1–10.7); dropped frames 1–5 against 3–34; pan unchanged; after-sample
  `rasterize_glyph` 14 % → 3.9 % of the thread, `paint_glyph` exact 0 in motion, chrome
  `shape_text` 0. **The user's bar — a pinch with twenty terminals that drops no frame — is met
  at p99 on this machine when it is idle and at 14–36 build processes**; the max frame
  (27–59 ms, 1–5 dropped of ~480) is the apply landing on a frame. Still no zoom guard in the
  smooth test: the same tree's max swings 27 → 59 with load. Not done, with the number: caching
  an item's taffy layout across steps (a retained-layout fork hook for ~9 % of a draw shared by
  the whole window's tree) and flattening the item tree.

## Terminal

- ✅ **VT state on the host, rows on the wire.** See ARCHITECTURE §2. Precedents: mosh, zellij,
  wezterm mux. Rejected: raw bytes + client VT (replay on reconnect, iOS needs the engine, slow
  links pile up bytes).
- ✅ **Engine: libghostty-vt via `Uzaaft/libghostty-rs` ≥ `5988a0b7`.** Verified 2026-09-04:
  crates.io 0.2.1 (2026-07-18), repo pushed 2026-09-02, soundness issue #75 fixed by PR #81
  (null-pointer slice in `ClipboardWrite::contents`, `from_utf8_unchecked` on OSC 52 payload).
  Author is an active ghostty contributor (27 upstream contributions). The C API header says
  "not yet stable" — we pin one ghostty commit and vendor the source (`GHOSTTY_SOURCE_DIR`, no
  network at build time). Only the host builds it. Zig 0.16.0 required (installed).
  Alternative kept as a differential-test oracle: `rio-vt` 0.5.26 (pure Rust, extracted 2026-07).
  2026-09-12: **ghostty bumped 752 commits to main `44f2a44df` (2026-09-10) through a binding
  fork** `aislopware/libghostty-rs` branch `slopty` (upstream `Uzaaft/libghostty-rs` never moved
  past our pin, and its bindings are checked in, not generated at build time). The fork commit
  re-pins `GHOSTTY_COMMIT`, regenerates `bindings.rs` with the repo's own `gen-bindings` tool
  (`GHOSTTY_SOURCE_DIR=<ghostty> cargo run -p libghostty-vt-sys --features bindgen-tool --bin
  gen-bindings`) and adapts the wrapper: every C enum is typed `: int` now, so the wrapper enums
  are `repr(i32)`; `ghostty_render_state_colors_get` is gone, colors come through
  `ghostty_render_state_get(COLORS)`. The wrapper's 34 tests pass; slopty-engine and slopty-pty
  compile and test unchanged (the terminfo source did not move, only gained a content hash).
  `xtask/upstream.toml` tracks the binding as a third fork: `check` also prints whether the
  binding's `GHOSTTY_COMMIT` equals `vendor/ghostty` and how far the vendored source is behind
  ghostty main; `sync` build-checks it with `cargo check -p libghostty-vt --all-targets`
  (`GHOSTTY_SOURCE_DIR` comes from our `.cargo/config.toml`, so the checkout under `.research/`
  builds against `vendor/ghostty`). A ghostty bump is therefore: move the submodule, re-pin +
  regenerate in the binding fork, push, `cargo update -p libghostty-vt`, gate.
  New C API worth adopting: native search (`ghostty_search_new/set/tick/get`, whole-terminal,
  incremental) — ⏸ after reading `search.h` at `44f2a44d` (2026-09-12): its matching is
  "byte-exact except ASCII letters", so a plain needle would fold case only for ASCII where
  ours folds Unicode, and regex needles would still take our path, which leaves two searches
  with two answers for one needle; the prize is the formatter's 10 ms of a 16 ms search over
  50 000 lines (MEASUREMENTS "search over a full history"), paid only while typing a needle
  into a history that size. Revisit if ghostty adds Unicode folding or the search bar becomes
  a live follow of a streaming scrollback (the incremental feed/tick split is built for that);
  `row_iterator_next_dirty`
  for the apply path; `ghostty_terminal_paste` with its Kitty clipboard/paste safety checks
  (`GHOSTTY_REJECTED`); semantic prompt state read straight from the C API; Kitty clipboard
  protocol reads (`clipboard_read` effect) behind a permission prompt.
- ✅ **Rendering is our GPUI element**, not sugarloaf or ghostty's renderer. Glyph shaping via
  GPUI's text system with our own cell layout, sprite glyphs for box drawing, per-row dirty
  tracking (zed's terminal element reshapes every frame; we won't).
- ✅ **Font metrics follow ghostty's `src/font/Metrics.zig`**, ported as the pure function
  `slopty_ui::terminal::metrics` (`Face` in, `Metrics` out; no window, no IO), verified
  2026-09-05 against the vendored Zig. The derivation, all in **device pixels** (the face is
  measured at `font_size × scale`, and `Grid` divides back to points so a cell is a whole
  number of device pixels):

  ```
  cell_width  = round(advance('M'))                       min 1
  cell_height = round(ascent - descent + line_gap)         min 1
  baseline    = round((line_gap/2 - descent) - (cell_height - face_height)/2)   [up from the bottom]
  underline_thickness     = ceil(post.thickness ?? 0.15 · ex)   min 1
  underline_position      = round((cell_height - baseline) - (post.position ?? -thickness))
  strikethrough_thickness = underline_thickness
  strikethrough_position  = round((cell_height - baseline) - (ex + unrounded thickness)/2)
  overline y = 0, overline/box thickness = underline_thickness, cursor_thickness = 1
  ```

  Rounding, not ceiling: the error stays under half a pixel and the apparent spacing matches
  between a 1× and a 2× display; the baseline is then centred in the rounded cell, so the text
  is inset (or overhangs) equally top and bottom. `Metrics::set_cell_height` is ghostty's
  `adjust-cell-height`: it splits the added pixels between top and bottom, giving the odd one
  to the side the text sits nearer. Estimates fill in what a font does not say — cap = 0.75 ·
  ascent, ex = 0.75 · cap, underline thickness = 0.15 · ex, underline position = −thickness.
  (Until 2026-09-06 GPUI's `TextSystem` exposed neither the line gap nor the `post` table, so
  those estimates always applied in production; the fork now exposes them, see "The font's
  own line gap and underline" below.) Two fonts, two sizes, two displays, in device pixels
  (`cargo nextest run -p slopty-ui terminal::metrics`; ghostty's formula recomputed
  independently gives the same numbers). The first four JetBrains Mono rows are the face
  without its `post` table (the estimates); the "tables" rows are what the app measures:

  | font | pt | DPR | w | h | baseline | underline y/thick | strike y/thick |
  | --- | --- | --- | --- | --- | --- | --- | --- |
  | JetBrains Mono (no post) | 13 | 1 | 8 | 17 | 4 | 14 / 2 | 9 / 2 |
  | JetBrains Mono (no post) | 13 | 2 | 16 | 34 | 7 | 29 / 3 | 19 / 3 |
  | JetBrains Mono (no post) | 15 | 1 | 9 | 20 | 4 | 17 / 2 | 12 / 2 |
  | JetBrains Mono (no post) | 15 | 2 | 18 | 39 | 8 | 33 / 3 | 22 / 3 |
  | JetBrains Mono (tables) | 13 | 1 | 8 | 17 | 4 | 15 / 1 | 9 / 1 |
  | JetBrains Mono (tables) | 13 | 2 | 16 | 34 | 7 | 31 / 2 | 20 / 2 |
  | JetBrains Mono (tables) | 15 | 1 | 9 | 20 | 4 | 18 / 1 | 12 / 1 |
  | JetBrains Mono (tables) | 15 | 2 | 18 | 39 | 8 | 36 / 2 | 23 / 2 |
  | Menlo | 13 | 1 | 8 | 15 | 3 | 13 / 1 | 8 / 1 |
  | Menlo | 13 | 2 | 16 | 30 | 6 | 26 / 2 | 16 / 2 |
  | Menlo | 15 | 1 | 9 | 17 | 4 | 14 / 1 | 9 / 1 |
  | Menlo | 15 | 2 | 18 | 35 | 7 | 30 / 2 | 19 / 2 |

  Consequences in the element (`crates/slopty-ui/src/terminal/element.rs`): the row height is
  the font's own, so `typography.mono_line_height` **defaults to 1.0** and now means ghostty's
  `adjust-cell-height` percentage rather than a multiplier over the point size; underline and
  strikethrough are painted as quads at these offsets because GPUI hardcodes its own
  (`text_system/line.rs`: underline at `baseline + descent · 0.618`), leaving only the curly
  underline to GPUI (drawing a wave is its alone); the bar and underline cursors take
  `cursor_thickness` (one device pixel, as ghostty); and `TermSize.metrics` on the wire is now
  the cell in **device** pixels, which is what `ws_xpixel`/`ws_ypixel` are supposed to carry —
  and so is the offset in a pixel mouse report (`CellMetrics::pixel_at`), since the host divides
  one by the other. Two more consequences of deriving rather than guessing: the glyphs are
  painted on the derived baseline (GPUI centres a line in the box it is given, which is close to
  but not the font's baseline, so the origin is offset by the difference — exact, and per row,
  so a fallback font still lines up with its decorations); and a zoomed grid is the unzoomed one
  **scaled**, never re-derived, because the columns that fit were counted with the unzoomed
  cell — re-deriving rounds the cell up and clips the last column (at 13 pt, DPR 2, zoom 0.3 the
  cell came out 2.5 pt against the 1.2 pt the item had room for).
- ✅ **The font's own line gap and underline, through the fork** (2026-09-06, fork commit
  `876a96f`). font-kit's Core Text loader had always read `CTFontGetLeading`,
  `CTFontGetUnderlinePosition` and `CTFontGetUnderlineThickness` (the `hhea` line gap and the
  `post` table) into GPUI's `FontMetrics`; only the accessor was missing. The fork adds
  `TextSystem::font_metrics(font_id) -> FontMetrics` plus per-size `line_gap`,
  `underline_position`, `underline_thickness` beside `ascent`/`descent`, on macOS and iOS alike
  (both platform text systems go through font-kit). `measure` in the terminal element now
  fills `Face::{line_gap, underline_position, underline_thickness}` from them and leaves an
  estimate only where the font says zero (ghostty's rule: `post.thickness ?? 0.15 · ex`).
  Effect on the bundled JetBrains Mono (`post` underlinePosition −155, thickness 50 per 1000
  em; `hhea` line gap 0): the underline moves one pixel down and thins from 2 to 1 device
  pixel at 13 pt on a 1× display (2 px at 2×), the strikethrough thins with it; the cell,
  baseline and line height do not change (no line gap). Menlo (`post` −130 / 90, line gap 0)
  already matched the estimate's rounding at these sizes. Verified end to end, not by pixels:
  `dump.terminals[].face` reports the measured face (device pixels per em, ascent, descent,
  line gap, underline position/thickness or `null` for an estimate) and the macOS and
  simulator self-tests assert JetBrains Mono's numbers to the em (`check_jetbrains_mono_face`:
  ascent 1.020, descent −0.300, line gap 0, underline −0.155 / 0.050). Goldens: see
  MEASUREMENTS.md "font truth" for the moved pixels.
- ✅ **PTY custody in a tiny separate daemon** (`slopty-ptyd`), masters handed to hostd by
  `SCM_RIGHTS` (`nix` `sendmsg`/`recvmsg`; `sendfd` dropped — one fewer dependency, and macOS
  has no `MSG_CMSG_CLOEXEC` so CLOEXEC is set by hand either way). ptyd drains the master into a
  bounded ring (4 MiB default) while detached. Verified 2026-09-04 by an end-to-end test
  (`apps/slopty-ptyd/tests/roundtrip.rs`): spawn → detached backlog → attach → connection loss →
  resume → reattach → exit status. macOS detail: `TIOCSWINSZ` on a fresh master fails with
  `ENOTTY` until the slave has been opened once; `Pty::open` sizes through the slave.
- ✅ **Absolute line numbering via a tracked grid ref** (see ARCHITECTURE §2). Verified by engine
  tests: 20 lines through a 4-line scrollback keep `epoch` 0 and contiguous indices.
- ✅ **`KeyCode` is generated from ghostty's key list** (`for_each_key_code!` in slopty-proto) so
  the engine mapping is exhaustive at compile time. Our hand-written W3C list had drifted
  (letters named `KeyA` vs `A`, missing numpad/browser keys).
- ✅ **Terminal type**: `xterm-ghostty` when its terminfo is installed, else `xterm-256color`.
  ghostty's entry (270 capabilities, three names) is ported to `slopty_pty::terminfo` as const
  data and rendered exactly as `Source.zig` renders it; the rendered source is an insta
  snapshot (`crates/slopty-pty/tests/snapshots`), so bumping the vendored ghostty shows up as a
  diff instead of a silent change in what programs are told. **ptyd compiles it on start-up**
  (2026-09-05): `tokio::spawn` right after `bind`, `/usr/bin/tic -x -o <db> -` with the source
  on stdin — the absolute path, never a `tic` from `PATH`. The database is `$HOME/.terminfo`,
  or `$SLOPTY_TERMINFO_DIR` when a test or a sandboxed run names one — and when it is set it is
  the *only* database consulted, so such a run answers from its own and not from whatever the
  machine happens to have (the app self-test points both it and `TERMINFO_DIRS` at the stack's
  temp dir, so no run touches the developer's home).
  A child is given `TERMINFO=<dir>` when — and only when — that override is in play: `TERM` and
  the search path have to agree, and a database ncurses knows nothing about would otherwise
  leave the shell advertising `xterm-ghostty` and unable to find it. Without the override the
  inherited `TERMINFO` is cleared, since `default_term` answered from the places ncurses
  searches by itself.
  Idempotent: `installed()` looks for `78/xterm-ghostty` or `x/xterm-ghostty` under the same
  directories the lookup searches, and does nothing when it is there. Nothing blocks on it —
  `default_term()` is read per spawn, so a shell that starts before `tic` finishes simply gets
  `xterm-256color`. On macOS `tic` warns about the description field and still exits 0, so only
  a non-zero status is an error.

## Transport

- ✅ **iroh 1.2.0** (1.1.0 verified from source in the cargo registry 2026-09-04; 1.2.0 adopted
  2026-09-12 the day after its release: `n0-dns-resolver` replaces hickory, nothing in our API
  surface moved, gate + app/iOS e2e green). QUIC via n0's own `noq` 1.3.0 underneath (a quinn fork; **not** the `quinn` crate, so quinn docs/types do not
  apply): streams for control/terminal, unreliable datagrams for media, connection migration for
  Wi-Fi↔cellular, hole punching + relay fallback, E2E encryption keyed by endpoint identity.
  Rejected: slop-desk's raw UDP + "the WireGuard mesh is the security boundary" (phone-anywhere
  needs app-level auth); WebRTC (ICE/SDP dead weight); MoQ (relay/pub-sub shape).
- ✅ **Default features off; `fast-apple-datapath` is banned.** `default-features = false,
  features = ["tls-ring"]`. iroh's default set includes `fast-apple-datapath` (dlsym of private
  `sendmsg_x`/`recvmsg_x`, batched UDP). Measured 2026-09-04 on loopback (`slopty ping`, see
  MEASUREMENTS.md): with it on both ends the app round trip is **53 ms** and QUIC's own RTT
  estimate 48 ms; with it off, **0.8 ms**. The batching path waits to fill batches, which is
  exactly wrong for keystrokes. The feature no longer exists in `slopty-net`; do not re-add it.
- ✅ **LAN discovery** is a separate crate, `iroh-mdns-address-lookup` 0.5.0 (there is no
  `discovery-local-network` feature in iroh 1.x). Behind `slopty-net/mdns`; hosts advertise,
  clients only look up. Wide-area lookup is iroh's `presets::N0` (DNS + pkarr on n0's infra).
- ✅ **Pairing**: `PairTicket { addr, token }` (`iroh-tickets` 1.0.0, base32 string, kind
  `sloptypair`), token single-use with a 10-minute TTL. On redeem the host trusts the client's
  endpoint key in `trust.json` (mode 0600); afterwards the QUIC handshake is the whole
  authentication and `Hello.pair_token` stays `None`. Rejections are sent then linger up to 2 s
  so the client reads `Rejected` before the connection closes (QUIC close discards unread data).
- ✅ **One connection per client**: bidi control stream opened by the client; one uni stream per
  attached session opened by the host, first message `StreamHeader { session }`; datagrams for
  media. Transport config: idle 45 s, keep-alive 5 s, 4 MiB datagram buffers.
- ✅ **Congestion controller**: noq default (Cubic); BBR3 measured 2026-09-05 (MEASUREMENTS.md),
  revisit once noq marks it stable (superseded 2026-09-05: BBR3 installed as default for both
  roles with `SLOPTY_CC=cubic` override; see line 683). The media path runs its own bitrate
  controller on top (see Video, "Adaptive bitrate").
- ✅ **ACKs within 2 ms, not QUIC's 25** (2026-09-05, `slopty_net::endpoint::MAX_ACK_DELAY`
  via `AckFrequencyConfig`, both roles). Media leaves the host as one burst per frame; BBR
  sizes the congestion window from bandwidth × min RTT, which on loopback is one or two
  frames (9–43 KB seen, 5.8 KB in `ProbeRTT`), so the tail of a frame waits for the ACK of
  its head. The receiver ACKs every second ack-eliciting packet at once and an odd last
  packet only when `max_ack_delay` expires: 25 ms by default, longer than a frame interval.
  Measured on loopback ("start-up on a cold connection" in MEASUREMENTS.md): the hostd pump
  logs every stretch in which the QUIC send buffer is not empty, and every frame ended with
  1.5 KB held for 5–7 ms, a 68 KB frame was held 105 ms at a 38 KB window, and the receiver
  NACKed the tails (6 NACKs in 3 s at the 5.8 KB window). With 2 ms the same run shows no
  hold over 5 ms and no NACK. Cost: an ACK per 2 ms of traffic at most, ~50 bytes each.
- ✅ **Initial congestion window 32 packets** (2026-09-05,
  `slopty_net::endpoint::INITIAL_WINDOW_PACKETS`, `SLOPTY_QUIC_IW` overrides it for
  experiments). RFC 9002's 10 packets is for an unknown peer on the open Internet; a paired
  host and client on a LAN or a mesh can start with the burst Chromium's QUIC uses. The
  first keyframe is tens to hundreds of KB and each window's worth costs a round trip, so
  the start-up cost of the default is 1–2 extra round trips per stream on a 10 ms path
  (numbers in "start-up over the mesh", MEASUREMENTS.md). Invisible on loopback.

- ✅ **QUIC datagrams, not raw UDP over a WireGuard mesh** (re-examined 2026-09-04 when the
  status bar showed ~100 ms). A QUIC datagram is one UDP packet plus ~30 bytes of header and one
  AEAD (hardware AES-GCM); WireGuard spends the same per packet on ChaCha20, so "UDP + WG" moves
  the crypto, it does not remove it. Measured: app RTT 0.8 ms on loopback, 0.9–2 ms on the LAN
  direct path; the ~100 ms readings were the *relay* path (n0's aps1 server) while iroh had
  dropped the direct path (see below). slop-desk's transport doc admits its numbers were
  loopback-only and the WireGuard case was never measured. What QUIC buys: reliable streams and
  unreliable datagrams on one connection, migration, NAT traversal and relay fallback for a
  phone off the mesh, and app-level auth instead of "the mesh is the boundary".
- ✅ **Direct-only mode** (`slopty_net::Reach::DirectOnly`; hostd `--direct-only`, every binary
  honours `SLOPTY_DIRECT_ONLY=1`) for hosts and clients on a private mesh (NetBird/Tailscale)
  or one LAN: `RelayMode::Disabled` + `clear_address_lookup()`, mDNS kept. The ticket then
  carries only IP addresses (the mesh address among them) and a relay can never be selected: a
  dropped direct path drops the connection, which the app's reconnect loop turns into a
  ~1 s blip instead of a silent ×50 latency step. Gotchas: `Endpoint::online()` waits for a
  home relay and hangs with relays off, so `endpoint::online` watches `watch_addr()` for the
  first `TransportAddr::Ip` instead; a client that paired in `Anywhere` mode has a stored
  address with a relay URL, so `client::connect` strips relay entries when direct-only (else
  the dial waits on a relay it cannot use). Loopback test `direct_only_pairs_without_a_relay`
  asserts the ticket is relay-free and the selected path is direct. Roaming caveat: with no
  relay and no pkarr the client only knows the addresses in its stored ticket plus mDNS, so a
  host whose mesh IP changes needs re-pairing. Observed 2026-09-04: when the host process is
  killed, the client's lone direct path times out at 15 s, noq logs `failed closing path
  err=LastOpenPath` and keeps it, and the connection only drops at the 45 s idle timeout
  (`IDLE_TIMEOUT`); the top bar showed a stale RTT until then. Seen once (2026-09-12, the
  first gate after the zed sync, cold build then 516 tests at once): the loopback test
  `pair_then_reconnect_then_reject_stranger` failed with the stranger's *dial* timing out at
  45 s (`Connect("timed out")`, the idle timeout again) instead of the host's `NotPaired`;
  alone it takes 0.24 s and the whole suite passed on the next run in 12.7 s. Not acted on:
  one occurrence, and a retry policy would only hide the rate. If it recurs, the number to
  read first is whether the host's accept loop ever saw the stranger. ✅ Liveness now comes from
  QUIC itself, no protocol message: the app samples `ConnectionStats.udp_rx.datagrams` once a
  second (`HostLink::received_datagrams`); keep-alive pings make a live host send something
  every 5 s, so a counter that stands still for `SILENCE_WARN` = 8 s turns the RTT readout into
  a yellow "host silent Ns". Verified 2026-09-05 by `SIGSTOP`ping hostd. Since the same day the
  app also *gives up* at `SILENCE_DROP` = 15 s (three missed keep-alives, noq's own path bar):
  `HostLink::abandon` closes the QUIC connection, the control reader reports `Disconnected`,
  and the normal reconnect loop runs, so a restarted host is back ~2 s after it answers instead
  of after the 45 s idle timeout. Measured with a frozen hostd: dropped at 15.2 s, reconnected
  2 s after `SIGCONT`. Reading the logs afterwards: a killed app's *old* connection still shows
  up on hostd as "connection lost: timed out" 45 s later; that is the stale one, not the new.
  Driving note: cliclick `kp:return` never reaches the app (osascript `keystroke return` does).
- ❌ **Machine load alone does not flap the path** (2026-09-06), closing the 2026-09-04
  investigation of one connection that went direct → relay-only for 43 s → direct while the
  machine was compiling. iroh's `BiasedRttPathSelector` always prefers a live direct path, so
  the direct path had to have been *closed* (noq abandon reasons `TimedOut` = 15 s path idle
  with 5 s heartbeats, `UnusableAfterNetworkChange`, `RemoteAbandoned`), and the standing
  hypothesis was that a busy machine starves the QUIC driver task until the heartbeats miss.
  A harness now says otherwise: `path_flap_under_{cpu,user_initiated_cpu,memory_io}_load`
  (`apps/slopty-hostd/tests/e2e.rs`, gate `SLOPTY_FLAP_E2E`) runs hostd and a client over iroh
  with relays enabled — both ends hold a relay path *and* a direct path, so there is somewhere
  to flap to — attaches a shell and a display stream, and hammers the machine for 90 s while
  sampling the selected path four times a second and keeping noq's own path log. Three shapes,
  each the whole machine: all-core spinning at default QoS, the same at
  `QOS_CLASS_USER_INITIATED` (what a build's workers ask for), and memory + I/O (GB-scale
  writes and reads plus thousands of small files). **None of them closed a path or spent a
  single sample on the relay**; the worst rtt on the direct path was 6–10 ms against 1.4–1.9 ms
  idle (MEASUREMENTS.md, "the path under load"). So load is ruled out on its own and nothing in
  the runtime or thread QoS is changed on the strength of it: no dedicated QUIC runtime, no
  `pthread_set_qos_class_self_np` on the driver threads, no send-queue priority.
  What the harness cannot see, and what the ruling therefore does *not* cover: its direct path
  is loopback on one machine, so it has no NAT rebinding, no Wi-Fi, no WireGuard and no black
  hole — the 2026-09-04 flap was on the mesh between two machines. The load half of the
  hypothesis is dead; the network half is untested and needs the second machine. Load is not
  free either: 19–38 datagram stalls per 90 s appeared under every shape where an idle machine
  has none, which is the pacer's problem, not the path's.

- ✅ **A dying connection detaches only its own sinks** (2026-09-05). Symptom on the simulator:
  ~45 s after relaunching the app, every terminal stopped updating while the host kept running
  the typed commands. Cause: the relaunched app reconnects under the same `ClientId`; the old
  QUIC connection idles out (`IDLE_TIMEOUT`) later and hostd's cleanup called
  `Host::client_gone(client)`, which detached *every* viewer with that id, i.e. the new
  connection's. Rule: viewer eviction is scoped to the connection that attached it:
  `SessionHandle::detach_sink(client, &sink)` removes a viewer only if `Sender::same_channel`
  matches, and `Peer::drop` in hostd uses it; `client_gone` is gone. Regression test
  `stale_connection_detach_keeps_the_reconnected_viewer`.

- ✅ **Terminal search runs on the host** (2026-09-05). The client caches at most 20k lines and
  the host retains 50k, so searching the client cache would miss most of the history or pull
  megabytes on every keystroke. `TermRequest::Search { needle, max }` answers with
  `TermEvent::Matches { needle, total, matches }`: every hit counted, the newest `max` (5 000)
  listed as `(line, col, len)` in cells. The engine renders the grid as plain text with
  libghostty's `Formatter` (plain, trim, selection over every row): one line per row, interior
  blank rows kept, trailing blank rows dropped, 0.36 ms for 904 rows (MEASUREMENTS.md), ~20×
  cheaper than the per-cell `lines()` walk. Columns come from ghostty's `grapheme_width`, so a
  hit paints exactly over its cells (wide characters count two). Smart case (no upper-case
  letter → case-insensitive) with a char-for-char fold so indices stay aligned; hits never span
  a soft-wrapped row. Rejected: libghostty-vt has no text search of its own (its "search"
  functions are word-selection helpers).
  UI: the field is a gpui-kit `Input` inside a `TerminalSearch` key context, so `escape` is
  bound there only and Esc in the grid still reaches the program; `TerminalView::key_down`
  also drops keys while that field is focused. Verified on macOS: 123 hits, stepping into
  history scrolls the hit to the viewport centre, Esc/✕ hand the keys back.
- ✅ **Regex search is a flag on the same request** (2026-09-05, protocol 5).
  `TermRequest::Search { needle, max, regex }` compiles the needle with the `regex` crate
  (1.13.1, linear-time, `size_limit` 1 MiB so a pathological pattern cannot blow up the host);
  smart case becomes a `(?i)` prefix, so plain and regex mode agree on casing. Hits are
  reported in chars and then mapped to cells exactly like plain hits; empty matches (`a*`)
  are skipped rather than painting zero-width highlights. An invalid pattern answers
  `TermEvent::SearchInvalid { needle, message }` instead of an empty `Matches`, so the client
  can tell "no hits" from "bad regex" (the bar shows `bad regex`). The `.*` toggle in the
  search bar restarts the search in the other mode and is lit with the accent when on.
  Verified on macOS: `item[13]` plain → none, regex → 4/4, `item(` → bad regex.
- ✅ **Scrollback is bounded by lines, not libghostty's 10 KB byte default** (2026-09-05).
  `Terminal.zig` defaults `max_scrollback_bytes` to 10_000; the engine had only raised the
  line limit, so every session kept about one page (~900 rows). `set_scrollback_max_bytes(None)`
  in the constructor; tests `the_line_limit_governs_retained_history` (20k of 20k kept) and
  `history_is_pruned_near_the_line_limit` (page-granular, bounded).

- ✅ **NACK and refresh requests are datagrams, not control-stream messages** (2026-09-05).
  Measured on the Wi-Fi → mesh path (MEASUREMENTS.md, "screen stream over the mesh"): loss is
  bursty (a typical gap takes 3–6 of a frame's 5–7 fragments, parity included, so FEC recovered
  2 of 29 gaps), and the frames the receiver gave up on had two NACKs out and *nothing* back
  within the 70–83 ms deadline while the host had the frames in its history. The control
  stream is ordered: once the packet carrying a NACK is lost, every retry queues behind it
  until QUIC's loss timer (a PTO, tens of ms on Wi-Fi) retransmits it, so the receiver's own
  retries cannot help. `slopty_proto::screen::Feedback { Nack, Refresh }` now goes
  client → host as a QUIC datagram (postcard body, no header; the host reads datagrams only
  for this), each one standing alone; a fragment list that would not fit a datagram degrades
  to "whole frame". Reports stay on the control stream (they are periodic and idempotent).
  Protocol 3.
- ✅ **Loss deadlines only run while the link is flowing** (2026-09-05). With NACKs as
  datagrams the give-ups did not go away, and the host side explained why: hostd now logs the
  selected path's `cwnd`/`congestion_events`/`lost_packets` and the datagram send-buffer
  headroom on every NACK — QUIC reported **0 lost packets** for the whole run, cwnd wide
  open, buffer empty, and the client's two NACKs for a "lost" frame reached the host 20 ms
  *after* the client had already asked for a refresh. So the Wi-Fi/mesh path does not drop;
  it **stalls** for 100–300 ms and then releases everything at once (both directions), and a
  wall-clock deadline turned every stall into a refresh (IDR) plus dropped frames. Rule in
  `Reassembler::tick`: a NACK is retried only if something arrived since the last one, and a
  frame past its deadline is lost only if newer datagrams are still coming in
  (`now - arrived_at < deadline`); an outright stall is waited out up to `Config::max_hold`
  (500 ms) — a refresh could not cross it either. When the stall clears (a silence of at
  least `Config::stall_gap`, 50 ms, or one NACK round trip if longer) every pending frame's
  clock restarts: the NACK written into the stall only left with the release, so its answer
  is a round trip away from *now*; the first version without this still lost a frame in the
  very tick the burst arrived. The 50 ms floor matters: at LAN round trips the NACK gap is a
  few ms, and without the floor every 17 ms inter-frame gap would count as a stall. Tests
  `a_stalled_link_holds_the_frame_until_it_moves_again`,
  `a_stall_restarts_the_nack_clock_when_the_link_resumes`,
  `a_stall_longer_than_max_hold_gives_up`. Cost: on a *still* screen a genuinely dropped tail
  waits the full 500 ms before the refresh (nothing newer arrives to prove the drop).
- ✅ **hostd binds a fixed UDP port** (2026-09-05): `slopty-hostd --port` / `SLOPTY_PORT`,
  default `slopty_net::endpoint::HOST_PORT` (45550), IPv4 required, IPv6 best effort. Found
  when a hostd restart stranded the paired MacBook: with `--direct-only` a client on another
  subnet knows the host only by the ticket's `ip:port` and cannot hear mDNS, so a random port
  per launch meant re-pairing after every restart. A port in use is a hard error (a silent
  fallback to a random port would bring the problem back); tests pass `--port 0`.

- ✅ **BBR3 congestion control on the QUIC connection** (2026-09-05). Cubic (noq's default)
  cut the host's window to 13–20 KB after 5–9 real losses in 15 s on the Wi-Fi/mesh path;
  at a 10 ms round trip that caps the stream near 10 Mbit/s, a third of the 30 Mbit/s target,
  and media datagrams sit in the send buffer waiting for the window. noq ships BBR3
  (`noq_proto::congestion::Bbr3Config`, a direct workspace dependency since iroh does not
  re-export the module); `transport_config()` installs it for both roles. See
  MEASUREMENTS.md for the before/after window sizes.
- ✅ **Refresh repeats back off** (2026-09-05). A target that never produces a frame (a hidden
  window) left the receiver in "need refresh", re-asking every `refresh_repeat` + 2 rtt — 79
  requests in 10 s. Each unanswered repeat now doubles the wait up to `refresh_repeat_max`
  (2 s); any video datagram resets it, so a live stream still recovers at the fast cadence.
  Settled by the ruling below: backoff is retained as a fallback cap, but an idle target is
  silenced by the host source hint ("The host says when its capture target is idle; the
  receiver stops asking, protocol 13").

- ⚠️ **Never await iroh's `Endpoint::close` on GPUI's executor.** It uses `tokio::time::timeout`,
  which panics (`Handle::current`) outside a tokio runtime context; the app aborted on every
  disconnect until the close was spawned onto the runtime and joined (crash report
  2026-09-04). Rule: anything from iroh/tokio-time runs via `runtime.spawn`, GPUI tasks only
  await join handles and channels.
- ⚠️ **One iroh endpoint per process** (found by the app self-test 2026-09-05). The app used to
  bind an endpoint to pair, close it, and bind a second one with the same secret key to
  connect; the second dial hung until the step timeout (the host never saw its packets;
  iroh's discovery/relay state for that key was still the old endpoint's). `slopty-app::net`
  keeps a process-wide `OnceCell<Endpoint>` used by both `pair_host` and `connect_to`, and
  never closes it.

## Video

- ✅ **HEVC Main (8-bit 4:2:0) default, Main10/P010 when the source is HDR.** AV1 is decode-only
  on Apple silicon (Apple spec pages, verified 2026-09-04). No 4:4:4 hardware path exists.
- ✅ **Encoder property set** — verified against `VTCompressionProperties.h` in the macOS 26.5 SDK:
  - `kVTVideoEncoderSpecification_EnableLowLatencyRateControl` = true, in the **encoder
    specification at session creation** (enforces infinite GOP, no reordering, temporal layers).
  - `RealTime` = true, `PrioritizeEncodingSpeedOverQuality` = true, `ExpectedFrameRate` = fps,
    `MaximumRealTimeFrameRate` = 120.
  - `AllowFrameReordering` = false. `AllowOpenGOP` = false (**HEVC defaults to true**; header
    verified). `MaxFrameDelayCount` = 0 (default is `kVTUnlimitedFrameDelayCount = -1`).
  - `AverageBitRate` in **bits per second** (header verified — slop-desk said bytes; wrong) plus
    `DataRateLimits` `[bytes, seconds]`. `ConstantBitRate` is incompatible with both and pads
    frames; not used. macOS 26 adds `VariableBitRate` + `VBVMaxBitRate` + `VBVBufferDuration`
    (header verified) — ✅ evaluated 2026-09-05, rejected; see "Encoder rate control" below.
  - `MaxAllowedFrameQP` / `MinAllowedFrameQP`: ✅ probed 2026-09-05, both accepted (status 0)
    by the low-latency and the VBV encoder; unused (see below).
  - `SpatialAdaptiveQPLevel` must be disabled under low-latency RC (header says so).
  - LTR: `EnableLTR` = true; per frame `ForceLTRRefresh` (CFBoolean) and `AcknowledgedLTRTokens`
    (CFArray<CFNumber>); output attachment `RequireLTRAcknowledgementToken`. Header verified.
  - `MaxH264SliceBytes` is H.264-only; no HEVC intra-refresh property exists. LTR is the recovery.
  - Keys are read from the framework's exported statics (objc2 bindings), never string literals.
- ✅ **Capture** — verified against `SCStream.h` (macOS 26.5 SDK): `minimumFrameInterval`,
  `queueDepth`, `pixelFormat`, `showsCursor`, `captureResolution`, `ignoreShadowsSingleWindow`,
  `includeChildWindows` (14.2+), `captureDynamicRange` (15+), `capturesAudio` +
  `excludesCurrentProcessAudio`. ✅ Defaults read back from a fresh `SCStreamConfiguration`
  (`slopty_capture::sck_defaults`, macOS 26.5, 2026-09-05): `queueDepth` **8**,
  `minimumFrameInterval` **1/60 s** — slop-desk was right on both. Ruling on the depth below.
- ✅ **Capture latency is measured per frame, host side, and the window's "8 ms floor" was the
  encoder** (2026-09-05). Every ScreenCaptureKit sample carries `SCStreamFrameInfoDisplayTime`
  (mach absolute time when the window server displayed the frame); read through
  `CMClockMakeHostTimeFromSystemUnits` it equals the sample's presentation timestamp to the
  microsecond on both the window and the display path (offset p50 0.00 ms over 285 frames each),
  so `CapturedFrame::latency_us` (display time → our callback) is what SCK adds. Measured
  (MEASUREMENTS.md, "capture floor"): the window filter delivers a scrolling 900×500 terminal
  at **0.46 ms p50 / 0.95–2.35 ms p95**, the display filter at 0.00 / 0.46 ms
  — SCK's own cost is under a millisecond either way, and the "window floor of ~8 ms" recorded
  earlier was the *encode* step: submit → VideoToolbox callback is **6.7 ms p50** for that
  window whatever the capture path and whatever the content (a scrolling and a static window
  encode in the same time), i.e. the hardware encoder's fixed pipeline, not rate control. The
  host keeps both as p50/p95/max rings per stream (`ScreenStats::capture` / `::encode`, 600
  frames) and exposes them on the local control socket (`CtlRequest::Screens`, `slopty host
  screens`); `slopty bench screen` appends them on loopback. Not on the wire: the client has no
  use for the host's internal split, and the protocol number stays with the tracks that need it.
- ✅ **Windows are served as a crop of their display when nothing covers them** (2026-09-05).
  `SCContentFilter(display:excludingWindows:)` + `sourceRect` at the window's frame (points in
  the display's space; `crop_for` is the pure geometry, `Target::resolve_crop` the SCK side)
  delivers the same picture as the window filter (interior mean |Δluma| 0.05/255 on a static
  window; only the corners differ, the window filter leaves them transparent) at the display
  path's latency: end to end (capture → decoded on loopback, `slopty bench screen`)
  **1.7 ms p50 against 9.0 ms** for the window filter in a debug build
  (release: 2.6 ms vs 9.0 ms), the ≥ 3 ms the brief asked for. Rules, in
  `slopty_host::screen::resolve` / `check_geometry`: crop only while the window is entirely on
  one display (`CGGetDisplaysWithRect` + `encloses`) and no on-screen window of another process
  at levels 0–8 overlaps it (`occluded`, from `kCGWindowListOptionOnScreenAboveWindow`); the
  Dock's window is a transparent full-screen hit region at level 20, the menu bar is 24 and
  status items 25, none of which counts. A move is one `updateConfiguration` with the new
  `sourceRect` (~20 ms to complete, frames keep flowing, measured), a resize rebuilds the
  encoder as before, and a window that gets covered or dragged off its display swaps to the
  window filter on the live stream (`updateContentFilter`) and back when clear; the crop is
  cleared *before* the filter swap because a `sourceRect` on a window stream is read in the
  window's own space. Known cost: on the crop path a drag lags by the geometry poll
  (100 ms, was 250) plus the update, during which one edge shows the desktop the window left.
  `SLOPTY_WINDOW_CAPTURE=window|crop` forces a path. Guard: `slopty-capture/tests/latency.rs`
  (gated) fails when the window filter's capture p95 exceeds 1 ms + 3 ms margin.
- ✅ **Rulings of the crop path: when it is allowed, what it must never show** (2026-09-06,
  after the Codex review of the first cut). The crop path is an optimisation of the window
  filter and must be indistinguishable from it to the viewer; wherever it cannot be, the
  window filter serves. `slopty_host::screen::crop_allowed(on_screen, crop, occluded)` is
  the rule, unit-tested; `wanted_crop` / `resolve` feed it from the window server.
  1. *Allowed only for an on-screen window* (`kCGWindowIsOnscreen`, `window_on_screen`): a
     minimised window or one on another Space has a frame but nothing at it; the crop would
     show whatever sits there now. The window filter shows the window wherever it is.
  2. *Allowed only when nothing counts as covering it*, and "covering" is per window, not
     per process: a sibling window of the same app in front is an occluder like any other
     (`counts_as_occluder`, unit-tested with two siblings); only the target's own
     child, sheet, popup and menu windows (same pid, level above normal) are not, since they
     belong to the picture the window filter shows too. Other processes count at levels 0–8;
     the Dock's full-screen hit region (20), the menu bar (24), status items (25) and the
     cursor do not. First cut had excluded the whole pid; a second Ghostty window dragged over
     the target went unnoticed.
  3. *It must never show the desktop after the window closed.* Through the window filter
     ScreenCaptureKit ends the stream itself and the connection reports `Closed`; a display
     stream never ends by itself, so `check_geometry` treats "no bounds for the window" on the
     crop path as the window gone: stops the capture and fires `on_stop`
     (`CaptureError::Stopped("window closed")`) once, the same event the filter path raises.
     Gated test `closing_the_window_under_a_display_crop_ends_the_stream` (`slopty-host`)
     kills the Ghostty window and waits for it. First cut returned "no change" and kept
     streaming the old rectangle.
  4. *Audio is the window's application's only.* `sourceRect` scopes the picture, not the
     sound: a `display:excludingWindows:` filter carries every app's audio. The crop filter is
     `display:includingApplications:exceptingWindows:` with the window's owning app
     (`Target::resolve_crop`; `included_applications()` reads it back), which is what the
     window filter carries. Gated test `display_crop_scopes_audio_to_the_windows_app`
     asserts the filter's app list is exactly Ghostty's bundle id on the crop path, empty for
     a display and for the window filter (no measuring: the configuration is the contract).
  5. *A switch is committed only when ScreenCaptureKit has committed it.* `updateContentFilter`
     and `updateConfiguration` complete asynchronously and can fail; the first cut wrote
     `path` / crop as soon as it had asked. Now `follow_window` opens a `Transition` (path,
     crop, number of outstanding callbacks) shared with the completion blocks and does nothing
     while one is in flight; the next tick settles it: every callback ok → path and crop
     become the stream's state; any failure → nothing changes and the tick asks again, since
     the window server still says the same thing. Unit-tested with a failing result. Order
     is unchanged: crop cleared before the swap to the window filter, filter swapped before
     the crop is set.
  6. *Host-side stats are matched on (client, stream)*: stream ids are per connection, so
     `slopty bench screen` looks its own stream up by its client id too, not the first
     connection that happens to have a stream of that number.
- ✅ **`queueDepth` stays 2** (2026-09-05). Measured 2 / 3 / 5 / 8 on the window filter
  (MEASUREMENTS.md, "capture floor"): p50 is flat (0.46 → 0.49 ms) and p95/max grow with the
  depth; no drops at any depth on a 60 Hz source. The default 8 buys nothing here since the
  callback hands the surface straight to the encoder.
- ✅ **Encoder rate control: keep `EnableLowLatencyRateControl` + `AverageBitRate` +
  `DataRateLimits`; the macOS 26 VBV keys are out** (2026-09-05). The VBV keys
  (`VariableBitRate`, `VBVMaxBitRate`, `VBVBufferDuration`) are documented incompatible with
  low-latency rate control and the probe agrees: the low-latency session is a *different*
  encoder whose `VTSessionCopySupportedPropertyDictionary` does not list them (it lists
  `EnableLTR`, `DataRateLimits`, `AverageBitRate`, no `MaxFrameDelayCount`, no
  `PrioritizeEncodingSpeedOverQuality`), while a session created without the specification
  lists them plus `LookAheadFrames`, `MCTFLatencyMode`, `AllowOpenGOP` — and loses LTR
  (`EnableLTR` rejected, `ltr false`), which is Slopty's loss recovery. Measured head to head on
  the same 900×500 Ghostty window at 8 Mbit/s (MEASUREMENTS.md, "encoder rate control"):
  encode p50 6.70 vs 6.59 ms — identical; frame-size spikes max/p50 **2.0×
  vs 5.6×** and 64 vs 163 kbit/s for the same picture — the VBV encoder
  spends more and spikes more. Nothing to adopt. Also tried on the low-latency encoder
  (`Encoder::set_property`, public for benches): `MaximizePowerEfficiency=false` (6.69 ms
  p50), `ExpectedFrameRate=120` (6.73 ms), H.264 instead of HEVC (6.45 ms): all within run-to-run noise of the 6.70 ms baseline (H.264 buys 0.25 ms p50 and loses 0.3 ms at p95 on the static window), so none is adopted.
  `MaxAllowedFrameQP` / `MinAllowedFrameQP` are accepted by both encoders (status 0) but stay
  unset: they trade frame drops for a QP bound, and a dropped frame is a hole the client asks
  a refresh for. The ~7 ms is the encoder's pipeline; the remaining glass-to-wire budget is
  there, behind keys the SDK does not export (`RequestedMaxEncoderLatency` shows in the
  low-latency encoder's supported list but is not in the 26.5 headers, so it is not used).
- ✅ **FEC**: `reed-solomon-simd` 3.1.0 (NEON), systematic RS per frame, redundancy adaptive
  (~20 % default, Sunshine's number). NACK inside the playout window before LTR refresh.
- ✅ **Adaptive bitrate** (`slopty_media::RateController`, 2026-09-05). The client's
  `Quality::bitrate_bps` is a ceiling, not a rate: a stream starts at min(ceiling, 12 Mbit/s)
  and the host judges every 10 receiver reports (~0.5 s at the client's 50 ms cadence).
  Overuse — datagram loss > 2 %, present queue ≥ 3, or hold p95 > 60 ms — cuts to 75 % and
  holds 4 decisions; a clean window (loss ≤ 0.5 %, queue ≤ 1, hold p95 ≤ 25 ms) grows by an
  eighth (≥ 0.5 Mbit/s); everything else stays. On top, the selected QUIC path's window caps
  the target at 90 % of `cwnd × 8 / rtt` (`slopty_net::endpoint::selected_path`): datagrams are
  congestion-controlled, so sending past cwnd only fills the host queue (`queue_full`).
  Applied with `VTCompressionSession` `AverageBitRate` in place (no encoder rebuild, no IDR);
  a `SetQuality` or resize re-clamps under the new ceiling and keeps the controller's place.
  Floor 1 Mbit/s. Rejected: GCC's Kalman delay-gradient estimator — the receiver already
  measures hold time and queue depth directly, which is the same signal without the filter;
  and TWCC-style per-packet feedback, which the 50 ms report already approximates.
  `ScreenStats.bitrate_bps` shows the last target in the close log. First mesh run in
  MEASUREMENTS.md: the cap took the start from 12 to 9 Mbit/s and a stalling afternoon link
  drove the target to the floor.
- ✅ **Stall-aware bitrate** (2026-09-05). A Wi-Fi stall (packets held 100–300 ms, then
  released together; the host dropped nothing, see "stalls, not drops" in MEASUREMENTS.md)
  looked like loss to the controller and got a cut it did not deserve: sending less does not
  clear a stall. *Wire:* `ReceiverReport` carries `stalled_ms` (silence past the reassembler's
  stall gap charged to the report window, a stall still on at report time included up to now)
  and `stalls` (releases in the window). Two fields, not one `flowing` flag, because a report
  is a 50 ms point sample and a stall of 150 ms can start and release between two of them;
  the count alone would miss a stall still on, the time alone cannot show two short ones. A
  third `stalled_ms` per stall was not needed: the policy only asks "was there a stall in this
  window". `ScreenEvent::Rate { target_bps, verdict }` goes back to the client on every
  decision so the ⌘⇧I overlay and `slopty bench screen` show the host's target and whether it
  is holding; before, the target lived only in hostd's log on another machine.
  `PROTOCOL_VERSION` 7 → 8, goldens `client_screen_report` and `host_screen_rate`.
  *Policy:* `slopty_media::judge` is a pure function of the decision window
  (`RateWindow`): any stall in the window → `Stall`, freeze (no cut, no grow, the cooldown
  neither restarts nor ticks, the window's loss is discarded — it is the receiver giving up on
  frames the link still holds); else the overuse / clean rules as before → `Cut` / `Grow` /
  `Steady`. The two reports after a stall (`SETTLE_REPORTS`) contribute loss but not queue or
  hold: the release fills the present queue for a moment and that burst must not be judged.
  Loss in those reports still counts, so a stall followed by real loss cuts once. The cwnd cap
  applies in every state, a stall included. Table of report sequences → target trajectory in
  `report_sequences_and_their_target_trajectories` (clean → grow; loss → cut then cooldown;
  stall → hold then grow; stall then loss → one cut; a stall inside a cooldown neither cuts
  nor ends it), the burst rule in `the_release_burst_after_a_stall_is_not_judged`, the
  reassembler's charging in `reports_carry_the_stall_time_and_count`. Evidence from the mesh
  in MEASUREMENTS.md ("stall-aware bitrate").
- ✅ **Capture heartbeat** (2026-09-05). The reassembler's stall clock counts silence on the
  link, but a screen that does not change is silent too: ScreenCaptureKit delivers no frame
  for a still display, and its warm-up after the first frame is a 300 ms hole. Every such
  gap read as a stall and froze the controller for a window (0.5 s of growth lost at every
  start, see "stall-aware bitrate" in MEASUREMENTS.md). *Wire:* `Kind::Heartbeat` (4), a bare
  16-byte `MediaHeader` with no body, `frame` reused as a beat counter; `PROTOCOL_VERSION`
  8 → 9, golden `media_heartbeat`. A header-only datagram over a new control message
  because it has to travel the same datagram path the stall clock watches (a reliable-stream
  message would arrive through a different queue and prove nothing about the datagram path)
  and because the reassembler already resets `arrived_at` for any datagram of its stream
  before looking at the kind: the beat resets the stall clock and nothing else — not
  `any_arrived`, not the frame or loss counters — so a report window full of beats is clean
  and the controller grows (`heartbeats_keep_a_quiet_source_from_reading_as_a_stall`: 400 ms
  of beats → `(stalled_ms, stalls) = (0, 0)` and `Grow`; the same 400 ms without → `(400,
  1)`). *Host:* `ScreenStream` records the time of its last successful push; the cursor
  sampler (already ticking at 120 Hz) pushes a beat whenever that is `HEARTBEAT_AFTER` =
  25 ms old, half the receiver's 50 ms `STALL_GAP`, so one lost beat still does not read as a
  stall. Counted in `ScreenStats::heartbeats`. The client ignores `Ingest::Heartbeat`.
- ✅ **The start-up stall was the client, not QUIC** (2026-09-05, corrects the reading in
  the capture-heartbeat entry above and its MEASUREMENTS.md section). The new e2e test
  `screen_start_up_over_iroh` (five cold connections to one hostd, direct-only loopback,
  each opening the first display at the app's default quality) stamps four instants on the
  client: `Open` sent, first datagram, first frame complete, first decoded picture. Before
  the fixes: opened at 353 ms on a fresh hostd (176–191 ms after), first frame complete
  5 ms later, first picture **168 ms** after that, and a 145–151 ms "stall" in the first
  decision window. The 168 ms is `VTDecompressionSessionCreate` for the first session in a
  process (3 ms for every later one); it ran inside the stream worker, which read no
  datagram meanwhile, so when it caught up the reassembler saw 150 ms of silence and charged
  it as a link stall, freezing the first rate window (the "QUIC send-side hold" the heartbeat
  section blamed). Rules that follow, each measured in the same test:
  * `slopty_codec::warm_up` builds one HEVC session from canned 64×64 parameter sets;
    `slopty_client::warm_up_decoder` runs it once per process on its own thread and is
    called at app launch (`open_workspace`), on the CLI's connect, by `HostLink::start`
    (`crates/slopty-client/src/link.rs`), and by the test. First picture 526 → 319 ms on
    the first open in a process.
  * `slopty_host::screen::warm_up` builds and drops a 64×64 HEVC encoder, then starts and
    stops a 64×64 capture of the first display, when hostd comes online. The capture-only
    version did not move the first `Opened` (270–295 ms); the encoder did (→ 127–140 ms):
    VideoToolbox's first compression session in a process is ~170 ms, the later ones ~10.
    `shareable()` keeps its last enumeration for 2 s (`SHAREABLE_TTL`) because a client
    lists, picks and opens within seconds and each enumeration is 60–75 ms.
    `Opened` 176–191 → 113–138 ms, first open included.
  * The datagram pump publishes how many bytes QUIC is holding (`DatagramBudget::held`,
    4 MiB buffer minus `datagram_send_buffer_space`) and logs every episode; the capture
    callback drops a frame, flagged for an LTR refresh, while more than two frames' worth at
    the current target (32 KiB floor) is held (`frame_fits`), and a NACK is answered only
    under the same condition. Over the mesh at a 4800-byte window the pump had held 275–365 KB
    for 4–7 s — every frame delivered stale — and 851 NACK answers piled 64 221 datagrams
    behind 12 frames. Stale frames and stale retransmits are worth nothing; a fresh keyframe
    after the drop is. (`a_frame_fits_unless_the_queue_or_quic_holds_too_much`)
  * The client router stamps every datagram with the instant the connection handed it over
    and the worker ingests with that instant; the worker drains everything already queued
    (a burst, or the backlog from before the stream was attached) before the timers look at
    frame ages. A slow worker can no longer read as a stalled link or as a lost tail.
  * The bitrate controller's cwnd cap is taken from the widest path sample in the decision
    window, not the last: BBR's `ProbeRTT` shrinks the window to four packets for 200 ms
    every 5 s and a cap read then cut a loopback stream to 6.4 Mbit/s for a window
    (`a_probe_rtt_dip_inside_the_window_does_not_cap_the_target`).
  * The ⌘⇧I overlay is two lines built by the pure `hud_lines`: picture (size, fps, Mb/s,
    rtt, age of the frame on screen) and path (jitter, hold p50/p95, queue, fec/lost/nack/
    refresh, stalls, the host's verdict, audio). `ScreenStats` carries the report's hold
    and jitter figures and the four start-up instants; `slopty bench screen` prints them.
  Loopback after all of the above, machine quiet: 0 NACKs, 0 stalls, 0 holds ≥ 5 ms in
  5 × 3 s under BBR3 and under Cubic (tables in MEASUREMENTS.md). Settled on a quiet machine
  (MEASUREMENTS.md, "start-up over iroh on a quiet machine"): 0 stalls and 0 NACKs across all
  samples, confirming the loaded-run stalls were the scheduler; the mesh run's held-frame drop
  is the one ruling from it.
- ✅ **Packet layout** (`slopty-media`, 2026-09-04): body = 16-byte `FramePrefix` (bitstream
  length, capture µs, LTR token) ‖ bitstream ‖ zero pad, cut into *balanced* fragments (all the
  same even size ≤ 1184 B, so padding ≤ 2 B per fragment and the RS shard size is inferred from
  any fragment); parity indexes follow the data indexes in the same header. The prefix rides
  under parity so a recovered frame recovers its metadata. Max 32 768 data + 255 parity
  fragments per frame; every such pair is a supported RS configuration (tested).
- ✅ **Loss policy** (`Reassembler`): deliver strictly in order; NACK after 3 ms of silence on an
  incomplete frame (fragments leave the host back to back), at most 2 tries one RTT apart; a
  frame that never showed up at all is NACKed whole (`fragments = []`); give up after
  `3 ms + 2·(RTT + 3 ms) + 10 ms`, then `RequestRefresh{last_good}` and ignore everything until
  an IDR or LTR-refresh frame. Parity is decoded only when data fragments are missing. Late
  parity for an already complete frame is dropped unread. Settled by the rulings below: initial
  NACK delay derives from round trip ("NACK delay from the round trip"), deadlines pause during
  stalls ("Loss deadlines only run while the link is flowing"), and the give-up deadline is kept
  ("The NACK give-up deadline stays where it is").
- ✅ **Redundancy control** (`Redundancy`): parity permille = clamp(2 × EWMA(datagram loss) +
  50, 50, 500), ×1.5 bump when a report shows a frame lost outright. Settled by the ruling
  below: "Parity tracks loss asymmetrically, with a deadband" (asymmetric rise ½ / fall ⅛,
  2 % deadband, stalled windows excluded from the estimate).
- ✅ **Host pipeline** (`slopty-host::screen`, verified on hardware 2026-09-04): one
  `ScreenStream` per open stream: SCK frame → `Encoder::encode` on the SCK queue → packetize in
  the VideoToolbox output callback → bounded datagram queue (4096) → one pump task per
  connection calling `Connection::send_datagram`. Backpressure drops whole *captured* frames
  when fewer than 256 slots are free and flags the next frame as an LTR refresh; a full queue
  mid-frame drops the rest of that frame (the reassembler NACKs). Encoder sessions are
  `Send + Sync` (VideoToolbox is documented thread-safe); the packet sink holds a `Weak` so an
  encoder never keeps its own stream alive.
- ✅ **Datagram budget**: QUIC's datagram limit starts near 1168 B at the 1200-byte initial MTU
  and grows with path-MTU discovery, so the protocol's 1200-byte `MAX_DATAGRAM` is a ceiling,
  not a promise. The pump reads `Connection::max_datagram_size()` into a shared
  `DatagramBudget`; the packetizer cuts the next frame to it (`Packetizer::set_max_datagram`),
  and a datagram cut before the budget shrank is discarded rather than rejected by QUIC.
- ✅ **Stream ids** are allocated per connection by the host, starting at 1; `WindowId` *is*
  the `CGWindowID` (clients open against a listing they just received, so reuse is harmless).
- ✅ **Cursor channel** samples `CGEventGetLocation` at 120 Hz on the host, re-reads the
  target's bounds at 10 Hz (`CGWindowListCreateDescriptionFromArray` / `CGDisplayBounds`), and
  sends a datagram only when the stream-pixel position or visibility changed.
- ✅ **Client receive path** (`slopty-client::screen`, verified end to end 2026-09-04):
  `HostLink` reads every datagram and a `ScreenRouter` fans out by stream id, keeping a
  512-datagram backlog for streams whose `Opened` has not been processed yet (datagrams beat the
  control stream). One task per stream: reassemble → decode; the reassembler's timers run at
  2 ms while frames are pending and 50 ms when idle; a receiver report goes out every 50 ms.
  The newest decoded frame and the cursor position are `watch` channels: the UI paints the
  latest and never queues video. Dropping the handle unroutes the stream; the caller still
  sends `Close` so the host stops capturing.
- ⚠️ **Unsupported encoder properties on Apple silicon** (M-series, macOS 26.5, observed
  2026-09-04): `AllowOpenGOP`, `MaxFrameDelayCount` and `PrioritizeEncodingSpeedOverQuality`
  return `kVTPropertyNotSupportedErr` under low-latency rate control. They are optional in
  `Encoder::new` and logged at debug; low-latency mode already implies no reordering and no
  frame delay, so nothing is lost.
- ⚠️ **`CMTime` → µs must use 128-bit math**: the host clock is nanoseconds since boot, so
  `value × 1e6` overflows `u64` after a few hours of uptime and silently saturates (found when
  every latency read 0). Fixed in `slopty-codec::cf::micros`; unit-tested with a 12-day value.
- ✅ **420f end to end.** Capture asks SCK for `kCVPixelFormatType_420YpCbCr8BiPlanarFullRange`
  (`PixelFormat::Nv12Full`), and the decoder's output attributes pin the same format with
  `kCVPixelBufferMetalCompatibilityKey`, because GPUI's `metal_renderer` asserts that exact
  format (`assert_eq!` in `draw_surfaces`) and samples the two planes as R8/RG8. Any other
  format would either abort the app or need a colour-space conversion on the client.
- ✅ **Present**: `CAMetalDisplayLink`-driven tick; `displaySyncEnabled = false`, drawable count 2;
  present on arrival (superseded 2026-09-05: slop-desk vsync-locked hypothesis settled by
  present-on-arrival measurement without buffering; see ruling below).
- ✅ **Present on arrival, and the presentation path is measured rather than assumed**
  (2026-09-05). A decoded frame goes up on the first paint after the decoder returns it and is
  never queued for a later one. The alternative — a one-frame playout buffer, which is what a
  video player does — buys evenly spaced pictures at the price of a whole frame of latency on
  *every* frame, and a remote desktop is judged on how long after a keystroke the screen moves,
  not on how evenly it moves. So the only decision left is what to do when the decoder is ahead
  of the display, and the answer is drop, not queue: `Pacer::offer` replaces the frame waiting
  to be painted and counts it (`skipped`) rather than lining up behind it, because by the paint
  after next that picture would already be wrong. The mechanism was mostly there — the `watch`
  channel between the worker and the element keeps only the newest frame — but nothing measured
  it and one path did buffer: a frame the reassembler released inside `tick` (freed when the
  frame ahead of it was given up on) sat in `ready` until the *next datagram* arrived, which on
  a still screen is a heartbeat away. The worker now drains after every tick.
  *Instrument:* the reassembler stamps each complete frame with the arrival of the datagram that
  finished it (`FrameOut::arrived`), the worker parks that instant under the frame's
  presentation timestamp (VideoToolbox's callback is handed nothing else to correlate on) and
  the callback picks it up, so the element receives a `Presentable` and its `Pacer` can close
  the clock at the paint. A ring of the last 240 presented frames gives arrival → present
  p50/p95/max, the decoder's share, the paint spacing and its jitter, plus `skipped` /
  `repeats` / `late` — the third line of the ⌘⇧I overlay and `ScreenInfo` in the app self-test's
  dump. The policy is pure with an injected clock (`slopty_client::pacing`), so a steady source,
  a source faster than the display, a source slower than it, a straggler and the ring's own
  forgetting are all unit tests; the element only feeds it a frame on one side and a paint on
  the other. Over a live iroh stream (MEASUREMENTS.md, "arrival → present"): p50 9.0–12.6 ms,
  p95 17.1–20.4 ms, decode 2.6–2.9 ms, `late` 0. A paint interval is 16.7 ms, so
  present-on-arrival predicts p50 ≈ decode + half an interval and p95 ≈ decode + a whole one;
  both land there, and a single frame of playout buffering would have added 16.7 ms to each.
  There is no room in the numbers for a buffer, which is the ruling's evidence.
  *Two things the ordering rests on, both found in review.* The wire carries the low 32 bits of
  the host's microsecond capture clock, which wraps every ~71.6 minutes; widening it with
  `u64::from` would make the first frame of the new turn compare older than the last of the old
  one and freeze the picture until the raw value climbed back past it — up to another 71
  minutes. `pacing::CaptureClock` counts the wraps instead (a step back of more than half the
  range is a wrap forward, a step forward of more than half is a straggler from before one) and
  is applied at the single point where the stamp becomes a `u64`, so the decoder echoes the
  widened value back and the parked arrivals inherit it. And `skipped` has to be counted on the
  decoder's side of the channel: the `watch` between the worker and the element keeps only the
  newest frame, so a burst finishing between two paints reaches `offer` as its last member
  alone and the counter would read zero while the display missed three. Each callback stamps a
  `decode_seq`, and the pacer reads the gaps — the only trace those frames leave.
  Not built: a jitter-adaptive delay. It would only earn its latency on a link whose delivery
  jitter exceeds a frame interval, and the same overlay now says whether that is happening.
- ✅ **NACK delay from the round trip, floor 1 ms, ceiling 20 ms** (2026-09-05). The delay was a
  fixed 3 ms. It exists only to outlast the spread of one frame's fragments on the wire — they
  leave the host back to back, so silence after them means loss, not pacing — and that spread
  scales with the path's delay variation, which scales with its round trip. A constant is
  therefore wrong at both ends: on loopback (RTT ≈ 0.6 ms) 3 ms is two extra milliseconds
  before every repair, and on a 40 ms link it fires while the fragments are still in flight and
  answers a NACK storm with retransmissions nobody was missing.
  `NackDelay { min: 1 ms, max: 20 ms, divisor: 4 }` makes it `rtt / 4` between the bounds — the
  reordering tolerance TCP RACK uses (`min_rtt / 4`), for the same reason. The bounds carry more
  weight than the fraction: without the floor, scheduling noise on a loopback link reads as
  loss; without the ceiling, a satellite path would hold a repairable frame past the point where
  the retransmission could still be shown (`max_hold`, 500 ms, still bounds it). The whole loss
  deadline follows, since it is `delay + retries × (rtt + delay) + grace`: loopback 20.2 → 14.2
  ms, a 40 ms link 99 → 120 ms. The storm gate from the start-up track is untouched — a retry
  still needs a datagram to have arrived since the last NACK, the deadline still only counts
  while newer datagrams are coming in, and an outright stall is still waited out. `tick` derives
  the delay from the RTT it is handed and caches it, so `stalled` and the resume path (which
  have no round trip to hand) use the same number. Tested against a table of fake RTTs
  (loopback / LAN / Wi-Fi / mesh / satellite → 1 / 1 / 3 / 10 / 20 ms), for monotonicity and
  bounds across a thousand round trips, and end to end in the pipeline tests, which now derive
  their own advances from the policy.
- ✅ **Cursor** is a separate channel drawn client-side; capture with `showsCursor = false`.
- ✅ **Audio**: SCK `capturesAudio` → `opus` 0.4.0 (verified; `audiopus` is dead) → `objc2-avf-audio`.

## Audio

- ✅ Opus via Apple's `AudioConverter` (`kAudioFormatOpus`), no libopus: the encoder exists on
  macOS 26.5 (verified 2026-09-05: `encodes_and_decodes_a_tone` round-trips a 440 Hz tone;
  the first packet comes back 120 frames short, Opus pre-skip) and the decoder on both
  platforms. One fixed configuration, 48 kHz stereo float interleaved, 960-frame packets at
  96 kb/s, so nothing about the format travels on the wire. Playback is an `AudioQueue` with
  three 20 ms buffers refilled from a mutex-guarded ring (≤200 ms; underrun pads silence and
  counts, overrun drops the oldest). The input-proc pattern: the proc hands its one slice and
  then returns a private status (`'SLOP'`) with zero packets, which `FillComplexBuffer`
  surfaces and the caller treats as "done".
- ✅ Capture rides the video `SCStream` (`capturesAudio`, `sampleRate 48000`, `channelCount 2`,
  `excludesCurrentProcessAudio`) with a second stream output of type `Audio` on the same
  serial queue. `CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer` returns
  `kCMSampleBufferError_ArrayTooSmall` (-12737) unless the list is sized by a first call with
  a null list, so the code asks for the size, allocates 8-byte-aligned words, then fetches;
  ScreenCaptureKit delivers float32 non-interleaved, which `interleave` folds to L/R.
- ✅ **Mute is client-side, decode-but-don't-play** (2026-09-05). `ScreenHandle::set_muted`
  flips an `AtomicBool` the worker reads per packet; the Opus decoder keeps running so its
  state stays continuous and unmuting resumes on the next 20 ms packet. Nothing goes to the
  host: a host-side stop would need a protocol change and would also silence the other
  clients, and the muted stream costs ~12 kB/s, less than one video frame. The pill shows on
  the active window once audio has arrived, and on every muted window regardless, so a
  silenced item is never mistaken for one whose sound stopped.
- ✅ Transport: one `Kind::Audio` datagram per packet (~240 B), sequence in the frame field,
  no parity and no retransmit; the client counts gaps as `audio_lost` and the ring pads. Host
  gate: after 300 ms without a sample above -80 dBFS no packets go out, so the many silent
  windows on a canvas cost nothing; the first loud chunk reopens it (the 300 ms hold keeps
  natural pauses from chattering). Measured 2026-09-05 on loopback (display 6, `afplay`
  Glass.aiff ×3, 5 s): 249 packets sent, 246 received, 0 lost, bench player audible.
- ✅ **Clipboard sync is polled on the host and pushed ahead of ⌘V on the client**
  (2026-09-05, protocol 4). `NSPasteboard` has no change notification, only `changeCount`,
  so hostd reads it every 200 ms (one Mach call to the pasteboard server); a client-side
  watcher was rejected because GPUI's clipboard API has no count and the client would have to
  push on every poll. The client instead pushes exactly when it matters: the paste chord
  goes to the host on the same ordered stream right after `ScreenRequest::Clipboard`, so the
  host's paste already sees the text. Only clients with a window open receive the host's
  clipboard (the connection filters the broadcast) and the client never writes text it
  already holds, which is what stops the loopback echo when app and host share a Mac.
  Text only; files and images stay local. `cargo xtask bundle` builds `Slopty.app` with the
  daemons and CLI beside the app (`Contents/MacOS/slopty host install` works unchanged since
  the installer copies from its own directory), ad-hoc signed unless `--sign` names an
  identity.
- ✅ **Short audio gaps are concealed by replaying the last packet with a fade** (2026-09-12,
  `slopty_codec::audio::Conceal`). Apple's Opus decoder takes no empty packet for its own
  concealment, so the client keeps the last decoded packet and, on a sequence gap of up to
  `MAX_CONCEALED` = 3 packets (60 ms), pushes it again faded linearly to silence across the
  gap before the packet that arrived: a lost packet is a dip, not a click, and the ring keeps
  the gap's 20 ms per packet so the next real packet is not played early. Longer gaps stay
  silence (replaying 60 ms of anything sounds worse than a pause, and the ring underruns to
  silence by itself). Counted as `audio_concealed` in the client's `ScreenStats` and on the
  overlay's second line. Unit-tested on the fade shape and the cap; the loss-injection app
  self-test (`SLOPTY_E2E_DROP_PERMILLE`) exercises the path. Still ⏸: a jitter estimator
  with an adaptive prefill (the ring starts playing at once and holds at most 200 ms; no
  measurement has shown late packets as a problem on the links tried). No `Quality` field for
  audio on purpose: keeping it off the wire avoided a protocol bump, and the gate makes "on"
  free.

## Input

- ✅ **`slopty-input` = `CGEvent` injection, one `Injector` per screen stream** (verified end to
  end 2026-09-04: keys typed into the Slopty app landed in a streamed Ghostty window). Stream
  pixels map back to global points through the target's current bounds (cached 100 ms) and the
  stream's pixels-per-point scale, which `ScreenStream` updates on every quality change.
  Window streams post with `CGEventPostToPid` to the owner; display streams post to the HID tap.
  Keys carry the client's text via `CGEventKeyboardSetUnicodeString`, so host and client
  layouts need not agree; bare modifier keys post as `FlagsChanged`. Needs post-event
  (Accessibility) access: `slopty-hostd` preflights at start-up, warns, and asks once.
- ✅ **The injector decides, a `Backend` posts** (2026-09-05). `Injector<B: Backend>` maps
  pixels to points, tracks held buttons and flags, picks the route (`Pid` vs `Hid`; right
  clicks always `Hid`) and activation, and hands a `Post { route, flags, event }` to the
  backend. `System` builds the `CGEvent`; `Recorder` keeps the `Post`s. The whole decision
  path is unit-tested against `Recorder` under `cargo gate` with no Accessibility grant and
  no real event. The one live test (`SLOPTY_INPUT_E2E`) checks only that a display-stream
  move lands, and runs via `cargo xtask e2e input`. Hand-driven checks (keys into a pid +
  window screenshots + reading them back) are banned: see `CLAUDE.md` ▸ Live tests.
- ⚠️ **macOS delivers keyboard events only to the active app.** Events posted to an inactive
  pid queue up and all land the moment the app activates (observed macOS 26.5: four probe keys
  arrived as `echoecho` after activation). So the injector activates the owner
  (`NSRunningApplication::activateWithOptions`) before a button-down or key press when it is
  not active, and the client sends `Focus` when a screen view gains focus. The host's own
  desktop sees that app come to the front; there is no public API around this.
- ⚠️ **`CGWindowListCreateDescriptionFromArray` takes raw `CGWindowID`s as array values**, in a
  `CFArray` built with NULL callbacks, not boxed `CFNumber`s: with numbers it returns an empty
  array (observed macOS 26.5; the docs say "array of window IDs"). `window_bounds` returned
  `None` for every window until this was fixed; the gated `slopty-capture` geometry test pins it.
- ⚠️ **Initialise CoreGraphics before ScreenCaptureKit in a daemon.** `SCContentFilter
  initWithDesktopIndependentWindow:` calls SkyLight, which aborts with
  `Assertion failed: (did_initialize), function CGS_REQUIRE_INIT` when nothing has connected the
  process to the WindowServer yet (a hostd whose first SCK call is a window filter, e.g. a
  client reconnecting with a persisted window item). `slopty-capture` calls `CGMainDisplayID`
  once before any enumerate/resolve.
- ✅ **⌘ chords go to the remote window unless the canvas binds them** (2026-09-04). GPUI runs
  key bindings before key listeners, so ⌘T/⌘N/⌘O/⌘W/⌘0/⌘1/⌘=/⌘- never reach `ScreenView`;
  every other chord (⌘C/⌘V/⌘Z/⌘S/⌘K…) is forwarded with `Mods::SUPER` and no text (GPUI gives
  no `key_char` for ⌘ chords; the injector's virtual keycode carries it). Verified: ⌘K from the
  app reached hostd as `Key { K, Press, SUPER }` and cleared the streamed Ghostty. The view
  tracks pressed keys so a release whose press was eaten by a canvas binding is not forwarded.
  ⌘Q/⌘H/⌘M stay with the app (menu bar). Modifier-only presses are not forwarded (GPUI has no
  key-down for them); the injector sets flags per event instead.
- ⚠️ **An absolute GPUI element without insets sits at its static position.** `ScreenView`
  recorded its bounds from a `canvas().absolute().size_full()` placed *after* the picture, so
  Taffy put it one body-height below the real top: every pointer event mapped ~834 px too high
  and the injector clamped it to the window's top edge (clicks "worked" only by landing on the
  title bar). Fixed with `inset_0()`; the canvas viewport recorder got the same for safety.
- ✅ **Host window resizes are polled, not observed** (2026-09-04). ScreenCaptureKit keeps
  scaling a window into the old output size (a 600×830 window in a 1264×834 stream looked
  2× wide), so `ScreenStream::check_geometry` (one `CGWindowListCreateDescriptionFromArray`
  per stream, every 250 ms from the hostd connection loop) compares the target's bounds with
  the stream's native size, rebuilds encoder + capture at the new size with a keyframe, and
  emits `ScreenEvent::Geometry`. The client's `ScreenView` updates its native size and the
  canvas gives the item the new aspect (width kept, `Place`). Verified: 600×830 → 900×500 changed
  the item to 1264×736 and the picture painted unstretched. No public resize notification
  exists for another app's window short of AX observers, which need the same polling fallback.
- ✅ **Magnify is ignored** for now: there is no public constructor for gesture `CGEvent`s.
- 🔬 Host daemon ships non-sandboxed and Developer-ID signed (App Sandbox blocks
  `CGEventPost`; slop-desk claims macOS 26 drops modifier combos from unsigned processes —
  unverified; verify with a ⌘ chord once the daemon is signed).

## Testing

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
  verdict. Two real bugs surfaced on the first run (below).
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
  five shells (`add_display`, needs `SLOPTY_SCREEN_E2E`), and types 60 letters at 15 a second
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

## Tooling (versions verified on crates.io / GitHub 2026-09-04)

- ✅ **The gate has a budget: 5–10 minutes warm, without losing a check** (user, 2026-09-05).
  What changed to meet it: every step prints its wall time (`step`), so a slow gate names its
  culprit; the tools that never touch `target/` (deny, shear, typos, taplo, committed) run on
  a second thread beside the cargo steps with their output captured and printed whole
  (`quiet_step`), since the cargo steps serialise on the build lock anyway; the two iOS
  triples are one `cargo clippy --target … --target …` invocation (one resolve, both targets
  compiled side by side) without `--all-targets` — tests never run on iOS and their code is
  the host's, so the iOS pass checks libraries and binaries; `rustc-wrapper = "sccache"` in
  `.cargo/config.toml` shares dependency compiles between the main checkout, the parallel
  sessions' worktrees and the triples (workspace crates stay incremental, which sccache passes
  through). Timings in MEASUREMENTS.md "gate wall time". Not done: a separate target dir per
  step to overlap cargo invocations (would double the disk and the cold builds).

- ✅ Rust 1.98.1 pinned; edition 2024; resolver 3; `[workspace.lints]` with clippy
  all/pedantic/nursery/cargo + curated restriction lints; `panic = "unwind"` everywhere
  (`panic = "abort"` turns any ObjC exception crossing objc2 into a process abort).
- ✅ **Dependency refresh is a routine, not an event** (2026-09-12): `cargo upgrade --dry-run
  --incompatible` (cargo-edit) lists what the reqs hold back, `cargo update` moves the rest;
  bump the reqs in `Cargo.toml` and the tool floors in `xtask::setup`, then gate and e2e.
  The one crate that must not follow crates.io is `core-video`: `gpui::SurfaceSource:
  From<CVPixelBuffer>` is typed against the fork's version (0.5.2 while zed stays there), so a
  bump to 0.6 fails `slopty-ui` — it moves when the fork's does (comment beside the req).
- ✅ nextest 0.9.144 · insta 1.48 · proptest 1.11 · cargo-mutants 27.1 · cargo-llvm-cov 0.9 ·
  cargo-deny 0.20.2 · cargo-shear 1.13.4 · cargo-hack 0.6.45 · cargo-semver-checks 0.50 ·
  typos 1.50.1 · taplo 0.10 · prek 0.5.2 · bacon 3.25 · samply 0.13.1 · tracing-tracy 0.12.
- ✅ **Releases from Conventional Commits**: `committed` 1.1.11 lints every message (commit-msg
  hook via prek, and `cargo gate` over the range since the last tag); `git-cliff` 2.14.1 computes
  the next version (`--bumped-version`, pre-1.0 rules: breaking → minor, feat → patch) and writes
  `CHANGELOG.md`; `cargo xtask release` glues them and tags `vX.Y.Z`. Rejected: cocogitto
  (last release 2026-03, overlaps both), release-plz / cargo-release (crates.io-centric; nothing
  here is published).
- ✅ **App icon rendered from one SVG at build time** (2026-09-05). `assets/icon.svg` (dark
  full-bleed square, faint dot grid for the canvas, an accent `❯` and a green cursor block) is
  rasterised by `xtask::icon` with `resvg` 0.48.1 + `tiny-skia` 0.12 (MPL-2.0/BSD, already
  allowed) into `Slopty.icns` (16…512 pt at 1× and 2× via `icns` 0.4.0) for `cargo xtask
  bundle` (`CFBundleIconFile`) and a 1024 px PNG in a generated `Assets.xcassets` for `cargo
  xtask ios` (`ASSETCATALOG_COMPILER_APPICON_NAME`). No `iconutil`/`sips`/design tool; `cargo
  xtask icon [dir]` writes the PNG ladder for a look. Full bleed on purpose: macOS 26 and iOS
  apply their own squircle mask, so baked-in rounded corners would double up. Verified: the
  Dock shows the icon for the debug bundle and the iOS 26.5 simulator home screen shows it
  after `cargo xtask ios sim`.
- ✅ Miri only for pure crates (cannot cross objc2 FFI); cargo-careful + ASan/TSan nightly lane for
  the rest; `leaks --atExit` for framework wrappers (Valgrind does not exist on Apple silicon).
- ✅ No mold/lld on macOS (Apple's ld-prime is competitive; mold's Mach-O port is commercial).
- ✅ objc2 0.6.4 / objc2-* 0.3.2 / block2 0.6.2 / dispatch2 0.3.1, pinned exact, `CFRetained`
  ownership in the type system; one counted `from_raw`/`retain` admission per wrapper crate.

## Canvas

- ✅ **Infinite canvas is the product.** slop-desk built and retired one; its reasons were
  AppKit-specific (a libghostty surface cannot live under a scaled ancestor) and product-fit
  (undiscoverable, not keyboard-navigable). We own the renderer, so zoom is real, and we add
  keyboard navigation, zoom-to-fit/selection, grid snap, arrange-by-repo and a minimap (kolu).
- ✅ **Canvas navigation: ⌘1 fit all, ⌘2 fit the active item, ⌘0 back to 100 %, ⌘⇧R arrange**
  (2026-09-06). ⌘1 and ⌘0 already existed; ⌘2 is the natural sibling of ⌘1 and ⌘⇧R is free
  (⌘⇧A is next-attention, ⌘⇧M mute, ⌘⇧I stats, ⌘⇧T/N/L the agent, the note and the
  conversation). ⌘0 no longer resets about the middle of the *viewport* but keeps the **active
  item** centred, which is what "back to 100 %" means when something is being worked on; with
  nothing active it behaves as before. ⌘2 with nothing active does nothing — ⌘1 is the action
  for "show me everything". There is no multi-selection on the canvas, so "zoom to selection"
  is zoom to the active item; `CanvasView::active_rect` is the one place a selection would join.
- ✅ **Camera moves are a pure flight the render loop advances** (2026-09-06):
  `slopty_client::canvas::Flight` interpolates between two cameras over `FLIGHT` = 180 ms with
  a cubic ease-out, and the canvas element's prepaint advances it by the time since the last
  frame. No timer anywhere — the frame *is* the clock — and a landed flight stops asking for
  frames, so a still canvas costs nothing. Zoom interpolates **geometrically** and the
  viewport's centre travels in a straight line between the two centres, so a simultaneous
  zoom-out and pan reads as one movement instead of a swing. The self-test turns animation off
  (`CanvasView::set_animation`, `#[cfg(feature = "e2e")]`): there a frame is a step, not a
  moment, and a dump taken mid-flight would report where the camera was passing through.
  A flight is the camera's default move, never a lock: pan, wheel, pinch, ⌘=/⌘- and a minimap
  scrub all abandon it (`CanvasView::take_camera`), so the 180 ms of an animation are never
  180 ms of ignored input.
  Tested with a clock of its own in `slopty-client` (ease, exact landing, straight centre) and
  through the actions headlessly.
- ✅ **Arrange by repository is a pure layout function, keyed on the working directory**
  (2026-09-06). `slopty_client::arrange::arrange_by_repo` takes items (id, size, cwd, activity)
  and returns origins plus one `Heading` per block: no document, no camera, no clock, so
  determinism, no-overlap, size-preservation and block order are ordinary unit tests. Blocks
  run left to right by most recent activity, which is the item's **z order** — raising an item
  on focus is already the recency the canvas keeps, so nothing new is stored. Inside a block
  items fill a square-ish grid, most recent first, ties broken on the item id. Gutters are
  `GAP` inside a block and `3 × GAP` between blocks; the heading is a 28-pt band above each one,
  drawn in canvas coordinates with role `Heading` so it pans, zooms and reads aloud.
  The key is the **repository root the host resolved** (see the protocol 14 ruling below), kept
  current as the shell moves: `TermEvent::Cwd` was being dropped by the terminal view, so it now
  reaches the canvas (`TerminalViewEvent::Cwd` → `CanvasView::session_moved`) and a shell that
  `cd`s into another checkout arranges under the new one instead of the directory it opened in.
  Each `Heading` carries the ids of its block, so a block whose items have all closed loses its
  heading on the next reconcile: otherwise ⌘1 would keep fitting an empty rectangle and a screen
  reader would keep reading a label for nothing. `CanvasItem.group` stays unused: it is the
  server's field for server-side groups, and this layout is a client view.
  Arranging is **not undoable**: the canvas has no undo stack, for this or for a drag.
  Goldens: `arrange-by-repo` is new and `note` was re-accepted for ⌘0's new centre (2.19 % of
  pixels, a pan). The four **iOS** goldens were re-accepted too — they predate the ghostty
  metrics ruling above, and `ios-phone-terminal` had drifted to 0.9–1.1 % against a 1 %
  tolerance, i.e. it was failing on some runs and passing on others before this branch touched
  anything. Refreshed they are all 0.000 %.
- ✅ **The repository root comes from the host, protocol 14** (2026-09-06). Only the machine a
  shell runs on can see its `.git`, so `slopty_host::repo::root_of` resolves it there: walk up
  from the working directory to the nearest ancestor holding a `.git` **entry**, and stop.
  No `git` subprocess and no libgit — a handful of `stat` calls per `cd`, on the session actor's
  own thread, cached in the actor and recomputed only when OSC 7 says the directory changed.
  The entry is a directory in an ordinary checkout and a *file* in a worktree or a submodule;
  the `gitdir:` link inside that file is deliberately **not** followed, so a worktree is its own
  repository and not the checkout it was made from — two worktrees of one project are two places
  to work. A repository nested inside another wins, because the walk stops at the first entry.
  The path is canonicalised first, so two shells that reached one checkout through different
  symlinks land in the same block; a directory that has since been removed answers `None` rather
  than guessing from the stale string. Wire: `SessionSummary.repo` and `TermEvent::Cwd` becoming
  `{ path, repo }`, both `Option<String>`; goldens `host_session_opened`, `host_term_cwd` and
  `host_term_cwd_no_repo` (new) and `client_hello` re-accepted, `PROTOCOL_VERSION` 13 → 14.
  `slopty_client::arrange` keys on the root when it is there and keeps the old containment
  heuristic only for the items the host resolved no root for — a directory outside any
  repository, or one that has since been removed — so a mixed canvas (a shell outside any
  repository next to shells inside one) still groups sensibly. It is **not** a compatibility
  path: the handshake compares `PROTOCOL_VERSION` for equality and refuses a mismatch, so a
  client that reads roots never sees session data from a host that does not send them.
  This replaces the deferral in the ruling above: sibling subdirectories with nothing checked
  out at the root are now one block, which the heuristic could never see. Tests:
  `slopty_host::repo` over a temp tree (checkout, nested subdirectory, worktree `.git` file,
  repository inside a repository, no repository, directory that is gone),
  `sibling_directories_of_one_repository_are_one_block` and
  `a_rootless_shell_does_not_join_a_rooted_block` in `slopty_client::arrange`, and
  `the_hosts_repository_root_decides_the_blocks` headlessly.
- ✅ Kind-aware culling: terminals keep state and stop painting off-screen; video pauses decode.
- ✅ **Host-authoritative document, optimistic client.** `slopty-host::CanvasStore` owns the
  document (JSON at `<data>/canvas.json`, atomic rename, serialised writers), validates every
  `CanvasOp` (finite, clamped geometry; unknown ids rejected), bumps a version and broadcasts a
  `CanvasSync::Delta { by }`. Clients apply their own ops immediately and recognise the echo by
  `by == me`. A new session gets a terminal item automatically (right of the rightmost, snapped
  to 16 units) so every client sees it in the same place; a closed session removes its item.
- ✅ Snapshot is pushed right after `HelloAck` on the control stream; no client request needed.

## Settings

- ✅ **One TOML file, every key optional** (2026-09-05): `<data dir>/settings.toml`, the
  directory the client identity already uses (`SLOPTY_DATA_DIR`, else
  `~/Library/Application Support/Slopty`; `slopty_settings::data_dir` is now the one copy the
  app and the CLI share). `slopty-settings` is serde + `toml` 1.1.5 only, no GPUI, so the CLI
  and tests use it without a window. `#[serde(default)]` on every struct makes the file and
  any subset of it valid; unknown keys are found by diffing the parsed table against the
  serialised defaults (no hand-kept key list) and reported as warnings; a parse or type error
  yields the defaults plus a one-line error (`Loaded { settings, warnings, error }`), never a
  crash. `slopty settings init` writes a commented file generated from `Settings::default()`
  and covered by a round-trip test. `terminal.scrollback_lines` is host-side and not here.
- ✅ **Hot reload by a 1 s stamp poll, not `notify`** (2026-09-05): a GPUI foreground task
  sleeps on the background executor's timer and compares `(mtime, len, exists)`. Editors save
  atomically (temp file + rename), which orphans a per-file FSEvents/kqueue watch, so `notify`
  would need a directory watch plus filtering and a debounce for half-written files anyway;
  one `stat` a second is free, handles create / replace / delete alike and adds no dependency
  to the iOS triple. Verified: `mono_size` 13 → 20 while the app ran re-fit the shell from
  21×87 to 14×58 (`stty size`) within ~2 s and back again; a `[font` typo showed
  `settings: …: TOML parse error at line 1, column 6` in the bar for 6 s with the defaults in
  force; `ligatures = true` showed the unknown-key notice for `font.ligatures` and loaded the rest.
- ✅ **Theme swap is a push, not a global** (2026-09-05): `Workspace::rebuild_theme` derives
  the `Theme` (`slopty_app::settings::theme_for`: variant, the family ahead of the bundled
  fallbacks, sizes clamped to sane ranges) and calls `CanvasView::set_theme`, which fans out to
  every `TerminalView`, `ScreenView` and the picker; the terminal element measures from the
  view's theme on each frame, so a size change re-fits the grid through the existing `fitted`
  → `TermRequest::Resize` path with no new wire message. Found on the way: the shaped-row
  cache was keyed by text, style, focus and size only, so a swap replayed the old palette's
  default text colour; the key now includes the family and the whole `TerminalPalette`.
- ✅ **Light variant + `system` appearance** (2026-09-05): `Theme::new(Variant::Light)` with a
  GitHub-light terminal palette and near-white surfaces; `appearance = "system"` (the default)
  follows `window.appearance()` through `observe_window_appearance`, so a macOS appearance
  change re-themes without a restart. gpui-kit's own theme mode (the pairing input) is switched
  alongside. Verified on a Mac in light appearance: the app opened light, `appearance = "dark"`
  turned it dark on save.
- ✅ **"Settings…" (⌘,) opens the file in the default editor** via `App::open_with_system`
  (`open <path>` on a background thread in `gpui_macos`), after `Settings::init` so a first
  press has a file to edit. Verified: the menu item opened `settings.toml` in VS Code. iOS has
  no entry: the defaults apply there and the file, if present in the sandbox, is still read.

## Hosts

- ✅ **The pairing store was already a list** (2026-09-05): `slopty_net::identity::Identity`
  keeps `hosts: BTreeMap<EndpointId, KnownHost>` in `client.json`; only the app's
  `connect_host` picked the first entry. So there is no second file and no migration:
  `net::known_hosts` reads the map (sorted by name), `net::forget_host` removes one, and a
  pairing on the panel is `Identity::remember` as before. The stored name is refreshed from
  each `HelloAck` so the switcher does not show a stale one before the link is up.
- ✅ **One link, one canvas, one loop per host; one canvas on show** (2026-09-05):
  `Workspace::spawn_host_loop` is the old connect loop with a host id: its own endpoint,
  backoff, silence check (`SILENCE_DROP` abandons the link after 15 s without a datagram) and
  event pump, writing into a `HostSlot` (status, canvas, zoom, needs-you, RTT). The loop ends
  when its slot is gone (forget). Each host owns its `canvas.json`, so one `CanvasView` per
  host and the workspace shows `active`'s; switching is a field change plus a focus, nothing
  is torn down. `Rejection::NotPaired` maps to `HostStatus::NeedsPairing` (red dot) and keeps
  retrying at the capped backoff so a fresh pairing of the same host resumes by itself.
  Verified with two daemons on this Mac (`SLOPTY_PORT` 45570/45571, `SLOPTY_HOST_NAME`
  "Studio One" / "Studio Two", the new env override in `slopty-hostd::paths`): "Add host…"
  paired the second while the first stayed connected; each host had its own shell (`echo
  two` on one, a new shell on the other); ⌘⌥←/→ and the switcher rows swapped canvases;
  killing one daemon turned its row amber with "disconnected … reconnecting" within ~16 s
  while the other stayed green, and restarting it turned the row green again; "forget"
  removed the row and the `client.json` entry (`slopty hosts` listed one host).
- ✅ **Switcher is a hand-rolled overlay, not gpui-kit's popover** (2026-09-05): a
  full-window backdrop that closes on click with an occluding panel anchored under the host
  name, the same pattern as the window picker. It needs no focus handling and lays out the
  same on a 520-pt window (the host button keeps a 96-pt minimum there and the status label
  yields; verified) as on the desktop. ⌘1…⌘9 are the fit shortcuts, so hosts step with
  ⌘⌥→ / ⌘⌥← (and ⌘⇧H adds one); the Host menu names the same actions.
- ✅ **Cross-host attention** (2026-09-05): the pill and `set_badge` use the sum of every
  slot's `needs_you` (each canvas still reports its own count through `CanvasEvent::NeedsYou`);
  a tap prefers the host on show, else the first with a waiting agent, switching first. A
  banner's tag stays the session UUID (unique across hosts); `on_system_notification_response`
  asks each canvas `has_session` to find the host, activates it, then calls
  `notification_response` as before. Verified 2026-09-06 against a **real second host** on
  another Mac (`crates/slopty-e2e/tests/hosts.rs`, `cargo xtask e2e hosts`, macbook-pro over
  the WireGuard mesh — direct path, ~8 ms RTT): the app pairs with both hosts at once; with host
  A on show, a permission hook played to a session on host B *through the real `slopty hook`
  relay over ssh* (never a Claude Code session, never a keystroke into a shell) badges the pill
  with the cross-host sum within a few ms; a tap on the pill switches to B and focuses the
  waiting session; a `notification_response` carrying only that session's UUID routes back to B
  and reveals it. Killing host B mid-stream turns its row amber while host A keeps streaming;
  restarting B goes green and the shell reattaches (the session survives in ptyd — the vt grid
  does not outlive a hostd restart, so reattach is confirmed by live I/O, not grid replay).
- ✅ **Two-host harness hardening** (2026-09-06, from the Codex review of `claude/twohosts`,
  fixed after landing). Four holes in `crates/slopty-e2e/src/harness.rs` and `tests/hosts.rs`,
  none in product code: the `Drop` guard passed unset daemon PIDs (0) to `kill -9`, which signals
  the cleanup shell's own process group and ends the script before the `pkill`/`rm -rf` that
  follow — `teardown_script` now leaves unstarted daemons out (unit-tested); the remote root was
  created and binaries copied before the `RemoteHost` guard existed, so a setup failure leaked
  them on the second Mac — the guard is built first and every remote write happens through it;
  the ssh helpers' timeouts dropped the child future without `kill_on_drop`, leaving an
  ownerless ssh process — every ssh and gzip child is `kill_on_drop(true)`; and the suite passed
  silently when `SLOPTY_HOST2_E2E` was set but `SLOPTY_HOST2` empty — `host2_gate` errors,
  naming both variables, and the test panics on it (unit-tested).
- ✅ **`slopty-hostd --direct-only` reads the same env spellings as the client** (2026-09-06).
  The flag was a plain clap bool with `env = SLOPTY_DIRECT_ONLY`, which rejects `1` (clap only
  accepts the flag's presence, and an env value must parse as the value type), so
  `SLOPTY_DIRECT_ONLY=1 slopty-hostd` failed to start while the same variable is how the client
  and every bench select direct-only. It now takes clap's boolish parser (`1`/`true`/`yes`/`on`)
  from the env or `--direct-only[=value]`, defaulting to false, matching `Reach::from_env`.
- ✅ **A terminal survives a hostd restart: ptyd keeps a checkpoint plus the output after it**
  (2026-09-06). Before, ptyd's ring only held output produced while no host was attached, so a
  hostd restart (an upgrade, a crash, `launchctl kickstart`) came back with live shells and a
  blank grid until the program redrew. Options weighed: (a) hostd relaying every byte through
  ptyd instead of reading the master itself (a relay hop on the hot path, ruled out when custody
  was designed); (b) ptyd holding the whole output history and hostd replaying it (unbounded,
  and a replay of a day of output takes seconds); (c) hostd handing ptyd a copy of each read
  (`PtydRequest::Output`) and, when the session has been quiet for 500 ms or 1 MiB has been
  tapped, the engine's whole state (`Checkpoint`), which empties the ring; `Attach` returns
  both and the engine replays checkpoint then ring. (c) is the ruling: the replay is bounded by
  one checkpoint plus at most 1 MiB, the hot path gains one `try_send` of a `Vec` copy per read
  on a channel a task drains onto the host's ptyd connection, and a full channel only makes the
  next checkpoint come early (taken inline, not through the timer, which the read-biased select
  could starve under a flood). The taps ride the *same* connection as the requests, and ptyd
  accepts them only from the connection holding the master: a first cut used a second
  connection, and the Codex review of it found the two races that buys — a dying host's
  buffered taps and checkpoint landing after its master connection's EOF (dropped, or worse,
  installed over the replacement's state) — plus a connection that never read ptyd's `Exited`
  broadcasts. One connection orders taps and EOF, the no-reply sends drain pending events
  first, and a failed tap send is logged without stopping the loop. The first checkpoint is
  taken right after the replay (the ring came to us with the master, so until then a second
  restart would have only the previous checkpoint), and a quiet-spell checkpoint is deferred
  while the output stands inside an escape sequence or a UTF-8 character
  (`slopty_engine::boundary::Boundary`), because it replaces the bytes before it and the rest
  of that sequence would print as text; the byte threshold forces one regardless. The
  checkpoint is libghostty-vt's own VT formatter (`Format::Vt`,
  palette, modes, scrolling region, pwd, keyboard, style, hyperlink, protection, kitty keyboard,
  charsets) with corrections, each with a test in `ghostty::checkpoint_tests`: the formatter
  only sees the active screen, so on the alternate screen the primary is snapshotted as the
  `?1049h`/`?1047h`/`?47h` goes by (`GhosttyEngine::write` splits the chunk there, and keeps
  the tail of a chunk that ends inside `ESC [ ? 10` so a switch split across two reads is
  still seen before it completes) and emitted first, then every mode the primary blob set is
  put back to its default (each blob only writes deviations, so a mode turned off on the
  alternate screen would otherwise stay on), then `?1049h` + home, then the alternate screen;
  it drops trailing blank rows, so the missing rows are fed as `CR LF` before the first margin
  sequence (`DECSTBM` or `DECSLRM`, found independently) so a scrolled primary keeps its
  history and cursor row; and its cursor comes before `DECSTBM`, which homes the cursor, so the
  cursor is emitted last, relative to the margins when origin mode is on. Tab stops are not
  emitted (their emission leaves the cursor at the last stop and shifts the content after it;
  programs do not set them). Known approximations: a pending wrap at the last column is lost
  (shared with the formatter), and soft wraps come back as hard rows (`with_unwrap(false)`
  keeps the row count exact for the padding; widening the terminal after a restart will not
  reflow lines drawn before it — ruled acceptable over a replay whose row count depends on the
  formatter's blank-row logic). The title is prefixed as OSC 0 because the formatter does not
  carry it.
  `PTYD_PROTOCOL` is 2; both daemons ship together so no compatibility path exists.
  Proved by `apps/slopty-ptyd/tests/roundtrip.rs` (tap, checkpoint, a stranger's taps ignored,
  reattach on a new connection), `crates/slopty-host/tests/session_actor.rs` (output tapped,
  quiet checkpoint, replay into a second actor) and `apps/slopty-hostd/tests/e2e.rs`
  `a_host_restart_keeps_the_screen` (marker printed, hostd killed after the checkpoint delay,
  a fresh hostd on the same ptyd shows the marker in its full frame and the shell still
  answers). Cost: MEASUREMENTS 2026-09-06 "checkpoint cost".
- ✅ **libghostty-vt is compiled `ReleaseFast` under every cargo profile** (2026-09-06, found by
  the checkpoint cost measurement). `libghostty-vt-sys`'s build script picks the zig optimize
  mode from cargo's `DEBUG` variable, which is `true` whenever the profile keeps any debug info,
  and every profile here keeps `debug = "line-tables-only"` for symbolised panics, release and
  dist included. So every Slopty binary ever built parsed VT with a `-Doptimize=Debug` library:
  50 KB/s, 1.2 ms per 70-column line, a 10k-line `cat` taking 12 s to draw. `.cargo/config.toml`
  now sets `LIBGHOSTTY_VT_SYS_OPTIMIZE=ReleaseFast` in `[env]`, which the build script honours
  over `DEBUG` (`cargo:rerun-if-env-changed` covers it, so the change rebuilds the library); the
  Rust side keeps its line tables. Ruled against dropping the line tables (they are what makes a
  crash report readable) and against `ReleaseSafe` (ghostty's own release builds are
  `ReleaseFast`; its safety checks are debug tooling, not a contract). Before/after in
  MEASUREMENTS 2026-09-06 "libghostty-vt built ReleaseFast": 12.5 s → 3.6 ms for the same 10k
  lines. Consequence: every latency number recorded before this date that passed through the
  parser is an upper bound; the ones that matter (keystroke echo, attach replay) get re-measured
  as they come up rather than all at once.
- ✅ **Sleep policy (2026-09-12): the host stays awake while a client is attached, its display
  stays on while a window streams, and the client keeps the device awake while it shows a
  stream.** Parsec's behaviour, for the same reasons: a host that idles to sleep drops every
  session, a sleeping display captures black, and a phone that dims mid-stream is a phone you
  keep poking. Host side: `slopty_host::wake::Wake` is a pure counter (clients, live streams)
  behind the `Holds` trait; hostd's `Assertions` maps it to `NSProcessInfo` activities
  (`Activity::system_awake` = `UserInitiated`, `Activity::display_awake` adds
  `IdleDisplaySleepDisabled`; `Activity` is now `Send`, NSProcessInfo being thread-safe). The
  first client joining holds, the last leaving releases; the display follows the stream
  `Registry`'s live count through its new observer (called under the registry lock, so counts
  arrive in mutation order — Codex found the race where two closes and an open could report
  `0` after `1`), so every open/close path (client close, connection drop, `close_screens`) is
  covered by construction. Nothing is held with nobody
  attached: an unattended host sleeps as its owner set it. Client side: `ScreenView` holds
  GPUI's `prevent_idle_sleep` guard (macOS `NSActivity`, iOS `idleTimerDisabled` in our fork)
  for its lifetime; a sleeping or removed window drops it. On the Mac that guard is
  `UserInitiated` (system sleep only), so `ScreenView` also holds `Activity::display_awake`:
  a viewer watching a remote window is not touching the keyboard (Codex, same review). Headless test
  `a_streaming_window_keeps_the_device_awake` reads the test platform's hold count; the host
  policy has unit tests on edges and underflow. The canvas also holds the device awake while
  any agent is `Working` or in a `Tool` (the human is waiting on it, as zed does for its agent
  panel) and lets go when every agent is idle, blocked on the human, or gone
  (`a_working_agent_keeps_the_device_awake`). Not done, ⏸ until asked: a host setting to opt
  out (`pmset` still wins over an activity for a forced sleep).

## Multi-client

Proven end to end by the pair suite (`crates/slopty-e2e/tests/pair.rs`, `cargo xtask e2e pair`):
two app processes on one host, each driven through its own test socket. Before this, one line in
ARCHITECTURE said "multi-client is cheap" and nothing exercised two live clients at once.

- ✅ **Item geometry, z, sleep and note text are host state; camera and zoom are client state**
  (2026-09-06, confirming what the code already did). `slopty_host::canvas::CanvasStore` is the one
  authoritative document (persisted `canvas.json`): every client proposes `CanvasOp`s and mirrors
  the host's `CanvasSync` deltas (`slopty_client::canvas`), so a terminal opened on A appears on B
  in the same place, an item A drags lands on B where A left it, and a note A edits reads the same
  on B once its editor commits (400 ms idle, `slopty_ui::note`). The camera `{x, y, zoom}` lives in
  each `CanvasView` and is never sent, so A's ⌘= and ⌘1 do not move B. This is the kolu-style shared
  canvas: one plane, one layout, many viewpoints. Pair-suite evidence: a shell opened on A is on B
  within a round trip (loopback, measured below) with the same title, size and rows; A zooming in
  leaves B at zoom 1; a title-bar drag on A moves the item to the same rect on B; ⌘W on A takes the
  item off B.
- ✅ **Input is serialised by the session actor; no client "holds" the keyboard** (2026-09-06). Both
  clients type into the same session at once and every key of each side lands in its own order,
  none lost or reordered: the actor (`slopty_host::session`) writes each `TermRequest::Key` to the
  PTY in arrival order on its own thread, with no per-client gating. A numbered sequence typed a
  key at a time from each side came out interleaved and complete (`a0b1c2…i8j9`). What one client
  owns is not the input but the **PTY size**: the driver (the opener, else the first to attach)
  sizes the grid, the others see its size and wear the "take" pill. Rejected again here, as under
  Terminal: tmux-style "latest input drives" (a phone would keep resizing the desktop's terminal).
  The take pill renders on the active item only, so a client takes over by focusing the terminal
  and clicking "take"; the PTY then follows the taker and the former driver gets the pill.
- ✅ **A permission answer is client-local; the host's next word clears the others** (2026-09-06). An
  agent's attention (a hook played to hostd) badges every client and each shows "1 needs you", read
  off the same `HostMsg::Agent` broadcast. "Allow" on A drops A's own count at once (the answer is
  A typing Enter into the prompt, `CanvasView::answered`), but B keeps counting it until the host
  reports the agent moved on (a `PreToolUse`), because only the host knows the prompt was answered
  and by whom. This is deliberate: the alternative (broadcasting one client's answer as authority)
  would clear B on an answer that the agent might still be blocked on.
- ✅ **A dying connection detaches only its own sinks; the survivor never stalls, and a relaunch
  catches up** (2026-09-06, exercising the "detach only its own sinks" ruling under two clients).
  A killed outright (SIGKILL, no QUIC goodbye) while B watches a flooding shell: B's output paused
  no longer than the dump-poll floor (about one frame; see MEASUREMENTS) and B kept typing and
  echoing. A relaunched on the same identity reconnects with no ticket, reattaches every session
  and shows the current rows; both clients then hold the same canvas. When A's abandoned QUIC
  connection finally idles out at the host (`IDLE_TIMEOUT`, 45 s), hostd's `Peer::drop` calls
  `SessionHandle::detach_sink(client, &sink)`, which removes a viewer only if `Sender::same_channel`
  matches — so the relaunched A's live viewer survives its dead connection's cleanup, and the host
  still reports both clients connected.

- ✅ **A display streams to two clients; the host encodes once per viewer, not once per stream**
  (2026-09-06). `slopty-hostd`'s `Peer` opens a `ScreenStream` per connection
  (`conn.rs::screen`), so two clients watching the same display run two captures and two HEVC
  encoders. Measured (loopback, native 1920×1080, debug): both viewers present the frames with
  no drops (arrival → present p50 4.3 ms, p95 21-57 ms; MEASUREMENTS below), and hostd draws
  0.15 cores with two viewers against 0.09 with one — the second viewer costs about 0.06 of a
  core. Kept per-viewer rather than fanning one encode out to both: each client already adapts
  its own bitrate to its own path (`ScreenRequest::Report` → per-stream rate control, the mesh
  measurements) and can ask for its own quality (`SetQuality`), which a shared encode would take
  away, and two clients of one display is the common case (a laptop and a phone), not twenty.
  `slopty host screens` lists both live streams with their client ids, which is how the test
  confirms the fan-out. Revisit if many viewers of one high-resolution display becomes real: a
  shared base layer with per-client rate would cap the cost, at the price of the per-client
  adaptation.

## Claude Code

- ✅ Hooks are the authoritative signal (33 events as of 2026-09; we register the status-bearing
  subset), transcript JSONL is read-only context, screen heuristics corroborate only.
- ✅ Relay path (verified 2026-09-04 with Claude Code 2.1.260 running `-p` inside a Slopty
  shell): `slopty hook install` registers `{"command": "<abs slopty>", "args": ["hook"],
  "async": true, "timeout": 5}` for 12 events (`SessionStart/End`, `UserPromptSubmit`,
  `Pre/PostToolUse`, `PostToolUseFailure`, `PermissionRequest/Denied`, `Notification`,
  `Elicitation/Result`, `Stop`) in `~/.claude/settings.json` (or `--settings`). Exec form
  (no shell) and `async` so the agent never waits on us. The relay reads `SLOPTY_SESSION` and
  `SLOPTY_HOSTD_SOCKET`, both injected by the host into every session's environment, posts
  `CtlRequest::Hook` and exits 0 whatever happens. `slopty-agent::Tracker` maps hooks to
  `AgentStatus`; `AgentEvent.attention` is true only on entering a blocked state or `Done`,
  so the permission `Notification` that follows a `PermissionRequest` does not alert twice.
  Joining clients get the current table after the canvas snapshot (no proto change: `HostMsg::Agent`
  already existed). Observed cycle: Idle → Working → Tool{Bash} → Working → Done → None.
- ✅ Attention signal: `AgentEvent.attention` → `CanvasEvent::Attention` → `slopty_platform::attention()`,
  which is `AudioServicesPlayAlertSound(kSystemSoundID_UserPreferredAlert)` on macOS (the alert the
  user picked in System Settings, respects their volume) and `AudioServicesPlaySystemSound(kSystemSoundID_Vibrate)`
  on iOS (`objc2-audio-toolbox`, `AudioServices` feature; AudioToolbox.framework linked in the
  iOS spec). No `UNUserNotificationCenter`: it needs a signed bundle with the notification
  entitlement, which the bare macOS binary is not; revisit when the Mac app ships as a bundle
  (superseded 2026-09-05: bundled app ships via `cargo xtask bundle` with GPUI `SystemNotification`
  banners; see "Notification-centre banners for agents" below).
- ✅ **Dock badge + bounce on macOS** (2026-09-05). `CanvasEvent::NeedsYou(n)` also calls
  `slopty_platform::set_badge(n)` (`NSApplication.dockTile.badgeLabel`, cleared at 0 and on
  every reconnect), and `Attention` adds `slopty_platform::bounce()`
  (`requestUserAttention(NSCriticalRequest)`, skipped when the app `isActive`, so a user looking
  at the canvas gets only the sound). Both need no bundle or entitlement, unlike
  `UNUserNotificationCenter`. iOS: no-ops — the icon badge needs notification authorisation and
  the connection dies in the background anyway, so the in-app "N need you" pill is the signal.
  Verified 2026-09-05 with a synthetic `PermissionRequest` through `slopty hook`: the tile
  shows "1" (bare binary and bundle alike), `PostToolUse` clears it. Gotcha: `cargo build -p
  slopty-app` builds only the library crate; the binary is `cargo build -p slopty --bin
  slopty-app`. GPUI's `show_system_notification` exists in the fork but is disabled outside a
  bundle ("system notifications disabled: not running from an app bundle") — next step for
  banners now that `cargo xtask bundle` exists.
- ✅ **Notification-centre banners for agents** (2026-09-05). `CanvasView::agent_event` posts a
  GPUI `SystemNotification` (tag = session id, title "Claude wants to use Bash" / "has a
  question" / "finished", body = the badge detail, Allow/Deny action buttons for a permission)
  when `attention` is set and `cx.active_window()` is `None` (the app is not frontmost —
  `NSApp.mainWindow` is nil then). Answering from the badge or the agent moving on dismisses it
  by tag. `cx.on_system_notification_response` (registered once in `open_workspace`) parses the
  tag, activates the app and calls `CanvasView::notification_response`: body → reveal the
  session, "allow"/"deny" → the badge's Enter/Esc. GPUI's macOS backend disables all of it
  outside a bundle, so this only works from `cargo xtask bundle` output (the first post raises
  the macOS "Slopty Notifications" prompt). Verified 2026-09-05 with the debug bundle: the
  notification landed in Notification Center ("allow? $ cargo test"), clicking it brought Slopty
  forward with that terminal active. The action buttons are wired through GPUI's category
  registration but were not exercised (macOS only shows them on hover of a live banner).
  iOS: no-op — the connection dies in the background, nothing would post.
- ✅ Answering a permission prompt from the badge (verified 2026-09-05 against Claude Code
  2.1.261 driven under a pty in `default` permission mode): the prompt is a numbered menu with
  the first entry highlighted — `❯ 1. Yes · 2. Yes, and always allow access to <dir> from this
  project · 3. Yes, and switch to auto mode · 4. No — Esc to cancel · Tab to amend`. Enter
  (`\r`) ran the command ("Ran 1 shell command", the file appeared); Esc (`\x1b`) ended the turn
  with "Interrupted · What should Claude do instead?" and nothing ran. So "allow" = Enter and
  "deny" = Esc, sent through `TerminalView::press` like the phone's key bar, with a sticky
  Control disarmed first. Neither key is sent twice: the canvas remembers the answer per session
  until the next `HostMsg::Agent` for it. Two things learned on the way: text and `\r` written
  in one burst are taken as a paste (the newline does not submit), and Esc while the model is
  still thinking interrupts the turn instead of answering — the badge only offers the buttons
  while the status is `Blocked(Permission)`, which the `PermissionRequest` hook raises after
  the menu is up. Question / elicitation badges cannot be answered with one key (the reply is
  text or a pick), so their button reveals and focuses the terminal.
- ✅ Finding the agent that needs you: `CanvasView::needs_you` is the list of terminal items
  whose agent is `Blocked` (not `IdlePrompt`) and not yet answered from the badge, sorted by
  `(rect.y, rect.x)`. ⌘⇧A (`NextAttention`, Canvas key context, so it works with a terminal
  focused since ⌘ chords fall through) picks the entry after the active item, wrapping, else
  the first, and runs the same reveal + focus as the "answer" button. The count travels as
  `CanvasEvent::NeedsYou(n)` to the workspace, which draws the "N need you" pill in the top
  bar on both platforms (a tap on the phone, where there is no ⌘⇧A). Reading order rather than
  z or arrival time because it is the one order the user can predict from what they see; the
  picker (⌘O) uses the same order within its "needs you / other agents / shells" ranking. No
  proto change: the count is derived from the `HostMsg::Agent` table the client already has.
- ✅ What the badge says when the agent waits or stops (verified 2026-09-05, Claude Code 2.1.261
  inside a Slopty session through `slopty open`, payloads captured with a `tee` hook beside
  the relay): every event carries `transcript_path`; `Stop` also carries
  `last_assistant_message` (here "Done. `/tmp/…/opened-enter` was created."), which the docs
  say to prefer because the transcript file "may lag the in-memory conversation". So the
  detail comes from the payload first — the `AskUserQuestion` input's first `question`, the
  `Elicitation` `message`, the `Stop` message's last non-empty line — and only a `Blocked
  (Question|Elicitation)` or `Done` event that still has no detail makes the daemon read the
  transcript's last 256 KiB for the newest non-sidechain `assistant` record with a `text`
  block (`slopty_agent::transcript`, `spawn_blocking`, table lock released meanwhile). The
  transcript is JSONL of `{"type":"assistant","message":{"content":[{"type":"text",…}]}}`
  records; `isSidechain: true` rows are subagent chatter and skipped. No wire change: `detail`
  already existed. Found on the way: the `permission_prompt` Notification that follows a
  `PermissionRequest` ~6 s later says only "Claude needs your permission" (no "to use X"), and
  used to replace `Permission{Bash}` + "$ touch …" with `Permission{""}` + nothing; the tracker
  now keeps the request's tool and detail when the notification names none. After Esc on the
  menu no `Stop` fires (the turn is interrupted; the next hook is whatever the user does), so
  the badge keeps saying "denied" until then — acceptable, since the outline is already off.
- ✅ **"+ agent" is a menu** (2026-09-12): "Terminal agent" / "Conversation" / "Resume
  conversation…" (`agent-terminal`, `agent-conversation`, `agent-resume`, each the action its
  shortcut runs), because the phone has no ⌘⌥T or ⌘⌥R and the bar has no room for two more
  pills next to the host name. The driven-agent scenarios open their first card through it
  (a click on the Mac, a `ui_tap` on the simulator) so the path a finger takes is the one
  tested. The pill's label and place are unchanged, so no golden moved.
- ✅ "Terminal agent" (⌘⇧T, "New Agent" menu item) sends `OpenSession { command: ["claude"], title:
  "claude" }`; nothing else is special about the session, so the hook relay, badges and
  attention all apply as they do to a `claude` typed into a shell. The daemon's `PATH` is
  launchd's under a LaunchAgent, and `claude` is often a shell alias (`~/.claude/local`), so
  `slopty-pty::resolve_command` runs a bare name it cannot find on `PATH` through
  `$SHELL -lic '<quoted words>'` (interactive login shell: rc files, aliases, job control);
  a path or a name found on `PATH` still execs directly. Covered by
  `unknown_bare_program_goes_through_the_login_shell`.
- ✅ **Conversation view from the transcript, not from hooks** (2026-09-05). The hook stream
  says what state the agent is in; it never carries what was said. The JSONL transcript does
  (every record the CLI writes, tool calls included), and its path arrives with the first hook
  payload, so the daemon tails that file for the sessions a client asked about and nothing
  else. Wire: `ClientMsg::Transcript(TranscriptFollow)` to start/stop following,
  `HostMsg::Transcript(TranscriptUpdate { reset, entries })` with a 200-entry snapshot on
  reset and appended slices afterwards; entries are already reduced to
  `User{text}` / `Assistant{markdown}` / `ToolUse{name, summary}` on the host so the phone
  never parses JSONL and the wire stays small. `PROTOCOL_VERSION` 9 → 10, goldens
  `client_transcript_follow`, `host_transcript`. Polling at 400 ms rather than FSEvents: the
  file grows in bursts of whole lines, a 400 ms lag is invisible next to the model's own
  latency, and one `Tail` per followed session (byte offset + the partial last line) costs a
  `metadata` call per tick. The view swaps the grid rather than splitting the card because
  the card is already the unit the canvas lays out and zooms; the grid is one ⌘⇧L away.
  Verified by `slopty_agent::transcript` unit tests (13: tail across appends, truncation,
  partial lines, every tool summary) and the headless
  `the_conversation_replaces_the_grid_and_follows_the_transcript`.
  **Correction (2026-09-06):** an earlier version of this ruling said `tool_result` bodies,
  thinking blocks and typing into the conversation were not done. All three are:
  `crates/slopty-ui/src/terminal/conversation.rs` renders `TranscriptBody::Thinking` and
  `TranscriptBody::ToolResult` as folds — a header with the line count, opening on a click, a
  tool result carrying its error state — and the composer under the list types its text into
  the session followed by Enter (↩ sends, ⇧↩ is a newline). Covered by `folds_open_on_a_click`,
  `the_composer_types_into_the_session`, `the_composer_wears_a_focus_ring_only_while_focused`
  and `the_attention_row_and_the_composer_are_read_and_tabbed` headlessly, and by
  `the_conversation_view_reads_and_answers_the_agent` in the app self-test.
- ✅ **Links: OSC 8 first, text scan second** (2026-09-05). The engine reads the URI of every
  linked cell with `ghostty_grid_ref_hyperlink_uri`, gated on the row's `has_hyperlink` page
  flag (a false positive costs one extra check per cell, a clean row costs nothing) and on the
  cell's own flag, both on the render path (`Point::Viewport`, the host never scrolls the
  viewport) and on the scrollback fetch path (`Point::Screen`). Runs, not ids: `Line::links`
  is `Vec<Hyperlink { col, len, uri }>` and the per-cell `Option<HyperlinkId>` that
  `slopty-grid` had carried unused is gone. Measured (MEASUREMENTS.md, same date): a
  link-free 80×24 full frame went from 15 477 to 13 581 bytes (−12 %), because postcard
  spends one byte per `None` and the empty run list costs one byte per *row*. A spacer tail
  continues its wide character's run. `PROTOCOL_VERSION` 5 → 6. Client: `url::link_at_col`
  returns the OSC 8 run when there is one, else the plain-text URL with the columns it covers
  (`text_link_at_col`, offsets mapped back to cells, a wide cell's spacer inside the span);
  ⌘-click opens it, and while ⌘ is held the run under the pointer is underlined
  (`TerminalView::link_highlight` → a 1 px quad in `TerminalElement::paint`, so the shaped-line
  cache is untouched; `on_modifiers_changed` plus the modifiers on every move keep the state
  right whether ⌘ goes down before or after the pointer arrives). Covered by
  `osc8_links_become_runs_on_screen_and_in_history`, `plain_rows_carry_no_link_runs`,
  `links_are_found_by_column_and_clipped_on_resize`, `osc8_runs_win_over_the_text_scan`,
  `text_links_come_with_their_columns` and the `host_lines_links` golden. macOS only for now:
  the phone key bar arms ⌘ for remote windows but not for terminals, so a tap has nothing to
  read; long-press stays selection. On the phone (2026-09-05) the terminal key bar has a ⌘
  key beside ⌃: it arms one tap (`TerminalView::set_sticky_command`), the next left press
  opens the link under it through `cx.open_url` (`gpui_ios`'s `UIApplication.openURL`, in the
  fork) and disarms; covered by `sticky_command_opens_the_link_under_the_next_tap`.
- ✅ **Command blocks from OSC 133, shell integration injected by ptyd** (2026-09-05).
  *Injection:* the `ZDOTDIR` bootstrap every terminal uses (Kitty, Ghostty, WezTerm); the
  scripts are Slopty's own (Ghostty's zsh files are GPLv3, inherited from Kitty, so they
  were not copied). `.zshenv` restores `ZDOTDIR` (the original travels in
  `SLOPTY_ZSH_ZDOTDIR`), sources the user's `.zshenv`, then `slopty-integration.zsh` for
  interactive shells; `.zprofile`/`.zshrc`/`.zlogin` load from the user's directory as
  before. `A` and `B` live inside `PS1` (`%{…%}`) and `A;k=s`/`B` inside `PS2`, so zle
  redraws keep them; the precmd hook moves itself to the end of `precmd_functions` each
  time so a theme that rebuilds `PS1` in its own precmd cannot drop them; `C` is printed by
  preexec, `D;$?` by precmd only when a `C` is open (so a bare Enter reports nothing).
  Learned: `status` is a read-only zsh special, use another name. `install` writes the
  scripts (compiled in with `include_str!`) under `$SLOPTY_DATA_DIR/shell` (else `shell/`
  beside the socket) on every daemon start, rewriting an edited or stale file, so a running
  install never reads the source tree; it also reads the daemon's environment once into a
  `ShellIntegration` (`enabled`, the daemon's own `ZDOTDIR` and `XDG_DATA_DIRS`) so the
  per-spawn decision `apply` is pure: it takes the program, argv, arg0 and the session's
  environment and returns an `Injection` (argv, arg0, extra variables) that `Pty::spawn_with`
  applies. zsh: only when the resolved program's basename is `zsh` (the login shell, an
  explicit `zsh`, and the `$SHELL -lic` path from 52725da all qualify; `-c` shells load the
  hooks but never reach precmd, so they print no marks). Opt-out
  `SLOPTY_NO_SHELL_INTEGRATION=1` (anything but empty or `0`) in the daemon's environment or
  the session's, for all three shells. Verified by `an_interactive_zsh_emits_prompt_marks`
  (real `/bin/zsh -i` on a PTY: A/B/C, `D;1` after `false`, `ZDOTDIR` empty again, the
  user's `.zshenv` ran), `install_writes_the_bundled_scripts_and_is_idempotent` and
  `opt_out_from_the_daemon_or_the_session`.
  *bash* (2026-09-05): bash has no `ZDOTDIR`; the only hook that leaves the user's files
  alone is `--rcfile`, which bash reads *instead of* `~/.bashrc`, and which it ignores for
  login shells. So `apply` puts `--rcfile <shell>/bash/slopty.bash` at the front of argv
  (GNU long options must precede the short ones or bash says `--: invalid option`), strips
  `-l`/`--login` and the leading dash of arg0 and hands that fact over as
  `SLOPTY_BASH_LOGIN=1` (likewise `--noprofile`/`--norc` → `SLOPTY_BASH_NOPROFILE`/`NORC`);
  the rcfile then does what bash would have done (`/etc/profile`, the first of
  `.bash_profile`/`.bash_login`/`.profile` for a login shell, else `.bashrc`), unsets the
  variables, and returns unless interactive. Shells given a script, `-`, `-c`, `-o`,
  `--posix` or their own `--rcfile`/`--init-file` are left untouched. Marks: `A`/`B` inside
  `PS1` (`\[…\]`) and `A;k=s` in `PS2`, wrapped by the *last* `PROMPT_COMMAND` entry so a
  prompt theme that rebuilds `PS1` in its own entry still gets them; `D;$?` by the *first*
  entry (bash 5's `PROMPT_COMMAND` array and bash 3.2's string both handled). `C` needs
  preexec, which bash lacks: a `DEBUG` trap of Slopty's own (MIT-clean, not bash-preexec's
  code) fires once per prompt, armed by the prompt hook and disarmed by the first command it
  sees, so a prompt with a five-command `PROMPT_COMMAND` prints one `C`, not five. If the
  user already loads bash-preexec (`__bp_imported`), Slopty registers with its
  `preexec_functions`/`precmd_functions` instead; if some other `DEBUG` trap is installed,
  Slopty keeps its hands off it and emits prompt marks only (no `C`/`D`). Works on the
  system bash 3.2 and Homebrew's 5.3. Verified on both by
  `an_interactive_bash_emits_prompt_marks_and_runs_the_users_bashrc` (A/B/C, `D;1` after
  `false`, `.bashrc` ran, `.bash_profile` did not, the `SLOPTY_BASH_*` variables gone) and
  `a_login_bash_reads_its_profile_and_still_marks` (arg0 `-bash`: the profile ran, `.bashrc`
  did not, still marks).
  *fish* (2026-09-05): fish sources every `<dir>/fish/vendor_conf.d/*.fish` for each entry of
  `XDG_DATA_DIRS`, so `apply` prepends `<shell>/fish` to the session's (else the daemon's,
  else the `/usr/local/share:/usr/share` default) `XDG_DATA_DIRS`; the user's `config.fish`
  loads as before, after the vendor files. fish ≥ 4.0 prints OSC 133 itself (ST-terminated,
  `A;click_events=1`, `C;cmdline_url=…`; the engine's scanner accepts both terminators and
  ignores the parameters), so on 4.x the snippet only sets `__slopty_integrated 1` and steps
  aside; on 3.x it wraps `fish_prompt` once (`functions --copy`) with `A`/`B` and hooks
  `fish_preexec`/`fish_postexec` for `C`/`D;$status`. Verified with the installed fish by
  `an_interactive_fish_emits_prompt_marks_and_runs_the_users_config` (skipped with a note
  when no fish is on the machine). Learned: fish answers `DA1`/`CPR` queries at start and
  waits up to 10 s for the reply, so a PTY test must answer them; and it walks `cwd` for
  `mise` configs, so the test chroots its `cwd` to the temp home.
  *Engine:* libghostty-vt's per-row `semantic_prompt` flag says "prompt row" but cannot
  separate two prompts on adjacent rows (a command with no output), and the `D` status is
  not exposed at all. `slopty_engine::osc133::Scanner` watches the bytes (state kept across
  reads, payloads over 32 bytes dropped, `ESC \` and BEL terminators, `A;k=s`/`k=c` not
  counted as starts); `write` feeds the terminal up to each mark, settles, and records the
  cursor's absolute line in `prompt_starts` / `exit_marks` (both pruned below `base`, both
  cleared with the epoch). A prompt row is `Prompt { exit }` only on a recorded start, else
  `PromptContinuation`; the status is the newest `D` within 4 rows above the start that no
  other start already claimed (the shell may print a blank line or the partial-line `%`
  between `D` and the prompt). Covered by
  `prompt_rows_carry_the_previous_commands_exit_status` (adjacent prompts, a `D` split
  across two writes, a gap row, a two-row prompt, the history path) and
  `captured_zsh_bytes_keep_output_rows_and_statuses` (bytes recorded from a real zsh:
  synchronized output, the `%` partial-line marker, `D` directly before the next `A`).
  *Wire:* `SemanticMark::Prompt` grew `exit: Option<u8>`; every other row still costs one
  byte, a prompt row with a status three (MEASUREMENTS.md: an all-prompt 80×24 frame is
  48 bytes larger than a blank one). The brief's `End { exit }` variant was not added: the
  `D` lands on the row the next prompt starts on, so a separate variant would collide with
  `Prompt` on the same row. `PROTOCOL_VERSION` 6 → 7, golden `host_lines_marks`; the other
  goldens are unchanged because `Unknown` is still variant 0 and `client_hello` only moved
  its version byte.
  *Client/UI:* `TermState::prompt_before/after` walk the cached lines (uncached history is
  skipped, not fetched), `scroll_to_line` puts a line at the top, `last_command_output` is
  the run of `Output` rows above the newest prompt start with the blank tail trimmed
  (`prompt_navigation_and_last_output_follow_the_marks`). ⌘↑/⌘↓/⌘⇧C are Terminal-context
  bindings (`PrevPrompt`, `NextPrompt`, `CopyLastOutput`); the separator is a 1 px quad on
  the prompt-start row's top edge from the same prepaint pass as the selection
  (`separator_color`: fg at 18 %, the theme's `surfaces.error` token at
  `alpha::SEPARATOR_ERROR` (70 %) when the status is non-zero; `crates/slopty-theme/src/lib.rs`,
  `crates/slopty-ui/src/terminal/element.rs`), never on line 0. Search bar and selection are
  untouched. Headless:
  `cmd_up_and_down_walk_the_prompts_and_separators_follow` (separator rows and colours read
  from `painted_quads`, the three jumps and the return to following output) and
  `cmd_shift_c_copies_the_last_commands_output` (clipboard untouched without marks).
- ✅ **OSC 52: write only, system clipboard only, ≤ `MAX_CLIPBOARD_BYTES`** (2026-09-05).
  libghostty's `clipboard_write` callback (registered in `install_callbacks` next to the bell)
  hands over a normalised, decoded write; the engine keeps the `text/plain` representation of
  a `Standard` write and answers `Unsupported` for selection/primary (X11 notions with no
  client counterpart), the session drops writes over the pasteboard-sync ceiling (the same
  256 KiB constant from `slopty_proto::screen`; a whole file pasted through OSC 52 would sit
  ahead of every frame on the session stream) and broadcasts `TermEvent::ClipboardWrite` to
  every attached client, whose canvas writes it with `cx.write_to_clipboard`. Reads are not
  implemented, on purpose: libghostty never forwards a `?` request, and the dead
  `TermEvent::ClipboardReadRequest` / `TermRequest::ClipboardRead` pair (the client used to
  answer it with its clipboard, unprompted) is removed from the protocol so a future host
  cannot ask. Covered by `osc52_writes_to_the_system_clipboard_only` (standard, primary,
  selection, `?`) and the `host_term_clipboard_write` golden.
- ✅ Deployment is two LaunchAgents written by `slopty host install` (`plist` crate, XML):
  `KeepAlive` + `RunAtLoad` + `ThrottleInterval 2` so a crash comes back in 2 s and login
  starts both; `ProcessType Interactive` and `LimitLoadToSessionType Aqua` because hostd
  needs the window server and ScreenCaptureKit and must not be App-Napped; sockets under
  `<data dir>/run/` (not `$TMPDIR`, which launchd children may not share) and the CLI's
  socket lookup falls back to that path when it exists. `install` re-bootstraps (bootout
  first, so a stale socket file never wedges the bind) and waits up to 10 s for a ticket.
- ✅ **Conversation view: richer entries, protocol 11** (2026-09-05). `TranscriptEntry` became
  `{ at, body }` with `TranscriptBody::{User, Assistant, Thinking, ToolUse, ToolResult}` and
  a `Clipped { text, more_lines }` for the long parts. The host clips thinking, tool input and
  tool output to 40 whole lines or 4 000 characters (`slopty_agent::transcript::clip`), whichever
  first: a `Read` of a 40-line file or a `cargo test` tail fits, a minified one-line blob is
  cut at 4 000 characters with an ellipsis, and the wire never carries a whole file while the
  reader still sees how much was dropped. Results are named after their `tool_use_id` by a
  bounded table in the `Tail` (512 ids, then it starts over), since Claude Code batches several
  calls before their results. Timestamps ride as Unix milliseconds and the client shows local
  "HH:MM" (`chrono`, already in the tree). Goldens `host_transcript` (every variant) and
  `client_hello` re-accepted; PROTOCOL_VERSION 10 → 11.
- ✅ **The composer types into the pty; no new wire message** (2026-09-05). ↩ sends the text
  as `TermRequest::Paste` (the host brackets it when the program asked, so Claude Code takes a
  multi-line message as one prompt) followed by the same `TermRequest::Key` Enter the keyboard
  sends, then clears. A `ClientMsg::Prompt` would have needed the host to know which program
  reads the pty and how it wants its input; typing keeps one input path, keeps slash commands
  and `@file` exactly as if typed, and works for any prompt the agent shows (an empty ↩ is a
  bare Enter that accepts it). Esc and Control keys keep their terminal meaning from inside the
  composer so ⌃C interrupts without leaving the chat; gpui-kit's input binds ⌃C to Copy only
  off macOS, so on iOS a hardware ⌃C in the composer copies and the key bar's ⌃ + C is the
  interrupt. Allow / Deny in the view raise `TerminalViewEvent::Answered` and the canvas types
  Enter / Esc through `allow_agent` / `deny_agent`, one answer per state, exactly as the badge.
- ✅ **Collapse defaults** (2026-09-05). Thinking and tool input start folded (they explain a
  step, they are not the step); a tool result shows its first 4 lines (`RESULT_PREVIEW_LINES`)
  because the head of a result is usually the verdict ("running 2 tests", "error[E0308]"), and
  opens to the whole clipped text with the dropped-line count. Folds live in the client (a
  `HashSet` of indices) and survive appends, a reset drops them with the entries. The list
  uses gpui's `FollowMode::Tail`: it stops following on a wheel-up, resumes when scrolled back
  to the bottom, and the "↓ latest" pill (`scroll_to_end` + `Tail`) is the shortcut.
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
- ✅ **Attribution without hooks: four signals, strongest wins, hooks never overruled**
  (2026-09-05). A `claude` the human started by hand — or one running before
  `slopty hook install` — now gets the same pill, badges, attention and conversation view as a
  hooked one. `AgentSource` (on the wire, `Process < Title < Transcript < Hook`) says which
  signal the status came from, and `Tracker::observe` / `observe_progress` refuse to let a
  weaker one overwrite a stronger one's state. hostd's `agents::watch` reads every session
  every 750 ms:
  - **Foreground process.** `tcgetpgrp` on the PTY master names the tty's foreground group;
    `slopty_pty::process` describes its leader (`proc_pidinfo PROC_PIDTBSDINFO` for the name
    and start time, the `KERN_PROCARGS2` sysctl for `argv`, `PROC_PIDVNODEPATHINFO` for the
    cwd) and `slopty_agent::detect` decides, as a pure function over name and `argv`, whether
    that is Claude Code. Present → `Idle`, gone → the agent is cleared.
  - **Title.** What Claude Code paints into OSC 0/2 separates a running turn from an idle
    prompt. Nothing finer is readable from a title, and that is all it is used for.
  - **Transcript.** `slopty_agent::discover` finds the JSONL from the session's own working
    directory and `transcript::progress` reads `Working` / `Tool` / `Done` out of the newest
    record. It can never report `Blocked`: a permission prompt is not written to the
    transcript until it has been answered. Blocking is what hooks are for, which is why the
    app still offers to install them.
- ✅ **The title's tables are `◐◑◒◓` for a running turn and `✳` for an idle one — not the
  in-pane spinner** (2026-09-05, from live `terminal_title` data on this machine: `◐ Claude
  Code` while working, `✳ GPUI and gpui-kit upstream sync track` when done). Claude Code
  paints a spinning half circle in front of the title while a turn runs and, between turns, a
  sparkle in front of the conversation's summary (the bare name before there is one). The
  `·✢✳∗✻✽` frames are the spinner it draws in its own output; they never reach the title, so
  a *title* that starts with one of them is some other program and is read as nothing. An
  earlier revision of `slopty_agent::title` had the two sets swapped, which read every idle
  agent as working and every working one as idle.
- ✅ **`detect::is_claude` reads both the executable name and `argv[0]`, and treats shells and
  JS runtimes as launchers** (2026-09-05, measured). The two disagree often: `/bin/sh` on
  macOS is `bash` by executable and `/bin/sh` by `argv[0]` (`slopty-pty`'s
  `a_ptys_foreground_process_is_the_program_it_runs` pins that), the kernel rewrites `argv`
  for a `#!` script so `~/.claude/local/claude` shows as `sh <script>`, the npm install shows
  as `node …/claude-code/cli.js`, and `slopty_pty::pty::resolve_command` itself starts an
  unfound `claude` as `zsh -lic claude`. So a name or `argv[0]` of `claude` counts outright,
  and a runtime counts only for the *first* argument that is not a flag — the script it runs —
  or, for a shell's `-c`, for the first word of the command it was handed. A bare login shell
  (`argv[0] = "-zsh"`) does not count, and neither does `sh -c 'echo claude'` or
  `node server.js --model claude`: the agent's name as somebody else's argument proves
  nothing.
- ✅ **The transcript is the newest `.jsonl` in the project directory, modified at or after
  the agent process started** (2026-09-05, verified against `~/.claude/projects` directory
  names only, never their contents). Claude Code escapes the working directory by replacing
  every character that is not an ASCII letter or digit with `-`
  (`/Users/x/.config` → `-Users-x--config`), which fixes the directory; time picks the file
  inside it, because the live session is the one still being written and a resumed
  conversation moves its old file's mtime forward. The start time comes from the process
  table, so a hostd restart does not make an old conversation look new. Only that one
  directory is ever read. Two `claude`s started in the same directory in the same window
  resolve to the same newest file, and the second one to write wins; the terminals are told
  apart by their sessions but their transcripts are not, which is a limit of the discovery
  and a reason the app offers the hooks.
- ✅ **The lookup is repeated, because `/clear` starts a new file** (2026-09-05). A tracker
  that already has a transcript keeps asking (`AgentTable::discoveries` carries the file
  being read, `Tracker::discovery` only stops for a hooked session whose hook named one), and
  hostd re-runs it every eighth tick (6 s); when the newest file in the project directory is
  not the one being tailed — `/clear`, `/resume <other>`, a compaction — the path moves and
  the `Tail` is dropped so the new conversation is read from its top. Without this the status
  froze on the old file's last record, so a cleared session sat on `Done` through its next
  turn.
- ✅ **A tracker follows a process, not a terminal** (2026-09-05). `Observation` carries the
  foreground process's pid and start time, and a tracker whose process changes is reset before
  it is attributed again: one `claude` exiting and another starting inside a 750 ms tick would
  otherwise inherit the first one's transcript, status and hooks in a terminal that looks
  unchanged. The reset also clears the transcript path, so the next lookup finds the new
  conversation and the tail starts over.
- ✅ **A poll event a hook has overtaken is dropped, not sent** (2026-09-05). hostd computes
  the tick's events under the lock and broadcasts them *before* it reads any file, and every
  event is checked against the table (`AgentTable::is_current`) immediately before it goes
  out. A hook arriving on the control socket between the poll and the send has already told
  every client something newer; without the check the poll's older state was put back and, for
  example, a permission badge vanished until the next hook.
- ✅ **Hooks decide the status; the process table may still end a session it watched**
  (2026-09-05). Once a hook has spoken, the weaker signals fill gaps only — the transcript
  path, so ⌘⇧L works whether it was named by a hook or discovered — and never change the
  status. Ending is nearly as strict: a hooked agent goes when `SessionEnd` says so, or when
  a `claude` that was actually seen in the tty's foreground has been absent for four probes
  (3 s). One probe is not enough because the relay (`slopty hook`) is itself briefly the
  foreground process of the terminal it reports on, and a session that only ever spoke
  through hooks (never seen as a process) is never ended this way at all — which is what
  keeps the played-hook self-tests honest, since those play hooks into a plain shell. The
  case this buys: a `claude` killed with `SIGKILL` sends no `SessionEnd` and would otherwise
  keep its pill until the terminal exited.
- ✅ **A first transcript read never raises attention** (2026-09-05). `Done` alerts only when
  the previous status was `Working` or `Tool`: the first read of a discovered file is usually
  a conversation that ended hours ago, and a Dock bounce for it would be a lie.
- ✅ **"install hooks" is a pill on the title bar of the first unhooked agent, and hostd
  writes the settings** (2026-09-05). The human whose agent is being guessed at may be on a
  phone, so `ClientMsg::InstallHooks` asks the host to do it and `HostMsg::HooksInstalled`
  comes back as a notice. The installer moved from `slopty-cli` into `slopty_agent::hooks` so
  the CLI and the daemon run the same code; hostd registers the `slopty` binary beside itself
  (`Contents/MacOS` in a bundle, `target/<profile>` in a build tree). The offer shows once per
  run and comes back only if the host reports it failed. Wire, with the `AgentSource` of the
  attribution ruling above: `AgentEvent.source`, `ClientMsg::InstallHooks` and
  `HostMsg::HooksInstalled`, goldens `host_agent_process`, `host_agent_hook`,
  `client_install_hooks` and `host_hooks_installed` (all new) with `client_hello` re-accepted,
  PROTOCOL_VERSION 11 → 12.
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
- ✅ **Structured driving is Claude Code's own stream-json protocol over stdio, not ACP**
  (2026-09-12, replaces the ⏸ ACP plan). Verified against CLI 2.1.268 with live probes
  (`claude -p --verbose --input-format stream-json --output-format stream-json
  --permission-prompts host --permission-prompt-tool stdio --include-partial-messages
  --replay-user-messages`, working directory the repo, no PTY): stdout is NDJSON of
  `system/init` (session id, model, tools, permission mode, slash commands), `system/status`,
  `system/thinking_tokens`, `system/permission_denied`, hook lifecycle records,
  `rate_limit_event`, `stream_event` (the API's own `content_block_delta`s, so text streams
  token by token), `assistant` and `user` records **in the same shape as the JSONL transcript**
  (so `slopty_agent::transcript` maps them unchanged), `result` (`subtype` `success` |
  `error_during_execution`, `is_error`, `num_turns`, `total_cost_usd`), and
  `control_request {request_id, request: {subtype: "can_use_tool", tool_name, display_name,
  input, description, permission_suggestions, tool_use_id}}` whenever a tool needs the human.
  The host writes `{"type":"user","message":{"role":"user","content":…}}` to prompt,
  `{"type":"control_response","response":{"subtype":"success","request_id",
  "response":{"behavior":"allow","updatedInput":…}}}` or `{"behavior":"deny","message":…}` to
  answer (a denial comes back as an error `tool_result` carrying the message), and
  `{"type":"control_request","request_id","request":{"subtype":"interrupt"}}` for Esc (acked
  with `control_response {still_queued: []}`, then a user "[Request interrupted by user]" and a
  `result`). The process lives across turns until stdin closes; `--resume <session id>` reopens
  a conversation. Prompts fire only for tools the user's settings do not already allow
  (`--restricted` ignores settings; the probes needed it on this machine, whose settings allow
  `Write`). Rejected: ACP (`@agentclientprotocol/claude-agent-acp` is a Node adapter around this
  very protocol, so it would add a runtime and a translation for nothing), and the docs' inferred
  `control_type` shape (wrong on the wire; the shapes above are what 2.1.268 emits).
  **What this buys over observing the TUI:** the conversation streams live instead of a 400 ms
  file tail; a permission arrives with the tool's full input and is answered in-protocol instead
  of by typing into a menu; the composer sends a message instead of keystrokes; Esc is an
  `interrupt`; the agent's session id and cost are known. What it costs: no Claude Code TUI in
  that card (its slash commands still work as messages — `init` lists them). Both kinds
  coexist: a `claude` typed into a shell stays the observed PTY agent it is today.
  Plan, landing in commits: (1) `slopty_agent::stream` — the pure protocol layer (parse every
  record into an `Event`, fold events into `TranscriptEntry`s, `AgentStatus` and
  `PermissionRequest`s, build the outbound lines), fixtures from the probes; (2) hostd runs the
  process per agent session and the wire carries `AgentRequest`/`AgentUpdate` (protocol bump), a
  fake `claude` in `slopty-e2e` replays the fixtures for the self-test; (3) an agent card on the
  canvas hosting the conversation view without a grid, on the Mac and the phone.
  Landed (2026-09-12, protocol 15): (1) as `slopty_agent::stream`; (2) as `hostd::driven` with
  `ClientMsg::{OpenAgent, AgentSay, AgentAnswer, AgentInterrupt}` and
  `HostMsg::{AgentPartial, AgentPermission}`, the session a `SessionKind::Agent` in the
  ordinary session list so the canvas, many-clients and reattach machinery is reused unchanged;
  (3) as `TerminalView`'s driven mode (ARCHITECTURE, "Driven agents"). Two rulings made on the
  way: the fake `claude` for the self-test is a Rust binary (`slopty-fake-claude`) handed to the
  host as `SLOPTY_CLAUDE_BIN`, not a shell script on `PATH`, because the host launches the real
  one through the login shell and a test must not depend on the tester's rc files; and the
  driven session keeps ⌘⇧T's PTY `claude` beside it (⌘⌥T / "New Agent (Structured)" opens the
  driven one) until the structured card has proven itself day to day — the TUI's own rendering
  of diffs, todo lists and slash-command output has no equal in the card yet.
- ✅ **A driven agent is retuned in place and resumed from its transcript directory,
  protocol 16** (2026-09-12). Probed on CLI 2.1.269 over the same stdio protocol:
  `control_request` `set_model` and `set_permission_mode` are acknowledged without a restart
  (`{"subtype":"success"}`; the mode change is followed by a `system/status` record naming
  the mode), while `supported_models` and `supported_commands` answer "Unsupported control
  request subtype". Rulings: (1) the model list is a fixed table of Claude Code's own aliases
  (`fable`, `opus`, `sonnet`, `haiku`; `--help` documents the alias form) rather than a
  guessed set of full names, and the header chip shows the full name the agent's next
  assistant record carries, so a wrong alias shows as the agent's word, not ours; (2) the
  slash-command list is `init`'s `slash_commands`, sent once per session inside `AgentInfo`
  rather than re-asked; (3) the host answers a retune with the whole `AgentInfo` again, not
  a delta, since it is small and a client joining late needs the same message; (4) the cost
  rides as `cost_micro_usd: u64` so the wire type stays `Eq`; (5) "Resume" lists the host's
  `~/.claude/projects/<escaped cwd>` directory itself (the same escaping `discover` already
  verified) named by the first human prompt, with the rule that a transcript holding no
  prompt is not listed — Claude Code writes a file for a `/clear` or a hook run too, and
  resuming one of those shows an empty conversation; (6) the completion keys are caught in
  GPUI's capture phase as *actions* (`MoveUp`/`MoveDown`/`Escape` of gpui-kit's input) and
  not as key events: GPUI dispatches a matched key binding's action before the raw key
  event, so an `on_key_down` on the card never saw ↓ (the headless test caught it: Tab took
  the first match, not the second); (7) Tab completes as a `CompleteSlash` action bound in
  the "Terminal" context that propagates when there is nothing to complete, because
  gpui-kit's `Root` binds "tab" to its own focus-ring action and would have taken the key
  first (the app self-test caught it: Tab landed on "Send"); (8) the project directory is
  named by the *canonical* working directory (`discover::project_dir` resolves it when it
  exists): Claude Code names it by `process.cwd()`, which on macOS is `/private/var/…` for a
  `/var/…` the host was given, and the self-test's temp HOME is exactly such a path — the
  listing found nothing until then; (9) after ⌘W removes the active item, `reconcile` makes
  the canvas take the keyboard back on its next frame instead of leaving focus on the dead
  handle, which is why ⌘⌥R right after a close now lands (and why the arrange-by-repo golden
  moved: the remaining note draws as the active item, as it should); (10) a resumed card is
  seeded with the transcript's last entries before the agent speaks — Claude Code's
  `--resume` replays nothing over stream-json, and a card that opens empty on "Resume" is
  worse than no resume at all; (11) while the agent works the composer's Send is a Stop
  (`composer-stop`, `AgentInterrupt`), because on the phone a runaway turn had only the key
  bar's Esc, and the fake's `linger` turn is now ended by a click on it. The self-test's private
  `HOME` (also given to the driven launch now, the way the fake-claude launch already did)
  keeps the fake's transcripts out of the developer's `~/.claude`.
- ✅ **A tool call rides the wire as what it would do, not as its JSON, protocol 17**
  (2026-09-12). The card showed every tool call as a name, a one-line summary and its input
  as pretty JSON behind a fold — a `Bash` read fine, an `Edit` did not: the change was two
  JSON strings with escaped newlines, and the TUI's inline diff was the one thing the card
  had no answer to (the ruling above kept ⌘⇧T's TUI beside the card for exactly that).
  Rulings: (1) the host reads the input into a `ToolDetail` (`Command`, `Diff`, `Write`,
  `Read`, `Search`, `Todos`, `Agent`, `Json`) and the client draws the shape — the host
  already knows Claude Code's tools (it names their summaries), the diff is computed once for
  every client instead of on each, and a phone with no `similar` and no JSON parse in its
  paint path is the point; (2) the diff is `old_string` against `new_string` line by line
  (`similar` 3.2, `TextDiff::from_lines`), so a one-line change inside a ten-line
  `old_string` reads as context around one removed and one added, cut to the same 40 lines /
  4 000 characters as any long text with the count of what was dropped; (3) a known tool
  whose input lacks the field it is known by degrades to `Json`, never to an empty block, so
  a shape change on the agent's side shows what the card showed before; (4) a `Todo` with
  no text is dropped and the status is Claude Code's three (`pending`, `in_progress`,
  `completed`); (5) the `PermissionRequest` carries the same `ToolDetail` in place of its
  JSON, so the row and the entry never disagree; (6) on the card an edit's diff and a todo
  list show *unasked* — the diff is the point of the entry — the diff folded past 12 lines
  (`DIFF_PREVIEW_LINES`, three times a result's 4: a diff is read whole or not at all),
  everything else opens on the click it always did; (7) a screen reader hears the counts,
  not the lines. Wire: `TranscriptBody::ToolUse.input` → `detail`,
  `PermissionRequest.input` → `detail`, goldens `host_transcript` /
  `host_agent_permission` / `client_hello` re-accepted and `host_transcript_tools` (every
  shape) added, PROTOCOL_VERSION 16 → 17. Tests:
  `an_edit_is_a_diff_a_todo_list_a_checklist_and_a_stranger_its_json`,
  `a_long_diff_is_cut_like_any_long_text` (`slopty_agent::transcript`), headless
  `an_edit_shows_its_diff_and_a_todo_list_its_checklist` (tinted rows counted in the scene,
  the fold, the labels), and the app self-test's `edit` turn against the fake (an `Edit` and
  a `TodoWrite` the settings allow, golden `conversation-tools`).
- ✅ **A question is answered on the card, not allowed, protocol 18** (2026-09-12).
  Probed on CLI 2.1.269 over stdio: `AskUserQuestion` arrives as an ordinary `can_use_tool`
  control request (`tool_name: "AskUserQuestion"`, `requires_user_interaction: true`, the
  input `{questions: [{question, header, options: [{label, description}], multiSelect}]}`),
  and the answer is an *allow* whose `updatedInput` is the input plus
  `answers: {"<question text>": "<label>"}`; the agent then receives the tool result "Your
  questions have been answered: "…"="…". You can now continue with these answers in mind."
  and goes on. Before this the card offered Allow / Deny for it: Allow sent the input back
  with no answers, which is a question nobody answered. Rulings: (1) the fold reports the
  request as `Blocked(Question)`, the same reason the hook path uses for the TUI's question,
  so the badge, the notification ("Claude has a question", an "answer" pill that reveals the
  card, no Allow / Deny buttons) and the caret-in-composer rule already apply; the
  `PermissionRequest` still rides beside it, carrying the `ToolDetail::Question`; (2) the
  options are buttons, and a single-select question is answered by the one tap — the
  fewest gestures on a phone — while a multi-select one toggles and sends on Answer, which
  is disabled until every question has a pick; (3) typed text is the "Other" answer to the
  first question without one, since the TUI offers exactly that and a prompt while the
  agent waits would be lost anyway; (4) the answer is filed under the question's text,
  never its index, because that is the key Claude Code reads (the probe confirmed the
  wording of the result); labels of a multi-select are joined with ", " (the SDK's
  convention; unverified on the wire, marked in `Question::multi`'s doc); (5) the wire
  carries `answers: Vec<QuestionAnswer>` on `AgentAnswer` (empty for a permission) rather
  than a new message, so the host's one answer path and the "answered once" rule serve both.
  Wire: `ToolDetail::Question`, `Question`, `Choice`, `QuestionAnswer`, goldens
  `client_agent_answer` / `client_hello` / `host_transcript_tools` re-accepted,
  `client_agent_answer_question` and `host_agent_question` added, PROTOCOL_VERSION 17 → 18.
  Tests: `a_question_blocks_as_a_question_and_the_answers_are_filed_into_the_input`
  (`slopty_agent::stream`, on the probe's line), headless
  `a_driven_view_answers_a_question_in_place` (one tap; two questions with a multi-select
  and Answer; typed text), the app self-test's `ask me` turn against the fake (the options
  in the accessibility tree, no Allow, the tap on "Blue", the result's wording, golden
  `conversation-question`).
- ✅ **"Always" is the agent's own suggestion echoed back, protocol 19** (2026-09-12).
  Probed on CLI 2.1.269: a `can_use_tool` for `Write` carries
  `permission_suggestions: [{type: "setMode", mode: "acceptEdits", destination: "session"}]`;
  an allow whose response adds `updatedPermissions: <those suggestions>` made the next
  `Write` of the same turn run without asking, and a `system/status` record naming
  `acceptEdits` followed (the mode chip moves through the path protocol 16 built). The TUI
  offers exactly this as its "Yes, and don't ask again" line; a card that can only say yes
  once makes the human answer every edit of a long turn from the phone. Rulings: (1) the
  host never invents a rule — it echoes the agent's suggestions verbatim, so what "Always"
  does is what the TUI would have done and nothing wider; (2) the suggestions stay on the
  host beside the input (`stream::Pending`) and the wire carries only their meaning in the
  human's words (`PermissionRequest::always`, `always_label`: mode + destination, or the
  rules as `Tool(content)` + destination), so the client draws a sentence and the phone
  never parses a permission schema; (3) no suggestion, no button — `always: None` — and an
  `always: true` on such a request is a plain allow, never an error; (4) the button names
  its effect under the word ("Always" / "accept edits for this session") and a screen
  reader hears both, because a permission taken for the whole session is the one answer
  worth a second's reading; (5) `AgentAnswer` became a struct (`session, request, allowed,
  message, answers, always`), the way `AgentSet` already was, once it reached six fields.
  Wire: `PermissionRequest.always`, `AgentAnswer.always`, `ClientMsg::AgentAnswer(AgentAnswer)`,
  goldens `client_agent_answer` / `client_agent_answer_question` / `client_hello` /
  `host_agent_permission` / `host_agent_question` re-accepted, `client_agent_answer_always`
  added, PROTOCOL_VERSION 18 → 19. Tests:
  `an_always_allow_echoes_the_agents_suggestion_and_a_plain_one_does_not` (the probe's
  line, the bare line, the rule wording), headless `a_driven_view_speaks_to_the_agent`
  (Always between Allow and Deny in the tree, one tap answers for good), the app self-test
  (`write once more` taken with Always: the chip reads Accept edits; `write yet again`
  asks nothing).
- ✅ **A subagent is followed under its call, never shown as the agent, protocol 20**
  (2026-09-12). Probed on CLI 2.1.269 with an `Agent` (Explore) call: the subagent's own
  `assistant` and `user` records stream on the same stdout with `parent_tool_use_id` set to
  the spawning call, and around them come `system/task_started` (`tool_use_id`,
  `description`, `subagent_type`, `prompt`, `is_backgrounded`) and `system/task_progress`
  (`usage.tool_uses`, `usage.duration_ms`, `last_tool_name`, a "Running …" description);
  no `task_completed` was seen — the call's own `tool_result` ends it. Before this the card
  showed the subagent's thinking, tool calls and results inline as the agent's, moved the
  status chip to the subagent's tools, and the last assistant line could be the
  subagent's. Rulings: (1) `is_subagent` (sidechain in the file, `parent_tool_use_id` on
  the stream) gates entries, progress and the model in one place — the transcript reader —
  so the JSONL tail and the stream can never disagree; (2) the progress is one
  `AgentTask` per spawning call, upserted (a start after progress keeps the counts) and
  marked done by the result, sent whole each time like `AgentInfo`, and kept per session so
  a client joining mid-run sees the running subagents; (3) the card needs the call id to
  hang the task on, so `ToolUse` carries `call` — the same `tool_use_id` the results are
  named after — rather than the client guessing by order, which breaks with two subagents;
  (4) the line under the call says kind, state, tool uses, seconds and last tool and the
  label says the same, because on the phone the subagent's minutes are the only sign the
  turn is alive. Wire: `TranscriptBody::ToolUse.call`, `AgentTask`, `HostMsg::AgentTask`,
  goldens `host_transcript` / `host_transcript_tools` / `host_agent_sessions` /
  `client_hello` re-accepted, `host_agent_task` added, PROTOCOL_VERSION 19 → 20. Tests:
  `a_subagents_own_records_are_not_entries` (`transcript`),
  `a_subagent_is_followed_by_its_task_records_and_its_own_are_hidden` (`stream`, the
  probe's six lines), headless `a_subagent_progresses_under_its_call` (the label through
  running and done, the reset), the app self-test's `delegate the listing` turn (the
  subagent's Bash absent, the task done in the dump and in the tree).
- ✅ **The card shows the subscription's windows and what the agent is doing between tokens,
  protocol 21** (2026-09-12). Probed on CLI 2.1.269: right after `system/init` comes a
  `rate_limit_event` with `rate_limit_info.unifiedWindows.five_hour` / `seven_day`
  (`utilization` 0–1, `resetsAt` epoch seconds) and a `status` that reads `rejected` when the
  subscription is spent; an API-key run never sends one. And each turn starts with
  `system/status { status: "requesting" }` (and `compacting` around auto-compaction), before
  any content: until now the chip sat on "working" with an empty card and a stalled agent
  looked the same as one waiting on the model. Rulings: (1) the windows ride in `AgentInfo`
  as `usage: Option<Usage>` — whole percent per window, computed as the smallest integer not
  below the fraction (no float cast, so no lint carve-out) — because it is what the agent says
  about itself and a joining client needs it with the snapshot; (2) the chip says both
  windows tersely ("5h 23% · 7d 74%") and only when limited names the earliest reset
  ("limited until 14:00"), since on the phone the question is "can I keep going" not the
  raw numbers; (3) `requesting` / `compacting` become the Working detail and never touch a
  Blocked status, so a permission or a question is not overwritten by the next poll. Wire:
  `AgentInfo.usage`, `Usage`, `UsageWindow`; goldens `host_agent_info` / `client_hello`
  re-accepted, PROTOCOL_VERSION 20 → 21. Tests:
  `a_policy_denial_and_the_rest_are_named_or_ignored` (`stream`: the probe's rate-limit
  line, the bare rejected one, `requesting` and `compacting` as detail and not over a
  permission), headless `a_driven_view_shows_the_agent_and_retunes_it` (the usage chip in the
  tree), the app self-test (the fake sends the probe's event after init; the dump's `usage`
  reads "5h 23% · 7d 74%").
- ✅ **A picture pasted into the composer goes to the agent as an image block, protocol 23**
  (2026-09-12). Probed on CLI 2.1.269: a stream-json user message whose content is
  `[{type: image, source: {type: base64, media_type, data}}, {type: text}]` is accepted,
  replayed as sent, written to the transcript as sent, and the model read a 64×64 PNG ("What
  colour is this image?" → "Blue"). On the phone a screenshot is the fastest way to show the
  agent something; without this the card could only type. Rulings: (1) the client decides
  what is a picture — ⌘V captures the input's `Paste` action and takes the clipboard's image
  entries, letting a text clipboard fall through to the input — rather than the host sniffing
  bytes, because the clipboard's format is known where it is read; (2) only the types the
  model reads are attached (PNG, JPEG, GIF, WebP): a TIFF, what a copied macOS screenshot
  can be, is dropped with a log line rather than transcoded, until a measurement says the
  paste path needs it; (3) caps live in `slopty-proto` (`IMAGES_MAX` 4, `IMAGE_BYTES_MAX`
  4 MiB, under the model's 5 MB with room for the JSON) and the host refuses a prompt over
  them whole — a half-sent prompt would be worse than none; (4) the wire carries the bytes
  only client → host: the transcript entry says how many pictures went with the prompt, so
  a joining client and a resumed card show "1 picture" without shipping megabytes back, and
  the host's own user entry and the agent's replay agree (`is_replay` compares the count).
  Wire: `AgentSay.images: Vec<Image>`, `TranscriptBody::User.images`; goldens
  `client_agent_say` / `host_transcript` / `client_hello` re-accepted, PROTOCOL_VERSION
  22 → 23. Tests: `tool_results_are_named_after_their_call_and_injected_texts_are_not_entries`
  (`transcript`: a picture with text is one entry, two alone are an entry of their own),
  `a_policy_denial_and_the_rest_are_named_or_ignored` (`stream`: the blocks, no empty text
  block), headless `a_picture_pasted_into_the_composer_goes_with_the_prompt` (chip label,
  the tap, text still text, TIFF refused, sent with the text and alone), the app self-test
  (the socket's `attach`, the chip in the dump, the fake reading back "image/png 70 B", the
  bubble's "1 picture").
- ✅ **The phone pastes a screenshot through the key bar; the fork reads pictures off
  `UIPasteboard`** (2026-09-12). The fork's `gpui_ios` clipboard was text-only both ways, and
  the key bar's "paste" on a driven card sent a `TermRequest::Paste` to a session the agent
  does not have. Rulings: (1) the fork reads a picture as the encoded bytes under the
  format's uniform type (`public.png`, `public.jpeg`, `com.compuserve.gif`,
  `org.webmproject.webp`, then TIFF and BMP) when `hasImages`, before text — a screenshot is
  a PNG and the bytes are what the model wants, so no `UIImage` round trip that would
  re-encode; it writes an `Image` entry the same way (`setData:forPasteboardType:`), which
  is what lets a test put a picture on the simulator's own pasteboard through GPUI instead of
  `simctl pbcopy` from the Mac's (fork `f629234166` on `8ffbc6e145`, pin moved; first try
  crashed the app on the simulator: `-[NSData bytes]` received as `*const u8` fails objc2's
  debug-build encoding check, it must be `*const c_void` then cast); (2) on a
  driven card `paste_clipboard` is the composer's paste: pictures attach, text is inserted at
  the caret (`Conversation::insert_composer_text`), nothing goes to a session; (3) the Mac
  scenario keeps using the socket's `attach` because the Mac pasteboard is every app's.
  Tests: headless `a_picture_pasted_into_the_composer_goes_with_the_prompt` (the key bar
  path: a picture attached, text in the composer, no session paste), the simulator's
  `the_driven_agent_card_on_the_simulator` (the socket's `clipboard`, a finger on "Paste",
  the chip, ↩ and the fake reading back "image/png 70 B" — the fork's read and write on a
  real `UIPasteboard`).
- ✅ **A pasted picture is made fit off the UI thread, and a refused one says why**
  (2026-09-12). A phone screenshot is a 2–6 MB PNG, a photo a 3–12 MB JPEG; the first cut
  dropped anything over the 4 MiB cap with a log line nobody sees, and pasted a TIFF into
  silence. Rulings: (1) `terminal::attachment::fit` decodes the picture and, when it is over
  1568 px on the long side (what the model reads at full detail) or over the cap, shrinks it
  (Triangle: a few times faster than Lanczos, indistinguishable to the model) and re-encodes
  it — PNG when it has transparency, JPEG q85 otherwise, q70 as a second try — so the wire
  carries hundreds of KB, not megabytes; a picture already inside both bounds passes through
  untouched, because re-encoding what fits would only lose; (2) the fit runs in
  `cx.background_spawn`, with the count of pictures in flight drawn as a muted "preparing…"
  chip, because decoding a 12 MB JPEG on the UI thread is a visible hitch on a phone; (3)
  what is refused — a type the model does not read, undecodable bytes, a fifth picture —
  reaches the top bar as a notice through a new `TerminalViewEvent::Notice` /
  `CanvasEvent::Notice` pair, the route `HooksInstalled` already used; the fifth is refused
  before any decoding; (4) `slopty-ui` takes `image` with the four decoders, which GPUI
  already builds, so the binaries grow by nothing; the e2e scenarios paste a real 64×48 PNG
  (`snapshot::tiny_png`) since made-up bytes no longer decode. Tests: `attachment` (a small
  picture passes through byte for byte, 3200×1400 → 1568×686 JPEG, transparent 600×2000 →
  470×1568 PNG, a TIFF and garbage refused by name), headless
  `a_picture_pasted_into_the_composer_goes_with_the_prompt` (the big one lands shrunk as a
  JPEG, the TIFF's and the fifth's notices), the app and simulator scenarios on the real
  PNG.
- ✅ **The badge says what the agent is doing between records: thinking, or which tool it is
  composing** (2026-09-12). With `--include-partial-messages` Claude Code streams every
  content block's opening (`content_block_start` with `content_block.type` `thinking`,
  `text` or `tool_use` + `name`) before any delta, and a long think or a large tool input
  (a whole file for `Write`) can take many seconds during which only `text_delta` was read:
  the badge sat on "working" — and the protocol 21 "waiting for the model…" detail was never
  drawn either, since the badge text ignored a Working detail. Rulings: (1) `Block::{Thinking,
  Text, Tool(name)}` parse into `Event::BlockStart`, and the fold turns them into the Working
  detail ("thinking…", "calling Write…", none for text — the partial says it) under the
  protocol 21 rule: never over a Blocked or Done status; (2) the badge shows the Working
  detail when there is one ("working" otherwise), so `requesting`, `compacting`, a thought
  and a tool being composed all move the chip; (3) thinking deltas are not shown — Claude
  Code's own UI hides them — and no wire change: `AgentEvent.detail` already carries it.
  Tests: `text_deltas_stream_the_partial_and_the_record_clears_it` and
  `a_policy_denial_and_the_rest_are_named_or_ignored` (`stream`: the openings as detail, a
  tool opening not over a permission), the app self-test's `ponder` turn (the fake opens a
  thinking block and waits; the dump's new `agent_detail` reads "thinking…", Stop interrupts
  it). Every fake turn now opens its text block first, as the real CLI does.

- ✅ **The card shows how full the context window is, protocol 24** (2026-09-12). Claude
  Code's own status line answers "how much room is left" and Slopty's card did not: the
  only way to know a compaction was near was to be surprised by it. Probed on CLI 2.1.269:
  every assistant record's `message.usage` carries `input_tokens`,
  `cache_creation_input_tokens` and `cache_read_input_tokens`, whose sum is the context the
  request carried (the model's view of the conversation), and each `result` carries
  `modelUsage.<model>.contextWindow` (200 000 for haiku, 1 000 000 for the 1M models). There
  is also a `get_context_usage` control request with a per-category breakdown, but it is
  answered only where a callback is registered (the SDK host), not over stdio, and a
  round-trip per turn buys nothing the records do not say. Rulings: (1) `AgentInfo.context:
  Option<Context { tokens, window: Option<u64> }>` — the sum from the latest assistant
  record, the widest window the turn's `modelUsage` named (the main model's; a haiku subagent
  never widens it); it rides in `AgentInfo` because a joining client needs it with the
  snapshot; (2) the chip reads "ctx 16%" once the window is known and "ctx 31k" before the
  first result (never a guessed window), in the warn tone from 80% since auto-compaction is
  near; (3) the fold reports only changes, and a compaction needs no special case: the next
  assistant record's smaller sum lowers the chip by itself. Wire: `AgentInfo.context`,
  `Context`; goldens `host_agent_info` / `client_hello` re-accepted, PROTOCOL_VERSION 23 →
  24. Tests: `the_context_fill_follows_the_usage_and_the_result_names_the_window` (`stream`),
  headless `a_driven_view_shows_the_agent_and_retunes_it` (the chip in the tree, the labels),
  the app self-test (the fake's records carry 40 000 tokens against a 200 000 window: "ctx
  20%" in the dump's `context`).
- ✅ **A compaction is a divider in the card, not a prompt the human never typed, protocol
  25** (2026-09-12). When Claude Code compacts (auto, or `/compact`) it writes a
  `system/compact_boundary` record — on stdout with `compact_metadata { trigger, pre_tokens,
  post_tokens }`, in the transcript file with `compactMetadata { trigger, preTokens,
  postTokens }` — followed by a user record flagged `isCompactSummary: true` whose content
  is the summary ("This session is being continued…"). Until now the boundary was dropped
  (no `message`) and the summary drew as a You bubble of several screens. Rulings: (1)
  `TranscriptBody::Compacted { trigger, pre_tokens, post_tokens }` is an entry, drawn as a
  ruled divider "compacted (auto): 167k → 12k" (label "Compacted (auto): …"), so the card
  shows where the agent's memory of the conversation became a summary; (2) the flagged
  summary record yields no entry — it is the agent's reading, not the human's words — and
  nothing else about it is special-cased (no text-prefix sniffing); (3) the boundary's
  `post_tokens` lowers the context chip at once rather than waiting for the next assistant
  record. Both key spellings are read since the fold sees the stream live and the file on
  resume. Wire: `TranscriptBody::Compacted`; goldens `host_transcript` / `client_hello`
  re-accepted, PROTOCOL_VERSION 24 → 25. Tests:
  `tool_results_are_named_after_their_call_and_injected_texts_are_not_entries` (`transcript`:
  the file spelling, the summary hidden),
  `the_context_fill_follows_the_usage_and_the_result_names_the_window` (`stream`: the stream
  spelling, the chip lowered, the entry stamped), the app self-test's `/compact` turn (the
  divider last, "ctx 3%", no "continued from" entry, and the divider in the resumed past).
- ✅ **What the loop says, not the model, is a notice line in the card; the slash list and a
  backgrounded task follow their records, protocol 26** (2026-09-12). Read from the CLI
  2.1.269 bundle's own schemas (`strings` on the binary, the zod descriptions): a hook's
  feedback, a slash command's output and non-error status lines arrive as
  `system/informational { content, level: info|notice|suggestion|warning, tool_use_id?,
  prevent_continuation? }` ("Hosts render `content` as plaintext at the given level"; hook
  feedback is spelled "<Hook> says: …"); a turn moved to the fallback model as
  `system/model_fallback { trigger, original_model, fallback_model, content }`; a retry after
  a mode change allowed denied commands as `system/permission_retry { content, commands }`;
  a mid-session change of the slash list as `system/commands_changed { commands: [{name,
  description, argumentHint}] }` ("clients should REPLACE their cached command list"); a
  backgrounded task's end as `system/task_notification { tool_use_id?, status, summary,
  usage }`. All were dropped before, so a Stop hook's reason or "Allowed cargo test" never
  reached the card and a background subagent stayed "running" forever. Rulings: (1)
  `TranscriptBody::Notice { level: NoticeLevel::{Notice, Suggestion, Warning}, text }` is an
  entry drawn as one plain line in the muted, accent or warn tone; `info` lines are not
  entries (Claude Code shows them only in its transcript mode) nor are lines keyed to a
  `tool_use_id` (progress that would repeat); (2) a fallback reads "Switched to <model> for
  this turn: <why>" as a warning, a retry as a notice; (3) `commands_changed` replaces
  `AgentInfo.slash_commands` whole, names given their slash; (4) `task_notification` marks
  the task done and keeps the brief and kind already known — one without a `tool_use_id`
  cannot be joined and is ignored; the fold now ORs `done` so a late progress record never
  un-finishes a task. Wire: `TranscriptBody::Notice`, `NoticeLevel`; goldens
  `host_transcript` / `client_hello` re-accepted, PROTOCOL_VERSION 25 → 26. Tests:
  `tool_results_are_named_after_their_call_and_injected_texts_are_not_entries`
  (`transcript`: the hook line, the `info` and keyed lines skipped, the fallback, the retry),
  `a_subagent_is_followed_by_its_task_records_and_its_own_are_hidden` (`stream`: the
  notification ends the task, one without a call is nothing, the command list), the app
  self-test's `hooked` turn (the warning line between the prompt and the answer).
  Two more warning lines, no wire change: a `system/status` with `compact_result: failed`
  reads "Compaction failed: <compact_error>" (the `compacting` detail alone would have left
  the card looking as if it had worked), and a `system/stop_hook_summary` whose
  `hook_errors` is non-empty reads "Stop hook failed: …" — the summary is otherwise not an
  entry, since a hook that ran already spoke through `informational`.
  Read but not acted on: `control_request/request_user_dialog` (plan approval, MCP
  elicitation links, fallback-model retry dialogs) is sent only to a client that declared
  `supportedDialogKinds` in `initialize`, which Slopty does not, so it cannot leave the
  agent hanging on an unanswered dialog; `get_context_usage`, `rewind_files` and
  `rewind_conversation` need an SDK-host callback and are not answered over stdio.
- ✅ **A driven agent that dies on its own says why, protocol 27** (2026-09-12). When
  Claude Code exited without being asked — a lost login, an unknown `--resume` id, a crash —
  the pump broadcast the same `SessionClosed { reason: Exited }` as a ⌘W and the card simply
  vanished: a conversation gone with no word. Rulings: (1) `CloseReason::Failed { status,
  detail }` is a fourth reason, sent only by the driven pump and only when no `Close` was
  asked and the status is non-zero (a shell's own `exit 1` stays `Exited`; the reason is a
  *driven* failure); (2) `detail` is the agent's last non-empty stderr line, truncated the
  way every detail is, because that is where Claude Code says "Not logged in"; (3) the
  client shows it as the top-bar notice ("Claude Code exited with status 3: …") and closes
  the card as before — the transcript is on disk and ⌘⌥R resumes it — rather than keeping a
  dead card open. The `slopty` CLI's attach prints "failed". Wire: `CloseReason::Failed`
  (the enum loses `Copy`); golden `host_session_closed_failed` added, `client_hello`
  re-accepted, PROTOCOL_VERSION 26 → 27. Tests: the app self-test's `die` turn (the fake
  writes "fake: not logged in" to stderr and exits 3; the card is gone and the dump's
  `notice` reads the status and the line).
- ✅ **A command block has a menu: copy the command, copy the output, run it again, select
  it — and the rows know where the command starts, protocol 28** (2026-09-12). Warp's
  blocks are the point of shell integration, and Slopty had only ⌘⇧C for the newest block's
  output; the typed command could not be told from the prompt at all, since the marks said
  which *row* a prompt started on but not which *column* the input began at (`133;B`). Read
  from libghostty: every cell carries a `CellSemanticContent` (`Prompt`, `Input`, `Output`),
  so the engine now notes the first `Input` cell's column on each prompt row. Rulings: (1)
  `SemanticMark::Prompt { exit, input }` and `PromptContinuation { input }` carry that
  column (`None` until something is typed) — on the continuation too, because a real zsh
  prompt is three rows and the command lands on the third; (2)
  `TermState::command_block(line)` reads a block from any of its rows: the prompt's rows up
  to the one with the input column (the command from there; later `Input` rows continue a
  multi-line command) and the output rows to the next prompt, blank tail trimmed; (3) a right
  click on a block row — when the program has not asked for the mouse, ⇧ overriding as for
  selection — opens a small menu at the pointer (`block-menu`, role Menu; items only for what
  applies: "Copy command", "Copy output", "Rerun", always "Select block"); Esc, any click
  or a pick closes it, and the click is not reported to the program; (4) "Rerun" is a paste of
  the command followed by ↩ as a key — the shell sees exactly what the human would have typed
  (bracketed when it asked), so aliases, history and hooks all apply; (5) it is our own small
  menu (the tokens, the a11y roles, the tab ring) rather than gpui-kit's `ContextMenu`, whose
  element-state machinery adds nothing here. Wire: the two marks; goldens `host_lines_marks`
  / `client_hello` re-accepted, PROTOCOL_VERSION 27 → 28. Tests:
  `prompt_rows_carry_the_previous_commands_exit_status` and
  `captured_zsh_bytes_keep_output_rows_and_statuses` (`engine`: the column on the row the
  command was typed on, `None` before typing, on both the screen and history paths),
  `prompt_navigation_and_last_output_follow_the_marks` (`client`: blocks from any row, the
  open prompt with no command), headless `a_right_click_on_a_block_offers_its_command_and_output`
  (the menu in the tree, each pick's effect on the clipboard, the paste + ↩, the selection,
  Esc and a click elsewhere).
- ✅ **A block scrolled past its prompt keeps its command in a sticky header** (2026-09-12).
  Warp pins the command of the block you are reading to the top of the viewport, so a
  screenful of output is never anonymous; without it a long `cargo test` or a paged log
  reads the same as any other wall of text. Rulings: (1) it is a GPUI child over the grid,
  not a row the element paints — one row high (`CellMetrics::line_height`), full width,
  panel colour, the command in the mono face and muted text, ruled under with the block's
  separator colour (red after a failure) so the header and the rule below the output agree;
  (2) it shows only when the top row is an output row of a block with a typed command
  (`TerminalView::block_header`: `command_block(index_at_row(0))` with its prompt above and a
  non-empty command) — on any prompt row the prompt itself is visible and the header would
  duplicate it; (3) it is a Button whose click scrolls the prompt to the top (`jump_to`, the
  same path as ⌘↑), so a reader lost in output has a one-click way back to what produced it;
  (4) the conversation view (⌘⇧L) never shows it — its transcript has no rows; (5) it reads
  `TermState::block_head` (the prompt's rows alone: prompt, exit, command, where the body
  starts), not `command_block`, which joins the block's whole output into a `String` — read
  every frame under a streaming `cat`, that was a 50 000-row copy per frame for one label.
  No wire change.
  Test: `a_block_scrolled_past_its_prompt_keeps_its_command_in_a_sticky_header` reads the
  header's bounds (`debug_bounds("block-header")`, the terminal's origin and width, one line
  high), its a11y Button label, its absence once ⌘↑ puts the prompt at the top, and the
  click.
- ✅ **A long shell command that ends unwatched badges its item** (2026-09-12). Agents
  already badge their title bar when they need the human; a `cargo build` or a test run left
  in a shell the human has panned away from ended silently, and Warp/iTerm both notify on
  exactly that. Rulings: (1) it is read from the shell-integration marks the client already
  holds, no wire change: `TermState::track_command` runs every frame on the prompt's rows
  alone (`block_head`), treats a command as running once the cursor has left the rows it was
  typed on (Enter moves it to the output or the next prompt; typing, including a multi-line
  command on `Input` rows, never does) and as finished when a newer prompt start appears,
  whose `exit` is the status the shell reported (`OSC 133;D`); a command that starts and
  ends inside one frame reports nothing, which is fine — it could not have been slow; (2)
  the first prompt of a new epoch (reflow, reset, alt screen) ends nothing: the numbering
  changed, so "newer" means nothing across it (`vim` returning from the alt screen therefore
  never badges, right for an interactive program); (3) the client measures with wall time
  in the view (`command_started: Instant`), not the host: the badge is about the human's
  attention on this client, and the state machine stays pure; (4) the canvas badges only
  when the command ran at least `SLOW_COMMAND` (5 s: shorter commands end before anyone has
  looked away) and its item is not the active one; `set_slow_command` exists for tests and a
  future setting; (5) the badge is a Button in the success tone ("done 12.3 s") or the warn
  tone on a non-zero status ("failed (1) 1 min 4 s"), between the chat pill and the agent
  badge, and a press activates the item, which is also what clears it (`activate`), so the
  human's look is the acknowledgement; (6) no system notification and no `needs-you` count:
  those mean an agent is waiting on the human, and a finished command is waiting on nobody.
  Tests: `a_command_is_reported_when_it_leaves_its_prompt_and_when_the_next_prompt_starts`
  (`slopty-client`, the state machine: typed-not-entered, entered, still running, the next
  prompt with its status, a new epoch), `a_long_command_that_ends_unwatched_badges_its_item`
  (headless canvas: the active item badges nothing, the other item's badge reads "done
  0.0 s" as a Button, a press activates and clears), `a_finished_badge_says_the_status_and_the_time`,
  and the app self-test's first scenario (`sleep 6` in the first shell, ⌘N to a second: the
  badge appears from the real zsh marks through ptyd, the engine and the wire, and the click
  back on the first card clears it).
- ✅ **Slash completions say what a command does and takes, protocol 29** (2026-09-12).
  The list was bare names in chips; Claude Code has forty-odd commands and skills, and a name
  like `/compact` says nothing about its optional instructions. Read from the CLI (2.1.269):
  `system/init` carries `slash_commands` as names alone, and `system/commands_changed` (sent
  once skills are loaded and after any change) carries `{name, description, argumentHint,
  aliases?}` for the whole list. Rulings: (1) `AgentInfo.slash_commands` is
  `Vec<SlashCommand { name, description, hint }>` — the init fills names only and the next
  `commands_changed` replaces the list whole, so a card may show bare names for a moment and
  then the described ones; (2) the completion list is one line per command instead of
  wrapped chips: the name in the mono face, the hint and the description muted after it, the
  description ellipsised, and its a11y label is `slash_label` (`/compact [instructions] —
  Clear history but keep a summary`) so the dump and a screen reader read the same line;
  (3) `slash_matches` caps the list at `SLASH_MAX` = 8 — a prefix narrows it fast and a longer
  list would push the transcript off the top; (4) completing still puts `/name ` in the
  composer, the caret after the space, so a command with a hint is ready for its argument
  and one without is ready for ↩; (5) aliases are not carried — the CLI lists each alias as
  its own command already. Wire: `SlashCommand`; goldens `host_agent_info` / `client_hello`
  re-accepted, PROTOCOL_VERSION 28 → 29. Tests: `parse` of a described `commands_changed`
  (`agent`), the capped and case-insensitive `slash_matches` and the described
  `ListBoxOption`s in `a_driven_view_shows_the_agent_and_retunes_it` (headless), and the
  app self-test's driven scenario, where the fake describes its three commands after its
  init and the dump's a11y carries the described line.
- ✅ **The composer completes `@file` from the host's working directory, protocol 30**
  (2026-09-12). Claude Code expands `@path` in a prompt into the file's contents, which is
  how a human points the agent at a file; the composer took the text as typed, so the path
  had to be known and spelled out — on a phone, from memory. Rulings: (1) the word the text
  ends in, when it starts with `@`, is asked of the host as it is typed (`ListFiles {
  session, query }`, once per distinct query; a `mail@x` mid-word is not an `@` word and an
  empty query asks nothing); (2) the host answers from the driven session's working
  directory (`Driven::cwd`), off the runtime, with `slopty_agent::files::matching`: the
  `ignore` walker (hidden entries and `.gitignore` rules skipped, `require_git(false)` so a
  plain directory's ignore file counts too), depth 8 and 20 000 entries at most so a home
  directory ends, case-insensitive substring on the relative path, a path whose last
  component starts with the query first and shorter paths first among equals, at most 8
  (`FILES_LISTED`), a directory ending in `/` so the next keystrokes can descend; (3) the
  answer carries its query back and the view keeps it only while that is still the word
  the composer ends in, so a slow answer to an old query never lists under a new one; (4)
  the paths list in the same box as the slash commands (`Completion { insert, hint,
  description }` is the list's row; slash commands fill all three, paths the insert) and
  Tab replaces the `@` word alone, leaving the sentence around it, with a space after so
  the next word can follow — none after a directory, so the word goes on and the host is
  asked for what is inside it; (5) slash commands win when both would match — a `/…` text is
  never an `@` word. Wire: `ListFiles` / `Files`; goldens `client_list_files`,
  `host_files`, `client_hello` (re-accepted), PROTOCOL_VERSION 29 → 30. Tests:
  `a_name_that_starts_with_the_query_ranks_first_and_ignored_paths_are_skipped` (`agent`,
  a temp tree), the `@` half of `a_driven_view_shows_the_agent_and_retunes_it` (headless:
  the queries sent as the word grows, the stale answer dropped, the word replaced) and the
  app self-test's driven scenario (a file in the private home, `see @no` → `@notes.txt`,
  Tab → `see @notes.txt `).
- ✅ **A command palette on ⌘⇧P** (2026-09-12). Warp, Zed and every editor since Sublime have
  one, and Slopty's shortcuts had grown past what a menu bar teaches (the phone has no menu
  bar at all); a hardware keyboard on an iPad had no way to an action it did not know the
  chord for. Rulings: (1) the palette is a canvas overlay like the picker, one `Input` over a
  list of every action with its keys, the keys read from the binding tables at build time
  (`PaletteItem::new` finds the first binding whose action `partial_eq`s) so a rebinding
  never desynchronises the label — shown in Apple's menu-bar order `⌃⌥⇧⌘` and the same on
  every platform (`palette::keys_label`, not GPUI's `Display`, which spells `cmd-` outside
  macOS); (2) the filter keeps a line when every word of the text is found in its label, any
  order, any case — no fuzzy scoring, since the list is thirty lines and a word narrows it to
  one or two; (3) the choice is not run from inside the palette: it closes, the focus goes
  back to where it was when ⌘⇧P was pressed (`window.focused` remembered), and the action is
  dispatched on the next frame from that element — so `Find in terminal` finds in the
  terminal that had the keyboard, and `New note` reaches the canvas the way ⌘⇧N does; (4)
  the app's own lines (settings, hosts) are appended by the app (`extend_palette`) since
  those actions live outside `slopty-ui`; (5) ↑/↓/Esc are caught in the capture phase from
  the field's own `MoveUp`/`MoveDown`/`Escape` actions, the pattern the composer uses; (6)
  the top bar ends with a "⋯" button (a11y "Commands") that opens it, since a phone without a
  hardware keyboard has no ⌘⇧P and the bar had no room for one button per action; on a
  phone-wide bar "+ window" yields its place to it (the palette lists "Add a window or
  display"), since with both the last button sat at x = 405 on a 402-pt screen — the iOS
  self-test taps it, types into the field through the soft keyboard and runs "New note";
  (7) the canvas's sessions are lines too — "Go to <title>" with the agent's status on the
  right, ordered as the picker orders them (waiting on the human first) and ahead of the
  actions, since on a wall of terminals the thing most often wanted is one of them; a
  session line reveals and focuses the terminal instead of returning the keyboard to where
  it was (`PaletteRun::Session` beside `PaletteRun::Action`). Tests:
  `keys_read_as_glyphs_and_the_filter_takes_every_word` (unit),
  `the_command_palette_runs_an_action_by_name` (headless: the Dialog and its lines with their
  keys in the a11y tree, the field focused, Esc closing with the canvas focused again and
  nothing run, `note` + ↩ leaving one note, "Go to shell" first once a shell is on the canvas
  and revealing it with the keyboard in its terminal), and the app self-test's notes scenario,
  which now makes its note through the palette.
- ✅ **A fenced block in an answer is its own element, with a copy button** (2026-09-12). An
  agent's answer is often "run this:" followed by a command, and the only way to get it into
  a shell was to select it by hand out of gpui-kit's markdown view — on a phone, not at all.
  Rulings: (1) an assistant turn is split at its ``` fences before rendering
  (`conversation::segments`: a line starting with ``` opens a block whose language is the
  rest of the line, the next such line closes it, an unclosed one runs to the end, blank
  prose between blocks is dropped); the prose segments still go through `TextView::markdown`,
  the code segments are drawn by Slopty — the same raised surface, mono face and `small()`
  size the markdown style gave a code block, so nothing moves — with the language and a
  "copy" button (a11y "Copy code") in a header row; (2) copy takes the lines inside the
  fences alone, no trailing newline, so it pastes as one command; (3) gpui-kit's markdown
  view was not forked for a per-block button — the split is thirty lines and leaves the fork
  in sync with upstream. Not done: "run in the shell", which needs a shell to name (the
  active one? a new one?) — a ruling for when a use case asks. Tests: `segments` cases and
  the button's click reading back from the headless clipboard
  (`the_conversation_view_lists_the_transcript_and_toggles_back`), and the app self-test's
  driven scenario (`snippet`: the fake answers with a fence, the dump's a11y carries "Copy
  code").
- ✅ **A conversation is resumed from any directory on the host, protocol 22** (2026-09-12).
  ⌘⌥R listed the active terminal's directory, and the daemon's default without one: on the
  phone, where there is no terminal to stand in, that meant one directory forever, and on the
  Mac a conversation in another project could not be reached without opening a shell there
  first. Rulings: (1) `ListAgentSessions { cwd: None }` now means every directory on the host,
  not the daemon's default — the phone's natural case, and the answer names its scope
  (`AgentSessions.cwd: Option<String>`) so the picker knows whether to offer more; (2) a
  directory's list keeps its focus and ends with one "Every directory" row that re-asks with
  `None`, rather than always listing the host, because the thirty newest across every project
  bury the one you were just working in; (3) the whole-host list is bounded by the same cap
  and opens only the newest candidates (mtime sort first, prompt read second), so a home with
  a thousand transcripts costs thirty reads; a transcript whose records never name a
  directory is listed under the escaped project directory name, which is what Claude Code
  itself knows. Wire: `HostMsg::AgentSessions.cwd` optional; goldens `host_agent_sessions` /
  `client_hello` re-accepted, PROTOCOL_VERSION 21 → 22. Tests:
  `the_conversations_on_disk_are_listed_newest_first_by_their_first_prompt` (`discover`: a
  second project joins in mtime order, the fallback name, the cap spans directories),
  headless `a_past_conversation_is_resumed_from_the_picker` (the row offered for one
  directory, the re-ask with `None`, none offered for the host), the app self-test (a shell
  that has reported its directory lists it and offers the host, one that has not lists the
  host outright; either way the host's list holds the fake's conversation and offers nothing
  wider).
- ✅ **The host says when its capture target is idle; the receiver stops asking, protocol 13**
  (2026-09-05). A stream whose target has never drawn (a hidden window) left the client in "need
  refresh", re-sending `RequestRefresh` on a doubling backoff for as long as it stayed hidden —
  79 requests in 10 s on the mesh run (MEASUREMENTS, "screen stream over the mesh"). A
  client-side cap alone is the wrong answer: the client cannot tell "nothing drawn yet" from
  "the link ate my keyframe", and the two want opposite behaviour. So the host answers it —
  `ScreenEvent::Source { stream, state: Idle | Live }`, sent 400 ms after `Opened` when the
  encoder has produced no frame (`ScreenStream::check_source`, polled from hostd's geometry
  loop) and again the instant it produces one. The receiver stops asking while the source is
  idle (`Reassembler::set_source_live`) and the placeholder says "waiting for the window to
  draw…" rather than "waiting for the first frame…", which is also the honest thing to show —
  as a `Role::Status` labelled with that text, since it is the only thing a screen reader has to
  read while the surface is empty and it changes when the host reports the source. A
  cap stays as the fallback for a host too silent to send the hint — `refresh_max_repeats` 12,
  ≈17 s of asking with the backoff. Different evidence clears each: any datagram, heartbeat
  included, restarts the cap (the host is there); only a video fragment lifts the suppression
  (the source drew). Wire:
  `SourceState` + the new `ScreenEvent` variant, goldens `host_screen_source` (new) and
  `host_screen_rate` / `client_hello` re-accepted, PROTOCOL_VERSION 12 → 13. Tests:
  `an_idle_source_stops_the_refresh_requests`, `refresh_requests_give_up_after_the_cap` and
  `a_heartbeat_restarts_the_cap_and_a_frame_lifts_the_idle_hint`
  (`crates/slopty-media/tests/pipeline.rs`), `the_placeholder_says_which_end_is_waiting`.
- ✅ **Parity tracks loss asymmetrically, with a deadband** (2026-09-05). The ratio is
  `2 × smoothed loss + 5 %`, clamped to 5…50 %, and a report with `frames_lost > 0` raises it to
  1.5× the current ratio at once (parity was demonstrably not enough for a frame that then cost
  a refresh). Twice the loss is the bursty-channel rule of thumb: a frame survives only if
  *every* missing fragment is covered, so the ratio has to beat the mean by enough to absorb its
  variance. The smoothing is asymmetric — half weight on a rising sample, an eighth on a falling
  one (~0.4 s to decay at the 50 ms report cadence) — because the mesh traces show loss arriving
  in clumps between clean seconds, and a symmetric filter spends every clump under-protected and
  every gap over-protected. A change under 2 % does not move the ratio, since every change
  re-cuts the frame layout at the packetizer and a ratio that jitters by a fragment per frame
  buys nothing. Windows the receiver spent stalled are excluded from the estimate, the same rule
  the bitrate controller's `Stall` verdict follows: a link holding packets is not a link dropping
  them. Ceiling 50 % because past a half, a smaller picture beats a better-protected one. Six
  unit tests with a deterministic clumped-loss channel pin all of it; one of them found a real
  overflow (`lost × 1000` saturating `u32` made a worse window read as *less* loss).
- ✅ **The NACK give-up deadline stays where it is** (2026-09-05). With the loss hook in place,
  loopback at 0/20/50/100 ‰ loses no frame and needs no refresh (MEASUREMENTS, "parity, NACK and
  refresh under injected loss"): there is nothing to tune away, and moving a deadline no
  measurement moves would be churn. The give-up path that does fire in practice is the stalled
  one, and that is already handled by the flow-aware deadline and the stall-restart rule
  measured on the mesh path.
- ✅ **Loss injection is a router hook, not an env var flip** (2026-09-05).
  `ScreenRouter::set_loss(permille)` drops that many datagrams per thousand from a fixed-seed
  LCG before routing, so a rate always drops the same datagrams of the sequence and two builds
  compare on the same losses; `SLOPTY_E2E_DROP_PERMILLE` (read once at construction) is the
  entry point for `slopty bench screen`. Setting it per stream rather than through the
  environment is what lets one test process run three rates back to back — mutating the
  environment in a multi-threaded process is `unsafe` in Rust 2024, and the drop rate is state
  the test owns anyway.
- ✅ **A stall is the link's silence, not the source's** (2026-09-06, measured). The receiver
  counted a stall whenever nothing arrived for a stall gap, which read the capture's own quiet
  gaps as a held link: 3 / 2 / 0 / 2 stalls per 5 s on an idle machine at 0 / 20 / 50 / 100 ‰
  (MEASUREMENTS, "parity, NACK and refresh under injected loss"), enough that the gated test
  skipped its per-rate verdicts on every run. The heartbeat was supposed to prevent this — it
  says "the link is up, the source is quiet" — but a beat that is itself late leaves the same
  hole. The answer was already on the wire: every datagram carries `send_ms_lo`, the low byte of
  the host's millisecond clock, so the difference between two stamps is how long the host waited
  between sending them. `link_gap = arrival_gap − host_gap` is the link's share, and only that is
  compared with the stall threshold and charged to `stalled_ms` — RFC 3550's interarrival
  arithmetic, used for attribution rather than jitter. Three cases keep the old pessimistic
  reading, because they are cases where the receiver genuinely cannot tell: a gap past the
  stamp's 256 ms range, a retransmission (it carries the original frame's stamp), and no previous
  stamp at all. `Reassembler::stalled` also returns false while the host reports the source idle,
  which is the in-progress half — a gap that has not ended yet has no stamp to settle it. Result:
  0 stalls at every rate, and the gated test asserts its verdicts again. Tests:
  `a_quiet_source_whose_heartbeat_was_late_is_not_a_stall` and
  `only_the_hosts_share_of_a_gap_is_forgiven` (`crates/slopty-media/tests/pipeline.rs`), with the
  existing stall tests rewritten to produce their frames *before* the silence they are released
  after, which is what a held link actually looks like.
- ❌ **Dropping parity on small frames** (2026-09-06, measured and reverted). The packetizer
  rounds any non-zero parity ratio up to one whole fragment, so a still window — where frames are
  two to five fragments — puts 240–440 ‰ of its data fragment count on the wire as parity the
  controller never asked for. Rounding to the ratio instead (nearest, then a quarter, with a
  floor kept for IDR and LTR-refresh frames) did what it promised: parity seen fell to 47–80 ‰
  against a one-per-frame counterfactual of 285–309 ‰ for the same frames. It also lost frames.
  At 20 ‰ datagram loss the run lost 2 and took 2 refreshes where the minimum loses none, and at
  100 ‰ it lost up to 9: a two- or three-fragment frame with no parity has nothing between a
  single drop and a NACK round trip, and when the retransmission is dropped too the frame is
  gone. The minimum stays, because the trade it makes is the right way round — it is a large
  *percentage* of a stream that is already small (a still window is ~0.5 Mbit/s, so 30 % of it is
  ~150 kbit/s) and costs nothing on the busy screens where bytes are worth something, since those
  frames run to tens of fragments. Group parity across consecutive small frames was not built:
  it needs a wire change and it can only repair once the whole group has arrived, which on a
  still window is hundreds of milliseconds — all of that to save a fraction of 150 kbit/s.
- ✅ **The gated loss test asserts, and says when it cannot** (2026-09-06). Its verdicts used to
  be skipped whenever any row stalled, which before the stall fix meant nearly every run. The
  skip still keys on the stall count, but the count now means something: host and client share
  the test's process, so nothing except the scheduler can hold a loopback datagram for a stall
  gap, and an idle machine reports none. Two guards that look reasonable and are not: the frame
  *count* (a still desktop draws 8 fps because nothing is changing) and the gaps between
  *decoded* frames — a recovery bug that starves decode while datagrams keep arriving would then
  skip every verdict instead of failing one, which is exactly the failure the test exists to
  catch. The evidence has to be at the datagram level, which is where the stall counter is. The
  loss verdict is a rate (under one frame in a hundred), not zero: a two-fragment frame plus one
  parity is beyond repair when two of its three datagrams go, and at 50 ‰ that is about one
  frame in three hundred whatever the policy does.
- ✅ **The refresh guard is checked against a window the test owns** (2026-09-06). The 2026-09-05
  ruling ("an idle source stops the refresh requests") was pinned by media unit tests and by one
  hand-run against a hidden Ghostty window; nothing checked the whole path. It does now, and the
  awkward part was the target: the guard needs a capture source that produces *no* frame at all,
  the app's picker lists on-screen windows only, and no window this repo does not own may be
  touched. So the harness owns one — `slopty-idle-window` (`crates/slopty-e2e/src/bin`), a
  240×160 AppKit window with no content that orders itself in and out on marker files. On screen
  it is listed and pickable; ordered out it is still in the host's window list (the host
  enumerates with `onScreenWindowsOnly: false`) and ScreenCaptureKit has nothing to deliver for
  it, which is exactly the state the guard is about. Two tests use it:
  `a_window_that_never_draws_is_reported_idle_and_stops_the_asking`
  (`apps/slopty-hostd/tests/e2e.rs`, gate `SLOPTY_SCREEN_E2E`) for the host and the receiver, and
  `a_remote_window_that_never_draws_waits_instead_of_asking_forever`
  (`crates/slopty-e2e/tests/app.rs`) for the app: ⌘O, pick the row, hide, and then the item's
  `Role::Status` reads "waiting for the window to draw…" while the host counts what it was
  actually asked for. **4 refreshes against the cap of 12**, then silence, and the picture
  returns on its own when the window draws again (MEASUREMENTS.md, "the refresh guard end to
  end"). The host now counts them: `ScreenStats::refreshes`, over the control socket as
  `slopty host screens` — a client-side counter would only say what the client believes it sent.
  Known limit the tests make visible rather than fix: `check_source` latches on `encoded > 0`, so
  a window that draws once and *then* hides stays `Live` forever and only the cap protects the
  receiver (superseded 2026-09-06: `SourceTracker` follows recent frames, resetting to `Idle` after
  2 s quiet or immediately on off-screen; see "The source state follows the frames, not the first
  one" below). The app case gates on `SLOPTY_SCREEN_E2E` as well as `SLOPTY_APP_E2E`: it is the only
  case in that suite that asks the machine for anything. (This entry first also claimed that a
  hidden window on the display-crop path keeps streaming the desktop behind it. It does not; see
  the crop-path ruling below.)
- ✅ **What a cropped stream must never show** (2026-09-06). The claim in the entry above — that a
  window hidden while on the display-crop path keeps streaming the patch of desktop behind it —
  **was wrong**, and the correction is worth more than the alarm was: it came from a run whose
  helper binary was stale, so the window under test never actually hid (that is also why
  `bin_of` now rebuilds every run). Measured properly, hiding a window on the crop path takes the
  stream off the crop in **0.4–1.4 s** and it encodes nothing at all while the window is away; it
  does not go back onto the crop, and the picture returns when the window does.
  What *is* true is that the swap is bookkeeping with a gap in it: the geometry tick decides, then
  two ScreenCaptureKit callbacks settle, and between the decision and the settle the crop is still
  live. So the rule is now enforced on the frame path rather than by the transition: `Shared`
  carries `target_hidden`, set from `window_on_screen` at the **top** of every geometry tick —
  before `Transitions::settle`, which returns early while a swap is in flight and would otherwise
  leave the guard stale exactly when it is needed — and `on_frame` drops anything captured while
  it is set, counting it as `ScreenStats::withheld`. `ScreenStats` also gained `on_crop` (which
  path the stream is on right now, as against `cropped`, which counts frames that came that way),
  because a test cannot otherwise tell "left the crop" from "the desktop happens to be still",
  and it is filled in one place (`Shared::stats`) so no reader can publish the counters' `false`
  placeholder and report the window filter for a stream on the crop.
  Not changed: `window_on_screen` still reads `kCGWindowIsOnscreen` — see the entry below for the
  measurement that finally settles it.
  Tests: `a_frame_captured_while_the_target_is_hidden_is_withheld` (unit, `slopty-host`) is the
  statement — a frame handed to `on_frame` while the guard is set is counted as withheld and goes
  no further, and it fails the moment the guard is removed. `a_hidden_window_stops_being_served_
  from_its_crop` (`apps/slopty-hostd/tests/e2e.rs`, gate `SLOPTY_SCREEN_E2E`) drives visible →
  crop path → hide → show for real, with a **second window repainting directly behind the
  target**: without that backdrop the framework has no new frame for the rectangle once the
  window goes, so every assertion about what is not sent passes for free. The client's own frame
  count is *not* the evidence either: frames sent legitimately while the window was still up are
  still being decoded seconds later, and asserting on them fails for the wrong reason.
- ✅ **A hide is ~270 ms late, and the crop keeps that latency anyway** (2026-09-06).
  With something repainting behind the target, the same test measures **12–15 crop frames sent
  after AppKit ordered the window out** — not the zero the entry above reports, which was measured
  against an empty rectangle. The cause is not this code: every way of asking CoreGraphics whether
  a window is on screen keeps saying yes for **267–279 ms** after `orderOut` returns, measured by
  a 2 ms probe as well as by the 100 ms geometry tick, and the window's own `kCGWindowIsOnscreen`
  and membership of the on-screen list flip in the same millisecond as each other. So no polling
  predicate can meet "within one geometry tick", and the guard above does not cover this gap
  either — by the time the host knows, the framework has stopped on its own.
  What bounds it today is the crop filter itself: it is
  `initWithDisplay:includingApplications:exceptingWindows:` restricted to the target's **owning
  application**, so in principle only that application's other windows can appear. The test does
  not confirm that — its backdrop is a bare executable with no bundle identifier, and copying it
  to a second path did not make ScreenCaptureKit treat it as a second application — so whether a
  genuinely different app can appear in the crop is unresolved.
  **The display-crop path stays on by default.** This is one ruled measurement against another,
  not a mistake to fix: `CROP_WINDOWS` buys ~6 ms of capture→decoded latency (2026-09-05,
  "capture floor", 9.0 → 1.7/2.6 ms p50), which is the point of the product on that axis, and
  turning it off would spend that everywhere to close a window that is ~270 ms long, bounded by a
  CoreGraphics flip no amount of polling beats, and scoped by the filter's construction to the
  target's own application. Against it stand the guard, which closes the part this side owns, and
  the e2e's ceiling of 30 frames, which makes a regression in detection visible. Recorded so the
  trade is re-openable rather than rediscovered.
  Both of the questions this entry left open are answered in the two entries below.
- ✅ **A crop cannot be made to show another application** (2026-09-06). The open question from
  the entry above, answered with a fixture that is a real second application: the idle-window
  helper inside a minimal `.app` with its own `CFBundleIdentifier`, ad-hoc signed, because a copy
  of a bare executable at another path is still the same application to ScreenCaptureKit.
  It also replaces the instrument. Frame *counts* cannot answer this — the framework delivers the
  crop rectangle at the frame rate whether anything in it changed or not, so an empty rectangle
  produces as many frames as a busy one, which is what the control run shows. The pictures are
  read instead, as one number: the mean brightness of the client's decoded frames. With the
  target up and flashing in the crop that measure swings by 136; once the target is ordered out
  it goes to **zero in all three cases** — nothing behind, a window of the same executable, and a
  window of another application. The crop carries black, and none of the window behind reaches
  the client (MEASUREMENTS.md, "what the crop shows"). So the ~270 ms gap costs the client a
  black rectangle, not somebody's content, and the default in the entry above stands on firmer
  ground than when it was taken.
  Tests: `what_a_crop_shows_of_another_application`, `what_a_crop_shows_of_an_empty_rectangle`
  and `a_hidden_window_stops_being_served_from_its_crop` share one driver and differ only in what
  is behind the target; the empty rectangle is kept as the control that makes the other two mean
  anything.
- ✅ **The accessibility API knows about a hide 260 ms before core graphics does, and the
  stream holds frames on its word** (2026-09-06, measured; wired the same day). Polled every
  2 ms from one thread against the same order-out: accessibility answers in **0 ms**,
  `kCGWindowIsOnscreen` and the on-screen list in **256–266 ms**. It is now the first guard on
  the crop path: `slopty_capture::HideWatch` puts an `AXObserver` on the window's application
  (its own `CFRunLoop` thread; `AXWindowMiniaturized` and `AXApplicationHidden` on the
  application element, `AXUIElementDestroyed` on every window element — that is what AppKit
  posts for a window it orders out — and `AXWindowCreated` to put new windows under the same
  watch). Its callback opens a `SUSPICION_HOLD` of 400 ms on the stream, in which `on_frame`
  holds every frame (`ScreenStats::suspected`, apart from `withheld` so a hide shows where it
  was caught); the geometry tick's `target_hidden` is the confirmation that outlives it. With it
  on, the three crop-hide tests send **0 crop frames after the order** and **0–2 after the
  hide was asked** (13–28 before), and 0–1 pictures of the gap reach the client
  (MEASUREMENTS.md, "the accessibility hide watch"). Two things were worth writing down rather
  than rediscovering:
  * **It cannot name the window, but it can match it once.** The public accessibility API
    exposes no window number for an `AXUIElement`; the call that does, `_AXUIElementGetWindow`,
    is private and out under the no-private-API rule. So accessibility can say "a window of that
    application went" but not "*the* window went" — except that at registration the watch reads
    every window's `AXPosition`, `AXSize` and `AXTitle` and keeps the element whose frame (within
    1 pt) and title are the target's from the window list (`kCGWindowBounds`, `kCGWindowName`),
    and from then on tells that element from every other by identity (`CFEqual`), since an
    element's identity is stable however the window moves. Its going, its minimising and the
    application being hidden are the *suspicion*: hold frames at once, let core graphics confirm
    or deny inside the hold. Any other window of the application going is `Went::Other`
    (`ScreenStats::siblings`): no hold, only the wake the next ruling describes. Amended
    2026-09-06 after `a_popup_of_the_application_closing_raises_no_suspicion` showed the
    unmatched watch treating a borderless pop-up panel's going — an autocomplete list, a
    tooltip — as the target's, a 400 ms freeze on every one; measured after the change, a
    pop-up or sibling closing is no suspicion and the stream flows straight through
    (MEASUREMENTS.md, "pop-ups and siblings against the targeted watch"). When nothing matches —
    the application does not list the window, or it moved between the two reads — every window
    of the application counts as the target, as before (`HideWatch::targeted` says which; the
    watch logs it). Focus and main-window changes are *not* suspicions: they fire on every
    switch between an application's windows and would freeze a multi-window target constantly.
    The hold is 400 ms because the confirmation is the 256–266 ms lag plus one geometry tick
    (≈100 ms).
  * **The constants are string literals.** Ruled in "Constants the SDK defines as `CFSTR`
    macros" below; the spelling lives once, in `crates/slopty-capture/src/ax.rs`.
  * **Without accessibility trust there is no watch** (`AxError::NotTrusted`, logged once per
    stream) and the crop carries black for the ~260 ms, as measured before: the tests assert the
    hold when `suspicions > 0` and fall back to "dark and still" otherwise, so they say something
    on either kind of host.
  Also considered and not measured: `SCShareableContent` and the stream delegate are the same
  window-server data core graphics reads, so they cannot be earlier than it; window-server
  notifications are private API and out for that reason; and deciding a hide from the frames
  themselves (the crop going black) is a guess about content that a dark window would trip.
- ✅ **A suspicion moves the stream to the window filter, because ScreenCaptureKit stalls an
  application-scoped display filter when a window of that application is ordered out**
  (2026-09-06, found by the false-suspicion test). With the hold alone, a sibling window of the
  target's process closing left the stream dead: no sample buffer ever again, no error, no
  status change (MEASUREMENTS.md, "a sibling window closing stalls the crop"). Re-applying the
  same filter, from the cached content or a fresh `SCShareableContent`, does not wake it; a
  change of filter kind does. So `follow_window` treats a live suspicion like a covered window
  — the crop is not allowed, the stream goes to the window filter on the next tick — and comes
  back to the crop on the first tick after the hold; a crop update asked in the same tick as
  the retarget is rejected once with `-3812` and lands on the retry, like any other path change.
  Frames stay held for the whole hold on either path: the variant that let the window filter's
  frames through encoded 1–2 pictures of the backdrop after the swap had settled, because the
  framework still delivers a frame or two of the old filter after the completion handler.
  Measured cost of a false suspicion, while every window of the application still counted as
  the target: 520 ms without frames and 16 held, nothing withheld, the crop back by itself.
  Side effect on true hides: off the crop path 158–206 ms after the order (373–473 before),
  since the swap is asked on the suspicion rather than the confirmation. The `capture frame
  status` debug line (one per change of `SCFrameStatus`) stays in `slopty_capture::stream`: it
  is how a silent stall is told from an idle source. Amended the same day, once the watch
  matched the target (the ruling above): another window going is no longer a suspicion but
  still the event the framework stalls on, so it sets `filter_stalled` and the next geometry
  tick takes a stream on the crop through the window filter and back with no hold — frames flow
  throughout, ten more of them 207–367 ms after the order-out, none held or withheld
  (MEASUREMENTS.md, "pop-ups and siblings against the targeted watch"). The 520 ms freeze is now
  only the unmatched watch's price.
- ✅ **Constants the SDK defines as `CFSTR` macros are spelled once, next to their use**
  (2026-09-06). "Apple framework keys and constants come from the objc2 statics, never string
  literals" is written for constants that have a symbol: the static is the guarantee that the
  spelling is the SDK's. `kAXWindowsAttribute`, `kAXUIElementDestroyedNotification` and the rest
  of `HIServices/AX*Constants.h` are `#define … CFSTR("…")` — no symbol is exported, objc2
  generates nothing for them (`AXNotificationConstants.rs` holds only `AXPriority`), and there is
  no static to come from. The carve-out: such a constant is a `const &str` in the one module that
  uses it, with a comment naming the macro and the SDK header, and nothing else in the workspace
  spells it. A binding that later exports the symbol replaces the `const`. Rejected: a
  `slopty-apple-constants` crate (a second place for a thing the SDK itself keeps in one header),
  and a runtime lookup of the header (no).
- ✅ **The source state follows the frames, not the first one** (2026-09-06). `check_source`
  decided `Live` from `encoded > 0`, a latch: a window that drew once and was then hidden, or
  closed and left up, stayed `Live` for the rest of the stream, and the receiver — which stops
  asking for refreshes only while the source is idle — had nothing but its cap of 12 to protect
  it. It now follows recent history, in a `SourceTracker` that takes the clock as an argument so
  the rule is unit-tested rather than slept through:
  * a stream that has produced nothing says nothing for `SOURCE_IDLE_AFTER` (400 ms), then `Idle`
    — unchanged, and still the answer to "is it just starting up";
  * any new frame is `Live` at once;
  * `SOURCE_QUIET_AFTER` (2 s) without one is `Idle` again. Deliberately longer than the 400 ms:
    a target that draws once a second would otherwise flap and spend a control message on each
    change, and the receiver only needs to know before it starts asking;
  * a target the geometry tick reports off screen is `Idle` immediately, with no quiet period —
    a window that is not on screen cannot be drawing, whatever the counter says, and that is the
    same evidence the crop guard acts on.
  Tests: five in `crates/slopty-host/src/screen.rs` (never drew, first frame, drew-then-stopped,
  a slow target that must not flap, hidden), and the hostd e2e now drives hide → show → hide and
  asserts the host reports `Idle` the second time — which under the latch it never did. No wire
  change: the states are the ones `SourceState` already had.
- ❌ **The pacer is not clumping frames under load** (2026-09-06), retiring the leftover the flap
  track raised. The claim was that 19–38 datagram stalls per 90 s under every load shape, against
  "none on an idle machine", meant frames were being released in bursts. Both halves were wrong.
  The comparison was a 90 s window against the loss test's 5 s ones, and the flap harness with
  **no load at all** produces 31 stalls in 90 s — as many as the loaded shapes, two of which
  produce fewer (3 and 4). The harness now records both ends of the run, and the host is clean
  under all five shapes: the datagram queue never fills, next to nothing is dropped, and `hold`
  p50/p95 are 0 ms, so frames arrive whole rather than dribbling. Running the same all-core burn
  in other processes instead of in the receiver's own (`Shape::CpuExternal`, `/usr/bin/yes` per
  core) changes nothing either, which rules out the harness starving itself.
  So nothing changed: no thread QoS for the capture path, no pacer burst cap, no send spreading.
  What the numbers do leave is a different question — a quiet loopback stream charges the link
  about a third of a stall a second whatever the machine is doing — and that is about the stall
  detector's pessimism rules (a gap it cannot attribute is charged to the link on purpose,
  2026-09-05), not about pacing. `Shape::None` is kept as the baseline row precisely so the next
  reading of these numbers starts from it.

- ✅ **A stall means the link held datagrams while the receiver was awake to notice** (2026-09-06,
  measured). "A stall is the link's silence, not the source's" (above) put the send stamps to
  work, and a quiet loopback stream still reported stalls the host could not have caused: it
  held no frame, QUIC lost no packet, and every charged silence had **no frame pending** — there
  was nothing for the link to hold (MEASUREMENTS, "a quiet loopback stream's stalls"). Taking
  all 13 silences past the gap apart over 270 s named two readings, both the receiver's:
  * The stamp difference was *discarded* whenever it read longer than the arrival gap it
    explained. It reads longer routinely: `send_ms_lo` is written when the host builds a
    datagram, not when QUIC sends it, and the two datagrams either side of a silence wait
    different amounts in the send queue — 7.3 and 7.6 ms in the runs. Discarding charged the
    whole silence to the link. The difference is congruent to the host's real interval modulo
    the byte's 256 ms range, so it is now read as a **signed offset from the arrival gap**: below
    the midpoint the host covers the silence, above it the datagram overtook its predecessor and
    is refused exactly as before (`STAMP_SLACK`, `Stamp::{Host, Covered, Backwards}`). Past
    256 ms the ambiguity is real and the pessimistic reading stands (`Stamp::Wrapped`).
  * **A receiver that was not scheduled charged its own load to the link.** Nothing distinguishes
    a held link from an unread socket from inside a task that never ran, and the same runs had
    the client's stream worker 64–80 datagrams behind by a stall gap or more, worst 149 ms.
    `Config::tick_period` is now the cadence the owner promises, `Reassembler::tick` records when
    it kept the promise, and the stretch of a silence beyond it is subtracted before anything is
    charged (`dozed`). `slopty-client`'s `IDLE_TICK` moved 50 ms → 25 ms so the receiver looks
    twice per stall gap, for the same reason the host beats twice per gap: at 50 ms a gap slept
    through entirely still left one whole stall gap charged.
  What a stall no longer means: "nothing arrived for 50 ms". What it means now: "for a stall gap,
  the host's own stamps say it was sending and this receiver was awake and saw nothing". Deadlines
  still restart on any silence nothing could have crossed, receiver-side included, so a NACK
  written before one still gets its round trip. Result over five 90 s samples: **not one stall
  charged for want of a reading** — `stamp_wrapped`, `stamp_backwards`, `stamp_absent` and
  `receiver_dozed` are zero in every sample, where before two of four stalls were the discarded
  stamp and the other two the descheduled receiver. Two samples report 0 stalls outright (against
  2 / 0 / 2 before) and the rest report holds the counters stand behind: `stamp=Host(0ns)` with
  **a fragment of the frame still pending**, on the host's send side (`cwnd 5808`, QUIC's floor,
  against a 30 Mbit/s target on a 2.9 ms path). Freezing growth on those is right. The self-check
  is `slopty bench screen --max-stalls 0`. Tests (`crates/slopty-media/tests/pipeline.rs`):
  `a_stamp_reading_just_past_the_silence_still_belongs_to_the_host`,
  `heartbeats_late_by_three_beats_are_charged_to_the_sender`,
  `two_hundred_milliseconds_in_flight_is_one_stall` (which must still be one stall of 200 ms, and
  still freeze the bitrate controller) and `a_silence_the_receiver_slept_through_is_not_a_stall`;
  the harness now separates `awake` (time passes, the receiver ticks) from `advance` (it does
  not), which is the distinction the rule turns on. Not done: widening `send_ms_lo` past its
  256 ms range — no run has produced a `Stamp::Wrapped` silence, so the protocol bump would be
  paying for a case nothing has shown yet.
  Amended 2026-09-06: the sentence above about the silences the stamps charged to the host was
  right, and the entry below is what was behind them — the host really was quiet, for up to
  242 ms, because its beat was sharing a task with a blocking call.
- ✅ **The heartbeat has its own task** (2026-09-06, measured). The beat is a promise about time:
  one every `HEARTBEAT_AFTER` (25 ms), so that a receiver counting a silence of `STALL_GAP`
  (50 ms) never has to. It was being sent from `cursor_loop`, which every 100 ms also asks the
  window server where the target is — a round trip this project has measured at up to 90 ms, and
  which the accessibility work showed is not cheap in general. A beat behind that call is late by
  several times its own period, which is exactly the failure the beat exists to prevent.
  Split in two: `beat_loop` touches nothing but atomics and the datagram queue, and `cursor_loop`
  makes its geometry and pointer calls through `spawn_blocking` so it does not hold a runtime
  worker either — a blocked worker delays every timer on it, which is the mechanism, not the
  syscall itself.
  Measured over a quiet minute, in `ScreenStats`: the median gap is 31 ms either way (the loop
  checks a 25 ms promise every 8.3 ms, so beats land on tick boundaries) and p95 is 35 ms. What
  changed is the tail — **242 ms worst before, 45–75 ms after over four runs**, with the late beats that used to
  appear mid-stream gone. The receiver counts 0 stalls and 0 ms stalled.
  The counter that found it is worth keeping in mind: the quantiles slide over the last 600 beats,
  about twenty seconds, so a single 242 ms gap in a sixty-second stream is *not in them* — p95
  read 35 ms while the stream had a quarter-second hole in it. `beat_gap_worst_us` is an all-time
  maximum for that reason, and a rule about a promise like this has to be written on the worst
  case, not on a quantile.
  Not fixed here: what remains is about 75 ms once a minute, and around a path swap about 108 ms.
  Neither is this loop — its own geometry call reads p95 5 ms, max 12 ms in the same runs — so
  the remaining blocking is elsewhere on the runtime, hostd's own `check_geometry` being the
  candidate, and moving that off a worker means turning a synchronous ScreenCaptureKit path
  async, which is not a change to make while chasing a tail. The e2e asserts the p95 against
  `STALL_GAP` and 0 stalls, and deliberately does not assert the worst gap.
  Tests: `a_slow_geometry_call_does_not_make_the_beat_late` (unit: 300 ms of blocking work beside
  the beat, cadence kept) and `the_heartbeat_keeps_its_cadence_on_a_quiet_stream` (hostd e2e,
  `SLOPTY_SCREEN_E2E`, a minute on a stream whose target draws nothing).

- ❌ **Raising BBR3's minimum congestion window, or softening `ProbeRTT`** (2026-09-06). The long
  holds on a quiet loopback stream are `ProbeRTT`: every 5 s without a lower RTT sample BBR3
  clamps the window to `max(0.5 × BDP, MinPipeCwnd)` for 200 ms, and on a 2 ms path the BDP is
  ~11 kB so the floor is what applies — 24 of the 32 holds past 25 ms in 7.5 minutes of samples
  had the window at exactly 5 808 B = `4 × smss` (MEASUREMENTS.md, "what actually holds a frame").
  Not reachable from here: `noq_proto::congestion::Bbr3Config` carries the fields
  (`probe_rtt_cwnd_gain`, `default_cwnd_gain`, …) as private members and `impl Bbr3Config` exposes
  **one setter, `initial_window`** (`bbr3/mod.rs:2015`); `min_pipe_cwnd` is `4 * self.smss`
  outright (`bbr3/mod.rs:1841`), asserted by a noq test. noq-proto 1.2.0 is the only version
  published, so there is no bump either. The ask upstream is either a setter for
  `probe_rtt_cwnd_gain` or skipping `ProbeRTT` for a flow that is application-limited — such a
  flow never fills the pipe, so its minimum RTT is already an unqueued one and the 200 ms buys
  nothing. Until then the cost is measured and named rather than worked around.
- ❌ **Switching the default congestion controller to Cubic** (2026-09-06), ✅ **`SLOPTY_CC=cubic`
  as the measured short-path escape hatch.** Cubic has no `ProbeRTT` and it shows exactly where it
  should: over 3 × 90 s its window never fell below 38 960 B against BBR3's 5 808, holds past
  25 ms fell from 4.3 per minute to 0.9 and the worst from 668 ms to 48 ms. That is not enough to
  move the default. The BBR3 ruling above (line 634) rests on the Wi-Fi/mesh path, where Cubic cut
  the window to 13–20 KB after a handful of real losses and capped the stream near 10 Mbit/s for
  the rest of the session; a 200 ms hitch every few seconds is worse than nothing but it is not
  worse than that, and no measurement of Cubic over the mesh exists to weigh against it. Two
  controllers, two paths, one knob: the numbers for both are on the MEASUREMENTS page and
  `SLOPTY_CC` selects either. Revisit when the mesh comparison exists, or when noq exposes the
  `ProbeRTT` knobs. **Settled 2026-09-06 by the mesh comparison below: the default stays BBR3,
  and the loopback numbers in this entry are true but were taken in a regime where no window
  binds.**
- ❌ **The client's `pacing.rs` is not the ACK path** (2026-09-06, checked while looking for a
  receiver-side limiter). It is the display pacer — present-on-arrival, replace rather than queue,
  and the percentiles that go with it. What governs ACK cadence is `slopty_net::endpoint`'s
  `MAX_ACK_DELAY`, already at 2 ms since 2026-09-05, and the eight 90 s samples here confirm it is
  not limiting: 0 NACKs, 0 lost packets, 0 congestion events in every run.
- ✅ **A congestion window is read as a series, not as a final sample** (2026-09-06,
  `slopty_net::endpoint::trace_path_health`, `SLOPTY_PATH_TRACE_MS` in milliseconds, off by
  default). One reading at the end of a run cannot tell a window that sat on BBR's four-packet
  floor the whole time from one that dipped there for 200 ms — and a 15 s sample missed the dips
  entirely. Paired with a **backlog** line in hostd's datagram pump, the host-side twin of
  `receiver_dozed`: a datagram waits either in the pump's channel, because the task did not run,
  or in QUIC's send buffer, because the window holds it, and a receiver sees the same silence for
  both. The pump is never late by more than 12 ms in any sample, which is what makes the
  window the answer. That measurement was wrong when first taken — the timer started when `recv`
  handed over a datagram, so it timed the drain and not the sleep before it, and a pump
  descheduled for 200 ms could have reported 1 ms. It now sums the turns owed to datagrams
  already queued and keeps the worst turn against the 1 ms deadline the loop asks for while work
  is outstanding; re-measured, no turn past 25 ms in 3 × 90 s against QUIC holds of 96–100 ms in
  the same logs. Unit test: `a_pump_descheduled_before_it_drains_reports_the_sleep_not_the_drain`.
  Amended 2026-09-06: the turn accounting still could not see a datagram that arrived in an
  empty queue and then waited out a deschedule alone — `queued_before` was zero, so no turn was
  owed. Every datagram now carries its enqueue time (`slopty_host::screen::Queued`, stamped in
  `Shared::push` with `host_now_us()`) and the pump reads the wait on the way out: the caught-up
  line reports `waited` p50/p95/max over the last 1024 datagrams and the worst since the last
  line, and a lone wait past 20 ms with no backlog to charge it to is logged on its own, at most
  once a second. Host-internal; the stamp never goes on the wire. Unit test:
  `the_wait_ring_reports_the_last_window_and_the_worst_once`; numbers in MEASUREMENTS.md
  "what a datagram waits in the pump's channel". The bench's `quic path (client side)` line — the feedback connection, permanently at
  `min_pipe_cwnd` because nothing measurable flows on it — is relabelled
  `quic path (client→host, feedback only)`; reading it as the media window is the mistake this
  entry exists to stop.
- ✅ **With no audio, the heartbeat is what keeps the cadence — and that is all a keep-cadence
  datagram can do** (2026-09-06). ❌ **Beating at the inter-frame gap instead of
  `HEARTBEAT_AFTER`.** The question was whether a stream carrying no audio loses its datagram
  cadence and lets the congestion window fall to the floor. Measured both ways on the same host
  (MEASUREMENTS.md, "Audio on"): with sound playing, `host_quiet` silences go to 0 — the sender is
  never quiet — and the window is at 5 808 B for **23 % of samples against 1.4–8 % on the quiet
  runs**, with 27 of its 28 long holds there and 14 stalls. Traffic does not lift the window; the
  window is low because the flow is application-limited and BBR sizes it from `bw × min_rtt`, which
  on a still desktop (~1 Mbit/s over 1.5 ms) is under a packet. More datagrams are more bursts into
  a four-packet window. The heartbeat's job is the receiver's stall clock, not the congestion
  controller's estimate, and at half the stall gap it already does that job: `host_quiet` silences
  run 0–4 per 90 s with `stamp_absent` zero throughout. Raising its rate would buy nothing and cost
  a datagram every 16.7 ms per stream. The only filler that would move the estimate is filler at
  the target rate, which is 30 Mbit/s of nothing on a link carrying 1 Mbit/s of content.
- ✅ **A stall still on when the run ends counts against `--max-stalls`** (2026-09-06,
  `apps/slopty-cli/src/bench.rs`, from Codex's review of the stalls track). The check read
  `stats.stalls`, which only counts stalls that *released* — so the worst case there is, a link
  that stops and stays stopped, passed with zero. A run reporting `stalls 0 (66 ms stalled)` did
  pass (MEASUREMENTS.md, 2026-09-06). It now counts an unreleased stall and requires
  `stalled_ms == 0` when zero stalls are allowed.
- ✅ **The receiver's doze credit survives the tick that observes it** (2026-09-06,
  `crates/slopty-media/src/reassemble.rs`, same review). A receiver waking from a long sleep runs
  whatever its executor polls first, and a ready timer is as likely as the socket. `tick` moved
  `last_tick_at` to now, so an ingest a moment later found nothing slept through and charged the
  whole gap to the link — undoing the stalls-track fix in exactly the interleaving that fix was
  for. `tick` now banks the slept stretch in `dozed_since_arrival` and the arrival that ends the
  silence spends it. The same edit closed a second hole the first did not name: `last_tick_at` was
  only moved by `tick`, so a receiver that was ingesting steadily but not ticking accumulated a
  doze it never took. An arrival is proof the loop ran, so it moves the mark too. Test:
  `a_tick_that_wakes_first_does_not_hand_the_silence_to_the_link` — the timer fires before the
  datagrams and the 200 ms is still not a stall, and the next silence, watched, still is one.
- ✅ **A sleep is forgiven once, and the two shares of a silence are not added** (2026-09-06,
  `crates/slopty-media/src/reassemble.rs`, from Codex's review of the cadence track). Two ways the
  doze credit was wrong. **It was spent again in every report**: the bank belongs to the whole
  silence but a charge only covers the stretch since the last one, so a receiver that slept 200 ms
  and then sat awake in a stall reported the silence once and zero thereafter — and a rate
  controller reads a window with no stalled time as healthy and grows into a link that is still
  holding everything. `charge_stall` now spends what it forgives. **And the host's share and the
  receiver's were added**, though they are not disjoint: the host's silence runs from the last
  arrival, and the receiver may have slept through exactly that stretch. A host quiet for 100 ms,
  then a link holding the next datagram for 100 ms with 75 ms of sleep inside the host's half,
  read as 25 ms of link time instead of 100 and the stall was missed. Only sleep past the end of
  the host's silence is credited now, which needs the doze's start as well as its length. Tests:
  `a_sleep_is_forgiven_once_and_a_stall_that_stays_on_keeps_reporting`,
  `sleep_inside_the_hosts_own_silence_is_not_forgiven_twice`; both were checked against a mutation
  of the fix they cover.

- ✅ **BBR3 stays the default congestion controller; the choice is decided by the busy case, and
  a still desktop cannot decide it at all** (2026-09-06, sixteen 90 s runs over the Wi-Fi/mesh
  path, MEASUREMENTS.md "BBR3 vs Cubic over the mesh"). This is the comparison the entry above
  said it was waiting for, and it splits by regime rather than by controller.

  On a **still desktop** the two are indistinguishable on everything the receiver sees — stalls
  165.5 against 166, stalled 18.7 s against 19.0 s, the same arrival-gap-max distribution (each
  has two of six runs past 1.4 s) — even though Cubic's window runs 3× larger at p50 and BBR3
  sits on its four-packet floor for 35.8 % of samples against Cubic's 0.1 %, and even though
  Cubic absorbed roughly twice the packet loss. Nothing binds: a still desktop offers ~1 Mbit/s
  against a 30 Mbit/s ask, so the window the controller picks is a window nothing needs. **The
  loopback ruling above was measured entirely in this regime.** Its numbers stand and its
  mechanism (`ProbeRTT`) is real; what it could not see is that the quantity it optimised does
  not reach the user.

  Put a scrolling terminal on the captured display and they separate at once, in the direction
  the 2026-09-05 ruling claimed and on the path it claimed it for: **Cubic's window collapses to
  13–15 kB under real loss and its target falls to 1.6–7.9 Mbit/s, while BBR3 holds 40–43 kB and
  one run reached the full 30 Mbit/s**, moving more datagrams in both interleaved pairs. That is
  picture quality, not smoothness — delivered fps is 54–56 either way because ScreenCaptureKit
  caps at 60 — and it is the whole reason a 30 Mbit/s target exists. `SLOPTY_CC=cubic` remains
  the escape hatch for a short path with no loss, where the `ProbeRTT` hitch is the only thing
  happening.

  **The strength of this evidence, stated plainly, because a later sweep partly cuts against it.**
  Counting every busy-source pair, BBR3 delivers more datagrams and a higher end target in three
  of four interleaved pairs. The exception is the 20 Mbit/s sweep run, taken after the link had
  degraded to ~600 lost packets per 90 s: there **BBR3** cut to the 1 Mbit/s floor with a 9.5 s
  backlog at cwnd 4 920 and a 5.8 s arrival gap, while Cubic held 5.1 Mbit/s and moved 36 % more
  datagrams. So the claim this ruling supports is narrow: BBR3 is better where the window is the
  binding constraint on a merely-lossy link. Once the link is bad enough that the rate controller
  is cutting to its floor anyway, **both controllers collapse and which one does so in a given
  run is not predictable from the controller** — neither is defensible there on this evidence.
  Settling that needs ≥ 3 runs per cell on a link in one state, which this session could not
  provide: it drifted monotonically worse across the two hours of measurement.

  Method notes worth keeping: runs were **interleaved** because the link drifted hard (23 → 240
  lost packets per run across the twelve still runs, with Cubic drawing the worse half); and the
  **mesh MTU is 1230**, so BBR3's `MinPipeCwnd` floor is 4 920 B here against loopback's 5 808 —
  the same four packets, so the two pages' floor figures must not be compared as bytes.

- 🔬 **The two controllers fail in opposite ways, and `held_ms` is one instrument reading both**
  (2026-09-06). `held_ms` in hostd's pump is how long QUIC's datagram send buffer went without
  emptying — not one datagram's delay — so a long hold means two different things. **BBR3
  starves**: 43.8 s and 33.8 s holds peaking at 37 kB with cwnd at 4 920, which is ~2 Mbit/s
  through four packets at a 20 ms rtt, delivered to the user as 269 stalls whose worst gap is
  186 ms — a drizzle, not a freeze. The 37 kB peak is `frame_fits`' 32 kB `HELD_FLOOR` plus a
  frame in flight, so the capture guard was dropping frames throughout. **Cubic overshoots**:
  1.7 s and 1.9 s holds peaking at 267 kB and 347 kB, each one refresh keyframe admitted while
  the buffer was under the 32 kB floor and then draining through a 10–17 kB window. `frame_fits`
  gates a frame *before* it is encoded, so it bounds what is queued ahead of a keyframe and never
  the keyframe itself. Read a hold's `max_bytes` beside its `held_ms` or the number means
  nothing.

- ✅ **`send_ms_lo` stays one byte; no protocol widening** (2026-09-06, the mesh case the stalls
  ruling left open). Across ~24 minutes of mesh streaming and ~2 800 released stalls: `stamp
  wrapped 0` and `stamp backwards 0` everywhere, and exactly one gap past the 256 ms range —
  2.016 s, `host_gap=0ns`, `stamp=Absent`, charged whole to the link, which is the right party
  (the host had a 1 979 ms send backlog in that run). The reason is structural rather than luck:
  a wrap needs a datagram to *arrive* carrying a stamp more than 256 ms old, and when the link
  holds everything for two seconds nothing arrives to carry one, so the case lands on the
  already-pessimistic `Absent` path. Wrapping would need a link that delays past 256 ms without
  reordering and keeps delivering; this path does not do that. PROTOCOL_VERSION 15 stays free.

- ✅ **Initial congestion window 32 packets stands, and buys nothing visible on Wi-Fi**
  (2026-09-06, five interleaved starts per window over the mesh). Median first-decoded 587 ms at
  IW 32 against 581 ms at IW 10, inside a 526–1 144 ms spread within each condition. The
  arithmetic is unrefuted — a 58.7 kB keyframe is 49 packets, 5 windows at IW 10 and 2 at IW 32,
  ~33 ms at this path's 11 ms rtt — but start-up here is dominated by the 217–304 ms to the first
  datagram and 40 ms of encode, not by the window. Keep it for LAN and loopback, where it is
  cheap and the arithmetic is the same; do not claim a Wi-Fi start-up win for it.
