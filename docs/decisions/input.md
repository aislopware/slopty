# Decisions — Input

See `docs/DECISIONS.md` for the legend. Newest entries go at the end.

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

- ✅ **⌘ chords go to the remote window unless the canvas binds them** (2026-09-04; superseded
  below on 2026-09-13: a focused window now gets the canvas's chords too). GPUI runs
  key bindings before key listeners, so ⌘T/⌘N/⌘O/⌘W/⌘0/⌘1/⌘=/⌘- never reached `ScreenView`;
  every other chord (⌘C/⌘V/⌘Z/⌘S/⌘K…) is forwarded with `Mods::SUPER` and no text (GPUI gives
  no `key_char` for ⌘ chords; the injector's virtual keycode carries it). Verified: ⌘K from the
  app reached hostd as `Key { K, Press, SUPER }` and cleared the streamed Ghostty. The view
  tracks pressed keys so a release whose press was eaten by a canvas binding is not forwarded.
  ⌘Q/⌘H/⌘M stay with the app (menu bar). Modifier-only presses are not forwarded (GPUI has no
  key-down for them); the injector sets flags per event instead.

- ✅ **A focused remote window gets every chord** (2026-09-13). ⌘W in a remote VS Code closed
  the Slopty card, ⌘T opened a shell, ⌘0 reset the canvas zoom: the canvas's bindings ran
  first. Now `ScreenView` puts `Screen` on its key context and the canvas binds its chords in
  `Canvas && !Screen`, so while the window has the keys they all go to the host (as Parsec
  does in a window: the app's chords are the remote's until the focus leaves). ⌃Tab / ⌃⇧Tab
  keep `Canvas` — the control ring is the keyboard's way out, then the same ⌘W is the card's
  — and a click on the canvas or a title bar does the same. No modal "capture" toggle: the
  focus is the toggle, visible as the card's focus ring. Test:
  `a_focused_remote_window_takes_every_chord`.
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

- ✅ **The host daemon ships non-sandboxed and Developer-ID signed, and the dev daemon is signed
  too** (2026-09-14). App Sandbox blocks `CGEventPost`, which settled the sandbox half from the
  start. The signing half had only slop-desk's unverified claim behind it until a second reason
  turned up on its own: a TCC grant is per executable, and `cargo build` ad-hoc (linker-)signs, so
  every rebuild is a new executable and the Screen Recording approval the last build was given is
  gone. It fails silently — ScreenCaptureKit returns `-3801` (`SCStreamErrorUserDeclined`) with no
  prompt, which reads as a broken link rather than a missing permission, and it cost the capture
  guard's confirming mesh run a whole session (MEASUREMENTS.md). Signing with a Developer ID
  certificate under a fixed identifier makes the designated requirement the identifier plus the
  certificate instead of the hash, so one approval covers every later build. Ruling: the daemons
  are signed as `dev.aislopware.slopty.hostd` and `….ptyd`, the same strings the LaunchAgents are
  labelled with, by `cargo xtask sign`; `xtask run host` calls it after the build so nobody has to
  remember, and reports rather than fails when there is no certificate, because an unsigned daemon
  still runs. Only a Developer ID certificate is accepted: an Apple Development one expires within
  the year and takes the approval with it. Tests `xtask::sign::tests`.

- ✅ **The daemon asks for Screen Recording, it does not only preflight** (2026-09-14). Start-up
  checked `CGPreflightScreenCaptureAccess`, logged that capture would fail and carried on, which is
  a dead end: a process that never *requests* the permission is never prompted for it and never
  appears under Screen Recording at all, so there is nothing for anyone to switch on and every
  stream keeps failing with `-3801`. The Accessibility path had asked (`CGRequestPostEventAccess`)
  since the first cut; this is the same three lines for the other permission
  (`slopty_capture::request_capture`), logged at `warn` beside it. Costs one prompt, once, per
  signed identity — which is exactly one now that the identity is stable. `slopty host doctor`
  still reports both, so a headless install can gate on it.

- 🔬 Whether macOS 26 drops modifier combos from unsigned processes — slop-desk claims it, we have
  never seen it. Verify with a ⌘ chord through an unsigned daemon against a signed one.

- ✅ **Modifier keys reach the host on their own** (2026-09-13). The injector has posted a bare
  modifier as `FlagsChanged` since the first cut, but the client never sent one: GPUI reports
  a modifier press as `ModifiersChangedEvent`, not a key, so a remote program never saw ⌘
  held on its own (an app that reveals shortcuts or alternate menu items while ⌥/⌘ is down,
  a game that runs while ⇧ is held), only the flags on the next key or click. Ruling: the
  screen view keeps the last reported modifiers and, on each change, forwards every one that
  moved as a press or release of its key (`ShiftLeft`, `ControlLeft`, `AltLeft`, `MetaLeft` —
  GPUI names no side, so the left key stands for both) carrying the new state as `mods`; an
  unchanged state sends nothing. The fn key stays local: forwarding it would fire the host's
  own fn setting (emoji picker, dictation) every time a client pressed it for a function key.
  Not done: releasing modifiers when the view loses focus while one is down (⌘-tab away with
  ⌘ held) — the release does arrive from GPUI when the key goes up in this window, and a
  focus-loss sweep belongs with a sweep of `held` too. Test: headless
  `modifier_keys_go_to_the_host_as_they_move`.

- ✅ **A window card's grip resizes the host window** (2026-09-13). Dragging a card's grip only
  changed the card, and the next `Geometry` event snapped it back to the window's aspect;
  the window itself could be resized only from the host. Ruling: on the grip's release the
  canvas sends `ScreenRequest::Resize { stream, width, height }` (protocol 41) — the card's
  new size over the scale it drew the window at when the drag began (its width over the
  native pixels), less the title bar — for a window stream whose size changed; hostd turns
  the pixels into points with the stream's `point_scale` and, off the runtime, sets
  `kAXSizeAttribute` on the window the accessibility API lists with the frame and title the
  window list gives it (`slopty_capture::resize_window`, the same match the hide watch uses;
  the API has no window number). Nothing is assumed about the result: the application keeps
  its own minimum size and aspect, the geometry poll reads the frame it settled on within
  250 ms, and `follow_geometry` re-aspects the card to it. A display card asks nothing.
  Not the alternatives: `CGEvent`-driven drags of the window's corner (a synthetic drag of
  another app's resize edge is fragile and visible); an `AXPosition` move too (the user
  pulled a size, not a place). Tests: proto golden `client_screen_resize`, canvas
  `the_grip_asks_the_host_to_resize_the_window`.

- ✅ **Losing focus lets go of every key held on the host** (2026-09-13). A key whose press
  went to the host and whose release went elsewhere — ⌘-tab away with ⌘ down, a click on
  another card mid-keystroke, the window going inactive — stayed down on the host until the
  same key was pressed again there. Ruling: the screen view registers, at its first render
  (the constructor has no window), `on_blur` on its focus handle and a window-activation
  observer; either sends a release for every key in `held` (with empty modifiers) and then
  the modifier releases through the same diff the modifier keys use, so the host sees the
  keyboard as the client does: nothing down. A view with nothing held sends nothing. The
  earlier "Modifier keys reach the host on their own" entry's open item is closed by this.
  Test: headless `losing_focus_releases_what_is_held_on_the_host`.

- ✅ **A fling reaches the host as a gesture and then as momentum** (2026-09-15). The wire type
  and the host have carried both phases from the start: `ScreenInput::Scroll` has `phase` and
  `momentum`, and `slopty-input`'s backend writes them into `kCGScrollWheelEventScrollPhase` and
  `kCGScrollWheelEventMomentumPhase`. The client filled in only the first. It mapped gpui's
  `TouchPhase` straight across and sent `momentum: None` on every event, so a two-finger fling
  over a remote window arrived as `Began, Changed…, Ended, Changed, Changed…` — a gesture that
  ends and then keeps changing, which is not a sequence macOS ever produces. AppKit's own scroll
  latching and rubber-banding read these fields, so the remote app was being handed a shape it
  has no case for.

  The cause is the same gpui detail behind the terminal's scroll latch: `gpui_macos` derives
  `TouchPhase` from `NSEvent.phase()` and never reads `momentumPhase()`, so a fling's coast is
  indistinguishable from more of the drag unless the client tracks the gesture itself.
  `Scrolling` does that — `Idle`, `Fingers` between `Started` and `Ended`, then `Momentum` — and
  `scroll_phases` reads it: the coast goes out with no scroll phase at all and a momentum phase
  that begins once and then continues. A `Lines` delta is a mouse wheel and carries neither
  phase, where before it was sent as part of whatever gesture gpui's phase claimed.

  The *end* of the coast has no phase of its own to read, but it does still arrive. macOS closes
  a fling with an event that carries `momentumPhase = End` and moves nothing; its `phase()` is
  none, so gpui passes it on as one more `Moved`. A momentum event that has stopped moving is
  therefore the end, and taking it at face value closes the gesture on the frame macOS sent it
  rather than some time later. Without that, the remote app stays latched to a scroll that never
  finishes, and AppKit holds its rubber-band open.

  `MOMENTUM_GAP` (120 ms, several frames of silence where momentum events arrive about one frame
  apart) stays as the backstop for a lost or dropped end, on a timer that every momentum event
  replaces and the end itself cancels; it emits the same zero-delta close. A finger landing on a
  running fling closes it first, as macOS does. A drag that ended without a fling behind it needs
  nothing: its own `Ended` closed it.

  Two more shapes come from the same place gpui flattens phases. `NSEventPhaseMayBegin` and
  `NSEventPhaseBegan` both map to `TouchPhase::Started`, so fingers that rest on the trackpad
  before they push open one gesture twice; the second `Started` is the same gesture continuing
  and goes out as `Changed`, because telling the remote app a scroll began again restarts its
  rubber-banding mid-scroll. And `NSEventPhaseCancelled` fell into the mapping's `_ => Moved`
  arm, so a gesture the system took back looked like one still running and was never closed.
  That one is not fixable from here — it is a lost distinction, not a lost sequence — so the
  fork carries it (`gpui_macos: a cancelled scroll or pinch is cancelled, not moved`,
  `2b52c4bc82`), mapping it to the `TouchPhase::Cancelled` gpui already has for touch. `Magnify`
  had the same arm and got the same fix; the canvas's pinch handler ignores phase, so nothing
  there changes.

  Test: `a_fling_over_the_picture_reaches_the_host_as_a_gesture_and_then_as_momentum` drives the
  whole shape, both ways it can finish, and the doubled open.
