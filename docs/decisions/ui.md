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
  system sleep is a no-op as on the web (fork commit `1d2b4bb500`; `6e87f6a825` on top makes
  the `secondary` modifier ⌘ on iOS as on macOS — an iPad keyboard is a Mac keyboard, and
  gpui-kit's `secondary-a` / `secondary-enter` never matched there, found by the iOS e2e's
  settings-editor step; gpui-kit `569f7a21` on top of `2604f69` does the same for its own
  bindings — ⌘A, ⌘C, ⌘V, ⌥-word motion were `cfg(target_os = "macos")` — and `Kbd`'s
  glyphs). gpui-kit was at upstream
  head; `vendor/ghostty` moved 5 commits to ghostty main `5252b193c` (translations and a
  translate-c build refactor, no C API change: the regenerated bindings are byte-identical) with
  the binding fork re-pinned at `e69a909`. Standing order from the same day: every coding
  session starts with `cargo xtask upstream check` (sync what is behind), `rustup update`,
  `cargo update -w` and the tool versions (`docs/DEV.md`).
  2026-09-14: rebased onto zed main `7cda6f0523` (8 commits, no API break, fork head
  `a07e5cf08a`) and gpui-kit main `9504b4658d` (13 commits, fork head `ef7525ba`). gpui-kit's
  only conflict was the usual one, now in `Cargo.toml` as well as `Cargo.lock`: upstream bumped
  its `gpui-pre` deps 0.3.1 → 0.3.5 in the block our one commit rewrites into git sources on the
  zed fork, so the bump has nothing to carry over and our side of the hunk stands. libghostty-rs
  was at upstream head. `vendor/ghostty` is left at `5252b193cf`, 19 commits behind ghostty main:
  the binding pins that commit and a bump means regenerating and re-checking the bindings, which
  is its own change. Nightly moved to 2026-09-13; stable 1.98.1 unchanged.
  2026-09-24: rebased onto zed main `d528c665e1` (178 commits, fork head `5cda33756c`) and
  gpui-kit main `aa2c3f77` (v0.6.5, 109 commits, fork head `64cd7539`).
  - The iOS backend commit conflicted in `gpui_apple`. Upstream moved from `block` to `block2`
    and made the Metal atlas generic (`AtlasState<MetalAtlasTextures>`, #64331). Resolution:
    upstream's shapes, with our `supports_shared_storage` flag and the shared macOS+iOS
    dependency block. Our glyph-size patch still passed the atlas key by reference, which
    #64331 broke; fixed as its own commit.
  - `upstream.rs`'s divergence check now also accepts a rebased branch that ends in fix commits
    of its own after the replayed patches.
  - Xcode 27.0 shipped without the Metal toolchain, so shader builds failed until
    `xcodebuild -downloadComponent MetalToolchain` was run.
  - Adopted for free: atlas-released sprites are skipped instead of crashing, no panic
    hit-testing past a wrapped line, synthetic drags end on button release, no hover through
    covering windows, and the a11y adapter no longer leaks on window close. gpui-kit also
    brought mobile selection handles and an edit menu for `Input`/`TextView`.
  - Queued as follow-ups: `ShapedLine::cursor` for truncating without reshaping, standalone
    modifier keystrokes for remote input, and integer `ElementId`s instead of per-frame
    `format!` ids.
  2026-09-24, fork commit `c2733ceab3` (`gpui_apple`): the video surface path holds each
  frame's `CVMetalTexture`s until the command buffer's completion handler and flushes the
  texture cache every frame. The shader multiplies by a matrix built from the buffer's
  `kCVImageBufferYCbCrMatrixKey` tag (BT.709 when untagged) at the range its pixel format says,
  where it used to hard-code BT.601. A pixel format it cannot sample, or a failed texture, skips
  that surface with one log line instead of the `assert_eq!`/`unwrap` abort. Tests in the fork:
  `the_matrices_map_codes_to_the_right_colours`, and
  `a_tagged_surface_renders_its_colour_and_a_foreign_one_is_skipped`, which renders BT.709 red
  headless three frames running and checks the pixel values.
  2026-09-25: rebased onto zed main `7fdb97cad5` (5 commits, fork head `c8f8aa66bf`) and gpui-kit
  main `62966c99` (fork head on top of it, 2 commits); libghostty-rs was at upstream head. The
  forks moved to their default branches the same day (the entry below).

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

- ✅ **Each fork carries our commits on its default branch** (2026-09-25). zed and gpui-kit on
  `main`, libghostty-rs on `master`: upstream plus our commits rebased on top, one branch per fork
  and nothing else on it. They used to live on a `slopty` branch beside a `main` that nobody
  synced, so GitHub's front page showed a mirror hundreds of commits behind while the branch we
  build was current; the user found the second branch confusing and the numbers misleading.
  `Cargo.toml` names no `branch`, so Cargo follows the default branch and `Cargo.lock` pins the
  commit. gpui-kit's own manifest must spell the zed source the same way (no `branch` either),
  or Cargo sees two sources and builds gpui twice. libghostty-rs was a plain repository pushed
  as a mirror, so GitHub showed no ahead/behind and offered no upstream pull request; it was
  recreated as a real fork of `Uzaaft/libghostty-rs`. `xtask/upstream.toml` has one `branch`
  per fork, the same name in the fork and in the checkout.

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
  driver/viewer hand-off is the "take" pill below. The first shell comes with the attach,
  often before a frame has measured the viewport (2026-09-15: the phone e2e opened onto a
  720-wide card one run in two, and its agent golden flapped between the two layouts); a
  placement that finds the viewport unmeasured waits in `fit_items_pending` and is fitted on
  the first frame, before the reveal that follows it. Test:
  `a_shell_placed_before_the_first_frame_is_fitted_on_it`.
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

- ❌ **Notes are shared text, last writer wins** (superseded 2026-10-05 by "A note is a
  Markdown file"). `ItemKind::Note { text }` was already in the
  document; the client now edits it in place with gpui-kit's `TextareaState` (`NoteView`).
  Edits go to the host after a 400 ms typing pause and on blur (as `SetNote`, the text alone,
  since 2026-09-27: multi-client.md, "An item change carries only the field it changes"); a remote change is applied only while this client is not editing. No merge: notes are
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
  | `surfaces.border` | `24272E` | `D8DBE1` | every hairline (1 pt; one device pixel since 2026-10-03) |
  | `surfaces.text` | `E6E6E6` | `1D1D1F` | primary |
  | `surfaces.text_secondary` | `B4B9C3` | `4B4F58` | labels, tool summaries, counts |
  | `surfaces.text_muted` | `8B919C` | `66666B` | hints, timestamps, folds, inactive titles |
  | `surfaces.accent` | `8AB4F8` | `2A63C4` | focus ring, active border, links (text and hairlines only) |
  | `surfaces.success` | `98C379` | `187633` | connected, agent done |
  | `surfaces.warn` | `E5C07B` | `8B5D00` | agent waiting, "N need you", muted, reconnecting |
  | `surfaces.error` | `FF9095` | `8F1D1D` | failed result, failed command, pairing error (2026-10-04: apart from the green and the amber for colour-blind eyes) |
  | `radii.xs / sm / md` | 4 / 6 / 8 | same | pills and inline buttons / buttons, inputs, key caps / panels, items, popovers |
  | `spacing.xxs … xl` | 2 / 4 / 8 / 12 / 16 / 24 | same | the only paddings and gaps in chrome |
  | `typography.ui_size` + `caption()/small()/title()` | 13 → 10 / 12 / 15 | same | chrome type scale (settings move the base) |
  | `typography.mono_size` @ `mono_line_height` | 13 @ 1.0 | same | terminal grid, code in the conversation (the multiplier is ghostty's `adjust-cell-height`; 1.0 = the font's own) |
  | `typography.markdown_line_height` | 1.5 | same | assistant turns |
  | `alpha::FAINT` | 0.12 | same | a quiet fill, a hover wash, the command-block hairline, the visual bell |
  | `alpha::TINT` | 0.25 | same | a tint that has to be seen: a text selection (a selected row is `overlay` since the second de-slop pass) |
  | `alpha::PRESSED` | 0.4 | same | under the pointer; a scrollbar thumb |
  | `alpha::SCRIM` | 0.6 (0.45 since 2026-10-03) | same | modal backdrop |
  | `alpha::STRONG` | 0.7 | same | the separator after a failed command, minimap item blocks |
  | `alpha::VEIL` | 0.9 | same | panels over video and the canvas: the stream HUD, the minimap, a looker's outline and tag |

  Rules that follow (as shipped): status tones are chrome tokens now — the terminal palette
  stays the terminal's; the chrome tones share its hues so nothing jars, and the light table
  has its own set. The agent badge uses the accent for both busy states (thinking and a tool;
  the label says which), `warn` while blocked, `success` when done. (Amended 2026-10-01: busy
  states recede into `text_muted` and a finish is the accent dot; see **Colour goes to what
  needs the person** below.) Shadows go except on
  floating layers (picker, host switcher, search bar, "↓ latest" pill), and those use
  `shadow_sm`. Focus is one accent hairline: the focused item's frame, the search bar while it
  has the caret, the composer while it has the caret. Hover is a `HOVER` wash or a step up
  the surface ladder, pressed one step further, never opacity. Pills: `radii.xs`,
  `spacing.sm`/`spacing.xxs` padding, `small()` type, `TINT` fill of their tone with the tone
  as text. (Amended 2026-10-03: a pill is a capsule, `radii.full`, filled with its tone at
  `FAINT` in dark and `FAINT_ON_PAPER` in light; a key cap stays at `radii.xs`. See the stage 3
  entry at the end.) Buttons: `radii.sm`, `spacing.md`/`spacing.xs` padding in a dialog and
  `spacing.sm`/`spacing.xs` in a bar; the one primary action per surface is an accent fill
  with `accent_fg` text, a secondary one is `raised` → `overlay`. (Amended 2026-09-27 (design audit): a fill takes
  `accent_fill` with `fill_fg`, and `accent_fg` is gone; see **Fills are fills** below.) Key caps are `raised` on the
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
  Amended 2026-09-24: the once-per-frame timer is the self-test build's only
  (`slopty_app::e2e::Pacer`, `feature = "e2e"`). Its reason was GPUI's `test-support`, which
  draws every dirty window inside `flush_effects`; the shipped app draws at the display's
  vsync however many updates land in between, so the timer only delayed an echo by up to one
  nominal 60 Hz frame (16.7 ms) and capped a ProMotion display at 60 updates a second. The
  shipped loop still drains up to 256 queued events with `try_recv` into one update.
  Amended 2026-09-25: the self-test build's timer is gone as well. `test-support` draws inside
  `flush_effects` only in GPUI's test mode (`GpuiMode::Test`, which only a `TestAppContext`
  sets), never in a running app, so the self-test build draws exactly as the shipped one
  does. The timer only made the self-test differ from the shipped app: under floods it drew
  60 frames a second on a 75 Hz display, and an echo typed beside them waited up to a frame to
  be applied (42.0 → 28.4 ms, MEASUREMENTS 2026-09-25, "keystroke to glass, hop by hop").

- ✅ **A session sends at most 125 frames a second** (2026-09-05). The host coalesced PTY output
  for 2 ms and then sent a frame, so a flooding shell produced 500 frames/s per session; twenty
  of them overran every client's 256-frame sink and hostd detached the client ("cannot keep
  up") within seconds. `slopty_worker::session::MIN_FRAME_INTERVAL` (8 ms) paces a session's
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
  every stage of one echo — `term input received` and `frame sent` in `slopty-worker`'s connection,
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

- ✅ **One overlay shell, one alpha ladder (2026-09-14).** The three modal surfaces (the command
  palette, the window picker, the settings editor) each carried their own copy of the same
  fourteen lines of chrome, and the copies had drifted: three max widths (520, 560, 640), three
  heights (440, 520, 720) and two elevations (`shadow_md` on the palette, `shadow_sm` on the
  other two). Nothing chose those numbers; they were typed one at a time. The `alpha` module had
  grown the same way, to fourteen constants, seven of them used once: `MINIMAP` 0.92 and `HUD`
  0.85 and `MINIMAP_LOOKER` 0.8 and `LOOKER_TAG` 0.9 are four names for "a panel laid over
  content", and `HOVER` 0.08 was `TINT_FAINT` 0.08 under a second name.
  Ruled, and this is what a minimalist interface means here, stated so it can be checked rather
  than admired: every overlay wears `kit::dialog`, which is one radius (`radii.md`), one hairline
  border (`surfaces.border`), one elevation (`shadow_sm`) and the UI font, sized from
  `kit::Overlay` — `List` 560×520 for a list typed at, `Editor` 640×720 for a file being edited.
  Two sizes, because a command list and a text editor want different room and a third size is one
  nobody could name. Every overlay sits on `kit::backdrop`: the canvas under `alpha::SCRIM`, the
  dialog near the top so a phone's keyboard covers fewer rows. The alpha ladder is six steps, each
  used in more than one place: `FAINT` 0.12 (a quiet fill, a hover wash, a wash across the grid),
  `TINT` 0.25 (a tint that has to be seen), `PRESSED` 0.4, `SCRIM` 0.6, `STRONG` 0.7 (a mark read
  over whatever it covers), `VEIL` 0.9 (a panel read through only barely).
  What moved on screen: a hover wash 0.08 → 0.12, a command-block separator 0.18 → 0.12 and the
  visual bell 0.15 → 0.12 (both hairline washes, and the gap to the failed-command separator at
  0.7 widens, which is the signal), the stream HUD 0.85 → 0.9 and another client's minimap outline
  0.8 → 0.9 (both more legible over video), the minimap panel 0.92 → 0.9, the palette from 520 to
  560 wide and from `shadow_md` to `shadow_sm`. Nothing else.
  The rules that hold from here: chrome takes every padding and gap from `Spacing`, every corner
  from `Radii`, every transparency from `alpha`, and every colour from `Surfaces`; no gradient, no
  glow, no second elevation (amended 2026-10-03: two named elevations, resting and floating, both
  drawn only by the kit, and the dark scrim at 0.45); no emoji and no decorative glyph in chrome text; motion only where it
  carries meaning (the camera flights, the take-back offer), never as decoration. Microcopy is a
  noun phrase or a verb in the imperative, never a sentence about what the program just achieved.
  Text that names a thing is sentence case; text that reports a value is lowercase. A heading, a
  button's accessible name, a field's placeholder, an empty state and a card's derived title all
  name something, so they read `Find`, `Take over`, `Type to filter`, `Nothing matches`,
  `Waiting for the first frame…`, `Display 2`. A readout says what a number or a state currently
  is — the stream HUD (`rtt –`, `stalled`), the agent pill (`working`, `allow? bash`), the file
  card's summary (`212 lines`, `binary, 1.2 MB`, `reading…`), a finished command (`done 1.2 s`),
  a toast (`pointed Ada at Notes`) — and a capital there reads as the start of a sentence rather
  than the start of a value. One deliberate crossing: an item pill's visible word is a single
  lowercase token (`reload`, `edit`, `find`, `take`, `hooks`) so the chrome band reads as a row
  of switches rather than a sentence cut into pieces, while its accessible name is the
  sentence-case phrase a screen reader announces (`Read the file again`, `Take over`).
  `kit::chrome_text_is_sentence_case` holds the naming half.

- ❌ **Ranking the command palette by match quality** (2026-09-14; superseded 2026-10-04 by
  "The palette ranks within each section, shows what matched, and Esc takes a query back"
  below, once its own revisit condition held; written and then measured
  against the real command list rather than a toy one). `palette::filter` keeps every item whose
  label holds all of the query's words and returns them in the order they were built. The obvious
  improvement is a score — the front of a label beats the front of a word inside it beats the
  middle of one — and it was implemented, tested and then dropped, because the palette's own data
  does not support it.

  Read the fixed commands and almost nothing moves: `zoom`, `new`, `term`, `note`, `close`,
  `prompt` all rank exactly as they are declared, because the labels are short noun phrases with
  no incidental matches in them. One query changes — `card` lifts `Card above` and its three
  siblings over `Name this card` and `Next card` — and that is the wrong way round for the
  commands people reach for. The one place a score would earn something is a shell's recent
  commands, where typing `test` should prefer `Rerun cargo test` over `Rerun cargo nextest`; that
  is a narrow win to buy with a ranking over everything.

  Two repairs were tried before dropping it. A length tiebreak (prefer the label the query covers
  more of) scores 0 for every line a context built, so on a tie it demotes a card the human named
  below whichever fixed command happens to be short — typing `note` put `New note` above
  `Go to notes.md`, caught by `a_path_typed_into_the_palette_opens_a_file_card`. Applying that
  tiebreak only between two fixed commands fixes the symptom and breaks the sort: a comparator
  that orders like pairs and calls unlike pairs equal is not transitive. A third shape works —
  rank within each group, never across, deriving the groups from where each kind of line first
  appears — but it is a `HashMap` and a coupling to the assembly order of `palette_lines` bought
  for one query about rerun lines.

  So matching stays what it is: whole words, case-insensitive, all of them present, in the order
  the lines were built — agents waiting on the human first, a shell's commands newest first, the
  cards in document order, then the commands as declared. That order is information; a score that
  changes one query is not a reason to spend it. Revisit if the fixed list grows long enough to
  carry incidental matches, or if the rerun lines become the palette's main traffic. Not to be
  confused with subsequence matching, which is separately unwanted: it would widen `note` to any
  label with those four letters in order, and a palette that answers a four-letter query with nine
  lines is slower to use than one that answers with one.

  The picker is left alone for the same reason and a stronger one: its rows are grouped by kind —
  sessions, then the host's displays and windows — and that grouping is the information the list
  carries.

- ✅ **A display card's grip is locked to the display's shape (2026-09-15).** `follow_geometry`
  carries a promise in its own doc — the picture is never stretched and pointer mapping stays
  exact — and it kept that promise for exactly one of the two kinds of screen card. A window's
  card can be dragged to any shape because the drag is a request: `resize_remote_window` asks the
  host for a window that size, the host answers with a `Geometry`, and the card settles on the
  shape the window really took. A display card had the same free drag and no such answer;
  `resize_remote_window` returns early for `CaptureTarget::Display`, because nothing can make a
  monitor a different shape. The card kept whatever the drag gave it and the picture, painted
  with `ObjectFit::Fill` over the full bounds, stretched for good.

  So the lock lives where the shape cannot be negotiated: `locked_aspect` is `Some(h / w)` for a
  display and `None` for a window, and `resized` reads it. A locked card has one ray of sizes it
  may take, and the corner goes to the point of that ray nearest the pointer. The first rule tried
  was simpler — whichever axis the hand moved further along drives the other — and it is
  discontinuous: at `|dx| == |dy|` the driving axis swaps and the width jumps tens of units, which
  a diagonal pull across a corner grip crosses all the time. Projection costs a little of the
  literal hand-follows-corner feel off the ray and buys a card that never snaps. On release the
  height is recomputed from the *snapped* width instead of being snapped itself, since the snap
  grid would otherwise nudge the card off its aspect by up to half a step and leave a stretch too
  small to see and too permanent to forgive.

  Letterboxing was the other way to keep the picture honest, and it is worse here: bars inside a
  card on a canvas are a second surface at a second colour inside a surface that already has one,
  and the card's own bounds stop meaning the picture's bounds, which is what `to_stream` maps a
  pointer through. Locking the card keeps one rectangle with one meaning.

  Tests: `a_card_over_a_display_keeps_the_displays_shape_as_it_is_dragged` over the arithmetic
  (free drag, a still hand, a drag along the ray, a pair straddling the diagonal whose widths must
  stay within two units of each other, the floor), and
  `the_grip_asks_the_host_to_resize_the_window` over the wiring, where the same canvas holds a
  window card and a display card and only the display reports a locked aspect.

- ✅ **Chrome text clears WCAG AA on every surface, not just the one it usually sits on.**
  The terminal grid has `minimum_contrast` to lift colours a program chose. Chrome had nothing,
  because these colours are ours and the fix is to pick better ones — so nothing checked them,
  and five of the light variant's inks failed 4.5:1 against the darker surfaces: `accent` 3.83
  and `warn` 3.93 on `overlay`, with `success`, `error` and `text_muted` between 4.09 and 4.45.
  All five are drawn as text. `accent` is a link and the primary action's label, `warn` is
  "N need you", `success` is "connected": body text, not decoration. Dark always passed, which
  is why this went unseen — dark is the default and nobody ran the app in light.

  The five were darkened 5–10% with the hue held (table above). `chrome_text_clears_wcag_aa`
  checks all seven inks against all four surfaces in both variants, plus `accent_fg` on an
  accent fill (Amended 2026-09-27 (design audit): the accent is no longer a fill, so that pair went with
  `accent_fg`; `status_fills_read_as_their_hue` holds `fill_fg` on `accent_fill` to AA): a status label follows its card and a card can sit on any surface, so checking
  an ink against the one surface it usually has is checking the easy case.

- ✅ **A title is cut for an ellipsis only when it is more than half a pixel too wide.**
  Every filled chrome title on the canvas was rendering as two glyphs and a dot — `sh…` for a
  terminal named "shell", `no…` for a note. The cause is a rounding mismatch, not a layout one:
  taffy hands the element a rounded box while the shaped line keeps its fractional width, so
  "shell" was shaped at 26.27 pt and given 26. `cut` read that quarter-pixel as an overflow, and
  because the ellipsis needs room of its own it dropped four glyphs to save a fraction of one.

  `chrome_text::SUBPIXEL` is half a pixel of tolerance on that one comparison. Half a pixel of
  overrun cannot be seen; the cut it used to cause could be read across the room.

  The first diagnosis was wrong and worth recording: `max_width: relative(1.0)` on a `flex_1`
  parent resolving a percentage against a zero flex-basis. Changing it moved **zero** pixels, so
  it was reverted rather than kept as a plausible-sounding extra — a change with no measurement
  behind it is not a fix.

  This defect is the argument for looking at a golden rather than only diffing it. The numeric
  comparison read 0 differing pixels on `terminal` because the golden had the bug baked in;
  nothing in the suite could have reported it. Tests at `chrome_text::tests`
  (`a_label_a_fraction_of_a_pixel_over_its_box_is_kept_whole` and two siblings) now pin the
  arithmetic, which needed `cut` split into `cut_at` over plain numbers — `ShapedLine`'s fields
  are `pub(crate)` in gpui, so the decision could not otherwise be reached from a test.

- ✅ **The top bar prints no key chords; a hover says what a button does and the key that does it.**
  The bar carried every binding inline — `fit ⌘1  + shell ⌘T  + agent ⌘⇧T  + note ⌘⇧N
  + window ⌘O  ⋯ ⌘⇧P` — six chords across one 40 pt strip, most of its text, none of it
  answering what a button does. Zed and Warp print none of theirs. The index belongs in the
  command palette, which Slopty already has and which is the phone's only way to an action
  without a button.

  `kit::Hint` is the tooltip: what the button does, then its key in `text_muted`, on `raised`
  behind the one hairline and the one elevation the rest of the chrome wears. It is gated on
  `SHORTCUT_HINTS` (macOS), since a hint needs a pointer to hover and glass has none.

  Nothing is lost to a screen reader: the `aria_label` was already the bare word, so VoiceOver
  never read the chord out of the visible label either. A dropdown row keeps its key inline
  ("Add host…  ⌘⇧H") — a menu showing its shortcut is what a menu is for, and that is the one
  place the convention runs the other way.

- 🔬 **The golden tolerance is blind to chrome text.** Taking the six key chords out of the bar
  moved 2289 pixels on `terminal` and 2558 on `note` — 0.42% and 0.47% against a 1% tolerance, so
  neither golden failed and `--accept` left both untouched, still encoding a bar the app no longer
  draws. The tolerance is tuned for what `snapshot.rs` says it is tuned for ("a font hint or an
  antialiasing change moves a few hundred pixels, a broken layout moves a few hundred thousand"),
  and every word of chrome text in the app falls between those two. `--accept-all` is the blunt
  answer and was used here. The sharp one is an assertion over the bar's text through the app's
  own dump socket, at layer 3, where a word appearing or vanishing is a string comparison and not
  a pixel count. Written 2026-10-02 for every golden: its text beside its PNG
  (`docs/decisions/testing.md`, "A golden is held in words too").

- ✅ **The de-slop pass: every state is a golden, and the chrome says less** (2026-09-25). The
  user judged the app's UI and UX to be AI slop and asked for it to be minimal and genuinely
  beautiful, in the Warp and Zed school. The tokens were already clean; the slop was in the
  composition, and only four goldens existed, so most of the UI had never been looked at.
  `crates/slopty-e2e/tests/app/gallery.rs` now renders every state worth a look through the
  self-test socket, each asserted through the accessibility tree as well (the tolerance cannot
  see a word): `first-run`, `add-worker`, `workspace` and `workspace-dark`, `overview`,
  `palette` and `palette-dark`, `settings`, `empty-workspace`, `agent-needs-you`,
  `remote-window`, `transfers` (upload pill and port chip) and `server-unreachable`; the iOS
  test adds `ios-<device>-first-run`, `-columns`, `-palette` and `ios-pad-split`. Reaching two
  of them took a socket command each: `PickWindow` (a window id that names no window, so a
  remote tile's placeholder renders without the capture grant, and no screen ever lands in a
  golden) and `Resize` on iOS, which lays the app out in the size asked at the window's top
  left, the stand-in for Split View since a UIKit window cannot be resized from inside. The
  harness pins `appearance = "light"`: under the `system` default every golden depended on the
  machine's appearance that day. What the review found and what changed, worst first:
  - The column indicator, a 160 pt track with the view bracketed and the active column
    filled, read as a progress bar. It is a dot per column now, the focused one in the text
    colour, the ones in view muted, the rest faint, and nothing for a single column.
  - The first run was a four-line form over a dimmed app whose titlebar still offered "+" and
    "…". It is the whole window now: a heading, one line ("Slopty finds your workers through a
    server on your tailnet or VPN."), the field, one primary button and the other way in as a
    quiet link. The port and encryption paragraph went; the phone keeps a "Paste".
  - "point" was the one visible action on every focused tile, and "go" sat beside an agent's
    badge doing what a click on the tile does. Both went: pointing is in the palette ("Point
    other devices at this tile") and ⌘⇧O, and a waiting agent's badge is itself the button.
  - Toasts landed at the top, over the headers whose pills they were about (the port notice
    hid the upload pill); they sit at the foot of the strip now, and "take back ⌘Z" is "Undo".
  - Chords were printed on buttons ("Cancel esc", "Save ⌘↩") and were an empty workspace's
    only content. The empty workspace is two buttons, "New terminal" and "Add a window";
    `kit::a_chord_is_spelled_only_by_the_key_tables` fails on any chrome literal holding a
    chord, and menus read their keys from the binding tables (`palette::keys_for`), since the
    "+" menu had spelled `⌘⇧T` where the palette said `⇧⌘T`.
  - The settings dialog's title was a seventy-character temp path; it is the file name, the
    path its accessible name.
  - A fresh shell showed two hairlines, the header's and a command-block rule over the first
    prompt. No rule is drawn over a prompt with only blank lines above it, nor a neutral one
    on the viewport's top row. Since 2026-09-28 a rule parts only heads that touch, so no
    prompt that follows output has one (terminal.md, "The head band is seen").
  - The focus ring was 2 pt of saturated accent against 1 pt hairlines; it is the accent
    hairline the token ruling always named. The status dot, accent when focused, said what the
    ring says; it now shows only while the tile's worker is away.
  - The overview drew the empty trailing workspace as a solid white slab; it is a dashed
    outline. A remote tile waiting for its picture sits in a canvas-coloured well instead of
    looking like an empty terminal. A ticked task is an accent box with a tick, not a grey
    square. The palette's field lost its second frame. "+" and "…" are drawn from quads,
    centred in square buttons, rather than set on a text baseline.
  - The phone key bar crowded fourteen equal caps into 402 pt ("paste" and "find" ran into
    their edges). Caps now have a floor (36 pt, 52 for a word) and the row scrolls past it;
    the keys the soft keyboard cannot type come first and the clipboard key follows the
    arrows, so both are in view before the row scrolls.
  The same day a lone column became centred and the overview centres a strip that fits
  (workspace.md, the niri port's deviations).
  The iOS renders, reviewed once the fork's simulator fix landed, added three fixes: an
  overlay's backdrop starts under the window's safe area (the phone's palette sat on the
  Dynamic Island); key caps take a fixed basis and share out any spare width equally (a
  phone keeps 36/52 pt caps and scrolls, an iPad's row fills the width, where equal flex
  shares had grown "esc" and "paste" and not the arrows); the phone's `columns` golden waits
  for the take-back toast to lapse. In Split View (511 pt) every column now shows at full width
  and the strip scrolls between them (workspace.md, compact width), where two half columns of
  231 pt had stood side by side.
  Terminal colours, the same day: the pale mint prompt in `workspace` was the prompt's own
  24-bit colour (`#80FFEA`, 1.2:1 on white), not the palette, so no ANSI tuning could reach it.
  libghostty draws nothing here; the minimum contrast is the app's own ghostty-style check
  (`Colors::text_over`) and `[terminal] minimum_contrast` defaulted to 1.0, off, as ghostty's
  does, so it never ran. It now moves a colour toward black or white only as far as the
  minimum needs, hue kept (mint on white becomes teal, where the snap made it black), and the
  light brights 11–14 and dark 8 were darkened or lightened, hue held, until every ANSI colour
  but the background's namesake clears 4.5:1 (`ansi_text_clears_wcag_aa_on_the_terminal_background`;
  11–14 read 3.1–3.6 before). The minimum now defaults to 3.0 (ruled the same day), so the
  prompt reads without a setting.

- ✅ **The file tile's editor** (2026-09-25). The body is gpui-kit's code editor
  (`EditorState` in code-editor mode, line numbers on, folding, soft wrap and its own search
  off), with `.appearance(false)` so the tile's own frame and tokens draw it, in the terminal
  mono at the tile's text size. Not the alternatives: a text area of our own would rebuild
  selection, IME, undo and scrolling that gpui-kit already has; gpui-kit's tree-sitter
  highlighter would bring a second grammar set with its own colours beside syntect's nine
  theme tokens. The editor takes a highlighter factory, so ours is `highlight::editor`: it
  keeps the last parse's colours per line, moves them with each edit (`editor::splice`, lines
  inserted or removed shift the rows below), and parses the whole text again on a background
  thread 60 ms after the typing stops. What that costs a frame and a keystroke is in
  MEASUREMENTS.md (2026-09-25, "the editor's highlighter"): tens of microseconds, so a
  keystroke never waits for a parse, and an edited line's colours catch up once the typing
  pauses.
  Saving and the disk:
  - ⌘S sends the whole text with the modification time it was based on. The worker refuses
    a save when the file changed since then, and the tile says so. While a save is out, a
    second ⌘S sends nothing.
  - A read drops the file's final newline and says whether there was one
    (`FileRead::Text.final_newline`); a save puts it back.
  - The dirty mark is a small dot in the header, before the actions (a11y "Unsaved
    changes"). The header carries no text pills: find is ⌘F and the palette's "Find in
    terminal or file", and the file leaves the app by its proxy, a small page drawn before the
    title as a Mac document window has one (a11y "Drag the file out"). Not the title itself:
    dragging the header moves the tile. The trouble line is one row above the text, in the muted tone, with "Reload"
    and "Overwrite" as the kit's small buttons. No modal: a change on disk under an edit is
    ordinary when an agent works in the same tree.
  - The text replaced by a read waits for the next render (`apply_pending`), since the
    editor's `set_value` needs the window.
  Headless tests (`file/tests.rs`) drive the real editor with real keys: save with its base
  and newline, the save's own echo, the conflict and both buttons, the silent reload with its
  tint and caret, read-only, a failed save. The app e2e (`tiles.rs`) edits, saves and
  catches a change on disk under the live worker, with the goldens `editor`, `editor-dirty`
  and `editor-conflict`.

- ✅ **The browser tile's native view** (2026-09-25; superseded 2026-09-30 by "A browser
  tile's page is composed by the window, not laid over it": the clip view, the covers and the
  key monitor below are gone). `slopty_platform::web` owns the
  `WKWebView`: a clipping view (the strip's area) added to the GPUI window's view from its
  `raw_window_handle`, and the web view inside it at the tile's body. macOS converts to the
  unflipped `NSView` coordinates. iOS uses UIKit's top-left points as they are, and speaks to
  the view by selector, since `objc2-web-kit` types `WKWebView` for macOS only; the fork's iOS
  window hands out its `UIView`, so both platforms run the same tile. The navigation
  delegate is shared and takes the web view as a plain object. Why the page is placed in a
  prepaint and hidden under covers is in workspace.md ("A browser tile is a native page that
  follows its tile").
  The keyboard: on the Mac the GPUI view never accepts first responder, so a click on the page
  hands the keyboard to WebKit as AppKit does for any view that accepts it. One local
  `NSEvent` monitor per process sees clicks (on a page: the tile takes the focus; elsewhere:
  the GPUI view gets the keyboard back) and keys (⌃Tab, or a second Esc within 400 ms, give
  it back and are swallowed). On iOS, a tap on a page is seen in the clip view's hit test
  and focuses the tile, the page's fields raise the keyboard themselves, and hiding the page
  ends their editing. A hardware keyboard's ⌃Tab and double Esc reach the page there, since
  UIKit has no monitor that sees keys first.
  The GPUI view answers every key equivalent itself and never passes one to its subviews,
  so a page with the keyboard would lose ⌘C, ⌘X, ⌘V, ⌘A, ⌘Z and ⇧⌘Z to the workspace. The
  monitor takes those (`web::edit_for`, by the character the key types) and does them in the
  page: the edit actions up the responder chain from the page's first responder, undo and
  redo on the web view's own undo manager. The app e2e shows the monitor those keys on the
  page's window (`Command::PageKeys`) and reads the page's own undo, redo and select-all back
  from its title; copy, cut and paste take the same path and are left to the unit test,
  since they would touch the machine's clipboard.
  The snapshot is `takeSnapshot` → PNG → a BGRA `RenderImage` decoded off the main thread,
  drawn as the body's image. The page's title and address are read after each navigation
  and once a second while it is open, since scripts change them without navigating. App
  Transport Security allows plain http to IP addresses and single-label hosts such as
  `localhost`, which is where forwards live; any other plain-http address fails with the
  system's reason in the body and a "↻" to try again. The tests load only a page the test
  serves itself on 127.0.0.1 (`tiles.rs`, golden `browser`), and read the view back over the
  self-test socket: its title, its own URL, shown or hidden while the palette is open.

- ✅ **The second de-slop pass: one button, one left edge, one focus hairline** (2026-09-25).
  A design review of every golden, done as a product designer would do it, found the
  remaining slop in composition: things that sat a few points off from each other or were
  built four times over. What changed, worst first:
  - Buttons had four hand-written copies (the empty workspace, the connect panel, settings,
    the file bar). The secondary among them was filled with `raised`, which on the light
    canvas is one step from the canvas: "Add a window" and the phone's "Paste" were words on
    a smudge. `kit::button` is the only button now, in four kinds: `Primary` (accent fill),
    `Secondary` (panel with the hairline, so it holds its edge on any surface), `Ghost` (text
    until hovered: Cancel) and `Link` (accent text with no pad, so its words start on the edge
    of the text above: the other way in, Open in editor). Every kind wears a 1 pt border,
    clear on a ghost or a link, so they stand the same height side by side and keyboard focus
    moves nothing. The file bar's buttons stay its own, since they scale with the overview.
  - The column dots were centred between the bar's two flexible halves, and the bar is
    inset 78 pt on the left for the traffic lights and 12 on the right, so they stood 33 pt
    right of the window's middle. They sit on the window's centre now. The round trip sat 4 pt
    from "+" and read as its label; the readouts and the buttons are two groups a `spacing.md`
    apart.
  - A focused field wore two rings: its accent border and gpui-kit's halo outside it (the
    first run's field showed both). `kit::sync` turns gpui-kit's `focus_ring` off, so focus is
    the one accent hairline the token ruling named.
  - A selected palette or picker row was an accent tint, the loudest fill on screen after
    the primary button, for the row the arrow keys are on. It is `overlay`, the neutral step
    Zed, Linear and Raycast use; hover is `raised` and no longer paints over the selection.
    The accent keeps to focus, the primary action, links and a text selection. (Amended 2026-09-27 (design audit):
    the primary action is the accent *fill*, not this text tone.)
  - Text in the palette, the picker and settings started on three edges within 10 pt: a
    gpui-kit field pads itself (10 pt at its default size), so the typed text sat right of the
    rows below it. The field's pad is taken off and the container pads to the rows' edge
    (`spacing.lg`); in settings and a note the text area runs at gpui-kit's `Small` size
    (8 pt) and the container makes up the rest. A note's caret now starts where its rendered
    text and its header's title do, where it used to jump 10 pt right on a click.
  - The file tile's proxy was a bare rounded outline, which reads as the box a font draws for
    a missing glyph. It is a page with two lines of text on it.
  - The first run's heading was 15 pt regular over a 12 pt line: no hierarchy. The type
    scale gains `display()` (base + 7) for the heading of a page that is the whole window, and
    `Typography::STRONG_WEIGHT` (600), the one weight above regular, for headings and the
    workspace's name in the bar. The other way in is a link, where it was muted text with
    nothing to say it could be clicked.
  - The overview drew each workspace as a white panel under white tiles, so the tiles lost
    their edges and the panel read as a thick frame. It is a `raised` tray with the
    workspace's name above it, and the empty workspace at the end is labelled "New workspace".
  - "server unreachable" had a muted dot, so a failure read as metadata. It takes `warn`, as
    a worker that is down does.
  - The `add-worker` golden held the link's hover, since the test's click left the pointer on
    it; the test moves the pointer away first.
  Left as found, and why: the overview's empty workspace is still cut off by the window's
  bottom edge, because where it goes is the layout's geometry (`slopty-client`), not the
  chrome's. The editor's line numbers sit tight against the code; that gutter is gpui-kit's.
  The terminal draws its scrollbar at rest (`transfers`). The settings dialog is still the
  TOML file with Save and Cancel, the way Zed treats its own settings file.

- ✅ **The second pass's leftovers: an overlay scrollbar and an overview that fits**
  (2026-09-25). Two of the three things that pass left as found are fixed. The third is
  gpui-kit's to fix.
  - The terminal's scrollbar is an overlay, as on macOS and in Zed. It is hidden at rest, even
    with history and even with the viewport in it. It shows while the viewport scrolls (any
    frame that draws a new offset: the wheel, a key, a jump to a prompt or a hit), while the
    pointer is within two cells of the grid's right edge and while the thumb is held. Once the
    last of those ends it stays for Zed's second and fades out over Zed's 400 ms. Under Reduce
    Motion it goes at once when that second ends. The old rule (history and the pointer
    anywhere over the card) kept a bar on every terminal the pointer rested on, which is what
    `transfers` showed after a drop. The pointer's way to the edge and away from it is
    watched on the window, so leaving the card also counts as leaving the edge.
    `terminal::scrollbar::Visibility` is the pure rule. The element asks for frames only while
    the fade runs, and a timer wakes the view once at the end of the linger. Tests:
    `the_bar_shows_while_used_then_lingers_and_fades` (the rule, on a synthetic clock) and
    `the_scrollbar_drags_and_pages_the_viewport` (a headless window on the test clock).
  - The overview zoomed to niri's 0.5 and centred the active workspace, so with one workspace
    and the empty one after it the empty one hung off the window's foot. It now zooms to fit
    the whole stack: each workspace, the gap above it where its name goes, and a gap of
    margin at either end, so the empty tray has the same air below it as the first name has
    above it. That is 0.5 while it fits, 1/2.4 for two workspaces and 1/3.5 for three, and
    never below 0.25. Past that the stack scrolls with the active workspace, which stays
    centred as in niri, but neither end comes further in than a gap. A count that changes
    while the overview is open springs the zoom to the new fit on the overview's spring
    rather than jumping. Navigation is niri's as before: the same step between workspaces,
    the same gesture travel, the same drops. With every workspace in view, a vertical swipe
    in the overview moves the focus and not the stack. Out of the overview the hold never
    binds. Tests in `slopty-client` `layout::tests`:
    `the_overview_fits_every_workspace_with_a_gap_around_each` and
    `a_tall_overview_scrolls_with_the_active_workspace_and_refits_smoothly`.
  - The editor's line numbers sit 6 pt from the code because gpui-kit hard-codes that gap
    (`LINE_NUMBER_RIGHT_MARGIN` in `crates/base/src/input/base/element.rs`, added to the
    width of the digits in `layout_line_numbers`). Neither `EditorState` nor `Editor` has a
    setting for it. Folding would widen the gutter by the fold icons' 18 pt, but that adds
    folding, which the file tile turns off. The fix is a gutter-padding option in the fork.
  Goldens rerendered: `overview`, `transfers`.

- ✅ **The overlay scrollbar goes when the pointer leaves without a move** (2026-09-25). The
  bar stayed up when the pointer left the grid's right edge without a move over the window: out
  of the window, to another app with ⌘-tab, or by the tile moving under a still pointer. The
  element now lets it go on the window's mouse exit, when the window goes inactive (an edge,
  kept in element state, so hovering an inactive window still shows it), and whenever prepaint
  finds the last pointer position is no longer near the edge as laid out now. Only a real move
  brings it up. Test: `the_scrollbar_lets_go_when_the_pointer_leaves_without_a_move`. The test
  platform reports the pointer at the origin after activation changes, so the inactive case
  passes there without the edge; that clause is checked by reading. The opacity still asks
  AppKit for Reduce Motion on every prepaint of a terminal with history. Caching it belongs in
  `TerminalView::scrollbar_opacity` and is left for whoever owns `terminal/view.rs`.

- ✅ **The overview's gap always holds the workspace's name** (2026-09-25). The name above a
  workspace is `spacing.xl` (24 pt) tall, but the gap was 10 % of the height at the overview's
  zoom: 17 pt for three workspaces in an 800 pt window, less on a phone on its side, so names
  overlapped the workspace above. `LayoutConfig::overview_label` is the band a gap must hold,
  set by the strip from the same token the label is drawn with. In the overview a gap is the
  larger of the share and the band (scaled in with the overview's progress, so the zoom
  animation stays continuous), and `overview_fit` takes the smaller zoom of the two that fill
  the height, so a stack that fits still shows whole. With room to spare nothing changes. Test:
  `the_overview_gap_always_fits_the_workspace_name`.

- ✅ **The column dots keep clear of the name and centre on the safe area** (2026-09-25). The
  dots were an overlay centred on the window, so on a narrow window they covered the workspace
  name, and on a phone on its side they ignored the notch. `dots_at` now places them on the
  middle of the safe area. It measures the name, the status texts and the right side (round
  trip, who needs you, "+" and "…") with the text system and moves the dots aside as far as it
  takes to clear both. With no room between the two the dots are not drawn. Tests:
  `the_dots_centre_on_the_safe_area`, `the_dots_give_way_to_the_name_and_the_buttons`.

- ✅ **The chrome gets a frame, a status vocabulary and icons** (2026-09-25). The user found the
  app sparse, not minimal. They pointed at soloterm, Orca, diri, otty, Warp, Zed and delta as
  apps that are minimal yet complete. Read from their source and styles, those apps share a
  frame that Slopty lacked. A navigator lists hosts and what runs on them, sorted by attention.
  A status bar says where the focused thing runs and how the link is. Each row carries a fixed
  status slot with one icon and one tone per state. Pane headers name the kind, the title and
  the place. Empty and failed states say what to do next. Slopty had only a title bar with a
  name and a frame time, and tiles headed by a bare word. This ruling amends the de-slop rule
  that chrome carries no decorative glyph. An icon is not decoration when it names a kind, a
  state or an action. No emoji, and still no glyph without a meaning.

  *Icons.* Tabler's glyphs drawn by us on whole pixels, and Material's icons for files (amended
  2026-10-07, "The chrome's icons are Tabler's, and a file's are Material's"; before, SF
  Symbols, and before that Hugeicons at 1.75). Sizes
  come from the type scale: `Typography::icon()` is base + 1 beside text and `icon_large()` is
  base + 3 standing alone. An icon takes the colour of the text it sits beside. Zed's icon crate
  is GPL, so none of its files are used.

  *Status.* `icons::Status` is the one vocabulary: `Idle`, `Working`, `NeedsYou`, `Done`,
  `Failed` and `Away`. Each has one icon and one tone (muted, accent, warn, success, error,
  muted). `status_mark` draws it in a fixed square slot, so titles line up whether or not a row
  has a mark. The tile header, the navigator, the palette and toasts all use it. The agent pill
  keeps its words (`allow? Bash`) beside the mark.

  The navigator does not bring back a host switcher: every worker still shows in one workspace
  (Workspace, "The layout is this device's"). It lists what is there and flies to it.

  *Frame (desktop).* From the top down:
  - The title bar holds the navigator toggle, then the workspace name as a button with a
    chevron. The column dots stay. The right side holds the needs-you chip, a bell with a count,
    then `+` and `…` as Lucide icons.
  - A navigator on the left: 248 pt by default, resizable from 200 to 400 by a 6 pt handle,
    toggled with ⌘B, its width and visibility kept with the layout. Three sections, in this
    order:
    - *Needs you* appears only when something waits on the human.
    - *Workers*: one 28 pt row each, holding the status slot (connected is success, away is
      warn, forgotten is muted), the name, and the round trip in caption type on the right. A
      row discloses its tiles, each with a kind icon, a title and a status mark. Clicking one
      flies the camera there.
    - *Workspaces*: the name and a count of tiles.
  - The strip.
  - A 26 pt status bar. On the left: the focused tile's worker, then its working directory's
    tail. On the right: active transfers, the round trip, the frame time (moved down from the
    title bar), and an agent summary (`2 working · 1 needs you`) that jumps to the next one
    needing the human.

  On an iPad the navigator is an overlay and on a phone a drawer. On a phone the status bar
  keeps only the readouts that fit.

  *Tile header (28 pt).* From left to right:
  - the kind icon (terminal, agent, window, display, browser, file, note);
  - the title at base size in primary text (muted only when unfocused);
  - the working directory's tail in muted text;
  - on the right, a worker chip when more than one worker is connected, then the status mark and
    pill, then an unseen dot;
  - close and split as icon buttons that appear on hover.

  The focused tile's header shares its body's surface. An unfocused header steps up to
  `panel`, so the focused tile reads as one piece.

  *In-body states.* A pill anchored at the bottom of the body says what is wrong and what to do:
  `Reconnecting…`, `N lines below · Back to live`, `Exited · code N` with Restart and Close. A
  dialog is never used for these.

  *Empty and failed states.* An empty workspace shows a large muted icon and a noun-phrase
  title. Below them come the three ways to begin, each with its key cap, then the recent
  workers. The palette groups its rows into sections (Tiles, Workers, Commands), with a kind
  icon on each row and the worker named where there is more than one.

  *Toasts.* Bottom right, up to 400 pt wide. Each has an icon, a line of text and at most one
  action. They stay 6 s, and no more than two are shown. The bell's inbox keeps what they said
  (Needs you, Finished, All).

  Every size, colour and gap still comes from the tokens, and the lint-as-tests in
  `kit.rs` enforce them. Chrome text stays sentence case, and keybindings stay in the palette
  and in key caps, not on buttons. Tests: `icons` unit tests (every listed icon loads and the
  component bundle still does; every status has its own embedded icon), plus the tests each part
  lands with. The app goldens are re-accepted for this ruling.

  *Shipped: tiles.*
  - **Header.** It is 28 pt. The kind icon is also a Mac's drag-out handle on a file. The place
    is a shell's directory tail from `SessionSummary.cwd` or a page's address. The client is
    never told the worker's home, so `cwd_tail` recognises a home by its shape.
  - **Status mark.** It is chosen in this order:
    1. the agent;
    2. `Away` while the worker's link is down;
    3. a finished command not yet watched;
    4. an exited session;
    5. the newest prompt's exit mark. That is one lookup in the prompt index, not a scan of
       the rows.

    The agent pill dropped its dot, since the mark beside it carries the tone. A failed
    finished command is now `error`, not `warn`.
  - **Controls.** Fullscreen and close are icon buttons. They show on header hover and always
    on the focused tile.
  - **Body pills.** A pill at the foot of the body says `Reconnecting…` (or that the worker is
    unreachable or gone), `Exited · code N` with Restart and Close, or `Session ended` with
    Close. Restart reruns the session's command in its cwd on the same worker. The Exited pill
    shows only for a round trip today, because the client closes an exited session at once.
  - **Toasts.** They stack in the strip's corner, two at most, 6 s each, with Go or Undo. (Moved
    into the status bar on 2026-10-02: "What the showcase showed".)

  The "lines below · back to live" pill came later, in `terminal.md`'s "A view scrolled up
  holds still, and a pill counts the lines below" (2026-09-25). Tests
  (`workspace/tests/tiles.rs`):
  `a_place_is_its_last_two_directories_with_home_as_a_tilde`,
  `a_shell_header_names_its_directory`, `the_worker_chip_shows_only_beside_another_worker`,
  `a_focused_header_shares_its_body_surface`,
  `the_status_mark_follows_the_agent_the_last_exit_and_the_link`,
  `the_header_controls_close_and_fullscreen_their_tile`,
  `a_tile_whose_worker_dropped_says_reconnecting_at_its_foot`,
  `an_exited_shell_offers_restart_and_close`, `notices_stack_two_in_the_corner_and_go`,
  `the_closed_notice_takes_the_tile_back`.

  *Shipped: the frame.*
  - **Navigator** (`workspace/navigator.rs`). It docks at 248 pt and drags from 200 to 400 pt
    by a 6 pt handle. ⌘B or the toggle after the traffic lights shows and hides it. Its width
    and whether it docks are saved in `layout.json` as `Saved::navigator`, and restore clamps
    the width. Where docking would shrink the strip to a phone's width, it opens over the strip
    instead: on an iPad as an overlay, on a phone as a drawer. Either closes once a row is
    chosen, and neither changes the saved setting. Native web pages hide under it.
  - **Worker rows.** A worker row folds its tiles. A tile or workspace row flies the camera
    there. An away worker is marked `Away` in `warn`, the tone the token table already gives
    reconnecting.
  - **Status bar** (`statusbar.rs`). It shows the focused tile's worker and directory tail,
    uploads in flight, the round trip, the probe's median draw time (recomputed at most once a
    second), and the agent summary, which runs next-attention. A phone keeps the worker, the
    round trip and the agents.
  - **Title bar.** `+` and `…` are Lucide icon buttons, and the workspace name is a button with
    a chevron. The bell counts waiting agents and unwatched finished commands, and opens
    `inbox.rs` with *Needs you* and *Finished*. The inbox has no *All* tab yet, because toasts
    keep no history. The column dots keep clear of the new left-hand items.

  Tests (`workspace/tests/frame.rs`):
  `the_navigator_docks_only_where_the_strip_keeps_its_room`,
  `cmd_b_hides_and_shows_the_navigator_and_the_layout_keeps_it`,
  `dragging_the_handle_resizes_the_navigator_within_its_clamps`,
  `the_navigator_lists_what_needs_you_then_the_workers_then_the_workspaces`,
  `on_a_phone_the_navigator_is_a_drawer_that_closes_on_a_choice`,
  `a_tile_row_focuses_its_tile`, `the_status_bar_reads_the_focused_tile_and_its_link`,
  `the_bell_counts_the_inbox_and_its_rows_go_there`,
  `the_column_dots_keep_clear_of_the_toggle_and_the_name`; `statusbar`
  `the_readouts_say_what_they_count`; client `save_and_restore_round_trip_through_json`,
  `restore_cleans_up_whatever_it_is_given`.

  *Shipped: the palette and the empty states.*
  - **Palette.** Its lines group under small muted headings in the order Tiles, Workers,
    Commands, then Files. Files are the paths a worker found, and they lead when the field
    spells a path. A heading shows only when two or more groups are visible. Each line has a
    kind icon in a fixed slot, muted, and in the text colour on the chosen line: a column of
    fifty icons in full colour was louder than the words. On the right come the worker's name
    when there is more than one worker, then the status mark and the keys. The main palette now
    lists the workers too.
  - **Window picker.** ⌘O opens it at once with the sessions and a row that says the worker is
    being asked for its windows. The listing fills that same picker. It groups rows under
    Sessions, Displays and Windows and says `Nothing matches` when a filter empties it.
  - **Empty workspace** (*amended 2026-10-01*: it asks for work instead, see "The empty
    workspace asks what an agent should do" below). It shows a large muted grid icon, `Empty workspace`, and three rows
    with icons and key caps read from the bindings (only the first accented). Below them come
    the workers with their marks, each opening a shell. With no worker it says `No workers yet`
    and where one comes from.
  - **Overview.** Workspace names are small strong text with a muted tile count, at one size
    at every zoom. The new-workspace zone has a plus and its name.

  Tests: palette `lines_group_into_sections_in_a_fixed_order`, `every_line_icon_is_embedded`;
  picker `the_picker_waits_for_the_listing_and_says_when_nothing_is_left`;
  `workspace/tests/palette.rs` `the_palette_lists_tiles_then_workers_then_commands`,
  `the_icon_slot_keeps_every_title_on_one_edge`,
  `the_empty_workspace_begins_a_terminal_an_agent_or_a_window`,
  `cmd_o_shows_the_picker_while_the_worker_lists_its_windows`,
  `the_overview_labels_keep_their_size_at_any_zoom`. The new chrome strings joined the
  sentence-case lint.

  The gallery renders at 900 × 600, where docking would leave the strip under
  `phone_below`, so there the navigator stays closed. `a_workspace_of_columns_in_both_themes`
  now widens the window to 1280 × 800 for one more golden, `workspace-navigator`, with the
  navigator docked. Every app golden was re-accepted for this ruling
  (`cargo xtask e2e app --accept-all`, 22 of 22 passing).

- ✅ **Panes sit flush, divided by hairlines, like Warp's splits on niri's strip** (2026-09-25).
  The user still found the app unattractive. They asked for Warp and niri together: no space
  and no rounded corners between panes, one divider between each pair, modern and minimal the
  way Warp is. Tiles had been cards: 8 pt gaps, `radii.md` corners, a hairline frame each, an
  accent ring on the focused one, and canvas showing between them. That reads as a canvas of
  cards, not one working surface. The strip is now one surface:
  - **Layout.** `LayoutConfig::gaps` is 0. Columns and the tiles stacked in a column touch, and
    the strip meets the title bar, the navigator and the status bar edge to edge. niri's
    scrolling, the presets and the phone's struts are unchanged. A peeking neighbour is simply
    cut by the window's edge.
  - **Dividers.** One `border` hairline between neighbouring columns and between stacked tiles.
    A tile draws the line on its right and bottom edges only where another tile continues, so
    nothing doubles. No tile has a frame of its own and no corner is rounded, in the strip or
    in fullscreen.
  - **Focus.** Warp shows the active pane by leaving it alone and quieting the rest. Every
    unfocused tile's body lies under the canvas colour at `alpha::FAINT`, and its title is
    muted. The focused tile has no ring. A single visible tile is never veiled.
  - **Attention.** A tile that needs the human gets a 2 pt `warn` bar along the top of its
    header in place of the old outline.
  - **Headers.** Every header, focused or not, is on the body's surface with a hairline under
    it. Focus comes from the veil, so the header surface rule of the frame entry is withdrawn.
  - **Resize and drop.** The handle straddles the divider: a 6 pt hit area centred on the line,
    which turns accent on hover and while dragging. A drop between columns draws a 2 pt accent
    line on the divider it will open. A drop into a column washes that column with no corner.
  - **Overview.** Each workspace is one flush block of its panes inside a single hairline
    frame, with no radius. The new-workspace zone keeps its dashed outline, square.

  Rejected: keeping the focus ring with flush panes. On shared edges it doubles the divider and
  sits half on the neighbour. Also rejected: an opacity dim of the focused pane's neighbours.
  Opacity dims video and terminal glyphs unevenly, which is the same reason Orca mixes colours
  for sleep rather than fading them.

- ✅ **Health is silent when it is good; lists sort by attention; the working mark steps**
  (2026-09-25). Orca's source puts it plainly: a healthy host is the silent default. A
  connected worker now shows nothing but its round trip, in caption type, in the navigator,
  the palette and the empty workspace. A worker that is down shows a short word and a mark in
  a status lane on the right: `connecting` with the working mark, or `silent`, `reconnecting`,
  `unreachable` or `gone` with `Away`. The status bar says the same when the focused tile's
  worker is down.
  - **Order.** A worker's tile rows come in order of attention: needs you, then done, failed or
    unseen, then working, then idle, each class in reading order. A tile row whose long
    command finished unwatched carries an accent unseen dot on its lane. The dot hides while
    the tile works or needs you.
  - **Marks.** `status_mark(theme, status, k)` takes the chrome's zoom and is an Image named by
    its status.
  - **Spinner.** The working mark turns in 12 steps a second, driven by one app-wide clock. A
    timer wakes only the views that painted a mark, so nothing repaints once none shows, and
    the mark stands still under Reduce Motion (amended 2026-10-03: it stays upright and
    breathes in opacity instead; see "A working mark breathes under Reduce Motion" at the end).
    Headless at a simulated 120 Hz with one agent
    working, the workspace rendered 12 frames a second against 120 for a per-frame animation,
    and 0 at rest (MEASUREMENTS, "a working mark that steps").
  - **Palette and first run.** The palette gains a foot legend in key caps, and `kit::key_cap`
    is now the one key-cap element. The first-run page shows a large muted Server icon above
    its heading.

  Tests: icons `the_working_mark_steps_twelve_times_a_turn_and_stands_under_reduce_motion`,
  `a_status_mark_is_an_image_named_by_its_status`,
  `a_working_mark_wakes_its_view_only_while_it_shows`; frame
  `a_healthy_worker_says_nothing_and_a_lost_one_says_what_is_wrong`,
  `a_workers_tiles_come_in_order_of_attention`,
  `an_unseen_dot_marks_a_finished_tile_until_it_is_looked_at`,
  `a_working_mark_draws_twelve_frames_a_second_and_none_at_rest`; palette
  `the_palette_foot_names_its_keys`.

  *Shipped: the flush layout.* The gap is gone from the model: `LayoutConfig::gaps` and
  `Geom::gaps` are deleted, not set to 0. A proportional column is the working width times its
  preset. Columns start where the previous one ends, and stacked tiles fill their column from
  its top.
  - **niri.** `compute_new_view_offset` lost its padding, so a column that must move lands
    flush on the nearer edge of the view. The presets, the phone's struts, fullscreen, the
    swipe snaps and the overview's labelled gaps behave as before.
  - **Handle and drops.** Each column divider carries a 6 pt handle centred on its line. The
    handle's 1 pt accent line lies over the divider on hover and while its column is dragged.
    A drop that opens a column draws its 2 pt line on that divider, and a join washes the
    column with no corner.
  - **Overview.** A workspace is one square hairline frame laid 1 pt outside its block of panes.

  Tests: client `a_peeking_neighbour_is_cut_by_the_window_edge`, and every layout test re-derived
  so that columns and stacked tiles abut. UI (`workspace/tests/strip_marks.rs`, named so it
  does not shadow `workspace::strip`):
  - `the_handle_straddles_the_divider_and_resizes_the_column`
  - `the_drop_line_sits_on_the_divider_and_a_join_washes_the_column`
  - `the_overview_draws_no_rounded_quads`
  - `a_healthy_worker_shows_no_word_in_the_empty_workspace`

  *Shipped: flush tiles.* A tile has no corner, no frame and no ring, in the strip, fullscreen
  and its closing fade.
  - **Dividers.** Its dividers come from its layout position (two lengths, no walk over the
    tiles). The strip draws them in a layer above every tile. The first build had each tile
    draw its own lines, and at the overview's fractional zoom the next column's edge rounded
    to the same pixel and painted over the line between the second and third columns.
  - **Veil.** It is a separate quad over the body, so cached bodies stay cached. A tile alone
    in its workspace or fullscreen is never veiled.
  - **Status mark.** The header uses the shared `status_mark`, and the status bar names the
    place with the header's `cwd_tail`.
  - **Scrolled back.** A terminal scrolled into its history shows `N lines below · Back to
    live` at the foot of its body, and a click returns to the bottom. Showing it exposed a bug:
    a scrolled-up view drifted with incoming output, because its offset counts lines up from
    the bottom. It now keeps its top line fixed. The rule lives in the client's `TermState`,
    so `slopty attach` holds still too (test `scrolled_up_the_view_holds_its_lines_as_output_arrives`). The pill costs about 15 µs at p50 per frame
    (MEASUREMENTS, "the lines-below pill").
  - **Exited shells.** An exited shell keeps its tile and its `Exited · code N` pill until
    Close or Restart (terminal.md).

  Tests (`workspace/tests/tiles.rs`): `tiles_have_no_frame_or_corner_and_headers_sit_on_their_bodies`,
  `a_divider_runs_only_between_neighbours`, `unfocused_bodies_are_veiled_and_a_lone_tile_is_not`,
  `a_tile_that_needs_you_has_a_warn_bar_on_its_header`, `an_exited_shell_stays_until_it_is_closed`,
  `the_exited_pill_takes_the_place_of_the_lines_below`; view
  `scrolled_up_the_pill_counts_the_lines_below_and_goes_back_to_live`,
  `the_lines_below_give_way_to_the_tiles_pill`. Every app golden was re-accepted for this
  ruling, 22 of 22 passing. The blank-frame check in the terminal test now asks for 0.4% of
  pixels off the corner's colour, not 1%. With panes flush on a surface a shade from the title
  bar's, only text and chrome differ from it.

- ✅ **On a phone and in Split View, panes meet the edges and the chrome fits the workspace**
  (2026-09-25). A pass over the iOS goldens after the flush-panes ruling found the phone still
  wearing the card layout's habits:
  - **No struts.** A phone's column showed 12 pt of each neighbour. With every pane flush, the
    slivers read as stray panes at the edges. `phone_peek` is 0, as niri's `struts` default:
    the column meets both edges, and a neighbour is a swipe away. The column dots still say
    there is one.
  - **Safe area.** The band under the key bar or the status bar, over the home indicator, is
    `panel` like the bar above it, not canvas. It continues the bar instead of showing a strip
    of a different surface.
  - **Overlays and the keyboard.** The backdrop is a column, and the dialog in it is `min_h_0`.
    With a phone's keyboard and key bar up, the palette gives up height: its list scrolls and
    its field and its foot stay in view.
  - **Frame width.** The title bar, the status bar and the navigator measure the workspace's
    width, not the window's. An iPad in Split View gives the workspace less than the window,
    and the column dots had sat past its edge. The difference is measured after layout and a
    resize is seen in the same frame, so the strip never lays out for a phone's width in
    passing.
  - **↩ as text.** A phone's font fallback drew ↩ as its emoji, a blue tile among plain keys.
    The drawn text carries the text-presentation selector. What a screen reader gets keeps the
    bare key.
  - **Navigator lane.** A healthy worker's row reserves no status lane, so its round trip ends
    on the edge where the workspaces' counts end.
  - **No chords without a keyboard.** With no hardware keyboard attached, the palette prints no
    action's chord and no key legend, since nothing there can be pressed. A worker's readout
    and a file's kind stay. The app forwards the attach and detach it already watches for the
    key bar.
  - **Tolerance.** The iOS goldens allow 0.3% of pixels to differ, down from 1%. Runs differ
    by 0.06% at most, and at 1% an iPad frame, mostly canvas, passed with its whole chrome
    redrawn (0.84%).
  - **Channel slack.** A pixel matches when no channel is more than 4 off, down from 24. The
    safe-area band moved from `canvas` to `panel`, 11 apart, and the navigator golden, taken
    before the move, still passed. At 4 the widest run-to-run difference is 0.3% of a Mac
    frame and 0.1% of a simulator frame, except `transfers` at 0.56%: its ports and readouts.
  - **Stable roots.** An e2e stack's temp dir is named for its test, not at random. Goldens
    draw paths under it, and `transfers` drew one five times, enough to fail at 1.18% on a
    run whose name happened to render wide.

  Tests: client `on_a_phone_a_column_meets_both_edges`, and the struts test keeps its case with
  `phone_peek: 12.0`. UI (`workspace/tests/frame.rs`): `the_frame_fits_the_workspace_not_the_window`,
  `the_round_trip_and_the_counts_share_the_right_edge`,
  `narrowing_the_window_never_lays_the_strip_out_as_a_phone`; palette
  `the_palette_fits_above_a_phone_keyboard`, `without_a_keyboard_the_palette_prints_no_chords`.
  e2e `ios` gains a navigator golden on each device.

- ✅ **Attention is said once per place, and a tile's content sits 12 pt in** (2026-09-25). A
  design pass over the Mac goldens against Warp found two things.
  - **One count.** A waiting agent showed four times: the warn bar and pill on its tile, a
    `1 needs you` chip in the title bar, the bell's badge, and `1 needs you` in the status bar.
    The chip is gone. The bell counts what waits for the human and what finished unwatched,
    and opens the list. The status bar names the agents and goes to the next one waiting, as
    the chip did. The tile marks the agent itself. The frame entry's "needs-you chip" is
    withdrawn.
  - **Inset.** A terminal's text, a note's and a file's started 8 pt from the divider, level
    with the header's icon. With panes flush, text that close to the hairline reads as
    cramped; Warp leaves about twice that. `Spacing::inset()` (12) is now the one edge for the
    header, the grid and both text bodies. A remote window and a browser page stay full-bleed.

  - **Words.** The agent and finished pills were lowercase fragments: `claude`, `allow? Bash`,
    `asking: a question`, `done 3.2 s`. They read as sentences now: `Idle`, `Allow Bash?`,
    `Has a question`, `Done · 3.2 s`, `Exit 1 · 1 m 04 s`, in the shape of `Exited · code N`.
  - **A worker coming up keeps its hands off the keys.** A worker with nothing on it is given
    a shell, and that shell took the focus. When a second worker came up (at launch, on a
    reconnect, or added by an agent), keys meant for the first machine went to it. The given
    shell still opens beside the rest, and the focus goes back where it was. With nothing
    focused yet it takes the focus as before. The two-machine e2e caught this against a real
    MacBook over Tailscale.

  Tests: the UI tests that looked for the chip look for the status bar's summary
  (`status-agents`); the e2e helpers already matched any button ending in "needs you".
  `a_new_workers_shell_opens_beside_without_taking_the_focus`. Every golden was re-accepted.

- ✅ **Upstream sync of 2026-09-25, second round: a double-click word follows a soft wrap**
  (2026-09-25). zed rebased onto main `397cbc84de` (4 commits, fork head `a5ceabe751`),
  gpui-kit onto main `d565fd57` (5 commits, fork head `6a552431`), and `vendor/ghostty` moved
  16 commits to ghostty main `982fe90d9`. The libghostty-rs fork pins the same commit at
  `137d62a`. The C header did not change, so the regenerated bindings are byte-identical.
  Every fork replayed without a conflict. Stable 1.98.1, nightly 2026-09-24 and every gate
  tool were already current; `cargo update` moved eleven semver-compatible patches (`cc`,
  `smallvec`, `siphasher`, the `wasm-bindgen` family).
  - **zed** touched no `gpui` crate. Its one terminal change rounds the background of Zed's
    own terminal view to fit a rounded card. Slopty's tiles are square by design (the tiles
    test asserts it), so the full-rect background stays.
  - **gpui-kit** brought a redesigned `Attachment`, a bottom dock that drags shut and back
    open in one motion, chart tooltip builders and a large docs pass. Slopty uses none of
    these components, so nothing changes here. The dock drag is worth a look if a bottom
    panel ever appears.
  - **ghostty** changed the VT library in two places that matter. Runs of DEC special
    graphics (the line-drawing set that `tmux`, `mc` and dialog boxes draw borders with) are
    now written in batches, which comes for free. Word selection stops at a hard line break
    and still crosses a soft wrap. Slopty's word selection never left its row, so it already
    stopped at a hard break, but it split a wrapped word in two. It now follows Ghostty's
    rule. A double-click on either half of a word wrapped over the right edge takes the whole
    word, and a word that ends at the edge before a hard break stays on its row. Copying a
    run joins a soft-wrapped row to the one before with no newline and keeps the blanks at
    the wrap, so a long wrapped line pastes as the one line the program printed. A block
    selection keeps a newline per row. The glyph cache key packing, the OpenGL/EGL fix and
    the tmux viewer fix are outside what libghostty-vt builds for Slopty.
  - Test: view `a_word_follows_a_soft_wrap_but_not_a_hard_break`.

- ✅ **Upstream sync of 2026-09-26: either half of a wide character is the character**
  (2026-09-26). zed rebased onto main `933d8d9381` (7 commits, fork head `909d7b3c58`),
  gpui-kit onto main `f8cd4860` (12 commits, fork head `0e2f7dbc`), and `vendor/ghostty` moved
  28 commits to ghostty main `6301810a4`. The libghostty-rs fork pins the same commit at
  `801866f` with regenerated bindings. Stable 1.98.1 and nightly 2026-09-24 were current;
  typos went to 1.50.3; `cargo update` moved `cc` and `zerocopy`. `cocoa`, `core-video`,
  `generic-array` and `unicode-properties` stay where zed pins them.
  - **zed** touched `gpui` in one place. The headless renderer factory now returns a
    `Result`, for the Linux wgpu headless renderer. Slopty's tests go through
    `gpui_platform`'s test support and never name the factory, so nothing moved here. Our
    iOS `render_to_image` commit conflicted in `gpui_platform`'s `test-support` feature list;
    the list now names both `gpui_ios` and upstream's `gpui_wgpu`. gpui-kit's lock had to
    keep `unicode-properties` at 0.1.3, the version `gpui_web` pins exactly.
  - **gpui-kit** fixed a Markdown line with inline code standing a device pixel or two
    taller than a plain one. Notes draw Markdown with `TextView`, so a list with a code item
    is now evenly spaced with no change on our side. `TextViewState::reveal_range`, the
    `chart.grid` colour and the tree-sitter byte-offset fix touch nothing Slopty uses: the
    file tile colours through syntect behind its own `InputHighlighter`.
  - **ghostty** exposes render-state overscan and stable row ids in the C API, for a
    renderer that scrolls smoothly and caches per row. Slopty's engine never scrolls
    ghostty's viewport (clients scroll their own cached history), and a row id cannot stand
    in for comparing the line: ghostty still marks every row dirty when the viewport moves,
    and an id survives a write to its row. The bindings carry the new calls; the engine does
    not use them. CSI 8 t (a program resizing the window) reaches only ghostty's app
    runtime, not libghostty-vt, and a tile's size is the canvas's to set anyway. The reverse
    wrap fix comes with the library.
  - **Word selection** follows ghostty's fix for wide characters: a spacer resolves to the
    character that owns it, a tail to the cell before and a head to the wide character that
    wrapped onto the next row. Before this, the right half of an ideographic space
    (U+3000) read as text, so a double-click on it took the word beside it, while the left
    half took only the space.
  - Test: view `either_half_of_a_wide_character_is_the_character`.

- ✅ **Upstream sync of 2026-09-26 evening, and the editor's gutter gap** (2026-09-26). zed
  rebased onto main `70e686c2a3` (2 commits: the extension page's upsell and a Windows remote
  server build, neither in anything Slopty builds). gpui-kit rebased onto main `7afd1708` (6
  commits); `ghostty`, the toolchains and the crates were current.
  - **gpui-kit**: a focusable list paints `Role::List` on its container, a form takes its
    styled refinements, and a table's highlights stop remapping quadratically. The input
    accessibility fix (#3246) came and went: upstream reverted it (#3253) because it broke
    reverse tab traversal, so nothing changes for Slopty's fields.
  - **Our commit on the fork**: `EditorState::line_number_gap`, the space between the line
    numbers and the text, which was a fixed 6 pt. The file tile sets it to `spacing.md`, so
    the code no longer starts flush against its numbers (golden review #9). Test (fork):
    `line_number_gap_widens_the_gutter_by_its_difference`; goldens `editor*`.

- ✅ **The status bar states facts, the inbox is a mailbox, and palette rows say where**
  (2026-09-26; its status bar half superseded 2026-10-05 by "No bar along the bottom" below). The reference study found the status bar showing a debug readout and the inbox
  and palette rows with too little to tell one from another. This ruling amends the status
  bar and inbox of "The chrome gets a frame" (2026-09-25).
  - **Status bar** (`statusbar.rs`). It sits on the canvas step, like the title bar. The left
    side holds the focused tile's worker behind a server mark (`ServerOff` when its link is
    down). Next comes its directory in mono. Inside a repository that is the repository's
    name and the path within it (`slopty/crates/ui`), since the tail alone loses the name
    once the shell goes deeper. Then the branch behind a branch mark. The right side holds
    the ports forwarded here (`2 ports`, which opens the port list) and the uploads. It says
    what is wrong with the focused worker's link only when something is, and gives the round
    trip. `N workers` carries a dot in the worst link's tone only while a worker is not up,
    and opens the hosts popover. Last comes the agent summary. The frame time is a
    measurement, not a fact about the work, so it shows only with the stream stats (⌘⇧I).
    Every count, age, port and round trip is set in tabular figures (`kit::tabular`).
    Amended 2026-09-27 (design audit): the directory is in the UI face, as all chrome context is (mono is for ports
    and figures). `N workers` shows only while a worker is down; with every link up it said
    nothing, and the "…" menu's *Workers* opens the popover instead. The agent summary counts
    only the agents at work: who waits on the human is the bell's count alone.
  - **Hosts popover.** It lists each worker in a 28 pt row: the server mark or the status
    mark, the name, and the round trip or the word for what is wrong. Under the pointer the
    row shows *Connect* (only while the link is down) and *Forget* (only for a worker added
    by address). *Add a worker* sits at the foot. Connecting and forgetting are the app's, so
    the app hands the workspace those actions (`set_host_actions`). *Connect* wakes the
    worker's redial out of its backoff; *Forget* is the "…" menu's forget. (Amended 2026-09-27 (design audit): the "…"
    menu no longer lists a *Forget* row per worker; the popover's is the one place.) A row goes to the
    worker's tiles, and a click anywhere else closes the popover.
  - **Inbox** (`inbox.rs`). The inbox keeps a history of the last 200 commands that finished
    unwatched, which gives it the *All* view the 2026-09-25 entry lacked. *Unread* is what is
    still badged on a header: read state is the badge, so reading a row, looking at its tile
    and clearing its badge are one act. A later command in the same session replaces the
    earlier one's badge, so the earlier row counts as read. Each row has two lines: the
    command (or the agent's words), then the outcome, worker and directory in mono.
    (Amended 2026-09-27 (design audit): see **The design audit** below for the rows' rhythm and words.) The age
    on the right swaps for a mark-read button under the pointer. *Mark all read* clears every
    badge. An agent waiting is not marked read: answering it is what reads it. Newest first.
    With nothing unread the inbox says "You're all caught up" under an inbox mark.
  - **Palette rows.** Tiles and workers are listed by name alone. "Go to" repeated what the
    section heading already says, and it pushed the name right. The row leads with one fixed
    slot, `palette::status_slot`, which holds the kind icon at rest and the status mark once
    there is a state; the picker, the navigator and the tile headers share it. A muted second
    column gives the worker (once there are two) and the directory in mono (Amended 2026-09-27 (design audit): in
    the UI face). The session's age
    (from the worker's `started_ms`) sits right-aligned. The filter matches that column too,
    so a worker's or a directory's name finds its tiles. Section headings are `small()` in the
    strong weight, as the plan's type discipline keeps `caption()` for badges.
  - Tests (`workspace/tests/bars.rs`):
    `the_status_bar_says_where_the_shell_is_and_counts_what_is_shared`,
    `the_workers_count_opens_the_hosts_and_their_actions`, `the_inbox_reads_like_a_mailbox`,
    `a_palette_row_says_where_the_tile_is`; `statusbar` `a_place_in_a_repository_is_named_by_it`;
    `palette` `an_age_says_the_one_unit_that_matters`. Golden: `inbox`
    (`the_inbox_lists_what_waits_and_what_finished`).

- ✅ **Three surfaces in order, focus told by the header, tabs drawn as tabs** (2026-09-26).
  The reference study found no order to the surfaces. In the light theme the navigator, the
  status bar, every header and every body were the same white. In the dark theme a body sat
  between the bars and the navigator, one shade from each. It also found focus invisible. The
  veil over an unfocused body was the canvas at `alpha::FAINT`, and measured as a WCAG contrast
  against the bare body it came to 1.000 in the dark theme (`0E0F12` under 12 % of `0A0B0E`
  rounds to itself) and 1.009 in the light one. This entry reverses the flush-panes ruling's
  "Focus comes from the veil" and "every header is on the body's surface" (2026-09-25).
  - **Surface order.** The window has three steps, and they rise in both variants: the title
    bar and the status bar on `canvas`, the navigator on `panel`, tile headers and bodies on
    the content step. Zed's One themes order theirs by distance from the editor. In the dark
    one the bars are the lightest, which would put Slopty's scrim, its picture wells and its
    hover steps (`raised`, `overlay`) on the wrong side of the bars. So Slopty orders by
    elevation, as VS Code's modern themes do. The content step is `Theme::content()`, the
    terminal's background, so a grid, its body and its focused header are one surface
    whatever the settings make it.
  - **Values.** Adjacent steps are at least 1.05 apart (VS Code's dark modern is 1.05 a step,
    Zed's One 1.08 to 1.22). Test: `the_surfaces_climb_in_three_steps_bars_navigator_content`.

    | Token | Dark | Light |
    |---|---|---|
    | `canvas` | `08090B` | `EBEDF0` |
    | `panel` | `0F1115` | `F6F7F9` |
    | content (`terminal.bg`) | `16181D` | `FFFFFF` |
    | `raised` / `overlay` | `1E2127` / `272A31` | `E8EAEE` / `E2E5EA` |
    | `border` | `2A2D34` | `CFD3D9` |
    | `border_subtle` (new) | `1F2127` | `E4E6EA` |

    The dark terminal's ANSI 8 goes from `747D8D` to `7A8393` to stay above 4.5:1 on the
    lifted background. Every chrome ink still clears WCAG AA on all five surfaces; the tightest
    pairs are 4.52 in light and 4.53 in dark, both on `overlay`.
  - **Two hairlines.** `border` stays for what divides regions: between panes, under a bar,
    round a popover. `border_subtle` is the quieter line inside one region: under an
    unfocused header, under a tab row, between a list's rows and a panel's sections.
  - **Focus.** The focused tile's header is its body's surface in primary text with nothing
    under it, so the tile reads as one piece. An unfocused header steps down to `panel`, its
    title muted, with the subtle hairline over its body (Zed's tab: `tab.rs`). The veil is
    gone. To be seen it would have to reach `alpha::PRESSED`, 1.056 in dark and 1.063 in
    light, which is a 40 % dim of every other pane's text and picture. Dimming inactive panes
    was ruled out with the flush panes. Dropping it also takes one quad per unfocused tile out
    of every frame.
  - **Leading slot.** A header leads with `palette::status_slot`, the one fixed square the
    navigator, the palette and the inbox share. It shows the kind icon at rest, and the status
    mark (working, waiting, failed, done, away) once there is one. The mark no longer also
    sits on the right. On a Mac a file's slot is its drag-out proxy.
  - **Density.** The worker chip becomes muted `small()` text after a server glyph: a fact,
    not a control. The place (a shell's directory, a page's address) and the ports are set in
    the mono face at `small()`. The header's right end is one strip, at least two icon buttons
    wide (`kit::icon_button_side`). It holds the readouts at rest (the agent's pill, a
    finished command's, the unseen dot) and swaps them for fullscreen and close while the
    pointer is on the header, so nothing beside it moves. The focused tile with nothing to say
    shows its buttons at rest, for touch.
  - **Tabs.** A tabbed column's header was the shown tile's with `1/3` after its title. It is
    a tab row now, a tab per tile, at most 200 pt each. A tab holds its slot, its title and a
    close button shown on the tab's hover (always on the shown tab). The shown tab is the
    content surface with no edge under it; the rest sit on `panel` over the bar's subtle
    hairline. A press on a tab shows and focuses it and starts a move; a double-click names
    it.
  - **Figures.** `kit::tabular(el)` sets OpenType `tnum` on an element's text style, and its
    children inherit it. `kit::tabular_figures()` gives the features for a shaped run. The
    system font's figures are proportional, so a changing count, age or round trip shifted
    what followed it. The header's strip, ports and upload wear it.
  - **Motion.** `kit::fade_in(el, id, cx)` fades chrome in over `kit::FADE` (120 ms, eased
    out) the first time it is drawn, and lands at once under Reduce Motion. `kit::motion(cx)`
    reads the system setting as well as GPUI's flag, since the app never sets the latter. Only
    opacity moves, so an overlay takes the pointer and the keys from its first frame. The
    hover hint wears it; the palette, menus and inbox belong to their own files.
  - `kit::sync` gives gpui-kit's bars `canvas`, its sidebar `panel`, its active tab the
    content step, and its table rows `border_subtle`.

  Tests: theme `the_surfaces_climb_in_three_steps_bars_navigator_content`,
  `chrome_text_clears_wcag_aa` (content added); kit
  `tabular_figures_set_tnum_on_the_text_style`, `chrome_holds_still_under_reduce_motion`,
  `the_kit_theme_follows_the_tokens`; tiles
  `tiles_have_no_frame_and_focus_is_told_by_the_header`, `no_body_is_veiled`,
  `the_header_leads_with_one_slot_for_the_kind_or_the_status`,
  `the_readouts_give_way_to_the_controls_on_hover_and_nothing_moves`,
  `a_tabbed_column_draws_a_tab_per_tile`. Golden: `tabbed-column`
  (`a_tabbed_column_draws_its_tab_row`).

- ✅ **The navigator runs the window's height, and the workspaces are tabs** (2026-09-26). The
  reference study (diri, Orca, Zed's sidebar) found the navigator reading as a list of names:
  one-line rows with nothing about where a shell is or what it did, headings that looked like
  rows, and the workspaces listed twice, in a section of their own and as the title bar's
  name. This amends the frame entry of 2026-09-25 and reverses "The column dots keep clear of
  the name and centre on the safe area".
  - **Height.** The navigator runs from the window's top to its bottom, and the title bar, the
    strip and the status bar stack to its right. Its top row is the title bar's height: on a
    Mac it leaves 78 pt for the traffic lights, then holds a filter field behind a search
    icon. The title bar starts at the navigator's right edge; only with the navigator hidden
    does it keep the traffic lights' inset. On an iPad or a phone the panel lays over the whole
    frame, title bar included, and has no traffic lights to make room for.
  - **Filter.** The field keeps the tiles whose title or second line holds what was typed, in
    any case, and every tile of a worker whose name holds it. A fold hides nothing while it
    filters. With nothing left the list says `Nothing matches`; ↩ goes to the first tile
    listed, and Esc empties the field and gives the keyboard back.
  - **Tile rows.** Two lines, 40 pt. A fixed leading slot shows the kind at rest (idle
    included) and the status mark otherwise, through the shared `palette::status_slot`. The
    first line is the title, with the unseen dot's fixed slot at its end. The second, muted,
    joins the directory's tail, what the agent says (else the command running or the last one)
    and the branch with a bullet, and ends with the age from the session's start
    (`SessionSummary.started_ms`) in tabular figures, so truncation drops the branch first and
    never the age.
  - **Worker headers.** A server icon (`ServerOff` in warn while the worker is away, the
    working mark while it connects), the name in the strong weight, the count of its tiles and
    its round trip in tabular caption type on the row's right edge, where the tiles' ages end,
    or the word for what is wrong. A folded worker's rollup comes before them. Under the pointer
    the chevron and "+" (a new shell on that worker) take the readouts' place, as the tile
    headers' actions do. The readouts grow leftwards, so neither the pointer nor a fold moves
    the count or the round trip. A first build kept a fixed empty slot after the round trip
    for the actions, and the golden showed the readouts floating 34 pt short of every other
    right edge in the list. Section headings are the shared `section_heading`, on the rows'
    leading edge.
  - **Rollup** (`workspace/rollup.rs`). A group's slot shows one thing, the most urgent: the
    warn mark with a count past one while tiles wait on the human, else the working mark, else
    the unseen dot, else nothing. A tile counts once, as what it is doing.
  - **Tabs.** The navigator's *Workspaces* section is gone. The title bar holds a tab per
    workspace with something in it or a name, and the active one even when empty. A tab is
    148 pt, giving way evenly (to 64) only when the bar runs out of room, so a long name, a
    switch or a mark moves nothing. The active tab sits on `overlay` in the strong weight. Each
    ends in the rollup's slot. "+" after the tabs (`New workspace`) goes to the empty workspace
    the layout keeps last. Tabs carry no chord. The overview is still in "…" and on its key;
    clicking the name no longer opens it.
  - **Dots.** The column dots follow "+" in the bar's flow. They are beside the active tab
    while there is one workspace, and in the same place whichever tab is active, since a slot
    that followed the active tab would move the tabs after it on every switch. The measured
    centring (`dots_at`) is deleted.

  The branch comes from the summary the session opened with; following a `cd` or a checkout
  needs the terminal view's `Cwd` event to carry it, which it does not yet. Tests
  (`workspace/tests/nav_rows.rs`, `tab_strip.rs`, `frame.rs`): `the_filter_keeps_the_rows_that_match`,
  `a_tile_row_reads_its_place_its_words_its_branch_and_its_age`,
  `a_folded_worker_rolls_up_what_its_tiles_want`, `the_plus_on_a_worker_opens_a_shell_there`,
  `the_navigator_is_the_windows_height_and_the_bar_starts_at_its_edge`,
  `the_workspaces_are_tabs_in_the_title_bar`, `the_navigator_lists_what_needs_you_then_the_workers`,
  `the_column_dots_keep_clear_of_the_toggle_and_the_tabs`,
  `the_round_trips_share_the_right_edge_and_hold_still`; rollup
  `a_rollup_shows_what_waits_then_what_works_then_what_is_unseen`, `a_tile_counts_once`,
  `an_age_runs_from_the_start`, `the_second_line_skips_what_says_nothing`.

- ✅ **The navigator is a virtual list, and the phone's drawer runs to the window's foot**
  (2026-09-27, the navigator cost 6.4 ms a frame at 120 rows in debug).
  - GPUI's `list` over a `ListState`, not `uniform_list`: headings, worker rows and two-line
    tile rows differ in height. Each frame builds plain row data; only rows within 80 pt of the
    view get elements, and a row whose kind changes forgets only its own height. The share of
    a frame fell to about 1.2 ms (MEASUREMENTS, "the navigator as a virtual list").
  - A focus move scrolls its row into view once, so a list the human scrolled away stays put.
  - A shell with no directory names its worker on line 2, so the line is never a lone age.
    Amended 2026-09-27 (design audit): a row under its worker's header never repeats the worker's name. A row with
    nothing to add is one line (`kit::Row::One`), and the age ends its first line either way.
  - On a phone the drawer and its scrim draw above the workspace down to the window's bottom
    edge, through the home-indicator band; the rows stay inset by the safe area. Amended 2026-09-27 (design audit):
    so does the navigator laid over the frame on an iPad (`Mode::Overlay`).
  Tests (`workspace/tests/nav_list.rs`): `the_navigator_draws_only_the_rows_in_view`,
  `the_focused_tiles_row_scrolls_into_view`, `a_row_with_nothing_to_add_is_one_line`,
  `the_phone_drawer_runs_through_the_home_indicator_band`,
  `the_overlaid_navigator_runs_through_the_home_indicator_band`.

- ✅ **The chrome is derived from the content, and what floats is elevated** (2026-09-27, UI
  wave 2 foundation). The design review of the goldens found overlays painted on `panel`, which
  sits below the content in both variants, so the palette, menus and the inbox read as holes. It
  found warn used as a fill, a brown bar and badge in light, and touch builds with 24 pt targets.
  The monocode study found the ladder hand-picked, so a terminal background set in the settings
  left every grey around it unrelated. This entry replaces the value table of "Three surfaces in
  order"; its order and its 1.05 step stand.
  - **Ladder.** `Surfaces::derive(content)` computes the chrome from the terminal's background.
    Fills and hairlines are the content mixed toward the chrome's text at fixed shares; dark
    sinks the bars and the navigator toward black, light darkens them toward the text and
    lifts what floats toward white. The variant is the background's (`Rgb::is_light`), not the
    appearance's, and `settings::theme_for` derives again after `[colors]`.

    | Step | Dark share | Dark | Light share | Light |
    |---|---|---|---|---|
    | `canvas` | 66 % to black | `07080A` | 8 % to text | `EDEDED` |
    | `panel` | 32 % to black | `0F1014` | 3.5 % to text | `F7F7F7` |
    | content | | `16181D` | | `FFFFFF` |
    | `elevated` (new) | 3.5 % to text | `1D1F24` | 60 % to white | `FFFFFF` |
    | `raised` | 5 % | `202227` | 10 % | `E8E8E9` |
    | `border_subtle` | 6 % | `222429` | 11 % | `E6E6E6` |
    | `overlay` | 9 % | `292B2F` | 13 % | `E2E2E2` |
    | `border` | 11 % | `2D2F33` | 21 % | `D0D0D0` |

    Mixing from white loses the light theme's cool tint; a tinted background keeps its own
    (solarized light's cream survives in its bars). Amended 2026-09-27 (design direction): new
    shares put the bars one notch under the content; the table there replaces this one.
  - **Text is lifted until it reads.** At these steps dark `text_muted` read 4.48 on `overlay`
    and light `accent`, `error`, `success` and `text_muted` about 4.40. Each text tone now moves
    toward white (dark) or black (light) only as far as WCAG AA on all six surfaces text lands
    on (canvas, panel, content, elevated, raised, overlay), and each text level keeps 1.25 times
    the worst-case contrast of the one under it, so muted and secondary never merge. The
    defaults move at most three steps a channel; the tightest pairs are 4.53 dark
    (`text_muted`) and 4.50 light (`error`), both on `overlay`.
  - **Supported range.** A background from black to a relative luminance of 0.05, or from 0.6
    to white, clears AA with three distinct levels: black, Catppuccin, Dracula, Solarized, Nord
    and a `3F3F3F` grey in dark; One Light, Solarized, Gruvbox, Catppuccin Latte and a `CCCCCC`
    grey in light. A mid grey cannot: no text colour reads 4.5:1 on it and a step above it at
    once, so the tones stop at white or black.
  - **Elevation.** Everything that floats goes through `kit::elevate`: the `elevated` surface,
    the `border` hairline and the one shadow of `Elevation`, a 1 pt contact layer (blur 2) and a
    4 pt soft one (blur 12), black at 0.4 and 0.5 in dark and 0.06 and 0.12 in light.
    `kit::scrim` dims under a modal, black at `alpha::SCRIM` in dark and `alpha::TINT` in light.
    gpui-kit's popover colour is `elevated`. A lint-as-test in `kit.rs` fails on a shadow or a
    scrim chosen anywhere else; the files that still lift by hand are listed there until wave 2
    phase B moves them.
  - **Fills.** `warn`, `success`, `error` and `accent` stay text tones. Bars, badges, dots and
    washes take `warn_fill`, `success_fill`, `error_fill` and `accent_fill`, with `fill_fg` for a
    count on them: dark `F5B83D`, `34C759`, `F0555F`, `6AA1FF`; light `F0A000`, `2DA44E`,
    `EF4B52`, `3B82F6`. Each is saturated and mid-light, so the light warn is amber where its
    text tone is ochre. A count reads on each at 5.35 or better, and in dark each stands 3:1 off
    every surface. Light amber on white is 2.16: it is seen by its hue, and it never carries
    words alone.
  - **Density.** `Density::COMPACT` (rows 28 and 40, headers 28, targets 24) and
    `Density::TOUCH` (44, 56, 44, 44, Apple's minimum) live on the theme. `kit::row`,
    `kit::Row::height` and `kit::icon_button_side` read it; the icon stays its size and only the
    target grows. The Mac default leaves every size as it was.
  - **Type and edge.** `Typography::meta()` (base − 2, 11 pt) is the step for second lines, bar
    readouts and status words (`kit::meta`). `kit::label` is the quiet section label: `small()`,
    `text_muted`, regular weight, never upper case. `Spacing::inset()` (12 pt) is the one edge
    grid for every panel's leading edge (`kit::inset_x`).

  Tests: theme `chrome_text_clears_wcag_aa`, `the_ladder_is_monotonic`,
  `a_mid_grey_background_cannot_clear_aa`, `the_default_tones_are_lifted_by_a_rounding_step_at_most`,
  `the_chrome_follows_the_terminals_background`, `status_fills_read_as_their_hue`,
  `elevation_and_density`; kit `a_floating_layer_wears_the_one_elevation`,
  `density_sizes_the_targets_not_the_icons`, `the_elevation_is_two_layers_of_the_shade`; app
  `a_custom_background_carries_the_chrome`.

- ✅ **The palette lists where to go, then what was run last** (2026-09-27, UI wave 2 overlays).
  The design review found the palette 3 pt off the status bar at 600 pt, an empty field that
  listed every command, an icon on every command row, and a right edge that mixed an age, a
  lowercase kind and a round trip.
  - On a desktop it hangs a fifth of the way down the window and takes at most three fifths
    of its height, under the list ceiling of 520 pt; it is shorter when it lists less.
  - An empty field lists the tiles, the workers and five commands: the ones last run from
    the palette (app-wide, newest first), then the first bound actions. Typing lists all of it.
  - Commands carry no icon. The leading slot is for tiles and workers, and a status takes it.
    Amended 2026-09-27 (design audit): a command keeps the slot, empty, so every title in the list starts on one
    left edge; two edges read as misalignment, not as a grouping.
    A tile's right edge says its status word ("Working", "Needs you") while it is not idle,
    in the status tone, else its age past a minute. A worker's says its state in sentence
    case, else its round trip. The kind ("Note", "File") sits in the muted context column.
  - Chords are plain `text_muted` glyphs, as in Zed. Key caps are for the foot's legend only.
  - The Tiles section lists every tile the navigator lists, unnamed notes included, by the
    title its header shows.
  - On a phone (narrower than `phone_below`) the palette is a sheet from the top at the
    window's width, down to the keyboard. A fade over the list's foot shows while rows run on
    below, because a touch list has no scrollbar. Opening the palette puts the drawer or the
    overlaid navigator away. Amended 2026-09-27 (design audit): the desktop list wears the same fade, since the row
    its height cut ended on the foot's hairline and read as broken. The palette and the
    pickers dim nothing (`kit::anchor`); the scrim is for the modals (`kit::backdrop`).
  - Empty states come in tiers. A filter that leaves nothing is one quiet line on the edge
    grid ("No command matches", the picker's "Nothing matches", an inbox emptied by reading).
    A list with nothing to hold at all says why: the picker's "Nothing on the canvas…", and an
    inbox that has never held anything names what lands there. None of them has an icon.
  - An inbox row reads down its right edge. The status word ("Needs you", "Done", "Exit 101")
    is on line one and the age on line two. (Amended 2026-09-27 (design audit): the age ends line one and no word
    repeats the section heading; an exit leads line two.) A waiting agent's age is counted from the worker's
    `since_ms`, so a reconnect keeps it. The popover stands `spacing.xs` clear of the title
    bar.
  Tests: palette `an_empty_field_lists_the_tiles_and_the_recent_commands`,
  `the_right_edge_says_status_age_or_keys_and_the_kind_is_context`; workspace
  `the_palette_hangs_at_a_fifth_and_is_a_sheet_on_a_phone`,
  `the_icon_slot_keeps_every_title_on_one_edge`, `the_palette_finds_an_unnamed_note`,
  `opening_the_palette_puts_the_phone_drawer_away`,
  `an_inbox_row_says_its_age_first_and_no_word_its_heading_does`,
  `an_empty_inbox_explains_itself_once`; picker
  `the_picker_waits_for_the_listing_and_says_when_nothing_is_left`.

- ✅ **One stacking order for what floats** (2026-09-27, UI wave 2 overlays). Each overlay
  used to pick its own `deferred` priority, or none, so a toast could fall under a dialog and
  a phone's drawer over the palette. `palette::Layer` names the order once: popover (the inbox,
  a menu, the hosts) < submenu (a menu opened from one) < dialog (the palette, the pickers, the
  settings) < toast. A panel that is part of the frame, such as the docked or drawn-out
  navigator, paints at the default priority, under all of them. The palette, the picker, the
  settings dialog and the toasts paint through it now; the title bar's menus and inbox, the
  hosts popover and a block's menu move to it with their owners' files.
  Test: palette `the_layers_stack_popover_submenu_dialog_toast`.

- ❌ **Settings open in a dialog as tall as the file** (2026-09-27, UI wave 2 overlays;
  superseded 2026-10-09 by "The settings are a page in the panes' place" below). The
  settings golden showed a 640 × 440 modal, 80 % empty, around two lines of TOML. There were
  two ways out: open `settings.toml` as a file tile, as Zed opens `settings.json`, or size the
  dialog to its content. A file tile reads and writes through a worker, but the settings file
  lives on the client, and on the phone there is no worker-side copy to open. So the dialog
  stays, and it now fits the file. The field is as tall as the file's lines plus one, from 8
  to 28 lines at the document line height (`mono_size` × `markdown_line_height`). It grows
  and shrinks as lines are typed, and gives up height to a short window or a phone's keyboard,
  scrolling inside. The Mac keeps "Open in editor".
  Test: settings_editor `the_dialog_is_as_tall_as_the_file`.

- ✅ **The first run sits a third down the height; the add-worker dialog floats** (2026-09-27,
  UI wave 2 app shell). The first-run block was pushed down by a 28 % top pad, and a
  percentage pad resolves against the width, so it sat at 42 % on the Mac and 12 % on a
  phone. Two spacers growing 1 : 2 now place it a third of the way down the height it has,
  inside the safe area, which on a phone includes the keyboard. The 16 pt mark over the
  heading read as a stray glyph and was decoration, so it is gone. Each blurb fits one line
  of a 402 pt phone. On touch the field ends in a Paste, since the phone has no ⌘V, and the
  primary action spans the panel. Over the workspace the panel is a dialog like the others:
  `kit::elevate`d on `kit::backdrop` (scrim, near the top, under the safe area), closed by
  Cancel, Esc or a click outside it. Amended 2026-09-27 (design audit): the first run's page is the content surface,
  not the canvas (in dark the near-black read as nothing having loaded); the field's example
  is the panel's own kind of host; a server the tailnet found is named in the blurb's place;
  the dialog opens at the one modal anchor.
  Tests: app `the_first_run_sits_a_third_down_on_every_device`,
  `the_dialog_closes_on_esc_or_a_click_outside`.

- ✅ **The key bar's caps hold their edges and keep their widths** (2026-09-27, UI wave 2 app
  shell). The caps were `raised` on `canvas`, a contrast of about 1.03, so they had no
  edges. They also grew to share an iPad's width, which made 100 pt "|" keys. They now sit on
  `elevated` under the `border` hairline, both bars (terminal and remote window) on `canvas`.
  The bar is a finger's 44 pt (`Density::TOUCH.hit`) whatever the chrome's density, since it
  only exists on glass. A symbol's cap is square and a word's is a small step wider each side,
  never grown: the terminal's row fits the narrowest iPad and scrolls on a phone. An end with
  keys past it fades into the bar, read from the row's scroll state after layout.
  Amended 2026-09-27 (design audit): where the row fits (an iPad) its groups spread along the bar, as a keyboard's
  do: esc, tab and the modifiers at the left, the arrows in the middle, the symbols and the
  word keys (Copy, Paste, Find) at the right. A lit key is the accent fill. Test:
  `a_wide_key_bar_spreads_its_groups`.
  Tests: app `the_key_caps_keep_their_width`, `the_key_row_fades_where_keys_run_past_the_edge`.

- ✅ **Touch density on iOS** (2026-09-27, UI wave 2 app shell). `settings::theme_for` sets
  `theme.density` from `settings::density(touch)`, touch being the iOS build: 44 pt rows,
  headers, title bar and icon-button targets round the same icons. It waited until every tile
  header, row and the strip's hit test read the density instead of a 28 pt constant, so a
  44 pt header could not misplace a body. The status bar steps aside while the key bar shows.
  Tests: app `a_finger_gets_the_touch_density`, `the_header_and_its_hit_test_follow_the_density`,
  `the_phone_title_bar_fits_its_touch_targets`, `the_status_bar_steps_aside_for_the_key_bar`.

- ✅ **The overview is shapes on lifted cards** (2026-09-27, UI wave 2 tiles). The dashed "New
  workspace" slab was bigger than the real workspace above it and the panes' text was 4 pt
  mush. Each workspace with tiles is now one card: `kit::elevate` a base unit wider than its
  panes all round (so the md radius clears their square corners), and a 2 pt accent ring
  round the active one. The gap above a workspace holds the name plus both cards' margins.
  The trailing empty workspace is a compact ghost "New workspace" button where the next name
  would go, on the last card's left edge; it opens that workspace. (Amended 2026-09-27 (design audit): its words
  start where the names and the panes do, `kit::button_text_inset` left of them.) Below zoom 0.5 a tile is
  its header and body surfaces only. The body's surface is laid over its view rather than the
  view dropped, so the keyboard stays in the focused terminal or editor through the overview.
  - Amended 2026-09-27: shapes alone made every tile the same blank card, so each surface
    now carries its kind's icon and title, centred, at the chrome's type scale whatever the
    zoom, as the workspace names above the cards do (Mission Control's labels, not niri's
    scaled pixels, since scaled text is what the shapes replaced). The overview golden was
    re-recorded by hand: the label sits inside the diff tolerance, so the old golden would
    not have caught it going missing.
  - Amended 2026-09-28: below zoom 0.5 a tile is now its true miniature, not shapes and a
    label on its surface; see `workspace.md`, "The overview draws miniatures".
  Tests: ui `the_overview_lifts_each_workspace_and_offers_a_new_one`,
  `a_small_overview_draws_tiles_as_shapes`.

- ✅ **A body waiting on its worker stays blank for a grace** (2026-09-27, UI wave 2, the
  study's item 7). "Opening…", "Reading…", "Attaching…", a page's "Opening …" and a stream's
  "Waiting for the first frame…" painted at once, so an answer a frame later flashed a word.
  They now show only after `screen::LOADING_GRACE`, 32 ms: two 60 Hz frames, the frame the
  answer lands in plus a round trip of up to one frame budget (numbers in MEASUREMENTS.md,
  "loading placeholders after a grace"). Each waiting body keeps its first-drawn instant as
  element state and a timer draws it again when the grace ends. A lasting state ("Sleeping",
  "Paused off screen") shows at once. A remote window says what opens where ("Opening Safari
  on e2e-worker…") on the body's own surface, not the canvas, and its header slot turns the
  working mark while it opens.
  Tests: ui `a_remote_window_waits_blank_then_says_what_is_opening`, file
  `reading_shows_only_after_the_grace`.

- ✅ **A failure is marked where it is seen** (2026-09-27, UI wave 2 tiles). A failed block
  carried four red marks: the wash, a bar, a lighter chip over the wash, and the tile's
  header slot. The wash and the bar are now `error_fill` and stop where the block separators
  end (the inset plus the grid's columns), not at the element's edge. The hovered block's
  facts sit on the band itself, with no chip. The header slot keeps the kind while the newest
  failure's rows are in view (`TerminalView::failure_in_view`) and turns Failed once they
  scroll away. An unfocused pane's hollow cursor is `text_muted`, a place marker, not the
  cursor colour. Warp-style room above and below a block needs a grid answer; it stays a
  design question.
  Tests: terminal `a_failed_blocks_facts_sit_on_its_band_without_a_chip`,
  `the_newest_failure_is_in_view_while_its_block_is`, `an_unfocused_cursor_is_a_muted_hollow_block`,
  `cmd_up_and_down_walk_the_prompts_and_separators_follow` (the wash's reach); ui
  `a_failure_the_grid_shows_leaves_the_header_slot_alone`.

- ✅ **Tile details: editor bar, unsaved dot, tabs, ports, page buttons, empty workspace, note
  title** (2026-09-27, UI wave 2 tiles). (Amended 2026-09-27 (design audit): see **The design audit** below for
  the header's context and the dot.) The file's conflict bar starts on the header's inset
  after a warn mark, with "Reload" as a small secondary button and "Overwrite", the way that
  loses the disk's text, as the quieter ghost. The unsaved dot follows the title. A tabbed
  column's tabs take their titles' widths between 120 and 200 pt. A forwarded port is one
  pill whose number opens a tile and whose arrow opens the browser. A page's back and reload
  are bare icon buttons like fullscreen and close. A page or remote picture keeps the
  header's hairline when focused, since its surface is another program's. The empty
  workspace is one left edge: heading, the three ways to begin with the first on the
  palette's selected fill, keys in the palette's plain muted glyphs, then the workers; no
  hero glyph. A note's opening line, when it is a heading or prose, leads at `title()` in the
  strong weight. The header, the body laid out under it, the strip's hit test and the
  window-follow arithmetic all read `theme.density.header`; the fixed `HEADER_H` is gone, so
  the touch density moves them together.
  Tests: ui `the_unsaved_dot_follows_the_title`, `the_header_and_its_hit_test_follow_the_density`, note
  `the_first_line_is_a_title_unless_it_opens_the_body`, and the existing tab, port and empty
  workspace tests.
  Open: the editor's gutter still paints gpui-kit's `editor_background()`, which falls back
  to `c.background = panel`, and the 6 px after the numbers is gpui-kit's
  `LINE_NUMBER_RIGHT_MARGIN`. Both need kit or fork changes, not tile code.

- ✅ **The chrome is three cached views, and an echo draws none of them** (2026-09-27, UI
  wave 2 frame chrome). A terminal's notify dirties its ancestors, so every echo ran the
  workspace's render: the navigator's rows, the title bar and the status bar with the strip.
  The navigator, the title bar and the status bar are now views of their own (`ChromeView`),
  placed by the frame with `Entity::cached` at the size it lays out, and drawing the
  workspace's region when they draw. They are notified by what feeds them: any change of the
  workspace's own (`observe_self`), a shell's running command changing or a command finishing
  (the navigator's rows), and their own clocks (a turn's time, an age, the frame time). An
  echo frame fell from 2.2–2.4 ms to 0.44 ms p50 in the headless bench (`docs/MEASUREMENTS.md`).
  The same change holds the working mark: a key typed into a focused shell holds the spin
  clock's steps for the round trip to its worker plus a 60 Hz refresh, and the terminal's next
  change (the echo's frame) lets them go, so a step never takes the frame the echo needed.
  Tests: workspace `an_echo_leaves_the_chrome_as_it_was_drawn`,
  `a_command_that_starts_draws_the_navigator_alone`,
  `a_typed_key_holds_the_working_marks_for_its_echo`; icons
  `a_held_step_waits_for_the_echo_or_the_end_of_the_hold`.

- ✅ **A navigator row says its state or its age on line one, and the rest on line two**
  (2026-09-27, UI wave 2 frame chrome). The review found a lone "~ … now", "Note" as a second
  line, a count and a round trip side by side on each worker, two strong lines stacked under
  "Workers", and a filter that read as a label.
  - Line one ends in the tile's state as a word in its tone ("Needs you", "Working", "Done",
    "Failed"), else the unseen dot, else its age from a minute on. The leading slot keeps the
    kind: one mark per row, and the state reads down the right edge. Line two is all meta
    text: directory, agent words or last command, branch; a note's task progress or next line.
    Amended 2026-09-27 (design audit): one part order everywhere, what it is doing then where (agent words or last
    command, directory, branch), joined by " · " as the rest of the chrome joins facts; a home
    directory alone, a kind word and the worker's name are left out, and a row with nothing
    left is one line. *Needs you* and *Working* rows read the agent's words, then worker and
    directory, in the same order.
  - A row waiting on the human is washed in `warn_fill` at `alpha::FAINT`.
  - A worker's header drops the tile count (its rows are the count) and names its round trip
    only from 20 ms, where typing starts to feel remote; the hosts popover always has it.
  - *Working* lists agents at their turn under a heading with the working mark and a count:
    four rows, then "Show N more". Each ticks its turn's time from the worker's `since_ms`
    once a second, by the navigator's own clock, only while one is at work.
  - The workers' heading shows only under another section. The filter is a raised field.
  - Hidden where it would dock, the navigator leaves a 40 pt rail: one server glyph per
    worker with its rollup dot, which flies to that worker. Monochrome.
  - The handle is 12 pt centred on the 1 pt edge, drawn over the strip; a double-click puts
    the width back at 248. Laid over the frame, the panel is elevated on `kit::scrim`.
  Tests: workspace `a_tile_row_reads_its_age_or_its_state_then_its_place`,
  `a_note_row_says_its_progress`, `a_slow_round_trip_shows_on_the_right_edge_and_holds_still`,
  `working_lists_the_agents_at_their_turn_and_ticks_their_time`,
  `the_rail_keeps_the_workers_in_view_when_the_navigator_hides`,
  `the_handle_straddles_the_edge_and_a_double_click_resets_it`; navigator
  `an_age_shows_past_a_minute_and_ticks_at_its_next_unit`; kit `a_clock_ticks_in_whole_seconds`.

- ✅ **Workspace tabs are sized to their names and the active one joins the content**
  (2026-09-27, UI wave 2 frame chrome). Tabs were fixed 148 pt `overlay` pills beside a second
  "+" and column dots that showed with every column in view. A tab is now 64–180 pt wide by
  its name, every tab the same box down over the bar's edge so switching moves none; the active
  one fills it with the content colour and breaks the edge. Where each tab would get 176 pt
  it grows a meta line ("3 tiles · 2 workers"). A lone workspace is its name in the strong
  weight. A tab that goes folds its width away over 160 ms, at once under Reduce Motion or with
  moves off. The dots show only while a column is out of view and never under the laid-over
  navigator. The right "+" is gone (the palette, ⌘T and the navigator's "+" open things), and
  the rollup is a dot of its fill, the bell's badge the one count. (Amended 2026-09-27 (design audit): a lone
  workspace's name carries no rollup; with one workspace it only repeated the bell.) The menus and the inbox
  paint at `Layer::Popover`, under the bell or "…".
  Tests: workspace `a_lone_workspace_is_its_name_and_tabs_say_what_they_hold`,
  `a_closing_tab_folds_away_unless_motion_is_reduced`,
  `the_column_dots_show_only_what_is_out_of_view`, `the_workspaces_are_tabs_in_the_title_bar`.

- ❌ **The status bar's left slot says where the human is** (superseded 2026-10-05 by "No bar
  along the bottom" below; 2026-09-27, UI wave 2 frame chrome). The server's state left the title bar, where "• server unreachable" read as a tab.
  The left slot now reads the server's state first while it does not answer (the crossed-out
  server in `warn_fill`, "Server unreachable"), then worker › directory › branch of the
  focused tile, its steps a quiet chevron apart, with no icon but a down worker's. Readouts
  are meta text in sentence case ("RTT 4.2 ms", "Frame 1.1 ms"), and a state is a small dot
  of its fill beside `text_secondary` words, so the tile and the bell keep the loud marks.
  Amended 2026-09-27 (design audit): no readout wears an icon. The server's state and a down worker take the warn
  dot, and the ports and uploads are their words alone.
  Tests: workspace `the_servers_word_leads_the_status_bar`,
  `the_status_bar_reads_the_focused_tile_and_its_link`.

- ✅ **A waiting agent is marked once on its tile** (2026-09-27, golden review). Amends
  **Attention is said once per place**. The tile still showed it twice: a 2 pt warn bar along
  the header's top and the pill in the header. The pill always comes with a waiting agent, is
  the button to its prompt, and sits beside the status slot's warn mark, so the bar is gone.
  The bell's badge and the status bar's count remain the one global count each.
  (Amended 2026-09-27 (design audit): the bell's badge is the one global count; the status bar's "N needs you" is
  gone, and ⌘⇧A still goes to the next one waiting.) Likewise a
  command that finished unwatched: its slot's mark says done or failed in its tone and the
  unseen dot says it went unwatched, so the "Done · 6.0 s" readout beside them is quiet
  `text_secondary` text, still the button to the shell, not a third tinted pill. Test:
  `a_tile_that_needs_you_says_so_once_in_its_header`.
  The navigator did the same: its *Needs you* section lists the agent, and the agent's row
  under its worker was washed in the warn fill too, beside its warn-toned "Needs you" word.
  The wash is gone; the section leads and the word stays (golden
  `agent-needs-you-navigator`). Test: `a_tile_row_reads_its_age_or_its_state_then_its_place`.

- ✅ **A link's tailnet path shows beside its round trip, and a refusal by the policy is
  named** (2026-09-27, with **Tailscale is the network, and its LocalAPI says who is calling**
  in `topology.md`). A worker reached over the tailnet says how Tailscale carries the link
  (`WorkerMsg::Path`). The workspace keeps the latest per worker and drops it with the link,
  so a relink shows nothing until the worker says again. A loopback or LAN link never has one
  and shows nothing extra. The words are "Direct", "Peer relay" and "DERP · fra":
  - the status bar puts the focused worker's path before "RTT 4.2 ms";
  - the hosts popover puts each worker's path before its round trip;
  - the navigator names only a DERP relay, like the round trip, which it names only when it is
    slow. A list of healthy workers stays a list of names;
  - DERP is TCP through a relay server and often a detour, so it takes the `warn` text tone
    everywhere. The other paths take the readout's own colour.

  A refusal by the tailnet policy is its own state, not a generic disconnect, and the redial
  goes on because a policy change can grant it. A worker that closes the dial with
  `NOT_GRANTED` shows `WorkerStatus::NotGranted`: "Not granted" where the link's health
  shows, and "closed to this device by the tailnet policy" when it is gone to. That word wins
  over the server's "unreachable", because the worker answered. A server's
  `Refusal::NotGranted` shows "Server access not granted by the tailnet policy" in the status
  bar's server slot. Both add panels say "Not granted by the tailnet policy", the words of
  `Refusal::text`, which the CLI prints too. `NetError::stream` walks noq's error chain for an
  application close with `NOT_GRANTED` and yields `NetError::NotGranted`, so no caller reads
  error text.

  Tests: workspace `the_link_path_shows_beside_the_round_trip_and_goes_with_the_link`,
  `a_worker_the_tailnet_policy_closes_says_not_granted`; app
  `a_refusal_by_the_tailnet_policy_is_named_over_other_words`; client
  `a_worker_closing_with_not_granted_is_told_from_other_closes`,
  `a_refusal_is_reported_by_name_and_the_link_keeps_dialling`; net
  `a_not_granted_close_is_its_own_error`.

- ✅ **The design audit: fills are fills, one count, one left edge, context in the UI face**
  (2026-09-27). An audit of every golden against Warp, Zed and monocode found controls that
  read as disabled, attention said twice, lists on two left edges, and context set three
  ways. This entry amends the rulings it names in place (each marked "Amended 2026-09-27
  (design audit)").
  - **Fills are fills.** The dark accent (`8AB4F8`) is a text tone, lifted to read on the
    canvas; as a button's fill it looked disabled. A fill now takes `accent_fill` with
    `fill_fg`: the primary button (`kit::button`, gpui-kit's `primary`), a ticked task box in
    a note, a lit key on the key bar, the terminal's accent buttons and the regex toggle.
    `fill_fg` on `accent_fill` clears AA in both variants, where white on the light fill
    would not (3.7). With nothing left drawing text on the accent, `accent_fg` is gone.
    `kit` `the_accent_text_tone_is_never_a_fill` holds it. (Amended 2026-10-02: those
    controls take the neutral solid, `kit::solid`, and the accent fill is a mark only.)
  - **One count of what waits.** The bell's badge is the only global count. The status bar
    counts only the agents at work, and a lone workspace's name carries no rollup dot, since
    with one workspace it only repeated the bell. ⌘⇧A still goes to the next one waiting.
  - **The status bar says what is wrong, in words.** `N workers` shows only while a worker is
    down (all up, it said nothing); the "…" menu's *Workers* opens the hosts popover. No
    readout wears an icon. The server's state and a down worker take the warn dot, and the
    ports and uploads are words. An upload's progress is the 2 pt `accent_fill` line along its
    tile header's foot, which was already drawn and is now held by a test.
  - **One left edge in a list.** A palette command keeps the leading slot, empty, so its
    title starts where a tile's or a worker's does. A tab of a tabbed column starts its kind
    on the header inset (12 pt, it was 8). A tile row's kind glyph sits under its worker's
    name. The overview's "New workspace" words start where the workspace names and the panes
    do.
  - **One modal anchor, and a scrim only for a modal.** Every overlay's top sits a fifth of
    the way down (`kit::MODAL_ANCHOR`), so the palette and the settings open at one place. The
    palette and the pickers are lists typed at and float with no scrim (`kit::anchor`); the
    settings and adding a worker hold the window and dim it (`kit::backdrop`). The desktop
    palette fades its list's foot while more runs on below, so a cut row no longer ends on the
    foot's hairline.
  - **Navigator and inbox rows share a rhythm.** Second lines read what a tile is doing, then
    where: agent words or command, directory, branch, joined by " · ". They leave out the
    worker's name under its own header, a home directory alone, and a kind word, and a row with
    nothing left is one line (its height follows, `NavRow::shape`). *Needs you* and *Working*
    rows put a " · " between the agent's words and the place. Inbox rows are the navigator's
    two lines (`kit::row`, `Row::Two`). The age ends line one and swaps for mark-read under the
    pointer. No word repeats the section heading ("Needs you" under *Needs you*, "Done" under
    *Finished*); a failed command's exit leads line two in its tone. The popover's right edge
    sits on the window inset, like the "…" menu's.
  - **A header is its title, then its context.** The context is muted, in the UI face, with
    no separator: a shell's directory, a file's folder, a note's task count (`2/3`), a page's
    host. A file's title is its name and a note's its first line; neither carries a " · "
    suffix any more. The unsaved dot follows the title's text by `spacing.xs`, before the
    folder. The status bar's breadcrumb and the palette's context column are in the UI face
    too. Mono is kept for ports and figures.
  - **The "…" menu in sections.** Navigation (palette, overview, stream stats), settings, then
    connections (the server, adding a worker, *Workers*), a hairline between them
    (`MenuGroup`). The per-worker *Forget* rows are gone; forgetting is the hosts popover's.
  - **Touch.** The navigator laid over the frame on an iPad runs, with its scrim, through the
    home-indicator band, as the phone's drawer does. A phone's tile, already the screen's
    width, offers no fullscreen button. Where the key bar's row fits (an iPad) its groups
    spread: esc, tab and the modifiers left, arrows centred, symbols and Copy, Paste, Find
    right.
  - **First run.** The page is the content surface, not the canvas. The field's example is the
    panel's own kind of host ("home-server or 100.64.0.1" for a server, "mac-studio or
    100.64.0.3" for a worker). A server the tailnet found is named in the blurb's place, its
    name in the text colour, its address already in the field. The discovery has no gallery
    scene: the e2e stack runs no tailnet, and filling `Adding::found` from a test hook would
    fake the network. The headless test drives `offer_found` directly instead.
  Tests: kit `the_kit_theme_follows_the_tokens`, `the_accent_text_tone_is_never_a_fill`,
  `a_list_floats_and_a_modal_dims`, `the_mono_face_is_for_ports_and_the_settings_file`; settings_editor `the_dialog_is_as_tall_as_the_file` (its
  anchor); workspace `a_lone_workspace_is_its_name_and_tabs_say_what_they_hold`,
  `the_status_bar_reads_the_focused_tile_and_its_link`,
  `the_status_bar_says_where_the_shell_is_and_counts_what_is_shared`,
  `the_servers_word_leads_the_status_bar`,
  `a_drop_on_a_shell_uploads_shows_progress_and_types_the_quoted_paths`,
  `the_icon_slot_keeps_every_title_on_one_edge`,
  `the_palette_hangs_at_a_fifth_and_is_a_sheet_on_a_phone`,
  `a_row_with_nothing_to_add_is_one_line`, `a_tiles_glyph_sits_under_its_workers_name`,
  `a_tile_row_reads_its_age_or_its_state_then_its_place`,
  `an_inbox_row_says_its_age_first_and_no_word_its_heading_does`,
  `a_header_is_its_title_then_its_context_in_the_ui_face`, `the_unsaved_dot_follows_the_title`,
  `a_tabbed_column_draws_a_tab_per_tile`, `the_overview_lifts_each_workspace_and_offers_a_new_one`,
  `the_more_menu_groups_its_rows_into_sections`,
  `the_overlaid_navigator_runs_through_the_home_indicator_band`,
  `a_tile_that_fills_a_phone_offers_no_fullscreen`; app `a_wide_key_bar_spreads_its_groups`,
  `the_panel_names_its_host_and_the_server_the_tailnet_found`. The goldens are to be recorded
  again with this change.

- ✅ **The accent fill has its own ink** (2026-09-27, golden review). The design audit gave every
  fill one text colour, `fill_fg`, near black, which is right on the amber and green badges. On
  the phone's first-run page it put a black "Connect" on a bright blue, which reads as a
  disabled or foreign control: a native primary button is white on a deep blue. White on the
  light variant's old `3b82f6` is 3.7:1, so the light `accent_fill` is now `2563eb` and its text
  is `accent_ink`, white, 5.2:1. The dark variant keeps its lighter blue with dark ink, where
  white would be 2.6:1. `fill_fg` stays for the success, warn and error fills. Everything that
  sits on the accent fill takes `accent_ink`: the kit's primary button and gpui-kit's
  `primary_foreground`, a ticked note box, a lit key cap, the terminal's accent buttons. Tests:
  the theme's fills test holds `accent_ink` on `accent_fill` and `fill_fg` on the rest to AA in
  both variants; `the_kit_theme_follows_the_tokens`. Amended 2026-09-27 (design direction): the
  dark fill is now `346BF1` and carries white too, and light's is `1B4ED8`. Amended
  2026-10-02 (design checkpoint 1): the accent fill is the brand's green and a mark only; a
  primary, a ticked box and a lit key take the neutral solid (**MonoCode's calm, the brand's
  green, a neutral primary**).

- ✅ **The design review wave: say, don't ask; one place; every row an icon** (2026-09-27, a
  review of every golden against Warp, Zed and monocode).
  - **A waiting agent's pill is a statement.** A permission request read "Allow Bash?", in
    the warn tone, on a button: a click looked like an approval, yet the app never answers for
    an agent. It now reads "Needs approval: Bash" (with the command when the hook gave one),
    in the badge, the navigator's second line and the inbox alike. The button's accessible
    description says what the click does ("Shows the prompt").
  - **"+" is a menu of what to open.** It made a new workspace and nothing else, sitting right
    after the workspace's name, where it read as "add to this". It now lists New terminal, New
    agent, Add a window or display and New note, then a hairline, then New workspace. It hangs
    from its own left edge, and each row runs the palette's action for it.
  - **A workspace is named by where it works.** Until someone names it, its name is its first
    shell's repository, else that shell's directory, else "Workspace N". The home directory
    says nothing, so it keeps the number. The e2e stack's shells start in its private home, so
    its goldens keep "Workspace 1". *Amended 2026-10-01:* the number is gone (see "Names come
    from the task" below).
  - **The light panel is as far from the content as the dark one.** At `ink(0.035)` an
    unfocused header was 2.8 L* from the focused one's white; dark's is 3.5. At `ink(0.05)` it
    is 3.8, and the panel still clears the bars by 2.4 L* (dark 2.5). A contrast ratio could
    not see this: both variants were 1.07:1.
  - **One place per tile.** The header, the navigator's second line and the palette's context
    come from `tile_place`: a note's progress is "1 of 3 done" everywhere (the header said
    "1/3", the palette "Note"), a file its folder, a page its address when the title is not it.
  - **Every palette line has an icon.** The earlier rule, no icon on a command, left an empty
    slot that on a phone read as an indent under the section heading. `PaletteItem::new` now
    takes the icon, so a command cannot be added without one.
  - **The overview draws whole cards.** At the shapes zoom a header is the body's surface with
    no hairline: a band on some tiles and not on the focused one read as cards half drawn. The
    empty workspace at the end is a dashed card the size of a real one with "+ New workspace"
    in it, niri's way of showing where the next one goes.
  - **Notes.** A ticked task is struck through and set back to `alpha::STRONG`, so what is left
    to do reads first. An empty note is "Untitled note". The old "note" broke sentence case,
    and "New note" would have matched the command of that name in the palette.
  - **Status bar.** With one worker its name is dropped from the place, unless its link is
    down. The round trip is the figure alone ("4.2 ms"; a screen reader hears "Round trip").
    It sits in a slot held at `999 ms` width from the link's first frame, so the first sample
    moves nothing.
  - **The keyboard comes back to the tile.** Closing the settings or the add-worker dialog
    focused the workspace's own handle, not the shell, and left its cursor hollow in the dark
    workspace golden. `WorkspaceView::return_keyboard` gives it to the focused tile's shell or
    editor. The settings, the add-worker dialog and the title bar's menus all use it.
  - **First run.** The page carries the app's mark over its heading. A server the tailnet found
    is a row to press (its name, "On your tailnet" and its address) under the blurb, not words
    in its place.
  - **The settings are coloured, and so is what a project is made of.** syntect's own set has
    no TOML, TypeScript, Dockerfile, Zig or Nix. The grammars are now bat's (`two-face`, on the
    same pure-Rust regex engine), and the settings dialog's field is the file tile's code
    editor with TOML's colours and line numbers, so a parse error's line is a glance away.
  - Tests: agents' badge in `a_tile_that_needs_you_says_so_once_in_its_header`, chrome
    `plus_lists_what_to_open_and_runs_it_as_its_keys_do`,
    `the_keyboard_goes_back_to_the_focused_shell`, bars
    `a_workspace_is_named_by_where_its_first_shell_is`,
    `the_round_trip_lands_without_moving_the_bar`, theme
    `the_steps_are_as_far_apart_in_light_as_in_dark`, overlays
    `the_palette_finds_an_unnamed_note` (header, navigator and palette agree), palette
    `every_line_icon_is_embedded` (every command's icon), strip_marks
    `the_overview_lifts_each_workspace_and_offers_a_new_one`, markdown
    `a_done_task_is_struck_through_and_set_back`, highlight
    `toml_and_the_languages_syntect_lacks_are_coloured`, settings
    `the_field_colours_the_file_as_toml`, app
    `the_panel_names_its_host_and_the_server_the_tailnet_found`. The goldens are to be recorded
    again with this change.

- ✅ **Upstream sync of 2026-09-27: text paints its highlights under the glyphs** (2026-09-27).
  zed rebased onto main `bda9c0bd43` (1 commit, fork head `6f8c0faf`); gpui-kit and
  libghostty-rs were current. The commit fixes Markdown search highlights drawn over the text
  and gives `TextLayout` `paint_background` and `paint_foreground`, so a caller can put its own
  quads between a text's run backgrounds and its glyphs. Slopty draws nothing between the two:
  notes and the file tile paint through gpui-kit, and the terminal paints its own cells. The
  goldens were recorded again on it.

- ✅ **Design review, round two: every surface says what a tile is, where, and how it is
  doing** (2026-09-27). The second pass over the goldens found chrome that said too little:
  every shell was "shell", a long command looked idle, the status bar could hold nothing but a
  round trip, and a worker without Screen Recording failed silently. One change per fact,
  each feeding every surface that shows it:
  - **Titles.** A shell's title is, first that says something, the command it runs, a title its
    program set, its repository or directory, else "Terminal". The shell's own name (`zsh`,
    the worker's fallback of the program's name), a path and a `user@host:path` prompt are not
    titles: the place says them better. An agent's shell takes the agent's own title, else the
    agent's name ("Claude Code"). The header's context says where the shell is less what the
    title said: named by its repository, the path within it or the branch; otherwise the
    repository and the path, or the directory.
  - **Home, exactly.** `HelloAck.home` gives each worker's home, and `cwd_tail` writes `~` for
    it and only it. The `/Users/<name>` shape is guessed only while no home is known, so a home
    on another volume reads `~` and someone else's `/Users/x` does not.
  - **Running.** A shell command past 3 s (`RUNNING_AFTER`) is `Status::Running`: the neutral
    `text_secondary` tone and lucide's `loader` turned one step a second, beside how long it
    has run, in the header, the navigator row and the status bar. It is the least of what a
    rollup shows, under the unseen dot. The calm mark rides the spin clock's own once-a-second
    lane, which keeps waking its views under Reduce Motion (the mark stands still, the time
    counts), so a dev server left running costs one frame a second, not twelve.
  - **The status bar is never empty.** With nothing that says where, it names the worker. On
    the right it says what the focused tile is: a file's language and `Ln, Col`, a stream's size
    and painted rate, a page's host, a command's running time. A repository's changes follow
    its branch as `+12 −3`, in the success and error text tones; the navigator row ends in
    the same words, whole, after its second line.
  - **Worker health.** Caps come with the hello, as `WorkerMsg::Caps` and from the server's
    directory while the worker's own link is down. The navigator's worker header grows a warn
    line only when something is wrong (Screen Recording or Accessibility off on a Mac, another
    version); the hosts list reads the machine ("macOS 26.5 · load 2.1"); "Add a window" on a
    worker that cannot capture says why instead of opening an empty picker.
  - **Where a new tile goes.** With several workers "+" lists them first, the target checked;
    choosing one keeps the menu open. The empty workspace's worker rows open a shell on their
    worker here rather than going to its tiles elsewhere. An empty workspace's tab no longer
    says "0 tiles".
  - **Overview covers** show the tile's state (else its kind), one muted line of what its
    navigator row says, and the worker where there are several; a workspace's name carries its
    rollup.
  - **The palette's order** is a tile recency (`recency`, which replaces `shell_recency` and
    still picks the "run in shell" target): agents waiting on the human first, then the latest
    used, the focused tile last, since nobody goes where they are.
  - **First run** says "Looking on your tailnet…" at once, lists every server that answers
    (best first), and says "Nothing answered on your tailnet" when none does. Workers are not
    probed yet: that needs a worker probe with the worker's ALPN in `slopty_net::discover`.
  - **iPad** docks the navigator where the strip keeps a regular width (900 pt beside it:
    either iPad in landscape), and lays it over the strip below that.
  - **Fixes.** An agent row joins its words and its place with the one meta separator, spaces
    and all, as a tile's second line does; it had a gap on each side of a bare dot. The app
    goldens wait for every link's first round trip before capture, so the status bar's readout
    is in the frame every run.
  - Tests: `tests::facts` (titles and context, running, changes, health, the status bar, "+"
    and the empty workspace, palette order, overview covers), `tiles`
    `a_place_is_its_last_two_directories_with_home_as_a_tilde`, `frame`
    `the_navigator_docks_only_where_the_strip_keeps_its_room`, `nav_rows`
    `a_tile_row_reads_its_age_or_its_state_then_its_place` (the separator), `icons`
    `a_running_mark_steps_once_a_second_even_under_reduce_motion`, `rollup`, app
    `the_panel_names_its_host_and_the_server_the_tailnet_found`. The goldens are to be recorded
    again with this change.
- ✅ **The conversation face: a projection of the TUI, toggled per tile** (2026-09-27). An agent
  terminal shows its TUI or its conversation face (`slopty_ui::conversation`), and the tile
  keeps the same PTY and session under both.
  - **The toggle.** ⌘J (`ToggleConversation`, Workspace context) switches between them, as ⌘J
    shows and hides the terminal in the editors this app learns from. The header has a quiet
    icon button that does the same, and the palette lists "Show thread or terminal". The
    keyboard goes with the body: the composer when the face shows, the TUI when it hides.
    Showing the face sends `Follow`. Hiding it, closing the tile, the agent leaving or a new
    link sends `Unfollow` or drops the follow, so the worker hands a held prompt back to the
    TUI. The face is made once per session and kept while the session lives, so its draft and
    scroll place survive a toggle. On a phone-width layout (`Layout::is_phone`) an agent's tile
    shows the face until the person picks, and the pick then sticks for that session.
  - **The place survives a re-follow.** A follow replays the whole conversation (`Reset` of
    every thread, the entries, `Current`). The model builds the replay aside and swaps it in at
    `Current`. The rows are diffed by key and spliced into the `ListState`, so a replay of the
    same conversation changes nothing and the anchor stays. The list follows the tail
    (`FollowMode::Tail`) until it is scrolled up, which shows a "latest" pill. It follows again
    at the bottom or from the pill.
  - **Turns fold.** A settled turn folds to one row between its prompt and its answer, such as
    "Worked for 35 s · 13 steps · +2 −0 · 1 failed", or "Stopped after …" when Esc ended it. The
    notes, compaction and final text stay outside the fold. The turn the agent is on, which
    includes one blocked on a prompt, never folds. Two or more consecutive reads, searches or
    task updates group into one row outside Verbose.
  - **Densities.** Normal, Thinking (adds the model's thinking to open turns) and Verbose (opens
    every settled turn and ungroups). ⌃O steps through them, as Claude Code's own ⌃O expands
    its transcript. The chip in the composer shows the density and cycles it on a click.
  - **Tools render at three levels.** Title, summary and full. An edit or write shows its
    syntax-coloured diff unasked, up to 12 lines, then "N more lines". Each side of a hunk is
    highlighted as its own text. The diff is unified in a tile narrower than 960 pt and side by
    side from 960 pt up (superseded 2026-10-07: unified at every width, "A diff is unified, its
    files stacked and folding"). Bash shows its command and output tail. A subagent is a card that
    opens its own thread under a bar that leads back, and the bar is named from the `Agent`
    call when the thread's origin has no description. A clipped text offers "Show all" and
    sends `Expand`.
  - **The composer types into the same PTY, as a person would.** A message goes as one
    bracketed paste (`TerminalView::paste`, which also ends the predictor's guesses). Then,
    200 ms later, Enter goes through the view's own key path, because Ink drops an Enter that
    arrives in the same read as the paste (`orchestrate::SUBMIT_PAUSE`). A one-line `/` or `!`
    command is typed raw, so Claude Code's command menu sees it keyed. Esc interrupts only
    while the agent has a turn, and is otherwise the field's own. A message sent while the
    agent works shows as queued until the transcript records it.
  - **Approvals own the composer's area.** The card previews the call at full level and
    offers Allow once, Always allow (listing what it grants, with where it is stored) and
    Deny (with a reason field). An answer goes once. A second press finds the card answering.
    Settled removes the card and leaves a line saying how it ended. Released, where the TUI
    shows its own dialog, offers "Show the terminal".
  - **Header chips** come from the meters: `+N −M` for the lines the conversation changed, a
    context ring (warn from 80 %, error from 95 %, the percentage in its hint), and the model.
    The navigator row and the overview cover keep the state vocabulary. With a followed
    conversation they add the call the agent is on, where the hooks give no detail, and the
    first line of its last answer once it stopped.
  - Tests: `conversation::{model, rows, tools, diff, composer, approval}` units over the
    recorded fixtures; `conversation::view::tests` (tail follow and replay anchor, ⌃O,
    subagent thread); `workspace::tests::faces` (toggle and follow, draft kept, composer bytes,
    answer once, released prompt, unfollow on close and agent exit, phone default); app goldens
    `conversation`, `conversation-dark`, `conversation-subagent` and `conversation-phone`,
    driven through the real worker and `slopty hook`.

- ✅ **Design review, round three: the title reads first, alike tiles are numbered, the
  tailnet's workers are a way in** (2026-09-27, a review of the goldens).
  - **The first run offers the tailnet's workers too.** It looked only for servers, so a
    tailnet with workers and no server said "Nothing answered". The panel now looks for both at
    once (`net::find_on_tailnet`, over `discover::servers` and `discover::workers`). Each server
    is a row ("Server · 100.64.0.1", press to connect), then each worker not yet added
    ("Worker · …", a Mac glyph, press to add it at once; the panel turns into the worker's, so a
    failure is told beside its address). A worker's own panel ("Add a worker…") looks too, and
    offers only workers. The field takes the best host of the panel's own kind, never a
    worker's address in a server's field. The words stay "Looking on your tailnet…" and
    "Nothing answered on your tailnet". Workers the server lists or the store holds are left
    out. The e2e app reads a stand-in tailnet (`slopty_e2e::TAILNET_STATUS_ENV`, empty from the
    harness), so no golden shows what this Mac's own tailnet answers, and `first-run` is taken
    once the look has ended. Superseded in part 2026-10-04 by **The first Mac runs the server**
    (topology.md): the first run is the server's panel and offers only servers; the tailnet's
    workers the server does not list are rows on "Add a machine", and pressing one opens the
    SSH sheet on it, to install this build there registered with the server.
  - **A header's title keeps its width; the pill gives way.** On a phone the agent's pill
    ("Needs approval: $ touch …") took the header and squeezed the title. The title is now its
    own flex item at its text's width, the place gives way first (`PLACE_SHRINK`), the readout
    strip next (`STRIP_SHRINK`, an ellipsis, down to its buttons), the title last. On a phone,
    and beside a face that shows the detail itself, the pill says the state alone
    (`agent_status_word`: "Needs approval", "Has a question"); a screen reader still hears the
    whole line. An idle agent has no pill: the hollow mark in the slot says it, and a grey
    "Idle" chip read as a button.
  - **Alike tiles are numbered.** Two shells in one worker both read "Terminal" in the
    navigator, the tabs, the overview and the palette. `number_twins` numbers each unnamed tile
    that reads like an earlier one of its worker, in the order they were made ("Terminal 2"),
    once a frame; a named tile keeps its name. A number, not the directory: two shells side by
    side are usually in the same one.
  - **A place does not repeat the title.** A shell titled by the directory it stands in showed
    that directory twice ("drop-here …/drop-here"). Its place is now the directory above
    (`place_beside`), or none.
  - **Overview covers share a title line.** A cover with a second line (a note's "1 of 3 done")
    centred the pair, so its title sat half a line above its neighbours'. The line now hangs
    under a title centred on every card.
  - **A step being written reads lighter.** The mod's live text block is `text_secondary`
    until the transcript's entry takes its place, and it is an `Article` labelled "Writing: …"
    (live thinking, "Thinking"), so a screen reader and the e2e can tell it from the settled
    answer. A call being prepared names its subject once its input is whole JSON ("Bash
    echo hi", `tools::preparing`), not the raw object.
  - Tests: `the_first_run_offers_the_workers_the_tailnet_found`,
    `a_phone_header_keeps_its_title_and_the_pill_gives_way`,
    `tiles_that_read_alike_are_numbered`, `a_place_does_not_repeat_the_title`, and the app
    golden `conversation-live` (`a_step_being_written_shows_live_until_the_transcript_settles_it`).

- ✅ **Design direction 2026-09-27: the chrome one notch from the content, white on the
  primary, a lit edge on what floats** (2026-09-27, `.research/design-direction-2026-09-27.md`).
  The user asked for chrome that stays minimal but reads as finished and modern rather than
  plain, held to Warp, T3 Code, Amp, Linear and Geist, with nothing that looks generated. The
  study read those products from source, CSS and token files, and reviewed only Slopty's own
  goldens. It found the dark window framed in near-black, light chrome greyer and its hairline
  darker than any reference, one weight above regular doing every job, a dark primary button
  that read as disabled, and floating sheets whose shadow vanished on near-black. This entry
  amends three rulings: "The chrome is derived from the content" (its shares),
  "The accent fill has its own ink" (white in both variants) and the one elevation (a lit
  edge in dark). Waves 3 to 6 of the document follow as their own entries.
  - **Shares.** Same derivation, new steps. The bars sit one notch under the content, where
    Linear dims its navigation, rather than three.

    | Step | Dark share | Dark | Light share | Light |
    |---|---|---|---|---|
    | `canvas` | 28 % to black | `101115` | 4 % to text | `F6F6F6` |
    | `panel` | 16 % to black | `121418` | 2.5 % to text | `F9F9F9` |
    | content | | `16181D` | | `FFFFFF` |
    | `elevated` | 4.5 % to text | `1F2126` | 60 % to white | `FFFFFF` |
    | `raised` | 6.5 % | `24252A` | 5.5 % | `F3F3F3` |
    | `border_subtle` | 6 % | `222429` | 6.5 % | `F0F0F0` |
    | `overlay` | 8.5 % | `282A2E` | 8.5 % | `ECECEC` |
    | `border` | 10.5 % | `2C2E32` | 11.5 % | `E5E5E5` |

    The document proposed dark 24 % and 12 % (`111216`, `13151A`). That put the dark panel
    1.5 CIE L* under the content against the light one's 2.1, so an unfocused header barely
    parted from a focused one and the two variants no longer stepped alike. At 28 % and 16 %
    the steps are content to panel 2.0 / 2.1 L* (dark / light), panel to bars 1.2 / 1.1 and
    content to bars 3.1 / 3.1. The 1.05 contrast step of "Three surfaces in order" is retired:
    on near-black every step one notch apart is under it, which is why the steps are held in
    L* now. The text tones need no lift at these steps.
  - **Primary fill.** `accent_fill` is `346BF1` in dark and `1B4ED8` in light (T3 Code's
    primaries), and `accent_ink` is white in both: 4.65:1 and 6.71:1. Dark's `6AA1FF` with
    near-black words read as a disabled or foreign control. `346BF1` still stands 3:1 off
    every surface in dark. (Amended 2026-10-02: the primary is the neutral solid; see
    **MonoCode's calm, the brand's green, a neutral primary**.)
  - **Type, radius, motion.** `Typography::MEDIUM_WEIGHT` (500) says "this one": a button's
    words now, and the selected row, the focused title, the active tab and an approval's
    statement as their waves land. `STRONG_WEIGHT` narrows to titles (`kit::title`, the title
    and display sizes, headings, the first run's wordmark); the workspace's name, a worker's
    name and the overview's names move to the medium weight. `prose()` is 14 for assistant text
    and prompts; `display()` is 22. `Radii::lg` (12) is the radius of what floats: dialogs,
    menus, the inbox, the workers' popover, toasts and the add-worker panel; a hint and a pill
    keep their control's radius, since at 12 a 20 pt hint is a lozenge. `slopty_theme::Motion`
    holds the durations (hover 0, fade 120, settle 160, sheet 240 ms; since 2026-10-03 also
    `unhover` 150 and `exit` 100 ms) and the two curves as
    cubic-bezier points (ease-out `0.22, 1, 0.36, 1`; drawer `0.32, 0.72, 0, 1`), solved by
    `Curve::at`; `kit::FADE`, `kit::ease_out` and `kit::drawer` read them.
  - **One elevation, lit in dark.** The soft layer is now 12 down with a 32 blur (0.5 dark,
    0.10 light); at 4 and 12 it did not show on `16181D`. In dark `kit::elevate` adds an inset
    line of white at `alpha::EDGE` (0.06, a new, quietest step of the ladder) along the top,
    between T3 Code's 4 % and Raycast's 10 %. GPUI paints an inset shadow under the element's
    border, so the line is two points deep and the second one shows, just inside the hairline.
  - **Focus.** The keyboard's ring is a 2 pt gap, then a 2 pt ring of the accent at
    `alpha::STRONG`, its corners the element's grown by 4 (Geist's `0 0 0 2px background,
    0 0 0 4px blue`). A spread shadow could not draw the gap without knowing the colour behind
    each element, and it showed through a transparent one, so the zed fork gained
    `Style::outline` (CSS's `outline` with an offset). Pointer focus shows nothing. A focused
    text field shows its caret only: gpui-kit's `ring` colour is now `border`, and the composer
    no longer turns its hairline accent.
  - **Key caps** sit on `overlay` with no hairline; a ring round every cap made the palette's
    foot a row of buttons.
  - **Checks.** Kit lint-tests: `a_floating_surface_is_rounded_lg`,
    `strong_weight_is_for_titles`, and `the_accent_text_tone_is_never_a_fill` now also fails a
    focused field told by an accent border (waived for the terminal's find bar until its owner's
    next change). The document's clause that the composer and the approval card wear
    `kit::elevate` lands with the conversation wave, which needs a frame-time number first.
  - Tests: theme `the_chrome_sits_one_notch_from_the_content`,
    `the_surfaces_climb_bars_panel_content`, `the_primary_fill_carries_white`,
    `a_curve_runs_from_rest_to_landed`, `elevation_and_density`, `chrome_text_clears_wcag_aa`,
    `the_ladder_is_monotonic`, `status_fills_read_as_their_hue`; kit
    `the_elevation_is_two_layers_of_the_shade_and_a_lit_edge_in_dark`, `the_curves_ease_out_and_land`,
    `the_focus_border_check_knows_a_field_from_a_ring`, `the_kit_theme_follows_the_tokens`;
    a11y `the_keyboard_rings_a_stop_and_the_pointer_does_not`; gpui (fork)
    `an_outline_rings_the_element_clear_of_its_edge`. Every golden is taken again.

- ✅ **A focused element with no node of its own is announced by its labelled ancestor**
  (2026-09-27). GPUI logged "a focused element has an id but no role" whenever a gpui-kit field
  took the focus: the field tracks focus on a role-less `input-state` element inside the
  labelled `TextInput` frame, so the tree's focus stayed on the window and a screen reader
  announced the window. The zed fork's `A11y::set_focus` (commit `a0282ca21a`) now reports the
  nearest ancestor with a node as focused. The focused element's prepaint runs after every
  ancestor pushed its node, so the top of the node stack is that ancestor; an element with no
  element id falls back the same way. Tests: gpui
  `a_focused_element_without_a_role_focuses_its_labelled_ancestor`,
  `a_node_less_focus_falls_back_to_the_nearest_ancestor`; ui
  `the_focused_field_is_what_a_screen_reader_hears`.

- ✅ **Navigator and palette: one chrome surface, *Needs you* only for what is out of sight, the
  palette as an instrument** (2026-09-27, wave 3 of `.research/design-direction-2026-09-27.md`,
  §5.2 and §5.5). The user asked for the navigator and the palette to read as finished, calm
  and modern with nothing generated about them, held to Linear, Raycast, T3 Code and Warp.
  - **The navigator is the bars' surface.** It sits on `canvas`, as T3 Code's sidebar shares
    its header's colour and Linear dims its navigation a notch; `panel` is left for the
    unfocused tiles' headers. Its top row has no hairline under it: the column is one surface
    from top to bottom and the rows need no rule to start. The filter is a well a row tall
    (28, touch 44) on `raised` with no border, its glyph a base unit in, 8 pt from the panel's
    trailing edge. The list starts half a base unit under it.
  - **Hierarchy by weight and space.** The selected row is `overlay` with its title in `text`
    at the medium weight (T3's `font-medium` on the active row); a worker's name is 500 in
    `text` over tiles in `text_secondary`. A worker after another's rows stands a base unit off
    them; under a heading, or first, it needs none. A worker's round trip is set like every
    other readout, at the meta size, where it had been a size larger than its neighbours. An
    open worker with no tile says "No tiles" in one quiet line on its tiles' titles' edge; a
    filter that matched the worker's name lists it bare. The filter's empty state is the lists'
    one quiet line.
  - **The dots between facts are quieter than the facts.** A second line's " · " is drawn in
    `text_muted` at `alpha::PRESSED`, as one run of text (`palette::dotted`), so the line still
    ends in one ellipsis. The document proposed the hairline's colour; on the light canvas that
    vanished and the facts read as spaced words. The palette's context column and the picker's
    place take the same dot.
  - ***Needs you* lists only what the list does not show.** A waiting tile whose row is in view
    ends its first line in "Needs you", so a second row for it at the top repeated it (the
    duplicate "Claude Code" in `agent-needs-you-navigator`). The section now lists a waiting
    agent only while its tile is folded away, filtered out, scrolled out of the list's view, or
    it has none. In view is read off the list's last layout; a row it has not placed yet (new
    this frame, or before the first layout) counts as in view, so nothing flashes in for a
    frame. The rule cannot flip back and forth: the section sits above the row it stands for,
    so showing it only pushes that row further away. A list at its very top stays there when a
    section opens above its first row; gpui's splice would otherwise keep the old first row
    anchored and open the section out of sight.
  - **The palette's field is bare.** 15 pt in a 44 pt row (a row plus 16), no frame and no fill,
    a `border_subtle` hairline under it (T3's `CommandInput`, Raycast's larger search than its
    rows). The rows are 32 (a row plus a half unit) on a 6 pt pad, radius 6 in the 12 shell, so
    the corners nest. The selected row is `overlay` with its title at 500 and its glyph in
    `text_secondary`; there is no hover wash, and the pointer moves the selection instead
    (Raycast: no hover highlights on a list), so the list has one highlight, the line ↩ runs.
    Section labels stand 8 above and 4 below their rows in every list that uses them.
  - **Recent is its own group.** An empty field's commands that come from history sit under
    "Recent", the rest under "Commands", so the list says why a command is there. The tiles
    still lead: ↩ on an empty field goes back to the last tile, which is what the palette is
    opened for most.
  - **The foot says what ↩ will do.** A 32 pt band on `raised` at the sheet's bottom radius,
    with no hairline; ↑↓ and esc on its left, and on its right ↩ with the selected line's verb
    ("go to", "run", "open"), as Raycast's action bar names its primary action where the eye
    ends. The caps are plates with no ring.
  - **Motion.** The palette and the picker rise 4 pt into place over the fade (120 ms,
    ease-out) while the layer under them fades in. (Amended 2026-10-03: what the keyboard
    summons fades in where it stands, with no travel, and every overlay leaves on
    `Motion::exit`; see the stage 3 entry at the end.) The phone's palette sheet reaches the top
    edge under the status bar and the island, its field below them, and comes down 8 pt with a
    fade over the sheet's 240 ms on the drawer curve. The navigator laid over the frame (a
    phone's drawer, an iPad's overlay) slides in from its leading edge while the scrim comes up,
    on the same time and curve; it carries its hairline on the trailing edge only, since it
    meets the window's other three. Entry only; under Reduce Motion, and in a headless frame,
    all of it lands at once.
  - **The picker speaks the palette's language.** No title row: the field heads the sheet and
    its placeholder says what the picker is for ("Jump to a session or add a window"). Its rows,
    selection, pointer and empty states are the palette's; "Nothing on the canvas or shareable
    on the worker" is a quiet line on the rows' edge, not a centred message.
  - Tests: `needs_you_lists_a_waiting_tile_scrolled_out_of_view`,
    `the_navigator_lists_what_needs_you_then_the_workers` (folded away, it leads),
    `a_tile_row_reads_its_age_or_its_state_then_its_place`,
    `a_worker_with_no_tile_says_so_quietly`, `the_palette_foot_names_its_keys` (the verb
    follows the selection), `the_palette_hangs_at_a_fifth_and_is_a_sheet_on_a_phone`, the
    picker's `the_chosen_row_is_scrolled_into_view`; app goldens `workspace-navigator(-dark)`,
    `agent-needs-you-navigator`, `palette(-dark)` and the iOS navigator and palette goldens.

- ✅ **The first run is a page with a place for the tailnet, and the key bar is the body's input
  row** (2026-09-27, design direction wave 5, §5.8–5.10). The first run was a web form: a blue
  mark tile over a heading, a status line, a bordered grey field and a button, left alone on
  white. The key bar was desktop chrome grown to 44 pt: white slabs with hairlines on the grey
  bar, "esc" and "tab" in lower case, and in the self-test's Split View it laid its keys out
  for the whole screen and cut them off at half of it.
  - **The page.** The app's name as a word (13/600, `text_secondary`), the heading at
    `display()`, one line under it, then two sections a step apart: what the tailnet answered
    and the address. A foot on the column's left edge says why there is nothing to pair
    ("Tailscale or your VPN encrypts every link…"). The block still sits a third down, now of
    the room over the foot. Linear's and Geist's sign-in pages are the model: a name, a
    heading, a short column, one line of fine print at the foot, and no hero.
  - **The tailnet has one place.** What answered is a framed list, radius `md` with its rows
    at `sm` inside a 2 pt pad so the corners nest, under "On your tailnet". Each row is the
    palette's two-line row: the kind's glyph, the name at the medium weight, "Server ·
    100.64.0.1" in tabular meta, a chevron, `raised` on hover. The per-row hairline is gone
    (§3.18). While it looks, or when nothing answered, the same frame holds one row that says
    so and what to do next ("Start the Slopty server on a machine there, or type its
    address."), so the page does not jump when the look ends. A Mac whose Tailscale is off
    says "Tailscale is not running on this Mac" (`net::Tailnet::running`), where it used to
    say that nothing answered.
  - **The field is a well.** 36 pt, `raised`, no hairline, caret-only focus, and its text on
    the rows' glyph edge. On the Mac its primary shares the row at the same height (T3 Code's
    `h-9` field). On touch the field is 44 pt with Paste, and the primary is full width under
    it. "Or type an address" labels it only when the tailnet's list is above. Over the
    workspace the same panel is the `lg` dialog, with no name and no foot.
  - **Key bar.** The bar takes the body's surface under the `border` hairline. On the canvas a
    cap was only visible with a hairline of its own. Caps are `raised` plates with no
    hairline, `overlay` while pressed, and the accent fill with its ink when armed. Words
    ("Esc", "Tab", "Paste", "Find") are `small()` and glyphs are `title()`, both at the
    medium weight, the way a keyboard sets its word keys smaller than its letters. The
    scrolled edge fades over 16 pt. The home band under it takes its surface. A screen
    reader hears each cap by name (`key_spoken`), so `a11y::key_name` left slopty-ui.
  - **Split View.** The key bar measures the size the app lays out in (`frame_size`), so at
    half an iPad it scrolls like a phone's instead of spreading three groups past its edge.
  - The drawer's trailing edge and sheet motion, and the palette sheet running up under the
    status bar, belong to the navigator's and the palette's owners and land with their wave.
  - Tests: app `the_first_run_sits_a_third_down_over_its_foot_on_every_device`,
    `the_field_and_its_action_share_a_row`,
    `the_panel_names_its_host_and_the_server_the_tailnet_found` (rows over the field, no
    tailnet here), `the_first_run_offers_the_workers_the_tailnet_found`,
    `every_key_is_spoken_by_name_and_shown_in_sentence_case`; goldens `first-run(-dark)`,
    `add-worker`, `ios-phone-*`, `ios-pad-*`, to be taken again.

- ✅ **Design wave 3, the frame: a status bar that says where you are, tabs on the midline**
  (its status bar superseded 2026-10-05 by "No bar along the bottom" below; 2026-09-27, `.research/design-direction-2026-09-27.md` §5.1 and §5.4). The user asked for
  a frame that stays minimal yet reads as finished, held to Warp, T3 Code, Linear and Geist.
  Most goldens showed a status bar holding "~" and a sub-millisecond round trip, and a title
  bar whose one workspace carried a "3 tiles" count.
  - **Where you are, as one path.** The status bar's left is the focused tile's worker, its
    directory (a repository's name and the path within it) and its branch, the steps a faint
    "·" apart (`text_muted` at `alpha::PRESSED`, the column dots' faint step; the document's
    `border` colour was all but invisible on light chrome). The worker is named with one
    worker too, since which machine a shell runs on is the first thing a remote tool says,
    and it leads in `text_secondary` while the rest stay muted, a breadcrumb read from its
    root. What the working tree changed follows the branch as figures in the muted tone with
    only the `+` and `−` in a diff's colours, as the tile header's counts are to be. The
    right's own compound readouts (a file's language and caret, the uploads) take the same
    faint dot.
  - **The round trip speaks when it matters.** It shows past the navigator's
    `RTT_SHOWN_FROM` (20 ms, one threshold for "slow enough to name"), in `warn` from 150 ms,
    and whenever the pointer is over the bar; how the tailnet carries the link shows on a
    DERP relay or under the pointer. It leads the right-hand cluster, which is pinned right,
    so appearing grows it leftward and moves nothing else; its figure keeps a slot pinned at
    its right, so a new sample moves only its digits. A quick link's samples no longer draw
    the workspace: `rtt_shown` asks the bar, which prints them only while slow or hovered.
    The frame time stays behind the stream stats.
  - **24 pt**, not 26: a notch under a tile's 28 pt header, so the bottom bar does not read as
    a second header row.
  - **Tabs on the bar's midline.** A tab's words sit in a row-high band (28, 44 on touch)
    centred where the traffic lights, the buttons and a lone name sit, and the tab's box runs
    from that band down through the bar's hairline. The active one fills its box with the
    content's colour inside `border` hairlines at `radii.sm`, in the medium weight and `text`,
    and joins the tile under it; the rest are regular weight in `text_secondary` and take
    `raised` in their band under the pointer, a row's hover rather than a tab's. Each name is
    laid out in the medium weight whichever it is drawn in, so the active tab is no wider and
    switching moves no neighbour. Sides are 12 pt each. The rollup's slot opens only for a
    mark; an always-empty slot pushed every name off centre, and a tab already changes width
    with its name. The two-line tabs (name over "3 tiles · 2 workers") and the lone name's
    count are gone: the navigator and the overview count tiles, and a count beside a name is
    badge soup in waiting. A closing tab folds over `Motion::settle` on `kit::ease_out`.
  - **Column dots**: those in view in `text_secondary`, the rest faint. Three dots in `text`
    were the darkest thing in the bar.
  - **The bell's badge** is 16 pt, with the accent's own white ink when only finished work is
    counted (it had near-black `fill_fg` on the new blue), and a 1 pt ring of the bar's colour
    that cuts it out of the bell's stroke, as a Dock badge is cut out of its icon. Under the
    pointer the ring takes the button's hover fill. It hangs a base unit off the button's
    corner, so the bell still reads under it.
  - Tests: `the_status_bar_says_where_the_shell_is_and_counts_what_is_shared`,
    `a_quick_round_trip_waits_for_the_pointer_and_a_slow_one_stands`,
    `the_link_path_shows_beside_the_round_trip_and_goes_with_the_link`,
    `the_active_tab_joins_the_content_on_the_bars_midline`, `a_lone_workspace_is_its_name_alone`,
    `the_workspaces_are_tabs_in_the_title_bar`, `the_status_bar_reads_the_focused_tile_and_its_link`.

- ✅ **Design direction, wave 3: tiles** (2026-09-27, `.research/design-direction-2026-09-27.md`
  §5.3, §5.7 and the empty workspace of §5.8). Amends **A waiting agent is marked once on its
  tile** (the slot), **Design review, round two** (overview covers) and the overview and empty
  workspace of the design review wave.
  - **Header.** The focused title is 500 in `text`; the others 400 in `text_secondary` (in
    `text_muted` an unfocused title read as disabled). The kind glyph sits a step under its
    title's tone. A header holds one filled chip at most, the state's: the agent's pill. Take,
    Mute or Muted, an upload's progress, the hooks offer and a forwarded port are ghosts, their
    tone in words at `radii.sm` with `raised` under the pointer, as T3 Code keeps fills for
    badges and leaves buttons bare. A waiting agent's slot keeps its kind's glyph: the chip
    beside the title says it, and the warn mark said it a second time. The tab shown in the
    focused column takes 500 too.
  - **Overview.** Each workspace is a card on the content's surface at `radii.lg` with a
    `border` hairline, a base unit (8) round its panes so their square corners sit well inside
    the round ones. Only the active card floats: `kit::elevate` and the keyboard's ring
    (`a11y::ring`, 2 pt of the accent outside a 2 pt gap), so where you are reads the way focus
    does. A shadow under every card said they all float. Names are 13 at 500 above their
    cards, counts in the meta size. "New workspace" is a ghost button a row tall on the last
    card's left edge, its glyph where the names start; the dashed slab, larger than a real
    workspace, is gone.
  - **Covers** are composed as a card is: the state or kind and the title (500) at the top
    left, the navigator's second line from the title's text edge, then as many of the tile's
    last lines as the card holds, in `caption()` and `text_muted`: a shell's screen
    (`TerminalView::tail`) and a file's first lines (`FileView::head`, off the rope) in the
    content's mono face, a note's lines with the Markdown marks left off. Niri and Warp show
    what a window holds in their overviews; a centred label over a blank card said only its
    name. Built only at the overview's shapes zoom, at most the rows that fit.
    (Superseded 2026-09-28: the overview draws true miniatures, MEASUREMENTS "the overview's
    miniatures against its word covers"; `TerminalView::tail` is gone.)
  - **The empty workspace is a start page on every empty workspace**, once the strip has come
    to rest on it (laid over a workspace still sliding in, it would hide the slide). It lands
    without a fade: a fade cut short by a tile arriving owed a frame after the strip was at
    rest. It was drawn only when no workspace had a tile, so the workspace the
    overview's "New workspace" opened was a blank strip. It hangs at `kit::MODAL_ANCHOR`, where
    the palette opens, not in the middle: a list read from the top, not a centred hero. Under
    the three ways to begin, *Recent* lists where shells stand on the workers that are up, one
    row per directory (the repository and the path in it, else the path's tail; its branch
    and, with several workers, the worker in the meta size), the latest used tile's first,
    then the latest started, five at most; a press opens another shell there. Then *Workers*.
    Sections part by space, never a rule.
  - Tests: tiles `a_header_holds_one_filled_chip_and_its_slot_does_not_repeat_it`,
    `the_status_mark_follows_the_agent_the_last_exit_and_the_link`; strip_marks
    `the_overview_lifts_each_workspace_and_offers_a_new_one`,
    `the_empty_workspace_offers_where_shells_stand`. App goldens to be taken again: overview,
    empty-workspace, tabbed-column, agent-needs-you, transfers, workspace(-dark).

- ✅ **Design critique round two, wave A: the shared pieces** (2026-09-27,
  `.research/design-critique-round2-2026-09-27.md` §2 #1, #2, #5, #12, #15, #16, #22 and §3).
  What several surfaces draw is drawn once, in `kit`, and a lint keeps it there.
  - **A diff's size is `kit::changes`** (`changes_at` at a zoom; `changes_text` for a line of
    plain text). Only the signs take the diff's tones; the figures are `text_secondary`,
    tabular; a side that is zero is left out, and no change draws nothing. The header chip
    coloured the signs while the diff head, the fold and "Changed 1 file" coloured the whole
    figure, and a red "−0" read as an error about nothing. Amp's fold sets its counts plain.
  - **The one gradient is `kit::edge_fade(edge, surface, depth)`**, a mask from clear to the
    surface along one edge of a scrolling region, drawn only while something lies past it:
    the palette's foot, the transcript's top (the first diff was sliced by the header's
    hairline) and bottom, the key bar's ends. Direction §3.1 allows exactly this gradient.
  - **`kit::FIELD_INSET`** is gpui-kit's medium field padding (10), pinned to
    `Size::Medium.input_px()`. What lines up with a field's text reads it, so the approval that
    takes the composer's place starts on the field's text edge rather than 10 pt left of it.
  - **One scrim.** Light's `Elevation::scrim` is the new `alpha::DIM` (0.16), about what an iOS
    sheet dims; at `TINT` a drawer turned the whole screen mid grey. Dark keeps `SCRIM`. Every
    dim is `kit::scrim`; the shade or the scrim's alpha read by hand fails the elevation lint.
  - **Motion goes through `kit::Pace`** (fade, settle, sheet: the `Motion` durations with their
    curves) and **`kit::slide_fade(el, id, from, pace, cx)`**: opacity and a few points of travel
    by `top` on an element in the flow, so nothing round it moves. Under Reduce Motion it is in
    place and opaque on the first frame, with no fade either.
  - **One wording per state.** `agent_status_word` is the state alone ("Needs approval"), and the
    new `agent_ask_text` is what is asked without it ("Bash · touch refused.txt"): no `$`, no
    "Needs approval:", the tool's own name not said twice. A row whose trailing word says the
    state takes the ask as its detail, and the header's pill, when it shows the word alone,
    shows the ask on hover. `kit::separator` is the faint "·" between facts on a line.
  - `IconName::CornerDownLeft` is embedded for the patch's "No newline" mark.
  - Lints in `kit.rs`: `a_diff_size_is_drawn_by_kit_changes`, `a_gradient_is_an_edge_fade`,
    `the_field_inset_is_the_kits`, and the scrim half of `a_floating_layer_wears_the_one_elevation`.
    The first three name the files wave B moves onto the kit; each such file leaves its list
    as it moves. Tests: `a_slide_starts_off_its_place_and_holds_still_under_reduce_motion`,
    `the_paces_are_the_motion_tokens`, `a_diff_size_drops_its_zero_side`, agents
    `the_ask_says_what_is_asked_without_the_state`, `a_state_has_one_word_whatever_its_detail`,
    theme `elevation_and_density`.

- ✅ **Settings open on a form; the TOML is one link away** (2026-09-27, design critique round
  2, finding 11, wave D). The dialog was the raw `settings.toml` with line numbers: nothing said
  what could be set, and a theme, a font or a cursor had to be known by its key. Raycast, Linear
  and Zed's settings window put a sectioned form beside the file, so the dialog now opens on one
  (`settings_form.rs`). A 168 pt sidebar holds the search field at its top, as System Settings
  does, and five sections as tabs a row tall: Appearance, Terminal, Input, Streams and Network.
  These are the tables the file has. There are no Agents or Keys sections, since the file sets
  neither. The page lists the section's rows under quiet group labels (Font, Cursor, Text,
  Behaviour, and so on). Each row is a label at 500, one line of meta in `text_muted` and its
  control on the right: a switch, a segmented choice, a stepper with its unit, a field for a
  host, an address list or a colour with its swatch, and a font picker that lists the installed
  monospace families, each in its own face. The families are found off the main thread the
  first time the list opens. Every control is a well on `raised` with no hairline: no cards,
  and no icon on a row. A query matches a row's words, its group and section, and its key as
  the file spells it (`max_bitrate`), and lists the matches under their section's names. A
  window too narrow for the sidebar shows every section in one column.
  - **One text behind both faces.** A row writes its key into the file's text a line at a
    time (`slopty_settings::edit::write`): the value is swapped in place with its comment kept,
    else the key joins the end of its table, else the table joins the end of the file, so an
    edit never reorders or drops a line. `slopty_settings::with_server` now edits through the
    same writer. The TOML field holds the same text. "Edit as TOML" shows it, and "Edit with
    controls" reads it back into the form, which applies it with its next change.
  - **A change applies as it is made; the file's face keeps its save.** System Settings, Zed's
    settings window and Linear apply a control as it moves and have no Save, so the form does
    too. A switch, a choice or a font hands the app the new text at once; a stepper and a field
    wait until the hand pauses for 300 ms (`settings_form::SETTLE`), so a run of clicks or
    keystrokes writes the file once. The dialog raises `SettingsEditorEvent::Apply(text)`, and
    `slopty-app` writes and reloads it through the save path it already had, keeping the dialog
    open. The form's foot is "Edit as TOML" and Done; Done, Escape, ⌘↩ and a click outside
    apply what is still waiting before they close, and so does turning to the file's face. The
    TOML face keeps Cancel and Save (⌘↩), since a half-typed file is not a setting: Save
    parses, writes, applies and closes, and Cancel drops what was typed there. A text the app
    refuses (a file broken by hand) shows its reason above the foot in either face.
  - **An invalid value is never written.** Each value is checked against its key alone before
    it joins the text (`schema::Field::check` parses a one-key file into `Settings`), so a
    colour that is not one or a host with a bad port stays in its field, the file untouched,
    and its row says why in `error` in place of its meta line, as an Alert that also describes
    the control. The reason goes as soon as the field is typed into again, since the fix is in
    progress, and the fixed value applies after the pause. A parse error in the whole file now
    reads "line N: why" rather than toml's first line, which only said where.
  - **The rows are the schema's.** `slopty-settings` derives `schemars::JsonSchema` on every
    table, and `schema::fields` walks that schema once into typed fields: the key, its title
    (`#[schemars(title)]`), its one-line summary (the doc comment's first paragraph), its kind
    (switch, choice with each variant's title, number, text, list, colour or font by `format`),
    its bounds (`range`), its grid and unit (`x-step`, `x-unit`), an example for a placeholder,
    and its default read off `Settings::default()` serialised. The form's rows are those fields;
    `slopty-ui` keeps only a presentation table (`settings_form_schema::LAYOUT`: each group's
    page, name and order). A key it does not name still gets a row, at the end of its table's
    page under the table's own title, so a setting added to `slopty-settings` shows up with no
    second edit. The bounds are the ones `slopty-app` accepts (mono 6 to 72 pt, chrome 8 to 32
    pt, scroll 0.1 to 10), where the form had narrower ones of its own. `slopty-app`'s
    `settings.rs` still holds those bounds as its own constants; reading them from the schema
    is the next step. `colors.ansi`, which the form had skipped, is a row now, as a list.
  - Keyboard: the search has the keyboard on open. ↓ goes to the first row's control, ↑ and ↓
    move from row to row and back to the search, ← and → move a choice or a stepper, and Space
    or Return turns a switch or opens the fonts. In the sidebar ↑ and ↓ change the section and
    → enters it. ⌘↩ from any control is Done. The roles are Tab, Switch (toggled), RadioGroup of
    RadioButtons, SpinButton (value, range and step), ComboBox (expanded) with a ListBox, each
    named by its label and described by its meta. A turned switch's knob slides on
    `kit::Pace::Settle`, the dialog rises 4 pt on `kit::slide_fade` (`Pace::Fade`), and under
    Reduce Motion both land at once. The page fades into the dialog's surface at an edge with
    more past it (`kit::edge_fade`), and a field's text sits `kit::FIELD_INSET` in. A stepper
    shows the value the file holds, even off its grid, and steps to the nearer grid point.
  - Tests: settings_editor `the_dialog_opens_on_the_form`,
    `a_change_applies_as_it_is_made_and_the_dialog_stays_open`,
    `an_invalid_value_is_not_written_and_its_row_says_why`, `the_keyboard_walks_the_form`,
    `a_search_finds_rows_by_their_words_and_their_key`,
    `a_switch_slides_unless_motion_is_reduced`; settings_form_schema `a_row_comes_from_the_schema`,
    `every_key_is_a_row_once`, `a_stepper_moves_on_its_grid`; slopty-settings schema
    `every_key_is_a_field_with_its_default`, `a_field_reads_its_type`,
    `a_value_is_checked_by_its_key`, `a_bare_key_still_reads`, and edit
    `a_write_keeps_the_rest_of_the_file`, `values_are_read_under_their_table`,
    `literals_read_back`; slopty-app `a_settings_change_applies_with_the_dialog_still_open`; app
    e2e `the_settings_form_edits_the_file`, which turns a switch and picks the dark theme with no
    Save and waits for the file and the dark theme under the open dialog (golden
    `settings-form`). The gallery's `settings` golden is to be taken again, since it is now the
    form.

- ✅ **The way in speaks a status as a line, lays out by the room, and the iPad key bar is one
  bar** (2026-09-27, design critique round 2, findings 8, 24 and 25 and the add-worker row of
  its motion table; `slopty-app`).
  - **A status is a line, not a box.** While the panel looks on the tailnet, or when nothing
    answered, one `kit::meta` line gives the words and what to do ("Nothing answered on your
    tailnet. Start the Slopty server on a machine there."), with no frame and no label. It is
    announced as a status by its words, and the step is its description. The stepped
    `Status::Running` mark leads it while looking, and `WifiOff` leads it only when Tailscale is
    off. The magnifier is gone. A framed row with a magnifier over a borderless field read as
    the search field to type in. Geist's and Linear's sign-in pages say a status as text and
    frame only what can be chosen, so the frame and its "On your tailnet" label come only with
    found rows. "Or type an address" follows as the field's label, so the steps no longer say
    it too.
  - **The field's row is chosen by width, not by input.** From 600 pt (`FIELD_ROW_FROM`, the
    width under which iPad Split View columns keep the phone's rules) the field and its action
    share a row. Narrower, the action is full width under the field. The iPad's first run was
    the phone's stack, with a 440 pt Connect button under its field. Where the tailnet cannot
    be listed (iOS), a meta line over the field says so, so the section's absence has a
    reason.
  - **The iPad key bar is one leading run.** Esc, Tab, Ctrl and ⌘, then the arrows, then the
    symbols form one run with `spacing.md` between groups, and the word keys (Copy, Paste,
    Find) trail. This is iOS's input-assistant layout. In three islands the arrows floated
    alone in the middle, about 350 pt from either hand's keys. Control is the word "Ctrl" at
    `small()` 500 like Esc and Tab, because "⌃" at the title size drew as a bare chevron that
    read as "up". The row's ends fade with `kit::edge_fade`, the one gradient chrome draws.
  - **The add-worker dialog arrives.** Its scrim goes from clear to `kit::scrim` while the
    dialog fades in and rises 4 pt (`kit::slide_fade`), both on `Pace::Fade`. Under Reduce
    Motion both are there on the first frame. The first run is a page, not an overlay, so it
    does not move.
  - Tests: app `an_ipad_key_bar_is_one_leading_run_with_the_word_keys_trailing`,
    `every_bar_spreads_on_an_ipad_and_scrolls_on_a_phone`,
    `the_field_and_its_action_share_a_row_from_a_tablets_width`,
    `the_dialog_rises_into_place_unless_motion_is_reduced`,
    `the_panel_names_its_host_and_the_server_the_tailnet_found`; kit
    `a_gradient_is_an_edge_fade` with `slopty-app` off its waiting list. Goldens: first-run(-dark),
    add-worker, ios-pad-first-run, ios-phone-first-run, ios-pad-terminal, ios-phone-terminal.

- ✅ **The lists: one plate, one wording, and motion** (2026-09-27, design critique round 2,
  waves B and C for the navigator, the palette, the picker and the inbox; §2 #1, #12, #21, #22,
  #28 and the lists' rows of §3).
  - **The selection is one plate** (`palette::Plate`). The selected row no longer paints its own
    `overlay`; it reports where it was laid out (`Plate::mark`, a canvas in its prepaint) and
    one plate laid under the rows (`Plate::under`) paints there in the same frame, since every
    element is laid out before any is painted. A list that scrolls carries the plate along
    with no frame of lag, the navigator's virtual list included. A move eases the plate's place
    and size from where it is drawn at that moment on `Pace::Settle`, so a second move
    retargets from the current value and nothing queues. A move sooner than the settle after
    the last one is a held key and lands in 60 ms; one sooner than 60 ms snaps, so a held arrow
    never trails the list. Under Reduce Motion it is on the row at once. The navigator, the
    palette, the picker and the inbox's Unread/All views use it; the inbox has no selected row,
    so its plate is the view's.
  - **The navigator seats its plate in the row** (`Plate::seat`, 2026-10-01). At rest, the
    plate is painted inside the selected row, before the row's content. Only the glide is
    painted under the rows. A canvas under a list keeps the background under its viewport from
    being one solid quad, so the list's scroll layer could never bake (tooling.md, "Scroll
    layers are on for macOS and iOS: the navigator and the face composite"). Test: palette
    `a_seated_row_holds_the_plate_at_rest_and_the_glide_is_painted_under`.
  - **A waiting agent is said once per place.** A tile's second line and a *Needs you* row take
    `agent_ask_text` ("Bash · touch refused.txt") rather than "Needs approval: …": the row's
    trailing word or the section's heading already says the state. The ask is muted; only a
    working agent's words keep its tone. A row's working-tree size is `kit::changes`, and a
    tree with no line changed shows none rather than "+0 −0".
  - **The palette.** The list's bottom fade is `kit::edge_fade(Bottom, elevated, 16)`. The foot
    is sentence case, as the key bar is: "↑↓ Move", "Esc Close", "↩ Go to", "Run", "Open". It
    opens with `kit::slide_fade` (4 pt rise on `Pace::Fade`; the picker too), and the field has
    the keys in frame 0. The phone's sheet comes down its whole height on `Pace::Sheet` over a
    `kit::scrim` that fades in with it, and it casts no shadow. `CommandPalette::leave` draws the
    way out in three quarters of the way in (90 ms fade, 180 ms sheet back up, nothing under
    Reduce Motion), takes no more keys or clicks, and returns how long its owner keeps it drawn.
  - `navigator::slow_rtt` is the one rule for naming a round trip unasked (from
    `RTT_SHOWN_FROM`, 20 ms); the palette's worker lines are to read it too.
  - The inbox drops 4 pt from the bell as it fades in (`kit::slide_fade`, `Pace::Fade`).
  - Tests: palette `the_plate_retargets_from_where_it_is_and_never_queues`,
    `the_palette_plate_settles_from_line_to_line`, `the_palette_leaves_quicker_than_it_came`;
    workspace `the_selected_row_sits_on_the_plate`,
    `the_inbox_drops_in_and_its_view_sits_on_the_plate`, and the updated
    `a_tile_row_reads_its_age_or_its_state_then_its_place`, `the_palette_foot_names_its_keys`,
    `repo_changes_show_in_the_row_and_the_bar`. `navigator.rs` and `palette.rs` left the
    `a_diff_size_is_drawn_by_kit_changes` and `a_gradient_is_an_edge_fade` lists.

- ✅ **The tiles and the frame: said once, and motion** (2026-09-27, design critique round 2,
  waves B and C for the tile, the title bar, the status bar, the strip, the faces and the
  notices; §2 #3, #6, #7, #10, #20, #23, #26, #27, #29, #30 and their rows of §3).
  - **The header's controls are one set.** An agent's face toggle left the readouts it split
    and sits with fullscreen and close in the trailing strip, shown on hover or focus; the
    strip is as wide as the buttons it holds, so the swap moves nothing. The readouts end on
    the state chip, alone.
  - **Rest shows the kind.** An idle agent keeps its glyph in the leading slot, as a waiting
    one does; the hollow ring read as an unticked radio button.
  - **The face can say "working".** `agent_mark` raises an idle or finished agent to working
    while its face shows a block streaming or a call of the last turn with no result (and no
    interruption after it). Hooks lag or are missing; the transcript then knows more. The
    header, the navigator's mark (through `tile_status`) and the status bar's count read it.
    It changes only marks. The workspace draws again only when that answer flips, not per
    streamed delta. The navigator's lines follow the mark: under a calm hook they say the
    face's summary, which names the call in flight (`ConversationView::mid_turn` lets the
    summary name it though the hook says idle), or nothing yet, and never the hook's "Idle".
  - **A finished command is marked once.** A good end reads as its time alone, in the meta
    size and `text_muted`; a failure keeps "Exit N · time". The header drops the unseen dot:
    the tile is on screen, and the navigator and the tab's rollup say it went unseen.
  - **A note says its title once.** An unnamed note whose page opens on its own title shows its
    progress as the header's title ("1 of 3 done"), else "Note", and no place.
  - **A page's host is its header's.** The header's place for a browser tile is the host, and
    the status bar no longer repeats it. Typing an address in the header needs an address mode
    of the rename field and a way to load a URL in `BrowserView`, which this change does not own.
  - **Uploads are said once, but the other way round from the critique.** The status bar counts
    the uploads of every tile but the focused one, whose header pill says its own and carries
    the only way to stop it. Hiding the pill for the focused tile would have taken away the
    cancel. The bar's right-hand readouts are parted by `kit::separator`, and its working-tree
    size is `kit::changes`.
  - **The column marks are segments**, 10 × 4 pt at `radii.xs` and a base unit apart, 8 pt from
    "+". Three dots after "+" read as the "…" button's glyph.
  - **A phone's bar is a navigation bar.** On a phone the bar shows the active workspace's name
    at 17 pt in the strong weight, with no tab row and no "+". What "+" opened leads the "…"
    menu, and the column marks follow the name.
  - **The overview.** The names and "New workspace" start on the edge of the panes' glyphs, and
    the active card's ring is the full accent (at the keyboard ring's strength it read grey).
    The keyed toggle stays on the layout's critically damped spring. The words fade in over its
    last 120 ms (`overview_words`, landing at 280 ms). They are not drawn while it closes, and
    are there at once where chrome does not move.
  - **Motion** (`chrome_moves`: not under Reduce Motion, nor the self-test). A tab that opens
    grows from nothing to the width its name takes on `Pace::Settle`, and its words fade in over
    the last 60 %. The active tab's fill is one plate that slides from the old tab's box to the
    new one's while the weight of the words swaps at once. The "+" and "…" menus drop 4 pt as
    they fade in (`kit::slide_fade`, `Pace::Fade`). A notice rises 4 pt in and, when its time is
    up, fades where it stands before it goes, no longer counted as up. A closing tile fades
    where it stood on `Pace::Fade`, with its glyph and title and no scale. The layout's 0.8
    shrink is no longer drawn, because text never scales. Nothing on the terminal's frame path
    moves.
  - Tests: workspace `the_readouts_give_way_to_the_controls_on_hover_and_nothing_moves`
    (updated), `an_idle_agent_keeps_its_kind_in_the_slot`,
    `a_face_mid_turn_marks_its_tile_working_while_the_hook_lags`,
    `a_finished_command_reads_as_its_time_alone`, `a_note_does_not_say_its_title_twice`,
    `the_overview_words_start_on_the_panes_glyphs`, `the_overview_words_wait_for_the_zoom`,
    `a_phone_bar_is_a_navigation_bar`, `the_column_marks_are_segments`,
    `chrome_moves_and_holds_still_under_reduce_motion`, and the updated
    `a_header_is_its_title_then_its_context_in_the_ui_face`.

- ✅ **A page's address is its header's field** (2026-09-27, design critique round 2 #27). The
  header of a browser tile works like the compact address bars of Arc, Safari and Chrome.
  - **At rest.** The header's place is the address: the host in the title's ink, between a
    quiet scheme and path (`text_muted`), with the path giving way first. A lone `/` is no
    path (`browser::address_parts`). It is a button named "Address" whose value is the whole
    address. A page with no title of its own is titled by its address, and that title takes
    the click instead. The navigator and the palette still say the host alone
    (`tile_place`).
  - **Editing.** ⌘L or a click on the address turns the header into a field in place of the
    title and the place. It holds the whole address the page shows, selected, so typing
    replaces it. ↩ goes there; Esc, or a click elsewhere or in the page, closes it with
    nothing changed. Text that is no web address keeps the field open with a notice. A bare
    host gets `http://`, as in the palette. A page that had the keyboard gives it to the
    field. It is the name field's machinery with a `Field::Address` purpose (`open_field`,
    `finish_rename`), not a second field. With no page focused, ⌘L opens "Open URL…".
  - **One source of truth.** A typed address becomes the item's, through a new
    `ItemOp::SetUrl { id, url }` (additive wire change, golden `client_item_set_url`). The
    worker takes it only for a browser item and only for an http or https address, as it
    does for a new item. A new address is what every client's page goes to, and the tools'
    view reads it too. Loading it only in this client's web view was the other choice. That
    would have kept a second address that only this client knew. The workspace serves
    forwards from the item's address, so a typed `localhost:3000` would also have loaded
    from this client's own loopback instead of the worker's. `reconcile_browsers` hands a
    changed item address to `BrowserView::go_to`: the header names it at once, the port is
    served anew, and the page loads when it is. The same address loads again. Where the page
    goes by its own links stays each client's, read back from the web view as before, and
    that read-back waits until the new address has loaded, so the header never flicks back.
  - **Back, forward and reload** are palette lines ("Page back", "Page forward", "Reload
    page"). ⌘[, ⌘] and ⌘R belong to the layout, so back and forward are ⌘← and ⌘→ (Chrome's
    other pair). They are bound in a `Page` key context that the workspace adds only while
    the focused tile is a page that does not hold the keyboard, so a text field in the page
    keeps them. Reload has no key. The header keeps its bare "←" and "→" (each while there is
    history that way) and "↻". `slopty_platform::web::WebView` gained `forward` and `Page::can_go_forward`.
  - Tests: workspace `command_l_opens_the_address_and_return_goes_there`,
    `escape_leaves_the_address_as_it_was`, `a_bad_address_keeps_the_field`,
    `the_address_is_a_button_at_rest_and_a_field_when_clicked`,
    `a_new_address_from_another_client_moves_the_page`,
    `command_l_without_a_page_opens_a_url`, `the_page_keys_are_bound_only_on_a_page`
    (`workspace/tests/address.rs`); `slopty-client`
    `a_browser_takes_a_new_address_and_nothing_else_does`; `slopty-worker`
    `an_address_takes_only_a_browser_and_a_web_address`; browser
    `addresses_are_http_or_https_with_a_host` (updated).

- ✅ **Design critique round three, wave A: the shared pieces** (2026-09-28,
  `.research/design-critique-round3-2026-09-28.md` §2 #1, #2, #6, #14, #18 and §3). The pieces
  wave B draws from, each in one place with a lint that holds it there.
  - **`kit::pill(theme, tone, k)`** is a state's pill: `kit::pill_frame(theme, k)` filled with
    the tone at `alpha::FAINT`, its words in the tone at the medium weight. The frame is a fixed
    `kit::PILL_HEIGHT` (20 pt, T3's `h-5`) at the chrome's zoom, `spacing.sm` at the ends,
    `radii.xs`, the text centred at `small()`. Grown from a pad round the text, the agent's pill
    stood 23 pt in a 27 pt header, off centre and touching the hairline. A header's words that
    act (Take, Mute, the hooks' offer) wear the bare frame so they stand as tall beside it. The
    agent's pill and the finished-command badge in `workspace/agents.rs` are on it now.
  - **`Surfaces::band`** is a terminal block's head band: the content with 4.5 % of the ink in
    light (about `F4F4F4` on white) and 3.5 % in dark, so the band sits 3.5 L* off the content
    in both variants, as far as the bars do, and more than a unit off `panel`. The critique kept
    dark on the sunken `panel` step. That left an unfocused tile's header and its first band
    one colour in dark, the double header it names in light, so both variants take one rule,
    and the dark band is lit a step rather than sunk. The terminal moves onto it in wave B.
  - **`kit::duration(Duration)`** is the one way chrome says how long: "850 ms", "6.2 s" (a
    tenth under ten seconds, left off when it is zero, so a clock ticking whole seconds never
    shows "6.0 s"), "35 s", "1m 5s", "1h 4m", in tabular figures. That is Claude Code's and
    cargo's compact form, so the tray row no longer sets "1 m 05 s" beside cargo's "1m 04s".
    The critique wrote hours as "1h 04m". Minutes and hours both leave the zero off here, the
    way seconds under a minute already do. `conversation::rows::took(ms)` is kept only as a
    wrapper over it until the conversation's callers move.
  - **One state word.** `agent_status_word` is the word wherever a state is said beside
    something else: the pill, the navigator row's trailing word, an inbox row with nothing
    asked. A tool call is now "Working", not the tool's name. "Needs you" only heads the
    section that groups "Needs approval", "Has a question" and "Needs input".
  - **`agent_ask_line`** is what a waiting agent asks, as a row leads with it. A bare tool name
    becomes `tool_statement`, the approval card's sentence without "Claude" or a subject
    ("Wants to run a command", "Wants to use query from db"), so the inbox no longer titles a
    row "Bash".
  - Lints in `kit.rs`: `a_pill_is_kit_pill` (a pill's height from a pad at the chrome's zoom, or
    a tone's faint fill at rest) and `a_duration_is_kit_duration` (a figure then seconds, or
    minutes or hours then a second figure; a latency in ms and an age are not durations). Their
    lists name what wave B moves: `workspace/tile.rs` and `file.rs` for the pill,
    `terminal/view.rs`, `workspace/navigator.rs` and `workspace.rs` for the duration. Tests:
    kit `a_pill_is_twenty_points_at_its_zoom`, `a_duration_reads_one_way` and the lints'
    self-checks; agents `a_state_has_one_word_whatever_its_detail` (extended),
    `a_bare_tool_is_said_as_what_it_asks`; theme `the_head_band_shows_and_is_not_a_header`.

- ✅ **Design critique round three, wave B: the conversation face** (2026-09-28,
  `.research/design-critique-round3-2026-09-28.md` §2 #4, #5, #10, #14, #15, #19).
  - **One surface under the list.** The background work and the task list are the composer
    shell's top sections, a `border_subtle` hairline apart, as T3's composer holds pending work.
    Before, they were three raised boxes 8 pt apart. Rows are `density.row` tall, with the mark
    on the field's text edge and the trailing figures on the send button's edge. A work row
    reads name · state and time in `text_muted`. The mono last line sits right-aligned, cut
    from its start, and only while the work runs, so "Done · 1m 5s" no longer sits beside
    cargo's "1m 04s". A finished row leaves once the transcript shows its call (Verbose, an
    open turn) or at the next prompt, so it is never said twice.
  - **One model.** The composer names the model that answered the last turn, and the status
    line's only before any answer. A fold's figures compare against that same name, so the
    latest fold never repeats or contradicts it. The default permission mode reads
    "Asks permission", which is what it does; "Default" named the setting's key.
  - **The grant is read before it is pressed.** The approval drops "What always allows ⌄". What
    "Always allow" grants is one `meta()` line over the buttons, the command in the mono face:
    "Always allow stops asking for `npm test` commands in this project, for you". It wraps as
    one paragraph. Where every grant is kept in the same place, the place is said once, at the
    end.
  - **Facts follow their subject.** A changed file's `+a −r` follows its directory. Only a
    hover arrow, the way into the diff, sits at the right.
  - **Durations.** Every time the face says goes through `kit::duration`, and `rows::took` is
    gone.
  - **A title for the tile.** `ConversationView::first_prompt` is the first line of the
    session's first prompt, cut at 48 characters with slash commands passed over. It is there
    for the tile's header when the agent has not titled itself.
  - Tests: ui `background_work_sits_over_the_composer` (the sections in the shell, over the
    field), approval `always_says_what_it_grants`, figures
    `a_session_is_named_by_its_first_prompt`; e2e `the_face_shows_the_work_beyond_words` (the
    finished row gone in Verbose, "1m 5s") and the fold without the model it shares with the
    composer.

- ✅ **Design critique round three, wave B: the lists and the first run** (2026-09-28,
  `.research/design-critique-round3-2026-09-28.md` §2 #1, #3, #9, #11, #17, #18, #25).
  - **One agent, one row.** *Working* lists an agent only while its own tile row is out of
    sight (folded, scrolled or filtered away, or with no tile), the rule *Needs you* already
    kept. A tile row ends in `agent_status_word` ("Needs approval", "Has a question", "Working"),
    and "Needs you" is left to head the section.
  - **An agent at rest** says what it last said (the face's summary, else the hook's detail),
    with a single word quoted (“done”), and no state word at all: "Idle · done" read as two
    states (amended 2026-10-04 by "A resting agent's lone word is left to its mark"). Its age runs from the hook's `since_ms`, when it came to rest, not from the shell's
    start.
  - **One clock.** The navigator's ticking time is whole seconds through `kit::duration`, from
    "1 s", and the inbox's finished row takes `kit::duration` too.
  - **Workspaces on a phone.** The phone's title bar has no tabs, so its drawer heads the list
    with *Workspaces*: a row per workspace the bar would tab, with its tile count, then "New
    workspace" (left out while the active one is that new one). The active row sits on a plate
    of its own, because the list's plate stays with the focused tile.
  - **The inbox** leads a waiting row with `agent_ask_line`, so a bare tool reads "Wants to run
    a command", not "Bash". It hangs flush from the title bar. The gap under it had shown the
    tile header's pill as a sliver, and the shadow already lifts it.
  - **A way out on glass.** With no keyboard attached the palette's field row ends in a Cancel
    link, as iOS search does, and the scrim dims under it on an iPad as well as on a phone, so
    the tap outside that closes it is visible. The window picker does the same since
    2026-10-03 (below, "The palette offers what the focus can do").
  - **The first run** leads with the app's own icon at 40 pt, `radii.lg`, with "Slopty" at
    `title()` in the strong weight beside it. The icon is `assets/icon.svg`, embedded and
    declared at 120 px: GPUI rasterises an SVG image at its declared size and samples it down
    with no mipmaps, and 1024 px drawn at 40 pt shimmered at its edges. Under the heading the
    block keeps two sizes, the body's and `meta()`. The labels, the tailnet line and the
    connection status are all `meta()`. The switch link is at the body size, like Cancel beside
    it. The field is labelled "Or type an address" only under rows to press, and otherwise
    "Server address" or "Worker address". On iOS the line saying the tailnet cannot be listed
    stands where the list would. The foot sits on the bottom safe edge
    (`insets().safe_area`), as on the Mac. Only the block's room gives way to a keyboard.
  - Tests: ui `an_agent_in_view_is_listed_once_in_its_own_word`,
    `a_resting_agent_reads_its_last_word_and_its_age`, `a_phone_drawer_lists_the_workspaces`
    (`workspace/tests/nav_rows.rs`), `without_a_keyboard_the_palette_prints_no_chords`
    (Cancel and the scrim), `an_inbox_row_says_its_age_first_and_no_word_its_heading_does` (a
    bare tool, flush under the bar), kit `a_clock_ticks_in_whole_seconds`,
    `a_resting_agent_is_quoted_not_stated`; app
    `the_panel_names_its_host_and_the_server_the_tailnet_found` (the mark, the field's label).

- ✅ **Design critique round three, wave B: tiles, the frame and notes** (2026-09-28,
  `.research/design-critique-round3-2026-09-28.md` §2 #1, #7, #8, #15, #16, #20–#24).
  - **An agent's state is said once on its tile.** While the face shows an approval the
    header wears no pill: the card is the statement, and a pill 450 pt above it said the same.
    Over the TUI the pill is back. The status bar's "N working" counts only agents whose tiles
    are off screen, as it already did for uploads; one in view says so in its own header.
  - **A tile is named by what it is about.** An agent that has not titled itself takes its
    session's first prompt (`ConversationView::first_prompt`) once its face has read it, so
    two agents no longer read "Claude Code" and "Claude Code 2". One that titles itself keeps
    that title.
  - **A note has one name.** Its header says its title ("Release"), as the navigator and the
    palette do. How far its tasks got is a count at the end, "1/3" in `meta()` muted with
    tabular figures, among the readouts where a command's time sits. Identity beats
    de-duplication: the page's own heading may repeat it.
  - **Real checkboxes.** A task box is a notch under the prose size (0.95), `radii.xs`, a
    `border` hairline that turns `text_muted` under the pointer, centred on the first line. A
    done box is the accent fill with a drawn check (`IconName::Check`, 4 pt inside the box),
    not the font's tick. A done task's text goes `text_muted` with the strike at
    `alpha::STRONG`, where the whole row had been dimmed.
  - **A shell's place does not contradict its prompt.** A shell titled by its directory shows
    no place: the directory above it ("drop-here ~") read as the cwd, and the status bar has
    the whole path.
  - **The frame.** The status bar's left is one path, the server's word first, parted from the
    worker by the faint dot, and the word says what it costs: "Server unreachable · direct
    links only". The column marks are 8 × 3 pt, 3 apart; the columns in view join into one
    thumb in `text_muted` and the rest are `border`, so they no longer read as "2 of 3
    loaded". The overview's active card wears a 1.5 pt accent edge flush with it, as a
    selected thumbnail does, not the keyboard's ring outside a gap.
  - **An empty workspace says where things open.** Its three ways to begin carry their worker
    as meta ("on e2e-worker"). The recent places and the workers follow as before.
  - **A remote window on its way** is one block in the body: the calm mark, "Opening Safari"
    at `small()` in the medium weight, the worker in `meta()` under it. The header's slot
    keeps the kind's glyph until the first frame. The critique asked for the app's icon; the
    window list carries none, so that waits for a wire change.
  - **The pills.** Take, Mute, the hooks' offer, an upload, a port's two ends, the body's
    Restart and Close, and the file's changed-on-disk buttons stand on `kit::pill_frame`, and
    a silent link's seconds and a finished command's time on `kit::duration`.
  - Not done: the overview's panes still show a cover of words, not a miniature of the tile.
    Drawing each tile's last frame there is paint work on the frame path, which wants a number
    first.
  - Tests: ui `faces::the_approval_card_is_said_once`,
    `faces::the_status_bar_counts_only_agents_out_of_view`,
    `faces::an_untitled_agent_is_named_by_its_first_prompt`,
    `tiles::a_note_keeps_its_name_and_counts_its_tasks`, `tiles::a_place_does_not_repeat_the_title`,
    `tiles::the_columns_in_view_are_one_thumb`, `tiles::the_servers_word_says_what_it_costs`,
    `tiles::the_ways_to_begin_say_where_they_open`,
    `tiles::an_opening_window_turns_its_mark_in_the_body`,
    `markdown::a_task_box_is_drawn_not_typed`; the overview's edge in
    `strip_marks::the_overview_lifts_each_workspace_and_offers_a_new_one`. The e2e
    agent-needs-you scenario opens a second agent that titles itself, so its goldens show two
    agents each named once.

- ✅ **Design critique round three, wave B: settings** (2026-09-28,
  `.research/design-critique-round3-2026-09-28.md` §2 #12, #13).
  - **A row holds one line** (superseded 2026-10-04 by `settings.md`, "A description wraps to
    two lines and is never cut"). The line on what a key does is cut to one line with an
    ellipsis, and the whole text is that line's hint. The wells for a colour, a host and a
    font are 140 pt, down from 184, and a segmented choice keeps its own width. The colour rows
    had wrapped to two lines, so the rows ran 40, 56 and 48 pt and the rhythm broke. System
    Settings and Linear hold one line of explanation per row.
  - **The columns start together.** The page's first label takes the search field's top and
    height with its words centred, so it stands on the field's line and not 9 pt under it.
  - **The title is "Settings".** The file's name beside it repeated the foot's "Edit as TOML".
    It shows on the file's face, where the path is what is being edited. This supersedes
    round one's "the title is the file name".
  - **Keyboard** is a page that sets nothing. It lists the app's keymap as bound
    (`cx.key_bindings()`), not a copy of the tables, so the app's own ⌘, is there beside the
    workspace's keys. Each action is one line in the palette's words (else its name in
    words), with every chord it has on key caps, each chord once. A numbered family (⌘1 to ⌘9)
    is one span, "⌘1–9". The groups are General, Layout, Terminal, Conversation and Files; a
    text field's own editing keys are left out. A query finds bindings too. The palette stays
    where bindings are run; this is where they are read, in one list, as in Zed, Warp and
    Raycast.
  - **About** names the app, the version and the build ("Release build for macOS") on one
    quiet line, and links to the source, the changes and a new issue. The crate carries no
    revision, so the build is the profile and the system.
  - Tests: schema `the_keyboard_page_reads_the_keymap`; editor
    `rows_hold_one_line_and_the_columns_start_together`,
    `keyboard_and_about_read_what_is_bound_and_built`; e2e `the_settings_form_edits_the_file`
    (the running app's ⌘, and ⌘T, ⌘N on the Keyboard page).

- ✅ **The browser tile's own chrome** (2026-09-28, `.research/gap-audit-2026-09-28.md` §3 #6).
  A page does what a browser would. What it draws sat beside the native view, never on it, while
  nothing GPUI drew could cover one; since 2026-09-30 a script's dialog is drawn over the live
  page ("A browser tile's page is composed by the window, not laid over it").
  - **Pop-ups.** One delegate object answers WebKit for a page: navigations, `WKUIDelegate` and
    `WKDownloadDelegate`, the last two by selector (the bindings type most of `WKUIDelegate` for
    macOS only, and WebKit asks `respondsToSelector:` rather than conformance). `window.open`
    and `target=_blank` get no web view back; the address, put back on the worker's port,
    opens a page tile right of this one through `open_browser`, the path "Open URL…" takes. A
    tile is an item every client shares, so it opens by address, and a pop-up loses its
    `window.opener`: a sign-in that posts back to its opener does not complete. A page that
    closes its own window (`window.close()`, which WebKit allows a pop-up or a page with one
    entry of history, and reports as `webViewDidClose:`) closes its tile as ⌘W would, so "Undo
    close" brings it back. Pop-ups follow WebKit's own rule on macOS, where a script may open a
    window without a click.
  - **Dialogs.** `alert`, `confirm` and `prompt` put the scrim and an elevated sheet from the
    kit over the page, which the scrim keeps from the pointer, the keyboard in the sheet: ↩ is
    OK, Esc Cancel. A
    `Dialog` answers WebKit's handler exactly once, and dropping one (the tile closed) cancels
    it, since WebKit raises on a handler never called.
  - **Web Inspector** follows `[web] inspector` (on by default, debug and release alike;
    `docs/decisions/settings.md`, 2026-10-02). `isInspectable` opens a page to Safari's
    Develop menu only, so a page opened with the setting on also has WebKit's developer extras,
    and "Inspect page" in the palette opens the inspector's own window. Both are WebKit's
    private headers (`_setDeveloperExtrasEnabled:`, `_inspector` and its `show`), asked
    `respondsToSelector:` first, so a WebKit without them loses the command and nothing else.
    The palette lists the line on the Mac; on iOS Safari's Develop menu on a Mac is the way in.
  - **Find** is a bar above the page, like a file tile's, on WebKit's `findString:`: ↩ and ⇧↩
    (⌘G and ⇧⌘G) step, Esc closes. WebKit answers found or not, never a count, so the page
    counts: a script run with `callAsyncJavaScript:` in WebKit's client content world, which the
    page's own scripts cannot reach or patch, takes the needle as an argument (never pasted into
    source) and counts it case-folded in `document.body.innerText`. The bar says "3 matches",
    "1 match" or "No matches"; a count for text since typed over is dropped, and text found only
    in a frame the count cannot see shows no number rather than a wrong one.
  - **Zoom** is `pageZoom` on Safari's steps (50 % to 300 %), per tile. ⌘+, ⌘− and ⌘0 zoom a
    focused page and size terminal text anywhere else, one key for "bigger" whatever is in front.
  - **Downloads land on the client**, in `~/Downloads` (the app's `Documents` on iOS), with a
    row under the page: progress, Saved, the failure, Show in Finder, ✕ to cancel or dismiss.
    The bytes come through the worker's tunnel to this client's WebKit, and the human wants the
    file on the machine in front of them. Saving on the worker would send every byte back
    across the link and needs an upload verb, and the worker already holds whatever its own
    server serves. Closing the tile cancels its downloads.
  - Tests: platform `the_delegate_answers_what_webkit_asks_it`,
    `a_dialog_answers_once_and_a_dropped_one_cancels`,
    `a_download_gets_a_name_of_its_own_in_downloads`, `an_attachment_is_saved_and_inline_is_shown`;
    ui `workspace::tests::page_chrome` (pop-up, a page closing its tile, dialog, find and its
    count, zoom, download row) and `browser::tests` for the zoom steps, the find bar's words and
    a row's; e2e `a_blank_link_opens_a_tile_and_a_script_s_dialogs_are_sheets_in_it` (a real
    `_blank` link, `alert`, `confirm` whose OK closes the tile, ⌘F's count on a real page).

- ✅ **A folder tile** (2026-09-28, `.research/gap-audit-2026-09-28.md` §3 #9). A worker's
  directory is a tile of its own, `ItemKind::Folder { path }`, browsed in place, and the item
  moves with it (`ItemOp::SetFolder`, refused for any other kind, as `SetUrl` is for a page):
  every client, and the next start, shows the folder the tile is at.
  - **The listing is the client link's**, `ClientMsg::ListFolder` answered by
    `WorkerMsg::Folder { path, listing }`, not orchestration's `ListDir`, which goes through the
    server and orders by bytes. Both read through one function,
    `slopty_worker::listing::first`: only the names are gathered, the first so many in an order
    kept on a heap, and only those looked at. A folder's order is folders first (a link to one
    counts), then the name with its case folded. It is cut at `folder::FOLDER_ENTRIES` (2 000)
    with the whole count, so the frame stays near a file card's read on the control stream, and
    the tile's foot says how much it shows. A hidden entry (a dot name, or the Mac's
    `UF_HIDDEN`, which `~/Library` carries) is listed, its name muted. A folder's row counts
    what it holds: one more directory read per kept folder, on the blocking pool.
  - **The view** (`slopty-ui::folder`) is a path bar (each folder above a click away, `~` for
    the worker's home, the middle folded past three) over rows drawn as far as they are seen
    (`uniform_list`: a folder can hold thousands), the palette's selection plate under the
    selected one. A row is its kind, its name, a muted size or count and its age
    (`palette::age_label`), in tabular columns. ↑ ↓ Home End walk it, ↩ opens (a folder in
    place, a file as a file tile right of the folder), ⌫ and ⌘↑ go up and select the folder
    left; the header's arrow goes up too. A click opens once: the second click of a
    double-click would land on the next folder's rows. A row dragged past the terminal's slop
    goes out as a file promise through the path a shell's ⌘-drag takes, and files dropped on
    the tile go up into its folder (`Dest::Path`), which is listed again when they are there.
    The tile asks again whenever it takes the keyboard and after a link comes back.
  - **A path of unknown kind is asked first.** ⌘-click on a path while a command runs, a tool
    call's "View" and the palette's `Open <path>` send `ListFolder` and open what the answer
    says: a folder tile with the listing at hand, else a file tile. A path that names a line or
    ends in `/` says what it is and opens at once. The cost is one round trip before a file
    tile opened that way appears. ⌘-click at a prompt still types the editor command
    (`terminal/view.rs` `open_path`), which cannot know a directory from a file.
  - "Open folder…" is the palette with the focused shell's directory in its field, and a
    directory spelled with a trailing `/` offers "Open folder …" first.
  - Tiles that read alike are numbered within a kind: a shell and a folder at one directory are
    told apart by their icons, not "project 2".
  - Tests: worker `listing::tests` (order, cap, hidden, links, a file is not a folder), worker
    and client `items` (the move, refused elsewhere, reopened), proto `golden::folders`, ui
    `workspace::tests::folders` (keys, click, the way up, a path opened as what it is, a drop),
    e2e `tiles::a_folder_tile_browses_the_worker_and_opens_a_file_beside_it` (golden `folder`).

- ✅ **Design critique round four** (2026-09-28, `.research/design-critique-round4-2026-09-28.md`).
  The surfaces added since round three, none of them critiqued before: the folder tile, this
  Mac as a worker, the file tile's notices, the trackpad control and the zoom readout, the
  composer's attachment chip, and the settings' Keyboard and About pages.
  - **This Mac is a row to press.** "Use this Mac as a worker" was a second link under the
    panel's way aside, at the page's foot, dressed like "Connect to a server instead". For one
    Mac it is the likeliest first step, so it is a row as a found worker's is, under "On this
    Mac": the Mac's glyph, the words over what pressing does, and a chevron.
  - **The checklist says to-do, not failure.** A grant not yet given wore the red cross of a
    failed install. It waits on the human, so it takes the waiting tone beside its button, and
    red is left for an install that failed or a worker that never answered. A line that is not
    in the way (a tailnet that does not reach this Mac) wears the quiet ring. A missing grant
    says "Turn on slopty-worker in System Settings." on one line: the button opens the very
    list, and naming it had wrapped the line in two.
  - **A hidden entry is set back.** Its name had gone `text_muted`, which is the AA grey every
    size and age in the list already wore, so `.env` read like `README.md`. The whole row is at
    `alpha::STRONG` now, the "present but set back" of a read inbox row, as Finder dims one.
  - **A folder says where it is once.** Its header shows no place: the path bar right under it
    names every folder above, and the parent beside the title said it twice 20 pt apart. The
    first crumb's words stand on the rows' icons; a crumb's pad had put them 2 pt right.
  - **`kit::notice`** is what a body says when it has nothing to show: a mark at the large
    icon size in `text_muted`, what is so at `small()` in the medium weight, why in `meta()`
    under it, and any ways on below. A remote window on its way, a file too large, not text or
    not readable, and an empty, missing or wrong folder all say it so. A file had printed its
    summary as the notice ("binary, 2 KB", lowercase), and the cap's reason ran as one clause
    ("Too large to edit here: 40 MB, past 16 MB"). It now reads "Too large to open here" over
    "40.0 MB, over the 16.0 MB a tile opens". The status bar says no caret under a notice.
    The file's secondary button is on `elevated`, as `kit::button`'s is, not `panel`.
  - **An attachment is said once.** The chip over the composer's field was one of three
    readouts of one upload, with the header's "↑ 0%" pill and its accent line. The header now
    leaves an attachment's upload out, and the chip carries the way to take it off the draft
    (✕, "Remove <name>"), which stops the upload; a picture still being written lands on no
    chip.
  - **A toggle has one name.** The trackpad control was "Use as a trackpad", then "Touch the
    picture directly", and "Trackpad mode" in the palette. `kit::icon_toggle` is one name with
    a pressed state (`aria_toggled`): on, it rests on the selected fill with its icon in
    `text`, and the pointer keeps that fill, where the hover's lighter `raised` had read as
    letting go.
  - **The zoom readout floats.** It was a hand-made pill at the caption size on a 90 % veil of
    the chrome's grey, over a remote picture's own pixels, which could swallow it. It is
    `kit::pill_frame` lifted by `kit::elevate`, and says "150%", as the context and an upload
    do, not "150 %".
  - **One mark.** `kit::brand` (the app icon at 40 pt, the name at `title()`) leads the first
    run and now About, with the version and build under the name.
  - **The overview's miniature** sets a tile's own lines on the card's edge under its glyph,
    as the body's text stands under the header's; hung from the title's words they read as
    indented output. The meta still hangs from the title, as a list row's second line does.
  - The Keyboard page was looked at and left as it is.
  - The self-test can draw this Mac's checklist: with `slopty_e2e::THIS_MAC_ENV` holding a
    `doctor` report, the e2e build's app runs the flow against a stand-in that installs,
    restarts, opens and adds nothing. Without it the self-test has no entry at all.
  - Lints in `kit.rs`: `a_percent_sits_against_its_figure`. Tests: kit
    `a_toggle_rests_on_the_selected_fill_only_while_on`, `the_brand_is_the_app_icon_at_its_side`;
    folder `a_hidden_entry_is_set_back`; file `a_file_that_cannot_open_says_what_is_so_then_why`;
    ui `folders::a_folder_says_where_it_is_once_on_the_rows_edge`,
    `facts::overview_covers_say_state_place_and_worker` (the lines' edge),
    `remote::a_picture_pasted_into_the_composer_uploads_and_its_path_is_typed` (no header
    pill, the chip's ✕ stops the upload); app `this_mac::tests` (the marks),
    `this_mac_installs_then_is_added_once_its_doctor_is_green` (the row over the field). New
    goldens: `this-mac`, `file-too-large`, `conversation-attachment`.
  - A waiting agent's row says the action on its subject, not the tool's name: "Run touch
    notes.txt", "Edit src/main.rs", "Use query from db: users" (`agents::tool_action`). The
    tooltip keeps the exact tool (`agent_ask_text`). Test: `a_bare_tool_is_said_as_what_it_asks`.
- ✅ **UI wave five, the parts with no wire change** (2026-09-28,
  `.research/ui-wave5-2026-09-28.md` §1.2, 1.3, 1.6, 1.7, 1.8, 2.1, 2.2, 2.3, 3.1).
  - **Stop keys off the transcript too.** Stop in the composer and Esc offered themselves only
    while a hook said the agent worked, so a turn the face saw streaming before any hook had
    Send and no Stop. Both now follow `turn_running`: the hooks say it works, or the transcript
    shows a turn in progress. A hook can lag or be missing (the conversation-live render had a
    calm hook under a streaming call), and the tile's mark already says "Working" on the
    transcript's word, so Stop and Esc follow the same word.
  - **An attachment stays a chip until the message goes.** The draft never holds a worker's
    temporary path. Sending types the landed paths after the text, as a drop at the end of the
    draft would have. Whether a message is typed as a command is the text's call: a path starts
    with `/`, and a picture sent alone had gone as a slash command. Send waits while anything
    still uploads. A pasted picture's chip is the picture, 40 pt square, with its ✕ on a lifted
    disc and the upload as a 2 pt `accent_fill` line along its foot. A file keeps its pill and
    says "↑ 42%" only while it uploads. A paperclip "Attach files" leads the composer's foot.
    It asks the workspace's Files seam, which on the Mac is now the system open panel (it had
    said the picker was for iPhone and iPad only).
  - **The stream stats lead with the human numbers.** The overlay is a lifted panel at the
    picture's top right with one line: fps, time to glass (arrival to present, p50), bitrate
    and round trip. "To glass" takes the warning tone when its p95 passes two display periods,
    the round trip from 150 ms (`screen::RTT_WARN_FROM`, now shared with the status bar). The
    frame rate is never flagged, because a still window sends no frames. The frame's age is
    not added to "to glass" either: on a still window it grows without bound. "Details" opens
    the engineering lines two to five in the mono face. The panel keeps its presses from the
    remote window.
  - **A screen tile says what is wrong, and only then.** Its header gets a dot and one word:
    "Stalled" (`error_fill`), "Low bandwidth" (the worker has cut the bitrate for 3 s), or
    "Frames late" (more than five a second missing the display). A click opens the stats. It
    is read once a second, and a change is the header's only news.
  - **Mute is an icon toggle.** One name, "Mute", pressed while muted, `VolumeX` or `Volume2`,
    beside the trackpad toggle. The muted state is a choice, not a warning, so no `warn`. The
    palette says "Mute sound" and the View menu "Mute Sound" (2026-10-01; "Mute Window" until
    the sound became the worker's, one for all of its tiles).
  - **A queued message wears the prompt's bubble, set back** (`raised` at `alpha::STRONG`, the
    text secondary) with "Queued" or "Sending" under it, not a dashed outline. A queued one's
    hint says it sends when Claude finishes the step.
  - **The context ring opens a popover.** It says how full the window is in tokens and as a
    share over a 4 pt bar in the ring's tone, the session's cost, and the five-hour and
    seven-day limits with the five-hour reset, each flagged from 80 %. Those limits reached the
    client and were drawn nowhere. "Compact" types `/compact` as the composer types any
    command, and only while the agent is idle.
  - **Where the view is along the strip is the strip's own thumb.** The title bar's column
    segments are gone, after four forms that each read as a progress bar. A 3 pt thumb sits
    8 pt over the strip's bottom edge on a faint track. It shows while the strip scrolls or the
    pointer is within 24 pt of that edge, and fades 800 ms after both stop (at once under
    Reduce Motion). It is not a control, and it redraws the strip alone, never the chrome.
    The self-test's frames are still pictures, so a scroll there shows no thumb.
  - **Diff lines can be quoted into the draft.** A press and a drag over a diff's numbers pick
    its rows on an accent wash. "Quote in message" floats at the last one and appends "In
    `path` lines 40–41:" and the lines with their signs as a fenced diff, then shows the
    conversation with the draft. The Changes pane switches between "This turn" (since the last
    prompt, across the session's threads) and "Session".
  - Dropped after checking the code: none of the nine. Two brief details changed on the code's
    word, both said above: the frame's age stays out of "to glass", and the frame rate is never
    flagged.
  - Tests: face `stop_is_offered_while_the_model_streams` (failed before),
    `the_context_popover_says_the_limits_and_compacts`,
    `the_changes_pane_scopes_to_this_turn_or_the_session`,
    `diff_lines_picked_by_their_numbers_are_quoted_into_the_draft`; composer
    `an_attachment_stays_a_chip_and_its_path_goes_with_the_message`; workspace
    `remote::a_picture_pasted_into_the_composer_stays_a_chip_until_sent` (the paperclip too),
    `frame::the_strip_thumb_shows_only_while_the_strip_moves`; screen
    `health::the_plain_line_leads_with_the_human_numbers_and_flags_trouble`,
    `health::the_health_mark_is_silent_until_something_is_wrong`; figures
    `the_context_figures_say_the_window_the_cost_and_the_limits`; diff
    `a_quote_names_the_lines_and_keeps_their_signs`.

- ✅ **Remote desktops, round five** (2026-09-28).
  - **A display tile can stream a display made for this device.** On a worker whose caps say
    `virtual_displays`, the palette offers "Open a display sized to this window" on the focused
    display tile. The tile's physical stream goes and `OpenDisplay` asks for its body in device
    pixels at the window's scale and the screen's refresh, with this device's key kept beside
    `layout.json`. As the tile resizes, `display::Follow` sends the new size once the tile has
    held it for `display::SETTLE` (300 ms), since a mode change reconfigures the worker's
    displays. The same command, now "Back to the physical display", undoes it. One key has one
    display, so a second tile on the same worker takes it over from the first. A worker that
    streams a physical display instead says why in a notice. The choice lives in this client
    and is kept with the layout ("A phone's desktop comes at its own size", 2026-10-10).
  - **The stats overlay says 4:4:4.** The details' first line is the picture: size, capture
    scale, "4:4:4" or "4:2:0" read off the decoded picture's format (`xf44` or `444f` is
    full chroma), and the frame's age. Rate, bitrate and round trip left it, since the plain
    line above says them.
  - **The palette finds an agent by what it is about.** A session line matches on its agent's
    first prompt and the answer of its last turn, as far as the face has read the transcript,
    2 000 characters of each. The line prints neither.
  - Tests: client `display::tests::the_display_follows_the_tile_once_it_holds_still`; workspace
    `desktop::a_display_made_for_this_device_follows_its_tile`,
    `palette::the_palette_finds_an_agent_by_its_first_prompt_and_last_answer`; screen
    `the_overlay_says_4_4_4_from_the_decoded_picture`,
    `hud_shows_age_jitter_hold_present_cadence_and_the_verdict`.

- ✅ **A DERP relay that holds, waking a worker, Reconnect, and paste on iOS** (2026-09-29).
  - **A relay is news only once it holds.** Each worker keeps a
    `slopty_client::relay::RelayWatch`, fed every `WorkerMsg::Path` on the executor's clock.
    Until `DERP_NOTICE_AFTER` (10 s) on DERP nothing says it except the path itself under the
    pointer. Then the status bar shows "Relayed via fra — adds latency" for the focused worker
    in the muted tone, with the fix (`LinkPath::relay_fix`) in its tooltip and accessible
    description, and the navigator names the relay beside the worker, muted too. It was in the
    warning tone from the first DERP message before, which flagged every path still settling. A
    timer at `due(now)` draws the chrome again when the notice comes due. A direct path, or the
    link going, takes it away.
  - **Wake.** `HostActions` gained `wake`. The palette lists "Wake <worker>" (the `Power` icon,
    under commands) for each worker the app offers it for, and the hosts row shows a Wake
    beside Connect and Forget. A line or button that needs the app's window closure queues it
    in `pending_runs`, which the next frame runs with the window.
  - **Reconnect.** The conversation face's "<worker> is unreachable · your draft is kept" line
    ends in a Link button, Reconnect. It emits `FaceEvent::Reconnect`, and the workspace runs
    the app's `connect` for that worker: the same dial-now as the hosts popover's Connect.
  - **Paste on iPhone and iPad through the system's button.** The key bar's Paste (a shell's,
    when nothing is selected, and a remote window's) has the system's `UIPasteControl` over it
    (`slopty_ui::paste_key::PasteKey`). It is drawn from the caps' tokens: the text colour on
    the raised plate, the small radius, the word alone. A canvas in the cap places it while the
    whole cap is in the row's view. The render hides it first, so a frame without the cap
    leaves no button behind. A theme change makes a new one. A tap's `Pasted` goes to
    `WorkspaceView::paste_made`. That hands the pasted board to `ClipSync::pasted` as the read
    of the clipboard at its current change count, then pastes as the cap would: the text into
    the shell, a picture as the offer ahead of the picture chord, ⌘V into a window. So nothing
    reads the general pasteboard and no alert shows. Where the button cannot be made, the cap
    under it pastes as before.
  - Tests: workspace `bars::a_link_that_stays_on_derp_is_said_once_it_has_held`,
    `bars::a_sleeping_worker_is_woken_from_the_palette_and_its_hosts_row`,
    `remote::a_paste_through_the_system_button_reads_the_clipboard_no_further`; face
    `composing::an_unreachable_worker_keeps_the_draft` (Reconnect); `paste_key::tests` for the
    placement and the colours. The UIKit button itself is not driven by a test: a tap on it is
    the person's by design.

- ✅ **A remote tile pops out into a window of its own** (2026-09-29).
  - **The view moves and the stream does not.** "Open in its own window" (⌃⌘N, in the palette
    on a focused window or display tile) opens a native GPUI window whose root
    (`workspace::popout::PopOutView`) draws the tile's own `ScreenView`. The workspace keeps
    the entity, so the stream, its decoder, the paste hook and the pointer are the same ones,
    and the worker hears no `Close` or `Open`. The tile draws "In its own window" in its body
    instead, and a click there brings the window forward. A popped tile counts as on screen,
    so it is never parked. After a reconnect the window draws the tile's new view.
  - **The view follows the window it is drawn in.** `ScreenView` registers its blur,
    deactivation and bounds observers with the window it renders in, and again when that
    window changes. Moving between windows lets go of held keys and buttons first. The bounds
    observer is what asks for a new screen's refresh ("The stream follows the screen's
    refresh", video).
  - **Sized to the remote window.** The window opens one stream pixel to a device pixel, scaled
    down whole to 90 % of the screen, at the target's aspect. It has an ordinary title bar
    with the tile's title, so ⌘Tab's window list and Mission Control name it like a local
    app's window. Resizing it asks a remote window to take the new size (`Resize`, once the
    size has held for 250 ms), as widening a tile does. A remote window resized on the worker
    resizes the window back. A display letterboxes, and a display made for this device keeps
    its shape while it is out.
  - **Keys, clipboard, focus.** The picture takes the keyboard when the window opens. Every
    chord goes to the worker except ⌃⌘N, which puts the tile back, in the `PopOut` context,
    the way ⌃Tab stays the way out of a tile. While the window has the keyboard its tile is
    the focused one, and the worker's clipboard is watched for it even though the workspace
    window is not frontmost. Closing the window, the command again, or the tile going away
    returns the picture to its tile.
  - **Not done.** A relaunch puts a
    popped tile back in its window (below, "A relaunch opens the app as it was left"). iPad is
    skipped: a second window there is a second `UIWindowScene`, which the gpui iOS fork does
    not make, so the command is not offered on iOS.
  - Tests: `workspace::tests::popout::a_tile_pops_out_into_its_own_window_and_back` (the same
    view in a new window with the keyboard, the tile's placeholder, the palette's two
    labels, nothing reopened, and back), and
    `popout::tests::a_window_opens_at_the_remote_size_fitted_to_the_screen`.

- ✅ **A quick terminal slides down from the top of the screen** (2026-09-29; superseded
  2026-09-30: removed). A global chord (a Carbon hot key, ⌃\` by default) slid one kept shell
  down from the top of the screen in a non-activating panel over any app. The user judged it
  not useful enough to keep, so the panel, its settings section, the palette command, the hot
  key and the panel's AppKit dressing are gone, with no setting kept for old files. The
  keymap's chord parser, which it lent the `[keys]` table, now lives in
  `slopty_ui::keymap::chord`.

- ✅ **The palette's "Toggle quick terminal" hides a shown panel; drawing it changes nothing**
  (2026-09-29; the quick terminal superseded 2026-09-30, see above). What outlives it: the
  approvals the workspace asks of its workers follow its changes rather than its render, and
  `ssh::tests::reopening_the_panel_and_the_sheet_keeps_no_subscription` checks that the SSH
  sheet's and the settings dialog's subscriptions go with them.
- ✅ **The strip and the chrome read facts, never a tile's body** (2026-09-29, the gpui-fast
  switch). Under retention a view is built again when anything it read changed. A header that
  read its terminal for the title was built with every line the shell printed, a navigator row
  that read a face with every word the agent streamed, and a stream's health mark with every
  frame, while what they showed stayed the same. The workspace now keeps
  `workspace::facts`: what the chrome and the strip show of each body, copied in that body's
  observer and compared with the copy before. A change goes only to the views that show it: a
  shell's command to the navigator and the strip, its last command to the navigator, a face's
  one-line summary to the navigator and the strip, its header chips to the strip alone; a turn,
  a first prompt or an approval, which every mark and title reads, to the whole workspace. The
  strip hands each body its zoom and size by comparing with what it handed the build before
  (`strip::Handed`), not by reading the body back, and the chrome and the strip build from a
  read of the workspace (`draw::Draw`) that holds every write back until the read is over. What
  it costs: a fact the workspace forgets to copy is a stale title, which the retained-frame
  oracle (`retained::stale`) catches and a state assertion does not. Tests:
  `workspace::tests::retained` (each step drawn as from scratch),
  `workspace::tests::chrome::a_frame_of_motion_is_no_news_for_the_chrome`,
  `retained::tests::a_view_changed_untold_is_stale_until_told`.
- ✅ **What a view keeps but does not show is not written through the view** (2026-09-30).
  Under retention an entity updated without a notify is a change for every view drawn inside a
  view notified since, so the terminal's key-to-glass record, written when a frame reaches the
  display, built the typed-into shell again with its strip's next build (a working mark's turn,
  a hover on a header) though nothing it shows had moved. The record now lives in a cell the
  view shares with its element (`TerminalView::latency_record`), and the element writes it
  there. The rule it follows: an update of an entity is news; bookkeeping that no render reads
  goes beside the entity, not through it. Test:
  `workspace::tests::retained::the_strip_built_again_replays_a_shell_typed_into`.
- ✅ **While an input method composes, its keys are its own** (2026-09-30). Our views take
  Enter, Esc, the arrows and Tab over a field in the capture phase (a menu's pick, prompt
  recall, closing a panel), which runs before the field sees the key. A Telex or kana word
  still marked in the field needs those keys to commit it or pick a candidate, and the face's
  command menu took the Enter meant to commit the word as a pick. Every such capture now stands
  aside while its field `is_composing()` (gpui-kit): the conversation face, the palette,
  project search, the picker, the settings form and editor, the navigator's filter and the
  tile rename. gpui-kit's own Enter already waits; its Tab still types over a marked word,
  which is the kit's to fix. Test:
  `conversation::view::tests::composing::the_keys_an_input_method_reads_are_its_own_while_it_composes`.
- ✅ **Dotted and dashed underlines are drawn as dots and dashes** (2026-09-30). The engine
  carries all five SGR 4:x styles; the terminal drew dotted and dashed as solid lines. A
  dotted cell now holds round dots as wide as the stroke is thick, evenly spaced, and a dashed
  cell a dash at each end (ghostty's pattern, so neighbours join into one dash), each piece
  snapped to device pixels at the window's scale, at the font's underline position and
  thickness. Test: `terminal::view::tests::dotted_and_dashed_underlines_are_drawn_in_pieces`.
- ✅ **The file tile parses again only as far as an edit reaches** (2026-09-30). Each line
  keeps the parser's state it starts in; a parse runs from the edited line down to the first
  line that starts in the state it started in before, and the first parse starts as the text
  arrives rather than after the typing pause. A keystroke's parse in a 2 000-line file went
  from 180–210 ms to 0.3–0.8 ms (MEASUREMENTS, "a keystroke's parse in the file tile"). Test:
  `highlight::editor::tests::an_edit_is_parsed_again_only_as_far_as_it_reaches`.
- ✅ **Two stale frames were GPUI's, and the forks fixed both** (2026-09-30). A file opened at
  a far line showed no text for its first frame, because gpui-kit's editor laid its lines out at
  the old scroll; gpui-kit `fbb7e913` lays them out at the caret's, and the file tile no longer
  asks for a second frame. A caret started by a focus listener was not drawn, because a notify
  raised in the draw's focus phase woke nothing; gpui-fast `8e135d4` wakes the window for it.
  Tests: `workspace::tests::remote::a_caret_started_by_focus_is_drawn_as_from_scratch` (no
  longer ignored) and the app self-test's 20 000-line file.
- ✅ **A page's next dialog keeps the keyboard** (2026-09-30). Answering a script's dialog hands
  the keyboard back to the workspace in its next build; a page that asks again at once (an
  alert then a confirm) put up a sheet that build then took the keyboard from, so ↩ went to
  the workspace. The workspace now leaves the keyboard with a dialog that holds it. Test: the
  app self-test `tiles::a_blank_link_opens_a_tile_and_a_script_s_dialogs_are_sheets_in_it`.
- ✅ **What the strip shows of a file, a page and a folder is a fact** (2026-09-30). The header
  read the file tile's unsaved mark, the page's address and back button and the folder's parent
  straight from the bodies, so every caret blink and page load built the strip. They are copies
  in `workspace::facts` now, taken in each body's observer and after the workspace's own updates
  made while drawing, which no observer hears. Test:
  `workspace::tests::retained::a_file_tiles_caret_blinks_without_building_the_strip`.
- ✅ **One frame of motion is asked for at a time** (2026-09-30). The strip's build and a drag's
  edge scroll each asked for the next frame, and every answered frame asked again, so the
  callbacks multiplied and a held drag scrolled faster the longer it was held.
  `strip::Drawn::motion` keeps one request outstanding. Test:
  `workspace::tests::retained::a_drag_held_at_the_edge_scrolls_one_frame_at_a_time`.
- ✅ **The stale-frame oracle judges what stands still during motion** (2026-09-30). A frame
  whose two scratch draws disagreed was not judged at all, so a view left stale beside a
  spinner passed. Now the lines both scratch draws agree on must be in the frame shown. Test:
  `retained::tests::a_view_changed_untold_is_caught_beside_motion`.
- ✅ **The app's root reads no view** (2026-09-30). It read the workspace for the key bar's
  target, so every workspace notify built the root and all it holds. It keeps the target it last
  saw and moves it only from the workspace's observer when it changed. Test:
  `tests::the_root_is_not_built_for_the_views_news` in `slopty-app`.
- ✅ **Reduce Motion is watched, not polled** (2026-09-30). An observer of the system's
  accessibility notification (`slopty_platform::motion`) replaces the settings poll's read, so
  the change lands at once and the poll no longer asks AppKit.
- ✅ **⌘-click on a path opens its tile and types nothing** (2026-09-30). It typed
  `$EDITOR path` at the prompt, and every session's `$EDITOR` is Slopty's own, which opened the
  same tile and held the shell until it closed. A file too large for a tile still offers a
  terminal editor, which skips Slopty's own for `$VISUAL`, else `vi`. Tests:
  `terminal::view::tests::cmd_click_on_a_path_opens_its_tile_and_types_nothing`,
  `terminal::url::tests::paths_are_found_with_their_line_and_nothing_else_is`.
- ✅ **The file tile's parser states are shared** (2026-09-30). A state kept per line held
  81.5 MB for the largest file coloured; a state equal to one of the last 64 kept is shared, which
  holds 27.4 MB (MEASUREMENTS, "a keystroke's parse in the file tile").
- ✅ **Momentum is what GPUI says it is** (2026-09-30). gpui-fast now reports the momentum after
  a swipe as `ScrollWheelEvent::momentum_phase`, on the Mac and in its iOS touch recognizer. A
  remote picture forwards those phases as they are instead of guessing them from a run of moves
  after the fingers lift, which read a smooth-scrolling mouse right after a swipe as momentum.
  The strip swallows only momentum after its own swipe, so that mouse scrolls the shell under
  it. The silence backstop stays for a close that never comes. Tests:
  `screen::tests::a_fling_over_the_picture_reaches_the_worker_as_a_gesture_and_then_as_momentum`,
  `workspace::tests::a_mouse_scroll_after_a_strip_swipe_reaches_the_terminal`.
- ✅ **A pointer moving over a shell builds nothing** (2026-09-30). The shell's element read the
  pointer's position in prepaint, and its move listeners updated the view on every move, so
  every shell under a moving pointer was built again with the next frame of anything around it.
  The element now lets the scrollbar go when the window goes inactive or the grid moves, and the
  listeners update the view only for news (see ARCHITECTURE, "Drawing under retention"). Entering
  a shell still builds it once: GPUI's hover reads change then. Test:
  `workspace::tests::retained::a_pointer_moving_over_the_shells_builds_none_of_them`.
- ✅ **A frame of a spring builds a shell once** (2026-09-30). The grid measured in prepaint moved
  or zoomed with every frame of a spring, and the shell asked to be built again for it, so each
  moving shell was built twice a frame. It lays its block headers out at the zoom handed to it
  before the frame, and asks again only when the grid's own size changed or a still pointer now
  hovers another block. Test:
  `workspace::tests::retained::a_frame_of_the_spring_builds_each_shell_once`.
- ❌ **A focus change still draws every view again** (2026-09-30). Notifying only the views a
  focus change touches (the strip, the chrome, the overlays, the bodies the focus left and
  entered) instead of refreshing the window measured no faster: GPUI builds a shell the focus
  never touched in the same frame whether it is notified or not. Worth doing once gpui-fast
  records a read of the focus as it records a read of the pointer (MEASUREMENTS, "the fork
  owner's five retention changes").
- ✅ **A paste is judged by the mode when it arrives** (2026-09-30). Paste protection judged a
  paste by the bracketed-paste mode of the last frame the client had, and the program may have
  turned the mode off since, so a line break could run a command unasked. The worker, which
  alone has the mode as it is, now judges it (`Engine::paste_is_safe`): `TermRequest::Paste`
  carries `confirmed`, and an unconfirmed paste that would run something comes back unwritten
  as `TermEvent::PasteHeld`, which puts up the confirmation strip; ↩ sends it again confirmed.
  The client's own check is gone. With `[terminal] paste_protection` off, and for what the
  person composed (the conversation face's composer, an agent's first prompt), a paste goes
  out confirmed. Tests: `a_paste_that_would_run_under_the_mode_now_comes_back_unwritten` in
  `slopty-worker`'s `session_actor`,
  `terminal::view::tests::a_paste_the_worker_holds_back_waits_for_a_confirmation`, and
  `a_paste_is_safe_by_the_mode_as_it_is_now` in `slopty-engine`.
- ✅ **What a shell hands over shows beside it, and a page not asked for waits for a yes**
  (2026-09-30). The worker wave's handoffs reach the app (`workspace/handoffs.rs`, over
  `slopty_client::handoff`; the rulings are terminal.md, "A shell's browser and editor are the
  client's").
  - **Declared first.** Every link starts with `HandoffCaps { open: true, edit: true }` before
    anything else goes out on it, since the worker hands nothing to a client that has not said
    what it takes. Files are always taken: the workspace is the window.
  - **A page.** One the person just asked for opens in the default browser. It opens behind
    (`slopty_platform::open_url_behind`, `NSWorkspaceOpenConfiguration.activates = false`)
    when a remote window or display is in front, so the stream keeps the screen. Any other
    page is held back in a notice: "{shell's title} wants to open **{host}**", the host in the
    medium weight, or in the warn tone and semibold for an address built to deceive (a user
    name before the host, a look-alike international name). Under it, muted, why it was held
    back and how long ago it was asked. The full address is in the notice's hint, in the
    monospace face, since it is read character by character to judge it: the mono face's
    third use (`kit::tests::the_mono_face_is_for_ports_and_the_settings_file`). "Open" and
    "Dismiss" are its actions, and it stays 20 s (a word stays 6 s), since it waits on a
    choice. Once the notice goes the page goes with it; the palette line that opened it later
    was deleted on 2026-10-06.
  - **A file.** It opens in a file tile right of the shell that asked, focused, at its line.
    One asked again after a reconnect is the same tile, brought forward. While a program waits
    on it, a line under the header says so, in the accent's wash under the shell glyph, with
    "Done" (secondary) and "Give up" (ghost). Done saves and answers the program only once the
    worker has written it; text typed meanwhile is saved first; a conflict or a failed save
    leaves the program waiting until the person settles it and asks again. Closing the tile is
    Done. Give up answers at once, unsaved. ⌘↩ is Done: bound in the file's context and over
    the editor's own ⌘↩ (a new line), with a handler only while a program waits, so the
    editor's ⌘↩ is back as soon as none does.
  - **The shell in front.** The app tells each worker which of its shells is in front of the
    person (`TermRequest::Focus`): one `true` as its tile takes the focus while the window is
    key, one `false` as the focus leaves it, the window resigns or the tile closes, nothing
    for a change that keeps the same shell in front, and the report again on every new link.
    It runs where the clipboard watch runs, on every change of the workspace.
  - **A paused agent and its pull request.** A `Waiting` agent says what it waits on ("Waiting
    on cargo test", "Waiting on 2 tasks", "Looping: {prompt}") under the working mark, calm
    and not attention. Its pull request rides on its header as words, not a chip (the
    header's one fill is the state's): the pull request glyph and `#1234` (`!1234` for a
    merge request), green when approved, red when changes are asked, muted as a draft, else
    secondary; a click opens its page. The worktree's name follows, muted, its branch and
    path in the hint. Both go with the agent.
  - Tests: `workspace::tests::handoffs` (the declaration on every link, an edit beside its
    shell answered once its save lands, Give up and a refused save, closing a waiting tile, a
    withdrawn edit and one asked again, a page opened or offered by its host until withdrawn,
    the focus reports each way and after a reconnect, the pull request on the header),
    `file::tests` (`done_on_an_unchanged_file_answers_at_once`,
    `text_typed_while_done_saves_is_saved_before_the_answer`,
    `a_save_lost_with_the_link_leaves_the_program_waiting`),
    `workspace::agents::tests::a_paused_turn_says_what_it_waits_on`, and the app self-test
    `a_paused_agent_says_what_it_waits_on_and_wears_its_pull_request` (golden
    `agent-waiting-pull-request`).
- ✅ **An unsaved edit survives a quit or a crash** (2026-09-30; hot exit). A file tile's edit
  lived only in its editor, so a crash, a SIGKILL or a phone that ended the app lost it.
  - **What the others do.** Zed (`decbf641`) writes each dirty buffer's whole text and the
    mtime it was based on into its SQLite database, one pass at most every 200 ms
    (`SERIALIZATION_THROTTLE_TIME`), and on restore lays the text over the disk's with the old
    mtime put back, so a changed disk shows as a conflict. VS Code (`47a634e2`) writes the whole
    text with mtime, size and etag to `Backups/<workspace>/<scheme>/<hash>` 1 s after typing
    stops, deletes it once the document is clean, and restores it dirty with the old etag, so
    the next save meets the conflict. Neither saves diffs. Zed keys its rows by the layout's
    item, and its open issue #55726 loses unsaved buffers that are not in the layout.
    Sublime's changelog records a lost very large file (4142) and a session torn by a crash
    mid-save (4126).
  - **What Slopty does** (`slopty_client::unsaved`, `workspace/unsaved.rs`). One backup per
    file tile, `<data dir>/unsaved/<blake3 of worker and item>.json` (directory 0700, file
    0600): the tile's item and path, the whole text, the final newline, the modification time
    the edit started from, and whether the disk had already moved under it. The item is the
    worker's, so the key outlives the app, and after a restart each backup goes back to its own
    tile. One whose tile went (another client closed it, its worker was forgotten) is kept. It
    gets a tile again at the first snapshot of its worker after the app next starts: a clean
    tile on its file takes it, and otherwise a new tile is opened for it. It is written by
    `slopty_platform::fs::replace`, the codebase's one way to replace a file: a temporary file
    ordered on the device (`F_BARRIERFSYNC`) ahead of its rename, so a crash mid-write leaves
    the backup before it. A temporary left behind is cleared at start.
  - **When it is written.** The first change after a quiet spell is written at once, and the
    rest no more often than every 200 ms (Zed's throttle; VS Code's debounce waits as long as
    the typing goes on). A large edit is written less often still, so the backups write at
    most 8 MiB a second: a 16 MiB file is kept every 2 s, not rewritten five times a second.
    A tile brings on a pass only when where its edit stands moves (`FileView::backup_mark`),
    never for its caret blinking. A pass reads no text on the UI thread: it compares marks, and
    for a tile that is behind it takes the editor's rope, shared in O(1). The text is read
    out, encoded and written off the UI thread, one pass at a time. On quit what is left is
    written there and then. Nothing extra is written as the app goes to the background: the
    next pass comes within 2 s at most, well inside iOS's background grace.
  - **Races closed.**
    - Changes to one file are numbered, and the store keeps the later of two whichever
      thread reaches it first. A write still on its way off the UI thread is never laid over
      the quit's, and a removal never lands after a later write.
    - A change counts only once it has finished. The flush on quit writes again anything on
      its way or failed. A failed write (the disk full for a moment) is tried again on its
      own after 1 s, then after twice as long each time up to 30 s. Before, it waited for
      another edit, so an edit left alone was only in memory until the next one.
    - **Two tiles on one file keep both edits.** The first version kept one backup per file,
      holding only the edit changed last, so an edit typed in a second tile on the file
      silently replaced the first's. Where that can happen: backups live on each device and
      only its own tiles write them, so two devices never meet in one store, and each app
      process has one window. It happens when one app holds two items for the same file on
      one worker. The worker's registry keys items by id alone, so that comes about in two
      ways: two clients open the file at the same moment (each checks its own copy of the
      registry before the other's item arrives), or ⌘Z brings a closed tile back after a new
      tile was opened on the file. One buffer per file, the other way out, cannot be
      enforced over a registry that clients share, and two edits made apart cannot be merged
      without asking. So a backup is per tile, a tile closed for good lets go of its own
      backup and no other, and a kept edit given a tile again is written under the new tile
      before the old backup is removed. Tests: `two_tiles_on_one_file_keep_both_edits`
      (failed on the per-file store, which kept one of the two),
      `two_tiles_on_one_file_each_take_their_own_edit_back`.
    - A save answered after the text moved on moves the backup's base with it, so the edit
      comes back after a restart as no conflict.
    - A crash after the worker wrote a save but before the backup went restores an edit the
      disk already holds: it comes back clean, and the backup goes.
    - Backups are read at start off the UI thread. A tile edited before they are read keeps its
      own, later edit.
  - A backup goes only when its tile is clean (saved and answered, reloaded) or closed for
    good (after ⌘Z's window), never because a layout lacks it. A kept edit whose worker has not
    come back for a week is mentioned at start, in a notice that stays until the person picks
    "Discard", which lets such edits go, or "Keep". Nothing is dropped unasked. A palette line
    did the discarding until 2026-10-05; it was deleted (readiness 10-05 item 17) because the
    notice is where the question is asked, and a standing palette line for a rare start-up case
    was clutter (test `workspace::tests::unsaved::an_edit_a_week_old_is_told_of_at_start_and_discarded_on_the_notice`).
  - **A closed tile never leaves `$EDITOR` waiting.** A tile a program waits on answers it as
    "Done" would on close: it saves, then tells. When that save is refused, fails or loses its
    link, the program hears it was given up once ⌘Z's window has passed, and the edit stays
    kept here. While the save is still out, the window is extended until the answer arrives
    (`docs/decisions/terminal.md`, "A shell's browser and editor are the client's").
  - **Restore.** The tile's first read takes the kept text, unsaved, over the disk's version.
    When the disk moved since the edit started, the file is gone or not text, or the edit was
    already in conflict, it comes back as "Changed on disk" with Reload and Overwrite: the
    conflict is found when restoring, and a save meets the worker's own check in any case. An
    edit the disk already holds comes back clean.
  - **Cost** (`docs/MEASUREMENTS.md`, "keeping an unsaved edit"). With a 16 MiB edit on the UI
    thread, a pass costs 0.04 µs per tile, against 5 ms to copy the text out as the first
    version did. Off it, reading the rope out takes 3 ms and encoding it 8.5 ms. A 4 KiB backup
    writes in 1.5 ms, against 5.1 ms with the full flush `File::sync_all` does on Apple
    platforms, which the store used before. The tile holds at most `FILE_BYTES` (16 MiB).
  - Tests: `slopty-client` `unsaved::tests` (one backup per tile on its worker, a write cut
    short leaves the one before, a change overtaken by a later one is dropped, the user's alone);
    `file::tests` (`a_kept_edit_over_the_version_it_started_from_is_unsaved`,
    `a_kept_edit_over_a_moved_disk_is_a_conflict`); `workspace::unsaved::tests`
    (`a_large_edit_is_kept_less_often`); `workspace::tests::unsaved` (kept as typed and let go
    once saved, let go once closed for good, back on its tile or on a new one, a clean tile
    keeps another's backup, closing for good lets go only of that tile's edit, a save under an
    edit moves the base, a failed write written again on quit and tried again without another
    edit); `workspace::tests::handoffs` (the waiting tile's three ways out); and the app
    self-test `an_unsaved_edit_survives_the_app_being_killed`, which types into a file tile,
    SIGKILLs the app, relaunches it and finds the edit back on the tile, unsaved, the worker's
    file untouched.
- ✅ **The empty workspace is placed by this frame's layout** (2026-09-30). The app self-test
  failed at launch about six runs in ten with a stale frame: 127 glyphs of the empty workspace
  painted 5 px lower than a frame from scratch put them. The page's top was
  `drawn.viewport.height × MODAL_ANCHOR`, the strip's size as the last frame measured it. The
  first worker added brings the 24 pt status bar and its hairline, and the strip loses 25 pt.
  The strip's view is built in that frame from the old measure. Its paint records the new
  one, and `strip_resized` then notified only the workspace, not the strip's view. The frame
  drawn in between, 5 pt off (a fifth of 25), is what a `dump` then saw. It was not the gpui-fast
  pin, the fonts or the media lane. Now the page starts under a spacer a fifth of its own
  height, so layout places it in the frame it is drawn, and `strip_resized` also notifies the
  strip's view, since the tiles are placed from the layout it just changed. Test:
  `workspace::tests::the_empty_workspace_is_placed_by_this_frames_layout` draws one frame
  after the measure is left stale, and it failed before. Evidence: the self-test
  `a_twenty_thousand_line_file_is_edited_near_its_end_and_saved` failed 6 runs in 10 before and
  0 in 10 after, and `an_unsaved_edit_survives_the_app_being_killed` also passed 10 in 10
  (`cargo xtask e2e app --no-build --filter …` in a loop, logs in `target/logs/stale/`).
- ✅ **Slopty's mark in the app** (2026-09-30; `docs/decisions/brand.md`). The prompt
  `#.. / .#. / #.#` leads the empty workspace, centred over its words, and the About panel
  ("About Slopty" in the palette), over the name, the version and the build. (Amended
  2026-10-04: the panel is gone; "About Slopty" opens Settings › About, see workspace.md "One
  About, in the settings".)
  - It is nine circles drawn with GPUI (`workspace::about::Mark`), lit in `surfaces.brand`
    (`slopty_theme::BRAND`, `#4ac06c`, the same in both variants), with the unlit dots at
    `Theme::brand_unlit`: the brand's 0.2 on a dark content and 0.3 on a light one, where 0.2
    fades into the paper. Sizes come from the spacing scale, a dot twice its gap as in the
    art: 8 pt dots on the page, 16 pt in the panel.
  - The cursor dot (2,2) is lit while a worker's link is up and sits at the unlit level while
    none is (`WorkspaceView::light_marks`, from the workspace's own notify). It blinks at the
    terminal caret's cadence, `Motion::blink`, 600 ms each way (Ghostty's). Under Reduce
    Motion it holds steady and its clock stops. The mark is a view of its own, so a blink
    draws the mark alone and not the strip under it, and its clock runs only while it is
    drawn. The About panel is a small modal on `kit::backdrop` that Esc or a click beside it
    closes. It reuses the settings' version line (`settings_form::schema::about`).
  - Tests: `slopty-theme` `the_mark_is_slopty_green_with_its_unlit_dots_set_back`;
    `workspace::tests::about` (lit while a worker is reachable and dim when its link drops;
    blinks at the cadence without building the strip, steady under Reduce Motion); golden
    `empty-workspace`.

- ✅ **A browser tile's page is composed by the window, not laid over it** (2026-09-30, gpui-fast
  f994c34's native hosts; MEASUREMENTS "a browser tile's page composed by the window"). It
  supersedes "The browser tile's native view" and the placing rules of workspace.md's "A browser
  tile is a native page that follows its tile".
  - The page's `WKWebView` is the content of a native host (`Window::create_native_host`,
    `attach_view`), and the tile's body is the `native_view` element. The window puts the
    page under GPUI's layer, cuts a hole where the element is drawn and clips it with the
    element, so the strip cuts it and what GPUI draws after it is over it: the palette, a
    menu, a toast, a script's dialog, the app's own dialogs. Gone: the clip view,
    `browser::placement` and `Cover`, the prepaint that placed the pages after every tile,
    `MIN_ALPHA`, the toast's cut, and the app telling the workspace it was covered. The page
    fades with its tile, over the window's background rather than over GPUI's content (the
    fork's limit for a faded native), for the moment a fade lasts.
  - A tile drawn scaled (the overview, the strip zoomed out) shows the page's snapshot, since a
    page laid out that small would reflow. A snapshot is taken after each load and as the tile
    goes scaled, and not each time something covered the page, as it was before. A render
    cannot see a native view, so it draws the snapshot too, and the `browser` golden is
    unchanged.
  - The keyboard is GPUI's focus on the page's element (`track_focus`). A click in the page is
    GPUI's first, which focuses the element; the platform's first responder follows GPUI's
    focus, and a first responder that moves into the page focuses the element. The page
    taking the keyboard focuses its tile; focus that leaves it for nothing gives it to the
    workspace. Esc twice gives it back, and one Esc stays the page's. ⌃Tab and the
    workspace's other chords are GPUI's keymap's, which sees a key before the page does.
  - The edit keys. AppKit offers ⌘ keys to the page before the menu bar, and WebKit gives back
    what the page did not take, to the menu and then to the window. ⌘C and ⌘V reach the page
    through the Edit menu's Copy and Paste, which AppKit sends up the responder chain. Nothing
    sends ⌘A, ⌘X or ⇧⌘Z to a native view, and the menu's "Undo close" holds ⌘Z. So the keymap
    binds undo, redo, cut and select all under the page's element, deeper than the workspace
    (`PageBody > NativeView`), and the page does them itself (`web::Edit`), as the key monitor
    did. "Undo close" is the page's undo while the page holds the keyboard, as Safari's
    Edit ▸ Undo is a field's; from the menu with the mouse it undoes in the page too.
  - Not taken: keeping the key monitor. It saw every key and click in the process before
    AppKit, and it did the page's focus by hand; the fork's hosts do both from GPUI's own hit
    test and focus.
  - Measured with the palette opening and closing over a page beside five streaming shells:
    WebKit spends a third to a half of what it did (0.01–0.02 s against 0.04–0.05 s per 5 s),
    and the palette's frames are 0.2–0.3 ms shorter at p95. Moving the strip costs the same.
  - Tests: `a_page_is_its_tile_s_body_where_the_strip_draws_it`,
    `the_overview_shows_the_page_s_picture_and_a_failed_page_says_why`,
    `the_page_taking_the_keyboard_focuses_its_tile_and_giving_it_back_the_workspace`,
    `a_click_in_the_page_focuses_it_and_its_tile`,
    `esc_once_is_the_page_s_and_twice_gives_the_keyboard_back`,
    `the_edit_keys_are_the_page_s_while_it_holds_the_keyboard` (slopty-ui
    `workspace/tests/page_host.rs`, on the test platform's hosts with the web view stood in
    for), `a_letter_is_its_ansi_key_and_anything_else_no_key` (slopty-platform), and the app
    e2e `a_page_on_localhost_opens_in_a_browser_tile`. That e2e opens the palette over a page
    that stays shown, clicks into the page until the platform's first responder is in it, and
    sends ⌘Z, ⇧⌘Z and ⌘A to the page's window as AppKit does (`Command::PageKeys`), reading
    each edit back from the page's title. The e2e app is never the key window (it must not take
    the keyboard from the person at the Mac), so the application's key-equivalent and menu pass
    is not exercised live; the unit test covers the menu's "Undo close" reaching the page.
  - Open: the fork gives a native that held the keyboard no way to hand it back when the native
    is dropped, so the web view makes the GPUI view first responder itself as it goes. A
    test-mode present in the fork would let tests read where the host was placed and what it
    clipped; today they read the element's bounds.

- ✅ **A focus change draws only what shows the focus** (2026-09-30). gpui-fast `beb580e`
  stopped `focus` and `blur` from refreshing the window. Each question a view asks about the
  focus while it draws (`is_focused`, `contains_focused`, `within_focused`, `Window::focused`) is
  recorded and asked again before the next frame, and only the views whose answer changed are
  built again. Slopty's own whole-window refreshes after a focus move went with it: the
  workspace's pending focus and the settings editor taking the keyboard. An audit found every
  view that draws focus reading it through those questions, or through a field whose writer
  notifies. A focus move made while the window draws still asks for no frame of its own, so
  those two places ask for the next one by notifying a view that is built again on every focus
  move anyway: the strip, or the editor. `workspace::tests::retained`'s
  `the_keyboard_moving_builds_only_the_shells_it_moves_between` builds again the two shells the
  keyboard moves between, not a third, and fails with the old refresh.

- ✅ **The workspace reads no focus as a whole** (2026-09-30, gpui-fast `beb580e` and pin
  `867b4d4`). The fork stopped refreshing the window on `Window::focus` and `blur`: what a view
  asks through a handle (`is_focused`, `contains_focused`, `within_focused`) is recorded, asked
  again before each frame, and only the views whose answer changed are built again, while
  `Window::focused` counts as reading the focus as a whole. Slopty still read it whole in two
  places a frame builds, so every focus move built the workspace and the strip again as a
  refresh would have: `apply_pending_focus` compared the focus before and after giving what
  was asked, and the strip kept what had the keys (`Drawn::keys`) so `cacheable` could draw a
  focused body afresh when the keyboard moved inside its tile.
  - **Now.** `cacheable` follows the strip's own focus alone (the tile the layout focuses,
    which bodies are laid out by). A shell the keyboard leaves for its header's rename field
    asks for its focus in its input handler, so the fork builds it again without the handler
    and the typed name goes to the field (`a_tile_is_named_from_its_header`). The workspace
    gives the focus asked for as it draws and reads nothing back: whether the focus moved is
    not read.
  - **Cost.** The keyboard moved by a view of its own (a click in a body, a find bar giving it
    back) over 60 shells and 60 notes: 0.28–0.33 ms p50 to 0.13–0.14 ms, and the workspace and
    strip built for none of 420 moves, against every one before (`docs/MEASUREMENTS.md`). A
    move the workspace asks for is unchanged (1.07–1.13 ms to 1.02–1.07 ms): the layout moves
    with it, which builds both anyway.
  - **The fork shows a focus given while drawing** (amended 2026-10-03, gpui-fast PR #17,
    pin `448d3dac`). A focus move made while a frame draws asks the focus questions again, so
    the views still to be drawn in that frame show it, and the window asks for one more frame
    when the focus ended a frame elsewhere than it began, which builds those laid out before
    the move. The workspace's notify of the strip after giving focus went with the plumbing
    that fed it (`focus_asked`, and the faces' and boards' "gave the keyboard" answers).
  - **No frame of motion draws the chrome** (2026-10-03). A frame that changed which tiles
    were on screen still told the status bar to draw in the next, for a count of agents at
    work off screen the bar stopped showing long ago (`7ecf8ad7`). The layout's springs run
    on the wall clock, so a slower machine drew more frames of the overview opening, crossed
    more of those changes and drew the bar more: CI failed the bound of two that a fast Mac
    met. The notify went, and with it `chrome_next_frame`, its only use. The chrome now draws
    for the change alone, whatever the frames' pace:
    `a_frame_of_motion_is_no_news_for_the_chrome` opens the overview on a held clock at 60 Hz
    and at 240 Hz (21 and 79 frames, four changes of what is on screen each) and finds the
    chrome drawn for the change and in no frame of motion, the same at both paces.
  - Tests: `workspace::tests::retained`'s
    `the_keyboard_moving_on_its_own_builds_neither_the_workspace_nor_the_strip` (the two shells
    built again, a third not, workspace and strip not, each frame the one drawn from scratch)
    and `the_keyboard_moving_builds_only_the_shells_it_moves_between`; `focus_cache`'s replay
    tests; `a_tile_is_named_from_its_header`.

- ✅ **A project's board is a face of its orchestrator's tile** (2026-09-30,
  `docs/decisions/projects.md` "What the user sees", R8 of
  `.research/agent-orchestrator-2026-09-30.md`).
  - **Where it lives.** A project is the server's, and its orchestrator is a Claude Code session
    in a terminal tile like any other. That tile turns between its TUI and the board (⇧⌘J, the
    header's button, the palette's line for the project; the conversation face stays one ⌘J
    away). So the board sits where the person talks to the orchestrator, every client that has
    the tile can show it, and no second kind of item (a tile with no worker behind it) enters the
    registry, the layout or the navigator. Not taken: a board as its own tile kind, which needs a
    wire item on some worker for something the server owns, and an overlay, which hides the
    strip the agents run on. A task agent's ⇧⌘J goes to its project's board; a project with no
    orchestrator, or one whose tile is not here, says so.
  - **What it shows.** (Narrowed 2026-10-04 to the lanes alone: `docs/decisions/projects.md`,
    "The board is its lanes alone".) A header with the project's title, where its work lands (repository →
    target branch), its verifier and its live agents against its limit, over a bar that is every
    task at once, one segment per lane in the lane's tone. Under it, what waits on the person:
    each blocked task and a blocked orchestrator, with what its agent asks when this client
    hears it. Then one of three lenses (1, 2, 3, or the tabs): the tree of who split what from
    whom, down to Claude Code's own subagents running inside a session, each node with its
    worker, branch, pull request, open dependencies ("after #1"), subagents and to-dos, and its
    agent's own status line; the board (R8), each task in the lane of its most urgent
    descendant (needs you, failed, working, up next, verifying, ready to merge, merged), lanes
    side by side where the tile is wide enough and stacked where it is not; the timeline,
    newest first, one sentence per moment with its age: a report says its kind and first line,
    and a delivery names the agent it reached (the orchestrator, or `#3's agent`).
  - **Keys and clicks.** ↑ ↓ (or j k) walk the rows on the selection plate, ↩ or a click opens
    the node's agent in its tile with the keyboard in it. A node with no live agent, or one on a
    worker this client cannot reach, says why in a notice. The orchestrator's own row turns the
    tile back to its terminal.
  - **Mirror.** `slopty_ui::project::model::Projects` keeps the server's snapshot parts and
    changes, dropping a change at or below the snapshot's `seq` (projects.md, "Project changes
    are deltas"). A native leaf's change moves its node's counts, since the server does not send
    the card again for it. The timeline keeps its latest 256 entries. Live counts come from the
    cards (open assignments and the orchestrator), not from the snapshot's `Live`, which no
    change updates.
  - **Cost.** Boards are made the first time they show and handed what they show only while
    they show, compared before a notify, so a change elsewhere draws no board. The mirror holds
    each project behind an `Arc` that a change copies on write, so the hand-over is a pointer and
    an untouched project compares by address. An agent's status moving marks the boards for the
    next frame like any change to the layout does. Lanes stand side by side where two fit at the
    tile's zoom. The clock
    alone draws nothing: the timeline's ages move on once a minute, only while it shows. A new
    row slides 4 pt up into place as it fades in; under Reduce Motion it is there at once.
  - Tests: `project::tests` (the snapshot and its `seq`, the timeline kept once and bounded,
    the tree, lanes by the most urgent descendant, open dependencies, native counts, sessions
    to nodes and terminals to agents, every moment in words, an update copying only the board it touches, lanes by
    width and zoom); `workspace::tests::projects` (the tile turning to its
    board and back with the keyboard, ↓↓↩ to an agent's tile, the lenses by key, clicks that
    open or say why not, the server's changes with the retained-frame oracle, forgetting the
    server, the palette's line and a task agent's ⇧⌘J).

- ✅ **Closer to MonoCode: tone steps over frames, hairlines of the ink, prose at 15/1.6, the
  agents in the status bar** (2026-10-01). This is the second reference pass, with MonoCode's
  source tokens beside ours (`.research/ui-ref-monocode-2026-10-01.md`). Where we already
  matched, nothing moved: radii 4/6/8/12, 28 pt rows, a 13/12/11/10 type scale, and motion at
  120/160 ms on the same curve. MonoCode's most-used grey text (≈ `#767676` on `#171717`)
  falls under the WCAG AA line our surfaces are lifted to, so our text tones stay.
  - **Hairlines are the ink at an opacity** (`slopty_theme::Hairline`: the chrome's text at
    the old step's share, `border` 0.105/0.115 and `border_subtle` 0.06/0.065 of 255). A
    fixed grey was mixed for the content, so on the bars, the navigator or a floating sheet
    it sat a different step off each. Laid over whatever it crosses, it sits the same step
    off every surface, and over the content it is the old colour to within a level.
    `colors::hsla` takes either tone (`colors::Tone`), so no call site changed.
  - **A plan is a tone step, not a frame**: `band`, with no border and no rule under its head,
    and its head and body on one text edge. The composer's pending work (the background tray
    and the task list) is one `panel` step at the top of the shell, with no rule between the
    two. With one piece of work, neither opened and the tile at least `ONE_LINE_FROM` (560 pt)
    wide, they share one line: the work on the left, the tasks on the right.
  - **Conversation prose is 15 pt at 1.6** (`Typography::prose()` is base + 2,
    `prose_line_height`), for the prompt, the answer and the words being written. Markdown
    elsewhere keeps 1.5: a note, a plan, a code block. A task box is sized from the icon size
    beside its text, so a note's boxes did not grow. At our tile widths a line of prose holds
    about 85 characters in the 720 pt column and about 40 on a phone, both inside the range
    that reads well.
  - **The status bar says how every agent stands**: per worker, the counts of working (a turn),
    waiting (background work) and blocked (on the person), each with a mark in its tone, and
    each worker named once there are two with agents. Agents at rest are not counted. This
    supersedes the rule that the bar counts only working agents whose tiles are off screen,
    and that a blocked one is counted on the bell alone. The bell stays the way to the
    blocked one; the bar is the ambient count, steady whatever is scrolled into view.
  - **The dark theme is neutral** (decided on renders of both, side by side). Content goes
    from blue-grey `#16181d` to `#171717`, with no hue. The chrome's secondary and muted text
    go from `#b4b9c3`/`#8b919c` to `#b8b8b8`/`#8f8f8f`, both still lifted to AA. ANSI black,
    white and bright black go from `#1a1b1f`/`#c8ccd4`/`#7a8393` to
    `#1c1c1c`/`#cbcbcb`/`#838383`; the hues are unchanged. On grey, the accent and the
    status colours (a ticked box, "Allow once", the amber and green marks) are the only
    colour on screen, where blue on blue-grey blended. One retune came with it: at a 0.28
    share toward black the navigator sat only 0.82 L* over the bars, under the unit the
    ladder test holds, so the bars go to 0.30. The seven `*-dark` goldens moved. The light
    theme keeps GitHub's slate greys in the terminal (text `#1f2328`, ANSI greys
    `#24292f`/`#57606a`/`#6e7781`/`#8c959f`) and a faint blue in `text_secondary`
    (`#4b4f58`). Its surfaces are near neutral, since they are mixed toward `#1d1d1f`.
  - **A fold names what its work touched**: "Worked for 35 s · Edited notes.md · Ran 3
    commands · 9 more steps". It names the two weightiest kinds (edits by file name while
    one, then commands, reads, searches, subagents), then counts the steps left. "N steps"
    alone was a number with nothing to picture. Explore and task runs were already one
    grouped line each, so nothing else changed.
  - **The composer's foot is two chips**: the model (the agent's mark, its name, a chevron
    while the list can open) and the permission mode (a shield, or the mode's own icon; the
    warning tone only for bypassing permissions). Both are read-only projections of what
    Claude Code reports. The model comes from the transcript (the ids that answered the last
    turn), or the status line's name before any turn has. The mode comes from the transcript
    (the mode the last prompt was sent in) or from the `permission_mode` the agent's own hook
    sent with a permission prompt, whichever is newer by the worker's clock. Neither chip
    drives the TUI's menus. The model list types `/model <alias>`, as before, and the mode
    changes only in the terminal. The mode set by Shift-Tab between prompts, with no
    permission asked, reaches the face only with the next prompt: every hook carries it, but
    only the server's tree hears it today, so showing it at once needs a field on the wire.
  - Not taken: shortcuts inside controls, because keybindings live in the palette (CLAUDE.md),
    and plan usage in the bar, which needs an undocumented endpoint and the Keychain
    credential. Both are parked for the user (plan usage superseded 2026-10-04 by "Plan usage
    in the status bar, from what the agents publish").
  - Tests: `the_mode_chip_takes_the_freshest_word`, `a_fold_names_what_its_work_touched`,
    `the_status_bar_counts_every_workers_agents`,
    `the_status_bar_reads_the_focused_tile_and_its_link`, `the_readouts_say_what_they_count`,
    `background_work_sits_over_the_composer`, and the theme's hairline ladder
    (`the_surfaces_climb_bars_panel_content` and the derived-surfaces sweep, now through
    `Hairline::over`).

- ✅ **The navigator groups by worker, or by repository on request** (2026-10-01; superseded
  2026-10-03 by "The navigator groups by project; the machine is a facet"). Slopty
  reaches many hosts first, so the navigator lists each worker's tiles under it by default.
  The palette's "Group the navigator by repository" swaps the workers for repositories, and
  the same line reads "Group the navigator by worker" to go back. It is a palette line and
  not a button, because keybindings and modes live in the palette. The choice is kept with
  the device's layout (`layout::Navigator::lens`), beside the navigator's width and whether
  it is docked: those are per-device state the app writes, where the settings file is what
  the person writes.
  - A group is a repository path as the worker reports it (`SessionSummary::repo`). A shell
    joins the group of its repository. A file or folder joins the deepest repository of its
    worker's shells that holds it. Tiles in none go under *No repository*, last. The groups
    run by name, and each group's tiles run in order of attention across workers, each
    worker's own order kept within a class.
  - The header names the repository's directory. When two groups share a name (a fork, a
    second clone), each also says where it is, from the last two parts of its parent path.
  - Each row's second line names its worker, then the directory below the repository (none at
    its root), then the branch. The worker's name is what tells two checkouts at one path on
    two machines apart, under one header.
  - Not done, because it needs the wire: one repository cloned at different paths on
    different workers shows as two groups. Folding them into one needs an identity the worker
    would report, such as the origin URL or the root commit.
  - A group folds like a worker, for this run only. The *Needs you* and *Working* sections
    are the same in both lenses.
  - Test: `the_repository_lens_groups_across_workers`.

- ✅ **Polish against the MonoCode bar, round four** (2026-10-01, a review of the goldens).
  - **The board says what needs you once.** On the board lens, the *Needs you* lane leads
    the lanes and holds the question, so the band over the lenses keeps only the
    orchestrator, which has no card. Before, the band and the lane both listed the waiting
    task (`project-lanes`). A parent card that stands in a lane for a descendant says "Needs
    you in a subtask", where it said "needs you below".
  - **Settings' sidebar is a tone step.** It sits on `panel` with no rule beside it, as the
    window's own sidebar does, and the page stays on the sheet (`settings-*`).
  - **The palette on glass keeps its corners.** With no legend under it, the fade over the
    list's foot follows the sheet's rounded corners. Before, a square band cut them off
    (`ios-pad-palette`, `ios-phone-palette`).
  - **The navigator's filter clears the title bar on touch.** A touch row (44 pt) filled the
    48 pt bar almost edge to edge. The field is now the bar less a step at the top and at the
    bottom (40 pt), as on the Mac (`ios-*-navigator`).
  - **The e2e shells start at `~` again.** The harness gives the daemons their home by its
    real path (`/private/var`), so it is spelled as `getcwd` spells it. Once the scrubbed
    environment stopped passing `PWD` on, a shell had named its home by its full path in
    every golden. The scratch root keeps the short `/var` spelling: resolved, it pushed the
    worker's socket paths past the 104-byte limit.
  - The iOS goldens are accepted again, now that the simulator build links ImageIO. Besides
    the navigator's filter and the palette's corners, each one takes the neutral theme's
    darker hairlines, which landed while the simulator could not be built.
  - **The phone's terminal golden waits for zle's cursor.** Its cursor had flipped between a
    block and a bar from one accepted golden to the next (both focused). The command leaves
    the default block (the integration's `preexec` resets the shape), the next prompt draws,
    and only then does zle's line-init set the insert bar, so a capture taken as the prompt
    came back caught either one. The iOS e2e now also waits for the dump's `cursor_shape` to
    be `Bar` before it renders, and `ios-phone-terminal` holds the bar the iPad already had.
  - **A phone's key bar fades on its first frame.** The trailing fade over the key row read
    how far the row scrolls from its last layout, so a bar just shown drew one frame with no
    fade (the retained oracle caught it on the iPhone). The caps have fixed widths, so the
    row's overflow is known before layout (`key_row_overflow`), and the fades read it with
    this frame's offset.
  - **The palette's foot fades in its first frame too.** Whether its list runs on past the
    foot was read from the frame before and checked again on the next one, which an idle
    simulator may not draw, so the phone's palette was caught without its fade. The list's
    rows have no fixed height, so the fade is `kit::painted_while` the list runs on, judged
    once the list has laid out in the same frame.
  - **Onboarding lists are tone steps.** The tailnet's answers, the "Set up a worker" rows and
    this Mac's checklist (and an install's steps) sit on `raised`, with no hairline frame, and
    a row under the pointer steps up once to `overlay`, as a board card does (`first-run`,
    `add-worker`, `this-mac`). The terminal's prompt-row bands are the terminal lane's.
    (Amended 2026-10-03 for light: white cards on a quiet well, see "Two named elevations".)
  - **No golden carries the machine's live round trip.** Under load the loopback round trip
    ran 30 to 70 ms, so "50 ms" showed in the navigator and the bar of whichever goldens were
    taken then. The readouts now read a figure the view can pin
    (`WorkspaceView::pin_rtt_readout`), as the workspace clock can be held. The app's e2e
    server pins `slopty_e2e::SHOWN_RTT`, a local link's 1 ms, from its first frame. The dump's
    `rtt_us`, the predictors and a remote window's hold keep the live figure, so the latency
    measurements still see the real link. A remote window's stats overlay prints the pinned
    figure too: `ScreenView::set_rtt` takes the link's round trip, which times the pointer
    hold, and the one to print, which the workspace hands it from the same pin
    (`stream-window-stats` says "RTT 1.0 ms").
  - Tests: `the_board_says_each_thing_once_and_fills_its_tile` (no band row on the board),
    `a_pinned_round_trip_is_what_the_readouts_show`,
    `the_fade_follows_the_sheets_corners_where_it_meets_them`,
    `the_key_row_knows_its_overflow_before_layout`,
    `the_overlay_prints_the_pinned_round_trip_and_the_hold_keeps_the_links`,
    `a_long_list_fades_at_its_foot_in_its_first_frame`.

- ✅ **The file editor stays light: its helpers come from the text and the grammar, never a
  language server** (2026-10-01). The user wants the file tile for quick edits on a remote
  machine, not an IDE, and ruled a language server out as too heavy. What a light editor is
  expected to have was read from the primary sources (Zed's and Lapce's default keymaps and
  settings, Helix's book and source, Sublime Text 4215's shipped package, micro's help pages,
  Warp's editor actions). All six toggle a line comment (⌘/ in five), go to a line (⌃G in four,
  `line:col` in most), find and replace with regular expressions, pair brackets and quotes, and
  indent on ↩. Five of six add the next match to the selection (⌘D) and match brackets; most
  move and copy lines (⌥↑/⌥↓, ⌥⇧↓), guess the file's indentation, and complete words from the
  file. Only some have an outline without a server, EditorConfig, trimming on save, or format
  on save.
  - **Round one** (this entry): ⌘/ toggles comments in the file's language, ⌃G goes to
    `line`, `line:col` or `line,col` (the caret follows the typing, ↩ keeps it, Esc puts it
    back, as Zed's), ⌥↑/⌥↓ move the selected lines, ⌥⇧↓ copies them, the bracket pair at the
    caret is framed in a muted hairline and ⌘⇧\ jumps between its two ends, Tab follows the
    file's own indentation, CRLF line ends and a UTF-8 BOM are kept, and Markdown wraps. "Wrap
    long lines" toggles it per tile. Each is in the palette with its chord (`file::editing`).
  - **Already in gpui-kit and on:** closing pairs (a quote is not paired after a word
    character, so "don't" types as written), indentation on ↩ from the brackets, indent guides
    (now at the file's own width; at gpui-kit's default of 2 a four-space file drew two guides
    per level), ⌥-click for another caret and ⌥⇧-drag for a block. ⌘⌥↑/↓ stay the workspace's
    focus keys, so a caret is added with the pointer.
  - **The comment token is asked of the grammar.** bat's grammars (two-face) carry no
    `.tmPreferences`: syntect leaves its `metadata` out of a pack (`#[serde(skip)]`), and
    two-face loads none. So `Syntax::comment` parses `// x`, `-- x`, `# x` and the other
    common tokens from the grammar's start state and takes the first whose `x` lands in a
    `comment` scope, else the first block pair (`/* */`, `<!-- -->`) that does. Every grammar
    in the bundle answers with no table to keep, in microseconds. `--` is tried before `#`,
    which the MySQL flavour of SQL also takes. JSON answers `//`, as Sublime does (JSONC).
  - **Indentation is guessed as Helix and Lapce do**: a histogram of the indent *increases*
    between neighbouring lines over the first 10 000 (`edit::detect_indent`), tabs when tab
    lines outnumber space lines, the narrower width on a tie, and the ` *` lines of a block
    comment skipped. A file with no indented line gets four spaces.
  - **Line ends and the BOM are kept apart from the text** (`edit::Format`). A file whose
    every line ends `\r\n` is edited as `\n` lines and saved with `\r\n` again, so a new line
    ends like the others; gpui-kit's ↩ puts in `\n`, which made mixed files before. A file that
    mixes the two keeps its bytes, so a save changes no line the person did not touch. The
    worker still sends a file that is not UTF-8 as not text.
  - **The commands bind in `FileText > Input`**, a key context round the editor alone, so ⌘/
    and ⌥↑ do nothing in the tile's find and "go to line" fields, and they win over gpui-kit's
    `Input` bindings in the editor.
  - **Not built, judged:** a language server (ruled out); format on save. That would take a
    worker verb with a timeout, a per-project setting and a way to report a formatter's
    failure, for what the shell beside the tile does with `cargo fmt`, after which the tile
    reloads by itself. Zed and Lapce ship it off. Folding stays off: without a syntax tree its
    ranges would be guessed from indentation, which reads worse than none in a small tile.
  - **Round two:** ⌘F's field gains match case, whole word and regular expression toggles
    (⌘⌥C, ⌘⌥W, ⌘⌥R, and icons in the field), with the terminal's smart case while match case is
    off. It counts `3/12`, and says "No matches" or "Not a valid pattern". ⌘⇧H opens a replace
    field: ↩ replaces the current match and moves on, ⌘↩ replaces them all as one edit, so one
    ⌘Z takes it back. `$1` and `${name}` expand only with the pattern toggle on; otherwise the
    text goes in as typed (`file::find`, the `regex` crate). A search keeps at most 10 000
    matches and a 4 MiB program. ⌘⇧O lists the names the grammar marks as defined: the text in
    an `entity.name.*` scope, which is Sublime's `showInSymbolList` set, so a call is not
    listed but a definition is. The list is read off the UI thread, holds at most 5 000
    symbols, and narrows by every typed word in any case. The caret follows the chosen row, ↩
    keeps it and Esc puts it back, as "go to line" does. The status bar adds the indentation and
    any line end or BOM that is not the default: `Rust · Spaces: 4 · Ln 1, Col 1`.
  - **⌘D and ⌘⇧L need a fork change.** gpui-kit adds a caret with the pointer only; its
    selection set is private. `SelectNextOccurrence` and `SelectAllOccurrences` live in the
    fork (aislopware/gpui-kit#2, with no open counterpart upstream). The first press with a
    bare caret selects the word under it; each later press adds the next match of the newest
    selection, wrapping. ⌘D and ⌘⇧L bind in `FileText > Input`, and the palette lists both.
    A word the caret selected is then matched only where it stands whole, as Sublime Text and
    VS Code match it, so `n` does not take the `n` in `len`; a selection made by hand matches
    anywhere (aislopware/gpui-kit#5).
  - **No word completion** (cut 2026-10-05, prune #10). The editor offered the file's other
    words as one typed (`file::complete`). The editor is for a person's quick fix beside an
    agent that writes the code, and no frontier tool ships word completion there (Amp removed
    its Tab, Claude Code Desktop has none), so it went whole rather than hidden.
  - **EditorConfig is resolved on the worker, and the client acts on what it knows.** The
    worker reads the `.editorconfig` files above the file with `ec4rs` (the EditorConfig core
    tests pass with it, and Zed resolves with it), adds the specification's fallbacks, and
    sends every property as an open key/value list on `FileRead::Text` and `Body::Text`. The
    client acts on `indent_style`, `indent_size`, `tab_width`, `end_of_line`,
    `insert_final_newline` and `trim_trailing_whitespace` and passes over the rest (`Rules`),
    so a new key costs no wire change.
    - The indentation set there wins over the guess from the text, which fills in only what
      it leaves out.
    - `end_of_line` sets the first line break of a file that has none yet. A file that
      already breaks its lines keeps its own way, because converting it would change every
      line on a save.
    - `insert_final_newline` decides how a save ends the file.
  - **A save trims the trailing whitespace off the lines the edit touched**, and no others, as
    one edit in the editor, so the tile holds what went to disk and one ⌘Z puts it back. It is
    on by default, since only the person's own lines change. Markdown is the exception (two
    trailing spaces end a line there), and `trim_trailing_whitespace` overrides it either way.
    ⌘S, "Done" and "Overwrite" trim; a save the tile makes on its own does not, as gpui-kit
    edits only with a window.
  - Numbers in MEASUREMENTS.md, 2026-10-01, "the editor's helpers". Tests: `edit::tests`
    (indentation, line ends, comments, moved and copied lines, brackets, go to line),
    `find::tests`, `complete::tests`, `highlight::tests::a_grammar_says_how_it_writes_a_comment`
    and `symbols_are_the_names_the_grammar_defines`, the worker's
    `file::tests::a_text_read_carries_its_editorconfig`, and `file::tests::editing` with
    the real editor and the default keys. The goldens `editor`, `editor-dirty` and
    `editor-conflict` show one indent guide per level.

- ✅ **The window comes back from the Dock and ⌘N, and the menu bar is a Mac app's** (2026-10-01).
  - **Closing the window leaves the app running.** The `Workspace` entity outlives its window,
    since the quit hook and the link tasks hold it, so a reopen is only a new window wired to
    the same workspace (`slopty_app::window::open`, which replaces the old window's
    observers). A click on the Dock icon (`Application::on_reopen`), ⌘N with no window, or
    Window ▸ Slopty bring the window forward, or open it again where it stood. Inside the
    workspace ⌘N stays "new shell", since its binding is the deeper one. A notification
    clicked with no window opens it as well.
  - **The menus are the standard ones.** Edit holds Undo, Redo, Cut, Copy, Paste and Select
    All, then Find. The items name gpui-kit's text-field actions, which every field answers.
    AppKit validates each item against the focused control (`is_action_available`), so what
    the control cannot do is greyed. A shell answers Copy, Paste and Select All as its own ⌘C,
    ⌘V and ⌘A, and has no use for Cut, Undo or Redo. Cut, Copy, Paste and Select All carry
    their AppKit selectors, so a web page tile takes them too. Window holds Minimize (⌘M), Zoom
    and the workspace window, and AppKit fills in its window list because the menu is named
    "Window". Help opens the project's page and the settings' Keyboard page.
  - Tests: `a_closed_window_opens_again_on_the_same_workspace` (slopty-app), and
    `workspace::tests::menus::the_edit_menu_reaches_whichever_control_has_the_keyboard`.

- ✅ **A relaunch opens the app as it was left** (2026-10-01). `layout.json` keeps three more
  things beside the arrangement.
  - **The window's frame**: position, size, full screen, and the display's UUID. It reopens on
    that display, or on the main one when that display is gone, and is clamped into the
    display's visible area, so it never opens off screen.
  - **Each tile's face choice**: whether a Claude Code shell shows its conversation or its
    TUI. It is applied once the tile's shell is back.
  - **Each popped-out tile and its window's frame.** The window opens again, where it stood,
    when the tile's stream first opens.
  - A frame smaller than 200 points or with a non-finite field is dropped (`WindowFrame::sane`).
    Pre-release, so an older `layout.json` without these fields fails to parse once and costs
    only the arrangement. The layout is also written as the app quits, so a move made just
    before ⌘Q is kept.
  - Tests: `workspace::tests::relaunch` (faces and frame, and a popped tile back in its
    window), `window_frame::tests::a_frame_opens_where_it_was_or_as_near_as_the_screen_allows`,
    and `layout::tests::a_window_frame_is_kept_only_when_it_can_be_opened_again`.

- ✅ **An empty workspace with no worker has a way to add one** (2026-10-01). It used to point
  at the command palette in words. Now it says what a worker does and offers an "Add a worker"
  row, the page's primary row, which runs the same panel as the "…" menu's (the status bar's
  `MenuRun`, so an app that cannot add a worker shows no button). In the same review, the
  settings hints that ran past their row were shortened ("Family", "Blink"), and "Open a file"
  became "Open file…", because like "Open folder…" it asks for more first. Test:
  `workspace::tests::no_workers::with_no_worker_the_empty_workspace_offers_to_add_one`.

- ✅ **The focused tile carries a line while there is more than one** (2026-10-01; its tone
  amended 2026-10-04 by "Focus is a line in the text's tone"). The
  focused header was told only by its surface and its title's weight: 255 against 249 in the
  light golden and 23 against 19 in dark, which read as faint once tiles sat side by side.
  The focused header, or a tabbed column's shown tab, now carries a 2 pt line along its top
  (`stroke::MARK`) in the accent at `alpha::STRONG`, as VS Code marks the active tab and Zed
  its active pane. It shows only while two or more tiles are in view and never in the
  overview, which rings its workspace instead. Under Increase Contrast it is the whole accent
  (`Theme::set_back`). Dimming the other panes stays ruled out, for the reasons in "Focus"
  above. Test: `workspace::tests::focus_line`.

- ✅ **Colour goes to what needs the person: one vocabulary of five words** (2026-10-01, the
  GUI-first brief, `.research/gui-first-2026-10-01/plan.md` §4.1). This amends the 2026-09-05
  rule that the busy states wear the accent. (Amended 2026-10-06 by "State is a glyph; colour
  is a hue per state and an identity per machine": the marks below are glyphs in their fills,
  a finish is a green check rather than the accent dot, and Working's glyph is blue.)
  - **The words.** *Needs you* in `warn`, *Failed* in `error`, *Working* and *Waiting* in
    `text_muted`, and a finish not yet seen as the accent dot with no word. Rest shows
    nothing, only the kind's glyph. `Status::Running` reads "Waiting", and its calm mark is a
    dashed ring that steps once a second (amended 2026-10-04 by "Waiting holds still"); *Working* keeps its stepped ring on the shared clock.
  - **Busy rows recede.** A navigator row at work is drawn at `alpha::STRONG` until it is
    hovered or selected, with its title at the regular weight, so a row that needs the person
    leads at full ink (`navigator::row_strength`). Increase Contrast draws it whole.
  - **Away loses `warn`.** A worker or server out of reach is muted: the struck-through name
    and the crossed-out server glyph say it, and `warn` means "needs you" alone.
  - **The ladder orders every list:** needs you, failed, unseen, working, waiting, the rest
    (`navigator::attention`).
  - **Where it shows.** The tile header's slot and pill (a finish is the slot's dot, with no
    pill), navigator rows, the palette's right edge, the inbox's marks, the status bar's
    counts and the rollups.
  - Tests: `navigator::tests::rows_climb_the_attention_ladder`,
    `working_rows_recede_and_a_row_that_needs_you_does_not` and
    `a_row_names_a_state_that_is_happening`, plus
    `palette::tests::the_right_edge_says_status_age_or_keys_and_the_kind_is_context`.

- ✅ **Attention is walked, triaged and said in the corner** (2026-10-01, the GUI-first brief,
  `.research/gui-first-2026-10-01/plan.md` §3 items 8 and 9).
  - **⌘⇧A walks the ladder.** "Next thing that needs you" goes to the agents that need the
    person, then the failed finishes, then the ones that ended well, each in layout order,
    and round again (`WorkspaceView::attention_ladder`). A finish it goes to is read, so it
    leaves the ladder; the walk goes on from the rung it stood on rather than back to the top.
  - **The inbox is worked by keyboard,** as Linear's and Superhuman's are: J/K or ↑/↓ select,
    ↵ goes to the row's tile, E marks a finish done and goes to the next row, H snoozes it, U
    marks a read one unread, ⌘↵ and ⌘⌫ allow or deny a held prompt, Esc closes. The bell's
    focus moves to the list when it opens, under its own key context (`Inbox`, a palette
    scope that takes bare keys). E, H and U say what they did in the corner with "Undo" for
    ten seconds.
  - **Snooze, honestly.** A snoozed finish leaves *Unread* and the bell's count, keeps its
    tile's dot, and comes back when its session finishes again or after an hour. An agent
    that needs the person cannot be snoozed: it stays where they see it until answered.
    (Amended 2026-10-04 by "Snooze is the server's, with presets".)
  - **Off screen, the corner says it.** With the app in front, an agent that newly needs the
    person, or a shell or agent that fails or finishes, while its tile is out of view gets a
    notice naming it, with its status mark and "Go". One in view says it itself, and a newer
    notice for the same tile replaces the older. (Narrowed 2026-10-04 by "The corner points
    at what needs you, and only that".)
  - **A notice under the pointer stays.** Hovering holds every notice; leaving gives it two
    more seconds (`SAY_AFTER_HOVER`), as macOS banners and Linear's toasts do.
  - Both later landed: pushes chosen by presence (the server pushes when the person is at none
    of their clients, `slopty-server/src/hub/ladder.rs`), and a child agent reporting through
    its parent ("Notifications by presence, and children through their parent", below).
  - Tests: `workspace::tests::triage` and
    `toasts::a_notice_under_the_pointer_stays_until_the_pointer_leaves`.

- ✅ **The empty workspace asks what an agent should do** (2026-10-01, the GUI-first brief,
  `.research/gui-first-2026-10-01/design.md` §10 #3). The heading "Empty workspace" restated
  what the page shows, and three equal rows put a shell, an agent and a window on one footing.
  The page now leads with a composer-shaped field asking "What should an agent do?", with the
  worker and the directory it starts in as chips under it (`workspace/ask.rs`). ↵ starts the
  agent there with what was typed as its first prompt, and the prompt's first line titles its
  tile; ↵ on nothing starts it bare, as ⌘⇧T does. The directory chip steps through the places
  shells stand in on that worker, then the worker's own default; the worker chip, with several
  workers, steps to the next. The field takes the keyboard once as the page shows, while the
  workspace holds it. "New terminal" and "Add a window or display" stay as the quieter rows
  under it, and "New agent" goes, since the field is that way in. Sources: Geist "cap at one
  primary CTA" and empty states that add something new rather than restate the title; Apple's
  HIG on empty states guiding people to what they can do. Tests:
  `workspace::tests::palette::the_empty_workspace_asks_what_an_agent_should_do` and
  `the_questions_chips_choose_where_the_agent_starts`.
  (Superseded 2026-10-04: the field and its chips are gone, folded into the starting tile, see
  workspace.md "One way to ask an agent its task".)

- ✅ **Names come from the task, never a number** (2026-10-01, design.md §10 #4). A workspace
  nobody named takes its first shell's repository, else that shell's directory. Where neither
  says anything, as with a shell at home, it takes its first tile's worker. A tile's own title
  was tried and dropped: it follows every command run and every page loaded, so the tab's name
  churned, and the title bar drew a name a frame behind the tile (the e2e stale-frame check
  caught it). With nothing on it, it is "New workspace". The e2e stack's first shell is at its private home, so its goldens now read
  `e2e-worker` where they read "Workspace 1". An agent started from the empty workspace is
  titled by its prompt from the first frame, before its face has read a word. An agent started
  from a shell is named by its thread's title, which the worker makes (`ThreadMeta::title`).
  Test: `workspace::tests::bars::a_workspace_is_named_by_where_its_first_shell_is`.

- ✅ **What the keyboard opens arrives whole** (2026-10-01, plan.md §4.4). The palette and the
  bar's menus, the inbox among them, opened by a key draw at full opacity in their first
  frame, with no fade and no travel: the person asked for them and is already typing. Opened
  by the pointer they keep the 120 ms fade, which helps the eye find where they came from.
  Which it was is GPUI's own `Window::last_input_was_keyboard` at the moment of opening. The
  inbox has its own key now, ⌘⇧U, and a palette row.

- ✅ **Notifications by presence, and children through their parent** (2026-10-02, GUI-first
  plan §3 item 8).
  - **Presence.** The phone posts nothing while the person is at another of their devices,
    and takes back what it had up as they arrive there: the Mac in front of them says it
    (`Attention::set_present_elsewhere`). Who is where is the server's list of every client's
    `Presence` (`FromServer::Present`): the app sets the gate while another client on a desk
    seat is active and this one is handheld.
  - **Children report through their parent** in the server's ladder, which folds each
    subagent into the thread it hangs from (`slopty_proto::thread::attention`), so the rail,
    the inbox and the notes take the root's word. A client-side fold was written and dropped
    rather than kept beside it: one fold, on the server, for every list. The rule it settled,
    asked of the ladder: a child that needs the person always lifts its root, naming the
    child; a child's failure stays its parent's (the root reads Working) while the parent
    works, and is the person's once the family has settled.
  - **To review.** The rail lists an agent whose turn ended unseen under *To review*, between
    *Needs you* and *Working*, while its own row is out of sight, with what it said it did,
    else how long it ran; the status bar counts them per worker beside working, waiting and
    blocked. Once the worker reports a turn's diff, *To review* narrows to turns that changed
    files.
  - Tests: `attention::tests::nothing_notifies_while_the_person_is_at_another_device` and
    `an_agent_that_ended_unseen_is_listed_to_review`.

- ❌ **Increase Contrast derives its own chrome** (2026-10-02, GUI-first plan, accessibility;
  superseded 2026-10-06 by "Light and dark only; the Increase Contrast variant is deleted").
  The system's Increase Contrast (iOS: Darker System Colors) sets `Theme::contrast` before
  `derive_chrome`, so a `[colors]` background gets the contrasted chrome too, and the app
  follows the setting as it changes (`watch_increase_contrast`; a self-test keeps the
  standard chrome so its frames compare).
  - **Text.** Every chrome text tone, muted text and the accent included, clears WCAG AAA
    (7:1) on every surface it lands on, where the standard look clears AA (4.5:1); the three
    levels keep their quarter-again steps. On a mid background past the supported range the
    tones go as far as black or white take them, as they do in the standard look.
  - **Hairlines.** The dividing hairline is laid on until it reads 3:1 on every surface it
    crosses, WCAG's least for what is seen and not read; the quiet one stays a level under
    it. Both are derived, not picked, so a custom background moves them with it.
  - **Focus rings** take the accent, which is now 7:1, and keep their `alpha::STRONG` share,
    which `Theme::set_back` already draws whole where a view asks it. The fills stay as they
    are: a fill is seen by its hue.
  - Tests: `slopty_theme::tests::increase_contrast_raises_text_and_hairlines`,
    `the_chrome_follows_the_theme_s_contrast`, and in the app
    `settings::tests::increase_contrast_derives_the_chrome_for_it`.

- ✅ **A review is a tile of its own, and the server's notices lead** (2026-10-02, GUI-first
  plan §4.3 and item 8).
  - **The review tile.** A thread view's "Review" adds an `ItemKind::Review { thread }` item on
    the worker whose agent runs the thread, so the review sits in the strip beside the agent,
    is kept with the layout and restored, and closes like any tile. Asked again while it is
    open, it goes to that tile. It takes the keyboard when focused, its header leads with the
    file-diff glyph and says "Review", and comments sent from it take the keyboard back to the
    agent's tile, where the answer shows. Its tile gone, here or on another client, lets the
    thread go. A tile restored before its thread view has asked waits on "Review" until the
    faces can open a review for it themselves.
  - **Notices.** Linked to a server, the server's `Notice` is the only agent moment that posts
    a note (`Attention::set_server_led`, `WorkspaceView::heard`): it has already picked this
    client by where the person is, so two devices never both say it. The note says the
    subagent it came from, and a finished turn shorter than the slow-command time is dropped
    here. The workspace's own look still adds a held prompt's approval buttons to the note up,
    takes back what was answered and keeps the badge. A shell's long command and a program's
    own notification stay this client's.
  - Tests: `workspace::tests::review_tile::a_thread_s_review_opens_as_a_tile_of_its_own_and_goes_with_it`,
    `attention::tests::led_by_the_server_only_its_notices_post_for_agents` and
    `a_server_notice_leads_to_its_tile_and_names_the_subagent`.

- ✅ **A control's outline reads 3:1** (2026-10-02, the Increase Contrast golden's review). An
  unticked task box was drawn in the dividing hairline, which reads about 1.3:1 on the default
  backgrounds (1.26 on white, 1.30 on the dark one): the box all but vanished, and WCAG asks
  3:1 of a control's edge (1.4.11). `Surfaces::control` is its own token, the chrome's ink laid
  on until it reads 3:1 on every surface it can sit on (3.75 under Increase Contrast, a level
  over the dividing hairline's 3:1 there), derived like the rest, so a custom background moves
  it too. A ticked box takes the neutral solid (amended the same day).
  - Tests: `slopty_theme::tests::a_control_s_outline_reads_three_to_one_everywhere`,
    `markdown::tests::a_task_box_is_drawn_not_typed`.

- ✅ **An agent's subagents fold into its row as a count** (2026-10-02, GUI-first plan §4.3).
  The fleet rail lists each agent once. Its subagents at work (working, waiting on their own
  work, or on the person) join its words as a count: "Editing parser.rs, 2 subagents", or the
  count alone. A count is part of the words rather than a part of the line, so the second line
  keeps its two separators. One that finished is the agent's history and is not counted. The
  rows come from the agent's thread table (`WorkspaceView::subagents`), under its root thread
  however deep.
  - Test: `workspace::tests::nav_rows::an_agents_subagents_at_work_fold_into_its_row_as_a_count`.

- ✅ **The keyboard stays with a tile whose thread view comes or goes** (2026-10-02). A tile's
  thread view goes when its thread moves to another terminal (the agent's session taken up
  there) or ends, and it takes the place of the conversation face the tile showed until the
  agent's thread was known. Whichever held the keyboard went out of the frame with it, and the
  keyboard was left with nothing: the gallery's `agent-needs-you` golden lost the focused shell's
  caret, and `thread-phone` lost its composer's in some runs and not others, by the frame the
  thread arrived in. Now the tile asks for the keyboard again (`pending_focus`) and it lands in
  whatever the tile shows next: the thread's view, or its TUI.
  - Tests: `workspace::tests::thread_face::a_thread_that_moves_away_hands_the_keyboard_back_to_its_tile`,
    `the_thread_view_takes_the_keyboard_from_the_face_it_replaces`.

- ✅ **MonoCode's calm, the brand's green, a neutral primary** (2026-10-02, design checkpoint 1;
  plan in `.research/design-2026-10-02/plan.md`). The user judged the UI less beautiful than
  MonoCode and asked for the most beautiful reference element by element. MonoCode's tokens
  (its `index.css`) set the overall calm; for any one element (diffs, tool cards, composer,
  menus, motion) the best treatment among MonoCode, Zed, Warp, delta, zeron, T3 Code and Amp is
  taken, judged on Slopty's own renders. Zed's app crates are GPL and Warp's client AGPL:
  their values and ideas are used, never their code. This checkpoint is the tokens and the kit;
  the thread, the frame and the tiles follow on top of them.
  - **Neutrals.** Dark content `#171717`, text `#EBEBEB`. The chrome stays darker than the
    content (bars 30 %, navigator 16 % toward black), as MonoCode's sidebar is; zeron's re-cut
    to near-black is not taken.
  - **Hairlines and fills** are the text over the content: dividers at 7 % in dark (a third
    again on white, where the same share reads fainter), hover 5.5 %, selected 8.5 %, what
    floats 3.5 %, under the hover step so a row's hover shows on a menu. `kit::selected` adds a
    1 pt inset hairline ring for a selection on a surface near its tone.
  - **Text tiers** are shares of the text over the content: 70 % and 57 % in dark, 78 % and 66 %
    in light, then lifted until each reads AA on all six surfaces (AAA under Increase Contrast)
    and a quarter again over the tier under it. MonoCode's most used `/50` and `/45` read 4.6:1
    and 3.9:1 on `#171717` and 3.7:1 and under on the selected fill, short of AA, so the muted
    tier stops at the least share that still reads AA there.
  - **The brand's green is the interaction accent** (OKLCH 0.72 0.16 150 in dark; in light the
    same hue at L 0.50 as text and L 0.65 as a mark), derived through `slopty_theme::Oklch`,
    which cuts chroma, never lightness or hue, at sRGB's edge. Success is the same green, so
    green means one thing: live, chosen, good. Words on the green mark are a near-black of its
    hue. Blue leaves the chrome: the terminal's cursor is its text colour, its selection and a
    field's caret and selection are neutral.
  - **The primary is the neutral solid** (`Surfaces::solid`, the text as a fill, and
    `solid_ink`): white on dark, near-black on light, as MonoCode's stop and Commit buttons
    are. It draws the one primary of a surface, the send and stop disc, a ticked box, a switch
    that is on and an armed key, through `kit::solid` and `kit::solid_pressable` (and gpui-kit's
    `primary`). The accent is never a control: `kit` `the_accent_is_never_a_control` flags any
    line that puts the green's ink on a fill, with the sites still to move named in its waiver.
  - **Buttons** stand one control height (`Density::control`, 28 on the Mac, 44 by touch). A
    secondary is a neutral fill with no hairline, a link is the text's own tone underlined
    under the pointer.
  - **Sizes.** Radii 4, 6 (the default), 8, 12 and full, with `Radii::nested` for a corner
    inside a card; spacing adds 32 and 48; a row ends 6 in from its edge against 12 at its
    start; headers are 40; type adds `heading` (20) and sets caption 11 and meta 12, MonoCode's
    most used size.
  - **Shadows** in dark drop to 10 % and 12.5 % from 40 % and 50 %, near Zed's quiet floor; the
    lit top edge stays, since on near-black the edge and the hairline say where a sheet ends.
  - **Motion** is 120 ms feedback, 160 ms reorder, 200 ms close, eased (0.22, 1, 0.36, 1).
    Reduce Motion is GPUI's one flag: the app sets it from the system at launch and as the
    setting changes, and `kit::motion` and the working marks read only it, so gpui-kit's
    animations and Slopty's hold still together.
  - **Tokens only at call sites.** Lint-as-tests in `kit` flag a colour literal
    (`chrome_has_no_colour_literals`) and a text size or radius written as a number
    (`chrome_sizes_come_from_the_scale`).
  - Not taken: in-app backdrop blur (the person later ruled every surface solid, in "The
    design system starts from MonoCode's"), and shortcut hints inside controls (keybindings
    stay in the palette).
  - Tests: `slopty_theme::tests::the_primary_is_the_neutral_solid`,
    `controls_and_the_words_on_fills_read_in_both_contrasts`,
    `the_accent_is_the_brand_green_and_blue_is_gone`, `the_brand_in_oklch_is_the_brand`,
    `the_hairlines_and_fills_are_monocode_s_shares`, `chrome_text_clears_wcag_aa`,
    `increase_contrast_raises_text_and_hairlines`; `kit::tests::a_button_is_neutral_and_one_height`,
    `the_kit_theme_follows_the_tokens`, `the_accent_is_never_a_control`,
    `chrome_has_no_colour_literals`, `chrome_sizes_come_from_the_scale`,
    `chrome_holds_still_under_reduce_motion`; `markdown::tests::a_task_box_is_drawn_not_typed`.

- ✅ **Text at the weight it is set in** (2026-10-02, design checkpoint 0). macOS's font smoothing
  thickens a glyph by the luminance of its colour, and GPUI copied it, so the dark theme's light
  text inked up to 15 % more than the same text on light: nearly a whole CSS weight, and the
  400/500/600 ladder read a step heavier in dark. The workspace sets gpui-fast's
  `TextSmoothing::Antialiased` for every window at launch (`open_workspace`), as MonoCode draws
  (the web's antialiased smoothing) and Ghostty does by default. Every colour draws at dilation 0,
  and a glyph holds one raster in the atlas whatever its colour. Frame cost is unchanged
  (MEASUREMENTS "renderer: text drawn antialiased"). Tests: gpui-fast
  `fast::tests::text_smoothing::antialiased_text_is_never_dilated_and_shares_one_raster_across_colours`,
  `a_scoped_smoothing_overrides_the_applications`; `gpui_macos`
  `a_dilated_glyph_inks_more_than_an_antialiased_one`.

- ✅ **The frame recedes: a breadcrumb, headers on their content, rows that nest** (2026-10-02,
  design checkpoint 3; `.research/design-2026-10-02/plan.md`). The frame round the strip had
  been drawn as bands and rules; MonoCode's and Zed's frame is felt, by tone and words.
  - **Title bar.** It takes the navigator's tone with no rule under it. The workspace tabs
    give way to a breadcrumb of where the focused work is, `workspace ▾ / checkout ▾ /
    branch`, as Zed names its project and branch (`workspace/breadcrumb.rs`). The workspace's
    menu lists every workspace with something on it and a new one, which is how the bar goes
    between them; what waits in another workspace shows as its rollup's mark on that segment.
    The checkout is the focused shell's repository (else its directory), with a menu of the same
    repository's other checkouts in the layout on any worker (`RepoId::same`) when there are
    some. A workspace takes its first shell's checkout for a name until it is named, so a
    checkout with no menu that the workspace already goes by is left out rather than said
    twice. The branch follows with its working tree's changes. A segment has a chevron only
    when it opens something: no branch list reaches the client, so the branch is words.
  - **Status bar.** No rule over it, and only ambient facts: the server's word while it is
    down and which worker the focused tile runs on on the left; the checkout and branch are the
    breadcrumb's and the directory the tile header's, so each is said once.
  - **Tile headers** sit on their content in every state, with no band and no rule; only a
    page's or a remote picture's header keeps a hairline, where two surfaces meet anyway. Focus
    is the title's tone (`tile::title_ink`): primary at the medium weight, the rest muted. The
    green line along the focused header goes; under Increase Contrast a line in the text's tone
    says it as well. A tabbed column's tabs are row-tall objects on the header, the shown one on
    the hover step's fill.
  - **Navigator rows** lead with a status glyph (working, waiting on the person, done,
    failed, away; at rest, the kind), the working tree's changes sit right-aligned under the
    first line's end, and under the pointer that end gives way to
    a close button, as Zed's thread rows swap their actions in.
  - **Sheets nest their rows.** A menu, the palette and the inbox pad their rows by
    `kit::sheet_pad` inside their hairline, and the rows (`kit::sheet_row`) round at 6 inside
    the sheet's 12, so the corners share a centre.
  - **Settings** set each group's rows in a card under its label, as System Settings and Zed's
    settings do (superseded 2026-10-06: a ring with no fill on the content plane, one-line rows;
    `settings.md`, "The settings pane is a macOS form"): the hover step in dark, the floating
    surface ringed by a hairline in light, rows parted by the quiet hairline. Each row stays a child of the page, so the keyboard's
    scrolling to a row still finds it.
  - **The green stays a meaning.** Links read in the text's tone, underlined under the pointer,
    their external-link glyph muted; a picker's tick is the text's; a notice's one action is
    a ghost at the medium weight. Green there had read as "done" on things that only act.
  - **Fields.** A floating list's field has no rule under it: the first heading parts it from
    the rows by space alone, as MonoCode's and Raycast's do. The navigator's filter is a well
    on the selection's fill, not the hover step, which on light sits a hundredth from the
    chrome's own tone (`a_well_on_the_chrome_stands_off_it`).
  - **Reduce Motion** reaches the strip's springs through GPUI's flag too: the workspace reads
    it at each drawing instead of the system's setting.
  - Tests: `workspace::tests::focus_line::the_headers_sit_on_their_content_and_focus_is_the_titles_tone`,
    `under_increase_contrast_the_focused_header_carries_a_line`,
    `tab_strip::the_breadcrumb_goes_between_workspaces`,
    `bars::the_breadcrumb_names_the_checkout_and_its_branch`,
    `bars::the_status_bar_says_where_the_shell_is_and_counts_what_is_shared`,
    `chrome::the_workspace_in_view_carries_no_second_mark`,
    `nav_rows::a_tile_row_leads_with_its_state_and_closes_from_under_the_pointer`,
    `overlays::a_sheets_rows_nest_in_its_corners`,
    `settings_form::tests::a_groups_rows_are_one_ring_under_its_label`.
- ✅ **What the showcase showed: say each thing once, where it fits** (2026-10-02, design
  review of the showcase renders, `target/lanes/showcase-notes.md`). A dense workspace with
  three workers and agents at work showed where the chrome still crowded or lied.
  - **A state is a glyph, not a word on the title's line.** "Needs approval" took half of a
    240 pt navigator row and cut "Harden the session refresh" to "Harden the s…". The row leads
    with the state's glyph (the waiting mark in the warn tone), its second line says what is
    asked, and the row's accessible name keeps the word. The title has the row to itself.
  - **An agent's words are plain in the chrome.** Its detail and a followed conversation's
    summary are Markdown as the agent writes it; every place that says them draws one plain
    line, so they are said as plain words once on the way in (`markdown::plain_line`):
    `**1.48.0**` reads 1.48.0, a code span its code, a link its words, and `snake_case` stays.
  - **Shells that read alike are named by what they ran.** Three shells in one checkout were
    `atlas`, `atlas 2`, `atlas 3`. Before a number, each is named by the command it last ran
    (`cargo test`, `docker compose logs`), and its second line no longer says it again. A
    number tells apart only those still alike.
  - **Notices live in the status bar** (superseded 2026-10-05 by "No bar along the bottom"
    below). A notice floating in the strip's corner lay over a
    composer's send button and a shell's last rows. They sit in the bar between where the
    focused tile runs and its readouts, on the selection's fill, one line each (a held-back
    page's why and age follow its host on the line, the address in its hint), two at most,
    the newest nearest the readouts. They are painted at the notice layer so the inbox's Undo
    is pressed through an open popover, and the bar comes up for a notice when it would be
    hidden (no worker yet, the keys' bar up).
  - **The machine is named where several meet.** A workspace is the person's grouping and is
    named after its first shell's checkout (since 2026-10-03, after the project most of its
    tiles are in); it is never grouped by repository behind their back. When it holds tiles on more than one worker, the breadcrumb names the focused
    tile's worker after the workspace (`atlas ▾ / devbox / main`), so three machines'
    checkouts of one repository no longer read as one machine.
  - **The overview shows that a strip goes on.** A workspace whose block runs past the
    window's edge gets a round button on that edge that steps its columns into view, as the
    strip's swipe does; nine tiles had been four miniatures cut off with nothing to say so.
  - **Bar menus close on Esc**, which the shell under them never gets, as well as on a second
    press of their button.
  - **The settings follow their file.** A file changed outside while the dialog is open (an
    editor, the appearance switched by writing it) is taken into the form and the field, so
    neither shows a value the file no longer holds nor writes it back; a change the person
    is making there is kept and lands.
  - **Secondary buttons carry a hairline** just inside their edge: on a raised card (this
    Mac's checklist) their fill was the card's tone and "Open settings" read as words. A
    project row's Merge, Retry and Approve are ghosts, as a tile header's actions are.
  - **A remote window fits its tile.** The synthetic window's picture is fitted to the tile's
    width with the tile's own surface above and below; the dark band and the small page in
    the showcase's render are the synthetic desktop the test worker draws around its page.
  - Tests: `markdown::tests::a_line_of_markdown_reads_as_its_words`,
    `nav_rows::a_tile_row_reads_its_age_or_its_state_then_its_place`,
    `tiles::shells_that_read_alike_are_named_by_their_last_command`,
    `toasts::a_notice_sits_in_the_status_bar_over_no_tile`,
    `bars::the_breadcrumb_names_the_checkout_and_its_branch`,
    `overlays::a_bar_menu_closes_on_escape_and_a_second_press`,
    `settings_editor::tests::the_dialog_follows_the_file_and_writes_nothing_back`,
    `thread_start::an_open_palette_takes_the_agents_as_they_arrive`.

- ✅ **A request is answered where it was asked** (2026-10-02, design checkpoint 2,
  `conversation/thread/view/tray.rs`; tests in `thread/tests/face.rs`). While the call that asks
  is on screen, its answers sit on that call's card. Once it scrolls away, a tray on the
  composer's top edge carries the request with the way back to the call (Scroll ↑/↓), so the
  person never answers blind and never hunts for the question. The request stays whole in the
  tray; the plan and edits beside it scroll past 30 % of the window (12 % under a request),
  so the conversation keeps its rows. The plain allow is the one white button; "always" and rules are quiet buttons of
  their own that never lead.

- ❌ **One icon set at one weight; files and agents by their own marks** (superseded
  2026-10-05 by "The chrome's icons are SF Symbols" below; 2026-10-02, design
  checkpoint 4, `crates/slopty-ui/src/icons.rs`, `file_types.rs`; licences vendored beside the
  drawings).
  - **Hugeicons draws the chrome, at 1.75.** Its free set is MIT (`assets/icons/LICENSE`,
    taken from the GitHub repository, whose README puts the free icons under MIT; the site's
    agreement restricts Pro). Each file in `assets/icons` is the Hugeicons glyph for the
    Lucide name gpui-kit's `IconName` carries, so call sites keep their names. The stroke went
    from Hugeicons' 1.5 to 1.75 when vendored; an outline Hugeicons draws as a fill gains the
    same quarter point from a stroke of its own. An icon the set does not draw comes from
    gpui-kit's Lucide bundle (ISC) with its stroke of 2 brought to 1.75, so a component's own
    icon keeps the weight. Where Hugeicons' drawing under a name reads poorly at 14 pt or
    means something else, its nearer drawing is taken: the arrows are its `-02` shafts (its
    `-01` are chevrons), the plain file `file-empty-02`, the terminal `command-line` (its
    `square-terminal` is a squircle with a dot of a prompt), activity `pulse-01`, maximize
    `arrow-expand-01` (its `maximize` are hands), pencil `pencil-edit-01` (its `pencil` is a
    crayon tip up, which read as a vector pen tool where a tile is named; amended 2026-10-03).
    A lint-as-test fails on any `IconName` the chrome names that Hugeicons does not draw, which
    is how case-sensitive, eye, git-merge, layout-dashboard and minus were found falling back.
  - **A file shows its type.** Material Icon Theme's drawings (MIT, `assets/file-types/
    LICENSE`), 55 of them, in their own colours: by the whole name first (Dockerfile,
    README, Cargo.lock, .gitignore), then the extension. A type the set does not draw keeps
    the plain file icon, so an unknown file never reads as a known one. Tile headers, the
    palette's file lines and the folder tile's rows show them.
  - **A coloured drawing is rasterised at the window's device pixels**, once for each size
    it shows at, and drawn on the first frame: `img()` from an asset path rasterises an SVG
    at its intrinsic size and loads it asynchronously, which blurs a 32-unit drawing at 14 pt
    on Retina and leaves the slot blank for a frame.
  - **An agent shows its own mark only where its licence allows one** (superseded 2026-10-05
    by "No agent wears a mark of its own" below: no agent has a mark). pi's mark is drawn
    from the four-by-four layout its MIT source spells out, in its own colours; OpenCode's
    comes from its MIT repository, without the tile behind it, in the ink beside it as its
    light and dark variants are. Anthropic allows its marks only in materials it approves
    and forbids visuals that mimic Claude Code; OpenAI's permission is non-transferable, so
    forks of an open repository would not hold it (Simple Icons removed its OpenAI icon for
    that reason); Google's needs approved artwork. Claude Code takes the neutral sparkles in
    the theme's agent orange (`Surfaces::agent`). Codex takes Hugeicons' code-circle in ink,
    and any other agent the sparkles in ink, so no two agents look alike and none borrows
    another's colour (Codex wore Claude Code's orange sparkles before, and read as it). No
    robot: the Lucide `Bot` is gone from the chrome.
  - **A picture's tile says what it is and can show it whole.** Transparent pixels show over
    a checkerboard of two neutral steps of the content plane; a foot under the picture or
    PDF says its type, pixels or pages, and size (`PNG · 1200 × 800 · 240 KB`); a picture
    larger than its tile can be shown at its own size, scrolled and decoded whole, and fitted
    again.
  - **Readouts side by side are parted by the middle dot** (the project header's
    `2 of 12 live · 1 of 7 merged`).
  - Tests: `icons::tests::every_icon_is_drawn_at_one_stroke_and_the_component_bundle_still_loads`,
    `icons::tests::an_agent_shows_its_own_mark_only_where_its_licence_allows`,
    `kit::tests::every_icon_the_chrome_names_is_drawn_by_one_set`,
    `file_types::tests::a_file_is_known_by_its_name_then_its_extension`,
    `file_types::tests::every_type_is_drawn_and_served`,
    `file::preview::tests::a_picture_at_its_own_size_is_decoded_whole`,
    `palette::tests::every_line_icon_is_embedded`.

- ✅ **The showcase's defects, settled** (2026-10-02, design round 2, `crates/slopty-ui`,
  `crates/slopty-client/src/layout.rs`).
  - **A thread's figures say each thing once and only once it is so.** The Edits tray counts
    a file once its edit is made, never while the edit is only asked for. A model is named
    once, by the name its agent speaks, with its provider muted after it (pi's
    `canned/canned-1` is `canned-1 · canned`), and a turn's footer leaves out the model the
    thread already runs. The context ring waits for the first usage instead of showing 0 %, and
    a share under one half says `<1%`. The composer's corner carries no spinner of its own,
    since the thread's rows already show that it works. A thought's collapsed line renders
    its inline code as code, not as backticks.
  - **A waiting request comes first, then the conversation.** The tray sits under the rows,
    never over them, so a click above its edge is the rows'. A request in it stands whole;
    what else waits (the plan, the edits, the queue) scrolls past 30 % of the window, and
    past 12 % while a request stands above it, so the conversation the question is about
    keeps the room. A first try that kept 35 % of the tile for the rows cut the
    questionnaire's own buttons off instead. A call that asks as it arrives at the foot of
    the thread is answered on its card in the first frame that shows it, not in the tray for
    one frame first.
  - **A request's row of answers never scrolls out of view** (2026-10-06). Where the room
    is short for a whole request (a phone with its keyboard up, a small window, a large
    text size, a split iPad), the request's words and its questions scroll inside the card,
    and the row under them (Deny, Answer in the terminal, Submit, or an approval's answers)
    stays put, as a sheet or an alert keeps its buttons. The field for one's own answer
    scrolls into view above the row as it takes the keyboard. The composer stays where it
    is: hiding it while the person answers would move the layout under their thumb. With
    room to spare nothing changes. On the iPhone the clipped Submit used to send a tap to the
    composer under it, so a question could not be answered.
  - **Names read as a person says them.** The palette's "New … thread" lines show a folder as
    the navigator does (`src/app`, under the worker's home), not as an absolute path. A
    terminal named after its command drops the `cd <dir> &&` before it, in the tile title, the
    navigator, the inbox and its notes, and the navigator says nothing of a `cd` alone, whose
    place its row already shows. A project's agents are named by their part in it,
    "Orchestrator" or the task's `#N title`, not "Claude Code 2".
  - **A board makes room and says when there is more.** An agent opened from a full-width
    board gives up the full width so the board and the agent sit side by side. A board taller
    than its tile fades at the edge that has more, and its card actions are secondary buttons
    of the hit height, not text.
  - **The overview fills the window.** When the strip is wider than the zoomed-out window, it
    is clamped so neither end leaves a gap past the strut, and the panel behind it spans the
    window.
  - **A thread with no terminal says it waits everywhere a terminal agent does.** Codex, pi
    and ACP threads have no terminal whose agent status could say it, so the thread's row in
    its worker's table does, ranked by the ladder's own `Rung::of` with its subagents folded
    in: the navigator's glyph, the tile header's pill with what it asks, the bell and the
    inbox (a row that opens the thread), the Dock's count, the status bar, and a note while
    the app is away. A note is about a terminal or a thread (`attention::About`), so the
    server's notice for a thread with no terminal leads to its tile instead of being dropped.
    A thread whose terminal's agent already speaks (Claude Code through its hooks) is counted
    once, by the terminal.
  - Tests: `thread::activity::tests::an_edit_counts_only_once_it_is_made`,
    `figures::tests::a_model_is_one_name_with_its_provider_beside_it`,
    `markdown::tests::a_line_says_where_its_code_is`,
    `thread::tests::questions::a_request_stands_whole_and_the_rest_of_the_tray_gives_way`,
    `thread::tests::face::a_call_that_asks_as_it_arrives_is_answered_under_it_at_once`,
    `workspace::tests::thread_start` (the folder said short),
    `workspace::tests::tiles::a_command_is_named_without_the_cd_before_it`,
    `workspace::tests::projects::a_project_s_agents_are_named_by_their_part_in_it`,
    `workspace::tests::projects::an_agent_opened_from_a_full_width_board_shows_beside_it`,
    `workspace::tests::projects::a_board_taller_than_its_tile_says_more_lies_below`,
    `layout::tests::the_overview_fills_the_window_with_a_strip_wider_than_it`,
    `workspace::tests::thread_waits::*`,
    `workspace::attention::tests::a_server_notice_leads_to_its_tile_and_names_the_subagent`.

- ✅ **A waiting thread is on every list a waiting terminal is on, and counts once** (2026-10-02,
  `crates/slopty-ui/src/workspace`, `crates/slopty-app/src/server.rs`).
  - **⌘⇧A and *Needs you* take threads by their rung.** A thread whose row speaks for it
    (Codex, pi, ACP) steps onto ⌘⇧A's ladder on the rung `Rung::of` gives it: one that waits
    on the person beside the waiting terminals, one that stopped on an error beside the failed
    commands, each rung in reading order with what has no tile here after it. A thread left to
    review is not a step: its rung lasts until its changes are kept, while the ladder's
    finishes leave once looked at, so it would never let ⌘⇧A move on. *Needs you* lists a
    waiting thread out of sight with what it asks; a step or a row opens the thread's tile, or
    a new one on its worker, and a worker this client cannot reach says so.
  - **One word per worker.** The server's ladder is to threads what its terminal list is to
    agents: it speaks for a worker this client has no link to. While a worker's own link is up
    its table is the word and the server's is ignored; once the link drops, the table goes
    stale and the server's stands in, so a worker reached both ways never counts a thread
    twice and never counts a stale one. The ladder carries no thread's terminal, so a thread
    takes the terminal of the tile it leads; one its terminal's agent speaks for is counted by
    the terminal. Forgetting the server forgets its threads with its agents.
  - **A clock says only what its tick can keep.** A live turn's time (the conversation face's
    working row, its running subagent and task, the thread face's working row) is whole
    seconds from "1 s" (`kit::clock`, the navigator's former `turn_label`), and the
    conversation face's clock ticks on the turn's own second, as the thread face's does. Its
    tenths would have gone stale between two ticks a second apart.
  - Tests: `workspace::tests::thread_waits::`
    (`the_ladder_and_needs_you_list_a_waiting_thread_by_its_rung`,
    `a_thread_the_server_ranks_counts_once_however_its_worker_is_reached`),
    `conversation::view::tests::the_working_clock_says_what_it_ticks`,
    `kit::tests::a_clock_ticks_in_whole_seconds`; e2e gallery `agent-needs-you` and `inbox` wait
    for the thread's request to arrive before their goldens.

- ✅ **Fades are per pixel, with the surface outside them** (2026-10-03, gpui-fast `decee29f`,
  aislopware/gpui-fast#18). Where content runs past an edge it fades out pixel by pixel
  (`gpui::edge_fade` around the scrolling element), and the surface behind it is painted outside
  the fade. `kit::edge_fade` laid a gradient from clear to the surface's colour over the content,
  which fades only over that one opaque colour: over the window's glass, a video or a frosted
  floating object it would draw a band of the wrong colour. It also needed `painted_while` to
  judge after layout whether anything lay past the edge, and it was one more quad over every
  scrolled frame.
  - **What fades.** The palette's rows at their foot (`lg`); the transcript's rows at the top
    (`md`) and the foot (`xl`); the project board's body at the top (`md`) and the foot (`xl`);
    the settings page at both (`lg`); the key bar's caps at either end (`lg`). Each list or
    scroll container is wrapped as it is, with `hidden_by_list` / `hidden_by_scroll`, so an
    edge fades only as deep as content lies hidden past it: none at the end, in full once a
    whole width is. The key bar keeps `key_bar_fades`, read from the caps rather than the last
    layout, so a bar just shown fades on its first frame.
  - **The surface stays outside.** A fade covers everything inside it, its own background
    included, and a scroll layer bakes the background under it only when that is one opaque
    quad without a fade. So the background (the face's, the sheet's, the bar's) is painted by
    an element around the fade, never by the element it wraps.
  - **A list's fade lands a frame late.** The fade reads the list's extent before the list lays
    out in the same frame, so a list just opened or narrowed draws one frame with the fade it
    had and gpui-fast asks for the next, which is right. The palette's old overlay was judged
    after layout to be right in the first frame. The self-test's `render` and `dump` now draw
    the frames the app asks for before they judge (`retained::settle`, at most four): the
    frame between is not where the app settles, and it failed the stale check as the palette
    narrowed to the folder it offers. A view changed without a notify asks for no frame, so
    the check still catches it.
  - **Chrome draws no gradient.** The lint `a_gradient_is_an_edge_fade` is now
    `chrome_draws_no_gradient`, over `kit` too, with no exception.
  - Tests: `retained::tests::a_fade_is_told_by_its_edges` (the helper `retained::faded_edges`
    reads which edges of a region the last frame faded, from `Window::painted_primitives`),
    `palette::tests::a_long_list_fades_at_its_foot_until_its_end`,
    `conversation::view::tests::the_rows_fade_where_more_lies_past_an_edge`,
    `settings_form::tests::a_long_page_fades_where_more_lies_past`,
    `workspace::tests::projects::a_board_taller_than_its_tile_says_more_lies_below`,
    `workspace::tests::palette::the_palette_hangs_at_a_fifth_and_is_a_sheet_on_a_phone`,
    `kit::tests::chrome_draws_no_gradient`, app `the_key_row_fades_where_keys_run_past_the_edge`.

- ✅ **The palette offers what the focus can do, and a pick is never dropped** (2026-10-03,
  readiness A13).
  - **The dispatch tree says what applies.** The workspace listens for an action bound to the
    focus only while it applies (`workspace::actions::Applies`, read once a frame): a tile's
    actions while a tile has the focus, a page's while a page does, a remote picture's, a
    file's, an agent's, a project's, and "Undo close" while a closing can be taken back. What
    the workspace does whatever has the focus (new tiles, the palettes, the inbox, the
    workspaces, the text size) it always listens for. The tiles' own actions (a terminal's, a
    file's, a board's) are their views' and live where the keyboard is. So GPUI's own question,
    whether anything on the way from the focused element up answers an action
    (`Window::is_action_available_in`), is the whole test: the palette keeps a command line only
    when it is answered from where the keyboard was, and the menu bar greys an item the same
    way, as macOS greys what nothing answers. A hand list of which command suits which tile
    would drift from the handlers; the tree cannot. A chord whose action is not answered falls
    through to the focused element, as an unbound chord does: a terminal passes ⌘ chords up, and
    a remote window takes them.
  - **Never silently.** A pick runs from the element that had the keyboard, or from the
    workspace when that is gone or nothing had it. If nothing there answers it by then (its
    tile went while the palette was open), a notice says "<command> does not apply here". The
    lines given again while the palette is open leave out what it left out when it opened.
  - "New `agent` thread" runs through the tree like any other line (the workspace answers
    `StartThread`), with no special case in the palette's handler.
  - **The window picker ends in Cancel on glass**, as the palette does: with no keyboard
    attached its field row ends in a Cancel link and a dim under the sheet shows where a tap
    closes it.
  - Tests: `workspace::tests::palette::the_palette_offers_what_the_focus_can_do`,
    `workspace::tests::palette::without_a_keyboard_the_picker_ends_in_cancel`.

- ✅ **The File menu opens and saves** (2026-10-03, readiness A15). Open File…, Open
  Folder…, Open URL…, Save and Save a Copy… run the very actions the palette's lines run
  (`OpenFile`, `OpenFolder`, `OpenUrl`, the file tile's `SaveFile`, `SaveCopy`), so their
  chords and their greying come from the same keymap and dispatch tree. Save is the file
  tile's own action, so it is greyed unless a file has the keyboard. Test:
  `slopty-app`'s `tests::the_file_menu_opens_and_saves_as_the_palette_does`.

- ✅ **A popped-out remote window takes the system's shortcuts while it is in front**
  (2026-10-03, readiness C11). The tap arms for whichever window holds the remote picture's
  keyboard: the workspace's, as its frames and its activation find it, or the tile's own
  window while that is the key window, which holds nothing but the picture. The own window's
  activation re-arms the tap at once (`rearm_system_keys`), since the workspace's window, then
  inactive, may draw no frame to do it from. The tap still lets the shortcuts be as soon as
  another app is in front. Test:
  `workspace::tests::desktop::a_window_of_its_own_takes_the_shortcuts_while_it_is_in_front`.

- ✅ **The repository lens groups clones by what they are** (2026-10-03, readiness C13;
  `repo_groups` folded into `slopty_client::groups` the same day, see "The navigator groups by
  project; the machine is a facet"). The
  navigator's "By repository" lens goes through `repo_groups::group`: each tile's clone is its
  path and the identity its worker read (`SessionSummary::repo_id`; a file's or a folder's,
  the shell's whose repository holds it), so one repository cloned at two paths on two
  workers is one block, and a clone whose identity has not come yet joins the others at its
  path. A block is kept, and folded, by the group's key (its least origin, else its first
  commit, else its path), named by the origin's last part or the directory's. Two blocks of
  one name say where each is: the origin's owner, else the least path's parent. Test:
  `workspace::tests::nav_rows::the_repository_lens_groups_clones_by_their_identity`.

- ✅ **The board opens a subagent and sets the project's checks** (2026-10-03, readiness B6,
  A6).
  - **A subagent opens as a task opens.** A click on one of Claude Code's own
    subagents in the tree opens its agent's tile, shows that tile's face, and opens the
    subagent's thread in it: in the thread view, its own way into a subagent (the bar that
    leads back, Esc too); before the session's thread is known, the conversation face's
    thread of that agent id. The subagent's thread id is derived from the session's thread
    and the agent id by one function in `slopty-proto` (`ThreadId::subagent`), which the
    worker's adapter and the client both call; a proto test pins its value. A thread the
    worker has not begun is said in a notice.
  - **Verifier and review on the board.** The header's checks toggle (and "Verifier and
    review…" in the palette, on a board) opens a panel under the header: the verifier
    command, a switch for a fresh-context reviewer before each merge, and the reviewer's
    brief. ↩ or Save sends `ProjectSet` with both (empty for none; a reviewer turned on with
    no brief gets a default one); Esc or Cancel closes it unsaved. A project started from a
    terminal opens its board with this panel open, so the checks are set where the project
    is started. Only the person sets them, as the server requires.
  - **The header and the panel sit over the board's keys.** The board binds bare letters
    (`c`, `m`, `a`) while it has the keyboard; the panel's fields, like the line to the
    orchestrator, sit outside that context, so a letter typed there is a letter.
  - Tests: `workspace::tests::projects::a_click_on_a_subagent_opens_its_thread`,
    `a_subagents_thread_is_named_by_its_session_and_agent` (proto),
    `a_board_sets_its_verifier_and_review`, `a_project_starts_in_the_focused_terminal` (its
    board opens on the panel).

- ✅ **A failed command is barred, its head washed, its output left alone** (2026-10-03,
  design). A failed block had a faint wash of the error fill over every row of it, so a long
  failure turned the tile into one pink slab, louder than anything that needs the person, with
  red text on red. Now the bar of the error fill runs down the block's left edge over every
  row, and the wash covers only its head: the prompt and the command, or the sticky header
  that stands for them once they scroll away. The output keeps the program's own background.
  The tile's header keeps its failed glyph and "Exit n". Test:
  `terminal::view::tests::a_failed_blocks_output_keeps_its_background`; goldens retaken.


- ✅ **A closed tile waits on a list, and the palette reopens it** (2026-10-03, readiness
  A20). ⌘Z could take a closed tile back for 5 s, and then a closed shell's session was gone
  with no way back. Now the notice still shows for 5 s (`UNDO_CLOSE`), but the last 20
  closed tiles (`CLOSED_KEPT`) stay on a list. The palette has a "Reopen …" line for each,
  latest first, and ⌘Z takes back the latest at any time. So there is no clock to beat.
  - **A shell idle at its prompt keeps running for 10 minutes** (`IDLE_SHELL_KEPT`): a
    plain shell (the login shell, no command, no agent) with nothing running and not exited.
    Taken back then, it comes back whole, scrollback and all. A shell running a command, a
    program or an agent stops once the notice is gone, as before, so what the person closed
    stops as they meant it to.
  - **An ended plain shell comes back as a new shell** in the directory it had, under its
    name. That covers a shell whose session ended after the wait, or one closed by its worker
    meanwhile. Anything else whose session has ended leaves the list, since what ran in it
    cannot come back.
  - **A file tile lets its editor go when the notice ends**, as it did when it closed for
    good: its unsaved edit goes with it, and a waiting program is answered (or, if its save
    did not land, told it was given up, the edit kept). Reopened later, it reads the file
    again. Keeping every editor for the whole list was rejected: twenty buffers held for tiles
    the person may never want back.
  - **The list holds data, not views.** Leaving it, the oldest entry ends anything it still
    holds. The leak checks count entries that still hold a session or an editor, and expect
    none.
  - Tests: `workspace::tests::a_closed_tile_waits_in_the_palette_to_be_reopened` (the
    palette line after the session ended, a new shell in `/w/src`, the twenty-first closing
    ending the oldest's session); `a_closed_shell_can_be_taken_back` (an idle shell runs on
    past its notice, then is closed); `tiles::an_exited_shell_stays_until_it_is_closed`.

- ✅ **A thread says each fact once, and the tray is the composer's head** (2026-10-03, design
  review `.research/design-review-2026-10-03.md` #2, #3, #5, #6).
  - **The composer holds the thread's changes and context.** An agent tile whose body is the
    thread view drew the old conversation face's chips in its header: its changed lines
    (`+24 −6`, a count the thread's own `+15 −6` in the composer contradicted) and the context
    ring. Neither click did anything, since only the old face draws their panes. Over a thread
    view the header now carries neither. The composer's context row says the changes, a click
    from the review, and its meter says the context, with the plan's windows in its hint.
    Over the old face, or a phone's header, nothing changed.
  - **A working agent wears no pill.** The header said "working" twice: the leading slot's
    spinner, then a grey "Working" pill (beside a ring that read as a third). A pill now says
    only what the mark cannot (`agents::wears_pill`): that the agent needs the person, failed or
    is out of reach, or what a turn paused on background work waits on ("Waiting on cargo
    test"). Working, at rest and finished are the slot's mark alone, as rest already was.
  - **The tray is the composer card's head.** It was a sheet tucked behind the composer, inset
    by the composer's radius and with no foot of its own, so the composer's rounded top
    seemed to cut its last row ("Edits · … Review"). Now it is as wide as the composer, with
    the composer's radius on its top corners and the composer's top corners square under it.
    The composer's top edge is the hairline between them, so one outline holds both. With no
    composer (a subagent's thread), the tray is a card of its own, as before.
  - **An answer's reach gives way.** "Always allow · cargo test -p atlas-api refresh" pushed
    the last answer onto a second line. The reach is now muted and cut with an ellipsis at 14
    ems (`SCOPE_EMS`), so the answers keep one row. The button's label still says all of it.
  - **A settings page never heads two groups alike.** A key the layout does not name now joins
    the group its table's title names, after that group's own keys. Before, it was appended
    at the page's end under a second heading with the same name (`remote.sharp_text` under a
    second "Remote windows and desktops"). Test: `a_row_comes_from_the_schema`.
  - **The way to the terminal stands on the request's title line.** "Answer in the terminal" is
    not an answer. In the row of answers it made five buttons, and they wrapped. It now sits at
    the right of the request's title, and a request with nothing to answer here is that one
    line. A questionnaire keeps its buttons together.
  - **The context ring is closed.** Its track was the hairline and all but vanished, so the arc
    beside the stop button read as a spinner. The whole track is now the muted ink at a tint
    (`alpha::TINT`), so the ring reads as a gauge.
  - Goldens retaken: `thread`, `thread-dark`, `thread-attachment`, `thread-phone`,
    `thread-questions`, `thread-settled`, `thread-subagent` (.txt), `thread-work`,
    `thread-work-open`, `inbox`, `inbox-dark`, `agent-needs-you`,
    `agent-needs-you-navigator`, `project-live-lanes` and `project-live-tree` (.txt: the
    working pill gone).

- ✅ **The navigator groups by project; the machine is a facet** (2026-10-03, the
  organisation study `.research/organization-2026-10-04.md` §6–7). The person works on a few
  projects spread over many machines, so a machine is where a tile runs, not what it is about.
  Before, the navigator listed each worker's tiles under it, and a tile from elsewhere went to
  the workspace holding that worker's tiles. So one project's agents on three machines sat in
  three places, and a workspace was named after a machine.
  - **One open model.** Each tile yields a map of facts (`slopty_client::groups::Facts`, key
    to values): `machine`, `kind`, `os`, `agent`, `cwd`, `branch`, `repo` (the clone's origin,
    its first commit and its place), `folder` (a shell's directory, not its home), `project`
    (the server board its session works for, or its thread's own `project` fact) and every
    thread fact as `facts.<key>`. A grouping is a chain of fact keys, by default `project`,
    `repo`, `folder`, `machine`; a tile joins the group of the first fact on the chain it has.
    Tiles that share any value of that fact are one group, transitively, so one repository's
    clones on three machines are one project, and a clone whose identity has not come yet
    joins the others at its place. A value names a place on one machine
    (`at:<worker>:<path>`), so two folders of one name on two machines stay two until a
    person names them one. A group's key is its fact and its strongest value (an origin over a
    first commit over a place) and does not depend on the order of the tiles. The server's
    orchestrated projects claim the clones of the repository they work in. No grouping or fact
    is a closed set: any key a tile has is a grouping.
  - **The navigator.** The filter; *Needs you*, *To review*, *Working*; *Projects*, a header
    per project with its glyph, its name (two of one name say whose or where, as before), its
    rollup when folded and, on the right, the machines it spans as a quiet word (`devbox,
    studio`, or `3 machines`) once more than one worker is known; then *Machines* (named
    *Workers* until "The person's word is machine"), a header per machine with its health and
    under it only what has no project (its windows, displays, notes, pages and home-directory
    shells). A row's second line says its machine only when its
    project spans several, then what it does, its directory below the project's root and its
    branch. An attention row says `project · machine`. The palette's "Group the navigator by
    machine" (⌘-less, the `toggle_navigator_lens` command) brings back one block per worker;
    "Group the navigator by <fact>" appears for every fact a tile has, and "Group the navigator
    by project" goes back. The chain is kept with the device's layout
    (`layout::Navigator::group_by`).
  - **The filter takes facets.** `machine:devbox`, `project:atlas`, `agent:codex`,
    `branch:main`, `is:waiting` (also `working`, `running`, `failed`, `unseen`) and any fact
    key, beside free text.
  - **Scope.** "Scope to <project>" in the palette narrows the navigator, its attention
    sections, the inbox and the status bar's agent counts to one project. It shows as a token
    leading the filter field, and Esc lets it go. It is a filter, not a mode: nothing moves.
    The bell and the Dock badge stay whole-fleet, so nothing waiting is ever hidden by a scope.
  - **Placement.** A tile from elsewhere joins the workspace that last held a tile of its
    project; one with no project (a window, a note) the workspace that last held a tile of its
    machine; else it fills an empty active workspace, else it gets one above the trailing
    empty one. A tile already placed never moves when its project changes: a shell that `cd`s
    into another clone changes row, not place. Each tile keeps the project it was last said to
    be in (`Tile::home`), refreshed before each placement.
  - **Names.** An unnamed workspace is named after the project most of its tiles are in, a
    project before a machine, a tie to the one first in the strip (the study said the focused
    tile's; the strip's order keeps the name still while the focus moves).
  - **Breadcrumb and header.** When the focused tile's project spans several machines, the
    breadcrumb names its machine (`atlas ▾ / on devbox ▾ / main`), and that segment's menu
    lists the project's clones by machine. A tile's header names its worker only where its
    workspace holds tiles of more than one, so a one-machine project never pays for the chip.
  - **The rail** (navigator hidden) keeps a glyph per project with its rollup, then a glyph for
    a worker while it holds tiles of no project or is not well (widened 2026-10-09 by "A window
    too narrow to dock the navigator folds it to the rail"). It scrolls.
  - **The palette** lists projects first ("atlas", its state, the machines it spans and what it
    holds: "3 agents", else "4 tiles"), ranked by how often and how lately the focus went to
    each on this device, as zoxide ranks directories (`groups::Frecency`, saved with the
    layout: a visit counts when the focus comes to a tile of another project than the last).
    ↩ goes to the workspace that last held it.
  - **Worked out once a frame.** The navigator, the rail, the breadcrumb and every
    workspace's name share one grouping while the window draws (`grouping::FrameProjects`),
    let go as the draw's update ends, so an action never reads a frame-old grouping.
    MEASUREMENTS "the navigator by project".
  - **Pins and names.** The palette offers, for the focused tile, "Add to <project>" for each
    project it is not in and "Take out of <project>" for the one it is pinned to. A pin is the
    item's `project` fact (`ItemOp::SetFact`), holding the key of the group it joins, so it is
    the same on every device and stands in for every link of the default chain. "Name this
    project…" opens a field in the tile's header with the project's name. ↩ keeps the project
    on the server (`ProjectCreate`, with no orchestrator and no repository) under the name
    typed. Its members are the values its tiles are known by, a place spelled as
    `{machine: <name>, cwd: <path>}` so a person or an agent reads it. A declared project's
    members claim the tiles they match (a machine by its worker's name, `~` spelled out).
  - **Declared projects in the navigator.** Each heads its group with a board row ("Board",
    "1 needs you · 3 of 5 merged", its most urgent lane's glyph). A project none of whose
    tiles is here is still listed, by its board row alone. A click shows the board in its
    orchestrator's tile, opening that terminal in a tile on its worker when it has none here.
  - **Threads with no tile.** A thread at work (any rung but idle) with no tile here, nor its
    terminal one, lists after its project's tiles, set back as a row at work is. The project
    is the one it names, else a declared project's claim on its facts, else a group whose
    values it shares, else one of its own. A click opens its tile. A thread at rest with no
    tile is history, found by search, not listed.
  - **On a phone,** the drawer is the same list: the workspaces, then *Projects*, then
    *Machines*. A notification's thread identifier is its project's key (`Note::thread`), so
    one project's notes stack together in Notification Centre.
  - Kept for later: the workers' recent places for the empty state. Kept as they were: the
    hosts popover, and the status bar keeping the focused tile's machine.
  - Tests: `slopty-client` `groups::tests` (the chain, clones across machines, two folders of
    one name, claims, any fact a grouping, keys independent of tile order over every order of
    six, frecency), `layout::tests::a_remote_tile_joins_the_workspace_of_its_project`,
    `a_project_with_no_workspace_fills_an_empty_one_or_gets_one_above_the_trailing`,
    `a_placed_tile_never_moves_when_its_project_changes`,
    `a_tile_with_no_project_joins_its_machines_work`,
    `the_navigators_grouping_is_saved_and_restored`; ui `nav_rows::`
    `a_project_header_says_the_machines_it_spans`,
    `a_row_names_its_machine_only_where_its_project_spans_several`,
    `group_by_machine_brings_back_the_workers_blocks`, `any_fact_a_tile_has_is_a_grouping`,
    `the_filter_takes_a_facet`, `an_away_worker_shows_on_its_projects_rows_and_in_workers`,
    `the_rail_keeps_the_projects_in_view`, `the_rail_scrolls_its_projects`,
    `a_shell_that_changes_checkout_moves_row_not_tile`; `pins::`
    `adding_a_tile_to_a_project_pins_it_there`,
    `naming_a_project_keeps_it_on_the_server_with_its_members`,
    `a_projects_members_claim_the_tiles_they_name`; `nav_projects::`
    `a_projects_board_heads_its_group`,
    `a_project_whose_orchestrator_has_no_tile_here_still_lists_and_opens`,
    `a_thread_with_no_tile_lists_under_its_project_and_opens_its_tile`,
    `on_a_phone_the_drawer_lists_the_projects_then_the_workers`; `attention::`
    `notes_of_one_project_share_a_thread`; `palette::`
    `a_project_line_goes_to_its_workspace`, `projects_rank_by_frecency`,
    `a_scope_filters_the_navigator_the_inbox_and_the_counts_alike`; `tab_strip::`
    `a_workspace_is_named_after_the_project_most_of_its_tiles_share`; `bars::`
    `the_header_chip_shows_only_where_the_workspace_spans_machines`.

- ✅ **The person's word is machine** (2026-10-04, `.research/rulings-2026-10-04.md` §8). The
  person says "machine", Tailscale's admin page says "Machines", and no remote-access or agent
  product calls a computer a worker; the chrome mixed both. Everything the person reads says
  machine: the navigator's and the palette's section "Machines", the hosts popover, the status
  bar's count ("2 machines, 1 not connected"), "Add a machine", "No machines yet", "List
  machines", and every notice ("The machine is away; nothing was saved"). Code identifiers, the
  wire, the decision files, the `slopty-worker` process and the `slopty worker` CLI keep
  "worker" (this amends "The worker is called a worker everywhere" in `topology.md` for the UI
  only). The update pill says "Updating Slopty on the machine", since the machine itself is not
  what updates. A lint keeps it: `kit::tests::chrome_says_machine_not_worker` scans the string
  literals of `workspace/**`, `workspace.rs`, `palette.rs` and `keymap.rs` (not their tests, log
  lines, element ids or `Debug` names) for "worker" as a word of its own. Left to their owners:
  `slopty-app` (`ssh.rs`, `this_mac.rs`, `finder.rs`, its menus' "Add a worker"),
  `apps/slopty`'s menu bar, `project/view.rs`'s empty board ("No workers yet"), and the words
  a worker sends itself ("the worker finds no host named …"). Tests: the lint, and the moved
  `bars::the_workers_count_opens_the_hosts_and_their_actions`,
  `chrome::the_more_menu_groups_its_rows_into_sections`,
  `palette::the_palette_lists_tiles_then_workers_then_commands`, `nav_rows::` headings.

- ✅ **Focus is a line in the text's tone** (2026-10-04, rulings §4b). Across a strip of five
  the focused title's tone alone was too faint to find, and the accent line before it spent
  green on something that is not done. While two or more tiles are in view, the focused
  header (or a tabbed column's shown tab) carries a 2 pt line (`stroke::MARK`) along its top in
  the **text** tone at `alpha::STRONG`, whole under Increase Contrast (`Theme::set_back`); the
  title keeps the medium weight in `text`. No ring, no frame, no dimming of the others; a lone
  tile and the overview draw none. The overview's active workspace keeps its elevation and
  takes a 1.5 pt ring of `text` at the new `alpha::RING` (0.5) in place of the accent. This
  amends the 2026-10-02 rule that focus is the title's tone alone. (Numbers amended 2026-10-03,
  see the stage 3 entry at the end: the line is 1.5 pt at `alpha::FOCUS` 0.45, inset at both
  ends with round caps; the overview's ring is one device pixel at `RING` 0.3 outside a 2 pt
  gap, and 1.5 pt whole under Increase Contrast.) Tests:
  `focus_line::the_focused_header_carries_a_text_line_while_two_tiles_show`,
  `focus_line::under_increase_contrast_the_focus_line_is_whole`,
  `strip_marks::the_overview_lifts_each_workspace_and_offers_a_new_one` (the ring in the
  text's tone, and no accent edge; kept under its name, which the entries above cite),
  `tiles::the_overview_words_start_on_the_panes_glyphs`.

- ✅ **Waiting holds still** (2026-10-04, rulings §4d). A mark that moves says work is in
  progress, and Waiting (an agent paused on its own background work, a long command) lasts
  minutes, past WCAG's five seconds of motion with no pause. `Status::Running` is a still
  dashed ring (`CircleDashed` in `text_muted`), drawn as an icon; Working is the only mark that
  moves, and under Reduce Motion it stands upright and breathes in opacity (amended
  2026-10-03). The calm lane of the spin clock went with it:
  how long a command has run is the readouts' clock's to count (`WorkspaceView::keep_time`).
  Test: `icons::tests::waiting_holds_still` (a Waiting mark wakes its view for nothing over
  three seconds).

- ✅ **A resting agent's lone word is left to its mark** (2026-10-04, rulings §4e). A row at
  rest shows the agent's last words only when they say more than a state, more than one word;
  a single word ("done") is left out, because the row's mark already says it, and nothing is
  quoted. One ambient place per fact. Tests:
  `navigator::tests::a_resting_agents_single_word_is_left_to_its_mark`,
  `nav_rows::a_resting_agent_reads_its_last_word_and_its_age`.

- ✅ **"New agent…" is the one start** (2026-10-04, rulings §4f). One agent had two starts in
  two words ("New agent" ran a bare `claude` in a terminal; "New … thread" lines came only from
  a linked server). Every agent-first tool has one generic start and asks the agent after.
  - **⌘⇧T, "New agent…",** asks in the palette which agent, then on which machine, then in
    which folder, and starts that agent's thread, the GUI face; Claude Code's TUI runs under it
    and is one action away, as for every observed thread (`workspace/agent_start.rs`). Each step
    lists the last choice first, so ↩ ↩ ↩ starts the last combination again, and a step with one
    choice is passed over. A machine the "+" menu chose first is not asked again.
  - **What a machine can start** comes from its own link (the agents its capabilities found
    installed) and from the server's facts, so a machine reached with no server still offers
    Claude Code.
  - **The folders** are the focused shell's on that machine, the last start's there, where its
    shells stand (most recent first), then its home.
  - **Per-agent lines skip the agent step**: the palette's "New Claude Code agent", "New Codex
    agent" and so on, one per agent any machine can start. The noun is "agent" in every start.
  - **No raw Claude terminal start.** `AGENT_COMMAND` is gone; the empty workspace's question
    and a folder's "agent here" start a thread too (the question with its text as the first
    prompt). A `claude` typed into a shell is still wired, as before.
  - Tests: `thread_start::new_agent_opens_the_picker_with_the_last_choices`,
    `per_agent_lines_skip_the_agent_step`, `a_start_with_no_server_lists_each_machines_agents`,
    `with_no_agent_anywhere_new_agent_says_so`, `the_plus_menus_machine_is_not_asked_again`,
    `a_started_thread_opens_as_a_tile_and_a_refusal_is_said`,
    `an_open_palette_takes_the_agents_as_they_arrive`;
    `palette::the_empty_workspace_asks_what_an_agent_should_do`,
    `palette::the_questions_chips_choose_where_the_agent_starts`.

- ✅ **Plan usage in the status bar, from what the agents publish** (2026-10-04, rulings §6).
  MonoCode reads an undocumented endpoint with Claude Code's Keychain token; Slopty never reads
  a credential. The plan's windows already arrive on every thread row (`ThreadRow::meters`,
  `Limit`): Claude Code's status line `rate_limits` and Codex's `account/rateLimits`, mapped by
  the agent adapters. `slopty_client::meters::PlanMeters` keeps the freshest reading per machine
  and agent from the thread tables. The bar shows the focused tile's machine's windows for its
  agent, else the freshest reading there, as `5h 23% · 7d 41%`, in `warn` from 80 %, a spent
  window with when it comes back ("5h 100% until 14:00"), and the reading's age once it is past
  a quarter of an hour. A window past its reset is dropped; no reading, no meter. A click lists
  every reading by machine and agent with its age. Nothing is polled outside the agents' own
  doors, so a Claude reading after a long idle is stale until the next turn, and says its age.
  This supersedes "plan usage in the bar … parked for the user". Tests: `slopty-client`
  `meters::tests::a_reading_past_its_reset_is_dropped`, `no_reading_shows_no_meter`,
  `the_focused_agents_reading_leads_else_the_freshest`, `a_windows_name_is_said_short`; ui
  `bars::the_status_bar_shows_the_focused_machines_plan_windows`,
  `statusbar::tests::a_plan_window_far_used_warns_and_a_spent_one_says_when_it_comes_back`.

- ✅ **The thread says what it could not do, and how to go on** (2026-10-04).
  - **A refusal line** in the activity bar, above the queue: an alert icon in `error`, the words
    ("Couldn't stop: no turn is running"), and a dismiss button. In review, a refused Keep or
    Revert is said in `error` on its file or hunk, beside the buttons, until the next try.
  - **A notice strip** at the composer's head, in `text_secondary` with an info icon (a spinner
    while a send waits on an upload). It is for what the composer itself turned down or waits
    on, and goes once the draft changes.
  - **The exited strip.** It takes the composer's place where a message would reach no agent
    (Claude Code, Codex): "{agent} exited" with a secondary Resume, "Resuming {agent}…" with a
    spinner while the start is on its way. Where the next message starts the agent again (pi,
    ACP), the composer stays and a quiet line says so.
  - **"Deny…"** is a ghost button after the plain deny. It swaps the answers for a small field
    ("Why, for the agent (optional)") with Cancel and Deny; ↵ denies.
  - **Send's secondary click queues.** A right click on Mac, or a long press on touch, sends
    behind the turn where the agent queues. It is the pointer's form of ⌘↵, so no button is added.
  - **"Answer in Codex"** replaces "Answer in the terminal" for an agent whose own TUI joins the
    thread, and brings that terminal into view once it runs.

- ✅ **Hairlines one device pixel, states that ride on their plane, floats a clear step up**
  (2026-10-03, `.research/design-systems-2026-10-03/study.md` §2.3 items 1 and 6, §3 #1 and
  #2). The person wants the chrome as finished as the best design systems while staying in the
  Zed, Warp and `MonoCode` school. The study read shadcn, coss, HeroUI, Radix, Geist, Linear,
  Raycast and Apple from source and token files and found Slopty's palette already in their
  band; what set it apart was the finish. Every line was a full point, two device pixels on a
  Retina screen, so the composer's internal rules read as a ruled form. The hover and the
  selection were solid steps mixed for the content, so a float had to sit under the hover step
  (+3 OKLCH L) or a menu row's hover would vanish on it. This amends "Design tokens" (the
  hairline's width) and "The chrome is derived from the content" (its shares and the grounds
  the text lift reads).
  - **Hairlines.** `stroke::HAIR` is half a point; GPUI rounds a stroke to whole device pixels
    and never under one, so it is one pixel at 1x, 2x and 3x. `Theme::hair` gives it, a full
    point under Increase Contrast. Every chrome border is `kit::hair` (sheets, rings inside
    buttons and selections, pane dividers, rules under bars); a divider in the flow is
    `kit::rule` or `kit::rule_v`, a border on an empty box, because a box half a point tall
    rounds to nothing at 1x. A quad painted by hand (a terminal block's rule) takes
    `kit::hair_painted`, GPUI's border rounding applied ahead. A line that is a thing's own
    edge (an unticked box, a drop target's ring, the cut round the bell's badge) is
    `stroke::EDGE`, a point. The shares grow a half again to keep the weight Linear keeps for
    its thin borders: dark `border` 0.07 → 0.10 and `border_subtle` 0.045 → 0.065, light
    0.095 → 0.13 and 0.06 → 0.085.
  - **States are washes.** `Surfaces::hover`, `selected` and `pressed` are the ink at 0.05,
    0.085 and 0.12 (light 0.05, 0.08, 0.12) laid over whatever plane is under them, as
    Linear's hover is the plane plus a step and Raycast's is white at 5 %. A row rests bare,
    takes `hover` under the pointer and `pressed` while held; a chosen row, an open menu's
    button, a toggle that is on and a key cap's plate take `selected`; a well (a secondary
    button, a code block, a chip, a field's ground) rests on `hover`. A button inside a hovered
    row needs no step of its own any more: its hover composes over the row's. The bell's
    badge cut takes the hover over the bars, solid, since it covers the bell's stroke.
  - **What floats sits +5 L.** `elevated` is the ink at 0.055 in dark (`#232323`, +5.1 L over
    the content, against +3.0 before), near Radix's +3.6 and shadcn's +6; a row's hover still
    shows on it, +4.1 L. Light keeps white floats.
  - **The text lift reads the grounds text lands on**: the planes (canvas, panel, content,
    elevated, band) and the selected wash over each, the deepest that stays (a press lasts a
    click). Muted text on a selected row of a float is the tightest pair, so the designed
    shares move to where the lift has nothing to do: dark `text_muted` 0.57 → 0.625
    (`#9c9c9c`) and `text_secondary` 0.70 → 0.715, light `text_muted` 0.66 → 0.675, and the
    dark error red one notch lighter (`#f1767e`). Hairlines are thickened under Increase
    Contrast against the planes and the hover wash over each.
  - `Hairline` is renamed `Tint` (the ink at a share: a hairline or a wash). `raised` and
    `overlay` remain only as the solid composites over the content for the call sites lane W
    still holds (`slopty-ui/src/project`, `slopty-app/src/lib.rs`, `ssh.rs`), and go when
    those move. (Amended 2026-10-04: they moved, and `raised` and `overlay` are deleted with
    `Typography::meta` and `prose`, the same sizes as `small` and `title` under second names.
    The compiler now holds what the lints `a_state_is_a_wash_over_its_plane` and
    `one_name_per_type_size` held, so those lints are gone, and no file is waived from the
    hairline and hint lints.)
  - Lints in `kit.rs`: `a_chrome_border_is_kit_hair` (no `border_1()`-style point borders or
    point-wide boxes filled with a hairline's tint) and `a_state_is_a_wash_over_its_plane` (no
    `raised` or `overlay`), each with its own check-the-check test. Tests: theme
    `the_hairlines_and_washes_are_monocode_s_shares`,
    `a_hairline_is_one_device_pixel_and_a_point_at_more_contrast`, `the_ladder_is_monotonic`
    (each state a step past the last on every plane), `the_washes_stand_off_the_chrome`,
    `a_float_rises_and_its_rows_still_answer_the_pointer`, `chrome_text_clears_wcag_aa`; kit
    `a_painted_line_is_one_point_in_whole_device_pixels`, `a_button_is_neutral_and_one_height`; ui
    `tiles::a_divider_runs_only_between_neighbours` (one device pixel, in the border's colour),
    the terminal's block rule tests.

- ✅ **Two named elevations, cards on a quiet well, and the lit rim on everything raised**
  (2026-10-03, `.research/design-systems-2026-10-03/study.md` §2.3 items 4 and 5, §3 #3, #4 and
  #11). The kit had one elevation, for floats, so a card was either a flat fill or floating;
  every reference has a pair (Geist base and menu, Radix shadow 2 and 5, HeroUI surface and
  overlay, coss rim and large shadow). In light, cards and onboarding rows were grey slabs on
  white where every reference sets white cards on a faintly grey well. This amends "One overlay
  shell, one alpha ladder" (a second, resting elevation beside the floating one) and, for light
  only, "Onboarding lists are tone steps".
  - **`Elevation` names both.** `shadow` stays the floating one; `rest` is the resting contact,
    light only (`0 1 2` at 5 %, coss's every shadow); `rim` is one rule for what is raised: a
    point of white along the top edge in dark (`alpha::RIM` 0.04 at rest, `alpha::EDGE` 0.06
    floating), a point of black along the bottom edge in light (0.04 at rest; a float's drop
    shadow already ends it). The ad-hoc shadow the old ruling forbade stays forbidden: the
    shadows and rims are drawn only by `kit::elevate`, `kit::card` and `kit::rests`, and
    `a_floating_layer_wears_the_one_elevation` still fails on any other.
  - **`kit::card`** (and `kit::card_part` for a card whose rows are children of their own) is
    a thing resting on its plane: `radii.md`, the rim, and in light white with a hairline round
    it and the contact; in dark the hover wash, a step over whatever it rests on (+4.7 L on the
    content), so a card on a sheet still rises from the sheet, with a hairline only under
    Increase Contrast. `kit::well` is the ground cards rest on: `band` in light, the plane
    itself in dark.
  - **Light cards are white on a quiet well**: the settings page is a well with its groups as
    white cards (System Settings' layout), and an install's steps are a card. Dark keeps its
    tone steps, now with the rim. Board lanes and cards and the first run's rows are lane W's
    files and move onto `kit::well` and `kit::card` there.
  - **The rim on everything raised**: the secondary button carries it inside its hairline ring,
    the card and the floating sheet carry theirs; the composer's shell and the segmented
    control's thumb take it as they are built (below).
  - Tests: theme `elevation_and_density` (the pair, the rims' sides and steps); kit
    `a_card_rests_on_its_rim`, `a_secondary_button_catches_the_light`,
    `the_elevation_is_two_layers_of_the_shade_and_a_lit_edge_in_dark`; settings
    `a_groups_rows_are_one_ring_under_its_label`.

- ✅ **Stage 3 of the design-systems study: one composer shell, capsules, quieter marks,
  motion that respects frequency, and the finishing details** (2026-10-03,
  `.research/design-systems-2026-10-03/study.md` §2.3, §3 #5 to #10 and #13 to #22, §4 items 5
  to 7). Each item is the study's row; the evidence and the systems compared are there.
  - **The composer is one shell** (#5). The checkout, branch and change count left their row
    over the field and sit in the toolbar after the chips, giving up their room first, so the
    composer is the field and its toolbar with no line inside. The tray over it parts its
    request, plan, edits and queue by room alone; the only line in the stack is the quieter
    hairline (`border_subtle`) where the tray meets the field. Composer and tray rest on the
    resting elevation through `kit::rests` (the tray takes the dark rim on its top edge, the
    composer the light contact under its foot). Tests: the kit's own elevation lint; goldens
    `thread*`.
  - **Pills are capsules** (#6), amending "Design tokens": `radii.full` at 20 pt, the tone at
    `FAINT` in dark and the new `alpha::FAINT_ON_PAPER` (0.10) in light, since a 4 pt box at
    20 pt reads as a tag or a button and these are states. Key caps keep `radii.xs`: they are
    keys. Test: `a_pill_is_a_twenty_point_chip_at_its_zoom` (a 6 pt chip since "The design system starts
    from MonoCode's").
  - **A quieter focus line and overview ring** (#7, #16), amending §4b's numbers: the line is
    `stroke::MARK` 1.5 pt in `text` at the new `alpha::FOCUS` (0.45, 3.95:1 over the content,
    above the 3:1 asked of what is seen), inset `radii.sm` at both ends with round caps, whole
    under Increase Contrast. The overview's active workspace is ringed one device pixel of
    `text` at `RING` (now 0.3) outside a 2 pt gap, 1.5 pt whole under Increase Contrast. Tests:
    `focus_line::*`, `strip_marks::the_overview_lifts_each_workspace_and_offers_a_new_one`.
  - **Pressed is its own step, and a hover lets go softly** (#8). Hover is the `hover` wash,
    pressed the `pressed` wash, on every kit control. GPUI-fast's state transitions ease a
    fill into a hover or press at once (`Motion::hover`) and back to rest over the new
    `Motion::unhover` (150 ms, eased out), applied at once under Reduce Motion. One kit helper,
    `kit::eased`, owns the durations; the kit's buttons and the rows of menus, the inbox, the
    bars' popovers and the settings wear it. The navigator's rows do not: its list is a
    composited scroll layer, and a fill easing out under rows scrolling past the pointer
    repaints the layer (headless, 2 of 30 scroll frames composited against all 30;
    `nav_list::a_scroll_of_the_navigator_composites_its_layer`).
  - **Overlays leave, and what the keyboard summons does not travel** (#9), amending the
    Motion doc. The palette, the picker, project search, About and the settings dialog fade in
    where they stand; menus and popovers opened by the pointer keep their 4 pt drop. Every
    overlay leaves on the new `Motion::exit` (100 ms, shorter than any entrance): its owner
    keeps drawing it one exit longer through `kit::fade_out` (since 2026-10-04
    `kit::Presence`, below), which holds the pointer only
    within the leaving panel (a modal's dim holds it as the modal did) while the keyboard is
    back where it was. Under Reduce Motion, and where the workspace holds its chrome still,
    nothing is kept. Tests: `the_paces_are_the_motion_tokens`,
    `the_palette_leaves_quicker_than_it_came`.
  - **A finished primary** (#10): the solid carries a point of white at 0.14 inside its top and
    a contact shadow in light, a shade of black at 0.10 inside its foot in dark; pressed, both
    go and a shade at 0.08 presses in (`Elevation::finish`). Test: `the_primary_presses_in`.
  - **A softer dark scrim** (#14), amending "One overlay shell, one alpha ladder":
    `alpha::SCRIM` 0.6 → 0.45, Linear's 0.4 the bound. Light keeps `DIM`.
  - **Empty states seat their mark** (#15): `kit::notice` puts it in a 40 pt disc of the hover
    wash, the icon at the icon size in `text_secondary`.
  - **A segmented control with a sliding thumb** (#13): a track of the hover wash held
    `kit::TRACK_PAD` in, the chosen option on a thumb painted as a card (the floating surface,
    the rim, in light its hairline and contact) at the concentric radius, sliding on the
    selection plate's settle. A hairline parts two options not chosen. The chosen label is set
    medium in the room the medium weight takes, which every label keeps, so nothing reflows.
    The settings form's choices and the inbox's Unread and All take it.
  - **Tool cards and thoughts** (#17): a call that acts is a `kit::card` at `radii.md`; a failed
    one is edged in the error tone at the new `alpha::FAILED_EDGE` (0.30) where it was dashed,
    one that waits on the person in the warn tone at `alpha::ASKING_EDGE` (0.40). An opened
    thought hangs from the hairline rule a quiet call's output hangs from. Test:
    `a_call_that_acts_is_a_card`.
  - **One name per type size** (#18): `meta()` and `prose()` repeated `small()` and `title()`;
    the scale reads caption, small, ui, title, heading, display. The two names remain only for
    lane W's call sites until they move. Test: `one_name_per_type_size`.
  - **Hints come as a warm group** (#19): the first hint waits 400 ms under a resting pointer;
    while one was on screen in the last second the next shows at once. GPUI builds a hint at
    once (`kit::hint_timing`) and the hint keeps the wait. Tests: `hints_come_as_a_warm_group`,
    and the lint `a_hint_keeps_the_warm_timing`.
  - **Group rules fade at their ends** (#20): `kit::list_rule` parts a menu's groups (and the
    machines popover's add row) with a `border_subtle` hairline that fades over its last 24 pt
    at each end through GPUI's per-pixel `edge_fade`, never a gradient. It draws only while a
    menu is open, off the terminal and input paths.
  - **Nesting guides on hover** (#21): a nested row in the navigator (a project's tiles, its
    board and threads) carries a hairline in `border_subtle` under the middle of its parent's
    icon, shown only while the pointer is over the navigator.
  - **Selection follows key state** (#22): `kit::selected` takes whether its list has the
    keyboard; without it the selection steps down to the hover wash with no ring, so one list
    in the window shows a live selection. The settings sidebar follows it. The navigator's
    selection is the focused tile, where the keys are, so it stays live. Test:
    `a_button_is_neutral_and_one_height`.
  - Deferred, as ruled: the shimmer on the working line (#23), hold-to-confirm (#24) and the
    in-window backdrop blur (#25).

- ✅ **The design lane's next stages are one ranked list from three more studies** (2026-10-03).
  `.research/gpui-references-2026-10-03.md` (Frame, Waku, Sonora, bezel, tty7; the GPL ones are
  read for ideas and rewritten, never copied), `.research/t3code-ui-2026-10-03.md` and
  `.research/designeer-2026-10-03.md`, merged after the design-systems study's stage 3 above and
  ranked by what the person sees per unit of effort. What stage 3 already built is left out
  (capsules, the composer shell, exits, the segmented thumb, the warm hints, `kit::eased`); the
  stale "T3 Code's 768" note on the reading column now says 736.
  1. **An APCA floor for dark text** (designeer #1). `Rgb::apca` beside `Rgb::contrast`; the
     lift takes the stricter of WCAG and APCA (secondary |Lc| ≥ 55, muted ≥ 45 on every
     ground), and the focus line derived to clear Lc 30 as well as 3:1. The values come from a test that
     computes them (`chrome_text_clears_apca`), never from the eye.
  2. **Shadows that fall, and the sunk finish** (gpui-references T2 and T1). `Shadow` gains a
     spread (the soft floating layer `0 12 32 −10`, a third heavier); `Elevation::sunk` shades
     the top edge of what is sunk, with `kit::sunk` and `kit::track` on fields, the segmented
     tracks and meters. The rule then reads whole: what is raised catches light at its top
     edge, what is sunk holds shade there.
  3. **Live marks breathe under Reduce Motion** (gpui-references T5): opacity only between 0.6
     and 1 over `Motion::breath` (2.4 s), stepped on the spin clock, with no travel or scale,
     since a frozen mark reads as hung and the platform keeps activity alive under Reduce
     Motion. The Reduce Motion rule above is amended in the same change.
  4. **Message actions in the thread view** (T3 #1): copy and a day-aware time on each message,
     and "Fork from here" on a settled turn where the thread's caps hold `Cap::FORK`, sending
     `Intent::Fork` (the wire is on main).
  5. **The palette finds better** (T3 #3, designeer #3): a score per item (exact, prefix, word
     start, substring, then context; recency breaks ties), the matched letters drawn in
     `text` over `text_secondary`, `>` for commands only, ↑↓ through the navigator filter's
     results, and Esc clearing a query before it closes the palette or the picker.
  6. **The send button says what ↵ will do** (T3 #6, its UI half): Send, Steer, Queue or
     Update with their glyphs and names; "Queue message" as a keymap action; ⌥↑ edits the last
     queued message; the meter opens a popover with the windows, their resets, the cost and
     Compact where `Cap::COMPACT` holds. Promote and reorder wait on their intents.
  7. **Notices with a severity and a memory** (T3 #7, designeer #2): a failure stays until
     dismissed and offers Copy; a notice's clock holds while the window is not key and gives
     two seconds more once it is; a thread's notice shows three lines with "Show all".
  8. **The thread view at the old face's level** (T3 #2): ↑ recalls earlier prompts at the first
     line, the plan as a card with "Plan ready" in the tray, a picture opens the viewer, and the
     live answer fades in once lane F pins the paced stream fade in gpui-kit.
  9. **Review comments carry the code** (T3 #4): each comment a fenced excerpt with its line
     range, a drag makes a range, and "Add to message" puts them in the draft instead of
     sending.
  10. **A terminal selection to an agent** (T3 #5): "Attach" on any selection, fenced and named
      after its command, and a palette line for it.
  11. **"3 new" on the Latest pill** (designeer #4), capped at 99+, said politely after 700 ms.
  12. **Labels fade at their tail when they overflow** (gpui-references T8): `kit::fit_label` on
      navigator rows, composer chips and tray rows, through `edge_fade`'s `hidden_by_scroll`;
      tile titles keep their ruled ellipsis.
  13. **A prompt and prose size of its own** (T3 §1.7): a setting that grows what is read
      without growing the chrome.
  14. **The phone's sheets follow the finger** (designeer #6), once phone work resumes.
  - **Waiting on lane F, built here once pinned:** one reversible value for open and close
    (the gpui-ce port, Apache; it would replace the leaving states stage 3 added), hints on
    keyboard focus, and the paced stream fade.
  - **Measured first:** intent prefetch on the navigator. (The navigator on system glass
    landed, then went: the person ruled every surface solid, in "The design system starts from
    MonoCode's".)

- ✅ **SF's optical size and tracking, measured** (2026-10-03, study §3 #12). The study asked
  whether GPUI sets the system face as Core Text does. A test lays out "Connect to a server"
  at 13, 20 and 26 pt through GPUI and through a Core Text line from `NSAttributedString`
  and compares the advances and the face. Both name `.SFNS-Regular` at every size, and the
  advances agree (13 pt 119.13 both; 26 pt 217.56 both; 20 pt 171.43 both). GPUI set 171.61
  at 20 pt until lane F's gpui-fast#25 (cc85e69, pinned 2026-10-04): it shaped alternate runs
  at the next float above the size, which crossed SF's optical step at 20 pt. The test now
  holds GPUI to Core Text's width within 0.01 pt. Set in proportion to 13 pt, the 26 pt line would be 238.26
  wide, so the face carries Apple's tracking per size already; nothing needs `letter_spacing`
  in the renderer. Test: `fonts::optical_size::gpui_sets_the_system_face_at_its_optical_size_and_tracking`.

- ✅ **Chrome text clears an APCA floor as well as WCAG's** (2026-10-03,
  `.research/designeer-2026-10-03.md` §3 #1; stage 4 item 1). WCAG's ratio calls a dark text
  tier the equal of its light twin where APCA reads it 20 to 30 Lc weaker: by WCAG alone dark
  muted text sat at Lc 43 on its worst ground (the selected wash over a float) and the dark red
  at 43, against APCA's 45 for text read beside large or heavy type.
  - `Rgb::apca` is APCA-W3 0.0.98G; a test holds it to the published values (`#888` on white
    Lc 63.06, white on black −107.88).
  - The lift takes the stricter of WCAG's ratio and an APCA floor on every ground text lands
    on: secondary |Lc| 55 (APCA's 60 for content text that is not body, less 5 for chrome at 12
    to 13 pt medium), muted and the status words (accent, warn, error, the agent's orange) 45.
    The default dark tones moved to where the lift has nothing to do: `text_secondary`
    0.715 → 0.745 (`#b5b5b5`), `text_muted` 0.625 → 0.651 (`#a1a1a1`), the red `f1767e` →
    `f27d84`. Measured on the default dark: text Lc 88.7, secondary 56.2, muted 45.3, accent
    50.5, warn 65.4. Light does not move, since WCAG already binds there (its muted is Lc
    62.8).
  - **The focus line is derived, not a fixed share.** `Surfaces::focus` is the text at
    `alpha::FOCUS` (0.45), laid on only as much thicker as the content needs for both WCAG's
    3:1 and APCA's Lc 30 for a mark that means something. At 0.45 it read Lc 24.8 on black,
    29.4 on the default dark, and 2.85:1 on white, each short of one floor; whole under
    Increase Contrast.
  - Tests: theme `apca_gives_the_published_values`, `chrome_text_clears_apca`,
    `the_focus_line_clears_apca_for_what_is_seen`,
    `the_default_tones_are_lifted_by_a_rounding_step_at_most`; ui `focus_line::*`.

- ✅ **A working mark breathes under Reduce Motion** (2026-10-03,
  `.research/gpui-references-2026-10-03.md` T5; stage 4 item 3). This amends the rule that
  everything lands at once under Reduce Motion, for the one mark that says an agent is at
  work. Standing still, it read the same as a mark that had stopped, and on an app whose point
  is what needs the person at a glance that lost the one live cue; the platform itself keeps
  its activity indicators alive under Reduce Motion. So the mark stays upright and its opacity
  breathes from 0.6 to whole and back over `Motion::breath` (2.4 s), in steps of a fifth of a
  second (twelve to a breath, so each breath ends on a step) on the same app-wide clock: no
  travel, no turn, no scale, and under half the frames the turn draws (headless, 5 a second
  against 12). Everything else still lands at once. Tests:
  `icons::tests::the_working_mark_steps_twelve_times_a_turn_and_stands_under_reduce_motion`
  (the breath's range, period and steps), `a_working_mark_wakes_its_view_only_while_it_shows`
  (4 to 6 wakes a second under Reduce Motion), and
  `workspace::tests::frame::a_working_mark_draws_twelve_frames_a_second_and_none_at_rest`.

- ✅ **Floats cast a shadow that falls, and what is sunk holds shade** (2026-10-03,
  `.research/gpui-references-2026-10-03.md` T1 and T2; stage 4 item 2).
  - `Shadow` gains a spread, which `kit::drop_shadow` passes through. The soft layer of a
    float is now `0 12 32 −10`: drawn in by ten points, so its shape starts two points below
    the top edge and shows no halo at the sides (at most six points past them), only a shade
    that falls under. Drawing it in thins it, so it is a third heavier: dark 0.125 → 0.16 and
    light 0.10 → 0.13. The contact layers keep no spread, and light's resting card draws its
    contact in by half a point so the hairline stays its edge.
  - `Elevation::sunk` is the other half of the rest rule. A card catches light at its top
    edge, and what is sunk (a field, a segmented track, the settings well) holds shade there
    instead. `kit::sunk` lays an inset shade a point inside the top edge, past any border: dark
    at 0.18, light at 0.05. In dark it adds a lip of light (`alpha::RIM`) inside the bottom,
    where light falls on a hollow's far wall. Light has none, since a white lip on a white
    ground reads as nothing. `kit::track` is the segmented track, the hover wash sunk, and the
    inbox's views wear it. The fields wear `kit::sunk` too: the navigator's filter, the settings
    well, the first run's address field, and the SSH sheet's fields.
  - Tests: theme `elevation_and_density` (the soft layer falls, starts below the top edge and
    keeps its halo within six points), kit
    `the_elevation_is_two_layers_of_the_shade_and_a_lit_edge_in_dark` (the soft layer drawn
    in) and `a_field_sinks_where_a_card_rises`.

- ✅ **A message offers its copy and its day, and a settled turn offers a fork** (2026-10-03,
  `.research/t3code-ui-2026-10-03.md` #1; stage 4 item 4).
  - Under each message (the person's bubble, right-aligned, and the agent's answer) a quiet
    row shows while the pointer is on the message, and always under a finger. It holds the
    time the message was written and a copy of its words that says "Copied" with a check for
    `COPIED_FOR`, as the old face's answers do. The time is said for today
    (`figures::stamp`): the clock alone today, "Yesterday", a weekday within the week, and a
    day and month past it.
  - A settled turn's fold offers "Fork from here" (the branch glyph, with a hint) where the
    thread's caps hold `Cap::FORK`. It is never offered on a turn under way. The press sends
    `Intent::Fork { after: Some(turn) }` through the agent's own door, and the fold keeps its
    state, since the press is the fork's and not the fold's. The new thread arrives as any
    other does.
  - Tests: `figures::tests::a_message_s_day_reads_by_how_far_back_it_is`, and
    `conversation::thread::tests::steps::a_settled_turn_forks_from_its_fold_where_the_agent_can`
    and `a_message_s_copy_puts_its_words_on_the_clipboard`.

- ✅ **The palette ranks within each section, shows what matched, and Esc takes a query back**
  (2026-10-04, `.research/t3code-ui-2026-10-03.md` #3 and `.research/designeer-2026-10-03.md`
  §3 #3; stage 4 item 5). This supersedes the 2026-09-14 ruling against ranking, whose own
  revisit condition now holds: the fixed list grew from a few dozen short noun phrases to 127
  commands that carry incidental matches.
  - **Measured first.** `palette::tests::palette_scoring_on_the_real_commands` (ignored; `cargo
    test -p slopty-ui --lib -- --ignored palette_scoring --nocapture`) runs every word of every
    label, and its first three letters, through the real list. 140 queries match two lines or
    more; ranking changes the first line of 55. Most of them now lead with the line that was
    meant: `page` gives Page back, not Open last offered page; `copy` gives Copy last output,
    not Save a copy…; `workspace` gives Workspace above, not Tile or workspace above; `select`
    gives Select all text; `over` gives Overview; and `start` gives Start a project here, not
    Move column to the start. A few go the other way: `down` leads with Download… over Move
    workspace down, and `fil` with Filter the navigator over Open file…. Neither is wrong, and
    the next letter settles both.
  - **Ranking.** The score is the whole label (4), its start (3), every word at a word's start
    (2), inside the label (1), or only in the line's context (0). A command run lately breaks a
    tie, and after that the order the lines came in. Lines rank only within their section, so a
    tile is never pushed under a command and a found file never rises over a tile. An empty
    field keeps the brief list as it was. Matching is unchanged: whole words, any order, no
    subsequences.
  - **What matched shows.** The label draws its matched words in `text` over the rest in
    `text_secondary` (`StyledText` highlights; nothing for a label whose lowercase changes its
    byte length).
  - **`>` asks for commands only**, as in VS Code and T3 Code. It never spells a path or asks
    the worker for files.
  - **Esc empties a field that holds text and closes only an empty one**, in the palette and
    the window picker (interior.dev's rule), so a wrong query is taken back without
    reopening.
  - **↑↓ walk the navigator filter's tiles.** While the filter holds text, ↑ and ↓ mark the
    next tile it left, round at the ends, as the focused tile's row is marked. ↩ goes to that
    tile, or the first when nothing was walked. The keyboard stays in the field.
  - Tests: `palette::tests::a_query_ranks_each_section_and_shows_what_it_matched`,
    `workspace::tests::the_command_palette_runs_an_action_by_name`,
    `workspace::tests::nav_rows::the_filter_keeps_the_rows_that_match`.

- ✅ **The send button says what ↵ will do, ⌘↵ and ⌥↑ are keymap actions, and the meter opens a
  panel** (2026-10-04, `.research/t3code-ui-2026-10-03.md` #6; stage 4 item 6).
  - **The one solid names its press.** Stop while a turn runs and nothing is typed, as before.
    Otherwise it is Update while a waiting message is being changed (a check), Send with no
    turn under way (the up arrow), Steer into a turn where the agent takes a steer (the arrow
    that turns in), or Queue after it where the agent only queues (the end of a list). The
    glyph and the accessible name change together, and the hint says the name alone. The keys
    stay in the palette.
  - **"Queue message" (⌘↵) and "Edit the last queued message" (⌥↑) are keymap actions.** They
    are bound in the thread composer's own key context (`ThreadComposer > Input`), so the old
    face and a question's own field in the tray keep their ⌘↵. Both are listed in the palette. ⌥↑
    takes the last waiting message that can still change into the composer, as its pencil
    does, and does nothing when none can.
  - **A press on the meter opens its panel in the tray**, where the background panel opens.
    It shows the context, each of the plan's windows with when it resets (said for today,
    `figures::stamp`), and what the session cost ("$1.24 this session", rounded up to a
    cent). Where the agent compacts through Slopty (`Cap::COMPACT` and no `/compact` of its
    own) it offers "Compact context", which sends `Intent::Compact`. Compacting stays the
    person's press. The hint keeps the same lines for a glance.
  - Sending a waiting message now and reordering the queue wait on lane A's `Promote` and
    `Reorder` intents. Until their buttons land, the thread names their refusals ("Couldn't
    send the message now", "Couldn't move the message").
  - Tests in `conversation::thread::tests::composing`: `the_send_button_says_what_return_will_do`,
    `command_return_queues_and_option_up_edits_the_last_waiting`,
    `the_meter_opens_its_panel_and_compacts_on_a_press`.

- ✅ **A failure stays until dismissed, no notice lapses unseen, and a thread's notice shows
  three lines** (2026-10-04, `.research/t3code-ui-2026-10-03.md` #7 and
  `.research/designeer-2026-10-03.md` §3 #2; stage 4 item 7).
  - **Failures are their own kind.** `show_failure` puts up a notice with the failed mark that
    has no timer. It offers Copy (its words to the clipboard) and Dismiss. Past the two shown,
    the oldest word goes before any failure does. Settings that did not parse, a save, an
    upload, a drop or a drag that did not land, a port not forwarded, a terminal's refusal, a
    machine not forgotten and a project verb's error are failures now. Words that only inform
    stay plain notices.
  - **A notice's time waits while the app is not in front**, as it waits under the pointer:
    one whose time comes then is held, and once the app is back it gets `SAY_AFTER_HOVER`
    (2 s) more. On this Mac the person is often in another app over Parsec while agents work,
    and "Undo close", a pointing or an offered page lapsed unseen.
  - **A thread's notice shows up to three lines**, then "Show all", where it showed only its
    first line (an agent's error often says what to do on its second).
  - **A usage limit says when it resets** where the agent says (`AgentStatus::Failed` with
    `until_ms`): "Hit its usage limit · resets 14:05".
  - Tests: `workspace::tests::toasts::a_notice_waits_while_the_app_is_not_in_front`,
    `a_failure_stays_until_dismissed_and_copies`,
    `conversation::thread::view::notes::tests::a_notice_shows_three_lines_and_says_when_there_is_more`,
    `workspace::agents::tests::a_failed_turn_says_why_and_when_a_limit_resets`.

- ✅ **A surface that comes and goes is one reversible value, and a hint answers the keyboard**
  (2026-10-04, lane F's gpui-fast #23 and #24, pinned at cc85e69; the ranked list's "waiting on
  lane F" items).
  - **`kit::Presence`.** A menu, the inbox, a status-bar popover, About, the window picker and
    the settings dialog are drawn through one `ValueTransition` each (gpui-fast's port of
    gpui-ce's, Apache), kept under the surface's id while it is drawn. Open, it eases toward
    whole over `Pace::Fade` (with the menus' and the inbox's 4 pt drop); closed, toward clear
    over `Pace::Exit`. Opened again on its way out it turns back from where it stands, in the
    time the way back takes, where the one-shot fades it replaces started over from clear.
    While it leaves, a blocker over it holds the clicks within its bounds, as before. The
    owners still keep the closed surface for `kit::exit_time` and drop it then. `kit::fade_out`
    is gone. The palette keeps its own exit, since the phone's sheet leaves on the drawer's
    curve. Test: `kit::tests::a_surface_opened_on_its_way_out_turns_back`.
  - **Hints show for the keyboard.** gpui-fast shows an element's tooltip while it is
    focus-visible, so every kit control on the keyboard ring (`a11y::tab_stop`, which gives it
    a focus handle) names itself when Tab reaches it, on the warm group's wait
    (`kit::hint_timing` composes unchanged), and Esc hides the hint before any binding, the
    focus staying. Test: `kit::tests::a_hint_shows_for_the_keyboard_and_esc_hides_it_first`.

- ✅ **An answer's new words lift in at the stream's pace** (2026-10-04, lane F's gpui-kit #7
  `TextViewMotion::with_stream_fade_pacing`, pinned; stage 4 item 8, its stream fade).
  - The thread view showed an answer's words as they arrived, with no lift. The older
    conversation view lifted them over gpui-kit's fixed 280 ms. A fixed lift is wrong at both
    ends: on a quick stream the tail stays grey, and on a slow one each chunk lights up and
    then sits still until the next arrives, so the text pulses.
  - **Paced.** `kit::stream_motion` lifts each update's words over three times the running
    gap between updates (gpui-kit's rule). It keeps that between `Motion::fade` (120 ms) and
    `Motion::stream` (400 ms), two new theme tokens, and lifts faster when updates queue
    behind. Each word starts `Motion::stream_stagger` (10 ms) after the one before, on the
    chrome's ease-out curve. An answer's first words come after the agent's pause and have
    no pace to follow yet, so they lift over the longest, which is how gpui-kit counts a
    pause past its bounds.
  - **Where.** In the thread view, only the latest turn's answer lifts. It keeps lifting
    once the turn settles, so its last words finish instead of snapping. An earlier turn's
    text, a "Show all" on it, and anything under Reduce Motion land at once. The older
    conversation view's live block uses the same motion.
  - Tests: `conversation::thread::tests::face::the_latest_answer_s_new_words_lift_in`
    (counts the fade's repaint ticks; it fails if every turn lifts or none does),
    `kit::tests::streamed_words_lift_on_the_motion_tokens`, and theme
    `a_curve_runs_from_rest_to_landed` (the stream's bounds and stagger).

- ✅ **The thread view recalls what was sent, reads a plan as a card, and opens a picture
  large** (2026-10-04, `.research/t3code-ui-2026-10-03.md` §3.2; stage 4 item 8). The thread
  view had none of what the conversation view gives here, and T3 Code has all three.
  - **Recall.** ↑ on the first line of an empty composer brings back the message sent
    before. ↓ on the last line brings back the one after, and past the newest it empties the
    draft. Inside a recalled message of several lines the arrows move the caret first, as T3
    does at the first visual line. The conversation view takes the key whatever the line,
    which left the caret stuck in a long recalled prompt. A recalled message that is edited
    is the person's draft, and the arrows leave it alone. A message that arrived clipped is
    left out, since its head is not what was typed. One sent twice running is recalled once.
    The open menu still takes the arrows first.
  - **A plan is a card** (`ToolDetail::Plan`): its heading as its title, how it stands only
    where it was put to the person ("Awaiting approval", "Not approved"; Codex's stated plan
    says nothing), a copy, and its Markdown at the prose size. A plan over 16 lines shows 12
    until "Show the whole plan". It stands outside a settled turn's fold where it was
    proposed, is not one of the fold's steps, and joins no group of quiet calls. While it
    waits it is edged in the warn tone and takes the answers, as a call's card does.
    Scrolled away, the tray names it "Plan ready" with the way back. It no longer copies
    the plan's words into the tray.
  - **A picture opens large** on a press, fitted over the thread on the scrim, with its
    words ("1600 × 1200 · PNG · 240 KB", the conversation view's, now shared). Esc closes it
    from the composer or anywhere else in the thread, and the Esc that closes it never also
    stops the turn under way. A press anywhere closes it.
  - Tests: `conversation::thread::tests::composing::up_recalls_the_messages_sent_and_down_comes_back`,
    `conversation::thread::tests::face::a_plan_in_the_thread_takes_its_answer`,
    `conversation::thread::tests::face::a_picture_opens_large_and_esc_closes_it`,
    `conversation::thread::rows::tests::a_plan_stands_outside_the_fold_and_out_of_a_group`,
    `conversation::thread::view::plan::tests::{a_plan_says_how_it_stands_only_where_it_was_asked,
    a_long_plan_shows_its_head_until_opened}`.

- ✅ **An agent asleep is woken from its thread, and an attempt is picked from its row**
  (the Wake half went the same day with lane A's sleep, `docs/decisions/agents.md`, "Sleep,
  waits on another thread, queue reordering and edited allows are gone")
  (2026-10-04, lane A's `Intent::Wake`/`Liveness::Asleep`/`Cap::SLEEP` and lane P's R11
  `Attempts`/`Verb::TaskPick`).
  - **Wake.** An agent put to sleep keeps its composer: the line over it says "{agent} is
    asleep. Your next message wakes it", with a pause mark where an exited agent has the power
    mark. Where the agent sleeps through Slopty's door (`Cap::SLEEP`), a Wake button beside the
    line sends `Intent::Wake`. Anywhere else there is no button, because only the next message
    wakes it. Test: `conversation::thread::tests::doors::an_agent_asleep_is_woken_by_wake_or_the_next_message`.
  - **Pick.** While none of a task's attempts is picked, the task's row says "Trying" (lane
    P's word) and every attempt that has not failed offers Pick. Pick is also in the palette
    as "Pick this attempt", on the board's bare `p` as its other row actions are, and sends
    `Verb::TaskPick`, saying "Picked #8 to land; the other attempts stop". No attempt offers
    Merge until it is picked, because only the picked one joins the merge queue. After the
    pick, only it offers Merge and none offers Pick. Test:
    `workspace::tests::projects::an_attempt_is_picked_from_its_row`.

- ⛔ **A message can be sent later, and what waits says when it goes** (2026-10-04, lane A's
  `Delivery::At`/`After`/`Draft` on `Cap::SCHEDULE`). The composer's clock and its menu were
  cut the same day; see "One way to branch, and continuing when a limit lifts" below. What
  waits still says when it goes.
  - **The composer.** Where the agent takes a message the worker holds until its moment, a
    clock beside the send button opens a menu over the field. It offers "In 30 minutes", "In
    an hour", "In 3 hours" and "Tomorrow morning" (09:00 in this machine's zone), each with
    its time at the right. Under them, one line for each of the worker's six most recently
    changed other threads: "When “Fix the parser” rests", once that thread has been at rest
    for a minute. A minute is long enough that a turn's quick follow-up is not taken for its
    end. A pick writes "Sends at 14:35" or "Sends tomorrow 09:00" over the field, with an ×
    to take it off, and the send button says Schedule with the clock. ↵ or ⌘↵ then sends the
    draft held until then, and the next draft goes as ↵ sends. "Send later…" is in the
    palette, with no default key, as the palette rule has it. The keyboard goes back to the
    draft after a pick.
  - **What waits.** A waiting message in the tray says when it goes ("14:35", "Tomorrow
    09:00"), what it waits on ("When “Fix the parser” rests", or "When another thread rests"
    where the table does not name it), or "Draft". A held one says why it is held, as before.
    Every message the worker keeps, a plain queued one too, offers Send now
    (`Intent::Promote`) wherever the agent queues (`Cap::QUEUE`, the cap the wire puts on
    Promote), as a quiet icon beside Edit and Take back. An agent that steers takes it into the
    turn under way, and an ACP agent is stopped and sends it first (amended 2026-10-06: it was
    held messages on steering agents only, which left ACP and the person's commonest case,
    a queued message that cannot wait, without it). Take back now also holds where only `Cap::SCHEDULE` does. Each row is a list
    item named by its words and where it stands, so a screen reader hears when it goes.
  - Day words read forward too ("Tomorrow", a weekday within the week, a date past it), so a
    time to come reads as a time gone does.
  - Tests: `conversation::thread::tests::composing::{a_draft_is_sent_later_from_the_clock,
    a_held_message_says_when_and_goes_now_on_a_press,
    a_queued_message_goes_now_on_a_press_with_or_without_a_steer}`,
    `conversation::thread::view::later::tests::{a_held_message_says_when_it_goes,
    the_times_run_forward_from_now}`, `conversation::figures::tests::{tomorrow_at_an_hour_reads_as_tomorrow_then,
    a_message_s_day_reads_by_how_far_back_it_is}`.

- ✅ **The tray's sections each stand apart, and background work says what is true**
  (2026-10-04, a design review of the `thread-work` renders).
  - **Sections.** The tray over the composer stacks refusals, answers, the plan, the edits,
    the queue, the commands run in the background, the agent's background tasks and the meter.
    (Superseded 2026-10-07 by "One reading measure, MonoCode's": a base unit of space parts
    them, no hairline.)
    Each is now parted from the next by a `border_subtle` hairline. Before, they were parted
    only by space, and a background command read as a step of the plan above it. This amends
    the earlier "no rule between the two" ruling for the composer's pending work. The
    background tasks opened from their chip sit under their own quiet head, "In the
    background" with their count words and a chevron that folds them.
  - **Output.** A command's row is its title, then how it stands at the right in tabular
    figures. While it runs, its last line of output sits under it as a second line, in the code
    face and the quiet tone, cut to one line past the mark's slot. Once it ends, the line goes,
    because "Completed · 1m 5s" says all that is left to say. Raw output, backticks and all,
    never runs inline after a title in body text.
  - **The chip says what is true.** While any task runs it says "2 running", with a spinner.
    Once none does it says how they ended: "1 finished", "2 finished · 1 failed",
    "1 stopped". A failure turns its mark to the error tone. "1 in the background" read as
    still running, so it is gone. The chip stays rather than hiding, because what finished
    while the person was away is worth one glance and one press. The words are one function,
    `tray::tasks_words`, shared by the chip and the head.
  - **A plan card's head is its title.** The map mark says it is a plan, so the "Plan" label
    that ran into the title at the same weight is gone. The title stands alone at the base size
    and the medium weight, a step over the card's small chrome (the strong weight stays for
    titles of panels and pages, as `kit::tests::strong_weight_is_for_titles` holds), and how it
    stands sits muted at the small size. Screen readers still hear "Plan: {title},
    {standing}".
  - **The picture frame** already follows the picture's aspect up to the 3:1 cap. The
    renders' frame looked empty because the made-up screenshot was a 640 × 400 page that was
    mostly white. It is now a 600 × 200 crop of the header it shows, as a person pastes one.
  - Tests: `conversation::thread::tests::face::background_work_opens_from_a_chip`,
    `conversation::thread::view::tray::tests::background_work_says_what_is_true`,
    `conversation::thread::tests::face::a_plan_in_the_thread_takes_its_answer`; goldens
    `thread-work`, `thread-work-open`.

- ✅ **Review comments carry the code, as a range, and can go into the draft** (2026-10-04,
  stage 4 item 9, T3 #4).
  - **Ranges.** A press on a line of the review tile comments on it, as before. A drag over a
    hunk's lines comments on the run, and a shift-press past the line being commented on
    reaches to it, so a run is a press and a reach where a drag is awkward. A run stays within
    one hunk, because a quote is one contiguous diff. Picked rows wear the accent's faint
    wash, the one the conversation face's diff quotes wear, laid over each line's own
    added or removed tone. The field says "Comment on these lines" for a run, and a kept
    comment names its run ("Lines 10–11") before the words.
  - **The code goes with it.** Each comment carries its lines quoted by
    `conversation::diff::quote`: "In `src/lib.rs` lines 10–11:" and the lines as a fenced
    diff, signs kept, numbered by the new file and by the old one for removals alone. Then come
    the person's words. Comments are parted by a blank line. Before, a comment went as `path
    L<n>: body`, so the agent had to reopen the file to know what was meant. A comment is
    anchored by its first line's text, so it goes if that line changes under it.
  - **Add to message.** Beside "Send N comments", "Add to message" puts the same text at the
    end of the agent's draft (`ReviewEvent::AddToMessage`, which the workspace hears). The
    agent's tile comes to the front with the keyboard in its composer, so the person adds
    words and sends once. Nothing is sent. Where no tile shows the thread, a notice says to
    open it. (Superseded by "A review's comments go only once taken", next.)
  - Tests: `review::tests::{a_drag_comments_on_a_run_and_add_to_message_sends_nothing,
    line_comments_go_as_one_message}`,
    `review::model::tests::comments_go_as_one_message_each_under_its_code`,
    `workspace::tests::review_tile::a_thread_s_review_opens_as_a_tile_of_its_own_and_goes_with_it`.

- ✅ **A review's comments go only once taken** (2026-10-05, the 10-06 re-audit,
  `.research/readiness-2026-10-06.md`). Amends "Comments on a line run, with their code".
  - The bug: both "Send N comments" and "Add to message" cleared the comments before anything
    took them. "Add to message" looked only for a terminal tile showing the thread, so for a
    Codex, pi or ACP thread, and for a Claude Code tile left on its TUI, it said to open the
    agent's tile and the person's comments were gone. A send the worker turned down lost them
    the same way.
  - Now the model hands out the message with what it holds (`Model::message`, a `Batch`) and
    forgets that batch only on an answer (`Model::forget`); comments written meanwhile stay.
    A send waits for its intent's outcome in the thread's outbox: accepted or settled, the
    comments go and the keyboard goes to the thread's tile; refused, they stay and the review
    says "Comments not sent" with the worker's reason. While either is away the foot reads
    "Sending…" and neither button sends twice.
  - "Add to message" goes to a composer of the thread whatever its agent and wherever it
    shows (`WorkspaceView::quote_to_thread`): a live terminal's tile is turned to its thread
    face, a thread's own tile is brought up, and a thread shown nowhere gets a tile opened on
    its worker. The text lands once that composer is made, and the review hears whether one
    took it (`ReviewView::added`). A thread on no connected machine is said at once; no
    composer within 5 s says "The agent's composer did not open". In both the comments stay.
  - The wait for a composer moved from counting four display frames (`on_next_frame`) to the
    workspace's own render: each frame, after the views are made, a waiting quote finds its
    composer (`settle_quotes`), and a timer gives it up. Four frames is about 33 ms, shorter
    than a tile opened on a remote worker takes to come back, and frame callbacks never run
    under the test platform, so no test could see the wait.
  - Tests: `review::tests::{line_comments_go_as_one_message,
    comments_whose_send_is_turned_down_stay}`,
    `review::model::tests::a_comment_stays_while_its_line_does_and_until_its_send_is_taken`,
    `workspace::tests::review_tile::{added_comments_reach_a_thread_tile_and_go_only_then`
    (Codex, pi and an ACP agent), `added_comments_turn_a_tui_tile_to_its_thread`,
    `added_comments_open_a_tile_for_a_thread_shown_nowhere`,
    `added_comments_for_a_machine_not_here_stay`, `added_comments_no_composer_takes_stay}`.

- ✅ **Snooze is the server's, with presets** (2026-10-04, `.research/t3code-ui-2026-10-03.md`
  §3.14). Amends "Snooze, honestly".
  - Before: a snooze lasted an hour, held in one client's memory. Another device still showed
    the finish, and a relaunch lost it.
  - Prior art: T3 Code's snooze keeps a wall time on the server. It offers presets from one
    shared list, which drops "This evening" once evening is under an hour off. A snoozed
    thread wakes early when something happens: a request for the person, a fresh failure, or
    a run that finished after the snooze.
  - The server holds every snooze (`slopty_proto::snooze`, `hub::snooze`).
    - `Verb::Snooze { of, until, zone }` sets one. `of` is a thread (`ThreadAt`) or, for a
      shell's finish or an agent known only by its tile, a terminal (`TermRef`).
    - `until` is a preset or a time the person picked: "In 1 hour", "This evening" (18:00),
      "Tomorrow" (09:00), or `At`. The presets are worked out in the zone the client sends,
      else the server's own, with jiff's built-in database, so a change of clocks lands on
      the hour the person means. "This evening" under an hour off is refused, pointing at
      tomorrow.
    - `Verb::Unsnooze` ends one now.
    - Both are refused from an agent. A thread or tile that needs the person now is refused
      too, since it stays where they see it until answered.
  - **Everywhere and across restarts.** The list goes to every client after where the person
    is (`FromServer::Snoozes`) and again on every change, each list replacing the last. It is
    kept in `snoozes.json` beside the workers' file, written whole on each change and at
    shutdown. A snooze that ended while the server was down is let go when it loads.
  - **It ends on its own.** The delivery loop wakes at the soonest end. A snooze ends early
    when its thread has news: any notice the ladder makes for it (needs you, failed, or
    finished again) ends the thread's snooze and its tile's. The server sees no shell
    command finish, so a client that sees a snoozed shell finish again sends `Unsnooze`.
  - The client's own hour-long snooze (`SNOOZE_FOR`) goes, with no shim. The inbox reads the
    server's list and offers the presets (lane D, `target/lanes/ui-queue.md`); until it
    does, the server's side stands alone.
  - Tests: `hub::snooze::tests` (presets across Berlin's change of clocks and in another
    zone, ending at its time and on news, the person's alone, refused while it needs them)
    and `a_snooze_outlives_a_restart` (server loopback); the goldens in `golden_snooze`.

- **The corner points at what needs you, and only that** (2026-10-04, ruled; built by lane D,
  the same study §3.14). Narrows "Off screen, the corner says it".
  - Before: with the app in front, a tile out of view that came to need the person, failed or
    finished put a notice in the corner with "Go". The inbox and the bell already list
    failures and finishes, and the person mostly directs and watches, so those notices were
    noise that pulled the eye from the work.
  - Prior art: T3 Code toasts only "Approval needed" for a thread other than the one in view,
    with "Open thread".
  - Now the corner speaks only when an agent comes to need the person, its tile is off
    screen, and the inbox is closed. With the inbox open, its row already says it. A failure
    or a finish goes to the inbox and the bell quietly, as their dot and count.
  - **A pointer, never an answer.** The notice names the agent and what it asks, with its
    status mark and "Go", which brings its tile into view with the keyboard. It never carries
    Allow, Deny or a choice. The answer is given where the whole request can be read: the
    tile, the inbox row, or the actionable system notification when the app is away.
  - A newer notice for the same tile replaces the older, and one whose agent was answered
    anywhere goes at once.
  - The workspace side is lane D's (`workspace/toast.rs`, `agents.rs`, triage), with the
    contract in `target/lanes/ui-queue.md`.

- ✅ **A terminal selection goes to an agent, not only a block** (2026-10-04, stage 4 item 10,
  T3 #5).
  - With text selected and an agent to take it (the attach probe), the terminal's menu offers
    "Attach selection to agent" after Copy. The palette has the same line
    (`terminal::AttachSelection`, no default key, as the palette rule has it) while the
    focused terminal has text selected, beside "Attach block to agent".
  - The text goes into the agent's draft as the block does, fenced with a fence longer than
    any run of backticks in it. It is named after the command whose block the selection starts
    in: "From the terminal, what `cargo test` printed:". A multi-line command goes in an `sh`
    fence, and a selection off every block, or on the alternate screen, says "From the
    terminal:". Nothing is sent. With no agent, or nothing selected, the action says so in a
    notice.
  - The terminal's event for both is `TerminalViewEvent::Attach`, renamed from `AttachBlock`
    now that a block is not the only thing attached.
  - Tests: `terminal::view::tests::{a_selection_is_attached_named_after_its_command,
    a_selection_s_fence_outruns_its_backticks}`,
    `workspace::tests::attach_block` (the event's new name).

- ✅ **The way down says how much is new, and says it once** (2026-10-04, stage 4 item 11,
  designeer #4).
  - Scrolled up while the thread moves on, the round way down becomes a pill: "3 new" in
    tabular figures before the chevron, capped at "99+ new". It counts the thread's items that
    came after the list stopped following its newest row, so a row that only grew (a streamed
    answer) adds nothing and the round chevron stays. Back at the newest row the count starts
    afresh. The count goes by the list's follow state, not by the way down's mark, because
    right after rows come the list has not laid them out and the mark reads as at the end.
  - Once a count has held for 700 ms (`tray::TELL_AFTER`), the button becomes a polite live
    region with the value "3 new below". A run of arrivals is said once, when it settles,
    never once per row. The label stays "Scroll to the newest", so nothing else is
    re-announced.
  - The live region is new in our gpui-fast fork: `gpui::LiveRegion::aria_live(Live)` sets
    AccessKit's `live` on the node (aislopware/gpui-fast#26, moved into `fast::live_region`
    behind one div.rs hook by #27, merged as caa325b; neither longbridge nor zed had an open
    PR for it). On macOS, AccessKit then posts
    `NSAccessibilityAnnouncementRequestedNotification` with the value at medium priority
    (high for assertive). `a11y::Node` carries `live`, so tests can see it.
  - Tests: `conversation::thread::tests::face::the_way_down_counts_what_came_and_says_it_once_it_settles`,
    `conversation::thread::view::tray::tests::a_count_of_new_rows_stops_at_99`, and in the
    fork `fast::tests::live_region::a_live_region_carries_its_politeness_and_value`.

- ✅ **The commit sheet, and the branch's pull request where the work is reviewed** (2026-10-04,
  P items 1 and 3; wire in `slopty_proto::git`, ruling in `docs/decisions/projects.md`).
  - **Where.** Where the thread works (the checkout and its branch in the composer's toolbar)
    is a button that opens the sheet, and so is the pull request's number beside it once the
    client has heard of one. The review tile's scope bar ends with the pull request, its
    number and its standing in the standing's tone, and "Commit…". The palette has "Commit…"
    and "Refresh pull request" (`conversation::palette_items`); no button carries a key.
  - **A sheet over the tile, not the window.** It is drawn on a scrim of its tile alone, so
    the other tiles stay in reach and a press round it closes it, as Esc and its close do. It
    is the dialog shell (`kit::dialog`, the list size), its head the title, the branch line
    ("feature → origin/feature · 2 ahead", "no upstream", "Detached HEAD"), refresh and close.
  - **The files.** Every changed file is ticked at first, one tick for all ("2 of 3 files"
    while some are not), git's letter in its tone (M, D, A, R, "new" for untracked, an
    unmerged file as git spells it), a rename as "from → to". The list scrolls past eight rows
    (a `uniform_list`, so a status at its 2000-file cap lays out what shows).
  - **The person's words only.** The message starts empty and nothing is suggested (amended
    2026-10-06: the thread's own agent can be asked to commit instead, "The agent commits on
    the person's ask"; Slopty still writes no message itself). Commit and
    Commit and push stay set back until a file is ticked and a word is written. With nothing to
    commit and the branch ahead of its upstream, or with none, the second button is Push.
    "Commit and push" sends the commit and the push only once the commit is made, never both
    at once. Every change is followed by a fresh status, so the list shows what is left; the
    message clears once its commit is made.
  - **The pull request over the files.** Its number (a link to its page), its title, and its
    standing as a pill: checks failing, conflicts with the base, changes requested, ready to
    merge, checks running, behind the base, waiting for review, blocked, draft, merged,
    closed (`git::standing_words`, read through `PullStatus::standing`, never matched on
    GitHub's spelling). Up to six checks follow, the most pressing first, each with its
    bucket's mark and opening its page; the rest are counted. With none, the sheet says so and
    offers "Open pull request", a page of title (empty means gh fills it from the commits),
    description, base and draft.
  - **A merge only on the person's press, only while ready.** A split button, "Squash and
    merge" with Merge and Rebase in its menu, and "Delete branch" ticked, send the head the
    person is looking at. Any other standing shows "Merge waits: …" in its place. Without gh,
    the merge and the pull request page say gh's absence in the worker's words.
  - **Refusals verbatim.** Slopty's own refusal, a missing program, and git's or gh's words
    (a hook's output, a rejected push) stand under the buttons in the code face, as said.
    What went is one quiet line: "Committed abc1234 · 1 file", "Pushed to origin/feature",
    "Opened" with the link.
  - **State in the hub.** `conversation::thread::git::GitBook`, one per worker, numbers each
    op, holds it until the worker answers (`ThreadHub::git_done`), and keeps each
    repository's status, pull request and last outcome, so a thread's tile and its review's
    show one answer. Out of reach, nothing is asked and the sheet says the machine is out of
    reach: an op held for later might no longer be what the person meant. The pull request is
    asked when the sheet or the review tile opens, on refresh, and comes with every push;
    nothing polls.
  - Tests: `conversation::thread::git::tests` (five), `conversation::thread::tests::commit`
    (three: commit and push in order, the merge's gate and its head, a refusal verbatim and
    closing), `review::tests::the_tile_shows_the_branch_s_pull_request_and_opens_the_commit_sheet`.

- ✅ **Carrying a thread on: continue in another agent, interrupt and send, edit from here**
  (2026-10-04; rulings R8 and R13 and "Edit from a turn" in `docs/decisions/agents.md`).
  - **Continue in….** Where the thread can (`Cap::CONTINUE`), the model chip is a button
    even with no model to switch to, and its menu ends with "Continue in a new thread": the
    thread's own agent first as "Start afresh", then every agent the worker can start
    (`ThreadHub::set_agents`, fed by the workspace from the link). Each row says what comes
    over: the account of this thread, as a draft. The models stay first, since switching the
    model is the smaller step.
  - **Started threads open.** An intent the worker answers with a new thread (a fork, a
    continue, an edit from a turn) raises `HubEvent::Started { from, thread }`; the workspace
    brings that thread up as a tile. Before, a fork's new thread was found only in the
    navigator.
  - **A draft is read and changed where it stands.** A message the worker holds as a draft
    stands whole in the tray, in a field of its own (up to twelve lines, then it scrolls), not
    pulled into the composer: an account can run to 32 KiB, and the composer is for the next
    message. Discard takes it back, Save keeps a change on the worker, Send gives the agent the
    words as they stand, the change first and the send after, in that order. A field the
    person has not touched follows the worker's words.
  - **Interrupt and send.** On an agent that takes no message mid-turn but can be stopped
    (`Cap::INTERRUPT` and `Cap::QUEUE` without `Cap::STEER`, every ACP agent today), a ghost
    "Interrupt and send" stands beside the Queue send while a turn runs and something is
    typed. The message waits in the tray under its own intent ("Stopping the turn to send"),
    as a queued one does, never as a bubble that looks sent.
  - **Edit from here** (superseded the same day by "Branch from here", below). Under a
    message of the person's, where the agent can (`Cap::REWIND`), an undo mark offered the edit; it opens a line under the bubble with
    T3 Code's two choices, Keep files (the solid) and Revert files too, and Cancel. While a
    turn runs it is set back and does nothing, since the worker refuses it then. A refusal
    reads in the tray in the worker's words.
  - Tests: `conversation::thread::tests::carry` (four).

- ✅ **A thought says how long it took, and says Thinking while it comes** (2026-10-04, Ely
  study #6, `.research/ely-gpui-components-2026-10-04.md`).
  - Before, a reasoning row read "Thought" and its first line whether it had ended or not.
  - Settled, it reads "Thought for 12 s", measured from the thought to the item after it (or
    its turn's end when it was the last), in the one way chrome says a duration
    (`kit::duration` in whole seconds); under a second "Thought for a moment"; with no times
    to measure, "Thought". The first line still follows, quiet.
  - While it is the thread's last item in a turn under way, the word is "Thinking", swept by
    gpui-kit's `ShimmerText`, which holds still under Reduce Motion. Only that word moves.
  - Tests: `conversation::thread::view::tests::a_thought_says_how_long_it_took`,
    `conversation::thread::tests::face::a_thought_says_thinking_then_how_long_it_took`.

- ✅ **A web search shows what it found** (2026-10-04, Ely study #5).
  - Before, the thread view drew a search as its query alone; the links the worker carries
    (`WebSearchDetail::links`) were dropped.
  - Its line now ends "3 sources · docs.rs · github.com": how many, and the first two sites,
    each once, quiet. Opened, the links are numbered rows of their title and site, each a link
    (a press, or ↵ once Tab is on it) to its page. Open state is the call's own entry in
    `items_open`, as every call's is. The worker carries no snippet, so none is drawn.
  - Tests: `conversation::thread::view::tools::tests::a_search_says_its_sources_and_their_sites`,
    `conversation::thread::tests::face::a_web_search_lists_its_sources`.

- ✅ **Motion keyed on a change, and figures that roll** (2026-10-04, Ely study #7; ported
  from Ely's `motion/changes.rs` and `typography/rolling.rs` with their notice, in
  `kit/change.rs`).
  - `kit::on_change(id, value)` counts how often a value changed since its element first
    drew: 0 on the first paint, one more per change. An animation keyed on the count plays
    once per change, is still when the element first appears, and never re-keys what is
    inside it. It is for every lane (the inbox and status-bar counts are lane U's).
  - `kit::Rolling` draws a figure whose digits roll to their new place, each along a column
    of 0–9 over `Pace::Settle`, in tabular figures, on a line of `FIGURE_LEADING` times its
    size snapped to whole device pixels. Signs, `%` and `+` stand. It rolls only when the
    figure keeps its words ("2 running" to "3 running"); new words ("1 finished") are drawn
    at once, as are the first paint and every paint under Reduce Motion.
  - Used for the way down's count ("3 new"), the background work in the composer and the
    tray ("2 running"), and the context share in the composer's meter.
  - Tests: `kit::change::tests::{figures_pair_by_place, only_the_digits_may_change}`, and the
    way down's face test, unchanged.

- ✅ **One way to branch, and continuing when a limit lifts** (2026-10-04, the cuts of the
  day's audit, `target/lanes/cuts-2026-10-04.md`). The person asked for fewer, plainer
  controls: three ways to carry a thread on became one, and the controls nobody reached for
  went.
  - **Branch from here.** Fork (on a turn's fold), edit from here (an undo mark on a
    message) and "Continue in a new thread" (the model chip's menu) are one branch mark under
    each message of the person's. It opens a small panel under the bubble with three rows of
    choices and Branch: the agent (the thread's own first, then each one the worker can
    start), where from (this message, or the end) and the files (keep them, or put them
    back). Each choice goes through the agent's own door: another agent is
    `Intent::Continue` with the account of the whole thread, so "where from" is not offered;
    this message on its own agent is `Intent::Rewind` (`Cap::REWIND`) with the files as
    chosen, or `Intent::Fork` after the turn before it where only `Cap::FORK` holds; the end
    is a fork of the whole thread, or a fresh thread of the same agent. While a turn runs
    Branch asks nothing, since the worker refuses it then. The thread it starts opens as a
    tile (`HubEvent::Started`). The model chip is a button only where models can switch.
  - **Send later is gone.** The clock beside the send button, its menu of times and other
    threads, and "Send later…" in the palette went. The one moment worth waiting for is a
    usage limit's reset, which the agent names (`TurnState::Failed::until_ms`): while a limit
    holds a thread whose agent takes a message kept for later (`Cap::SCHEDULE`), a line over
    the field says "The usage limit lifts at 14:35", and "Continue at 14:35" sends the draft
    (or "Continue", with nothing typed) to go then. Once that message waits, the tray says
    when it goes and the line goes.
  - **Edit… on an approval is gone.** Changing an edit's text before allowing it was rarely
    used and doubled the tray's states. Deny… with a reason stays. The wire's
    `Request::editable` is lane A's to remove.
  - **Settings.** `[web] inspector` went: every page is open to Web Inspector, as a
    developer's browser is, since a worker's dev page is a browser tile's main use.
    `[remote] fps` went: a stream asks for the refresh of the screen it is drawn on, up to
    120, which is what the default did. `[terminal] bell_alert` and `agent_alert` are one
    `[terminal] alert` ("never", "hidden" by default, "always") for a terminal's bell and an
    agent that needs the person alike. The Mac's "Open in editor" in the settings dialog went:
    the dialog's own file face is the editor.
  - Tests: `conversation::thread::tests::carry` (four, on the panel),
    `conversation::thread::view::branch::tests::a_branch_asks_the_agent_s_own_door`,
    `conversation::thread::tests::composing::a_thread_a_limit_stopped_continues_when_it_lifts`,
    `conversation::thread::view::later::tests::{a_held_message_says_when_it_goes,
    a_limit_lifts_when_its_turn_says}`, `slopty_app::settings::tests::an_alert_sounds_only_in_the_background_unless_asked`,
    `screen::tests::{the_rate_follows_the_screen_up_to_the_ceiling,
    a_view_on_a_screen_of_another_rate_asks_for_it}`, and the platform's
    `every_page_is_open_to_web_inspector`.

- ✅ **The context ring and bar glide to a new share** (2026-10-04, Ely study #8).
  - `kit::Gliding` draws a value through a closure at each step of a glide over
    `Pace::Settle`: a change while it glides sets out from where it is drawn, so it turns
    back rather than jumping to its last end first (Ely's restarted from its last end). The
    first paint and every paint under Reduce Motion draw the value as it is.
  - The context ring (`parts::context_ring`) is a true arc drawn with `PathBuilder::arc_to`,
    its ends rounded as filled discs since GPUI exports no line cap, and the meter's bar
    fills through `Gliding::fill`.
  - `kit::Spark` draws a short history as one line over its own low and high, the newest
    value at the right edge with a dot, sliding one step left over `Pace::Settle` as each
    value comes (keyed on how many came, so a repaint between them never moves it; still
    under Reduce Motion). A value that is no number is a gap. It is said as the words for its
    newest value. From Ely's `spark.rs` and `charts/realtime.rs`, with their notice.
  - The stream overlay's "Details" opens on three of them over the engineering lines: the
    last half minute (thirty samples, one a second) of the frame's age, the interarrival
    jitter and the round trip, each with its latest figure ("Round trip 7 ms"). A stalling
    link shows as a rise before it shows as a stall.
  - Tests: `kit::spark::tests` (two),
    `screen::tests::the_details_show_the_last_half_minute_of_age_jitter_and_round_trip`.

- ✅ **Find in a thread** (2026-10-04, on lane A's `ThreadRequest::Search`; the old face's
  find goes with it).
  - ⌘F in a thread opens a find bar at the list's top right: the field, "2 of 9", the older
    and newer match, and Close. The matches run newest first, as a thread reads from its end,
    so ↵ steps back through them and ⇧↵ forward, round, as the terminal's find does. Esc in
    the bar closes it and gives the composer the keyboard.
  - It reads what the worker's search reads: the person's messages, the agent's answers and
    its reasoning, its calls' titles and its notices, every word of the query in any order
    and case. The items the client holds are matched at once as the person types.
  - Where the thread has older turns than the client holds, the worker is asked
    (`ThreadHub::search`, once the words rest for 120 ms, from two characters), and its hits
    in those turns come after the held ones. Going to one pages the thread back, a page at a
    time and never twice for the same page, until its turn is held; the count says "+" when
    the worker left matches out. An answer for words no longer asked is dropped.
  - The match on show is washed in the selection's hue at its faint step and scrolled to;
    one in a folded turn, a closed group or a closed step opens what hides it.
  - The workspace routes `WorkerMsg::ThreadHits` to the worker's hub (lane U's hunk), which
    the palette's "Threads" section can read too.
  - Tests: `conversation::thread::find::tests` (three),
    `conversation::thread::tests::find::{a_match_opens_the_fold_over_it_and_return_walks_back,
    a_match_in_an_older_turn_pages_back_to_it}`.

- ✅ **A steer splits its turn's fold** (2026-10-04, audit A5).
  - Before, a message the person sent into a running turn stood after the one fold over the
    whole turn's work, so the fold summed work from both sides of it and opening the turn
    put the steer in the middle of the work it had folded.
  - Now each steer stands where it was sent between two folds: one over the work before it,
    one over the work after. An earlier fold says only what its stretch did ("Ran a
    command"); the last carries the turn's own figures as before (its time, its changes, its
    model and cost). The turn opens as one, from any of its folds, since its stretches are
    one piece of work. `Row::Fold` carries its `part`, so each fold keeps its own row.
  - Tests: `conversation::thread::rows::tests::a_steer_splits_the_fold_where_it_was_sent`,
    `conversation::thread::tests::steps::a_steer_stands_between_the_folds_of_its_turn`.

- ✅ **Opposite states stay apart for colour-blind eyes** (2026-10-04, Ely study's next
  batch; `crates/slopty-theme/src/vision.rs`).
  - A test draws the three pairs the chrome sets side by side to mean opposite things (lines
    added against removed, a live mark against a failure, waiting against failed) through
    Machado, Oliveira and Fernandes's simulation of protanopia, deuteranopia and tritanopia
    at full severity (2009, linear sRGB), in both appearances, and holds each pair 0.06
    apart in Oklab, about three just-noticeable steps.
  - It found two pairs a deuteranope could not tell apart. In dark, the green and One Dark's
    red (`f27d84`) sat 0.020 apart, one step, so a diff's added and removed lines read alike.
    In light, the amber (`8b5d00`) and the red (`c7212c`) sat 0.029 apart, one brown. Both
    reds now differ from their neighbours in lightness, the one difference every dichromacy
    keeps: dark's error is a light coral, `ff9095` (near Radix's dark red 11), and light's
    a deep red, `8f1d1d` (near Tailwind's red 800). Both read past their floors (APCA Lc 45
    in dark, AAA on white), and the green and the amber are unchanged. Goldens that draw an
    error or a removed line move with this.
  - Increase Contrast is not held to it: its lift carries every tone toward the pole, where
    they meet. The signs and marks beside them (a diff's `+` and `−`, the check and the
    cross) carry the difference there.
  - Tests: `vision::{opposite_states_stay_apart_for_every_dichromacy,
    the_simulation_keeps_greys_and_merges_red_with_green}`.

- ✅ **A disclosure chevron turns** (2026-10-04, Ely study's next batch, from Ely's
  `primitives/disclosure.rs` with its notice).
  - `kit::Disclosure` is one chevron that points right when closed and down when open, turned
    a quarter over `Pace::Fade` as the state changes (keyed by `kit::on_change`), where rows
    swapped two glyphs and the eye lost which row moved. It is still on the first paint and
    under Reduce Motion.
  - A thread's folds, stretches, groups and notes that open use it. The navigator's section
    headers (lane U) can take it the same way.

- ✅ **The effort chip switches how hard the model thinks** (2026-10-04, lane A's
  `Intent::SetEffort`, `ThreadMeta::efforts` and `Cap::SET_EFFORT`; ruling in agents.md,
  "Model, effort and mode are switched through each agent's own settings door").
  - Each of the composer's three chips is a switch only where the agent has the door and a
    list to switch among: the model (`SET_MODEL` and `models`), the mode (`SET_MODE` and
    `modes`), the effort (`SET_EFFORT` and `efforts`). Without them a chip is a quiet meter.
  - The effort chip reads the level the agent says it is at, by the label of the level it
    published when one matches its id or its label (Codex and pi say the id, an ACP agent
    the label). Its menu lists the levels with their descriptions and checks the current
    one, as the modes' does. An agent with the door but no levels for its model (a pi model
    that does not reason) shows no chip.
  - "Next effort level" is in the palette, with no default key (it can be bound as
    `cycle_effort`); it steps to the next level, round. A refusal reads in the tray in the
    agent's words, as every refused intent does.
  - Test: `conversation::thread::tests::face::the_effort_chip_switches_how_hard_the_model_thinks`.

- ✅ **Labels fade at their tail, and reading has a size of its own** (2026-10-04, items 12
  and 13 of the thread-view plan above).
  - `kit::fit_label(id, text, theme)` lays a label out whole, keeps it from scrolling, and
    fades its right edge through GPUI's per-pixel `edge_fade` only as far as text lies past
    it (`hidden_by_scroll` on a scroll handle that tracks an overflow-hidden row, so no wheel
    moves it). A label that fits is drawn sharp to its last letter; one that is cut keeps the
    start of every word, offers its whole text in a hint, and is heard whole. The composer's
    model name, where the thread works (the checkout and the branch) and a queued message's
    line use it; tile titles keep their ruled ellipsis. The navigator's rows are lane U's to
    move onto it.
  - `[font] prose_size` ("Reading size", 15 pt by default, 10 to 32) sets what is read at
    length: an agent's answers, the person's messages, a plan's body and the composer's
    field. The chrome keeps `ui_size`. Headings sit 3 and 1 points over it, as 18 and 16 did
    over 15. It is under Settings, Appearance, Interface, after the text size.
  - Tests: `kit::fit::tests::only_a_label_past_its_room_runs_past_its_edge` (on layout: the
    test platform paints no glyphs, so the fade itself is GPUI's to prove),
    `slopty_app::settings::tests::the_reading_size_rides_on_the_theme`,
    `conversation::thread::tests::face::the_reading_size_grows_what_is_read_and_not_the_chrome`,
    and the settings crate's `[font]` keys.

- ✅ **An aside is a sheet over its thread, not a tile** (2026-10-04, A9 on lane A's contract).
  - "Ask aside" asks a question beside the work without touching it. It is a choice of
    "Branch from here" (As: a thread, or an aside; the agent, the start and the files then
    drop, since an aside forks the whole thread as its own agent) and a palette line, with no
    default key (`ask_aside`). It was an icon in the composer's foot too until 2026-10-05:
    a second door to the same thing, crowding the composer. It is offered where the agent
    forks (`Cap::FORK`), and not on a thread that is itself an aside.
  - It forks the whole thread (`Intent::Aside`), and the draft goes to the fork as its
    question once the fork is there. With no draft, the sheet opens on the fork's own
    composer.
  - The sheet takes the lower half of the thread above its foot. It holds the fork's own
    thread view without a header, so answers, steps and requests read and are answered as
    anywhere else, and follow-ups go from its composer. It has two doors:
    - Close ends the fork for good (`Intent::Discard`; the agent's own session file stays,
      since Slopty never deletes one). Closing before the fork came ends it as it comes.
    - "Keep as a thread" drops the mark (`Intent::KeepAside`). The workspace then opens it
      as it opens a fork, on the worker's word (`HubEvent::Started` with `aside: false`).
  - Esc does not close the sheet: ending a fork is for good, so it takes a press.
  - The lists of threads pass over an aside by its row's fact (`hub::is_aside`). That is
    lane U's navigator and attention, and the server's ladder and notes.
  - Tests: `conversation::thread::tests::aside::*`.

- ✅ **A Codex goal is one line over the field, read-only** (2026-10-04, A11).
  - While the agent holds a goal (`ThreadState.goal`), one quiet line stands over the
    composer's field. It shows the objective (fading at its tail), where the goal stands when
    that is not plain work ("Paused", "Blocked", "Budget limited", "Done"), and the tokens
    spent, against the budget where there is one.
  - A budget draws a thin bar under the line, gliding, in the context meter's tones: warn
    past 80 %, error past 95 %. The hint says how long it worked toward it.
  - While the goal is active, the Stop button's hint adds that Codex may go on by itself
    toward its goal: a stop ends a turn, not the goal.
  - There are no controls: the goal is set and paused in Codex's own TUI.
  - The line sits by the field rather than in the header, which a tile may hide.
  - Tests: `conversation::thread::tests::face::a_goal_is_one_quiet_line_with_its_budget`,
    `view::goal::tests`.

- ✅ **A stop pauses the queue, said once** (2026-10-04, A1). Messages the person's stop holds
  (`Pending::stopped`) sit under one quiet line, "Queue paused · sends after your next
  message", rather than each saying it. Each held message can be sent now, which lets the
  rest go after it. Test: `conversation::thread::tests::composing::a_stop_pauses_the_queue_until_the_next_message`.

- ✅ **A refused rewind offers going back without the files** (2026-10-04, A10). When the
  worker turns down `Rewind { files: true }` (another thread works in the same folder), its
  words show verbatim in the tray. Beside them, "Without the files" goes back to the same
  turn with the files left as they are. Test: the end of
  `conversation::thread::tests::carry::branching_from_a_message_keeps_or_puts_back_the_files`.

- ✅ **One face for every agent: the old Claude Code face is gone** (2026-10-04, item 7 of the
  thread-view plan, after the cuts ruling). It ruled out the conversation face first built for
  Claude Code, which is now superseded: `conversation/view/**`, its model, rows, tools, find,
  composer, approval, question and fixtures. Every agent, Claude Code included, is drawn by the
  thread view over the agent-neutral thread model. A Claude Code terminal's face is its
  thread's view, as the worker's table names it.
  - What both faces used lives on with the thread:
    - `conversation/attach.rs` holds the pasted picture or dropped file as a chip. Its
      `Target` is now the thread view alone.
    - `menu.rs` holds the `/` and `@` menus, over the thread's commands only.
    - `figures.rs` keeps a model's spoken name and the times of day.
    - `diff.rs`, `lines.rs` and `chips.rs` are unchanged.
  - A permission prompt is a request on its thread. The worker holds a yes or no for the
    clients that keep its table (lane A's half). The workspace answers it from the
    navigator's *Needs you* row and the note's buttons with `Intent::Answer`, once.
    - A terminal's note and row answer the request of the thread its agent runs. The thread's
      row speaks for the request even where the terminal's hook status speaks for the agent.
    - A note tapped before its request is here waits until the worker's table has had time to
      come after the link came up, then says it no longer waits.
    - The person at the agent's terminal, its TUI shown and the app in front, gets the agent's
      own dialog at once: the workspace hands the request back with `Intent::Release`.
  - A terminal shows its thread only once the worker's table names one, and then by default
    on every device; ⌘J picks the TUI, and the pick is saved with the layout. With no thread
    yet there is only the TUI, so a phone no longer opens on a face of its own.
  - A thread view is made only while it shows. What its composer held is kept per session
    when it hides and put back when it shows again, so ⌘J never loses a draft.
  - A face no longer outlives a dropped link on its tile. The tile is set back like any other
    with its worker away, and the thread's own tile keeps what it showed.
  - The marks and names the old face gave the chrome come from the thread's table row:
    - a tile reads as working while its thread's row says a turn runs, though the hook lags;
    - an untitled agent is named by its thread's title;
    - the navigator, the overview and the palette say the row's last line.
  - The kit's floating-surface lint now names the thread's composer, find bar and aside
    sheet.

- ✅ **One warm neutral, white floats and raised surfaces** (2026-10-04, light pass phase 1,
  from `.research/light-premium-2026-10-04.md` §3 and §5). Light mode read as grey slabs: a
  field, a tray head or an option rested on the hover wash, and a card wore three edges.
  - Every neutral in both modes sits on one warm hue, OKLCH 85 (`NEUTRAL_HUE`), at a chroma
    under 0.006. The text is `#1c1c1a` in light and `#ecebea` in dark. Light content is
    `#fdfcfb`, and what floats or is raised is pure white, so it rises by tone as well as by
    its ring.
  - `kit::raised` is what rests on a plane (a card, a secondary button, a toast, a block of
    facts): white with the quieter hairline and a contact in light, the hover wash with the
    lit top edge in dark. `kit::inset` is what sits in a plane (a code block, a chip, a
    legend): the band. `kit::field` is a field: the raised fill, sunk. The 31 resting washes
    moved onto these.
  - A wash is a state, never a surface: the lint-as-test `a_resting_fill_is_raised_or_sunk`
    allows the hover, pressed and selected fills only in a hover, an active or a selection.
  - The light card drops its bottom rim and its ring is `border_subtle`: one edge and its
    contact, not three.
  - Error is a crimson (OKLCH 0.41 0.165 25, `#8f0214`). The colour-blind test set its
    lightness: the first pick sat too close to the agent's orange under protanopia. Green
    text is more saturated (OKLCH 0.49 0.135), and the light accent fill keeps 3:1 on the
    new content.
  - Pruned: the board's lane is a heading over a column, not a well (the cards are white on
    the content now). A *Needs you* heading and "… in a subtask" are the muted ink, since the
    mark beside them already says it. The overview's frame was already the neutral double
    ring.
  - Tests: one hue per meaning across both modes, one neutral hue, nearer is lighter in both
    modes, the dividing hairline is seen equally, and the agent's colour stays apart from
    waiting under every vision. The kit's card, button and secondary tests follow the rims.

- ✅ **Light shadows are the ink, in three layers** (2026-10-04, light pass phase 2, from
  `.research/light-premium-2026-10-04.md` §3.4). Black shadows on warm paper read as grey
  smudges. The light elevation's shade is now the warm ink, `oklch(0.24 0.012 85)` =
  `#221f19`, so a shadow is a deeper paper (Radix tints its light greys the same way). Dark
  keeps black.
  - What floats: `0 1 1 0` at 5 %, `0 4 8 −4` at 6 % and `0 16 32 −8` at 11 %, which is
    Geist's menu and modal shape a notch firmer for the half-point ring. Dark keeps its two
    layers. `Elevation::shadow` holds three slots, and `Shadow::NONE` fills a slot a mode
    does not use, which the kit skips.
  - What rests: `0 1 1 0` at 4 % and `0 1 2 0` at 3 % (Primer's resting small).
  - The scrim is the ink at 0.20 (`alpha::SCRIM_ON_PAPER`), not black at 0.16, so the work
    behind a sheet keeps its material. The solid's contact and the sunk shade take the ink
    by the same token.
  - The old bound "no halo to the sides" (6 pt) becomes a ratio: a soft layer reaches at most
    8 pt aside and a third of its reach below.
  - Tests: `elevation_and_density` (R8: black in dark, the ink in light, layers tightest
    first) and the kit's `the_elevation_is_layers_of_the_shade_and_a_lit_edge_in_dark` (every
    drop layer and the scrim are the shade).

- ✅ **The light terminal palette is generated from the dark one's hues** (2026-10-04, light
  pass phase 3, §3.5 and §3.6). GitHub light's palette had other hues than One Dark's, uneven
  normals (Lc 72 to 85), and brights paler than their normals. Each light ANSI colour now
  keeps its dark slot's hue, with green moved to the brand's 148°. Normals sit at OKLCH
  L 0.525 and brights at 0.465, so a bright is stronger on paper as it is on black. Chroma is
  as high as sRGB allows, up to a cap per hue. The greys are the one neutral. Dark stays One
  Dark, uneven normals and all, since the person chose it.
  - Code comments take ANSI 8, the shell's dim slot, in both modes instead of the chrome's
    muted grey. Muted is held near body strength by the chrome's AA rule, while ANSI 8 clears
    AA on the code's ground and still recedes.
  - Tests (R7): `light_ansi_is_generated_from_the_dark_hues` regenerates every slot from its
    OKLCH; `ansi_slots_keep_their_hue_across_modes` (within 10°, green 15°, neutral greys);
    `ansi_brights_are_at_least_as_strong_as_their_normals` (both modes, light normals within
    8 Lc); `a_comment_is_the_dim_slot_and_still_reads`.

- ✅ **One find bar, the kit's** (2026-10-05). The terminal, the file tile, the thread and the
  page each drew a find bar of their own. They had drifted apart: three widths, two arrow glyph
  sets, "Bad regex" beside "Not a valid pattern", "2/9" beside "2 of 9", and a regex toggle only
  the terminal had. After Ely GPUI Components' `FindWidget` (Ely study §5 #9), `kit::FindBar` is
  the one bar. It holds the field, the case, word and pattern toggles (shown only where the owner
  handles them), the tally in one wording (`kit::find::Tally`: "2 of 9", "No matches", "Not a
  valid pattern" with the pattern's own word as its value, "Finding…"), the steps and the close,
  plus the replace row where the owner has one. Each owner places the bar and binds its keys round
  it. Its parts go by `{bar}-count`, `-previous`, `-next`, `-close`, `-case`, `-word`, `-regex`,
  `-replace` and `-replace-all`, under `terminal-find`, `file-find`, `thread-find` and
  `page-find`. The terminal gains case and whole word: the query reaches the worker as one
  pattern with its rule written in, so the worker's own smart case never overrules a toggle. The
  page's bar moved from a strip above the page to the top-right corner over it, as the others sit;
  GPUI draws it after the page, so it shows over the native view. The query itself
  (`kit::find::Query`) is the file tile's, moved to the kit so every bar shares it.

- ✅ **One menu engine, the kit's** (2026-10-05). The bar's menus, a terminal block's menu and the
  commit sheet's merge-method menu each drew their own panel and ran their own keys: only the
  bar's walked its rows with the arrows, only the block menu closed on Esc by the terminal's own
  key handler, and the merge menu took no keyboard at all. After Ely GPUI Components' menu and
  select (Ely study §5 #2), `kit::Menu` holds the rows (plain, check or radio, an icon, muted keys,
  disabled) with hairlines between groups, and `kit::MenuPanel` draws and runs them. It takes the
  keyboard when it opens. Opened from the keyboard its first row is marked; from the pointer, none.
  ↑ and ↓ go round past a disabled row, Home and End go to the ends, and letters typed within
  800 ms go to the row they start, as Finder does. ↩ and Space choose on their release, armed by
  their own press, so the key that opened a menu never chooses in it. Esc and a press outside
  dismiss it. Choosing closes the menu first and then runs the row, so the row runs with the
  keyboard back with its owner. The marked row is the panel's active descendant, so a screen
  reader follows the mark. Rows go by `{menu}-{key}` under `menu`, `block-menu` and
  `commit-method`. There are no submenus, since no menu has one. The composer's model, mode,
  effort, `/` and `@` lists stay as they are: they are pickers bound to the field, which keeps
  the keyboard while they filter, so a panel that takes the keyboard is the wrong shape for them.
  The status bar's popovers show facts and hold no rows to choose, so they are not menus.
- ✅ **A Markdown file opens on its preview** (2026-10-05). File tiles showed Markdown only as
  source, while notes drew it rendered (readiness gap #16). A `.md`, `.markdown`, `.mdown` or
  `.mkd` tile now opens on its preview once its text is in, drawn as the note was drawn: prose
  through `crate::markdown::style`, task lines as boxes that tick, and fenced blocks with Copy, and
  Run while there is a shell. The rows are segments in a gpui `list`, so a long file lays out
  only what is in view. ⌘⇧V (Zed's and VS Code's key), the header's toggle ("Show source" /
  "Show preview", sitting where an agent's face toggle does) and the palette swap the preview
  and the source. The preview draws the editor's text, so an edit not yet saved shows in it. A
  tile opened to be edited opens on the source: a file not on disk yet, an empty one, one opened
  at a line, one a program waits on, or one holding an edit kept from before a quit. ⌘F swaps to
  the source, where the hits are marked. A tick in the preview changes its line in the editor.
  On a file with nothing else unsaved it saves at once, since the tick is the whole edit. Under
  an edit it waits for ⌘S with the rest. The header reads which face shows from the workspace's
  file facts and never from the view. Reading the view would rebuild the strip at each caret
  blink, which `a_file_tiles_caret_blinks_without_building_the_strip` caught. This is the base
  the notes merge into.
- ✅ **A note is a Markdown file** (2026-10-05). After the feature audit (merge notes into file
  tiles), `ItemKind::Note`, `ItemOp::SetNote`, `slopty-ui::note` and its last-writer-wins sync
  are gone, with "Keep last block as a note" and the block menu's "Save as note". A note is now
  a Markdown file on the worker in a file tile, read in its preview ("A Markdown file opens on
  its preview"): one editor and one sync model, and an agent reads and writes a note as the file
  it is. ⌘⇧N, "+"'s "New note" and the palette open a new file in the focused shell's directory
  on the target worker, else the worker's home, named for the moment on this device's clock
  (`note-2026-10-05-143210.md`; to the second, so two notes never share a file). It opens on its
  source with the keyboard, and nothing is on disk until ⌘S, as with any new file. The time is
  read through `CFTimeZone`'s offset and `slopty_core::WallMs::civil`. The project's directory
  was weighed against a notes folder in the worker's data directory. Beside the work, an agent
  and the person find the note where they are working, and a note that is not wanted is one
  delete in a folder tile. A checklist's progress no longer shows in the header and the
  navigator, since that was read from the note's text in the registry. A file's text is not in
  the registry, and working it out for every Markdown tile on each change was not worth a
  readout. No migration: a worker whose `items.json` still holds a note drops that one item
  with a warning on its next start and keeps every other ("A worker starts over any item store"
  in `workers.md`).
- ✅ **A row's hidden actions show for their own focus** (2026-10-05). A machine row's "+" and
  "…" in the navigator, and a transfer's Cancel in the status bar, stay hidden until the
  pointer is over their row, their menu is up, or they have the focus. They were revealed with
  gpui's `in_focus`, which holds inside any focused ancestor too, so while the workspace itself
  held the keyboard (at launch, before a tile takes it) every row brought out its actions. They
  now use `focus`, the element's own focus, which also covers a screen reader moving to one.
  The self-test's stale-frame check found it in about one full app run in two: the "+" drawn in
  its first frame clear, and the same state drawn from scratch with it shown. The cause was
  in gpui-fast, which answered "inside the focused element" from the last frame drawn. An
  element drawn for the first time was therefore never inside it in that frame, and since the
  focus had not moved, no later frame came to correct it. gpui-fast now answers from the frame
  being painted, whose dispatch tree is whole once prepaint is done (branch
  `fix/focus-contains-this-frame`, test
  `an_element_drawn_first_inside_the_focused_one_is_within_it_at_once`). The stale-frame
  report now says where the pointer was, whether the keyboard was the last input, and what
  held the focus, which is what told this apart from a hover.

- ✅ **Every thread button is an action too, answered only while it shows** (2026-10-05,
  readiness 10-05 G5). Review, Take back from the terminal, Compact context, Branch from
  here… and Resume the agent were buttons only, so the keyboard could not
  reach them and the palette did not list them. Each is now an action in the `conversation`
  namespace with a palette line and an unbound keymap entry (keys are the person's to give,
  and the palette shows one once given). The thread view listens for an action only while its
  button would show (`thread/view/keyed.rs`): Review over a last turn that changed files, Take
  back while the agent's TUI holds the session and no
  take-back is on its way, Compact where the agent compacts through Slopty, Branch from here
  under the person's last message (scrolled into view), Resume once the agent has exited and
  can be taken up. So the palette, which keeps a line only where the focus answers it ("The
  palette offers what the focus can do"), never offers one that does nothing. The thread's
  palette lines (`conversation::palette_items`) had been written and never added to the
  workspace's, so Commit…, Refresh pull request, Review with the agent, Watch the agent's
  screen, Ask aside and Next effort level reach the palette with them now.
  - *Deleted: "Show the agent's terminal"* (readiness 10-06 item 8). Once an agent's tile
    turned between its thread and its TUI, the line did what ⌘J ("Show thread or
    terminal") does, under a second name and a second action. The tile's header toggle and ⌘J
    are the one way to the TUI. A thread tile whose agent has a live terminal already becomes
    that terminal's tile, so it needs no way of its own. The thread still brings the terminal
    into view by itself where its answer goes there: "Answer in the terminal", and Codex's TUI
    once it joins.
  - *One noun: thread* (readiness 10-06 item 9). The chrome called the same object a thread
    in some places and a conversation in others: the tile's toggle and its accessible
    description, three palette lines, and the Keyboard settings group. Every one now says
    thread ("Show thread", "Show thread or terminal", "Thread density", "Find in terminal,
    file or thread", and the "Threads" group, plural like Files and Folders). Words that name
    the agent's own context ("Compacted the conversation", a slash command's description) keep
    the agent's word. The keymap's `conversation.*` names and the `Conversation` key context
    are config, not chrome, and stay.
  - *The keyboard stays on the thread when the field goes.* The field leaves while the
    agent's own TUI holds the session and once an agent has exited for good, the two states
    whose buttons matter most here. A focus on what is no longer drawn reaches nothing, so the
    thread's actions went unanswered from the keyboard exactly then. The thread takes the
    keyboard as the field goes and hands it back when the field returns.
  - *"Mute sound" says "Unmute sound"* while the focused stream is muted, so the line says
    what picking it does.
  - Tests: `conversation::thread::tests::doors::every_button_of_the_thread_is_an_action_while_it_shows`,
    `workspace::tests::facts::a_workers_tiles_share_its_one_sound_and_say_its_mute_together`.

- ✅ **One word for each state, and nothing drawn before it is known** (2026-10-05, readiness
  10-05 item 16). Four small places said a thing two ways or showed a blank as a value.
  - *A page's header goes forward too.* The page's history going forward was read back from
    the web view and offered only in the palette, so after a back the header had no way to
    return. "→" now sits beside "←" and shows, as it does, only while there is a page that way.
  - *A task row says its lane's word.* The board's lane is "Up next" and a planned task's row
    said "Planned". A row now says its lane's heading, and only a waiting task says more
    ("Waiting", inside "Working"), as the status mark does elsewhere.
  - *The stream's plain line leaves out what it does not know yet.* Before the first frame is
    presented it said "– to glass", and before a round trip was measured "RTT –". Both
    figures are now left out until there is a number, which is what the readiness 10-01
    ruling asked. The details readout keeps its dashes, since it is a fixed table where a
    blank cell has to hold its place.
  - *An empty review names its span.* "Nothing changed" read like a pane that had not loaded.
    Now each span says what it found: "The last turn changed no files", "Nothing new since
    you last reviewed", "No turn has changed a file yet".
  - Tests: `workspace::tests::page_chrome::back_and_forward_show_only_with_history_that_way`,
    `project::tests::a_row_names_a_state_as_its_lane_does`,
    `review::tests::an_empty_review_names_its_span`,
    `screen::health::tests::the_plain_line_leads_with_the_human_numbers_and_flags_trouble`
    (updated).

- ✅ **An agent with a terminal lives in that terminal's tile** (2026-10-05, readiness 10-05 §3).
  A thread whose agent runs in a live terminal is shown in one tile, that terminal's, with two
  faces: the thread and the agent's own TUI. Today that is Claude Code and pi's TUI, and Codex
  where its TUI runs beside the app-server. `ItemKind::Thread` stays only for a thread with no
  live terminal: an ACP or pi RPC agent, Codex with no TUI open, or an agent whose program has
  exited. The rule follows the table's `ThreadRow.terminal` for the terminal's own thread (never
  a subagent's row that shares it), not the agent's kind, so it holds for any agent the moment
  it runs in a terminal. Before this, one agent could be in two kinds of tile. Drops,
  attachments and keys behaved differently on each, and "Show the terminal" opened a second
  tile beside the thread's.
  - A thread tile whose thread gains a live terminal becomes that terminal's tile in its place
    and under its id. Its item goes and comes straight back as the terminal's, so the layout
    keeps it where it stood, and its name and facts go with it. Its `ThreadView` is handed to
    the terminal's face, so the draft, the scroll and the keyboard go on. Where the terminal has
    a tile already, the thread tile goes and that tile takes the focus. A start whose agent is
    already in a live terminal lands straight in the terminal's tile, the start's own.
  - An agent that exits leaves its terminal's tile showing the last screen, its thread's
    Resume on the face. Taken up again, the agent runs in a new terminal, and that tile goes
    on as the new terminal's in its place, under its id, with its face pick, draft and view.
    The exited session is closed on its worker, since its screen has given way. A thread tile
    is not made for it.
  - One rule (`WorkspaceView::settle_thread_tiles`, run when a table arrives and when a session
    is listed) covers a start landing, a thread tile gaining a terminal, a resume, and a layout
    saved before this. No wire changes. The worker now tells every client of a terminal it
    opens for an agent, with the same `SessionChanged` summary an `OpenSession` sends, so a
    client knows the agent's terminal is live as soon as the table names it; before, a client
    heard of it only once it moved or exited.
  - The face is the tile's own state, saved with the layout by tile. A new start opens on the
    thread face; a tile the person left on the TUI reopens on it. Opening a thread from the
    navigator, a note, a notice or a line's author goes to its terminal's tile (or adds one),
    and never flips the face the person picked.
  - What arrives on the tile follows its face. Dropped files, pasted pictures and attachments go
    to the composer on the thread face, and to the shell on the TUI face, as on any terminal.
    The tile's accessible description says which face shows ("Thread" or "Terminal"), and
    the header's toggle is named for what it turns to ("Show terminal", "Show thread").
  - Deleted: the thread tile's own way to its terminal (`open_thread_terminal`), which opened a
    second tile. A thread that brings its terminal into view flips its tile to that terminal
    once one is live, and otherwise says the terminal has ended.
  - Tests: `workspace::tests::agent_tile::{a_tui_agents_start_lands_in_its_terminals_tile_on_the_thread_face,
    a_thread_tile_whose_agent_gains_a_terminal_becomes_its_tile_in_place,
    a_thread_whose_terminal_already_has_a_tile_goes_to_it,
    a_thread_with_no_live_terminal_keeps_its_own_tile,
    opening_an_agents_thread_opens_its_terminals_tile,
    an_exited_agents_tile_goes_on_where_its_thread_is_taken_up_again}`,
    `workspace::tests::remote::drops_on_an_agents_tile_follow_its_face`, the workerd
    `a_claude_code_start_with_no_prompt_opens_in_the_home_it_names` (extended), and the app
    self-test
    `a_palette_start_opens_the_agents_terminal_on_its_thread` (renamed from
    `a_palette_start_opens_a_thread_tile`).

- ✅ **No focus line: the focused tile is said by its title's tone alone** (2026-10-05,
  the person's review). The person found the line along the focused tile's top was what made
  the workspace read as generated. It was a 1.5 pt bar in the text's tone, inset at both
  ends, drawn while two or more tiles showed. Across a strip it stopped at the tile's edge and
  read as a stray separator, not as a sign of focus. The tools held up as the bar mark focus
  without one: Zed by its active tab's tone, Ghostty and iTerm2 by a hollow cursor in the
  panes without focus. Slopty already does both. The focused title leads in the primary tone
  and the others step back to muted (`tile::title_ink`), and a terminal without focus draws a
  steady, muted hollow block (`terminal::element`).
  - Deleted: the line (`tile::focus_line`) and its call on the header and on a column's shown
    tab, `Chrome::focus_line` and the strip's count of tiles in view, the theme's
    `Surfaces::focus` with `alpha::FOCUS` and `SEEN_LC`, and their tests. This supersedes
    the focus-line parts of the earlier entries above (§4b's numbers, "A quieter focus line
    and overview ring", "The focus line is derived, not a fixed share"). The overview's ring is
    unchanged.
  - Test: `workspace::tests::focus::the_focused_tile_is_said_by_its_titles_tone_alone`. With
    two tiles in view, both headers sit on the content with nothing drawn along their tops.
    The focused title is in the primary tone and the other is muted, and the tone follows the
    focus.

- ✅ **The thread is type on one plane** (2026-10-05, the premium pass;
  `.research/premium-pass-2026-10-05.md` T1–T4). The person found the UI still read as
  generated. In the thread the cause was composition: a call that failed or waited was a card
  with a coloured edge while its neighbours were lines, the plan was a card of its own, and a
  request's answers each wore a glyph. Zed's and Codex's threads, and MonoCode's, put every
  step on one plane and say how it stands in words.
  - **Every call is a line**, whatever it did or how it stands (`view/tools.rs`). How it stands
    is its mark and a word after its name: "Waiting for you" in the warn tone, "Failed" in the
    error tone, "Not allowed", "Stopped". Its body (a diff, a command's output, sources) sits
    in a quiet well (`kit::inset`, `radii.md`) indented to the line's words. Deleted: the card
    look (`Look`), the edges in the warn and error tones and their `alpha::FAILED_EDGE` and
    `alpha::ASKING_EDGE`.
  - **The plan is type** (`view/plan.rs`): a small muted eyebrow ("Plan", and "· Awaiting
    approval" in the warn tone while it waits), its heading at the prose size in the medium
    weight, then its Markdown. The copy shows under the pointer. This supersedes "A plan is a
    card" in "The thread view recalls what was sent, reads a plan as a card…".
  - **Rhythm parts the kinds.** Where the thread goes from prose to the lines of calls, or
    back, and round a plan, the gap grows to `spacing.md`; lines of a kind keep the tight one.
  - **Answers are words.** A request's buttons carry no glyphs, and the solid one comes last,
    where macOS puts the default. A group's rule gave way to space. A waiting request, its
    call and its plan lead with the one *needs you* mark (`ThreadView::needs_you`).
  - Lint: `kit::tests::the_stream_wears_no_card` (no `kit::card`, `card_part`, `raised` or
    `raised_part` in the call or plan views).
  - Tests: `conversation::thread::view::tools::tests::a_call_says_how_it_stands_in_a_word`,
    `conversation::thread::tests::face::{a_call_that_asks_as_it_arrives_is_answered_under_it_at_once,
    a_plan_in_the_thread_takes_its_answer}`.

- ✅ **One progress language: `kit::progress`** (2026-10-05, the person's review;
  `.research/ux-audit-2026-10-05.md` items 1 and 8). Progress was drawn five ways, three of
  them a hard square line along an edge (the terminal's `OSC 9;4` report, a tile header's
  upload, a navigator row), and the project board's was a flat double line. macOS, Linear and
  Zed draw one: a rounded, capped stroke on a soft track that eases to each value.
  - **`Bar`** is a capsule on the quiet track (`border_subtle`, `border` under Increase
    Contrast), `spacing.xs` tall. `Progress` says what it shows: a known share glides to it
    (`Motion::settle`, ease-out); `Busy`, with no share known, breathes its opacity on the spin
    clock (12 Hz, no frames of its own); `Paused` is muted and `Failed` is the error tone. It
    shows only after 400 ms (`SHOW_AFTER`), laid out but unseen until then, so a quick task
    never flashes and nothing jumps; `at_once` skips the wait where the bar is the thing
    looked at. Under Reduce Motion a share lands at once and `Busy` stands at
    `alpha::STRONG`. It is a `ProgressIndicator` with its value ("42%") and its name.
  - **`Segments`**, a capsule per part, was the board's until it showed the merged share
    alone as a `Bar`; deleted 2026-10-05.
  - **`ring`** is the same in a round slot: an arc with round caps on a `border` track. The
    composer's context, an attachment's upload and the upload pill in a tile header use it.
  - **Where each went.** The terminal draws nothing for a report: `TerminalView::progress`
    says it, `TerminalViewEvent::Progress` tells the workspace, and the tile's header shows a
    48 pt bar with its figure beside the title (`report-{session}`). The upload line under a
    tile's header became the pill's ring and figure. A navigator row says the figure, then a
    still 24 pt bar after it. A picture's upload, a goal's budget and a worker's install use
    `Bar`, and so does the project board, for its merged share.
  - Deleted: the terminal's sweep and its edge line, `tile::PROGRESS`, the navigator's
    `progress_line`, `add_worker`'s `SWEEP` and `SEGMENT`, the composer's own ring.
  - Lint: `kit::tests::a_progress_is_kit_progress` rejects a hand-drawn fraction width beside a
    progress tone outside the kit; its check of itself is
    `the_progress_check_knows_a_bar_from_a_column`. The status bar's transfers and the board
    are waived until they move.
  - Tests: `kit::progress::tests::{a_share_is_its_figure_in_its_state_s_tone,
    an_unknown_share_breathes}`,
    `terminal::progress::tests::a_report_shows_as_the_kits_progress`,
    `terminal::view::tests::a_progress_report_is_told_not_drawn_on_the_edge`,
    `add_worker::tests::a_busy_bar_breathes_on_the_spin_clock_unless_motion_is_reduced`,
    `workspace::tests::nav_rows::a_progress_report_is_a_figure` and the remote upload tests.

- ✅ **The navigator's top is the lights, and the filter is the first row under them**
  (2026-10-05, the person's review; UX audit item 3). In light the filter was a white,
  hard-edged field crammed beside the traffic lights: the brightest thing in the corner.
  Apple's sidebars, Things and zeron keep that row to the window's controls and put search
  under it.
  - The top row is the title bar's height and holds the traffic lights and, past them, the
    navigator's toggle, at the same place the title bar keeps it while the navigator is
    hidden, so it never moves.
  - The filter is `kit::search_field`: a capsule a row tall on the selection's wash, the
    search glyph leading, `spacing.sm` in from the panel's sides. Holding the keyboard it
    takes the selected fill and its ring; at rest it has a hairline only under Increase
    Contrast. No rule under the top: the panel is one surface from top to bottom.
    This supersedes the navigator parts of "The field is a well" and of "The frame recedes"
    ("The navigator's filter is a well on the selection's fill").
  - Test: `workspace::tests::tab_strip::the_navigator_is_the_windows_height_and_the_bar_starts_at_its_edge`.
  - Amended 2026-10-06: the filter is hidden at rest and the top row ends in "Search" and
    "New agent" (below).

- ✅ **The title bar takes the content's tone** (2026-10-05, UX audit item 5). "The frame
  recedes" put the title bar in the navigator's tone, one frame round the strip. Beside the
  terminal's ground it read as a band of its own over every tile. The bar now sits on
  `theme.content()`, so the tiles' headers, the bar and the content are one plane and only the
  navigator steps off it, as Zed's and Ghostty's do. The bell's badge cuts out of the same tone.
  This reverses the title bar part of "The frame recedes".

- ✅ **A field is seen on its page in light** (2026-10-05, the premium pass T5). `kit::field`
  in light was the sunk shade on white, which a hundredth of tone could not part from the
  page: a field with no edge. It now wears the `border_subtle` hairline in light (`border`
  under Increase Contrast) with the shade; dark keeps the shade alone. This amends "The field
  is a well" ("no hairline") for light. Test: `kit::tests::a_field_is_seen_on_its_page`.

- ✅ **A palette on its way out never takes a press, and the navigator keeps its place**
  (2026-10-05, found by the navigator's new top moving the rows down).
  - A dismissed palette fades for `Pace::Exit` while it is drawn. It was still lifted over
    everything at the dialog's priority, so a machine's menu opened in that time lost the press
    on any row the fading sheet covered. Leaving, it is drawn in its place under a menu
    (`CommandPalette::render`, `WorkspaceView::render`); live, it is lifted as before. Test:
    `workspace::tests::bars::the_clipboard_is_stopped_and_shared_with_one_machine_from_the_palette_or_its_row`.
  - The navigator splices its list from the first changed row to the last. When *Needs you*
    opened at the top in the same frame a row at the foot grew a line, the splice spanned the
    list and GPUI put the view back at its top. The row at the view's top is now found again
    by what it shows (a tile, a worker, a group, a thread, a board; `NavRow::anchor`) and the
    view starts from it as before. Test:
    `workspace::tests::nav_list::needs_you_lists_a_waiting_tile_scrolled_out_of_view`.
  - A waiting row in *Needs you* says the agent's words and its place as one line, as a
    tile's second line does, so the place gives way at the end. It had been pressed to a lone
    "…" between the words and the answers. Test:
    `workspace::tests::nav_rows::a_tile_row_reads_its_age_or_its_state_then_its_place`.

- ❌ **No agent wears a mark of its own: status first, the agent in words** (2026-10-05,
  `.research/icons-2026-10-05.md` §4.1, with the companions' deletion in `brand.md`;
  superseded the same day by "Each agent wears its owner's mark, in one colour" in `brand.md`
  and "Identity leads, state trails" below). The
  person saw a tiny orange figure in rows and headers and orange sparkles in the composer:
  one agent in two marks, neither readable at 1×.
  - **A row and a header lead with how they stand.** That is the status mark from
    `icons::Status` whenever there is one. At rest, every agent's tile, thread and palette line
    shows the same neutral kind glyph (`icons::AGENT`, a conversation) in the row's ink. A row under *Needs
    you* or *To review* leads with its status mark, where it showed the agent's mark before.
  - **The agent is named in words.** A tile's place (its header, its navigator row and its
    palette line) starts with the agent's name ("Claude Code · ~") unless the title already
    is it. The composer's model chip says the model with no mark. The thread's header and
    empty state, the branch's agent choices, the screen's driver pill and a file's author tag
    carry none. A review's comment by an agent and its review button keep the neutral glyph.
  - **No colour for agents.** Colour stays for meaning: green, amber, red.
  - The thread's working line shows the spinner, or the *needs you* mark while it waits on
    the person, in the slot the companion took.
  - Deleted: `icons::AgentMark`, `Glyph::agent`, the `agents/` assets (pi, `OpenCode`,
    `code-circle` and their licences), `Surfaces::agent` and its OKLCH with its tests,
    `slopty_theme::PI`, and the spin clock's `steps_now` and `steps_wake`. `icons::glyph` no
    longer takes the theme.
  - Test: `workspace::tests::agent_tile::an_agents_tile_names_it_in_words_beside_the_one_agent_glyph`.

- ✅ **A request is one decision: Allow and Deny, the rest set apart** (2026-10-05, the
  second critique, finding 03, as ruled). An approval laid five like buttons in a row
  ("Always allow /work; accept edits mode", "Deny", "Deny…", "Deny and stop", "Allow"), tucked
  into the composer's head. The person had to read every one to find the two that matter, a
  standing grant sat at the weight of this once, and the decision read as another way to send
  the message under it.
  - **The row is Deny, then Allow**, a base unit apart, the solid last as a dialog's default
    is (`view/decision.rs`, `arrange`). The solid is the neutral one, never green. A
    question's answers keep their order in the row, all quiet.
  - **Other ways to deny are behind the deny's chevron**, in a menu named "Other ways to deny":
    "Deny with a reason…" first, then every other deny the agent offers ("Deny and stop", a
    deny that reaches further) with its reach in its words. Deleted: the "Deny…" button.
  - **A standing grant never sits in the row.** It stands under it, headed "From now on", its
    words on a quiet button and its reach written out whole at the chrome's size, never cut,
    since that reach is what the person grants.
  - **What is asked reads first.** The title is at the task-title size (base + 1,
    `Typography::task_title`) at the medium weight, the command or words under it at the
    chrome's size, and a full step of space before the answers.
  - **It is its own card**, a full step above the composer. The rest of the tray (the plan,
    the queue, the work in the background) stays the composer's head. Under a call, the same
    decision answers it, the way back to the agent's own prompt leading the row.
  - Tests: `conversation::thread::view::decision::tests::{allow_and_deny_lead_and_the_rest_go_where_they_belong,
    a_standing_grant_never_leads_or_sits_in_the_row, a_questions_answers_keep_their_order_and_none_leads,
    a_scoped_answer_says_how_far_it_reaches}`,
    `conversation::thread::tests::doors::{an_approval_is_allow_and_deny_with_the_rest_set_apart,
    deny_with_a_reason_sends_the_reason_with_the_deny}`.

- ✅ **The chrome's icons are SF Symbols, drawn by the OS at device pixels** (2026-10-05,
  `.research/icons-2026-10-05.md` §4.3–4.4 and §5, steps 3 to 5 of its plan). Supersedes "One
  icon set at one weight" above. (Superseded 2026-10-07 by "The chrome's icons are Tabler's,
  and a file's are Material's": the platform's `symbols` and its prewarm are deleted.) (Amended 2026-10-06 by "State is a glyph": the empty and
  dashed rings are painted by `icons::Ring`, since SF's `circle.dashed` lost its gaps at 1x.
  Amended 2026-10-06 by "An icon takes its words' size, weight and tier": git is Octicons, and
  no symbol is drawn under 12.5 pt.) At 1x, the person's view, only 14 % of the Hugeicons ink
  pixels were solid; SF Symbols at the text's size have 77 % more, and sharpen and grey with
  the system font beside them.
  - **The platform draws them** (`slopty_platform::symbols`). A `Symbol` is one of a closed
    list of SF Symbols names. `rasterize` draws one at a `SymbolSize` (the point size and
    weight of the text beside it, and the symbol scale) and a display's scale. The result is
    one alpha byte per device pixel, with the symbol's alignment rectangle and baseline. The
    OS draws it, so an icon sharpens and greys with the system font beside it.
  - **Placed by its alignment rectangle, not its box.** Measured on macOS 27, AppKit's
    rectangle for a symbol is the box's width and runs from the baseline to the cap height of
    the text it is sized to. The box adds a point or two of uneven padding. A slot centres on
    the rectangle, and an inline symbol puts its bottom edge on the text's baseline. iOS
    reports the baseline itself (`baselineOffsetFromBottom`).
  - **Any thread, and prewarmed.** Each raster makes its own image and bitmap context and
    makes the context current on its own thread alone. Eight threads at once draw the same
    bytes as one. `Masks` keeps every mask drawn, and keeps a miss as a miss. Its `prewarm`
    draws a list on a utility-QoS thread, because the first symbol of a process loads the
    catalogue (40–70 ms; `docs/MEASUREMENTS.md`, "SF Symbols as masks").
  - **Each raster in its own autorelease pool, and only what is drawn warmed** (2026-10-06).
    Without a pool, every drawing's image and context stayed until its thread ended. The old
    prewarm of every symbol at four sizes and two scales then left about 31 MB in the
    footprint at rest. Every symbol drawn also keeps its share of the OS's symbol data for
    good. So the prewarm draws what the last launch's first frames drew, written down three
    seconds after the window opened (`data_dir/symbols`, by display scale). A first launch
    warms the catalogue alone and draws its first frame's few masks itself. A list of symbols
    kept by hand would drift from what the chrome draws; the written one cannot
    (`docs/MEASUREMENTS.md`, "the footprint at rest").
  - **Painted unscaled** by gpui-fast's `Window::paint_mask` (`fast/mask.rs` in the fork).
    `paint_svg` draws at twice the size and halves it, which cost the symbols crispness at
    1x.
  - **Sized by the words beside it** (`icons::IconSize`, `icons::Drawn`). An inline icon is
    drawn at the secondary text's point size in the `icon()` slot, a large one at the chrome's
    size in `icon_large()`, a disclosure chevron at the caption's size, semibold and small, as
    Apple draws its own. A slot sized again by the chrome's zoom draws its symbol larger by as
    much. The ink is the slot's text colour.
  - **A wide symbol fits its slot.** A symbol is drawn at the words' size, but a wide one
    (`server.rack`, `folder`, `display`) at that size is wider than the square slot, and in the
    first review a machine's rack touched its name. One wider than the slot is drawn again at
    the point size that fits, in quarter points (`icons::fitted`), so it keeps its weight and
    shrinks only as much as it must.
  - **Drawn by us, three things only:** the app's mark; the working mark, now twelve spokes
    (amended 2026-10-06 by "The working mark is a braille cell": a cell of six dots)
    whose brightness steps round on the existing spin clock (a ring turned in 30° jumps read as
    dropped frames), upright and breathing under Reduce Motion; and the dot of a finish not yet
    seen.
  - **The status marks:** idle `circle`, waiting `circle.dashed`, needs you
    `exclamationmark.circle.fill`, failed `xmark.circle.fill`, away `wifi.slash`. The two that
    carry colour are filled, so the colour has a body at 1x.
  - **A file is one of nine kinds** (`FileType`), each a monochrome symbol in the row's ink:
    code, text, data, image, PDF, archive, audio, video, lock; anything else is the plain
    document. The 55 coloured drawings are deleted, with `assets/icons` and `assets/file-types`.
  - **The neutral agent glyph is a conversation, `text.bubble`** (`icons::AGENT`), not the
    sparkles: those are the cliché mark of AI. It is worn by an agent with no mark of its own;
    Claude Code, Codex and pi wear their owners' (`brand.md`, "Each agent wears its owner's
    mark, in one colour").
  - **An empty state has no plate.** `kit::notice` puts its mark on its own, a light symbol at
    the page heading's size and the large scale, as the system's own empty states do; the
    raised 40-point disc under it is gone. A tile's state (away, opening, starting, a thread
    being read) heads its notice with its status at the same size (`icons::notice_status`);
    the plate had carried the small inline mark, and without it the mark was a speck.
  - **A disclosure chevron is the system's own at rest**, right or down at exact pixels, and
    only while it turns is the right one turned, by `paint_mask`'s transformation.
  - **The kit's components draw the same symbols.** gpui-kit's `IconPainter` (our fork)
    hands an icon a component names by path to the app, which draws its symbol; a name with
    no symbol keeps the kit's SVG.
  - Lints: `kit::tests::the_chrome_draws_symbols` (since 2026-10-07 `the_chrome_draws_its_own_icons`; no SVG, `IconName` or `Glyph` in the
    chrome but the app's mark) and `kit::tests::an_icon_takes_its_words_size` (a symbol is
    sized and painted only in `icons.rs`, from the type scale).
  - Tests: `slopty-platform/tests/symbols.rs`, including
    `every_symbol_the_chrome_draws_is_on_this_os`, which the macos-26 lane runs too;
    `icons::tests::{a_file_leads_with_its_types_symbol, each_status_has_its_own_mark}`,
    `file_types::tests::*`. The `thread@2x` golden is the thread as a Retina display draws it,
    since every other golden is at 1x and cannot show a symbol's or a hairline's detail. The
    test socket's `Render` takes a scale, and gpui-fast's `Window::render_to_image_at` draws
    the window offscreen afresh at it, so a 1x runner (CI's macos-26) takes it too.

- ✅ **A command is its words** (2026-10-05, `.research/icons-2026-10-05.md` §4.5, step 2;
  premium pass T10). Every palette command led with an icon picked to decorate it (a sticky
  note for "New note", a brain for effort), so the palette read as a grid of clip art and no
  icon said anything the words did not.
  - **The palette.** `PaletteItem::new` takes no icon, and its `icon` is optional. Only a line
    that is a thing leads with what it is: a tile, a machine, a project, a file, a folder, and
    the agent picker's agents, machines, folders, checkouts and past sessions
    (`with_icon`). A command keeps an empty slot while other lines show a mark, so every title
    starts on one edge. Unmute keeps its words.
  - **The thread.** No brain on the effort chip or on a thought's row, and no glyph on the
    background work's chip once it has finished or on its tray head; a running spinner and a
    failure's mark stay, since they say something.
  - **The find bars** (`kit::FindBar`, the search tile): match case, whole word and pattern
    are the typographic toggles `Aa`, `W` and `.*` (`kit::text_toggle`), and Replace and Replace
    all are words.
  - **Away** is said by its word: the server and the display are their plain glyphs in the
    muted tone, not a struck-through one.
  - Lint: `kit::tests::a_command_is_its_words` (no command line from any palette source has
    an icon, and no `with_icon` follows a `PaletteItem::new` outside the agent picker).

- ✅ **The UX audit's small items** (2026-10-05, `.research/ux-audit-2026-10-05.md` items 7,
  9, 10, 13, 14, 17 and 18, with the coordinator's amendments; item 19, a shimmer on the
  working line, was dropped).
  - **A request answers by key only where it has the keyboard.** ⌘↵ gives its plain allow and
    ⌘⌫ its plain deny, bound in the `Request` key context alone: the request's card (a press on
    it, or Tab to one of its answers) and a focused *Needs you* row. They are never bound
    because the composer is empty, so a request that arrives while the person types never
    changes what a habitual key does. Both are in the palette and in Settings → Keyboard.
  - **An empty thread says one thing.** A new one is a `kit::notice`: "New Claude Code
    thread", then where it works, the machine and the model. A thread whose only row is a
    request says nothing in the list (its card says it), and one out of reach leaves it to its
    tile. Reading one says so after the loading grace.
  - **Away is one word with a clock.** "Reconnecting…" everywhere, from the first dial; past
    ten seconds the tile says for how long in ten-second steps. A body with nothing to show
    (a tile kept from the last run, a shell never attached) says it in its middle as a notice
    with its actions, not in a pill at its foot.
  - **Touch.** A request's answers are at least a control's height (44 points on touch). The
    navigator's "+", "…" and chevrons stand at rest on touch, after the readouts, since a
    finger has no hover.
  - **Settings → Keyboard** rows are one line: the command's words, what it was when the file
    changed it, and the caps with no well at rest. The command's name in the file is the row's
    hint. A command with no keys offers a quiet "Add shortcut" under the pointer, on the
    keyboard's focus and on touch.
  - **Words.** The composer invites ("Ask Claude Code…"); "/" and "@" are taught by the "+"
    menu (Attach files, Commands, Files and symbols). An MCP tool reads by what it does, its
    server after it ("App click · computer-use").
  - Tests: `conversation::thread::tests::{a_request_is_answered_by_its_key_only_where_it_has_the_keyboard,
    an_empty_thread_says_it_is_new_and_a_request_alone_speaks_for_itself,
    a_requests_answers_are_a_fingers_target_on_touch}`,
    `workspace::tests::tiles::{reconnecting_says_for_how_long,
    a_tile_says_how_long_its_worker_has_been_away}`,
    `workspace::tests::relaunch::a_tile_kept_nowhere_says_where_its_worker_is`,
    `workspace::tests::frame::a_finger_finds_the_navigators_actions_at_rest`,
    `workspace::attention::tests::a_focused_needs_you_row_answers_by_its_keys`,
    `settings_editor::tests::a_chord_is_recorded_into_the_file`,
    `conversation::thread::view::tools::tests::a_tool_id_reads_as_words`,
    `conversation::thread::view::composer::tests::the_add_menu_begins_a_command_or_a_mention`.

- ✅ **Type roles, and focus apart from the accent and success** (2026-10-05,
  `.research/design-critique-astra-2026-10-05.md` §4 and finding 08). Earlier rulings that put
  the focus ring in the accent are superseded by this one.
  - **Type roles.** `slopty_theme::TypeRoles` (`Theme::roles`) names what a piece of text is,
    each role with its size, line and weight. With a pointer they are caption 11/16, metadata
    12/18, chrome 13/19, action 13/19 at 500, task title 14/20 at 500, section 13/18 at 600,
    panel title 16/22 at 600, prose 15/24, page heading 22/28 at 600 and first run 26/32 at
    600. On touch every role steps up (chrome 17/22, task title 17/22, panel title 20/25), so a
    label has the presence its 44 pt row gives it. Each role follows the chrome size setting,
    and prose follows its own. `kit::typed` sets a role. The request card (its question, what
    it asks, the standing grants) and the Keyboard rows use them first; other surfaces move to
    them as they are next touched.
  - **Focus is neutral.** `Surfaces::focus` is the chrome's text, the ring every keyboard stop
    wears at `alpha::STRONG`. It clears 3:1 on every ground on every background and at either
    contrast. Green now means live and done only, never "this has the keyboard".
  - **Success has its own seed** in the tone tables, apart from the accent's. Both are the
    brand's green for now, so a done state and a live mark can part without touching every
    use.
  - Tests: `slopty_theme::tests::{the_focus_ring_is_neutral_and_seen_everywhere,
    success_is_its_own_token, the_type_roles_are_the_scale_the_critique_set}`,
    `a11y::tests::the_keyboard_rings_a_stop_and_the_pointer_does_not`,
    `workspace::tests::bars::the_keyboard_reaches_a_machines_menu`.

- ✅ **No focus plate: the focused pane is found without one** (2026-10-05,
  `.research/design-critique-astra-2026-10-05.md` finding 10 and I6). The critique asked for a
  restrained plate round the focused title in case two panes that are not terminals left focus
  unfindable. Judged on the composed scenes `review-agent.png` (a thread beside a review) and
  `project-lanes.png` (a shell beside the orchestrator's board), focus is found at a glance by
  three cues that agree:
  - the focused title in `text` at the medium weight, the others in `text_muted` at the
    regular weight (`tile::title_ink`);
  - the focused header alone shows its controls at rest (`tile.rs`, `focused && quiet`);
  - the navigator's plate sits under the focused tile's row.
  A plate would be a fourth signal, and chrome the person has to look past on every pane, so
  there is none. Reopen this if a scene comes up where the focused header holds readouts (its
  controls then hide) and the navigator is closed, so that tone and weight are the only cues.

- ✅ **The review gives the change the tile; a failed remote open ends its wait in its pane**
  (2026-10-05, `.research/design-critique-astra-2026-10-05.md` findings 04 and 23).
  - *Review chrome steps back.* The switch, refresh, the agent's review and the git part share
    one toolbar at the header's height (`density.header`, 40 pt). Its title stays in the tile's
    own header, so the toolbar does not repeat it. A file's head holds its keep and put back.
    A hunk holds its own only in a file of more than one hunk: in a file of one, the same choice
    twice was noise. A hunk's buttons keep their place while hidden, so revealing them moves
    nothing. They show while the pointer is on the hunk's head or lines, show for the one that
    has the keyboard (a stop at no opacity until it is focused), and always show on a touch
    screen, which has no hover.
  - *Findings fold.* The band of the agent's findings with no line on show is measured before
    it is drawn: its words in lines of the band's width at the foot's letter estimate. When it
    would take more than a quarter of the diff's room (`FINDINGS_SHARE`; the room is the body
    under the toolbar and over the foot, less the band), it folds to one row at the row height.
    That row has the agent's mark, what came (in the error's tone if the review was turned
    down), a disclosure that opens the findings and folds them again, and the ✕. The estimate
    was chosen over measuring the drawn band, which settles a frame late and would flash the
    findings open first. The tile now learns its body's height with its width
    (`ReviewView::set_layout`).
  - *Code reads at the code's size.* Diff lines are at the terminal's `mono_size` (13 by
    default) on a 1.5 line, 13/19.5. Line numbers stay at the chrome's small size
    (`lines::Ink::number` sets it, so they keep 12 pt beside any code size), and the gutters
    keep their 8 pt gap.
  - *A failed open is an end, not a wait.* A window or display the worker could not open
    replaces "Opening Safari" in its pane with the error's mark (CircleAlert, large icon size,
    error tone), what is so as a task title ("Window is no longer available"), why in the
    chrome's words ("Safari is not open on studio any more."), and "Choose another window" as a
    secondary button at the control height (44 pt on touch). Other failures say "… did not
    open" with the failure's own words. The header's slot no longer turns. The status bar no
    longer repeats it: a target's failure is said once, where it waited. A display made for
    this device still says it in a notice, as no pane waits for it. The button opens the
    picker on that machine, and the window picked takes the failed pane's place.
  - Tests: `review::tests::{a_hunks_keep_shows_with_the_pointer_and_never_twice,
    many_findings_fold_to_one_line_that_opens_them}`,
    `workspace::tests::bodies::a_window_that_did_not_open_says_so_in_its_pane_and_gives_way_to_another`.
    Goldens to retake: `review-agent*`, `remote-window`.

- ✅ **No bar along the bottom: notices go beside their work, readouts to the title bar's end
  only while they have something to say** (2026-10-05; its bar half reversed 2026-10-09 by "A
  foot bar holds the plan, the agent and what runs out of sight" below; UX audit item 4
  (C3), read with the design critique's dissent; supersedes "The status bar's left slot says where the human is",
  "Design wave 3, the frame"'s status bar, the status bar half of "The status bar states
  facts", and "Notices live in the status bar"). The 24 pt band along the window's foot held
  the focused tile's worker in most goldens, which the navigator and the breadcrumb already
  name. The rest of it was a mix: a file's caret, a stream's rate that its overlay also said
  with another number, the plan, ports, transfers, a release, and the notices. Apple's guidance
  keeps critical items out of a bottom bar, since a window is often moved until its foot is
  hidden. The audit moved everything into the title bar. The critique objected that a global
  top bar should not own every state either: a pane's facts stay in the pane, and a failure
  stays beside the work it concerns. Both halves are taken.
  - **Deleted:** the bar, its region view and the key bar's hand-off (`set_key_bar_shown`:
    with nothing at the foot, the iOS key bar has nothing to displace). The strip runs to the
    window's bottom edge.
  - **Said once, where it belongs:**
    - the focused tile's worker, its health and a quick link's round trip go; the navigator's
      machine rows and the breadcrumb say them;
    - a command's running time is its header's;
    - a stream's size and rate are its stats overlay's;
    - a file's language, indent, line endings and caret are a quiet foot line inside the file
      tile (`FileView::render_foot`, `Rust · Spaces: 4 · Ln 12, Col 5`, line endings only
      when not LF), as an editor's own status line says them.
  - **Notices beside their work.** A notice about a tile's own work (a drop that did not
    land, files not sent, an upload that did not reach its machine, a window that did not
    open) is said under that tile's header at its trailing edge and moves with it
    (`show_failure_at`, `show_notice_at`). Any other notice (a copy, a closed tile to take
    back, an agent off screen that needs the person) sits in the title bar's lane between the
    breadcrumb and the readouts: Xcode's activity area, a place no tile draws in. A tile's
    notice whose tile has closed moves to the lane. Two at most in each place; the hold,
    hover and stickiness rules are unchanged.
  - **Readouts at the title bar's end, before the bell, only while they have something to
    say** (`workspace/readouts.rs`):
    - the server while it does not answer, with what that costs under the pointer;
    - the focused machine's link once it has held on a DERP relay, with its fix;
    - the plan's windows once one is 80 % used (under that the composer's meter is enough);
    - the ports forwarded here and the transfers in flight, each opening its list;
    - a newer Slopty;
    - the frame time while the stats show.
    Nothing shows there by default. The popovers drop from under the title bar.
  - **macOS:** traffic lights, then the navigator toggle, breadcrumb and "+", then the notice
    lane, then the readouts, then the bell and "…". The content runs from the title bar to
    the window's foot.
  - **iOS:** iPad matches macOS, with the safe area. A phone's bar is its navigation bar (the
    workspace's name, the bell, "…"), with no room for readouts. Its notices hang centred just
    under the bar. The home indicator's band continues the strip's ground or the key bar.
  - Tests: `workspace::tests::chrome::no_bar_runs_along_the_bottom_and_a_notice_sits_by_its_work`,
    `file::tests::a_file_says_its_facts_at_its_foot`,
    `workspace::tests::bars::{a_far_used_plan_is_said_in_the_title_bar,
    the_readouts_count_what_is_shared_and_say_no_machine, a_quick_round_trip_draws_no_chrome}`,
    `workspace::tests::toasts::a_notice_sits_in_the_title_bar_over_no_tile`.

- ✅ **A focused text field says so quietly** (2026-10-05, design review of
  `agent-needs-you-navigator`). The composer that had the keyboard wore the focus tone, which
  is the chrome's text colour, as its whole hairline. A near-black ring round the card was the
  loudest thing on the screen. A field's edge now takes `Theme::field_focus`: the focus tone
  mixed 45 % into the card's ground, raised in 5 % steps until it clears 3:1 against the card
  (WCAG 1.4.11). It is the focus tone whole under Increase Contrast. Measured across the
  fifteen test backgrounds: 3.2 to 4.0:1, against 8.8 to 17.1:1 for the full ring. The keyboard
  ring on buttons and rows is unchanged. Test:
  `slopty_theme::tests::a_focused_field_says_so_quietly_and_still_clears_three_to_one`.

- ✅ **Identity leads, state trails** (2026-10-05, `.research/agent-marks-2026-10-05.md` §3.4,
  with "Each agent wears its owner's mark, in one colour" in `brand.md`). With real marks, the
  old rule (the status in the lead slot, the kind only at rest) would have hidden the mark on
  exactly the rows the person watches: a working agent showed a spinner, so marks would have
  appeared on idle rows alone. The macOS source lists (Mail, Finder, Xcode) lead with the
  item's icon and put its badge or count at the end, and T3 Code splits a row the same way.
  - **A row's lead always says what it is** (`palette::lead_slot`): an agent's mark, else its
    kind (a terminal, a file's type, a folder, a window). It never changes while the row lives,
    so the eye finds the same agent in the same column. An agent's mark names its agent to a
    screen reader ("Claude Code"); a kind's symbol says nothing its words do not.
  - **How it is doing ends the row's first line, one mark by precedence:** needs you, failed,
    at work (a running command's clock beside it), away; else the dot of a finish not yet
    seen; else the age. Under the pointer the end still gives way to the close button. Never a
    badge on the mark: no dot in a logo's corner, no ring round it, no tint. Amber is the one
    warm colour in a list of one-colour marks, so a row that needs the person is still found at
    a glance, and *Needs you* still gathers them first.
  - **The same order everywhere:** navigator tile, thread and board rows; a tile's header and
    its tabs, the status before the header's controls (an agent asking keeps saying so in its
    chip alone, and none at rest); the palette, where the state's mark replaces its word; the
    session picker; the overview's labels; a starting thread's header.
  - **Words.** A tile's place no longer starts with the agent's name ("Claude Code · ~"): the
    mark says it and the width goes back to the title. The name stays in the mark's
    accessibility label, the agent picker's lines (mark and name, the name being the choice),
    the composer's model chip (the mark before the model) and the empty thread's question
    ("What should Codex do in slopty?"; the notice it replaced is gone, see "An empty thread is
    its composer").
  - **A thread is called by its work, whichever agent runs it** (2026-10-06). Every adapter
    fills the thread model's one title alike: the agent's own name for the session where it
    publishes one (Claude Code's summary, a Codex thread's name, a pi session's name, an ACP
    session's title), else its first prompt, trimmed. Before either, a thread is called by its
    agent ("Codex", "pi", "opencode"), as a Claude Code terminal is before its summary, never
    a bare "Thread N". So the mark and the title read as one identity. Test:
    `workspace::tests::agent_tile::a_thread_is_titled_by_its_work_else_by_its_agent`; the
    adapters' own in `slopty-agent/tests/{codex,pi,acp}.rs`.
  - Tests: `workspace::tests::agent_tile::an_agents_tile_leads_with_its_mark_and_ends_with_its_state`,
    `workspace::tests::tiles::the_header_leads_with_its_kind_and_ends_with_its_state`,
    `workspace::tests::nav_rows::a_tile_row_reads_its_age_or_its_state_then_its_place`,
    `workspace::tests::frame::an_unseen_dot_marks_a_finished_tile_until_it_is_looked_at`,
    `workspace::tests::thread_start::new_agent_opens_the_picker_with_the_last_choices`.
- ✅ **The first run asks where to work** (2026-10-06,
  `.research/design-critique-astra-2026-10-05.md` finding 20). The first page led with
  "Connect to a server" and the tailnet, so the infrastructure came before the choice it
  serves. The local checklist also buried "is this Mac ready" under versions and addresses.
  - **The page** opens on "Choose where to work" in the first-run role (26/32, strong), with
    one line under it. Then come the two ways, 24 pt apart, each said in one 13/19 line.
    "Use this Mac" is a row to press ("Work on this Mac. Your other devices reach it too.").
    "Connect to an existing server" is a heading over its line ("Reach the machines a Slopty
    server already lists"). Under it, 12 pt apart, are the servers the tailnet answered with
    (or the line saying nothing did, and what to start), and the address with Connect. What
    the link needs stays as secondary help: that line, and the foot about the tailnet's
    encryption. Setting a server up over SSH follows.
  - **Only where there is a choice.** A device with no Mac to share (an iPhone, an iPad) has
    one way in, so its first run stays "Connect to a server". So does the "Connect to a
    server…" dialog over the workspace.
  - **This Mac's checklist is concise.** A line that holds is its mark and its name. A line
    that does not keeps what it waits on or what to do, with its button. "Turn on
    slopty-worker" keeps the worker's name, since System Settings lists it by that name.
    What a line that holds says of itself (the worker's version, where the server runs,
    this Mac's name on the tailnet), with the app's version and build, waits under "Show
    details" below the list (`Flow::details`).
  - Tests: `slopty-app` `tests::the_checklist_is_concise_and_its_details_open_on_request`,
    `tests::this_mac_runs_the_server_then_waits_for_it_to_list_the_worker`; e2e
    `gallery::the_first_run_offers_one_way_in` (its headings),
    `through_server` (the first run's heading). Goldens: `first-run`, `first-run-dark`,
    `this-mac`, `add-worker` (the blurb's 13/19 line).
- ✅ **Work under way shimmers on the step clock** (2026-10-06, `.research/ux-audit-2026-10-05.md`
  §11 #19). A thread's working line said "Working" in still, muted text, while "Thinking"
  shimmered, so the two looked like different kinds of thing.
  - **The line.** The working line's words ("Working", "Stopping", a retry's words) now
    shimmer as "Thinking" does. "Waiting for you" stays still, since it describes a state
    rather than work.
  - **The element.** Both use `kit::shimmer`, a band of `text` sweeping across `text_muted`
    words. It moves one step at a time on the working mark's spin clock rather than gpui-kit's
    `ShimmerText`, which asks for a frame on every refresh. A thread at work draws 12 frames a
    second, not the display's 120 (`docs/MEASUREMENTS.md`, "words at work shimmer on the step
    clock"). Under Reduce Motion the words are plain.
  - Tests: `kit::shimmer::tests::the_band_crosses_the_words_once_a_sweep_and_rests_between`,
    `conversation::thread::tests::composing::the_working_line_shimmers_and_waiting_does_not`,
    `...::the_working_line_shimmers_on_the_marks_twelve_frames`.
- ✅ **The window is titled by its workspace** (2026-10-06, `.research/ux-audit-2026-10-05.md`
  §11 #20). Every window was called "Slopty", so the Window menu, Mission Control and cycling
  the windows by key could not tell two workspaces apart. The window's title is now the
  workspace's name, the same one the title bar shows. It is set again only when the name
  changes (`WorkspaceView::retitle_window`). The screen reader names the window by it too, so
  every e2e golden's first line changed.
  - Test: `workspace::tests::bars::a_workspace_is_named_by_where_its_first_shell_is` (the
    window's accessible name).
- ✅ **"Server offline" is a menu** (2026-10-06, `.research/ux-audit-2026-10-05.md` §11 #22). The
  title bar's server readout said "Server unreachable" and did nothing when pressed, so the
  person had to find the setup to act on it.
  - **The words.** It now says "Server offline", the shorter, plainer word.
  - **The menu.** Pressed, it opens a menu under itself (`MenuKind::Server`) with "Retry now"
    (redial at once) and "Connect to another server" (the add panel on its server page). The
    app fills the menu (`WorkspaceView::set_server_menu`).
  - **The dot.** It stays muted, since `warn` is kept for what needs the person.
  - Tests: `workspace::tests::the_servers_word_is_said_at_the_title_bars_end` (opens the menu
    and retries), `workspace::tests::tiles::the_servers_word_says_what_it_costs`; e2e
    `server_unreachable`. Golden: `server-unreachable`.
- ✅ **The tailnet scan is a section with its own retry** (2026-10-06,
  `.research/ux-audit-2026-10-05.md` §11 #21). With nothing found, the add panel said
  "Nothing answered" in a loose line, and the only way to look again was to reopen the panel.
  - **The section.** It now always has its "On your tailnet" label over a card.
  - **With nothing to offer.** The card holds one status row. The row has a mark (turning
    while it looks, a crossed-out signal when Tailscale is off, a magnifier otherwise), words
    that say what was not found ("No Slopty server found yet", "No machine found yet"), and
    "Scan again", which is hidden while a scan runs.
  - **What to do next.** One line under the card says what to start there and to scan again.
  - Tests: `slopty-app` `tests::the_panel_names_its_host_and_the_server_the_tailnet_found`
    (the label, the row, the button, scanning again); e2e `gallery` (the first run and "Add a
    machine"). Goldens: `add-worker`, `first-run`, `first-run-dark`.
- ✅ **One face switch on an agent's tile** (2026-10-06, `.research/readiness-2026-10-06.md` §4
  #12). An orchestrator's tile had two toggles that each flipped a different pair: ⌘J and a
  header button between the thread and the TUI, and ⇧⌘J and a second button between the TUI and
  the board. The person had to remember which chord left which face. This entry replaces the
  toggle in "The conversation face" and the ⇧⌘J of "A project's board is a face of its
  orchestrator's tile".
  - **The switch.** An agent's faces are one list (`workspace::faces::Face`): Thread when its
    worker names its thread, Terminal always, Board while it orchestrates a project. The header
    shows one radio group, "Face", with an icon for each face, and the one on show sits on the
    selected wash. It is quiet at rest: the icons are in the muted ink, with no outline round
    the group, since every agent's tile carries it. Each icon is an icon button's square, 44 pt
    under touch. The trailing strip reserves room for every icon, so the hover swap still moves
    nothing. A shell no agent runs in has no switch. Neither does an agent with one face.
  - **The key.** ⌘J (`SwitchFace`) goes to the next face in the switch's order and round to
    the first. The palette lists it as "Switch thread, terminal or board". ⇧⌘J and the task
    agent's jump to its project's board are deleted. The palette's line for the project still
    goes there.
  - **The keyboard.** It goes with the face: the composer, the TUI or the board. Leaving the
    board gives it back to the face shown, even where that face was already the pick.
  - **For a screen reader.** The tile says the face it shows by the switch's own word
    (`Face::label`).
  - Tests: `workspace::tests::faces::the_switch_picks_the_face_and_a_plain_shell_has_none`,
    `workspace::tests::projects::the_orchestrators_tile_turns_to_its_board_and_opens_its_agents`
    (⌘J to the board, the switch back to the terminal),
    `workspace::tests::agent_tile` (the radio group and its words),
    `workspace::tests::tiles::the_readouts_give_way_to_the_controls_on_hover_and_nothing_moves`.
    The e2e project tests reach the board by ⌘J (`projects::to_board`). Golden: the
    `settings-keyboard` words.
- ⏳ **A thing's own menu opens by a right click or a long press** (2026-10-06,
  `.research/ux-audit-2026-10-05.md` §11 #6, I1). Only the terminal and the remote screen
  answered a right click. A Mac person right-clicks first, as in Finder, Linear and Things,
  and an iPad person holds.
  - **One press.** `kit::menu_press` opens a menu where the press landed, on a right click or
    a long press. The innermost thing pressed has it, so a message in a tile opens the
    message's menu, not the tile's. The menu is `kit::MenuPanel`, with its keyboard and
    typeahead.
  - **A thread's messages, first.** A message's menu holds Copy (which says "Copied" under it,
    as its copy does) and Quote in reply (its lines behind "> " in the draft, composer
    focused). A subagent's thread takes no messages, so it has no Quote in reply. Branch from
    here appears under the person's own messages where the thread can branch. Esc closes it,
    and the keyboard goes back where it was.
  - **A tile, a project, a machine.** A tile's navigator row and its header open the tile's
    menu: Open (from the navigator only), Rename, Fullscreen, Copy path where it has one, then
    Close tile set apart. A project's header offers New shell here and Fold or Unfold. A
    machine's row opens the menu its "…" opens, at the press. These are drawn by the bar's menu
    machinery (`MenuKind::Context`, `workspace/context_menus.rs`), hung at the press instead of
    under a button. Every row runs what a key or a palette line already runs.
  - **A folder's rows.** A press selects the row and offers Open, Rename or move…, Download…
    (Save to Files… on an iPhone or iPad), Open in the person's editor where that would open
    something, Copy path, and Move to Trash set apart (`folder/menu.rs`).
  - **The keyboard moves after the frame.** `kit::MenuPanel` took the keyboard while it was
    drawn. A view's own menu is built late in a frame, so the element that had the keyboard and
    the panel both claimed it in one frame (a screen reader is told of one focus a frame). The
    panel now takes it once the frame is drawn.
  - **A tab.** A tabbed column's tab opens its tile's menu with Move out of the column (its
    tile into a column of its own) and Close other tabs beside Close tile.
  - **Still to come.** Review files.
  - Tests: `conversation::thread::tests::composing::a_right_click_on_a_message_quotes_or_copies_it`,
    `conversation::thread::view::message_menu::tests::a_quote_marks_every_line_and_keeps_the_blank_ones`,
    `workspace::tests::context_menus::a_right_click_or_a_long_press_opens_a_things_own_menu`,
    `workspace::tests::context_menus::a_tabs_menu_moves_its_tile_out_of_the_column`,
    `workspace::tests::folders::a_right_click_on_a_row_offers_what_its_keys_do`.
- ✅ **An upload says how to stop it; a drop says where it lands** (2026-10-06,
  `.research/ux-audit-2026-10-05.md` §11 #15, I4).
  - **The pill.** An upload's pill in a tile's header stopped the upload when pressed, but
    looked like a readout. Under the pointer its ring now turns into "×", as Safari's download
    button does, and its tooltip and its name say "Stop upload". On touch, which has no hover,
    the "×" shows at rest.
  - **The drop.** A file dragged over a tile drew a 1 pt accent edge and nothing else, so the
    person could not tell where the file would go. While a drag of files is over a tile that
    takes them, a wash in `accent_fill` at `alpha::FAINT` sits over its body, inset by
    `spacing.sm` with `radii.md`. One centred line in it says where the files land, the way
    `drop_files` takes them:
    - "Upload to studio · ~/work" for a shell's directory or a folder;
    - "Attach to the message" for an agent's composer;
    - "Drop on <title>" for a remote window.
  - **Read by a screen reader.** The line is a status, so a screen reader says it as the drag
    arrives. The overlay is drawn only while the drag is over the tile (`files_over`), not
    hidden the rest of the time: a hidden element would still be in the accessibility tree.
  - Tests: `workspace::tests::remote::a_drop_on_a_shell_uploads_shows_progress_and_types_the_quoted_paths`;
    e2e `gallery::a_forwarded_port_and_an_upload_show_where_they_belong`. Golden:
    `transfers.txt`, where the pill's name changed.
- ✅ **A board card's facts read as facts, its one move as the solid** (2026-10-06,
  `.research/ux-audit-2026-10-05.md` A3). On a held card the facts ("PR #42", "1 of 4 checks
  fail", "Changes requested") and the actions under them ("Fix CI", "Address comments") looked
  alike, so the card did not say what was wrong or what to press.
  - **The facts.** The way to the target stays plain text parted by the quiet dot. A stage that
    failed (the verifier, the pull request's checks, the push) is the only one coloured: the
    error ink with its crossed circle (`Stage::failed`). A stage that holds without failing,
    such as changes requested or to-dos still open, stays in the text ink.
  - **The actions.** On a held card (a stage holds, or its verifier failed) the first action is
    the solid, the move that frees it. The rest stay secondary. A lane of cards ready to merge
    is not a column of solids, since nothing holds them.
  - Tests: `project::tests` (the pipeline's failed stages, a failed push). Golden:
    `project-live-lanes`.
- ✅ **"New project…" starts its orchestrator first** (2026-10-06,
  `.research/readiness-2026-10-06.md` N12). Starting a project needed a terminal that already
  ran an agent: open a terminal, start an agent in it, then "Start a project here". Away from
  one, the app only said to stand in a terminal.
  - **The steps.** The palette's "New project…" runs New agent's steps (agent, machine,
    folder) with the same last-first order and passing over a step that has one choice. The
    agent step lists only the agents that run in a terminal, because a project's orchestrator
    is one, and its field says so: Claude Code, whose TUI Slopty observes, and Codex, beside
    whose TUI Slopty is a second client (`agent_start::runs_in_terminal`). pi and ACP agents
    are driven over their protocols and have no terminal. The folder step offers no past
    sessions.
  - **Then the sheet.** The folder's pick starts the agent at once, with nothing said
    (`StartOrchestrator`). Once its tile is its terminal's, the "New project" sheet opens over
    it, filled in from that terminal as "Start a project here" fills it. A start that fails or
    a tile closed first drops the wait.
  - **The words.** Away from a terminal, the refusal now names the way that works: "A project
    is run by an agent in a terminal: start one with "New project…"".
  - Test: `workspace::tests::projects::new_project_starts_its_orchestrator_then_asks_for_the_project`.
- ✅ **A phone has one bar, the focused tile's** (2026-10-06, `.research/ux-audit-2026-10-05.md`
  §11 #16, P3). A phone stacked a navigation bar with the workspace's name over the tile's own
  header, so two rows of chrome stood over every screen of work.
  - **The bar.** On a phone the bar is now the focused tile's: its kind (or its agent's mark),
    its title, and how it is doing, at the size and weight the breadcrumb uses on a wider
    window. Its heading says all the agent's pill would ("…, Needs approval: Run ls"). With no
    tile focused it names the workspace.
  - **No header.** The tile draws no header of its own (`WorkspaceView::header_h` is 0), so
    its body runs up to the bar.
  - **The "…" menu.** It leads with the tile's own rows (`MenuGroup::Tile`): the agent's other
    faces ("Show thread", "Show terminal", "Show board"), Rename, Copy path and Close tile.
    There is no Fullscreen, since the tile fills the screen already. What "+" opens follows.
  - **The palette.** It is a sheet from the bottom, as iOS search in a toolbar is. Its field
    sits just above the keyboard, in a thumb's reach, and what it finds sits above the field.
    The sheet starts under the status bar with its top corners rounded, rises in on the
    sheet's pace and goes back down to leave.
  - **Still to come.** The drawer names the workspace, after the navigator's material change.
  - Tests: `workspace::tests::tiles::a_phone_bar_is_a_navigation_bar`,
    `workspace::tests::tiles::a_tile_that_fills_a_phone_offers_no_fullscreen`,
    `workspace::tests::faces::a_pill_says_the_state_and_a_phone_bar_says_it_all`,
    `workspace::tests::palette::the_palette_hangs_at_a_fifth_and_is_a_sheet_on_a_phone`; e2e
    `conversation::a_phone_opens_on_the_thread`. Golden: `thread-phone`.
- ✅ **The navigator stands on the system's glass** (2026-10-06, readiness #19; superseded the
  same day by "The design system starts from MonoCode's": the person ruled every surface solid,
  and the material is deleted). On a Mac the
  docked navigator lies over the system's sidebar material, as Finder's and Mail's sidebars do.
  The material blurs the desktop behind the window, takes a cast of its colour, and goes flat
  while the window is inactive. The rest of the frame stays opaque.
  - **How.** `slopty_platform::material::Glass` puts one `NSVisualEffectView` (sidebar
    material, behind the window) across the whole content view, under GPUI's view. It resizes
    with the window, so it never trails the navigator's width or motion by a frame. While it is
    there the window draws on a clear layer. The workspace root paints nothing, everything
    right of the navigator paints the canvas, and the navigator's ground is the canvas at
    `alpha::GLASS` (0.90).
  - **Light as dark.** The material leans light or dark as the theme does, not as the system
    does, so a light theme on a dark Mac stands on light glass.
  - **Contrast, by the numbers.** The navigator's text is drawn in `Surfaces::on_glass`: each
    tone moves toward black or white only as far as it takes to keep its opaque floors over the
    canvas at 0.90 over black and over white. Those floors are WCAG AA, APCA Lc 55 for
    secondary text and 45 for muted text, and each level a quarter past the one below. Any
    wallpaper's ground lies between those two. In the default themes dark moves at most 0.01
    OKLCH L and light about 0.05 (a muted grey a shade deeper), as Apple's sidebar labels go
    deeper on vibrancy. Only the text moves.
  - **When not.** There is no glass under Increase Contrast (which turns Reduce Transparency
    on), on iOS, in the self-test build (whose renders are compared pixel for pixel), or while
    the navigator floats or is hidden. Under Reduce Transparency AppKit draws the material
    solid, and the window is opaque again. There is no setting.
  - **Cost.** No dropped frames and no WindowServer time past the noise
    (`docs/MEASUREMENTS.md`, "what glass under the navigator costs"). Key-to-glass waits on an
    `on_frame_presented` in gpui-fast.
  - Tests: `slopty-theme` `text_on_glass_keeps_its_floors_over_any_wallpaper`,
    `glass_reads_as_the_chrome_in_light_and_dark`; `slopty-ui`
    `workspace::navigator::tests::on_glass_the_navigator_takes_the_glass_ground_and_tones`;
    `slopty-platform` main-thread test
    `the_glass_lies_under_the_whole_window_through_any_resize` (the view's frame and
    autoresizing mask through resizes, its order under GPUI's view, its appearance).
- ✅ **A phone's drawer names the workspace** (2026-10-06, `.research/ux-audit-2026-10-05.md`
  P3, the rest of "a phone has one bar"). A phone's bar now names the focused tile, so the
  workspace's name had nowhere left on screen but the tone of its row under *Workspaces*.
  - **Where.** It goes in the drawer's top row. That row is the bar's height and stood empty
    on a phone, since no traffic lights or toggle live there. The name sits beside the bar,
    which still shows at the drawer's edge naming the tile, so opening the drawer reads as one
    level up: the workspace, then its tiles. It takes the bar's title size and weight and
    lines up with the section headings below it. As a Heading it is what a screen reader
    meets first in the drawer.
  - **Only a name.** Switching stays with *Workspaces* below it, and the counts stay on those
    rows. A desktop's navigator leaves its top row to the window's controls, and there its
    title bar names the workspace.
  - Test: `workspace::tests::nav_rows::a_phone_drawer_names_the_workspace`. Golden:
    `ios-phone-navigator`, retaken on the simulator.
- ✅ **A review file has its own menu** (2026-10-06, `.research/ux-audit-2026-10-05.md` I1,
  the last of "context menus everywhere"). A right click, or a long press under a finger, on a
  file's row in the list or on its head in the diff opens the file's menu where the press
  landed: Keep, Open, Open in the person's editor, Copy path, then Revert set apart last.
  - **Revert says what it does.** The review path has no undo for a put-back: the file goes back
    on the worker and leaves the review. So the row names the file and the point it goes back to
    ("Revert lib.rs to before the last turn"), and that is all the warning it needs before it
    acts. The head's own Revert sits in the file's head, whose name is beside it. Keep and Revert
    show only on a thread's own review, and only while nothing is already on its way for that
    file.
  - **Paths from the root.** A review names its files from the repository's root, which need not
    be the thread's folder. The tile asks the repository's status once as it opens, beside its
    pull request, and takes the root from it. Until the root is known, Open and the editor are
    left out, and the copy row says "Copy path in the repository" and copies just that. Open
    opens the file in a tile on the review's machine (`ReviewEvent::OpenFile`).
  - **A folder's review is heard.** A folder's changes tile was never subscribed, so a press on
    who wrote one of its lines went nowhere. Its events now go through the same handler as a
    thread's review.
  - Tests: `review::tests::a_files_menu_keeps_opens_copies_and_says_what_revert_does`,
    `workspace::tests::review_tile::a_reviews_open_file_opens_it_on_its_machine`,
    `workspace::tests::review_tile::a_folders_review_opens_the_thread_that_wrote_a_line` (fails
    without the subscription).
- ✅ **The drawer's title takes the panel title role** (2026-10-06, amends "A phone's drawer
  names the workspace"). On a simulator review it read as one more row. Set at the bar's
  13 pt medium, it was smaller than the drawer's own rows, which a finger's roles set a step
  larger. It now takes `TypeRoles::panel_title` (the strong weight, a step above the rows), so
  it stands as the drawer's heading. Test: `a_phone_drawer_names_the_workspace` checks its
  line height. Golden: `ios-phone-navigator`, retaken on the simulator.
- ✅ **A phone bar's title takes the panel title role too** (2026-10-06, amends "A phone has one
  bar, the focused tile's"). It had the same flaw as the drawer's title: at 13 pt medium it sat
  below the 17 pt rows a finger's roles set beside it. It now takes `TypeRoles::panel_title`,
  as an iOS navigation bar's title stands above its rows, and the rename field that takes its
  place does too, so renaming moves nothing. Test:
  `workspace::tests::tiles::a_phone_bar_is_a_navigation_bar` checks its line height. Golden:
  `thread-phone`; the iOS phone goldens that show the bar are retaken on the simulator.
- ✅ **An agent's tile is announced by its agent** (2026-10-06, from the simulator review of the
  phone bar). The spoken heading put the tile's kind first, so a Claude Code tile was read as
  "terminal Fix the login", which is not what the person sees: the tile wears the agent's
  mark. Where a tile wears an agent's mark (`kind_icon`'s rule), the heading now leads with
  the agent's name (`agent_label`), else with the kind. The lead is dropped where the title
  already says it, so a twin is "Claude Code 2" and not "Claude Code Claude Code 2". Only what
  is spoken changes: twins are numbered, and tiles grouped, by their kind
  (`WorkspaceView::spoken_kind`, `tile::spoken_heading`). Tests:
  `workspace::tests::tiles::an_agents_tile_is_announced_by_its_agent`,
  `a_heading_never_says_its_agent_twice`. The agent goldens' words moved with it.
- ✅ **A phone's titles are inline navigation titles** (2026-10-06, amends the two panel title
  entries above). In the panel title's role the bar's title came out at about 2.5 times its
  16 pt icon: a large title squeezed into a 44 pt bar, cut short early. Both the bar's title and
  the drawer's now take `titlebar::phone_title_role`: the task title's size (17/22 on touch)
  in the strong weight, which is iOS's 17 pt semibold inline title. They stand a step above
  the 17 pt rows by weight, not by size. Tests: `a_phone_bar_is_a_navigation_bar`,
  `a_phone_drawer_names_the_workspace`. Golden: `thread-phone`; the iOS phone goldens are
  retaken on the simulator.
- ✅ **State is a glyph; colour is a hue per state and an identity per machine** (2026-10-06,
  `.research/status-color-2026-10-06.md`, checkpoint 1 of its build order; amends "Colour goes
  to what needs the person" and "The chrome's icons are SF Symbols", and overrules the earlier
  studies where its §8 says so). The person found the status marks plain and asked for colour
  with elegance. Every reference (Linear, the Codex app, T3 Code, Warp, Raycast) gives a state
  one glyph in one hue and keeps the words beside it neutral; none draws a state as a dot.
  - **One family of circles.** Needs you is `exclamationmark.circle.fill` in `warn_fill`,
    failed `xmark.circle.fill` in `error_fill`, a finish not yet seen
    `checkmark.circle.fill` in `success_fill` (it fades in over `Pace::Settle`, at once under
    Reduce Motion), working the stepped spokes in `working_fill` (a braille cell since
    2026-10-06, "The working mark is a braille cell"). Waiting is a dashed ring
    and idle, where a mark is needed, an empty ring, both muted. Away keeps `wifi.slash`.
    Rows still show nothing at rest.
  - **The rings are ours** (`icons::Ring`: `Empty`, `Dashed`, `Pie(share)`). SF's
    `circle.dashed` drew gaps under a device pixel at 1x and read as a plain circle. The
    painter puts the outer edge on the device grid and strokes a whole number of device
    pixels, one at 1x. A dashed ring's six gaps are two pixels or more there.
  - **The board speaks the same family** (`icons::Phase`, named so because the project model
    already has a `Stage`). Up next is the empty ring, verifying a pie of its share (half
    while unknown) in `working_fill`, ready to merge `checkmark.circle` in `success_fill`,
    merged the filled check in `merged_fill`, violet as GitHub's merged is. A lane head leads
    with its lane's glyph at the small size. A card follows its agent once one is on it.
  - **Two hues and an identity set join the theme.** `working`/`working_fill` (OKLCH hue 250)
    and `merged`/`merged_fill` (300), each a text step and a fill step, plus `identity`, eight
    hues for a machine's or a project's own glyph (identity colour itself is the next
    checkpoint). Every fill sits at one lightness per mode: 0.72 in dark, 0.58 to 0.64 in
    light. Amber keeps its own (OKLCH yellow at 0.6 is olive), and so does red, since at the
    others' higher lightness red turns pink and it stays where the colour-blind pairs pinned
    it. The identity set leaves out red, amber, green, blue and violet. Each of its hues
    stands 20 degrees or more from every status hue, so the spec's purple moved to a magenta
    at 325 (it sat 10 from merged) and indigo to 272.
  - **A glyph carries the hue; words stay neutral.** `Status::ink` is the fill step and
    `Status::word` a neutral tone, failure alone keeping `error`. Inside a thread's stream the
    spinner stays muted (`Status::quiet_ink`). The bell's count, when only reviews are unread,
    is the green fill, not the accent.
  - **Dots are gone.** The navigator's unseen dot, the rollups', the stream health's, the
    unreachable server's and the lane heads' are glyphs now. A file's unsaved dot is the word
    "Edited" after its name (a label in `text_muted`, as macOS's "— Edited"), and a
    miniature's state says "Edited" too.
  - Tests: theme `status_fills_share_one_lightness`, `identity_keeps_clear_of_the_states`,
    `a_status_fill_reads_as_a_mark`, `vision::working_merged_and_done_marks_stay_apart`;
    icons `each_status_has_its_own_mark`,
    `a_status_wears_its_fill_and_its_word_stays_neutral`,
    `ring::tests::a_ring_is_one_device_pixel_at_1x`, `a_dashed_ring_shows_its_gaps_at_1x`,
    `the_pie_fills_its_share_clockwise_from_twelve`; the kit lints
    `a_state_is_a_glyph_not_a_dot` (a filled, childless `rounded_full` box anywhere in the
    chrome, the two switch knobs and the brand mark's dots ruled out of it) and
    `a_status_glyph_wears_its_fill` (no status glyph handed a text tone);
    `workspace::tests::bodies::edited_follows_the_title`,
    `frame::an_unseen_check_marks_a_finished_tile_until_it_is_looked_at`.
- ✅ **Light and dark only; the Increase Contrast variant is deleted** (2026-10-06, ruled with the
  status-colour pass). Supersedes "Increase Contrast derives its own chrome" and the Increase
  Contrast clauses of every entry above that carries one: the focused tile's line, the
  calm-and-green pass, the frame's focus line, focus as a line, the elevations, stage 3 of the
  design-systems study, the APCA floor, colour-blind pairs, a field on its page, a focused
  field, the glass under the navigator, "A control's outline reads 3:1" (3:1 stands; the
  level over it is gone) and the hairline entry (`Theme::hair` and its full point).
  - **What goes.** `slopty_theme::Contrast`, `Theme::contrast`, `Theme::set_back`,
    `Theme::hair` and the `HAIR_CONTRAST` stroke. `Surfaces::derive` takes the content alone
    and `field_focus` no contrast. The kit's ringed-in-dark branches (cards, fields, the
    search field, the segmented thumb), the overview's flush ring, the progress track's
    louder tone, `slopty_platform::motion::watch_increase_contrast` and
    `increase_contrast`, the app's watch, the self-test's `Command::Contrast` and the
    `workspace-navigator-contrast` golden.
  - **The hairline is a constant.** `kit::hair(theme)` is `kit::HAIR`, `kit::hair_painted`
    takes the scale alone, and `kit::rule`/`rule_v` take the tint alone; each was handed a
    theme only to read the contrast.
  - **What stays.** The terminal's `minimum_contrast` (the program's colours lifted to read,
    ghostty's setting) and its per-frame memo. Under the system's Reduce Transparency AppKit
    still draws the glass solid by itself.
  - Tests: the theme's contrast-looping tests read the one look; the tests of the variant
    itself (`increase_contrast_raises_text_and_hairlines`,
    `the_chrome_follows_the_theme_s_contrast`, the hairline's point and the stage's black)
    went with it, as did the app's `increase_contrast_derives_the_chrome_for_it`.
- ✅ **The navigator's top row ends in Search and New agent, and its filter waits hidden**
  (2026-10-06, the person's review: the filter capsule under the lights still looked out of
  place, where other apps keep icon-only buttons such as the sidebar toggle, new thread and
  search). Amends "The navigator's top is the lights, and the filter is the first row under
  them". Apple's Notes and Mail keep a sidebar's actions on its top row and show search when
  asked; Zed's and Linear's sidebars open with their list.
  - **The row.** The lights, the toggle where it always was, then at the row's trailing end
    inside the panel "Search" (`magnifyingglass`) and "New agent" (`square.and.pencil`),
    each the toggle's size and target, each with its hint and keys. "New agent" runs "New
    agent…", the one way to start one; the bar's "+" keeps the rest.
  - **The filter.** Hidden at rest, so the list is the panel's first row. "Search", ⌘F while
    the navigator holds the keyboard (`filter_navigator_from_a_row`, in the `Navigator` key
    context, so a tile's ⌘F keeps its find), ⌘⇧E, or typing while a row holds the keyboard
    shows it with the keyboard in it. Typing goes on after what it already holds, as a
    Finder list's type-select starts a search. It fades in over `Pace::Settle`, at once
    under Reduce Motion. Only its opacity moves: a height animation made the first clicks
    miss. Esc empties and hides it, and so does the keyboard leaving it empty. That is read
    as the frame is drawn, because the field's blur does not always arrive when the app
    moves the focus itself. A query or a scope keeps it shown. A phone's drawer keeps it
    always, and its row has neither button.
  - **The bar.** With the navigator hidden, the bar's leading cluster is the toggle,
    "Search" (`bar-search`, which opens the palette, the one search left on screen) and
    "New agent", so neither goes away with the navigator.
  - Tests: `workspace::tests::nav_rows::the_filter_waits_hidden_until_search_its_keys_or_typing`,
    `tab_strip::the_navigator_is_the_windows_height_and_the_bar_starts_at_its_edge`, and
    `keymap::tests::the_files_chord_wins_over_a_deeper_default`, which counts the navigator's ⌘F.

- ✅ **The request is one card with one row of answers; the composer's foot keeps four things**
  (2026-10-06, `.research/elegance-icons-2026-10-06.md` §3.6 and §5 items 3, 4 and 6). Supersedes the
  "From now on" section of "A request is one decision" and "The way to the terminal stands on
  the request's title line".
  - **One foot row.** Deny▾ and Allow stand at the right as before. A grant that lasts leads
    the same row from the left: its words on a quiet button, its reach after it in words at
    the metadata size, wrapping rather than cut. The "From now on" heading is gone; a screen
    reader hears the group as "Grants that last".
  - **Answering in the terminal waits behind the deny's chevron**, last in "Other ways to
    deny" after a hairline. It was a third answer on the title's line. With nothing to answer
    here it is still the card's one answer, and with no plain deny to hang it from it leads
    the row.
  - **The edits are said once.** The composer's changes chip says what the turn changed,
    whether a request is on show or not, and opens the review. Amended 2026-10-06: the counts
    rode a request's card head while it showed, which read as the permission's own figures
    ("Allow Bash? +2 −1"); a permission's card asks only its question. The tray's "Edits" row
    shows only where there is no composer (a subagent's thread). The chip is the one door to
    the review, so it never hangs on the counts: a turn whose edits counted no line (a file
    Claude Code created whole, whose result carries an empty diff) shows a pencil and the
    file's name, or how many files.
  - **The composer's foot** is "+", the model, then the place, the meter and send. The model
    is its name alone: the agent's mark leads the tile's header. The mode and the effort show
    only when the agent is not at its default ("default" by id or name), and a mode or effort
    it names none of goes unsaid; the "+" menu holds "Mode" and "Effort" under a hairline, so
    their switches are always one press away. The meter is its ring alone under half full; from
    50 % its share is in figures too, and from 80 % it takes the warning tone.
  - **A new thread's empty state wears the conversation glyph**, not the agent's mark: its
    title names the agent.
  - **The review wears no agent mark.** "Review with Claude Code", the band of findings and
    what came name the agent in words; a finding and a person's comment share the comment
    glyph, the finding's bold first line saying whose it is. A tile too narrow for the words
    shows the conversation glyph with the words in its hint. An agent's mark now leads only
    the navigator's row, the tile's header and the new-agent picker.
  - Tests: `conversation::thread::tests::doors::an_approval_is_allow_and_deny_with_the_rest_set_apart`,
    `review::tests::the_agents_findings_become_comments_and_notes_sent_as_one`,
    `conversation::thread::tests::face::{the_default_mode_goes_unsaid_and_the_plus_menu_switches_it,
    the_meter_says_its_share_from_half_full, a_requests_card_carries_the_turns_edits_once,
    a_created_file_alone_still_opens_the_review}`.
- ✅ **How surfaces adapt to their room** (2026-10-06, the person asked whether the UI was
  responsive after a thread beside a board was cut at the window's edge;
  `.research/responsive-2026-10-06.md`). Every surface lays out for the room its container
  gives it, never for the window's size or a golden's. Each view had kept its own thresholds
  (280, 560, 700, 720, 900, 960), and only three views were ever told their width.
  - **Room.** `kit::Room` is `Narrow` under 420 pt, `Regular` to 720 pt and `Wide` from
    there. Text decides the edges, so they are written at the default 13 pt chrome and scale
    with the chrome size setting. The workspace hands each tile its room from its placed
    width. A surface that is not a tile (a popover, an overlay, a sheet) asks its own
    container through `kit::room_query`, GPUI's `container_query`.
  - **Priority rows.** A row of a title, facts and controls is a `kit::priority_row`. Each
    item is laid out at its own width. While the row overflows, the item of the lowest
    `kit::Priority` leaves, the trailing one first among equals, as `NSToolbar`'s
    `visibilityPriority` has it. `ESSENTIAL` never leaves. The title has a floor. Items under
    the title's priority (`MEDIUM` by default) leave before it narrows at all, and the rest
    stay while it narrows to its floor. What left is named in `kit::Dropped` for the row's
    menu, whose button shows only while something has left. Controls hidden at rest are an
    overlay at the trailing end and keep no room. Measured at about 0.7 µs per item against
    a flex row (`docs/MEASUREMENTS.md`, "what a priority row costs").
  - **Overflow discipline.** Words that do not fit end in an ellipsis (a name at its end, a
    path at its start) or fade, and are never clipped mid-glyph. An ellipsis is the text's
    own: a `ChromeText` lays out its own words, so a `truncate` around it does nothing. A
    chip of words that never shrinks has a bound or stands in a priority row.
  - Tests: `kit::priority::tests::{a_priority_row_drops_from_the_trailing_low_end_and_keeps_the_title_floor,
    a_row_lays_out_what_stays_and_names_what_left}`, `kit::room::tests::{a_room_is_read_from_its_width_at_the_chromes_size,
    a_sheet_reads_its_room_from_its_container}`, and the kit lints
    `no_words_are_cut_mid_glyph`, `no_ellipsis_is_asked_around_chrome_text` and
    `a_chip_of_words_has_a_bound`, each with its self-test. The files that do not obey yet
    are listed in each lint as awaiting their owner's next change.

- ✅ **The tile header, the tabs, the board's header, the breadcrumb and the notices fit their
  room** (2026-10-06, `.research/responsive-2026-10-06.md`, defects 1, 3, 4, 9 and 10). Each had
  a row of chips that never shrank beside a title that took all the shrinking, so a narrow
  column showed chips and no name.
  - **A tile's header** is one priority row in place of the 480 pt stopgap. The lead glyph and
    the title stay. The title keeps at least a third of the header. The worktree leaves
    first, then the worker, then the pull request. The state, an upload and the
    finished mark stay longest. At rest a focused tile with no readouts shows its controls.
    Otherwise they are an overlay on the header's ground that shows on hover over the
    readouts it hides, and keeps no room while hidden.
  - **A tile showing a thread** says where its agent works after the title: the checkout by
    its folder's name, then the branch with its glyph. Each is a press away from the commit
    sheet when the checkout is a repository. They replace the shell's directory beside the
    title, and the composer's foot no longer says them. They stay while a long title narrows
    to its floor. Then the branch leaves, then the checkout, and the pull request last. What a
    worktree's chip says already is left out. The header reads them from the worker's table,
    as the workspace keeps it, and never from the thread view, whose working mark would
    otherwise redraw the whole workspace at every step.
  - **Containment.** `workspace::tests::rooms::nothing_escapes_its_tile_at_any_room` lays out
    a shell, an agent's shell with its chips, its thread, a board, a file, a folder and a column
    of three tabs at 280, 312, 360, 420, 560 and 720 pt. No node of a tile's accessibility
    tree may reach past the tile's edges.
  - **Tabs** in a narrow column shrink to their mark and a few letters, and the row scrolls
    sideways, keeping the tab on show in view.
  - **The board's header** keeps its name and its three controls. The running count leaves
    first. Then the name narrows to a floor (8 em), and only then does the progress leave,
    since the bar under the name says it too. The dot between the two goes with the count.
  - **The breadcrumb** keeps the workspace and "+". The worker leaves first, then the
    checkout. The branch is its title and ends in an ellipsis.
  - **The title bar's notices** are as wide as they are where the bar has room. Where it has
    not, the newest stays whole (and narrows with an ellipsis only once the older are gone).
    The older go behind a count ("+1") that opens them under it. None is cut off unseen. The
    same holds for a tile's own notices.
  - **Facts parted by dots** are a `kit::facts_row`. Facts are set into lines as words are,
    and a dot is drawn only between two facts on one line, so no line starts or ends in a dot.
    A board card's pipeline stages are one, which ended a wrapped line in a dot before.
  - `kit::priority_row` measures its items before layout, so a row sized by its content
    (`fit_content`) has its width in the same frame. It gives its items the text style it was
    given, as a `div` does.
  - Tests: `kit::facts::tests::{facts_set_into_lines_with_dots_only_between_facts_on_a_line,
    a_wrapped_fact_row_never_ends_or_starts_a_line_with_a_separator}`,
    `workspace::tests::chrome::the_notices_the_bar_has_no_room_for_go_behind_a_count_that_opens_them`,
    `workspace::tests::tiles::{a_narrow_header_keeps_its_title_and_shortens_its_place,
    the_readouts_give_way_to_the_controls_on_hover_and_nothing_moves}`. The mid-glyph lint now
    waits only on `terminal/view.rs` (the link preview).

- ✅ **The composer, the answers, the review's bars and the symbols list fit their room**
  (2026-10-06, `.research/responsive-2026-10-06.md`, defects 2, 5, 11 and 12). Each of these
  had been laid out for a roomy column, and in a column beside a board (312 pt) Send, the
  primary answer, the span switch's words and the symbols list ran past the tile's edge.
  - **The composer's foot** is one `kit::priority_row`. The "+" and the send never leave. The
    rest leave the least needed first: the handoff, the place, the agent's screen, the pull
    request, the mode, effort and background work, the changes, "Interrupt and send", the
    meter, then the model. What left waits in the "+" menu after a separator and does what its
    chip does there. The place is whole or gone, never faded to a glyph. When an edited
    file's lines are uncounted, the changes chip names the file under a pencil, so the review
    stays one click away.
  - **A request's answers wrap.** Where they do not fit beside the standing grants, they take
    a line of their own and wrap among themselves, ending at the trailing edge with the one
    solid last. An answer is never wider than the card: a long one wraps its words, which the
    person reads whole before choosing.
  - **The review's scope bar and foot** are priority rows at the shaped widths of their words,
    in place of a guess of 0.55 em a letter. In the bar, the span on show and the refresh
    never leave; the pull request, the agent's review, the other spans and Commit leave in
    that order and wait behind "More". In the foot the send never leaves; "Mark reviewed",
    then "Add to message", go behind "More".
  - **The symbols list** takes its width from the file through `kit::room_query`: 24 em of the
    chrome where the file has room, the file's width less its margins where it has not.
  - Tests: `conversation::thread::tests::face::{a_narrow_foot_keeps_send_and_hands_the_rest_to_the_plus,
    a_created_file_alone_still_opens_the_review}`,
    `conversation::thread::tests::doors::long_answers_wrap_inside_a_narrow_card`,
    `review::tests::{a_narrow_foot_keeps_the_send_and_folds_the_rest,
    a_narrow_scope_bar_keeps_the_span_on_show}` and
    `file::tests::editing::the_symbols_list_keeps_inside_a_narrow_file`.

- ✅ **An empty state is a title, not a footnote** (2026-10-06,
  `.research/elegance-icons-2026-10-06.md` §5 item 10). `kit::notice` set what is so at 12 pt
  in `text_secondary` under a 28 pt mark, a footnote to its own glyph.
  - **The title** is the task title role (14/20 at the medium weight) in the text's ink. **The
    detail** is the chrome role in `text_secondary`. Both keep to a measure of 26 ems
    (`kit::NOTICE_MEASURE`) where the tile is wide, and to the tile less its margins where it
    is narrow, wrapping and never cut, so the block fits any room.
  - **One next step, where there is an obvious one** (`kit::notice_action`, a secondary
    button under the words). An empty span of a review offers the widest span of its kind:
    "Show all turns" for a thread, "Show the whole branch" for a folder. The widest span offers
    none. An empty folder offers "New shell here", a shell on its machine at the folder. A new
    thread offers none, since its composer is right there. The review's reading,
    absent and empty states are notices now, with the diff glyph, where they were a muted line.
  - Tests: `review::tests::{an_empty_review_names_its_span, an_empty_span_offers_the_widest}`
    (the latter at 312 pt) and `workspace::tests::folders::an_empty_folder_offers_a_shell_in_it`. Goldens: `agent-marks*`, `project-lanes*`, `file-too-large` and
    every tile that says it has nothing to show.

- ✅ **Section heads, a thread's turns, and the reading measure** (2026-10-06,
  `.research/elegance-icons-2026-10-06.md` §5 items 2 and 12).
  - **A section's head reads as a group's head.** `kit::label` was 12 pt at the regular
    weight in `text_muted`, the level of the facts on the rows under it, so every section ran
    into the next. It is 12 pt at the medium weight in `text_secondary` now, as Linear's group
    heads are. The strong weight stays for one thing per region, so a head never rivals the
    names under it. The palette's, the pickers', the navigator's and the empty workspace's
    heads all take it.
  - **A turn reads as one block.** A person's message opens a turn a large step
    (`spacing.lg`) under the last. What answers it follows at the small step (`spacing.sm`),
    and inside the answer a run of calls keeps its tight steps and its paragraph's step round
    prose. The bubble's actions (the time, the copy, the branch) used to keep a hidden line
    under the bubble, which pushed the answer away from its question. Under a pointer they wait
    beside the bubble's foot now. Under a finger they always show, so they keep their line.
  - **The thread's gutter follows the tile's room** (`kit::Room`) in place of its own 560 pt
    edge: 48 pt in a wide tile, 24 in a regular one and 16 in a narrow one. The gutter gives
    way before the words do. The agent's screen chip keeps its words everywhere but in a narrow
    tile.
  - **The review reads its room.** The file list sits beside the diff in a wide tile, as it
    did from 720 pt. The diff shows both sides when what the list leaves is itself a wide room
    (960 pt of tile at the default chrome, as before; superseded 2026-10-07, the diff is
    unified at every width). Both edges now move with the chrome size,
    so no fourth room was needed.
  - **A Markdown file reads on the thread's measure.** The preview centres its lines on the
    thread's 736 pt column, so a wide tile keeps lines the eye can follow back, and a long
    file reads as an agent's answer does.
  - **The composer stops restating where.** The tile's header says the thread's checkout and
    branch, so the composer's foot drops its place chip. "Commit…" stays in the "+" menu
    whenever the thread works in a repository, and no longer only when the foot ran out of
    room.
  - Tests: `conversation::thread::tests::steps::{a_question_and_its_answer_read_as_one_turn,
    the_column_s_gutter_follows_the_tile_s_room}` and
    `file::tests::reading::the_preview_keeps_to_the_reading_measure`. Goldens: `thread*`,
    `palette*`, `workspace-navigator*`, `agent-needs-you-navigator`, `project-*`,
    `empty-workspace` and the wide-reading golden.

- ✅ **An icon takes its words' size, weight and tier, and git is drawn in Octicons**
  (2026-10-06, `.research/elegance-icons-2026-10-06.md` §3.1–3.4, build step 2). (Superseded
  2026-10-07 by "The chrome's icons are Tabler's, and a file's are Material's": the tier
  stays, the weights, the 12.5 pt floor and the Octicons go.) SF Symbols
  was never the weak part; how it was configured was. Ninety of the chrome's icons were drawn
  at 12 pt beside 13 pt words, in `text_muted` beside titles in `text`, always at the regular
  weight, and the wide ones shrunk to fit a 14 pt slot. Together that cost an icon about a
  third of the visual weight of the words beside it at 1x.
  - **The floor.** Under about 12.25 pt SF draws a smaller design, a fifth narrower for the
    same stroke (`docs/MEASUREMENTS.md`, "SF Symbols' smaller design"). No symbol is drawn
    under `icons::SYMBOL_FLOOR` (12.5 pt) except Apple's own disclosure chevrons.
    `IconSize::Inline` beside a row's facts draws at max(small, 12.5) in its 14 pt slot.
    `IconSize::Lead` (formerly `Large`) draws at the chrome's size in a 16 pt slot.
  - **Size and weight follow the words.** `icons::beside(theme, mark, role, ink)` and
    `Drawn::beside` take a `TypeRole` and give its point size (floored) in a slot that grows
    with it, so a finger's 17 pt rows get a 17 pt glyph. The weight is regular beside 400,
    medium beside 500 and semibold beside 600 (`icons::weight_beside`), as the HIG matches
    them. Every row lead in the palette, the pickers, the navigator and the tile headers is
    the lead size. A group's machine or project glyph is drawn at the medium weight beside
    its medium name, and a tile header's at the medium weight while its title is.
    `kit::square_icon` is the lead size at the medium weight.
  - **One tier under, never two.** A lead is `text_secondary` at rest and the title's `text`
    when chosen. Muted is for away and disabled. The tile header's lead wears its title's
    tier, the focused title's `text` and any other's muted, so it never outshines the title
    it leads. A folder's rows follow suit: no file is a tier under the folder beside it.
  - **Wide symbols keep their size.** `fitted` judges a symbol on its ink, not on the OS's
    padded image (about 2 px each side), and lets it reach an eighth past its slot
    (`FIT_ROOM`, 18 of 16) before drawing it smaller. The server rack, the folder and the
    window keep their words' size in a lead.
  - **Git is GitHub's Octicons** (MIT, `crates/slopty-ui/assets/git/` with
    `LICENSE-octicons` and `NOTICE`): branch, pull request (open, draft, closed), merge,
    commit and repository, as `icons::GitGlyph` (`Mark::Git`). SF has no git vocabulary, and
    its `arrow.triangle.pull` read as a lone bent arrow. They are drawn the way the agents'
    marks are, filled from their outlines by Core Graphics into the same masks at device
    pixels. Their 16-unit grid spans 16/14 of the words' point size, since GitHub sets them
    beside 14 px text. That stands them at SF's height beside the same words, with strokes
    between SF's regular and medium. At 1x each is within 0.02 of SF's crispest symbol or
    crisper, and most have more whole pixels (`docs/MEASUREMENTS.md`, "git glyphs at 1x"). A pull request's glyph wears its
    state in its fill step (`GitGlyph::state_ink`): open green, merged violet, closed red, a
    draft grey. Its words keep their review's tone. A branch, a commit and a repository take
    their words' tier. Octicons stays to git only: as the whole set, it would read as GitHub
    Desktop and lose the system's weight matching.
  - **A machine wears its form** (`icons::machine(Option<Form>)`): `laptopcomputer` for a
    laptop, `display` for a desktop, `server.rack` for a server or one that has not said. The
    worker says its form (`WorkerCaps::form`). It shows in the navigator's machine heads and
    rail, the tile header's worker, the breadcrumb, the away tile and the palette's workers.
    A repository group is the Octicons repo, not the diff glyph. A project group is a folder,
    since a project is a folder first. A workspace row on the phone has no glyph, and its
    empty slot keeps the names' edge.
  - **Lints as tests:** `kit::tests::a_symbol_is_never_drawn_under_12_5` (no icon slot sized
    to `small()` or `caption()`, a check inside its filled box excepted) and
    `kit::tests::a_lead_is_never_muted_at_rest`, beside
    `icons::tests::{no_icon_size_draws_a_symbol_under_12_5,
    an_icon_takes_its_words_size_and_weight, a_leads_wide_symbol_keeps_its_size}` and
    `icons::git::tests::{every_git_glyph_reads, a_git_glyph_is_drawn_at_its_words_size,
    the_git_glyphs_are_crisp_at_1x}`.

- ✅ **The working mark is a braille cell** (2026-10-06). Overrules the twelve spokes of "The
  chrome's icons are SF Symbols" and "State is a glyph". The person found the spokes ugly and
  dated: Apple's activity indicator from the Aqua years, and pale, because eleven of its twelve
  spokes were faded copies of the hue. The bar is MonoCode and T3 Code.
  - **What they do** (read in their sources).
    - MonoCode's sidebar marks a working agent with braille frames (⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏) every 80 ms
      in its accent. A running tool is a dashed ring turning slowly.
    - T3 Code's sidebar shows a still dashed circle with the word "Working". Its generic
      spinner is an open arc that turns only where motion is allowed.
  - **The cell.** Six dots in two columns of three, drawn by us (`icons::Spinner`). Three dots
    lit go round the ring, and a fourth lights on the step between as the head moves on: twelve
    frames a turn, one a step of the existing spin clock (`SPIN_STEP`, a twelfth of a second).
    So nothing draws more often than the spokes did, and the frame and wakeup budgets hold
    unchanged ("a working mark that steps").
    - Each dot is a whole number of device pixels on a whole-pixel pitch: 2 px dots, 4 px apart
      at a row's 14 pt slot at 1x, and 5 and 8 at 2x, so every dot is sharp and alike.
    - The dots at rest keep 22 % of the ink, a faint track, so a column of working rows holds
      one shape while the lit dots move.
    - The first frame is ⠋, MonoCode's first, never a column of three, which would read as a
      menu's "⋮". It is the frame Reduce Motion stands on, and the one a pinned clock draws.
    - The lit dots are the working hue's mark step (`working_fill`) at full ink. No dot is a
      faded copy, so the mark reads as meant in both themes, not as a pastel.
    - Under Reduce Motion it stands on its first frame and breathes, as before.
  - **Rejected.**
    - An open arc over a hairline track looked the most modern still. It only looks right
      turning smoothly, though, which needs a frame at the display's rate for every working
      row. Stepped at 12 a second, it jumps 30° at a time, the very thing that retired the
      ring before the spokes. A braille cell is made to step.
    - The still dashed ring stays waiting's (`Status::Running`), as in both references. Using
      it for working too would leave working and waiting alike.
    - MonoCode's bare cell (unlit dots absent) changed its outline from frame to frame at 1x,
      and its column frames read as "⋮". The track and the first frame fix that.
  - Specimens: `.research/icon-specimen/app` (`cargo run --release -- spin <dir>`) renders the
    spokes, five cells, the arc and the dashed ring beside a "#42" pull request and an agent's
    mark, in light and dark at 1x and 2x.
  - Tests: `icons::tests::{the_working_cell_goes_round_in_twelve_frames,
    the_working_cell_sits_on_whole_pixels}`, beside the spin clock's own.
- ✅ **Space, headings and tone part the review and the thread** (2026-10-06,
  `.research/status-color-2026-10-06.md` §7.2, the divider inventory; amends "Fades are per
  pixel, with the surface outside them"). The person found the chrome ruled into boxes. The
  references part a surface by space, a heading's weight and a step of tone, and keep a rule
  for a real seam. So the hairlines inside a thread and a review go.
  - **The thread.** The rule under the header is gone. The turns fade under the header while
    some lie above it (`gpui::edge_fade`, top `lg`, hidden by the list), as macOS 26's scroll
    edge does. The trail's, the aside's and the composer menu's rules are gone, and so is the
    composer's rule when it is capped. The aside's head words take the medium weight and its
    icon the secondary tier. A commit's three rules become `md` of space under its head. A
    tucked tray's activity sits on the band (`kit::inset`), and an open one is raised.
  - **Diffs.** The side-by-side view parts its two columns by `xxs` of space, not a vertical
    rule, and a hunk's head sits on the band instead of between two rules. A file's notice
    band loses its rule.
  - **The review.** The scope bar's and the foot's rules give way to the diff list's fade at
    both ends (`lg`). The file list stands on the panel's tone, not behind a vertical rule. A
    file is no longer framed: its head row sits on the band, rounded, with `xs` under it. A
    comment's rule becomes space, a comment has no border, and a draft is raised.
  - **What stays**, as §7.2 rules: the thought's left rule (the stream's one aside mark), a
    call's sunk inset, a diff card's rim outside the review, a picture's or a chip's ring, a
    button's ring, and the hairline between two tiles.
  - **Cost.** The fade costs nothing measurable on the CPU against the plain list
    (`docs/MEASUREMENTS.md`, "the thread's soft top edge").
  - Tests: `review::tests::no_rule_or_frame_parts_the_review` (at 1200 and 800 pt, no border
    runs most of the way across the tile or down the file list), and
    `conversation::thread::tests::the_turns_fade_under_the_header_with_no_rule` (no hairline at
    the header's foot; the top fades on a long thread and the foot does not); the bench
    `soft_edge_cost`. Goldens: the thread, review, file and agent renders retaken in the same
    batch.

- ✅ **A machine and a project wear their own colour** (2026-10-06,
  `.research/status-color-2026-10-06.md` §5.2, build step 4). The person asked for more colour
  that stays elegant. Apple's sidebars tint their icons. So a machine's glyph and a project's
  glyph each take one of the theme's eight identity hues (`Surfaces::identity`), and nothing
  else of theirs does: never the name, the row, a chip or a wash.
  - **Which hue.** It is the FNV-1a hash of the thing's group key as `GroupKey::as_str` spells
    it: `machine:<worker id>` for a machine, `project:<project id>` for a project, `repo:…` and
    `folder:…` for the repositories and folders the navigator lists as projects. Every client,
    the phone included, spells the same key, so each shows the same colour with no wire change
    (`kit::identity_ink`, test `a_machine_wears_the_same_colour_on_every_client`, which pins a
    known worker id's hue).
  - **Where.** A machine's glyph wears it in the navigator's machine heads and rail, the tile
    header's worker, the breadcrumb, the palette's workers and the new-agent picker's machines.
    A project's group glyph wears it in the navigator's heads and rail, and on its board's row
    and its palette line. Away, a machine's glyph goes grey (`kit::machine_ink`), so a lost
    machine reads as lost.
  - **What stays neutral.** A grouping by a branch, an agent or a label (`kit::wears_identity`)
    keeps the lead's tier. The hues sit at the status fills' lightness and keep 20° from every
    status hue, so an identity glyph never reads as a state. Shape keeps them apart as well,
    since an identity is never a circle.
  - Lint: `kit::tests::identity_colour_stays_on_its_glyph`. The hues are read only in
    `kit/identity.rs`, and the two inks are called only where a machine's or a project's glyph
    is drawn.
- ✅ **A field that speaks to an agent keeps its lines** (2026-10-06,
  `.research/readiness-2026-10-07.md` R2). A review comment and the reason a request is denied
  with are read by an agent, and a comment often carries a few lines of suggested code. Both
  were single-line fields, which dropped a paste's line ends and took no ⇧↵. Each is now a
  field of several lines that grows to a few rows (eight for a comment, six for a reason) and
  then scrolls: ↵ adds the comment or denies, ⇧↵ breaks the line, as in the thread's composer.
  Deny's buttons sit at the field's last line. The written answer to an agent's question goes
  the same way once the questionnaire takes a field of several lines (the gpui-kit fork).
  - Tests: `review::tests::a_comment_keeps_its_lines`,
    `conversation::thread::tests::doors::a_reason_keeps_its_lines`.
- ✅ **The first message is written in the thread's composer** (2026-10-06,
  `.research/readiness-2026-10-07.md` R1 and R5; amends "A start opens its tile at once" in
  `agents.md` and supersedes "A Claude Code start can plan first" in `claude-code.md`). The
  prompt that defines a task was written in the weakest field in the app: one line, which
  glued a pasted spec's lines together and dropped a pasted picture, with no `/`, `@`, recall
  or model. T3 Code's draft thread is its full composer, and MonoCode's start takes the model,
  effort and mode with the prompt.
  - **One composer.** A start's tile hosts the thread view in a draft mode
    (`conversation::thread::draft`). It draws a local state in place of a mirror, asks the
    worker nothing, and keeps the composer's intents: a model, mode or effort picked sets the
    draft's meters, so the chips read as they will, and ↵ hands the start the message, its
    attachments and those choices. Pasted lines, ⇧↵, pictures, files, drops on the tile, `/`,
    `@` and ↑ all work as in a thread, ↑ bringing back earlier starts' first messages. ↵ on an
    empty composer still starts the agent bare.
  - **What the chips offer** is what the machine says a new thread of the agent can start with
    (`InstalledAgent::offers`): the adapter's own start options, such as Claude Code's modes,
    else the newest thread's. With none offered, the chip offers nothing.
  - **The start carries it all** (`Start::{prompt, model, mode, effort, attachments}`), and
    each adapter applies it at launch. The app no longer builds Claude Code's
    `--permission-mode plan`.
  - **Deleted:** the start's own field (`StartField`, `FieldView`), the "Plan first" tick, the
    palette's "Start in plan mode" (`TogglePlanFirst`) and the plan flag.
  - **A refusal gives the draft back.** Once sent, the thread says "Starting …" in place of
    the composer. A start the machine refuses, or one whose link drops, returns to the draft
    with its words and files as they were, where it used to close the tile.
  - **A worktree is named by the first message** (R5): its words, lower case and joined by
    hyphens, six at most and 40 bytes, then four hex digits of the tile's id
    (`fix-the-login-redirect-3f2a`). With no words, the agent's name stands in. Amended
    2026-10-06: a word is its letters, digits and marks in any script, and Latin letters fold
    to ASCII (NFD with the nonspacing marks dropped, and a small table for the letters with no
    decomposition: đ, ß, ø, æ, ł and the like), because remotes, pull request addresses, shells
    and CI take an ASCII branch best. "sửa lỗi đăng nhập" names `sua-loi-dang-nhap`; it used
    to be cut to `s-a-l-i-ng-nh`. A script with no Latin form (CJK, Cyrillic, Thai) keeps its
    letters and marks. A word longer than the room is cut at a letter's edge.
  - Picking a model, mode or effort, or closing the "+" menu by its button, hands the keyboard
    back to the field, in a draft and in a thread alike.
  - Tests: `workspace::tests::thread_start::{a_start_is_written_in_the_threads_composer,
    a_worktree_is_named_by_its_first_message, a_start_can_take_a_new_worktree_of_a_repository}`;
    e2e `threads_start` and `showcase` start on the composer.
- ✅ **A tile's header holds no button at rest** (2026-10-06,
  `.research/elegance-icons-2026-10-06.md` §3.5, §5.13 and §5.14). The focused tile showed four
  buttons at rest: a face switch of two or three segments, fullscreen and close. A wall of tiles
  read as rows of buttons, and the person mostly watches. At rest a header now shows the lead,
  the title, the facts and how the tile stands, focused or not.
  - **Under the pointer** come the kind's actions (a page's back and forward, a folder's way up,
    the trackpad, mute), one face toggle and close. The toggle is a single button showing the
    face ⌘J goes to next, named for it ("Show terminal"). The face switch is gone.
  - **What stays at rest** is what says how the tile stands: another client ruling the PTY's
    size ("Take over"), a stream's health, the system's keys sent on, a silenced worker.
  - **Fullscreen has no button.** A double-click on the header's empty part fills the screen, as
    a Mac's title bar zooms its window. A double-click on the name renames the tile. The palette
    and the header's menu also offer "Fullscreen".
  - **A page's reload** moves to the header's menu and the palette's "Reload page". ⌘R stays
    the layout's (niri's width cycle). Back and forward are a matched pair, `chevron.left` and
    `chevron.right`, where `arrow.right` stood beside `chevron.left`.
  - **The state is a glyph, not a pill.** The agent pill ("Needs approval") is gone: the header
    ends in the state's glyph, which carries the agent's whole state to a screen reader and what
    it asks to the pointer. A terminal's agent waiting on the person makes it a button to the
    TUI's prompt. Under the pointer it gives way to the controls, as the readouts do, so it is a
    button for the keyboard, VoiceOver and touch; a click on the tile reaches the prompt anyway.
    Hidden controls stay in the accessibility tree.
  - **Touch** has no hover and draws none of them. The header's long press holds the other
    faces, reload, fullscreen and close, and a phone's "…" lists the same rows.
  - **A pull request** is read from the thread's row (`ThreadRow.pull`), for every agent alike,
    not from Claude Code's status line. The client keeps only the worktree a status line names
    and drops its pull request on arrival, so the header has one source (2026-10-06,
    `.research/readiness-2026-10-07.md` rank 15). Its glyph is drawn by where it stands (open, merged,
    closed, a draft) in that state's ink, then its number. The navigator's row shows it on its
    second line, the glyph inked only where it needs the person or is ready to merge.
  - **The navigator's machine and project heads** set the name at the section role's weight
    (600), with the identity-tinted form glyph beside it at semibold.
  - Tests: `workspace::tests::tiles::{a_header_at_rest_has_no_buttons,
    the_header_fills_the_screen_names_and_closes_its_tile}`,
    `faces::the_toggle_picks_the_face_and_a_plain_shell_has_none`,
    `page_chrome::back_and_forward_show_only_with_history_that_way`,
    `handoffs::an_agents_pull_request_rides_on_its_header_while_the_agent_runs`.
- ✅ **A folder's review sends its comments to a new agent there** (2026-10-06,
  `.research/readiness-2026-10-07.md` R9). A folder's review kept no comments, having no agent
  to tell. Work done by an agent whose thread is gone, or in a plain `claude` elsewhere, then had
  no way back: the person saw the problems and typed them again into a new start.
  - **Comments as a thread's review takes them.** A press or a drag on a folder's lines opens a
    comment, quoted with its code as for a thread. Keep, put back, "Mark reviewed" and "Add to
    message" stay a thread's, so a folder's foot shows only while comments wait, with its one
    action: "Send to a new agent".
  - **The agent is the one the folder last ran.** The newest thread that worked in the folder
    itself, else in its repository, among the agents the machine can start; the machine's own
    choice otherwise. It starts in the folder, so a finished task's branch review starts it in
    the task's worktree.
  - **Through the start's composer, not straight to a send.** The start opens with the comments
    in its composer, to be added to, with the agent's model and mode chips, before ↵ sends them
    as its first message. They leave the review once the composer holds them, as "Add to
    message" does. A machine out of reach or with no agent opens nothing, says why, and keeps
    them.
  - Tests: `workspace::tests::thread_start::{a_folders_comments_go_to_a_new_agent_there,
    a_folders_comments_stay_when_no_agent_can_take_them}`.
- ✅ **The agent commits on the person's ask** (2026-10-06, `.research/readiness-2026-10-07.md`
  R10). The commit message was always the person's to type. t3code has its provider write commit
  messages, PR text and branch names. Slopty calls no model itself, but the thread's own agent
  knows why it changed what it did.
  - **One ask through its own door.** A thread's commit sheet, from its tile or its review,
    offers "Ask `<agent>` to commit" beside "Open pull request", a quiet button: the person's
    message and Commit stay the default. It sends one message, "Commit what you changed, with a
    message saying why.", after the turn where the agent queues
    (`ThreadMeta::delivery_after_turn`), so work in hand is not cut into. An agent that takes no
    message (no `Cap::QUEUE` or `Cap::STEER`) is not offered, and a folder's sheet has no one
    to ask.
  - **The sheet reads again when the turn ends.** While it waits it says so ("Waiting on Claude
    Code…"). Once the message has left the queue and the turn it went into has ended, the
    sheet asks the status and the pull request again, so it shows the agent's commit. A
    refusal is said under the buttons in the worker's words, and the ask can go again.
  - Tests: `conversation::thread::tests::commit::{the_agent_is_asked_to_commit_and_the_sheet_reads_again_after_its_turn,
    an_ask_to_commit_turned_down_says_why, an_agent_that_takes_no_message_is_not_asked}`.
- ✅ **A phone's drawer floats as iOS 26's sidebar does** (2026-10-06,
  `.research/elegance-icons-2026-10-06.md` §5.11). The drawer was a plain sheet to the window's
  edges over a modal's scrim. A large title repeated the workspace's name, a *Workspaces*
  section listed the one workspace, "New workspace" was a row of its own, and each machine's
  head showed its chevron, "+" and "…" all at once.
  - **Floating.** The panel stands a small step (`spacing.sm`) in from the safe area's top
    and leading edges and from the window's bottom edge. Its corners are `radii.lg`, it is
    rimmed all round, and it wears the floating elevation (`kit::elevate`). Its rows clear the
    home indicator. The scrim under it is the elevation's shade at `Elevation::aside` (0.22
    dark, 0.10 light, `kit::aside_scrim`), half a modal's, because the work is one tap away.
    Laid over an iPad's frame, the navigator still meets the window's edges.
  - **No large title, no lights row.** The search field is the drawer's first row. The bar
    beside the drawer already names where the person is.
  - **Workspaces only from two.** With one workspace the section says nothing the bar doesn't.
    "New workspace" is the bar's "+" menu's, and the row is deleted.
  - **A finger's head keeps its chevron.** A machine's "+" and "…" and a project's "+" are its
    long press's menu. The machine's menu now leads with "New shell here", as the project's
    does.
  - **Touch chrome at 17 pt.** A lead, an icon button's symbol and an inline glyph take their
    words' type role (`IconSize::Lead` is the chrome role's size and `Inline` the metadata
    role's). The rows they sit in (navigator rows, the filter, menu rows, the dialog shell,
    buttons, meta and section labels) take the same roles. A pointer's sizes are unchanged;
    a finger's are 17 pt chrome, 13 pt metadata and 17 pt leads in a 20 pt slot.
  - **Follow-up: true glass.** The drawer stands on the elevated surface for now. iOS's own
    material under a floating panel needs window composition in gpui-fast, the track upstream
    as zed#62379. When that lands, the drawer takes the platform material as the docked Mac
    navigator does.
  - Tests: `workspace::tests::nav_rows::{a_phone_drawer_floats_clear_of_the_edges,
    a_phone_drawer_lists_the_workspaces_once_there_are_two}`,
    `workspace::tests::nav_list::the_phone_drawer_runs_through_the_home_indicator_band`,
    `workspace::tests::frame::a_finger_finds_a_headers_actions_in_its_menu`,
    `workspace::tests::nav_projects::on_a_phone_the_drawer_lists_the_projects_then_the_workers`,
    `kit::tests` (the aside scrim is lighter than a modal's).
- ✅ **A tile's notice names the tile as it is drawn** (2026-10-06). A "needs you" notice kept
  the tile's name from the moment it was raised. A thread that comes to ask in the same table
  row that names it got there before the tile's title was worked out again, so the notice read
  the shell's directory ("atlas needs approval") while its row and header read the thread. The
  notice now keeps the tile and what happened, and reads the tile's title each time it is
  drawn.
  - Test: `workspace::tests::toasts::a_tiles_notice_reads_the_name_the_tile_comes_to_have`.
- ✅ **An empty thread is its composer** (2026-10-06, `.research/premium-foundations-2026-10-06.md`
  change 8). An empty thread drew a speech-bubble glyph over "New Claude Code thread" and a line
  of where it worked, centred where its rows would be, with the composer far below at the foot.
  The line carried a path (a run's temporary home read "in slopty-e2e-94f6a37e/home") and broke
  "Opus 5.5" over two lines in a narrow tile. The glyph and the notice are deleted.
  - **The composer is the page.** It stands centred at the reading width under one question in
    the page heading's size at the regular weight, in the secondary tone: "What should Claude
    Code do in slopty?" (t3code's "What should we build in {project}?"). The folder goes by its
    name, the project's when its repository is known, else its own, `~` at home, never a path. A
    start in a new worktree asks "… in a new worktree of slopty?".
  - **Where it works sits in the composer's foot**, before the model chip: "studio · slopty", the
    first of the foot's items to give way. Once the thread has rows its tile's header says it.
  - **The first message docks the composer at the foot.** It moves there on the sheet's pace,
    and under Reduce Motion it is there at once. The keyboard stays in the field across the
    move. A start that went still says "Starting Claude Code" in the rows' place, and a thread
    whose only row is a request asks nothing, its card being the statement.
  - Tests: `conversation::thread::tests::{an_empty_thread_is_its_composer_under_a_question,
    the_composer_moves_to_the_foot_after_the_first_message}`,
    `conversation::thread::view::tests::an_empty_thread_asks_what_to_do_where`.
- ✅ **Premium foundations: the theme batch** (2026-10-06,
  `.research/premium-foundations-2026-10-06.md` changes 2–6, 9, 10, 12, 13). The person still
  found the UI provincial. Five token values were off the references and are seen on every
  screen, so the fix starts at the theme.
  - **Reading type 14/22** (`Typography::default().prose_size` 15 → 14, and the settings'
    default with it). zeron sets 14/22 and t3code 14 × 1.625; at 15 beside 13 pt chrome the
    thread read as a blog post set large. Touch prose is 16/26.
  - **One type scale; the strong weight is for titles.**
    - `section` is 12/16 at 500, drawn a tier under the rows it heads.
    - `panel_title` is 15/20 and `page_heading` 20/26, both at 600.
    - `first_run` is 26/32 at 500, SF's Display cut.
    - The navigator's machine and project heads move from 600 to 500: a name, not a title.
    - `Typography::{title, heading, display, task_title}` are deleted. Every site reads
      `roles()`.
    - Lints: `kit::tests::one_type_scale` (no call to the old sizes on the typography),
      `strong_weight_is_for_titles` (600 only by a title's role), and the theme test that only
      `panel_title` and `page_heading` are strong.
  - **A light selection rises.** On the canvas (the docked navigator) the chosen row is the
    raised surface, white on the dimmer canvas, on the resting contact, with no inner ring. The
    pointer's step is half way up to it (`Surfaces::chosen_on_canvas`, `hover_on_canvas`,
    `alpha::HALF`). In dark both are the washes. On a float, including the phone's drawer and
    an overlaid navigator, the selection stays the wash, since white on white would vanish
    (`kit::Plane`, `kit::selected_on`, `kit::paint_chosen`, the list plate's plane).
  - **A dimmer navigator.** A row's words rest in `text_secondary` and take `text` at 500 when
    chosen. Section labels are the section role in `text_muted`. The machine and project heads
    stay in `text`. This is Linear's dimmer sidebar: the work leads.
  - **Colour by urgency.**
    - Identity drops to C 0.06 in dark and 0.07 in light (L 0.60), a tint that tells machines
      apart without competing with a state.
    - Working's fill drops to C 0.11 dark and 0.12 light; merged's to 0.09 and 0.10. The amber
      and red keep full chroma: they are what the person must act on. Every hue is kept.
    - The identity tint now lands on the navigator's heads alone. The palette's machine and
      project rows, the start's machine list, a tile header's machine, the breadcrumb and the
      empty workspace's machines draw their glyph in their words' tier. `PaletteItem::identity`
      and `with_identity` are deleted.
    - `identity_colour_stays_on_its_glyph` allows `navigator.rs` only, with `project/view.rs`
      until lane A's board meta moves.
  - **No bell disc.** The amber or green count disc, the one web badge in the app, is gone.
    The bell's glyph becomes `bell.badge` while anything is unread, in its words' tier, and
    takes the warn fill only while something needs the person. The count is said to a screen
    reader ("1 new"), not drawn.
  - **No repeated second lines.** A file's or a folder's navigator row leaves its folder to its
    tile's header and is one line. It shows the folder only beside a namesake, as the palette
    tells two apart. A shell's line stays: it says only what its title does not (what it is
    doing, the directory below the title's root, its branch). The overview's miniatures keep
    the whole line (`nav_tile_meta` against `tile_meta`).
  - **Tile focus by tone as well as weight.** An unfocused title steps back to
    `text_secondary` at 400 and the focused one leads in `text` at 500. The weight-only swap
    made "Terminal" look bold among greys.
  - **Glass that shows.** `alpha::GLASS` drops from 0.90 to 0.82. That is the lowest step of
    0.02 at which every text tone keeps its floors over any wallpaper, and t3code's glass is
    0.80. The chrome-reads test now bounds the ground by the share the glass lets through, and
    a text tone's shift on glass by a tenth of L, with the tiers still a step apart. The glass
    spanning the whole frame (the title bar, the gutters, the status bar) lands with the panels
    (change 1), because without them the frame has no canvas of its own to show it.
  - **Overrules.**
    - The 2026-10-05 premium pass's "the tokens are sound; the slop is composition". Five values
      were off, and composition alone cannot give a tile a plane.
    - The identity chroma of "A machine and a project wear their own colour" (status-color
      §5.2): every hue stays, at under half the chroma, on the navigator's heads only.
    - Status-color §7's "no rule between settings rows", now that rows are one line (lane A's
      change 7, `docs/decisions/settings.md`).
  - Tests: `slopty_theme::tests::{the_type_roles_are_the_scale_the_critique_set,
    a_selection_on_the_canvas_rises_in_light, glass_reads_as_the_chrome_in_light_and_dark,
    text_on_glass_keeps_its_floors_over_any_wallpaper}`, `kit::tests::{one_type_scale,
    strong_weight_is_for_titles, identity_colour_stays_on_its_glyph}`,
    `icons::tests::an_icon_takes_its_words_size_and_weight`,
    `workspace::tests::nav_rows::{a_docked_selection_rises_to_white,
    a_files_row_says_its_folder_only_beside_a_namesake}`,
    `workspace::tests::frame::the_frame_says_where_the_focused_tile_is_once` (no disc, the count
    said),
    `workspace::tests::focus::the_focused_tile_is_said_by_its_titles_tone_and_weight`.
- ✅ **Premium foundations: tiles stand on the canvas as panels** (2026-10-06,
  `.research/premium-foundations-2026-10-06.md` changes 1 and 9). The strip was one flat plane:
  the tiles, the title bar and the navigator one surface, parted by hairlines. Every reference
  (t3code, zeron, Linear, niri itself) puts its work on panels over a frame.
  - **Panels.** Each tile is a panel on the canvas: the content's surface at `radii.md`, a ring
    in `border_subtle`, and in light the resting contact (`Elevation::LIGHT.rest`); in dark its
    top edge catches the light (`Rim::rest`). `kit::panel` is the only way to draw a tile's
    ground: the strip's tiles, a tile closing, a tile starting, a tile whose machine is away and
    the empty workspace's page. The lint `a_tile_stands_on_a_panel` holds the files that draw
    them to it.
  - **Gutters.** `Spacing::gutter()` (8) parts two columns, two stacked tiles, the navigator
    from the first column and the last row from the window's bottom. The layout keeps its
    columns flush. The frame sets the strip half a gutter in from its sides and bottom, and
    each panel stands half a gutter in from its place, so widths, scrolling and every handler's
    geometry stay the layout's. A terminal's grid is sized to its panel. The strip clips its
    own tiles, so a panel sliding off is cut half a gutter in from the window's edge. Without
    that clip a frame of motion cost about a quarter of a millisecond more, as every panel
    past the edge was painted in full (`docs/MEASUREMENTS.md`, the same entry).
  - **No hairlines.** The dividers between tiles and the navigator's and the rail's trailing
    hairline are deleted: the gutter is the edge. A header lies inside its panel's top with no
    fill and no rule, a page's or a remote picture's too: the rule they kept under the header
    was the last divider on the canvas. The focused panel is unmarked; focus stays the title's
    tone and weight.
  - **Tabs.** A tabbed column's tab row fades at an edge only where tabs lie hidden past it
    (`gpui::edge_fade(..).hidden_by_scroll`), so a row that fits has no fade and a scrolled one
    says which way the rest lies. Showing the first tab scrolls the row home, its leading pad
    included, so no fade is left at the start.
  - **Handles and drops.** The resize handle sits in the gutter, as tall as the panels, its
    accent line down the gutter's middle. A drop's line runs down the gutter where the column
    opens, and its wash is the panel the tile would stand as, rounded as one.
  - **Corners.** GPUI clips children to rectangles, so a child painting to the edge (a remote
    picture, a page, a body's own fill) would poke square corners out of the round ones. The
    panel covers its corners last with the ground it stands on, then draws its ring. The
    composition oracle draws a primitive after a native over it, so a remote picture's corners
    are covered too.
  - **What it costs.** Drawn whole, two shadows, the corner cover and the ring under each
    panel cost the GPU about four times flush tiles, because a shadow's quad covers its whole
    element. The contact shadow is drawn in bands along the edges, and GPUI already draws a
    border round an empty middle as its edge strips. The panels then cost a quarter more than
    flush tiles (`docs/MEASUREMENTS.md`, "Panels on the canvas: what their edges cost the GPU").
  - **Phone.** A phone's tiles are full-bleed: no gutter, no radius, no ring and no shadow, as
    its screen shows one tile at a time. A Mac window narrower than `phone_below` lays out as a
    phone, so `window-minimum` (375 pt) shows one full-bleed column. The iPad keeps gutters.
  - **The overview.** A workspace's block is the canvas in small, its panels standing on it,
    and the words above it start on the panels' glyphs. A panel's radius follows the zoom down
    to the miniatures' zoom and holds there. A miniature wears its words at the chrome's size,
    and scaled further, its corners read as square inside the block's round ones.
  - **Glass across the frame.** The material is under the window whenever it can be, not only
    while the navigator docks. The frame lays the canvas on it at `alpha::GLASS` wherever the
    canvas shows: under a docked navigator or the rail, in the title bar (which paints no ground
    of its own now) and in the gutters. The panels stay opaque. The title bar, the breadcrumb
    and the bar's readouts draw in the glass's tones (`frame_theme`). A navigator floating on
    its own sheet keeps the workspace's tones.
  - **Overrules.** The 2026-09-25 "Panes sit flush, divided by hairlines" ruling and its
    shipped follow-ups ("the flush layout", "flush tiles"), which removed gaps and corners at
    the person's word. The layout itself stays flush; only the drawing stands back. Why a
    ruling of the person's own is reversed: they had asked for flush panes to look modern and
    minimal the way Warp does. Flush panes, with the hairlines between them since thinned at
    their word, became one flat page, and on 2026-10-06 they judged the whole as provincial,
    not premium. Panels are niri's own look, and the foundations study traced the provincial
    read to tiles having no plane. The way back is cheap if they want it: `Spacing::gutter()`
    to 0 and `kit::panel`'s radius off.
  - **Also in this batch.**
    - A navigator row's "Allow" is neutral: the text's tone at the action's weight, "Deny" a
      tier back. It was the accent's green, which says done.
    - A project's notice in the title bar says the task and how it stands. It is named by its
      project only while that project's orchestrator is not on the workspace in view.
  - Tests: `kit::panel::tests::{the_bands_hold_the_edge_and_leave_the_middle,
    a_panel_stands_on_the_canvas_with_its_corners_covered, a_flat_panel_is_its_surface_alone}`,
    `kit::tests::a_tile_stands_on_a_panel`,
    `workspace::tests::tiles::{a_tile_stands_on_a_panel_and_its_header_on_it,
    a_gutter_parts_every_neighbour, a_phone_tile_is_full_bleed,
    a_tab_row_fades_only_where_tabs_lie_hidden}`,
    `workspace::tests::strip_marks::{the_handle_sits_in_the_gutter_and_resizes_the_column,
    a_small_overview_draws_tiles_as_miniatures,
    the_drop_line_runs_down_the_gutter_and_a_join_washes_its_panel}`,
    `workspace::tests::frame::on_glass_the_frame_shows_the_material_and_the_panels_stay_opaque`,
    `workspace::tests::focus::the_focused_tile_is_said_by_its_titles_tone_and_weight` (the
    panels' edges alike),
    `workspace::approvals::tests::an_answer_is_neutral_and_the_yes_leads`,
    `workspace::tests::projects::a_project_s_notice_in_view_says_its_task_alone`.

- ✅ **One message on several agents, each in a worktree of its own** (2026-10-06,
  `.research/readiness-2026-10-07.md` §4 rank 17, after Orca's parallel worktrees: one prompt
  fanned across agents, the results compared and the best one kept).
  - **Where it is offered.** A start in a new worktree lists the machine's other agents in its
    composer's "+" menu, after a separator, as "Also run Codex" and so on, each ticked while it
    is chosen. A start in the folder itself offers none, because two agents in one tree would
    edit the same files. The question over the composer names everyone who will run it: "What
    should Codex and Claude Code each do in a new worktree of atlas?".
  - **What ↵ does.** The message starts on the draft's agent with what its chips chose. It
    then starts on each other agent ticked, at that agent's defaults, with the same files
    attached, each in a new worktree of its own. Each tile is a column of its own, opened to
    the right of the one before. The keyboard stays with the run the person wrote. The
    worktrees share the message's words and differ in their last four hex digits
    (`try-the-other-layout-1a2b`, `…-3c4d`).
  - **Runs are found from the worktrees, with nothing kept.** Two threads are runs of one
    message when they work in worktrees of the same clone named by the same words. That
    holds across a relaunch and on every client. A worktree named only by its agent (a start
    with no message) belongs to no set of runs. A thread with runs says so in its "+" menu,
    "Review the 2 runs side by side", which opens every run's review, its own first, each as a
    tile beside the last. The person keeps the best one through its commit sheet as usual.
  - Tests: `workspace::tests::thread_start::{one_message_starts_on_several_agents_each_in_a_worktree,
    a_start_in_the_folder_runs_on_one_agent}` and
    `workspace::tests::review_tile::a_messages_runs_are_reviewed_side_by_side`.

- ✅ **Photos beside Files in the composer's "+" menu on iOS** (2026-10-06, readiness rank 13).
  On a phone or a tablet the pictures are in Photos, which the Files picker cannot reach, so
  "Photos…" follows "Attach files…" there. What it picks lands as a drop on the tile would, the
  same path the Files picker takes. A picker that cannot be shown says "The Photos picker could
  not be shown". The Mac has no such row; its open panel reaches the Photos library itself.
  Tests: `conversation::thread::tests::face::the_add_menu_offers_photos_where_they_live` and
  `workspace::tests::remote::a_picture_pasted_into_the_composer_stays_a_chip_until_sent`.

- ✅ **Review a pull request by its number** (2026-10-06, readiness rank 18's UI, on the wire
  in `docs/decisions/projects.md` "A worktree can check out a pull request").
  - **The steps.** The palette's "Review a pull request…" asks which repository, then which
    pull request. The repositories are the focused tile's, then every one a shell or a thread
    stands in, on each machine that can start an agent. Each clone is listed once, since a
    thread in one of its worktrees names the clone, and a line names its machine only when
    several machines have one. With a single repository the number step comes at once. The
    number is typed as `123`, `#123` or the pull request's page, and the step's one line says
    "Review #123 in atlas". No list of open pull requests is fetched: the person comes with a
    number from a link or a notification, and asking the forge first would only add a wait.
  - **The start.** The machine's usual agent starts in a new worktree that checks the pull
    request out, named `pr-123-` and four hex digits, so several agents on one pull request
    are found as runs. Its composer holds "Review pull request #123", to send as it is or add
    to, and the question over it says "What should Claude Code do with #123 in a new worktree
    of atlas?". Other agents ticked in the "+" menu check out the same pull request in
    worktrees of their own.
  - **The review.** Once the thread is there, its review opens beside it on the whole branch.
    A thread's whole branch is now read against the branch its pull request merges into, as
    the forge names it (`PullSeen::base`, `Against::Branch`), for every thread with a pull
    request and not only these. Until the worker has said which, it reads against the guessed
    base.
  - Test: `workspace::tests::thread_start::a_pull_request_is_reviewed_in_a_worktree_that_checks_it_out`.

- ✅ **Premium pass from the mockups: a deeper light canvas and one green way on** (2026-10-06,
  from reference mockups an image model drew for the person's "still provincial" verdict,
  `.research/mockups-2026-10-06/`). Each proposal was held against the pixels and the earlier
  rulings before it was taken.
  - **The light canvas is the panels' frame.** Sampled, the mockups' canvas is OKLCH L 0.941
    to 0.946 (`#ecebea`, `#ededed`) and the settings sidebar L 0.967 (`#f3f4f5`), all at chroma
    0.0017, under panels at L 0.991 to 0.997. Ours was the same chroma at hue 68 (`#f4f3f2`,
    L 0.965) under panels at L 0.992: the warm tint was not the difference, the step was, 0.027
    L against about 0.05. The light canvas share goes from 0.04 to 0.06 of the ink (`#f0efee`,
    L 0.953), so the panels stand 0.039 L over it. The one neutral stays. Past 0.06, secondary
    text on glass had to darken more than a tenth of L. Dark is unchanged: its panels stand on
    their lit rim. `the_chrome_sits_one_notch_from_the_content` now holds light's canvas at 4 to
    6 L* under the content and dark's at 2.5 to 4. At the extreme light end (a `#cccccc`
    terminal background) secondary text on glass goes as far as black.
  - **One green way on** (superseded 2026-10-07 by "The design system starts from `MonoCode`'s":
    the send and the plain allow are the neutral solid, and `kit::go` is deleted). The composer's
    send and the plain allow of an agent's ask take the
    brand's green with its near-black ink (`kit::go`, `ButtonKind::Go`), as the mockups and the
    Codex Astra direction draw them. Overrules "The primary is the neutral solid" for these two
    alone: they are the presses that set work going. Every other primary (Commit, Create, Done,
    Merge) keeps the solid, and so does the send disc while it stops a turn. Under the pointer
    and pressed the green lightens, away from its ink, which reads AA in every state in both
    variants. The lint `the_accent_is_never_a_control` still holds every other file; `kit/go.rs`
    is where the one is drawn.
  - **The composer's foot.** (The send's disc is superseded 2026-10-07: a 26 pt square at
    `radii.sm`.) The send is a disc (`radii.full`). The "+" is an outlined disc
    and the model an outlined pill, the ordinary hairline at the full radius, as the two
    things set before writing. The other chips stay quiet words.
  - **The composer card** wears the quiet hairline at rest and the ordinary one with the
    keyboard, over its contact. The 3:1 focus edge (`field_focus`) is deleted: the caret says
    where the keyboard is, and the dark ring round the card was the loudest thing on screen.
  - **The board.** A lane's rows sit in one raised group (`kit::raised`, `radii.md`), a quiet
    hairline between two rows. The head reads glyph, name and count, the count at the trailing
    edge, as the reference boards set theirs. The state glyphs themselves wait for their own
    pass, with the chrome's icons. Kept: the working mark stays the braille cell ("The working
    mark is a braille cell", ruled today; a turning arc steps 30° at a time at the spin clock's
    rate), and working keeps its blue, one hue per meaning, though the mockup drew it green.
  - **Settings.** The sections lie on the canvas, and the one shown rises off it as the white
    plate the navigator's chosen row wears (`kit::Plane::Canvas`). The pages were already
    grouped inset sections.
  - **The destructive button** (`ButtonKind::Destructive`, `kit::destructive`) is the one
    press that removes or ends something for good, behind a confirm (Remove a machine). It fills
    with `error_solid`, the error mark three tenths of the way to the error's word: deeper in
    light, where white on the mark's own red read 3.6:1, and lighter in dark, where the dark
    ink read 4.1:1 over the lightest dark content. It takes the solid's ink. Under the pointer
    and pressed it eases as the solid does, away from its ink, so the ink reads AA in every
    state. At rest it wears coss's finish in both variants, a white 1 px highlight inside its
    top at `alpha::DIM` (`Finish::lit`), with light's contact under it. Pressed, the solid's
    shade goes inside its top. The red is the kit's alone (`the_destructive_red_is_the_kits`).
  - Tests: `kit::go::tests::the_go_control_reads_in_every_state`,
    `kit::tests::the_destructive_button_reads_in_every_state`,
    `conversation::thread::view::decision` (the plain allow is `Go`), the theme's notch and
    glass floors and `controls_and_the_words_on_fills_read`,
    `kit::tests::the_accent_is_never_a_control`.

- ✅ **A machine is removed from its row or the palette, after a confirm** (2026-10-06,
  readiness rank 20; what goes and what stays is ruled in `docs/decisions/workers.md`, "A
  machine is removed whole").
  - **Where.** "Remove…" closes a machine's "…" menu, after Forget, and the palette has
    "Remove studio…" for each machine. Both are offered only by the Mac's app, which can reach
    a machine to change it. While a removal runs, neither is offered for that machine again.
  - **The confirm.** An alert titled "Remove studio?" says what goes: Slopty stops there, its
    services, its own files and its hooks go, and the machine is forgotten here and on the
    server. It says the person's repositories, worktrees and agent sessions there stay as they
    are. It adds "Its 3 open shells and agents end." while shells are open there, and for this
    Mac "Slopty will no longer open at login." Remove runs the removal. Cancel, Esc or a click
    outside removes nothing.
  - **The order.** The app takes the worker off over ssh (or in place on this Mac), then waits
    for the server to list the worker as away, and only then asks it to forget the machine
    (`Verb::ForgetWorker`, which refuses one that is online). The wait follows the directory's
    own word; a worker still listed online 10 s after its removal is said to still answer. A
    removal that fails there says why and forgets nothing.
  - Remove wears kit's destructive style.
  - Tests: `workspace::tests::bars::removing_a_machine_asks_first_and_says_what_stays`;
    `slopty-app` `ssh::tests::a_removal_takes_the_worker_off_then_the_server_forgets_it`.

- ✅ **A finished turn of a thread with no terminal is left to review** (2026-10-06, readiness
  2026-10-08 R2). Only a terminal's agent earned *To review*, the bell's count, the away note
  and the tile's unseen dot: a turn was timed from the terminal's session, so Codex beside no
  TUI, pi, an ACP agent and a message's runs ended their turns unseen.
  - The turns under way and the finishes not looked at are kept by what they are about
    (`attention::About`): a terminal's session, or a thread with none. Every worker's table
    feeds them alike: a thread's row moving to working starts its turn, and done ends it.
  - A thread's finish reads as a terminal's: long enough (`SLOW_COMMAND`) and unwatched, it
    lists under *To review* with the agent's last line, counts on the bell, posts a note while
    the app is away, and dots its tile. A thread with no tile here is still listed; its row
    opens it. Focusing its tile clears it, and a thread its worker's table no longer holds is
    not counted.
  - Test: `workspace::tests::thread_waits::a_threads_finished_turn_without_a_terminal_is_to_review`.

- ✅ **Every start offers one list of places, newest first** (2026-10-06, readiness 2026-10-08
  R5). A start offered only where shells stood, so with no shell open the folder step had just
  `~`, and "New agent" on the empty workspace started there.
  - One list (`WorkspaceView::recent_places`) holds, per machine and folder once: where its
    shells stand, where its threads work (subagents are their parent's), the last start's
    folder, and where the agents' past sessions ran. The machine lists those past sessions with
    no words as its link comes up (every agent's) and again as a folder step opens (that
    agent's); the folders it lists replace those kept and join the step still up. The most
    recently used tile's place leads, then the newest by when it last changed.
  - The folder step lists the focused tile's folder, then that list for its agent, then home.
    "New agent" on the empty workspace starts in the list's first place for the usual agent.
  - A place on the empty workspace starts the machine's usual agent there, and opens a shell
    only where the machine has no agent. "New terminal" as a second way into each place needs
    a control on the row, which waits for the tiling and MonoCode design pass.
  - One wordless ask is out per machine and agent at a time. "Resume a past session…" opened
    while the folder step's is out sends none, and that answer feeds both the session step
    and the places. A dropped link lets the next ask go.
  - Tests: `workspace::tests::thread_start::every_start_offers_where_threads_and_past_sessions_worked`,
    `…::one_ask_for_past_sessions_feeds_the_folder_and_session_steps`.

- ✅ **Starts remember the last one across a relaunch** (2026-10-06, readiness 2026-10-08 R10).
  The last start and the chips lived only in the window's memory, so every launch began on the
  first agent, the first machine and default chips.
  - The device keeps its starts in `starts.json` in the client's data
    (`slopty_client::starts`), apart from the layout, since they are the person's defaults. It
    holds the last start that went (agent, machine, folder, whether in a new worktree, when) and
    each agent's chips as its last start sent them. The file is written whole, off the UI
    thread, each write after the one before. A file that does not read is dropped.
  - Only a start that goes counts: a draft closed unsent changes nothing. A project's
    orchestrator, which starts with no draft, sets the last start and leaves its agent's chips.
  - The steps list the last agent and machine first. When the last start on that machine and
    agent made a new worktree, the folder step lists that repository's "New worktree" line
    first, so ↩ ↩ ↩ makes another.
  - A draft's chips begin on its agent's last choices, each only where the machine offers it
    now; another one stays at the agent's default.
  - Tests: `slopty_client::starts::tests` (the round trip, and seeding only what is offered);
    `workspace::tests::thread_start::a_relaunch_starts_where_the_last_run_left_off`.

- ✅ **The design system starts from MonoCode's** (2026-10-06, from
  `.research/monocode-system-2026-10-06.md`, the person's rulings on it, and the tiling ruling
  in `docs/decisions/workspace.md`; amended the same day from Zed and Warp,
  `.research/zed-warp-system-2026-10-06.md` §2, since the person likes the square style). Where
  an earlier choice of ours was taste, MonoCode's wins. Where Zed and Warp are squarer and
  crisper, theirs win. Panes now meet edge to edge, so the panels, the canvas under them, the
  finish and the lit rim had nothing left to stand on. This supersedes, by name:
  - "Premium foundations: tiles stand on the canvas as panels";
  - "Premium pass from the mockups: a deeper light canvas and one green way on" (its canvas
    and its finished destructive solid);
  - "One warm neutral, white floats and raised surfaces";
  - "Two named elevations, cards on a quiet well, and the lit rim on everything raised";
  - "Hairlines one device pixel, states that ride on their plane, floats a clear step up";
  - the finished primary in "Stage 3 of the design-systems study";
  - "Pills are capsules" in the same entry;
  - "The navigator stands on the system's glass".

  What it rules:
  - **Solid surfaces only.**
    - The person dislikes MonoCode's frosted glass, the one part of it not taken. Every surface
      is solid and opaque, as in Warp and Zed: no vibrancy, no blur, no translucent sheet.
    - The navigator's system material is deleted (`slopty_platform::material`, `alpha::GLASS`,
      `Surfaces::on_glass`), and the window is always opaque.
    - Floats are their solid ground with a border and a shadow. Only the washes (hover,
      selection, the card's 3 %) are translucent, and only over a solid plane.
  - **Two opaque planes, all neutral** (Zed and Warp amend MonoCode's one ground).
    - The work lies deepest: panes and the terminal are on the ground, #171717 in dark and
      #f7f7f7 in light.
    - `chrome` is a step toward the ink, #1e1e1e and #f0f0f0. It holds the title bar, the tab
      rows, the navigator and the status bar.
    - `canvas`, `panel`, `band` and the sidebar's darker hair are gone. Every text tone is
      lifted on the chrome too.
    - `every_grey_is_a_true_neutral` holds every grey under 0.002 OKLCH chroma. Ink keeps our
      ladder, made neutral.
  - **Two lines, both 1 pt and snapped to device pixels.**
    - `sash` is 12 % of the ink in dark and 14 % in light. It runs between panes and along a
      bar's edge against a pane.
    - `stroke` is 7 %, for dividers inside a pane. `border` is 10 %, for a control's or a
      card's ring.
    - "The structural line at 1x and 2x" in `docs/MEASUREMENTS.md` has the numbers.
      - Every 1 pt line weighs the same at both scales; half a point doubles at 1x.
      - The sash weighs 1.6 to 2.1 dividers on both planes, and the two sashes land close
        across the modes.
  - **States.**
    - Hover is 5 % and a selection 8.5 % in dark, 8 % in light.
    - A list that has the keyboard shows its selected row at 14 % in dark and 12 % in light
      (`keyed`), with a square 1 pt focus line as the cursor.
    - A press is 18 % and 16 %.
    - Text is lifted to AA on the keyboard's row over the three planes a list lies on. That
      moved the light green word to L 0.465. It also moved the light crimson to
      `oklch(0.37 0.15 25)`, to stay apart from it for every dichromacy.
  - **Elevation.**
    - Gone: `Finish`, `Rim`, `Elevation.rest` and `Sunk.lip`.
    - What rests (`kit::raised`, `kit::card`) is ink at 3 % inside `border`, with no shadow.
      A secondary button is clear inside `border`.
    - Primaries are flat, and a press shows the pressed wash with no scale.
    - Floats are an opaque step: ink 5 % in dark, white in light, ringed in `border`. They
      wear Zed's two crisp layers, `0 1 0` and `0 2 3`.
    - A modal (`kit::modal`: dialogs, adding a worker) wears Zed's four modal layers, none
      blurred past 12 pt.
  - **Destructive is tinted.** The error's wash at 0.20 (0.30 under the pointer) carries the
    error's word, held to AA over the ground and over a float. `error_solid` is gone.
  - **Radii** (Zed's and Warp's compact ladder, which replaces MonoCode's 4 to 16).
    - 0: panes, bars, tabs, pane lists, tree and diff rows.
    - 2: tiny boxes such as a tab's close box.
    - 4: buttons, chips (a state's pill is now a chip, glyph and word), fields, icon buttons,
      navigator and menu rows, code and diff shells.
    - 6: resting cards, a question's card, the composer boxed in its column.
    - 8: every float (menus, popovers, the palette, dialogs, toasts).
    - Capsules only for the switch, count badges, dots, the scrollbar thumb, a progress bar's
      round ends and one-line user bubbles.
  - **Focus.**
    - The keyboard's outline is 2 pt of the accent green, whole, 2 pt clear of its control,
      and held to 3:1 on every ground.
    - The focused pane is told by its labels' tone. A tab of two or more panes adds a 1.5 pt
      green top edge to its active tab.
  - **Motion.**
    - Feedback (hover and press) takes 120 ms, eased out (`Motion::feedback`).
    - Menus, popovers and the palette open on their first frame, as Zed's do: latency comes
      first.
    - Only toasts move, 150 ms and 8 pt (`Pace::Toast`).
  - **Type and heights.**
    - Prose is 14/24 and the composer 14/22.
    - The title bar is 36 (`density.title`), a pane's tab row and its toolbar 32
      (`density.header`, `density.bar`), and the status bar 28 on the chrome. Rows and controls
      stay 28.
  - **Tiling's pieces** for the panes (`kit::pane`).
    - `pane_surface` is square on the ground, with nothing of its own.
    - `sash` is a 1 pt line, found across `density.sash` (12 pt), under the resize cursor
      along its axis.
    - Under the pointer or in a drag the line steps to the focus green at 2 pt, since only the
      green clears 3:1 on the ground; a brightened neutral stays near 1.3:1.
  - The AA floors stand everywhere.
  - **Landed after the tiling's wiring (2026-10-07):**
    - **Every pane's header is a tab row,** as Zed's is. The row is on the chrome step with a
      sash line along its foot. The shown tab stands on the pane's ground, square, a sash line
      on each side (none at the pane's own edge), over the foot line, so it opens into what it
      shows. A tile alone in its pane has a row of one tab: its lead and its title, the facts
      and the controls after it on the chrome. The starting and the out-of-reach tiles' headers
      are the same row. A tab's one tile has none since 2026-10-09: the title bar is its
      header ("A tab's one tile says its title once", below). The look is one, `workspace::tab_look`, shared with the title bar's
      tabs.
    - **Focus** is the title's tone and weight, and, while the tab on show holds two panes or
      more, a `stroke::MARK` (1.5 pt) focus-green edge along the top of the focused pane's shown
      tab. With one pane there is nothing to tell it from, and no edge.
    - **A keyboard's selection is keyed.** `kit::selected` where the keyboard is: the keyed
      wash (14 % dark, 12 % light) with the focus green's 1 pt line inside it. Elsewhere: the
      selected wash alone, no longer the hover's. The list plate follows (`Plate::keyed`). The
      navigator's row for the focused tile is not where keys go, so it wears the selected
      wash. A row chosen from its filter is, and wears the keyed one.
    - **Lines and radii.** The sash is `Surfaces::sash`, at the panes' sashes and along the
      docked navigator's edge, laid over its last point so it takes no room. The navigator is
      on the chrome step, and the transient `Surfaces::sidebar` is deleted. Cards, settings
      groups and the thread's own messages are at `radii.md`, the search field at `radii.sm`.
      A tab's close is `kit::close_box`, 16 pt at `radii.xs`. The dead `kit::panel` is deleted.
    - **The composer is squared down.** Boxed in the reading column it is `radii.md` (6) inside
      the control's ring (`border`, 10 %) on the raised step. Focus adds the card's 3 % wash
      and leaves the ring alone, where it used to step the ring up. In a narrow pane it bleeds:
      edge to edge at the pane's foot, square, under a sash line, with the tray over it a card
      of its own, not its head. Centred under an empty thread's question it stays boxed, and
      so does a view whose tile has not yet said its width. The tray's cards and its head are at
      `radii.md` with it (`kit::message::Frame`, `ThreadView::bleeds`).
    - **No green control is left.** Send is `kit::message::SEND`, a 26 pt square at `radii.sm`
      in the neutral solid whether it sends or stops (a finger's is its hit square). An
      ask's plain allow is `ButtonKind::Primary`, the solid, still last in its row and still
      what ⌘↵ answers. `ButtonKind::Go` and `kit::go` are deleted, and the lint
      `the_accent_is_never_a_control` now waives only the title bar. This is audit item 2,
      landed with the composer.
    - **Terminal blocks.** Warp's blocks were mostly there: a 7 % rule over each prompt below
      a row, edge to edge, square, and a failed block's `error_fill` wash over its head with a
      bar down its left edge. The bar is now `stroke::BAR`, Warp's 3 pt. The wash stays on the
      head and not the whole block, as ruled before: a long failure washed whole was one pink
      slab with red text on red. **Not taken: the line of air above and below a block.** The
      grid's rows are the program's rows. Air between blocks would take rows from the program
      (the PTY's height would move with the prompts on screen, a resize at every prompt) or
      push the last rows out of view. Warp can do it because it draws each block as a view of
      its own, not as one grid. A selected block is its text selection, on the terminal's
      selection colour.
  - **Still to land:** the palette with no scrim (lane A). The icons landed after it, as their
    own entry below.
  - Tests:
    - `slopty_theme::tests`: `the_work_lies_deepest_under_the_chrome`,
      `the_line_weighs_the_same_at_every_scale`, `the_ladder_is_monotonic`,
      `the_washes_stand_off_the_chrome`, `a_float_rises_and_its_rows_still_answer_the_pointer`,
      `the_focus_outline_is_the_green_and_seen_everywhere`, `elevation_and_density`,
      `chrome_text_clears_wcag_aa`;
    - `kit::tests`: `the_elevation_is_layers_of_the_shade`, `a_card_rests_on_its_edge`,
      `a_button_is_neutral_and_one_height`, `the_destructive_button_reads_in_every_state`,
      `a_floating_surface_is_rounded_lg`, `a_painted_line_is_one_point_in_whole_device_pixels`,
      `a_pill_is_a_twenty_point_chip`;
    - `kit::pane::tests`; `a11y::tests::the_keyboard_rings_a_stop_and_the_pointer_does_not`;
    - `workspace::tests::frame::the_window_is_one_opaque_ground`,
      `workspace::tests::nav_rows::a_selection_is_the_wash_docked_or_drawn`,
      `workspace::tests::focus::the_focused_tile_is_said_by_its_titles_tone_and_weight`,
      `workspace::tests::tiles::a_tile_fills_its_pane_and_its_header_lies_on_it`,
      `workspace::tab_look::tests`, `kit::message::tests`,
      `conversation::thread::tests::face::the_composer_bleeds_in_a_narrow_pane`,
      `conversation::thread::tests::face::the_one_solid_stops_a_turn_or_sends_the_draft`.

- ✅ **The chrome's icons are Tabler's, and a file's are Material's** (2026-10-07, the person's
  pick in `.research/tabler-palette-2026-10-06.md`, the stroke and the ladder from
  `.research/monocode-system-2026-10-06.md` §(f) and `.research/zed-warp-system-2026-10-06.md`
  item 15). Supersedes "The chrome's icons are SF Symbols" and the Octicons of "An icon takes
  its words' size, weight and tier". One icon family on every platform, in place of the OS's
  catalogue and a second set for git.
  - **Tabler, drawn by us on whole pixels.** Each glyph is a vendored Tabler file
    (`assets/icons/tabler/`, v3.49.0, MIT, stroked at 1.75 in place of 2), read into an outline
    and drawn on its 24 grid by `slopty_platform::outline::rasterize_on_grid`: the grid scaled
    to a whole number of device pixels and the stroke rounded to whole pixels, so at 1x a row's
    glyph has a one-pixel line where `svg()` lands a softer 1.02 px (`docs/MEASUREMENTS.md`,
    "Tabler glyphs at 1x and 2x"). It is an alpha mask in the words' ink, as before. A filled
    glyph (`-filled`) is filled. `icons::Symbol` keeps the names the chrome knew its glyphs by,
    and the files' `NOTICE` pairs each name with its file.
  - **An icon fills its slot.** The grid spans the slot: 14 pt inline (in bars, buttons and
    beside a row's facts), 16 pt as a row's lead, `kit::NOTICE_MARK` in an empty state, the
    ladder Zed and `MonoCode` use. A disclosure chevron is on the 12 pt grid in the inline slot,
    and turns while it opens. There is one stroke whatever the words' weight, as in both
    references. A slot's weight, the 12.5 pt floor and the fitting of wide symbols were SF's
    and are gone.
  - **Git is Tabler's git glyphs** (`git-branch`, `git-pull-request` and its draft and closed,
    `git-merge`, `git-commit`, and `book-2` for a repository), with the states' inks as before.
    The Octicons are deleted.
  - **A file wears its type's Material icon in its own colours** (`assets/icons/files/`,
    Material Icon Theme v5.39.0, MIT): in trees, tabs, the palette and tool rows, as `MonoCode`
    shows them beside Tabler. A file is known by Material's own names first, then its
    extension (`FileType::of`). Code in a language with no icon kept leads with the code glyph,
    and an unknown type with the plain document, so an unknown type never reads as a known one.
    GPUI's SVG renderer draws an icon at the device size it is painted at, so nothing is
    resampled. Where Material draws an icon for a dark ground and keeps a variant for a light
    one (bun, deno, toml), a light theme takes the variant. This reverses "no type is drawn in
    colour": the reference that set the bar uses the colour to tell a file's kind at a glance.
  - **Nothing is drawn ahead but cheaply.** A glyph draws in about 8 µs, so the first frame
    draws its own. What it would pay is reading the files and Core Graphics' first context,
    about 6 ms, so `icons::prewarm` does that on a thread while the window is made
    (`docs/MEASUREMENTS.md`, "Tabler glyphs drawn on demand"). The SF list written down for the
    next launch is gone with SF.
  - **Deleted with it:** `slopty_platform::symbols` and its pool test, the objc2 features only
    it used, `icons::{Weight, Scale, SymbolSize, SYMBOL_FLOOR, weight_beside, remember}`,
    `palette::lead_slot_weighted`, the floor's lint `kit::tests::a_symbol_is_never_drawn_under_12_5`,
    `assets/git/` and five Tabler files nothing drew. The kit's
    and the icons' last zoom arguments went in the same change (`kit::{notice, notice_mark,
    pill, pill_frame, typed, text_toggle, tick_box}`, `icons::{status_mark, notice_status}`,
    `ChromeText::new` and `FindBar::zoom`), since the chrome no longer zooms.
  - Lints: `icons::glyphs::tests::every_glyph_reads` (every glyph reads on its grid, filled or
    stroked as its name says, and every file kept is drawn and listed),
    `file_types::tests::every_file_icon_is_drawn_and_none_is_left_over`,
    `kit::tests::the_chrome_draws_its_own_icons` (no SVG drawn outside the icon modules but the
    app's mark) and `kit::tests::an_agents_mark_wears_no_colour_of_its_own`.
  - Tests: `icons::glyphs::tests::a_glyph_is_drawn_on_whole_pixels`,
    `icons::tests::{an_icon_fills_its_slot_on_the_ladder, a_file_leads_with_its_types_icon,
    a_file_icon_is_drawn_at_its_size_in_colour}`, `icons::git::tests::each_git_glyph_is_tablers`,
    `file_types::tests::a_file_is_known_by_its_name_then_its_extension`.

- ✅ **A turn's work is one line, open while it runs** (2026-10-07,
  `.research/ui-audit-monocode-2026-10-06.md` items 5 and 6, M14 and M15). A settled turn's
  work already folded to one line over its answer, but the turn under way drew every call
  loose, so a long turn was a wall of calls, and the line it would fold into only appeared
  once it ended. MonoCode's transcript keeps one activity line per turn: open while the turn
  works or waits on the person, folded once it is done.
  - **The turn under way has its line too** (`rows::turn_rows`). Its work shows under the line,
    open, as it comes, and two or more quiet calls in a row there are still one line of their
    own. The line says what the work did so far ("Working: Read 3 files · Ran a command",
    `Fold::running`), with no time; the working row at the foot keeps the clock. A steer splits
    it as it splits a settled turn's. A click folds it (`ThreadView::shut`), and once the turn
    settles it folds unless the reader opened it then. Find opens either kind
    (`ThreadView::open_turn`).
  - **A call is one line: its kind, its verb and file, how it stands at its end**
    (`view/tools.rs`). The lead is the kind's mark, or the file's type for a call on one file,
    and no longer turns red: the spinner while it runs and the amber mark while it waits stay.
    A call on one file says its verb in the tense of how it stands ("Read", "Edited",
    "Editing", "Edit" while it asks), then the file named first. The name opens the file in a
    tile of its own on the thread's machine (`ThreadViewEvent::OpenFile`, a path under the
    agent's folder made absolute); the rest of the line opens the call's diff or output in
    place, as before. A failure is marked at the line's end, an ✕ before "Failed" in the error
    tone; "Not allowed", "Stopped" and "Waiting for you" stay words. The line reads aloud as it
    reads ("Read src/lib.rs", "Count lines, Failed").
  - Kept: a command is still its own words (7052), the body's well still has no border, and
    Allow and Deny still sit under a waiting call. Nothing here animates as it arrives, so a
    turn read from history draws as a live one does.
  - Tests: `rows::tests::{the_live_turns_work_is_open_under_its_line_and_it_says_it_works,
    quiet_calls_in_a_row_are_one_line_in_the_live_turn,
    a_turn_left_open_folds_once_the_next_begins}`,
    `thread::tests::steps::{a_turns_work_is_open_under_its_line_while_it_runs_and_folds_when_done,
    a_call_names_its_file_which_opens_and_a_failure_is_marked_at_its_end}`.

- ✅ **The composer's ledge says where the work is** (2026-10-07,
  `.research/ui-audit-monocode-2026-10-06.md` item 7, M10). The place, a new worktree's base and
  the meter stood in the composer's foot among the model and the mode. The place was the first
  thing to leave for want of room, and the meter wandered to wherever the foot's end fell.
  MonoCode's composer keeps a ledge over its field for where the work is, with the context meter
  at its right, and a foot for how the agent works.
  - **The ledge** (`ThreadView::ledge`, `thread-ledge`) is a row at the top of the composer
    card. On its left are the place chip and, beside it, the branch: a new worktree's base as a
    switch ("from main"), or the branch checked out, as words, while the draft starts in the
    folder itself. The meter sits at its right. Nothing on it leaves for want of room: the place
    gives up its width first and the meter keeps its own.
  - **Only while it says where.** A thread with rows has no ledge: its tile's header already
    says where it is, and a row over the field for the meter alone took the thread's height for
    one figure (in the e2e window it pushed the prompt's picture out of view). Its meter stays
    at the foot's end, as before (`ThreadView::on_ledge`).
  - **The foot** keeps the "+", the model, the effort, the mode, the background work, the pull
    request, the changes, the screen, the handoff and the send. It still leaves the least
    needed first into the "+" menu, which no longer lists a place or a base.
  - Deleted: `FOOT_PLACE`, `FOOT_BASE` and their "+" menu rows with `PLACE` and `BASE_BRANCH`.
  - Tests: `thread::tests::face::a_narrow_foot_keeps_send_and_hands_the_rest_to_the_plus` (a
    thread with rows has no ledge) and
    `workspace::tests::thread_start::the_place_chip_starts_a_worktree_from_a_branch_picked`
    (the place on the ledge, the branch checked out as words).

- ✅ **One reading measure, MonoCode's** (2026-10-07, `.research/ui-audit-monocode-2026-10-06.md`
  item 8, M12). The transcript, the tray and the composer already shared one centred column,
  and the tray's code already parted its groups by a base unit of space rather than the
  hairlines "The tray's sections each stand apart" ruled, as MonoCode's stack does; that
  entry's hairlines are superseded here. What was left was the measure: 736 pt of text against
  MonoCode's 896 pt column (`max-w-4xl`) with 16 pt pads on its rows.
  - `thread::view::COLUMN` is 864 pt, MonoCode's column less its pads. The gutters by room are
    unchanged (48, 24, 16), so a wide tile's column is 960 pt and a narrow one gives way as
    before. A Markdown file's preview keeps to the same measure (`file::reading`), so a file
    still reads as an answer does.
  - The queue, a question and a limit stand over the composer in the tray, on the same column.
    A question taking the composer's place is item 18.
  - Tests: `conversation::thread::tests::steps::the_column_s_gutter_follows_the_tile_s_room`
    and `file::tests::reading::the_preview_keeps_to_the_reading_measure`, both read
    `COLUMN`.

- ✅ **The latest turn ends in the files it changed** (2026-10-07,
  `.research/ui-audit-monocode-2026-10-06.md` item 13, M18). What a turn changed was said only
  by the composer's changes chip and, in a thread with no composer, a row of the tray. Keeping
  or putting back a change meant opening the review tile. MonoCode ends the latest turn in a
  card of its changed files, with Undo, Keep and Review.
  - **The card** (`view/changes.rs`, `Row::Changes`) stands under the latest turn's answer
    once the turn settled and its edits were made. Its head says "Changed 2 files" and the
    lines added and removed, then Undo, Keep and Review. Under it are the files, the first
    three and "Show N more files", each with its type's icon, its path and its lines. A file
    opens the review, as Review does. The review tile stays the place for a whole review.
  - **Keep and Undo act on the turn's own review** (`ReviewScope::Turn`), which the card asks
    once as it shows where the worker snapshots the tree (`Cap::SNAPSHOTS`). Keep sends
    `Intent::Keep`, and Undo `Intent::Revert`, for each file as that review showed it, with its
    blobs as the stamp, so the worker refuses a file that changed since. Either done, the card
    goes (`ThreadView::kept`). A refusal is said in the tray, as the review tile's are. Without
    snapshots the card offers only Review.
  - **The client keeps a review per scope** (`slopty_client::threads::Threads::review`). It kept
    one per thread, so the card's turn review and an open review tile's scope would each have
    replaced the other's. The review tile reads the scope it asked.
  - Deleted: the tray's edits row (`edited_section`, `thread-edited`), which the card replaces
    where the thread has no composer.
  - Amended 2026-10-07 (golden review): the card stands right under the answer's line of
    actions, which already parts it from the words; a paragraph's step on top of that left it
    48 pt adrift. A file outside the agent's folder is named by its name alone, not by its
    whole path from the root (`changes::shown_path`).
  - Tests: `thread::tests::steps::the_latest_turn_ends_in_its_changed_files_which_keep_from_there`,
    and `workspace::tests::thread_start::a_pull_request_is_reviewed_in_a_worktree_that_checks_it_out`
    now counts the review tile's asks apart from the card's.

- ✅ **A diff is unified, its files stacked and folding** (2026-10-07,
  `.research/ui-audit-monocode-2026-10-06.md` item 16, M22). The review tile laid a wide room's
  lines out side by side and a narrow one's in a column, so the same change read two ways and
  a comment's lines were picked by pairs in one and by lines in the other. MonoCode's diff is
  unified only, its files stacked, each folding to its head, with a switch for all of them.
  - **Unified at every width.** `Row::Pair`, the side-by-side row, `lines::Ink::split` and
    `diff::pairs` are deleted. A comment's lines are always a run of the hunk's lines.
  - **A file folds to its head.** The head's name, folder and counts are one button with a
    chevron, which folds the file to its head or opens it. Keep and Revert stay beside it.
    What is folded is held by path (`ReviewView::folded`), so it lasts through the review's
    updates and a change of span. Picking a folded file in the list opens it.
  - **One switch for every file**, at the scope bar's end beside the refresh, as MonoCode's
    diff head has: "Collapse all files" while any is open, "Expand all files" once all are
    folded. It never leaves the bar for "More". Two Tabler glyphs came in for it, `fold` and
    `arrows-move-vertical`.
  - **What was already there.** The files were stacked, a hunk had its own Keep and Revert
    (the thread's stage of a hunk), and a comment was written inline under its lines.
  - **The unchanged lines between hunks fold** to one line each, "27 unchanged lines", before
    the first hunk and between two, and past the last once the file's length is known
    (`review::view::gaps`). A hunk carries three lines of context and nothing else of the file
    reaches the client, so opening one asks the worker for the file's new side whole, by the
    blob the review named: `ContentRef::blob(id)` through the thread's own
    `ThreadRequest::Expand`, which the worker's snapshots answer from git (`Snapshots::blob`)
    on a task of its own, so the thread's frames go on. No wire type changed. A side comes
    once per blob and is kept while the tile is open; one that is not UTF-8 or longer than
    `EXPANDED_CHARS` comes as gone, and its folds stay shut. A folder's review has no thread to
    ask through, so it shows no folds. (Added 2026-10-07.)
  - Tests: `review::tests::the_diff_is_unified_at_every_width`,
    `review::tests::files_fold_to_their_heads_one_or_all`,
    `review::tests::the_lines_between_hunks_open_from_the_file_s_blob`,
    `review::view::gaps::tests::the_stretches_lie_between_the_hunks`, and on the worker
    `review::a_review_s_sides_read_back_whole_from_their_blobs` (`crates/slopty-worker/tests/review.rs`).

- ✅ **A question is answered from the keyboard, or skipped** (2026-10-07,
  `.research/ui-audit-monocode-2026-10-06.md` item 18, M20). An agent's questions already
  stood in the tray directly over the composer (item 8), one at a time, with ↑↓, a digit, ↵,
  ⌘↵ and "Other". MonoCode's question form adds Home and End, says "Select all that apply"
  under a question that takes several answers, and lets a question be skipped.
  - **Home and End** go to the question's first answer and its last while an answer has the
    keyboard (`questions::Questions::to_end`); the field for one's own answer keeps them for
    its text.
  - **"Select all that apply"** stands under a multi-choice question's text.
  - **Skip** sits with the ways on. A question is now optional in the questionnaire, so Skip
    can pass it over, and going on (↵, ⌘↵, Next, Submit) still needs an answer, since the
    questionnaire holds an optional question left empty as unanswered. A question skipped is
    answered with nothing, and the answered line says "Skipped".
  - The composer stays under the question, as in MonoCode, so a message can be written in
    place of an answer.
  - Test: `thread::tests::questions::a_question_is_walked_by_home_and_end_and_skipped`.

- ✅ **The prompt outline stands at the transcript's right edge** (2026-10-07,
  `.research/ui-audit-monocode-2026-10-06.md` item 20, M19). The old conversation face had a
  rail of prompt ticks ("The prompt rail is a cached view of its own"), and it went with that
  face, so the thread view had no outline. MonoCode's sits at the transcript's right edge,
  centred on its height, and hides in a pane under 58 rem.
  - **A bar per prompt** (`thread::view::outline`), 2 pt thick and 11 pt long, 10 pt apart,
    in a stack at most 330 pt tall or three quarters of the transcript. The prompt in view is
    lit: the last at the transcript's end, else the topmost one in view, else the last above
    it. Past what fits, the gap closes to a point first, then a window of bars slides to keep
    the prompt in view inside it, the newest preferred.
  - **The pointer** on a bar lifts it to 24 pt and its two neighbours each side less, as a
    dock magnifies, and shows a card to its left with the prompt's start and its answer's,
    two lines each. A press takes the transcript to the prompt.
  - **It needs two prompts and a tile 928 pt wide** (`outline::OUTLINE_FROM`), so it never
    crowds the reading column.
  - Tests: `thread::tests::steps::the_prompt_outline_takes_the_transcript_to_a_prompt`,
    `thread::view::outline::tests::the_stack_closes_its_gap_then_slides` and
    `a_bar_lifts_with_its_neighbours`.

- ✅ **Motion's numbers are MonoCode's** (2026-10-07, `.research/ui-audit-monocode-2026-10-06.md`
  item 19, M5, M28, M40). The motion tokens held a feedback, a fade, a toast, an exit, a settle
  and a sheet. MonoCode's motion also has a new pane sliding in over 260 ms, a tab closing over
  200 ms, an approval card rising over 180 ms, and a sent prompt's turn rising 10 pt over
  320 ms. Every one of them has its reduced-motion block.
  - **The tokens** (`slopty_theme::Motion`): `toast` is 180 ms (it was 150), for a toast, a
    notice or a card; `pane` is 260 ms; `reveal` is 320 ms; `prompt_rise` is 10 pt. A tab
    closing takes `sheet`, already 200 ms. The paces `kit::Pace::Pane` and `kit::Pace::Reveal`
    put the new two on the ease-out curve.
  - **A sent prompt rises.** The row of what the person just sent (`Row::Sending`) comes up
    10 pt from below as it fades in, over `reveal` (`kit::slide_fade`). The agent's echo
    replaces it in place, so the prompt does not move again.
  - **Reduce Motion lands each at once**, as `kit::motion` gates every pace.
  - The pane's slide and the tab's close draw in the tiling area and the title tabs, which
    take `Pace::Pane` and `Pace::Sheet` as they come to it.
  - Tests: `slopty_theme::tests::a_curve_runs_from_rest_to_landed` and
    `kit::tests::the_paces_are_the_motion_tokens`.

- ✅ **A pane of tabs names no place, and alike files by their folders** (2026-10-07, golden
  review). A pane of tabs put the shown tile's place after its tabs, so a terminal's row ended
  in a lone "~" far from anything, read as a stray. Zed's tab bar says no place at all.
  - **No plain place after the tabs.** A shell's prompt says where it is, and the title bar's
    project says which project. A lone tile's header keeps its place beside its title, as
    before.
  - **A page keeps its address after the tabs,** since it is a control: a click turns it into
    the address field (`a_tabbed_page_keeps_its_address_after_the_tabs`).
  - **Files that share a name show their folders,** dimmed after the name inside each tab, as
    Zed's tabs do, by the name alone rather than "lib.rs 2". A file whose name is its own in
    the row shows none. The tab's spoken name carries the folder too ("lib.rs, src").
  - Test: `workspace::tests::tiles::a_tab_row_names_folders_only_where_files_read_alike`.

- ✅ **A foot bar holds the plan, the agent and what runs out of sight** (2026-10-09, MonoCode
  audit row 3, M8; reverses the bar half of "No bar along the bottom").
  - **Where.** A bar the status step tall (`Density::status`, 28 pt, 32 on touch) along the
    window's foot, under the panes and beside the navigator, on the chrome with a hairline
    over it (`workspace::foot`). It is a cached view of its own, drawn again on the
    workspace's news and when the shells running out of sight change, not on a clock.
  - **What.** On the left: the plan's usage on the focused tile's machine and agent, always
    shown while a reading exists, in `warn` from 80 %, and a click lists every machine's
    readings in a popover above it. Then the focused agent: its mark, its name, and how it is
    doing unless at rest. On the right: the ports forwarded here, the transfers in flight, the
    frame time with the stats, a chip for each shell of the project on show whose command runs
    out of sight, and the toggle of the tab's terminal.
  - **Why it came back.** The old bar was struck because it said a worker's name twice and
    said it whatever it had to say. This one says what nothing else on screen does. The plan's
    usage under 80 % had no place at all, and MonoCode keeps it in view. A running shell has no
    chip while its own header is on show, so nothing is said twice.
  - **The title bar keeps only what warns:** the server out of reach, a relay link, a newer
    build. The plan, the ports, the transfers and the frame time moved down, and their popovers
    rise from the foot bar's ends.
  - **No foot bar on a phone.** Its foot is the key bar, and its title bar already holds the
    focused tile. The iPad keeps it above the home indicator.
  - **The bar pads for nothing under it** (2026-10-09, from the goldens retake). The app lays a
    band under the workspace over the home indicator and a soft keyboard, so a bar that also
    added the bottom inset stood a keyboard tall on an iPad (golden `ios-pad-panes.png`). The
    bar is the status step tall and no more, and the band under it takes the chrome while it
    is drawn.
  - Tests: `workspace::tests::foot::*`,
    `workspace::tests::chrome::the_panes_meet_the_foot_bar_and_a_notice_sits_by_its_work`.

- ✅ **What an agent out of sight waits on is a card at the panes' top right** (2026-10-09,
  MonoCode audit row 4, M21 and M35; reverses "a pointer, never an answer" for a plain yes or
  no, as `workspace.md` 2026-10-04 already let one be answered wherever it is seen).
  - **Where.** Cards 360 pt wide stack under the title bar at the panes' trailing edge, the
    newest on top, at most three (`workspace::approval_cards`). The rest wait under *Needs
    you*, which stays the full list. A card rises in over the toast's pace, and arrives at
    once under Reduce Motion.
  - **What.** The agent's mark and the thread's title, "Approval" in the warn tone, up to
    three lines of the request, the agent and its machine, then Deny and Allow across the
    foot. Allow is the neutral solid primary, as every primary is, never green. They answer
    with the request's plain allow or deny, once (`approvals`). A question, a plan or a form
    is answered in its thread, so its card has one button, Open thread.
  - **When.** Only while the agent's tile is not on show. A tile on show asks in its own tray
    or TUI. A card goes once its request is answered, here or anywhere. Its close puts it away
    for good. A project muted in the navigator raises none, which is what the mute is for.
    A phone has none: its notes and *Needs you* say it.
  - Tests: `workspace::tests::approval_cards::*`.

- ✅ **The settings are a page in the panes' place, and the navigator lists their sections**
  (2026-10-09, MonoCode audit row 9, M26; supersedes "Settings open in a dialog as tall as the
  file").
  - **Where.** The app hands the workspace its settings editor, and the page takes the panes'
    place under the title bar (`workspace::settings_page`). No dim and no dialog. The foot bar
    goes with the panes, as MonoCode's usage footer does while its settings are open.
  - **The section list.** While the navigator is docked, its body becomes the settings'
    sections; its top row stays. The search sits at the list's top. The sections follow under
    three group names: App (Appearance, Terminal, Input), Machines (Streams, Network) and Help
    (Keyboard, About, which the Help menu opens). Each section has its Tabler glyph. "Edit as
    TOML" and Back sit at the foot. MonoCode groups App, Agents and Workspace. No setting here
    is an agent's or a workspace's, so neither group is made up.
  - **Without a docked navigator.** With the navigator hidden or over the panes, the page
    carries the same list as its own sidebar. A window too narrow for it lists every section
    in one column, with the file and Done in the head. The list is one builder over the form,
    drawn in either place (`SettingsForm::sections`).
  - **Leaving.** Back, Esc and ⌘↩ leave, writing first what was waiting for a pause. So does
    turning to the work: a press on a title tab, any action on the tiling, or the bell.
  - Tests: `workspace::tests::settings_page::*`, `settings_editor::tests::*`,
    `settings_form::schema::tests::the_groups_list_the_sections_in_order`; the app's
    `a_settings_change_applies_with_the_page_still_up`.

- ✅ **A window too narrow to dock the navigator folds it to the rail** (2026-10-09, MonoCode
  audit row 30, M6 `CompactProjectRail`; the rail was only the hidden navigator's before).
  - Where the navigator would leave the panes less than a phone's width, so it cannot dock,
    the rail of project glyphs keeps its column. Before, nothing stood there until the panel
    was opened over the panes. The panel still opens over the panes, and the rail stays under
    it, so the panes do not move as it opens. A phone keeps its drawer and no rail.
  - The rail is now a view of its own (`Region::Rail`), drawn while the panel is open.
  - **A machine stands for its loose tiles.** A healthy worker used to be left off the rail
    until something of its own waited, so a window whose work sat on one machine, in no
    project, showed an empty 40 pt column (golden `workspace.png`). A worker holding tiles of
    no project now has its glyph there, as a project has, and its rollup once something waits.
  - Tests: `workspace::tests::frame::a_narrow_window_folds_the_navigator_to_its_rail`,
    `workspace::tests::chrome::the_rail_shows_a_worker_with_its_own_tiles_and_what_waits`.

- ✅ **The settings gain an Agents page** (2026-10-09, readiness item 7, MonoCode audit row 9's
  missing group).
  - **What.** First under Machines, before Streams and Network, with the agent glyph: the ACP
    agents beside the ones Slopty knows (`worker.acp`), the bounds every project stays under
    (`server.projects`: live agents, looser permissions) and how notes reach a pocketed phone
    (`server.push`: the relay, or an APNs key with its IDs), each its own group.
  - **Why.** These keys stood at the foot of Network, under their tables' titles, among the
    addresses and the server, where nobody looking for an agent's setting would look. The
    agents are what the app is for, so their page comes first among the machines'.
  - **Not on an iPhone or iPad.** Every key on it is a worker's or the server's, which a phone
    or a tablet does not run, so the section is not listed there (`Section::ALL` is the
    platform's).
  - Tests: `settings_form::schema::tests::the_agents_page_holds_the_agents_projects_and_notes`,
    `settings_form::tests::the_agents_page_writes_the_projects_and_the_notes_tables`.

- ✅ **A tab's one tile says its title once** (2026-10-09, readiness item 9).
  - **What.** A tab holding one tile, with no other pane zoomed away, draws no pane header.
    Its title bar tab already said the title, and the row of one tab under it said it again
    32 pt lower and took that height from the body. The body now starts at the area's top,
    under the bar's tab, which opens into it as a pane's shown tab did.
  - **Where the rest of the header went.**
    - The bar's tab says the title, then "Edited" for a file with an edit not on disk, then
      where the tile is (a file's folder), muted and giving way first. It leaves its agent's
      mark to the strip, whose glyph says it with its words and its hint. The tab grows to 340 pt
      while it says more than the title.
    - The breadcrumb already names a shell's machine, checkout and branch, so nothing else
      repeats them.
    - A strip at the bar's trailing end, before the notices, holds the rest: a page's address
      (its field while typed in), an agent's place, pull request and worktree, an upload, the
      kind's states, how the tile is doing, its readouts, then its controls (the kind's
      actions, the face toggle). The controls stand at rest, as the bar's buttons do. A strip
      that showed them only under the pointer would have nothing to point at while it held no
      facts. Close is the tab's.
    - The strip is the tile's heading to a screen reader, named as its header was. A right
      click on it opens the tile's menu.
    - A double-click on a tab names its focused tile. For a tab's one tile the field stands in
      the tab, in the title's place.
  - **When it comes back.** A second tile in the tab, as a split or as a tab of the pane,
    brings the pane headers back and empties the strip. A zoomed pane over others keeps its
    header. A phone is unchanged: its bar was the focused tile's already.
  - **Drawing.** The strip is a view of its own (`Region::TileStrip`), always in the bar and
    empty while the tab holds more. It is told the news a header is told
    (`WorkspaceView::panes_news`), so a header's news never builds the bar. Every header reads a
    program's progress report from the workspace's copy (`facts.progress`), never the shell.
  - **The copy, for the panes too** (amended after the gpui-fast bump to `fa876f7c`). Before
    that bump a pane's header had to read the shell, because with the copy gpui-fast's splice
    of a shell's cached view into panes it had not built failed (`Window::splice_gaps`, an
    invalid taffy key). Its cached-layout fix (longbridge#52) ended that, so the panes read the
    copy too, and a line of output builds only its shell: over five lines the panes were built
    five times while their headers read the shell and not at all with the copy, the shells
    beside it never (`workspace::tests::retained::a_shells_output_builds_only_that_shell`;
    `docs/MEASUREMENTS.md`, 2026-10-09).
  - Tests: `workspace::tests::lone_tile::*`; the header tests give their tile a neighbour
    (`tests::paired`).

- ✅ **A task asked to start again says so at once** (2026-10-09, readiness item 13, the board's
  half of R22's `Verb::TaskRestart`). The board already offered "Start fresh" and "Give to
  another agent…" among a stood-on task's controls (`docs/decisions/projects.md`, "A task starts
  fresh, or goes to another agent"). The card said nothing until the server's board moved, which
  can take a whole agent start, so a second press looked needed.
  - **The intent.** The moment either is asked, the card's first detail line says
    "Starting #3 fresh…" or "Handing #3 to Codex…", in the secondary ink, as a status a screen
    reader is told. Neither control is offered again on that task while it shows, and the
    palette's ask says the task is starting again. The board holds the ask with the agent's
    terminal or seat at the time, so the line ends when the card shows a new one, or when the
    task merges or leaves the board. A refusal (or no server linked) is said in the server's
    words, and the card goes back to what it said (`ProjectView::handing_refused`).
  - **The palette.** "Start the task fresh" and "Give the task to another agent…" act on the task
    the board stands on, as its other task commands do, with no key: each is rare. The second
    opens the card's agent picker.
  - Tests: `workspace::tests::projects::a_task_starts_again_and_says_so_at_once` (both verbs from
    the card and the palette, the line before the answer, the refusal, the new agent ending it);
    `project::tests::a_task_starts_fresh_or_goes_to_another_agent`. The keyboard settings golden
    lists the two commands.

- ✅ **A program's status is its tile's state** (2026-10-09, readiness item 11, the UI half;
  the wire is `terminal.md`'s "A program's status records (`OSC 7501`) are session state on the
  wire").
  - **Needs you.** A record waiting on the person (`blocked`) makes its terminal's tile "needs
    you" wherever the tile's state shows: its header, its tab, the navigator, the foot and the
    bar. The word says what it needs as a waiting agent's does: "Needs approval" for
    `permission`, "Has a question" for `question`, "Needs a sign-in" for `auth`, "Needs you" for
    a need the client does not know.
  - **A result until looked at.** A `done` or `error` record is the tile's "done" or "failed"
    until the tile takes the focus, a failure first. One that arrives while the tile has the
    focus is looked at already. The records stay on the worker; what the person has looked at
    is the client's (`WorkspaceView::program_seen`).
  - **Its words.** The tile's row says what the program says under its name: the waiting
    record's message, else one at work, else an unseen result's, each its message or else its
    title, in place of the command the shell ran.
  - **Precedence.** An agent's own adapter leads, being the richer source; a record outranks
    what is only inferred (a command's exit, a finished command not looked at). The records come
    with the session's summary, so a tile not viewed shows them.
  - **It notifies as an agent's need does.** A waiting record where no agent speaks for the
    tile is on the attention ladder (`needs_you`): the bell and the badge count it, ⌘⇧A walks to
    it, a tile not on show gets the corner's word, it sounds per the alert setting, and while
    the app is away it posts the tile's one Time Sensitive note in the record's words, taken back
    once it no longer waits. The server never hears the records, so the note is this client's
    own even while a server leads; a phone pocketed with its link gone hears nothing of it,
    since the push path starts at the server's ladder.
  - **Rate-limited.** A program is any program, so as the ruling asks, its sound and its note
    come at most once in `attention::PROGRAM_QUIET` (10 s) per tile, however often its record
    flips; the tile's state follows every flip.
  - Tests: `workspace::tests::program_status::a_programs_status_is_its_tiles_state` (needs
    you and its word, an unknown need, results and a failure first, looked at on focus, a result
    arriving while focused, the row's words; each frame the one drawn from scratch) and
    `a_program_waiting_on_the_person_notifies_as_an_agent_does` (the bell and badge, one sound,
    the note while away with a server leading, none again for a progress update, taken back
    when answered, and a flip back inside the quiet neither sounding nor notifying).

- ✅ **A subagent's thread follows its newest row from its first frame** (2026-10-10, readiness
  audit item 1).
  - **The defect.** Opened from a thread scrolled up, a subagent's thread was only scrolled to
    its end, so the list stayed out of tail-follow until a layout found it at the end. The frame
    its rows arrived in read the marks before that layout and drew the way down; the next frame
    took it away. A dump between the two frames saw a stale 28 px button
    (CI run 37950895313, `conversation::a_subagent_has_a_thread_of_its_own`).
  - **The fix.** Opening a subagent's thread sets the list to follow its newest row
    (`FollowMode::Tail`), so its first frame and every frame after it agree. The thread above
    remembers whether it followed: on the way back it follows again, or stands where the reader
    left it.
  - Test: `conversation::thread::tests::face::a_subagent_s_thread_opens_at_its_newest_row_without_the_way_down`
    (a short and a long subagent's thread: the first frame of the opening and of its rows
    holds the marks the layout keeps; the way back; a followed thread followed again).

- ✅ **A big review shows every file it leaves out, and leaves out the ones read last**
  (2026-10-10, readiness audit item 6).
  - **The defect.** A review carries at most 20 000 diff lines (`REVIEW_LINES`). The worker
    spent them in path order while the list sorts the heaviest file first, so on a big branch
    the files at the top of the list were the ones that came with no hunks. Their rows said
    "No lines to show", though `Patch::clipped_lines` said how many there were.
  - **The budget goes in reading order.** `slopty_proto::thread::wire::reading_order` is the
    list's order: the heaviest first, tests, fixtures, locks and generated code after the rest,
    then by path. The worker spends the budget in it (`repo::snapshot::spend`). A file past
    what is left keeps its counts, and a lighter one after it may still fit, so a big lock is
    left out before the source it would have crowded. `quiet` moved from the review's model
    into the wire crate so the worker and the list share one rule.
  - **Said, then read whole on a press.** A file left out reads "25000 lines of changes, past
    what one review shows at once", with "Show this file's changes". The press asks the
    repository for that file alone by the blobs the review named (`GitOp::FileDiff`, answered
    with `GitDone::FileDiff`). It is cut with the worker's own `diff`, so its hunks number as
    the review's do, and its folds, comments and picks work as on any other file. A folder's
    review has no thread, and a thread's review is always in a git repository, so one git op
    serves both through the reviewed folder. "Reading this file's changes…" shows while it
    comes, and a failure says why in git's words and offers the press again. What came is
    kept in the hub's git book by its pair of blobs, so a review read again keeps it.
  - **Not taken.** Fetching both sides through the thread's `Expand` and diffing on the client
    would not reach a folder's review, and a second diff algorithm could number hunks
    differently from the worker's.
  - Tests: `units::a_review_is_read_by_weight_with_the_quiet_files_below` (slopty-proto, the
    order), `review::a_big_review_spends_its_lines_in_reading_order_and_a_file_comes_whole`
    (slopty-worker, on a real repository: the source shown and the lock left out where path
    order did the reverse, a file past the budget whole by its blobs, a name that is no
    object id refused), and
    `review::tests::a_file_past_the_review_s_budget_says_so_and_comes_whole_on_a_press`
    (slopty-ui: the row, the ask by blobs in the thread's repository, reading, the hunks in
    its place, a failure said with the press again).

- ✅ **A phone's desktop comes at its own size, and a display pick is kept** (2026-10-10,
  readiness audit item 12).
  - **The defect.** A desktop opened from a phone came at the Mac display's size, letterboxed
    to a strip. The display made for the device was a palette toggle held in memory, so iOS
    killing the app in the background lost it, and the person had to find the palette line
    again on every launch.
  - **Touch takes the device's size unasked.** On iOS (`Desktop::sized_first`) a display tile
    on show whose worker can make displays takes one made for the device at the next frame
    (`adopt_sized_displays`), and its physical display is never opened meanwhile
    (`awaiting_sized` holds it out of `reconcile_screens`). One key has one display, so one
    tile per worker takes it and the rest stream their physical displays. A worker that
    refused to make one, or a device with no key kept, falls back to the physical display and
    is not asked again until it links again. On the Mac nothing changes unasked.
  - **The pick is kept.** "Open a display sized to this window" and "Back to the physical
    display" record the person's pick for that tile in `layout.json` (`Saved::displays`), only
    where it differs from what the device does unasked, so the next launch starts from it on
    either device.
  - Tests: `workspace::tests::desktop::a_touch_device_opens_a_desktop_at_its_own_size_and_keeps_the_pick`
    (the made display asked for at the tile's size, no physical open first, the pick back to
    physical saved and read back, and an unasked pick not kept) and
    `a_display_made_for_this_device_follows_its_tile` (a Mac's pick kept).

- ✅ **The commit sheet frees a merged pull request's worktree** (2026-10-10, readiness audit
  item 22).
  - **The gap.** Once an agent's pull request merged, the sheet showed it merged and offered
    nothing more. "Remove worktree" lived only on an exited thread, a folder tile's path bar
    and the palette, so the person had to go looking for it at the moment it was due.
  - **The press.** With the pull request merged and the repository an agent's worktree under
    its clone's `.claude/worktrees/`, the sheet's pull request part ends with "Remove worktree"
    (the exited thread's words). The sheet tells its tile (`CommitEvent::RemoveWorktree`), and
    the thread's or the review's tile hands it to the workspace (`ThreadViewEvent` and
    `ReviewEvent::RemoveWorktree`), which asks it as the palette does: refused in words while
    an agent that has not exited works there, and refused by the worker while a terminal works
    in it or anything in it is not committed. "Removing the worktree…" shows while it is
    asked, and once it went the press goes and the sheet says what went.
  - Tests: `conversation::thread::tests::commit::{a_merged_pull_request_offers_to_remove_its_worktree,
    a_merged_pull_request_outside_a_worktree_offers_no_removal}` and
    `workspace::tests::review_tile::a_reviews_sheet_frees_a_merged_worktree`.

- ✅ **A stream the worker ends on its own reopens once, then says it stopped** (2026-10-10,
  readiness audit item 20(a)).
  - **The gap.** A capture that stopped on its own (`ScreenEvent::Closed` this client did not
    ask for) dropped the tile's picture and waited on nothing: the tile said "Opening …"
    until some unrelated change asked for its stream again.
  - **The rule.** The picture still goes with the stream, and the tile asks for it again at
    once, so a passing stop (a display that slept, a capture the system restarted) heals
    without a press. One that ends again within `STOP_AGAIN` (10 s) of the last is not asked
    for again: its pane says "Window 7 stopped" with the worker's words under it ("studio ended
    its stream: …") in the failed open's layout, the header's slot stops turning, and
    "Reopen" asks for it once more. A window that closed for good reopens into the worker's
    "no longer available", which offers another. The next link clears it, as it does a
    failed open (`Worker::stopped`).
  - Test: `workspace::tests::bodies::a_stream_the_worker_ends_reopens_once_then_offers_to_reopen`.

- ✅ **The sweep takes the agents' worktrees and says whose they were** (2026-10-10, readiness
  audit item 18, the client half).
  - **The rule.** The worker now lists every linked worktree, each marked by the agent whose
    tool made it (`AgentWorktree::made_by`). "Remove merged worktrees" takes the merged ones an
    agent made, Codex's as well as Claude Code's, under the same guards as before. A worktree
    the person made by hand is theirs: the sweep leaves it and counts it. Such a worktree can
    still be removed by hand, one at a time.
  - **What it says.** The notice names who made what went, then what stayed and what was
    left, for example "Removed 2 merged worktrees (1 Claude Code, 1 Codex); 2 worktrees in use
    or not committed; 1 worktree of your own left".
  - **Where removal is offered.** "Remove this worktree" (palette, path bar, exited thread,
    commit sheet) now also finds a Codex worktree's root, from its managed layout
    `.codex/worktrees/<four hex>/<name>` (`worktree_root`).
  - Tests: `workspace::tests::worktrees::remove_merged_takes_only_the_landed_worktrees_nothing_works_in`
    and `workspace::worktrees::tests::a_worktrees_root_is_read_from_any_folder_in_it`.

- ✅ **A start held at Claude Code's trust dialog offers to trust its folder** (2026-10-10,
  readiness audit item 13, the client half; the worker half is in `agents.md`).
  - **What the card shows.** The held start's request ("Claude Code is asking something in its
    terminal") carries a "Trust this folder" choice, scoped to the folder. The card treats it as
    the grant that lasts that it is: a quiet button, with the folder written out after it,
    leading the row from its far end. "Answer in the terminal" stays the solid button,
    because a card never leads the person to a standing grant. Before, the release went quiet
    whenever any option was on the card; now it does so only when another answer stands in
    the row (`release_button`).
  - **The press.** It answers with
    `Intent::Answer { ask: "terminal-start", choice: "trust" }`. If the worker refuses, the
    thread says why, and the card returns with the way to the terminal.
  - Test: `conversation::thread::tests::doors::a_start_held_at_its_trust_dialog_offers_to_trust_the_folder`.

- ✅ **A request's card shows what is being approved: an edit's change, a plan whole**
  (2026-10-11, readiness audit items 1 and 3, the client half).
  - **The defect.** Claude Code's hooks name no call, so an edit put to the person read
    "Allow Edit?" with nothing to judge it by. The patch was on the wire (`Request.proposed`)
    and nothing drew it. A plan with no card on show printed its last 12 lines.
  - **An edit.** The card draws `proposed` under the file's line: its type mark, its name
    (which opens it), its folder muted, then +N −M. The diff is the thread's own renderer
    (`patch_well`), drawn whole, in a well at most 0.4 of the window tall that scrolls, so
    the answers under it stay on screen. The file is the request's call's, else that of the
    newest call in the thread carrying the same patch: the transcript holds the call before
    its hook asks (`proposed_path`). With neither, the diff shows with no file line.
  - **A plan.** With no card of its own on show, the plan reads whole as Markdown at the
    prose size, in the same bounded well. A plan the agent's side cut says how many lines
    are left in its terminal.
  - **A call's diff in the thread** keeps its first 12 lines until "Show all N lines", and
    once all of it shows, says how many lines the agent's side left out. Before, it stopped
    at 12 lines and did not say so.
  - Tests: `conversation::thread::tests::proposed::*`.

- ✅ **A failure reads as one, and a failed turn offers "Try again"** (2026-10-11, readiness
  audit item 4).
  - **Notices.** An API error the agent gave up on, and a hook notice, take the error tone and
    a cross mark. An API error being retried, and a usage limit, take the warn tone and a
    triangle. Auto mode's decline stays a muted cross. Words in passing keep the muted info
    mark. A notice in a tone also reads a step stronger (`notice_mark`). A hook notice now
    means a hook failed, blocked a call or stopped the turn; one that only speaks is to come as
    information (lane W's adapters).
  - **The failed turn.** While the last turn stands failed with no limit holding it, and
    nothing works or waits, one line over the field says what failed in the agent's first
    line of it, with "Try again". The button sends "Continue" as ↵ would send it. A lapsed
    sign-in (Claude Code's "run /login", an expired OAuth token, a refused key, a 401) offers
    no Try again, since sending cannot mend it. It says "Claude Code needs you to sign in again,
    in its own terminal" and offers "Show terminal". Slopty never offers a login of its own.
  - Tests: `conversation::thread::tests::failing::*`,
    `view::notes::tests::a_failure_reads_in_the_error_tone`,
    `view::later::tests::a_lapsed_sign_in_is_told_from_other_failures`.

- ✅ **A Claude Code command that opens a dialog brings its terminal into view** (2026-10-11,
  readiness audit item 10).
  - **The defect.** `/config`, `/mcp`, a bare `/model` and the like were pasted into the TUI,
    and their dialogs showed where nobody looked while the thread was on show.
  - **The rule.** The command still goes as any message goes, through the worker's paste. A
    command Claude Code draws as a dialog then flips the tile to its terminal, as ⌘J does.
    Its command list does not say which commands do that, so `menu.rs` names them from Claude
    Code's `local-jsx` commands: `DIALOGS` always, and `BARE_DIALOGS` (`model`, `resume`,
    `add-dir`, `output-style`, `export`) only when sent with no argument. The slash menu says
    "Opens in the terminal" beside each one. Other agents' commands are not touched.
  - Tests: `conversation::thread::tests::doors::a_command_that_opens_a_dialog_shows_the_terminal`,
    `conversation::menu::tests::a_dialog_command_is_told_by_its_name`.

- ✅ **An opened call shows what it was called with; a subagent says what it did**
  (2026-10-11, readiness audit item 11, the client half).
  - **Input.** An MCP tool's call, a skill's, or one of a kind this client does not know shows
    its input over its output, as JSON laid out to read (`called_with`). An input cut short
    on the wire shows as it came. An empty one shows nothing. A call of a kind the client
    knows already says its input on its line, so it shows none.
  - **Length.** Each part shows its first 12 lines (the output its last 12) until "Show all N
    lines". That one press opens the whole call.
  - **A subagent's line** says its kind, its calls and its tokens beside its title, as far as
    the agent said them: "Explore · 14 tools · 12k tokens" (`agent_words`).
  - Tests: `conversation::thread::tests::proposed::an_mcp_call_shows_what_it_was_called_with`,
    `view::tools::tests::{a_call_shows_what_it_was_called_with, a_subagent_says_what_it_did}`.

- ✅ **A review is read by keyboard, marks what was viewed, and reaches a file on a narrow
  tile** (2026-10-11, readiness audit item 7).
  - **The keys.** A review tile has a key context of its own (`Review`; a new `review` table
    under `[keys]`, listed as "Reviews" on the Keyboard page). ↓ and ↑, or j and k, walk the
    changes in the diff's order. A file folded to its head, or one with no lines to show,
    counts as one stop. ⇧ with them walks the files' heads. ⌘Y keeps what the keyboard stands
    on and ⌘N puts it back (Zed's keys), then step on, so a run of ⌘Y reads a review down. A
    file of one change is kept whole. `c` opens a comment on the whole change, and `v` marks
    its file viewed. Each is in the palette.
  - **Bare keys.** The bare keys bind on `Review && !Input`. While a comment's field has the
    keyboard, a `j` is a letter. GPUI's `!` looks at the whole context stack.
  - **Where it stands.** A hunk's or a file's head takes the selection wash, and the list
    marks the file. The keyboard stays on its file by path when the review comes again, since
    a kept file leaves it and the rest move up.
  - **Viewed.** Each file's head has a "Viewed" tick box, as a forge's diff does. Ticked, the
    file folds to its head and the list shows a tick in place of its counts. The mark is the
    person's own, kept by the file's path and the blob it shows, so a file that changes again
    is not viewed. It lives in the tile for now; item 8's store will keep it across quits.
  - **A narrow tile** has no list beside the diff. Its scope bar gets "Files" (⌘⇧O), a menu
    of every file with its counts or "Viewed". A pick reveals the file and stands on it.
  - Tests: `review::tests::{keys_walk_the_changes_and_keep_them,
    a_comment_s_field_keeps_its_letters, a_narrow_tile_goes_to_a_file_from_its_menu}`.

- ✅ **"To review" is the worker's word, and a thread reviews its whole branch** (2026-10-11,
  readiness audit items 9 and 2's client half).
  - **The section.** *To review* used to list only this device's unread finishes, so a phone
    and a Mac disagreed and a thread looked at but not kept dropped off. It now leads with
    every agent whose row stands on `Rung::ToReview`: the worker says its tree holds changes
    the person has not kept. That reads the same on every device, until the person keeps
    them. Unread finishes follow, each agent once. The bell counts both.
  - **Review next** (⌘⇧R, in the palette while anything waits) opens the review of the first
    agent under *To review*, then the next after the one on show, round again.
  - **Scopes.** A thread's review also offers "Uncommitted" and "Whole branch", the folder's
    working tree as the worker reads it for a thread. An empty span still offers "All turns"
    as its next step.
  - **The opening span** (item 2). A thread's review opens on "Since reviewed" when an earlier
    turn changed files too and the tree still holds unkept changes. Otherwise it opens on the
    last turn (`Scope::opening`), so the first view is never part of what waits. A span the
    person or the thread view chose stands.
  - Tests: `workspace::tests::thread_waits::a_thread_with_changes_unkept_stays_to_review`,
    `review::model::tests::a_review_opens_on_since_reviewed_when_an_earlier_turn_waits`.

- ✅ **Drafts survive the app: one store for composers, starts and review comments**
  (2026-10-11, readiness audit item 8).
  - **The defect.** A half-written message to an agent, a start's first message and unsent
    review comments lived only in memory. The faces kept hidden views' text in a map by
    terminal session. A quit, a crash or iOS ending a suspended app lost all of them.
  - **The store** (`workspace/drafts.rs`) is one file, `drafts.json` beside the layout,
    replaced whole by `slopty_platform::fs::replace`. It is the user's alone (0600 in 0700),
    since a draft may hold a secret. It keeps each draft by what it is about, not by the view
    that held it:
    - a thread's composer by the thread, whichever tile shows it, or none;
    - a start's first message by its machine, agent and folder (start tiles are not kept
      across a launch, so the next start there takes the words back);
    - a review's comments and the files marked viewed by its thread, or by its folder on its
      machine.
  - **Writes.** A composer emits `ThreadViewEvent::Drafted` and a review `ReviewEvent::Drafted`
    (a hash of its comments and viewed files changed). A pass 400 ms after the last change
    reads every live view and writes off the UI thread. A view that goes hands its words over
    as it goes. Going to the background (`set_app_active(false)`) and quitting write at once
    on the UI thread, so the words are on the disk before the app can be ended.
  - **What clears.** A sent message, a start that went, a start tile closed by the person,
    and comments the agent took leave nothing. A draft kept more than 30 days ago goes when
    the file is read. The faces' in-memory `drafts` map is deleted: one store, not two.
  - Tests: `workspace::tests::drafts::a_composer_s_words_come_back_after_a_relaunch`,
    `workspace::drafts::tests::drafts_come_back_and_old_ones_go`.

- ✅ **A project's board opens in a tile of its own when no orchestrator can show it**
  (2026-10-11, readiness audit items 12, 13, 16 and 17).
  - **The defect.** A board showed only as a face of its orchestrator's terminal tile. A
    project named with "Name this project…" has no orchestrator. An orchestrator's agent can
    end, and its machine can be away. In all three cases the palette's line for the project
    said why it could not open, and nothing let the person read, tell, move or cancel the
    tasks.
  - **The board tile** (`workspace/board_tiles.rs`). The board belongs to the server, not to
    a machine, so its tile sits under `BOARD_WORKER`, the nil key that no worker has. It opens
    in a tab of its own, takes the keyboard in the board, and is gone to again rather than
    opened twice. ⌘W closes it. It is saved with the layout as `Saved::boards` (tile and
    project), so a relaunch puts it back. Until the server's projects arrive, and while the
    server is away, it shows the state pill saying so. The orchestrator's tile keeps the board
    as one of its faces wherever it can show it, including while its machine is away, since
    that tile is kept then.
  - **Away mark** (item 13, client half). `WorkerSeen.away` is true when this client has no
    link to the worker. A task whose agent runs there wears the away glyph on its place chip,
    which reads "studio · away", and its hint says that its agent is not heard from until the
    machine is back. The row's own mark still says the task's state: the server has not
    changed it.
  - **Not sent** (item 16, client half). A board's action pressed with no server linked used
    to do nothing. It now says "Not sent: the server is away".
  - **Start** (item 17, client half). A task never started offers "Start" (s), with "Run on…",
    on the card the keyboard stands on. Start reads the task's brief (`TaskGet`) and sends
    `TaskSpawn` on the task's pin, with the brief as the first prompt. It uses the agent the
    orchestrator is, or Claude Code when this client cannot tell. Dependencies still hold it.
    The card says "Starting #3…" at once, and a refusal puts the card back and says why.
  - Tests: `workspace::tests::projects::a_board_with_no_orchestrator_to_show_it_opens_in_a_tile_of_its_own`,
    `…::a_task_on_a_machine_gone_away_says_so`, `…::a_task_never_started_is_started_from_its_card`,
    `workspace::tests::relaunch::a_relaunch_puts_a_board_tile_back`.

- ✅ **Setup says where it stands, and the phone's terminal works by touch** (2026-10-11,
  readiness audit items 19, 20 (interim) and 21).
  - **The server away, no machine listed** (21). The empty page sent the person to add a
    machine while the real cause was the server, which lists the machines. With no worker and
    the server out of reach, the page now leads with the server's state ("Server offline").
    Under it come what that means and the server menu's own doors, with "Retry now" raised;
    adding a machine comes after. A phone's title bar has no readouts, so its navigator says the
    same under its header (`nav-server`), with "Retry now" on the line's end.
  - **Tailscale** (21). On the "Use this Mac" checklist, a tailnet line that is stopped or not
    answering offers "Open Tailscale" (the installed app, else its download page), and a Mac
    with none offers "Get Tailscale". Both stay quiet advisories, not stops.
  - **A remote Mac that takes no input** (21). A window or display tile whose Mac has not
    granted Accessibility used to take clicks that did nothing; only the navigator said
    "Accessibility off". It now shows a slim line over the picture that names the Mac and the
    setting to turn on there.
  - **A phone's first run** (21). The server panel on an iPhone or iPad first says "On your
    Mac, open Connect a phone or iPad and scan its code with the Camera."
  - **Copy command** on a worker tile of another build is gone on iOS: a phone cannot run it.
  - **Touch terminal** (19).
    - A phone's key bar ends in "Hide", held at the trailing edge while the row scrolls. It
      puts the soft keyboard away and leaves the focus in the terminal; a tap on the terminal
      brings the keyboard back. An iPad's own keyboard has a key for this, so its bar has no
      Hide.
    - A long press dragged past the grid's top or bottom scrolls the history a tick at a time,
      as a drag with the pointer does, so a selection can run past the screen.
    - On iOS, ⌘. sends Escape in a terminal and in a remote window, for a Magic Keyboard with no
      Esc key.
  - **Allow and Deny on a note** (20, interim). On iOS they now bring the app forward. iOS may
    have ended the app, and a background press then launches it with no scene, so GPUI and the
    links never start and the agent's held prompt times out. Brought forward, the app links up
    and sends the answer the note held. On a Mac they still answer where the note is. The whole
    fix, a headless answer from the app delegate, is still to come.
  - Tests: `workspace::tests::setup_doors::*`,
    `terminal::view::tests::a_long_press_dragged_past_the_top_scrolls_into_history`,
    `…::send_escape_types_escape`, `this_mac::tests::the_doctor_maps_to_lines_and_buttons`,
    `notify::tests::the_approval_note_answers_in_place_and_shows_on_demand`. The keyboard calls
    themselves (`request_virtual_keyboard`, `dismiss_virtual_keyboard`) do nothing on a Mac, so
    the iOS e2e's key bar scenario is the layer that sees them.

- ✅ **An update from a tile can be cancelled, goes on in the sheet, and never takes a machine
  back** (2026-10-11, readiness audit items 15 (the tile half) and 6).
  - **Cancel** (15). While an update runs, the tile's Update gives way to "Cancel". A link
    that stalled held the tile until the deploy's own timeout; Cancel drops the run and says "The
    update was stopped", with "Try again" to start it over.
  - **A password or a host key** (15). An update that stops at what only the SSH sheet asks
    used to say so and leave the person to find the sheet. It now names the host ("mini asks
    for a password", "mini's host key is not trusted yet"), and its button reads
    "Continue…": it opens the sheet on that host with its user and port filled in.
  - **A machine on a newer build** (6). The update notice used to offer Update whatever the
    order, so a client a release behind could push its older build over a newer worker. When
    the other side is newer, the tile says "This machine runs a newer build" (or "The
    server…") and "Update Slopty on this device to match it.", with no Update and no command
    to copy, and the navigator row offers none either. Update all skips such a machine.
  - **When the order cannot be told** (6). Two builds of one version with no stamp to compare
    cannot be ordered. The first press then stops and says "It may run a newer build than
    this one"; a second press deploys. Update all touches only machines known to be older.
  - Tests: `workspace::tests::bars::an_update_under_way_can_be_cancelled_and_one_stopped_at_a_password_continues`,
    `workspace::tests::tiles::a_worker_on_a_newer_build_asks_this_device_to_update`,
    `ssh::tests::a_tile_cancels_an_update_and_a_password_goes_to_the_sheet`,
    `…::a_tile_sends_a_new_machine_s_key_to_the_sheet`,
    `…::an_update_never_takes_a_machine_back_and_asks_when_it_cannot_tell`.

- ✅ **A failed dial says what to do, and a machine that keeps Slopty's hooks off says so**
  (2026-10-11, readiness audit item 5's A half and the `runs_own_hooks` deletion line).
  - **The defect** (5). A machine that did not answer, and one that turned this device away by
    its `[worker] allow` ranges, both showed the transport's words ("connect: 100.x:45550: no
    answer; reconnecting…") with no next step.
  - **One status per kind** (5). The app maps each of `slopty_net`'s kinds to a
    `WorkerStatus`, and the raw error goes to the log.
    - **No answer.** The tile says "mini does not answer", and under it that the machine may be
      asleep or off the tailnet, or Slopty has stopped there. It offers Wake and Retry now. When
      the server lists the machine as away, the server's word still wins.
    - **Turned away.** The tile says "mini turned this device away", and under it "Add
      100.64.0.9 to Allowed addresses in its Slopty settings." (the settings' name for
      `[worker] allow`). Its "Copy address" button puts that
      address on the clipboard. The address is this device's source address on the route to the
      machine, which a connected UDP socket reads from the routing table without sending a
      packet. Wake is not offered: the machine answered, so it is awake.
    - **Not found.** "mini cannot be found": its name does not resolve, so check that this
      device is on the tailnet.
    - **Dropped.** A link that ended reads "the link dropped; reconnecting…", not the stream's
      error text.
  - **Hooks off by company policy.** A machine's managed settings can keep Slopty's hooks from
    running (`InstalledAgent::managed_hooks_off`). Its Claude Code approvals and questions then
    never reach the thread, which looked like an agent stuck for no reason. A Claude Code
    thread there, and a Claude Code start before its first message, now carry a slim line
    across the top: "Company policy keeps Slopty's hooks off on mini: answer Claude Code's
    approvals and questions in its terminal." The machine's facts read "Claude Code 2.1.0,
    hooks off by company policy". Codex and pi threads carry no line, and it goes as soon as
    the policy changes.
  - **A picture paste's offer waits for room** (22, the client half). It used to go on the link
    with `try_send` and was lost to a full queue, and the worker then held the paste for good.
    It now waits in the terminal view's own queue of unsent messages, ahead of its chord
    (`docs/MEASUREMENTS.md`, 2026-10-11: the key's fast path holds).
  - Tests: `workspace::tests::away::each_kind_of_failed_dial_says_what_to_do`,
    `terminal::view::tests::a_picture_offer_waits_for_room_ahead_of_its_chord`,
    `workspace::tests::agent_tile::a_claude_thread_where_policy_keeps_the_hooks_off_says_so`,
    `net::tests::a_failed_dial_is_told_by_its_kind` and
    `server::tests::each_kind_of_failed_dial_is_its_own_status` (slopty-app).

- ✅ **A review says what a file is: a rename, a mode, a picture, a file too large** (2026-10-11,
  readiness audit item 18's drawing; W's half carries `FileDiff::{old_path, kind, modes}`).
  - **The defect.** A renamed file read as a removal and an addition, a picture and a text
    file over 4 MiB both said "Binary file", and a change of the executable bit said "No lines
    to show".
  - **The head.** A renamed file's head says where it was: "Renamed from old.rs" when it kept
    its folder, "Moved from lib/old.rs" when it did not. A change of mode is said in words
    ("Made executable", "No longer executable", "Now a symbolic link", "No longer a symbolic
    link", else both modes), after any other status, joined by a middle dot. The words cut
    with an ellipsis rather than push the counts off the row.
  - **The empty row.** A rename or a change of mode with no change to its lines says so where
    the lines would be.
  - **A picture** shows its two sides beside each other, "Before" and "After" (one of them for
    a picture added or removed), fitted in a 160-point well, with its size under them. Each
    side's bytes are asked of the worker by blob (`GitOp::Blob`), once per tile, the first
    time the row is drawn, so a review of many pictures fetches only those scrolled to.
    The client's git book keeps the bytes, and the tile decodes each side once. A side that
    could not be read says why in its well.
  - **A text file too large to cut** says its size ("6.0 MB, too large to show its changes
    here") with "Open the whole file", which opens it in a tile of its own once the
    repository's root is known.
  - **A blob read is a read.** The git book counted any op it did not list as a change, which
    would have read the status again after every picture's side. `GitOp::Blob` is now listed
    with the reads.
  - **Deleted:** `conversation::diff::blocks`. Only its own tests called it; the review, the
    face and the thread view all go through `thread_blocks`. Its tests now run over
    `thread_blocks`, so `diff.rs` no longer names `slopty_proto::conversation`.
  - Tests: `review::tests::a_rename_a_mode_and_a_large_file_are_said_in_words`,
    `review::tests::a_picture_shows_both_sides_and_a_large_file_opens_whole`.

- ✅ **A pull request's review threads hang on their lines, and the person's comments post as
  their review** (2026-10-11, readiness audit item 23; W's half is `GitOp::PullReview` through
  the person's own `gh` or `glab`).
  - **The defect.** The open pull request's review threads were read only to count them for
    "Address the review (N)". The person never read them in place, and their own comments
    could only go to an agent.
  - **Threads in the diff.** A thread on a line of the new side that is on show hangs under
    that line, as a waiting comment does. It shows each note's author and words, and a link to
    its page. A thread on a line not on show, or on code changed since, hangs at its file's
    end and names its line ("Line 99", "Line 11, changed since"). A reviewer's words over
    their whole review stay with the commit sheet's "Address the review".
  - **Posting.** While the person has comments of their own and the branch has an open pull
    request whose threads were read, the foot offers "Post 2 comments to #42…". Its menu holds
    "Post as comments", "Approve" and "Request changes". The comments go as one review,
    anchored at the head commit last read, each on its last line and side. The agent's
    findings are left out: they are not the person's to sign.
  - **What comes back.** Once the forge took the post, its comments go and "Posted 2 comments
    to #42" is said above the diff, and the pull request is read again. A post turned down
    keeps every comment and says why in the error's tone ("Review not posted to #42: …").
    The commit sheet says "Posted the review · 2 comments", with its page.
  - Test: `review::tests::forge_threads_hang_on_their_lines_and_comments_post_as_a_review`.

- ✅ **The settings of another machine's worker or server are edited from here** (2026-10-11,
  readiness audit item 24's picker; W's half answers `Verb::Settings`).
  - **The defect.** The Agents page, and the worker and server groups on Network, edited
    `[worker]` and `[server]` keys in this device's own `settings.toml`. That set nothing
    when the worker or the server was another machine. A phone had no Agents page at all.
  - **The picker.** A page that holds a daemon's keys leads with "Settings of", followed by
    the machine they are for. The choices are this Mac's own file (where this device runs a
    worker or a server), the server, and each worker the workspace lists. A phone or an iPad
    starts on the server and has an Agents page again. A narrow window shows the picker once,
    at the top of its single column.
  - **Another machine's file.** Picking a machine reads its file once. Its rows wait,
    saying "Reading mini's settings…". They then show what that file holds, and only the
    machine's own table: a worker's `[worker]` rows, or the server's `[server]` rows. Group
    headings name it ("Share mini's shells and windows"). This app's own rows on the same page
    stay this device's.
  - **Edits.** An edit is written the way the form writes its own file
    (`slopty_settings::edit`), so the rows answer at once. It is kept as a `SettingEdit` and
    sent when the hand pauses, the same pause a local write waits for. The machine answers with
    its file as it then stands. Edits made while an answer is on its way are replayed over it
    and go next.
  - **What did not go.** An edit turned down says so ("Not changed on mini: …"), and the file
    is read again so the rows show what it holds. A machine that cannot be read says why.
  - Test: `settings_form::tests::another_machines_settings_are_read_and_edited_there`.

- ✅ **Allow and Deny on a phone's note answer with no window** (2026-10-11, readiness audit
  item 20, the whole fix; it replaces the interim above).
  - **The defect.** A press of "Allow" or "Deny" on a note can launch an app iOS ended, in the
    background and with no scene. GPUI, and with it the links to the workers, starts only
    when a scene connects. The answer therefore waited until the person opened the app, and
    the agent's held prompt timed out first. The interim brought the app forward on every
    press.
  - **The fix.** The buttons act where the note is again, on iOS as on a Mac. The app delegate
    installs an answer for a press that finds nothing listening for taps
    (`notify::answer_unheard`) before the notification delegate. A press that arrives with no
    window goes there instead of waiting in the inbox, and so does one whose listener is gone.
  - **How it is answered** (`slopty_app::verdict`). The answer takes the request, and the
    thread or the terminal's agent, from the note. It then links to the server the settings
    name, on a thread and a runtime of its own, over an endpoint of its own, because the
    process's endpoint is bound to the app's runtime. Through that link it reads the thread's
    open requests for the choice that allows or denies once, then answers with it: the same
    route the workspace takes for a worker it is not linked to. It gives up after 20 seconds,
    inside the time iOS grants, and then tells the system the press is done.
  - **What stays.** While the app runs with its window, a press is answered by the workspace as
    before. The note itself and "Show" still open the app.
  - Tests: `verdict::tests::a_press_names_its_thread_or_terminal_and_its_request`,
    `…::a_verdict_answers_with_the_choice_that_allows_or_denies_once` (slopty-app), and
    `notify::tests::a_tap_before_the_app_listens_arrives_once_it_does_exactly_once`, now
    including the unheard press (slopty-platform). The e2e
    `the_app_finds_its_workers_through_the_server` (`cargo xtask e2e through-server`) presses
    Deny on far's held request with the test socket's `PressNote`, which takes the same
    `verdict::answer_alone` the phone does. It proves the route end to end: the settings'
    server, a link of the press's own, the held relay ending on its answer, and the request
    gone from the tile.
  - Not covered by a test: the system's own press of a delivered note's button. That needs
    XCUITest or synthetic input, and the session rules forbid the latter. The person checks it
    on a device.
