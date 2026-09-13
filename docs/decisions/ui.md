# Decisions — UI

See `docs/DECISIONS.md` for the legend. Newest entries go at the end.

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
  2026-09-13: rebased onto zed main `7960b2a7c9` (2026-09-12, 8 commits), fork head
  `1d2b4bb500`. Upstream grew `Platform::on_system_sleep` and
  `PlatformWindow::{visibility, on_visibility_change}` (`WindowVisibility`, so an occluded
  window is not asked for frames); the iOS backend now answers them — visible from
  `willEnterForeground` to `didEnterBackground`, only transitions reach the callback, and
  system sleep is a no-op as on the web (fork commit `1d2b4bb500`). gpui-kit was at upstream
  head; `vendor/ghostty` moved 5 commits to ghostty main `5252b193c` (translations and a
  translate-c build refactor, no C API change: the regenerated bindings are byte-identical) with
  the binding fork re-pinned at `e69a909`. Standing order from the same day: every coding
  session starts with `cargo xtask upstream check` (sync what is behind), `rustup update`,
  `cargo update -w` and the tool versions (`docs/DEV.md`).

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

- ✅ **The picker is typed at, like the palette** (2026-09-13). Thirty conversations, or a
  canvas of shells and a host of windows, were a wall to click through with the mouse or Tab
  round. Rulings: (1) a field under the title filters the rows by every word typed, any order,
  any case (`picker::matches`, the palette's rule), over the row's whole text (title and the
  status/age/directory line), so "slopty fix" finds a conversation by its project as well as
  its prompt; the "Every directory" row is a way out, not a match, and always shows; a hidden
  row keeps its selector index (`picker-agent-<i>` is the i-th conversation, filtered or not);
  (2) ↑/↓ choose a row (accent tint, wrapping) and ↩ picks it, through gpui-kit's `MoveUp`/
  `MoveDown`/`Escape` actions captured on the backdrop as the palette does; Tab still walks
  the rows; (3) the field is made on the picker's first frame, since `InputState` needs the
  window and the picker is opened from a host answer that has none — the canvas focuses the
  picker's backdrop on that frame, and the frame moves the keyboard into the field, so
  typing works at once (`Focusable::focus_handle` is the field from then on); (4) an empty
  filtered list says "no conversation matches" / "nothing matches", not that the host has
  nothing; (5) the picker sits at the top of the backdrop as the palette does — one shape
  for the two dialogs that are typed at, and the phone's keyboard, rising from the bottom,
  covers fewer rows than under a centred one. Tests: unit `the_filter_takes_every_word_in_any_order_and_case`; headless
  `a_past_conversation_is_resumed_from_the_picker` types a word, sees one row, presses ↩ and
  gets the `OpenAgent` for it.

- ✅ **A note or a file card opened on the phone fits the phone** (2026-09-13). The host's
  terminals are cut down to the viewport when this client opened them (above), but the cards
  the client places itself — a note at 320×240, a file card at 560×420 — went in at their
  desktop size, so a file card on a 393 pt phone ran past the right edge with its find bar
  there. Ruling: `fitted(size)` cuts a default size to `viewport_max` before the free slot
  is asked for, so a phone gets a phone-wide card and a desktop the default; the rule is the
  terminal's (`fit_to_viewport`), applied at placement since the client is the placer. A
  window or display picked from the host is cut the same way, keeping its shape (the
  `MAX_PICKED` cap is itself fitted first, so the aspect-preserving shrink sees the
  viewport). Test: headless `a_phone_opens_notes_and_file_cards_that_fit_it` (defaults at
  1000 pt, both within the snapped viewport width at 393 pt, a 1920×1080 display inside it
  at 16:9).

- ✅ **The palette and the picker fit the phone** (2026-09-13). Both were a fixed desktop
  width (520 / 560 pt) centred on the backdrop, which on a 393 pt phone put a third of the
  dialog off each edge: the field was typed into blind and the rows' right ends were gone.
  Ruling: `w_full` capped by `max_w` at the desktop width, with the theme's `md` margin each
  side, so a desktop sees exactly what it did and a phone sees the dialog inside its screen.
  Test: headless `the_palette_and_the_picker_fit_the_screen_they_are_on` (the desktop widths
  at 1000 pt, both inside 393 pt with a margin after a resize).

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
  `Rc<ShapedLine>`, then painted at its start column (2026-09-13: shaped without the forced
  width and placed by cell, terminal.md "Glyphs are placed by their cell"). Pixel-identical
  to whole-row shaping: every glyph sits at its cell's column, kerning is off, and
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
  the theme's size on the base cell, and `paint` walks each word's placed glyphs (read off
  `ShapedLine::layout()`, the fork's accessor; `LineLayout` / `ShapedRun` / `ShapedGlyph` were
  already public) and calls `Window::paint_glyph` / `paint_emoji` at the zoomed font size, at
  `origin + placed position × zoom` on the derived baseline (`glyph_origin`, unit-tested). Sound
  because a glyph's placed x is its cell's column × base cell and the
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

- ✅ **What the zoom p99 is now, and whose** (asked 2026-09-06, answered by the 2026-09-14
  smooth probe at `c689d06`, MEASUREMENTS "the smooth probe after the week's element work":
  the 20-shell zoom cycle's draw p99 is 2.8 ms and its max 10.2 ms, 0 dropped, after the
  raster ladder, the shaped chrome and the prompt-walk fix; the paragraph below is the
  state that ruled it worth chasing). With the element at 4 % (prepaint) + 12 % (paint,
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

- ✅ **Code is coloured by grammar and painted from the theme** (2026-09-12). A file card and a
  fenced block in an answer read as plain mono, so a Rust function and its comment looked the
  same as a diff of prose. Ruled: `slopty-ui::highlight` parses with `syntect` — the bundled
  Sublime grammars on `regex-fancy`, the pure-Rust engine, no onig and no C — and reduces its
  TextMate scopes to nine tokens (`Token`: comment, string, constant, keyword, type, function,
  punctuation, invalid, plain) whose colours are looked up in the app's theme at draw time:
  keyword = ANSI magenta, string = green, constant = yellow, type = cyan, function = blue,
  comment = `text_muted` italic, punctuation = `text_secondary`, invalid = `error`. The theme
  (a settings reload, light or dark) recolours without parsing again, and code reads in the
  shell's own palette. Not the alternatives: gpui-kit's own highlighter is tree-sitter (a C
  runtime and a C grammar per language, against the pure-Rust rule, and its feature is off in
  our build); a TextMate theme in syntect would hold a second palette that drifts from the
  tokens. Where it runs: the file card parses on a background thread after every read
  (`FileView::recolour`, a generation guard drops a parse the next read overtook, plain text
  until the spans land, `"2 lines, Rust"` in the summary once they have) and draws each visible
  row as a `StyledText` with one `TextRun` per span; a fenced block is coloured through
  gpui-kit's `TextViewDefaults` code-block hook (`highlight::code_block`, installed by
  `kit::sync` after `sync_base` reinstalls the defaults), on the UI thread at layout, cached per
  block by the view. Numbers (`highlight::tests::timing_of_a_full_card`, MEASUREMENTS
  2026-09-12): a 2 000-line card ≈ 165 ms on the background thread, a 4 KB block ≈ 6 ms; a line
  past 4 000 bytes stays plain (`LINE_MAX`), so a minified bundle costs nothing. Tests:
  `highlight::tests` (tokens by extension, name, first line and fence; a block comment across
  lines; runs cover the line; byte ranges of a block), `file::tests::a_file_is_coloured_by_its_grammar_after_the_read`
  (background parse, generation guard, summary), `kit::tests` (the hook is installed after a sync).

- ✅ **A font run spans colours; the painter reads them by glyph (2026-09-13).** GPUI shaped a
  line in one CoreText run per *decoration* change (colour, underline, strikethrough), so a
  coloured line of code cost one shape per token and the shaped-line cache keyed on all of it.
  A 2 000-line file card zooming on the canvas went from 0 dropped frames to 116 of 565 after
  the colouring landed (p95 9.4 → 22.5 ms), bisected to that commit, and an experiment that
  drew the same card without its spans brought the frame back (MEASUREMENTS 2026-09-13). The
  fork commit `06c345cf` (`gpui: shape a line's font runs by font, not by colour`) merges font
  runs by font id only, at all four sites (`shape_line`, `shape_text`, `layout_line`,
  `layout_line_by_hash`); the decoration runs are untouched, and `paint_line` already applied
  them by glyph byte index, independent of the font runs, so nothing else changes. With it the
  zoom cycle is back to 0 dropped of 787 frames (p95 9.6 ms). The trade-off, accepted: a
  ligature can now form across two colours and takes the first one — with a coding font that
  means `->` in one hue where the punctuation and the operator were coloured apart, which the
  nine-token theme does not do. Not the alternatives: colouring a file card only when the zoom
  rests (a flicker, and the terminal grid's coloured rows have the same cost); a per-line
  plain-text fallback in `FileView` (kept only for a row with no spans, where it saves the
  run vector, but it did not move the coloured card's numbers). To carry on an upstream sync:
  the patch is one hunk per site, `cargo xtask upstream sync` replays it.
