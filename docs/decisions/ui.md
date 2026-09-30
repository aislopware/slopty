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

- ✅ **Notes are shared text, last writer wins.** `ItemKind::Note { text }` was already in the
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
  | `surfaces.border` | `24272E` | `D8DBE1` | every hairline (1 pt) |
  | `surfaces.text` | `E6E6E6` | `1D1D1F` | primary |
  | `surfaces.text_secondary` | `B4B9C3` | `4B4F58` | labels, tool summaries, counts |
  | `surfaces.text_muted` | `8B919C` | `66666B` | hints, timestamps, folds, inactive titles |
  | `surfaces.accent` | `8AB4F8` | `2A63C4` | focus ring, active border, links (text and hairlines only) |
  | `surfaces.success` | `98C379` | `187633` | connected, agent done |
  | `surfaces.warn` | `E5C07B` | `8B5D00` | agent waiting, "N need you", muted, reconnecting |
  | `surfaces.error` | `F06C75` | `C7212C` | failed result, failed command, pairing error |
  | `radii.xs / sm / md` | 4 / 6 / 8 | same | pills and inline buttons / buttons, inputs, key caps / panels, items, popovers |
  | `spacing.xxs … xl` | 2 / 4 / 8 / 12 / 16 / 24 | same | the only paddings and gaps in chrome |
  | `typography.ui_size` + `caption()/small()/title()` | 13 → 10 / 12 / 15 | same | chrome type scale (settings move the base) |
  | `typography.mono_size` @ `mono_line_height` | 13 @ 1.0 | same | terminal grid, code in the conversation (the multiplier is ghostty's `adjust-cell-height`; 1.0 = the font's own) |
  | `typography.markdown_line_height` | 1.5 | same | assistant turns |
  | `alpha::FAINT` | 0.12 | same | a quiet fill, a hover wash, the command-block hairline, the visual bell |
  | `alpha::TINT` | 0.25 | same | a tint that has to be seen: a text selection (a selected row is `overlay` since the second de-slop pass) |
  | `alpha::PRESSED` | 0.4 | same | under the pointer; a scrollbar thumb |
  | `alpha::SCRIM` | 0.6 | same | modal backdrop |
  | `alpha::STRONG` | 0.7 | same | the separator after a failed command, minimap item blocks |
  | `alpha::VEIL` | 0.9 | same | panels over video and the canvas: the stream HUD, the minimap, a looker's outline and tag |

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
  glow, no second elevation; no emoji and no decorative glyph in chrome text; motion only where it
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

- ❌ **Ranking the command palette by match quality** (2026-09-14, written and then measured
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
  a pixel count. Not written yet.

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

  *Icons.* Lucide (ISC), from gpui-kit's asset crate. `slopty_ui::icons` embeds only the icons
  it lists and backs them with gpui-kit's component bundle; the macOS and iOS apps register it. Sizes
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
  - **Toasts.** They stack in the strip's corner, two at most, 6 s each, with Go or Undo.

  Not built: a "lines below · back to live" pill, because the terminal view tracks only its
  offset. Tests (`workspace/tests/tiles.rs`):
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
  - **Empty workspace.** It shows a large muted grid icon, `Empty workspace`, and three rows
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
    the mark stands still under Reduce Motion. Headless at a simulated 120 Hz with one agent
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
  (2026-09-26). The reference study found the status bar showing a debug readout and the inbox
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

- ✅ **Settings open in a dialog as tall as the file** (2026-09-27, UI wave 2 overlays). The
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
  `an_age_shows_past_a_minute_and_ticks_at_its_next_unit`, `a_turn_ticks_in_whole_seconds`.

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

- ✅ **The status bar's left slot says where the human is** (2026-09-27, UI wave 2 frame
  chrome). The server's state left the title bar, where "• server unreachable" read as a tab.
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
    `kit` `the_accent_text_tone_is_never_a_fill` holds it.
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
  dark fill is now `346BF1` and carries white too, and light's is `1B4ED8`.

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
    its goldens keep "Workspace 1".
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
    icon button that does the same, and the palette lists "Show conversation or terminal". The
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
    side from 960 pt up. Bash shows its command and output tail. A subagent is a card that
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
    once the look has ended.
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
    every surface in dark.
  - **Type, radius, motion.** `Typography::MEDIUM_WEIGHT` (500) says "this one": a button's
    words now, and the selected row, the focused title, the active tab and an approval's
    statement as their waves land. `STRONG_WEIGHT` narrows to titles (`kit::title`, the title
    and display sizes, headings, the first run's wordmark); the workspace's name, a worker's
    name and the overview's names move to the medium weight. `prose()` is 14 for assistant text
    and prompts; `display()` is 22. `Radii::lg` (12) is the radius of what floats: dialogs,
    menus, the inbox, the workers' popover, toasts and the add-worker panel; a hint and a pill
    keep their control's radius, since at 12 a 20 pt hint is a lozenge. `slopty_theme::Motion`
    holds the durations (hover 0, fade 120, settle 160, sheet 240 ms) and the two curves as
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
    ease-out) while the layer under them fades in. The phone's palette sheet reaches the top
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
  (2026-09-27, `.research/design-direction-2026-09-27.md` §5.1 and §5.4). The user asked for
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
    keeps them. Reload has no key. The header keeps its bare "←" (while there is history)
    and "↻". `slopty_platform::web::WebView` gained `forward` and `Page::can_go_forward`.
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
    states. Its age runs from the hook's `since_ms`, when it came to rest, not from the shell's
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
    the tap outside that closes it is visible. The window picker has no Cancel yet. The
    workspace does not tell it whether a keyboard is attached.
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
    bare tool, flush under the bar), navigator `a_turn_ticks_in_whole_seconds`,
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
  - **A row holds one line.** The line on what a key does is cut to one line with an
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
  - **Web Inspector** is on in debug builds, off in release; a setting can come when someone
    needs it in a release build. `isInspectable` opens a page to Safari's Develop menu only, so
    a debug build on the Mac also turns on WebKit's developer extras, and "Inspect page" in the
    palette opens the inspector's own window. Both are WebKit's private headers
    (`_setDeveloperExtrasEnabled:`, `_inspector` and its `show`), asked `respondsToSelector:`
    first, so a WebKit without them loses the command and nothing else, and neither reaches a
    release build. The palette lists the line only in a Mac debug build; on iOS Safari's Develop
    menu on a Mac is the way in.
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
    palette says "Mute".
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
    only, for this run.
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
  - **Not done.** The system-shortcut tap arms only in the workspace window. The popped state
    is not saved in the layout, so a relaunch opens every tile in the workspace. iPad is
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
    choice. The palette's "Open last offered page" opens it after the notice went.
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
    come back for a week is mentioned at start, and the palette's "Discard unsaved edits over a
    week old" lets such edits go. Nothing is dropped unasked.
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
  ("About Slopty" in the palette), over the name, the version and the build.
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
    blinks at the cadence without building the strip, steady under Reduce Motion; the panel
    opens from the palette's action and Esc closes it); goldens `empty-workspace` and `about`
    (`about_slopty_leads_with_the_mark`).

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
    notifies the strip for the next frame only when it gave focus that was asked for
    (`focus_asked`), since the fork asks for no frame when the focus moves while a window
    draws and a view replayed in that frame would keep the old focus. Whether the focus moved
    is not read.
  - **Cost.** The keyboard moved by a view of its own (a click in a body, a find bar giving it
    back) over 60 shells and 60 notes: 0.28–0.33 ms p50 to 0.13–0.14 ms, and the workspace and
    strip built for none of 420 moves, against every one before (`docs/MEASUREMENTS.md`). A
    move the workspace asks for is unchanged (1.07–1.13 ms to 1.02–1.07 ms): the layout moves
    with it, which builds both anyway.
  - **Left for the fork.** A focus given while the window draws could ask for its own frame
    (`Window::draw` compares the focus only across its focus listeners); then the strip's
    notify goes too.
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
  - **What it shows.** A header with the project's title, where its work lands (repository →
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
    credential. Both are parked for the user.
  - Tests: `the_mode_chip_takes_the_freshest_word`, `a_fold_names_what_its_work_touched`,
    `the_status_bar_counts_every_workers_agents`,
    `the_status_bar_reads_the_focused_tile_and_its_link`, `the_readouts_say_what_they_count`,
    `background_work_sits_over_the_composer`, and the theme's hairline ladder
    (`the_surfaces_climb_bars_panel_content` and the derived-surfaces sweep, now through
    `Hairline::over`).
