# Decisions — Input

See `docs/DECISIONS.md` for the legend. Newest entries go at the end.

- ✅ **`slopty-input` = `CGEvent` injection, one `Injector` per screen stream** (verified end to
  end 2026-09-04: keys typed into the Slopty app landed in a streamed Ghostty window). Stream
  pixels map back to global points through the target's current bounds (cached 100 ms) and the
  stream's pixels-per-point scale, which `ScreenStream` updates on every quality change.
  Window streams post with `CGEventPostToPid` to the owner; display streams post to the HID tap.
  Keys carry the client's text via `CGEventKeyboardSetUnicodeString`, so host and client
  layouts need not agree; bare modifier keys post as `FlagsChanged`. Needs post-event
  (Accessibility) access: `slopty-worker` preflights at start-up, warns, and asks once.

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
  arrived as `echoecho` after activation). So the injector activates the owner before a
  button-down or key press when it is not active, and the client sends `Focus` when a screen
  view gains focus. The host's own desktop sees that app come to the front. How it activates
  changed 2026-09-30: "A window stream's pointer reaches the view".

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
  ⌘Q/⌘H/⌘M stay with the app (menu bar; for ⌘Q and ⌘H, superseded below on 2026-10-03 in a
  tile sending system shortcuts). Modifier-only presses are not forwarded (GPUI has no
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
  Superseded 2026-09-30: "Trackpad gestures reach the remote app".

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
  are signed as `dev.aislopware.slopty.worker` and `….ptyd`, the same strings the LaunchAgents are
  labelled with, by `cargo xtask sign`; `xtask run worker` calls it after the build so nobody has to
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
  signed identity — which is exactly one now that the identity is stable. `slopty worker doctor`
  still reports both, so a headless install can gate on it.
  - Amended 2026-09-26: **only the installed daemon asks.** Every worker a test started asked
    too, as a fresh unsigned binary, so each gate put an Accessibility and a Screen Recording
    prompt on the screen of whoever used this Mac. `slopty worker install` now passes
    `--ask-permissions` and nothing else does. A worker without the grant also never reaches
    ScreenCaptureKit: `Pipeline::shareable` preflights and answers `NotPermitted`, since an
    enumeration without the grant raises the private-window consent prompt. Test:
    `plists_carry_the_paths_and_flags`.
  - Also 2026-09-26: **the installed worker stopped asking every five seconds.** Its
    capability watch listed the displays through ScreenCaptureKit on each tick, and on macOS 26
    each such enumeration by a process not yet allowed to bypass the private window picker
    raised that prompt again: tccd logged 960 requests from the worker in 40 minutes. The watch
    now reads the displays from CoreGraphics (`slopty_capture::active_displays`, the same
    `display_info` formula the stream's listing uses), which needs no grant and asks nobody.
    ScreenCaptureKit is reached only when someone opens or lists a stream, so the prompt comes
    at most once, when it is first used, and "Allow" holds it for the month macOS gives. Test:
    `the_displays_are_listed_without_a_grant`.

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
  Focus leaving the view, or the window going inactive (⌘-tab away with ⌘ held), releases
  every key and button held on the worker (`let_go`). Tests: headless
  `modifier_keys_go_to_the_worker_as_they_move` and
  `losing_focus_releases_what_is_held_on_the_worker`.

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
  Test: headless `losing_focus_releases_what_is_held_on_the_worker`.

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

  iOS reaches the same state machine by a different road, and the road is not the one it looks
  like. `gpui_ios` has a `UIPanGestureRecognizer` for scrolling, but it sets
  `maximumNumberOfTouches: 0`, so it carries indirect scroll only — a trackpad or a wheel on an
  iPad — and no finger ever reaches it. Finger scrolling runs through `gpui/src/gestures.rs`,
  which recognises a pan from raw touches and coasts it on a `UIScrollView` deceleration curve,
  ticked from the window each frame. Its shape is the macOS one plus a closing event: `Started`,
  `Moved`s, `Ended` at the lift, the coast's `Moved`s, then **one more `Ended`**. Read naively
  that second `Ended` is a gesture ending twice, and the momentum it was closing stays open until
  `MOMENTUM_GAP` runs out — the exact latch this ruling exists to prevent, on the platform where
  flinging is the only way to scroll. It closes the momentum instead.

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

  Test: `a_fling_over_the_picture_reaches_the_worker_as_a_gesture_and_then_as_momentum` drives the
  whole shape, both ways it can finish, and the doubled open.

- ✅ **A stream that ends lets go of everything held on the worker** (2026-09-25). A connection
  that dropped mid-⌘-chord or mid-drag left ⌘ or the button down on the worker, and on a display
  stream that is the worker's whole session. Nothing let go of held input when a stream closed or
  its connection went. The injector tracked buttons but not keys, and the client's `ScreenView`
  dropped whatever did not fit the connection's full outbound queue, releases included. Ruling:
  the injector keeps held keys as well as buttons, and `Injector::release_all` lets go of each
  button where the pointer last was, then each key, then each modifier, whose release carries
  the modifiers still down. A drop does the same. The sink is dropped when `ScreenStream::close`
  ends, which both a client's close and a lost connection go through, so both end with nothing
  held. `InputSink::release_all` is there to let go before the close waits on ScreenCaptureKit
  as well. On the client the view writes through an `Outbox`. What the queue has no room for
  waits and goes in order as room frees, and a move replaces a move waiting last. Past 256
  waiting, input is dropped unless it ends something: a key or button release, a scroll's or
  momentum's end. Tests: `release_all_lets_go_of_every_button_key_and_modifier`,
  `dropping_the_injector_lets_go`, `dropping_the_sink_lets_go_of_what_it_held`,
  `a_full_queue_coalesces_moves_and_keeps_every_release`.

- ✅ **Each stream's input is injected on a thread of its own** (2026-09-25). The stream's tokio
  task called the injector inline, and the injector talked to the window server there. It read
  the target's bounds (`CGWindowListCreateDescriptionFromArray`) every 100 ms of pointer input,
  and looked the owner up in `NSRunningApplication` on every key-down and button-down. Measured
  on the task, a move cost up to 12 ms and a key press 1–2 ms at the median and up to 17 ms
  (MEASUREMENTS.md, "input injection off the runtime"). Ruling: `CgEvents` is an `InputThread`.
  The stream's task hands each event over a channel, and the thread owns the `Injector`, so
  order holds and the task waits on nothing. A second thread re-reads the bounds every 80 ms
  while pointer input flows and hands them to the injector (`Injector::set_bounds`). It stops a
  second after the pointer does and is woken by the next pointer event. Only bounds older than
  the 100 ms TTL are read in front of an event: the first move after a quiet second, or a wake
  that raced the reader going idle. The owner found active, or activated, is taken as active
  for 250 ms. Activation lands some milliseconds after it is asked for, so the check on every
  key re-activated an owner that was already coming forward. The price is that a key typed
  within 250 ms of the owner losing activation some other way waits in the owner's queue until
  the next check. The reader thread went later the same day, when the geometry probe's bounds
  took its place (below). Tests: `fresh_bounds_from_outside_spare_the_read`,
  `a_burst_of_keys_checks_the_owner_once`.

- ✅ **The pointer maps with the scale the view asked for** (2026-09-25). `ScreenView` mapped a
  pointer into the size of the last decoded frame. After a scale change (zooming a card out,
  the overview) frames at the old scale are still in flight for about a round trip. The worker
  applies the new scale in order with the input behind it, so every click in that time landed
  at the old scale's position. The view now keeps the size the worker maps input with apart
  from the picture's: the size it asked for, or the one `Geometry` reported. Test:
  `input_maps_with_the_scale_asked_for_not_the_frame_in_flight`.

- ✅ **Held input is let go before the capture stops** (2026-09-25). The sink let go of held keys
  and buttons only when it was dropped, and `ScreenStream::close` drops it after ScreenCaptureKit
  answers the stop, which can take a while. For that long ⌘ or a button stayed down on the
  worker. The stream task now calls `ScreenStream::release_input` as soon as its command loop
  ends and before `close`. The input thread posts the releases while the close waits, and the
  drop afterwards finds nothing held. Test: `release_all_lets_go_while_the_sink_lives`. The
  order in `screens.rs` has no test, since a `ScreenStream` needs a real capture.

- ✅ **Input maps through the geometry probe's bounds** (2026-09-25). Two things read the same
  bounds. The stream's geometry probe read them every 100 ms off the runtime, and the input
  thread had a reader of its own, every 80 ms while pointer input flowed. Now `check_geometry`
  hands each probe's bounds and read time to the sink (`InputSink::set_bounds`), in order with
  the input, and the reader thread is gone. The probe runs whether or not input flows, so the
  first move after a pause no longer reads in front of itself either. `BOUNDS_TTL` went from 100
  to 250 ms. At 100 ms, bounds handed over every 100 ms would expire just as the next ones
  arrived, and a probe held up by a slow window-server answer would put a read in front of a
  move. Past 250 ms the injector still reads the bounds itself, which covers an injector nobody
  feeds.

  100 ms against 80 ms makes no difference to a window drag. Neither cadence follows a window
  moving under the pointer between reads. When the client drags a window by its own title bar,
  fresher bounds are worse, not better. Each move maps to the new origin plus the pointer's
  place in the picture, so the window gains the distance it has already moved, again, at every
  read. Pinning the bounds from button-down to button-up would fix that. It needs a check on
  hardware first, since it is not settled whether a title-bar drag posted to a pid moves the
  window at all. Test: `the_probes_bounds_spare_every_read_in_front_of_the_pointer`.

- ✅ **A window stream's pointer is where its input put it** (2026-09-25). Audit #15. On a window
  stream there was no pointer, or one stuck in place. Events posted to the window's owner leave
  the worker's pointer alone, yet the cursor loop sampled that real pointer. Meanwhile the card
  hides the client's own pointer whenever a frame is up. The injector already kept the last
  place it put the pointer, for letting go of buttons. It now also writes that place to a
  `PointerWatch`, and the cursor loop sends it for streams routed to a pid. Before the first
  event the sample says hidden. Display streams go through the HID tap and move the real
  pointer, so they still read it. The shape still comes from the system cursor, which follows
  the real pointer and may not match what the window would show at the placed point. Keeping
  the client's own pointer visible on window cards would have been the other way. It shows
  input without the round trip, but not the worker's cursor picture or where a click landed
  after clamping. Tests: `a_window_streams_pointer_is_where_its_input_put_it`,
  `a_window_streams_cursor_is_where_its_input_put_the_pointer`. A hardware check is still owed.
  Stream a window, hover and click, and see the pointer follow.
  - Amended 2026-09-28: **the client draws the pointer where it put it.** On a window stream
    the worker's sample is only the `PointerWatch` echo of this client's own input. Drawing it
    cost a round trip, up to a cursor tick and a frame for a point the client already had,
    about 30 to 67 ms to glass on a 10 to 40 ms mesh against 19 ms now (MEASUREMENTS, "a remote
    pointer drawn where the client put it"). The objection above does not hold for the way it
    is now done. The overlay still draws the worker's cursor picture (`ScreenEvent::Cursor`),
    only at the client's point. The point is clamped to the stream's bounds, as the release
    already was, and rounded to whole stream pixels, as the worker's sample is. So a click is
    shown where the worker puts it. `ScreenView` draws its `pointer_at` on a window stream
    once this client has put the pointer somewhere, and a move redraws the view with the
    pointer in that frame. A display is different: the worker's own hand, another client or an
    app's warp moves the real pointer. There the client's point is drawn for `LOCAL_HOLD`
    (200 ms) plus the round trip after its last move, which covers the echo of its own moves.
    Outside that hold the worker's sample is drawn, and a sample that differs from the
    client's point when the hold ends redraws the view then. Trackpad mode keeps drawing the
    trackpad's pointer. What is left between this and a local pointer is one app frame: the
    overlay is drawn by the app, not by the hardware cursor. A fork `CursorStyle` that carries
    the worker's picture would remove that; it was built on 2026-09-30 ("The remote pointer is
    the system cursor" below). Tests:
    `a_window_streams_pointer_is_drawn_where_this_client_put_it_in_the_same_frame`,
    `a_displays_pointer_follows_the_worker_unless_this_client_moves_it`.

- ✅ **The worker's pointer is drawn at the scale input maps with** (2026-09-25). The cursor
  overlay and the IME caret divided the worker's sample by the size of the frame on screen.
  After a scale change that is the old size until the first new frame is decoded, while the
  worker samples at the new scale as soon as it applies the change. Both now divide by
  `mapped`, the size input uses. That is off the other way for samples the worker sent before
  it applied the change, so the view rescales the sample it holds when it asks for the new
  scale. A still pointer stays put, and only a pointer that moved in the round trip before the
  ask is drawn wrong until the worker's next sample. Test:
  `the_workers_pointer_is_drawn_at_the_scale_asked_for`.

- ✅ **A button pressed on the picture is always let go on the worker** (2026-09-25). gpui
  reports a mouse-up only over the element that saw the press, so a drag let go off the
  picture never sent its release. The worker then kept the button down and turned every later
  hover into a drag on the real desktop. `ScreenView` now keeps the buttons whose press went to
  the worker, as it keeps keys. A release off the picture comes in through `on_mouse_up_out`
  and goes at the picture's nearest edge, where the worker's pointer stopped. Focus leaving the
  view or the window going inactive releases held buttons along with keys and modifiers, at the
  last position sent. A release with no press behind it sends nothing. A stream that closes is
  let go by the worker (`release_input` before the close). Test:
  `a_held_button_is_released_off_the_picture_and_on_focus_loss`.

- ✅ **The worker lets go of every held key and button before it exits** (2026-09-25). Only
  SIGINT was handled. launchd stops a job with SIGTERM (`launchctl kickstart -k`, `bootout`), and
  that killed the daemon with whatever clients held still down on the desktop. Even on SIGINT,
  the streams' tasks were dropped with the runtime rather than awaited, so an input thread still
  posting when the process ended posted nothing. SIGTERM now shuts down the way SIGINT does. After
  the endpoint closes and drains, `slopty_input::let_go_everywhere` asks every input thread in the
  process to release what it holds and waits up to 500 ms for the posts. A thread whose handle is
  already gone lets go on its way out, and the wait covers it too. The threads are counted as
  they start, in a process-wide census of weak handles, since the streams own their sinks and
  nothing else can reach them. Tests: `the_census_lets_go_of_every_stream_before_the_process_ends`
  and `the_census_wait_is_bounded`. The signal wiring in `apps/slopty-worker/src/main.rs` has
  no test; it is checked by reading.

- ✅ **A remote picture zooms inside its tile, and fingers can be a trackpad** (2026-09-28). On
  a phone a 5K display drawn at fit is unreadable and a 12 pt control is smaller than a
  fingertip, and a pinch over the picture opened the workspace overview. Four rulings, all on
  the client, with no wire change:
  1. *The pinch belongs to what it starts on.* The strip's capture handler decides at the
     pinch's `Started` and keeps the decision for the whole pinch. A pinch that begins on the
     body of a remote picture that would keep a sideways swipe (every picture on a phone, the
     focused one on the Mac) goes on to the picture, with the overview closed. Anything else,
     and any pinch while the overview is open, is the overview's, and the strip now stops it
     there so the picture under it does not zoom as well.
  2. *The zoom is a fraction of the body.* `screen::Zoom` holds a scale over fit and the
     picture's top-left, both in fractions of the tile's body. A tile that changes size keeps
     the same part of the picture in view, and the picture is placed with `relative()` lengths,
     so a body that changed between the zoom and the draw needs no correction. The scale runs
     from fit to twice one to one (never less than twice fit). The origin is clamped so a zoomed
     picture always covers the body. A pinch keeps the point under the fingers where it is,
     and the centroid's travel pans. A double tap goes to one to one about the tapped point,
     or to twice fit where fit already magnifies, and back. Its second tap is not sent, since
     the first has clicked; a remote double click by touch is trackpad mode's. The zoom lives
     on the view, so it is per tile and lasts as long as the tile. Everything the view maps goes
     through it: a tap, a click, a release off the body, the worker's drawn pointer and the IME
     caret. A zoomed picture asks the worker for the scale it is drawn at (the existing
     `SetQuality`, bucketed and rate-limited as before), so it sharpens up to native as it grows.
  3. *Trackpad mode is per tile.* One finger claims gpui's touch drag, so it is neither a tap
     nor a pan, and moves a pointer relatively. The gain is 1 up to 150 pt/s, so aiming is point
     for point, and rises linearly to 3× at 1500 pt/s. A point of finger travel is a point of
     pointer travel on the drawn picture, so zoomed in the same stroke covers fewer of the
     target's pixels. A tap clicks at the pointer, a quick second tap double-clicks, a still
     finger held for 500 ms right-clicks, and a tap followed at once by a drag drags with the
     button held. Two fingers lock to a pinch (6 % of scale) or a drag (10 pt), whichever comes
     first: the pinch zooms, the drag scrolls at the pointer with the phases a trackpad sends.
     The pointer is drawn where the fingers put it, not one round trip later where the worker
     says it is. Zoomed in, the view pans to keep it `spacing.xl` inside the body's edges.
     Turning the mode off lets go of a held drag.
  4. *Two fingers tapped right-click*, on glass only, at the pointer in trackpad mode and under
     the fingers otherwise. It relies on UIKit's pinch recognizer reporting a two-finger tap
     that barely moves (under 8 pt and 6 % in 300 ms); a device run has to confirm that it does.
  A zoom puts a small readout up (`Fit`, or the size against one to one, so `100 %` is pixel for
  pixel) for 700 ms, which then fades over `kit::FADE`, or goes at once under Reduce Motion. The
  toggle is `screen::ToggleTrackpad`: "Trackpad mode" in the palette, handled by the focused
  picture and otherwise by the workspace for the active one, and `ScreenView::trackpad_button`
  is its header control on glass, filled while the mode is on
  (`the_palettes_trackpad_mode_reaches_the_active_picture`).
  Tests: `screen::zoom::tests` (mapping at fit, about the fingers, at the edges, clamps, the
  double tap, revealing, the readout), `screen::touch::tests` (the acceleration curve, relative
  moves, taps, hold, tap-drag, the two-finger lock), `a_tap_lands_on_the_pixel_drawn_under_it_at_any_zoom`,
  `a_double_tap_toggles_fit_and_one_to_one_on_glass`, `two_fingers_zoom_pan_and_tap`,
  `trackpad_mode_moves_a_pointer_and_clicks_where_it_is`,
  `the_zoom_readout_fades_and_the_stream_follows_the_zoom`, and the workspace's
  `a_pinch_over_a_stream_zooms_it_and_over_a_shell_opens_the_overview`. The iOS simulator
  harness can deliver a pinch (`UiPinch`) but has no streamed picture to aim it at, so these
  run at the slopty-ui layer on gpui's own pinch and touch-drag events. The frame cost is in
  MEASUREMENTS ("a zoomed stream's frame"). Panning a zoomed picture on the Mac, where a
  trackpad pinch reports no centroid travel, is the next entry.

- ✅ **A zoomed picture pans when the pointer pushes at the edge, and with ⌥-scroll**
  (2026-10-03). On the Mac a zoomed picture could not be panned: a trackpad pinch reports no
  centroid travel, and every scroll and drag belongs to the remote app. Other clients:
  - Apple's Screen Sharing offers three ways to scroll a scaled-up screen: continuously with
    the cursor, when the cursor reaches an edge, or only by the scroll bars.
  - Jump Desktop pans with the pointer, with an optional "Screen Scrolling" on ⌘-scroll that
    is off by default, and zooms on ⌥⌘ shortcuts.
  - RealVNC Viewer has scroll bars, and "bump scrolling" at the edges in full screen.
  - Parsec documents no zoom.
  - Screens 5 and Windows App document no pan of a zoomed picture.
  Two of them use the pointer at the edge, and it needs no hand off the pointing device, so:
  - **Edge push.** This Mac's pointer inside a `spacing.xl` band at an edge of a zoomed
    picture's body pans the picture toward that edge for as long as it stays there. The pan
    grows with the square of how deep in the band the pointer is, up to 1.5 frames a second at
    the edge, stepped at 120 Hz (`zoom::edge_push`, `EDGE_PAN_FRAMES_PER_S`). It stops at the
    picture's own edge, when the pointer leaves the band or the body, and at fit. As the picture
    moves under the still pointer, the worker's pointer is moved to what is now under it, so a
    click lands where it is drawn. A body that reaches the display's edge puts the band where the
    pointer stops, which is Screen Sharing's "when the cursor reaches an edge".
  - **⌥-scroll pans** a zoomed picture by the scroll's travel (a wheel's line is `spacing.xl`)
    and sends nothing to the worker, for a person who wants a precise pan. A trackpad gesture
    decides as it begins and keeps its side to the end of its coast, so letting go of ⌥ halfway
    leaves neither the pan nor the remote app's scroll without its end. ⌥ was the free
    modifier: ⌘-scroll is the remote app's (the ⌘-scroll entry below), ⌘⌥ is the workspace's,
    ⌃-scroll is macOS's own accessibility zoom, and ⇧-scroll is a horizontal scroll. ⌥-scroll
    at fit still goes to the remote app.
  Rejected: panning continuously with the cursor (the picture moves on every pointer move, so
  nothing holds still under the pointer to be clicked); scroll bars (chrome on every zoomed
  picture, against the design); and a key binding (the arrow keys and every chord belong to the
  remote app, and the palette is no way to steer). Tests:
  `the_mac_pans_a_zoomed_picture_at_the_edges_and_with_option_scroll`,
  `screen::zoom::tests::the_pointer_pushes_at_the_edges`.

- ✅ **⌘-scroll is the tile's, no longer held back for a zoom** (2026-09-28). The canvas zoomed
  on ⌘-scroll, so the terminal dropped a ⌘-wheel and a remote picture never sent one. The
  scrolling workspace has no such zoom (its own wheel chord is ⌘⌥, taken before the tiles see
  it), and the guards had become a dead key: ⌘-scroll did nothing anywhere. A terminal now
  scrolls its history on ⌘-scroll as on a plain one, and a remote picture forwards it with ⌘
  held, as the same gesture on that Mac would arrive. Tests:
  `the_wheel_adds_up_fractions_and_reaches_a_program_that_wants_it`,
  `pointer_and_scroll_reach_the_worker_in_stream_pixels`.

- ✅ **A move with another move queued behind it is not posted** (2026-09-28). The input thread
  posted every job in turn. After a stall (the owner lookup before a press, up to 17 ms, or a
  bounds read no probe spared) it posted each move handed over meanwhile, replaying a path the
  client had left, before the click or key behind them. Now the thread takes what is queued
  before it posts a move. It drops the move when the next job, skipping new bounds and a new
  scale, is another move. Every other job keeps its turn. Only the pointer's place is lost, and
  a drag keeps its button state because the next move posts as the same drag. With a 17 ms stall
  at 500 Hz moves, 257–264 of 6 000 moves are passed over and every post's p99 drops from 18.8 to
  10.1–10.7 ms (MEASUREMENTS, "moves queued behind a stall"). A drawing app behind a stall gets a
  straight segment where the client drew a curve. That is the price. Test:
  `moves_queued_behind_a_stall_post_as_one_before_the_click`.

- ✅ **A window's input is applied from whichever copy comes first, clicks and keys in order and
  moves latest-wins** (2026-09-28). Window input now has datagram copies beside the control
  stream (`docs/decisions/transport.md`, same date). The worker applies each click, key, scroll
  and pinch exactly once and in the order sent. A move may overtake older moves but never a
  click, key or quality change sent before it, and a move older than anything applied is
  dropped, so the pointer never jumps back. Every ordered input carries its own position, so
  skipping the moves before a click changes nothing the click does. Tests:
  `a_window_input_copy_applies_once_and_clicks_and_keys_only_in_turn`,
  `window_input_through_any_race_lands_in_order_and_never_goes_back` (500 random races of
  stream delay and copy loss), `window_input_through_a_lossy_link_lands_once_in_order` (the real
  link through a shaper losing a fifth of its packets each way).

- ✅ **System shortcuts go to the remote Mac through a session tap, behind a per-tile toggle**
  (2026-09-28). macOS acts on ⌘Tab, ⌘Space, ⌃Space, ⌃ with an arrow (Spaces, Mission Control)
  and ⌘⇧3/4/5 before any app sees them, so the key path GPUI feeds `ScreenView` never gets
  them. Jump Desktop and Screens 5 send them to a Mac host; Parsec needs its HID mode for it.
  - *How.* `slopty_platform::system_keys::Tap` is a `CGEventTap` on the session, at the head,
    for key-downs and key-ups, on the main run loop. While armed it swallows the chords the
    pure filter (`system_keys::chord`) names and hands them to the workspace, which sends them
    to the focused tile's worker as `ScreenInput::Key`. The modifiers go the usual way, as
    GPUI reports them. A taken key's repeats and release are taken with it even after the tile
    lets go, and `ScreenView::let_go` releases one still held. macOS turns a slow tap off; the
    callback turns it back on.
  - *When.* The tap is armed only while the app is frontmost (checked in the callback too), its
    window is active, no palette is up, and a remote tile with the toggle on has the keyboard.
    The toggle is per tile and off by default, offered on the Mac for a Mac worker's windows
    and displays: "Send system shortcuts" in the palette. While on, the header shows it as a
    pressed control that turns it off. A notice says each change ("System shortcuts go to
    studio" / "System shortcuts stay on this Mac").
  - *The way out stays here.* ⌃Tab is not a system chord and still leaves the tile. A click
    elsewhere or another app disarms the tap. ⌘⌥Esc and ⌃⌘Q are never taken.
  - *The grant.* An active tap needs Accessibility (`CGPreflightPostEventAccess`). The tap is
    made only when the person turns the toggle on. Without the grant, `CGRequestPostEventAccess`
    shows macOS's prompt once, a notice says what to allow, and the tile keeps to this Mac. No
    test makes a tap or asks: the filter is unit-tested in `slopty-platform`, and the workspace
    test drives a stand-in `KeyPort`. Research had it on by default once granted; it is off
    until turned on, since a session tap in a process the person did not ask for one in is
    the surveillance pattern `CLAUDE.md` rules out.
  - *Not measured.* A chord passes one run-loop turn in the tap's channel before it reaches the
    view; no gated test can time a real tap without the grant.
  - Tests: `system_keys::tests::the_filter_takes_the_system_chords_only`,
    `system_keys::tests::a_taken_press_takes_its_release`, workspace
    `desktop::system_shortcuts_go_to_the_remote_mac_per_tile`.

- ✅ **"Type the clipboard" types it key by key, up to 1 KB** (2026-09-28). For a remote login
  window or a field that refuses paste, where the pasteboard offer cannot land. The palette
  command, offered while a remote tile has the focus, sends this device's clipboard text as
  the committed-text path does: one press and release per character, a line break as one ↩.
  It stops at `screen::TYPE_MAX` bytes on a character boundary, as Jump caps it, and a notice
  says so. Test: `desktop::the_clipboard_is_typed_into_the_focused_window`.

- ✅ **Private input APIs are looked up at run time, pinned and tested per macOS version**
  (2026-09-29). The user allows private APIs for input where they are the only route. This
  overrides the no-private-API rule of `decisions/video.md` for input only, on the pattern
  `slopty-vdisplay` set: each symbol is found with `dlsym` at run time, checked before use,
  and a missing one degrades to the public path instead of failing to load or crashing.
  Nothing private is linked, so notarization sees nothing. Every private number is spelled
  once, next to its use, naming the header or project it comes from. Each path carries a
  gated test that needs no permission and posts nothing, plus an opt-in live test for what
  only real delivery shows, and the OS builds a hardware check passed on are recorded here.
  A public route, where one exists, comes first: the WindowServer's hotkey layer, the first
  candidate, turned out to have one (below), so no private call is made yet. Research and
  staging: `.research/design-input-fidelity.md`.

- ✅ **Keys go by position; the worker types under the client's input source** (2026-09-29).
  Supersedes the 2026-09-04 text-on-every-key path and the 2026-09-28 key-by-key typing of
  "Type the clipboard". Every character outside ASCII was lost on the way to a remote window:
  the client named a key by the character it typed, anything else became `Unidentified`, and
  the worker dropped the event with the text on it. So é, Cyrillic, every Vietnamese, Japanese
  or Chinese commit and a non-ASCII clipboard never arrived. ⌘ chords also landed wrong when
  layouts differed (a US ⌘Q became ⌘A on an AZERTY worker), and the text attached to every key
  kept the worker's dead keys and input methods from ever composing.
  - *The wire.* `ScreenInput::Key { code, action, mods }` carries no text. `code` is a
    position, the one `kVK_*` table in `slopty-proto` (`KeyCode::to_mac_vk` and
    `from_mac_vk`), which the client and the worker share, and `mods` keep the side of each
    modifier and fn (`Mods::FN`). New: `Text` (committed text), `Lock { caps }`,
    `Media { key, down }`, `KeyboardSource { source }`, and the answer
    `ScreenEvent::KeyboardSource { stream, source, applied }`. The source ask rides with the
    input rather than as a `ScreenRequest`, so the keys after it are read in order under it,
    and the connection's request routing did not change.
  - *The client, Mac.* A key's position comes off the event AppKit is dispatching
    (`NSApp.currentEvent.keyCode`, `slopty_platform::keyboard::current`): GPUI's `Keystroke`
    has no physical code, and reading the event beside it needs no fork change. When the tile
    takes the keyboard, and whenever the person switches source (HIToolbox's distributed
    notification), the view sends its TIS input-source id. Until the worker answers `applied`,
    the text system here composes. Characters, dead keys and input methods go to it, and what
    it commits goes as `Text`, while named keys go by position. A ⌘ or ⌃ chord goes by its
    character's place on a US keyboard while the worker is not under this device's source: the
    worker matches it by character under its own layout, and the US place is where every Latin
    QWERTY layout, and the ASCII layout macOS matches ⌘ against under a non-Latin one, puts
    that character, so ⌘A typed on AZERTY stays ⌘A there instead of quitting as ⌘Q. The
    client does not know the worker's layout, so it also names the character, and the worker
    presses the key that types it under its own layout ("A shortcut goes by its character",
    below); its release goes to the key its press went as. Under the client's source the key's own place is right. Once the worker
    answers, a local `NSEvent` monitor takes every key without ⌘ or ⌃ off
    `-[NSApplication sendEvent:]`, ahead of the window and the input method, and the view
    sends it as it is, so the remote app composes inline. The chords still reach GPUI, whose
    bindings (⌃Tab out of the tile, the workspace's ⌘ chords) come first. A key the monitor
    took has its release taken too, and taken keys are drained before anything else the view
    sends, so a chord or a click never overtakes one. ⌘⌥⎋ and ⌃⌘Q stay on this Mac.
  - *The client, iPad.* An iPad names neither positions nor a source a Mac can select, so it
    always composes: UIKit's text system types, its commits go as `Text`, and chords name
    their character ("A shortcut goes by its character", below). `UIKey.keyCode` needs the fork change the design
    names, which this change does not make.
  - *The worker.* A key is `CGEventCreateKeyboardEvent` with the position and the flags, the
    side's device bit included, and no Unicode string. Events come from a private-state
    `CGEventSource`, one per input thread, and carry `kCGEventSourceUserData` = `SLOPTY_EVENT`.
    `Text` goes in pieces of at most 20 UTF-16 units (`CGEventKeyboardSetUnicodeString` cuts
    there, Chrome Remote Desktop found), cut only between the composed character sequences
    CoreFoundation reports. Each piece rides a Space press and release (never keycode 0) with
    no modifier but Caps Lock. `Lock` sets the lock through `IOHIDSetModifierLockState` and
    never posts the key. The lock is the worker's, so every stream that sets it shares one
    claim (`slopty_input::CapsClaims`): the worker's own state is read as the first stream sets
    it and comes back when the last lets go, only if the lock is still as the streams last set
    it, so a change the person at the worker made meanwhile stays. The worker's own state and
    the one set are kept in `caps-lock` in the data directory while a stream holds the lock,
    and a start that finds the file puts the lock back under the same rule. The HID parameter
    connection is opened once and kept, and it is never called under the shared claim, so a
    `Lock` waits on no other stream's HID call. `Media` posts `NSSystemDefined` subtype 8 to the
    HID tap for play/pause, next and previous.
  - *Several clients.* The input source is the worker's for all its windows, so the last
    stream to ask wins: the person typing last is the one looking. A claim is a token drawn
    once per stream task (`sources::Claim`, a process-wide counter), not the client and stream
    ids: a client that reconnects opens `StreamId(1)` again under the same `ClientId` before
    its old stream has ended, and the old stream's release must not take the new one's claim.
    The token is a guard, so a stream task that ends any way, a panic included, queues its
    release. When a stream ends, the source of the latest
    stream still asking comes back, then the worker's own once none asks, and every source the
    worker turned on for a claim (`TISEnableInputSource`) goes off again. A source the worker
    cannot select is answered `applied: false`, and that client composes. The worker restores
    only what claims selected: it remembers the source they last selected (in memory and in
    `input-source`), puts the worker's own back only while that one is still current, and
    never turns off the current source, so a source the person at the worker picked by hand
    stays, at the last release, at `SIGTERM` and at the next start alike. TIS runs on the
    worker's main queue, since HIToolbox asserts it; a stream queues its ask from its own
    loop, so the release as it ends is queued behind it and no claim is left behind.
  - *Who is typing under the source.* Every switch on the worker is heard
    (`kTISNotifySelectedKeyboardInputSourceChanged` on the worker's main run loop,
    `Sources::heard`), and each stream compares it with its client's source: a client whose
    source another client took is told `applied: false` and goes back to composing, and is
    told `applied: true` when its source comes back.
  - *The answer and the keys behind it.* An ask for the source the worker already has is
    answered at once. A switch is answered once the worker hears a switch to it reported after
    the selection (the ask subscribes to the reports on the main queue right after selecting,
    so a report from before, of the same source, cannot answer it), the same distributed
    notification that tells the target app, at most 100 ms (`SETTLE_MOST`) after it; that
    replaces a fixed 50 ms that was chosen and never measured. The stream holds the keys and
    text that came after the ask until the answer, at most 150 ms (`HOLD_MOST`), so the keys
    typed right after focus are read under the source they were typed for, ⌘ chords included
    (a position means another chord under another layout). The pointer is not held, since no
    source changes what a click, a move or a scroll does, unless a key is already held ahead
    of it, where order must hold. How long a real switch takes on the worker cannot be
    measured here without switching this Mac's source; the bound is kept, and the opt-in
    `SLOPTY_MEASURE=1 SLOPTY_MEASURE_SWITCH=<source id> cargo test -p slopty-input --test
    key_path` measures it on a machine where switching is fine. The other way, answering the
    wire's promise by changing it, costs nothing on the keys but lets the first chords after a
    switch land under the old layout; the hold costs latency only while a switch is under way.
  - *How long a claim holds.* From the ask until the stream ends or the client sends
    `KeyboardReleased`, which it does once its tile has gone 10 s without the keyboard
    (`RELEASE_AFTER`). Letting go at every blur was rejected: hopping between tiles or opening
    the palette would cost a real switch and a hold on the first keys each time back, while a
    claim kept for a tile left alone keeps the person at the worker off their own source for
    as long as the stream is open. Coming back after the release asks again: the client keeps
    taking keys at once, and the worker holds them until its switch back is heard (the cost is
    that switch; an ask for the source still current is answered in tens of microseconds,
    MEASUREMENTS).
  - *A worker that stops.* The worker's own source, those it turned on and the one claims last
    selected are kept in `input-source` in its data directory while a claim holds; `SIGTERM`
    puts them back before the daemon exits, and a start that finds the file (a crash,
    `SIGKILL`) puts them back before it serves anyone, each under the rule above.
  - *The client while the answer is coming.* A tile taking the keyboard again under the
    source the worker already took keeps taking keys: nothing waits for the answer. Keys are
    never taken while a composition is marked here: a word under way when the worker answers
    finishes here and goes as text, and the next key is taken. GPUI reports no modifier
    change that leaves the modifiers as they were (both ⇧ down, one let go), so every held
    modifier key whose modifier is off at the next change is let go.
  - *Terminals are untouched.* Their keys go through `keys::key_event` and the engine's
    encoder as before. `Mods::FN` is set only on screen input.
  - *Not yet.* The iPad's `lang:` sources, the HID-usage table, and the stage 5 virtual
    keyboard. The remote caret, the opt-out and the paste by character landed on 2026-10-03
    (the three entries at the end).
  - Tests: proto `every_mac_vk_round_trips`, `every_key_has_a_mac_position_or_is_listed`,
    goldens `client_screen_{key,text,lock,media,keyboard_source}` and
    `worker_screen_keyboard_source`. UI (`screen::keyboard::tests`):
    `a_non_ascii_character_reaches_the_worker` (stage 0's pin, which fails on the old path),
    `a_dead_key_composes_here_until_the_worker_takes_the_source`,
    `a_dead_key_goes_raw_once_the_worker_took_the_source`,
    `a_vietnamese_ime_commit_goes_as_text` (Telex and VNI), `a_japanese_ime_commit_goes_as_text`,
    `a_key_goes_by_position_not_by_character` (⌘Q, AZERTY),
    `taken_keys_go_before_what_follows_them`, `right_modifiers_keep_their_side`,
    `caps_lock_state_follows_focus_and_changes`,
    `the_source_is_told_on_focus_and_on_every_switch`,
    `overlapping_modifiers_are_all_let_go`,
    `a_composition_under_way_when_the_worker_answers_finishes_here` (Telex straight after
    focus), `the_keyboard_taken_again_under_the_same_source_takes_keys_at_once`,
    `a_source_the_worker_lost_is_composed_here_again`,
    `a_chord_the_worker_reads_under_its_own_source_goes_by_character`,
    `a_tile_left_a_while_lets_the_source_go`. Worker, built and never posted:
    `backend::tests::keys_carry_no_unicode_string`, `text_rides_on_space_with_its_string`,
    `media_keys_post_system_defined_subtype_8`, `text::tests::text_goes_in_graphemes_of_at_most_20_units`,
    `injector::tests::{text_goes_in_pieces_without_modifiers, caps_lock_sets_the_lock_not_a_key,
    caps_lock_goes_back_when_the_last_stream_lets_go (and never under the shared claim),
    caps_lock_the_person_changed_is_left_as_they_set_it,
    caps_lock_is_kept_and_put_back_at_the_next_start, media_keys_go_to_the_system}`,
    `sources::tests::{every_stream_draws_its_own_claimant,
    a_reconnected_streams_claim_outlives_the_old_streams_release,
    a_source_already_current_is_answered_at_once, a_switch_is_answered_once_heard,
    a_stale_report_of_the_source_does_not_answer_a_switch,
    sources_turned_on_go_off_once_none_asks, a_source_picked_by_hand_is_left_as_it_is,
    a_release_runs_after_the_claim_before_it, a_claim_dropped_by_a_panic_is_released,
    the_original_source_is_kept_and_restored_at_the_next_start,
    a_restore_at_start_leaves_a_source_picked_since, stopping_lets_every_claim_go}`;
    the stream (`slopty-worker` `screens::sourcing`):
    `a_key_after_a_switch_waits_for_it_to_be_heard` (checked by order, not by a time),
    `the_pointer_waits_only_behind_a_held_key`,
    `a_reconnected_streams_source_outlives_the_old_stream`,
    `a_released_keyboard_gives_the_source_back`, `asking_for_the_current_source_holds_nothing`,
    `a_client_whose_source_another_took_is_told`. Tests hold for 10 s rather than 150 ms, so a
    loaded machine cannot let a key through before the answer the test waits on. Platform:
    `keyboard::tests::plain_keys_are_taken_and_chords_pass`. The cost of the taken path and of
    the answer: MEASUREMENTS, "a taken key's hop and the input-source answer".

- ✅ **System shortcuts: the WindowServer's hotkey layer goes off while a tile has the
  keyboard** (2026-09-29). Amends the 2026-09-28 tap entry. The tap took a fixed list, so
  customised symbolic hotkeys, the Mission Control and Launchpad keys, ⌃1…⌃9 and the
  input-source keys stayed on the client. While the tap is armed (same conditions as before)
  and the app is frontmost, HIToolbox's public `PushSymbolicHotKeyMode` turns every symbolic
  hotkey off but the Accessibility ones (`kHIHotKeyModeAllDisabledExceptUniversalAccess`, the
  mode VirtualBox picks), and `PopSymbolicHotKeyMode` turns them back on. Every other symbolic
  hotkey then reaches the app as an ordinary key and goes to the worker by position. The
  tap's chord list (⌘Tab, ⌘Space, ⌃ arrows and screenshots) is taken as well, whether or not
  the layer went off: `PushSymbolicHotKeyMode` hands back a token whenever the app has
  Accessibility, and the WindowServer can still decline to apply the mode (the app not
  frontmost as it pushed), which the app cannot see. Gating the list on the token left it
  dead exactly when it was needed. A chord the tap takes is swallowed there, so it never
  also reaches the app and goes twice. When macOS refuses the push outright, a notice says
  only the list goes.
  - *Scope.* The mode is stored on the app's WindowServer connection, but while that
    connection lives it holds for the whole session: the WindowServer ORs every connection's
    mode into one session field that hotkey matching reads (SkyLight on macOS 27.0.1:
    `_XSetGlobalHotKeyOperatingMode` writes the connection's field,
    `CGXUpdateGlobalHotKeyOperatingMode` ORs them, `CGXCheckForHotKey` reads the session's).
    A mode flagged as the foreground app's counts only while that connection is in front, and
    a connection that dies is taken out and the mode worked out again. The private
    `CGSSetGlobalHotKeyOperatingMode` that SDL, UTM and VirtualBox call sets no such flag, so
    a tile armed through it held the person's ⌘Tab off in every other app until the view
    noticed. `PushSymbolicHotKeyMode` sets it when the app is frontmost, as `CarbonEvents.h`
    says: the mode is active only while the app stays frontmost and reverts when the app is
    deactivated or exits without popping it. That settles hardware check H5 without posting
    anything, so the launch-time repair is dropped: no run can leave the mode behind.
  - *Restoring.* A guard (`HotkeysOff`) holds the mode: disarming, dropping the tap and
    unwinding from a panic pop it. The tap pops it as `NSApplicationDidResignActive` is posted
    and pushes it again on the way back while armed (`system_keys::Hotkeys`), the view
    disarms as its window stops being the key window rather than at its next frame, and the
    app pops what is left as it terminates (GPUI's `on_app_quit`; ⌘Q drops no view).
    Per-hotkey `CGSSetSymbolicHotKeyEnabled` was rejected because its effect outlives the
    process.
  - *The ways out.* In this mode SkyLight's hotkey table keeps Force Quit (⌘⌥⎋, and ⌘⌥⇧⎋)
    among the hotkeys that still fire, with the power, eject and Accessibility keys, so ⌘⌥⎋
    stays on this Mac as ruled; ⌘⇥, Spotlight, screenshots and ⇧⌃⌥⌘Q go to the worker. ⌃⌘Q is
    no WindowServer hotkey at all but AppKit's Apple menu in this process, which the mode does
    not touch.
  - *Media keys.* The armed tap also takes `NX_SYSDEFINED` subtype 8 events for play/pause,
    next and previous (`FAST` and `REWIND` too, which an Apple keyboard sends). They go to the
    worker as `ScreenInput::Media`. Volume, mute and brightness stay on this Mac, where the
    stream's sound plays.
  - Tests: `system_keys::tests::the_hotkey_mode_is_restored_on_disarm_and_drop` (the guard
    over a recording seam, including the unwind and a refusal),
    `leaving_the_app_turns_the_layer_back_on_at_once`,
    `a_refused_layer_falls_back_to_the_chord_list`, `media_keys_go_and_volume_stays`; the
    workspace's `the_window_going_inactive_lets_the_shortcuts_be` and
    `a_notice_says_when_only_the_chord_list_goes`.

- ✅ **Trackpad gestures reach the remote app** (2026-09-30, M1 Max, macOS 27.0). The wire carried
  a pinch and the worker dropped it, and there was no rotation, smart zoom or swipe at all.
  - *Wire.* `ScreenInput` gains `Rotate { degrees, phase, x, y }`, `SmartMagnify { x, y }`,
    `Swipe { direction, x, y }` and `Gestures { remote }` beside `Magnify`, appended so no
    older golden moved. `SwipeDirection` is named as the trackpad's swipe mask names it and
    read by apps as `deltaX` / `deltaY`: left is `deltaX` 1, right −1, up `deltaY` 1, down −1.
    Goldens `client_screen_magnify`, `client_screen_rotate`, `client_screen_smart_magnify`,
    `client_screen_swipe`, `client_screen_gestures_remote`.
  - *Injection.* No public API makes a gesture `CGEvent`, so the worker makes one the way Mac
    Mouse Fix (`TouchSimulator.m`, `GestureScrollSimulator.m`) and Sensible Side Buttons do:
    `CGEventCreate`, the type `NSEventTypeGesture` (29), the IOHID subtype in field 110 that
    AppKit turns into the event kind (8 magnify, 5 rotate, 22 smart magnify, 16 swipe), the
    amount in 113 (magnification) or 114 (degrees) or the swipe mask in 115, and the phase in
    132 as IOHID's phase bits, the same values as `CGScrollPhase`. Each goes on the stream's
    route, like a scroll: to the owner's pid for a window, the HID tap for a display.
  - *One swipe, one event.* AppKit makes an `NSEventTypeSwipe` of every navigation swipe
    posted, a directionless one included (`deltaX` 0), so the began-then-ended pair that
    Sensible Side Buttons posts reaches the app as two swipes. A swipe is now one event, its
    end with its direction, whatever the phase field says.
  - *Swipes between pages.* A two-finger swipe that an app follows as it moves (Safari's back
    and forward, `trackSwipeEventWithOptions:`) is not a swipe event: AppKit's tracking reads
    the gesture events a trackpad sends beside its scroll events. With the tile's gestures sent
    (`Gestures { remote: true }`), each trackpad scroll in its gesture phase (MayBegin and
    Cancelled included, never its momentum) is followed by a gesture of subtype 6
    (`kIOHIDEventTypeScroll`) with the scroll's travel scaled by 1.67 in fields 116 and 119 and
    the same phase, as Mac Mouse Fix posts it (scroll first, then gesture). AppKit takes those
    in: no app sees one as an event. Without them a sideways swipe's tracking stays at its first
    step (−0.010) and is cancelled when the fingers lift; with them it follows the fingers
    (−0.010, −0.027, −0.043 … −0.144), ends, and turns the page (−1.0). That holds at a
    trackpad's 120 reports a second whether they arrive steadily, each 0 to 12 ms late, or two
    at once every 16.7 ms (`docs/MEASUREMENTS.md`, "a trackpad scroll's gesture, one swipe, a
    press's number").
    - *What 120 was not.* An earlier cut measured 60 reports a second only, and a scratch run
      at 120 turned one swipe in three. The cause was the test's swipe, not the rate: its steps
      (8, 20, 30, 40 points) at 120 a second made an accelerating flick of 25 to 33 ms, which
      AppKit's tracking cancels whatever the gesture carries. Nothing on the gesture changed
      that: one timestamp for both events or the scroll's own, the fixed-point deltas in lines,
      −0 for no travel, the travel scaled by 0, 1, 1.67 or 3, the gesture before its scroll or
      after, AppKit's mouse coalescing off, the lift held back 25 to 50 ms, or the reports
      merged down to 60 a second (which would also add up to 16.7 ms of latency). Mac Mouse
      Fix's fields 41, 134, 135 and 139 belong to its dock swipe (subtype 23, the branch its
      `TouchSimulator.m` marks "pre-macOS 27"; on 27 it builds the dock swipe from an IOHIDEvent,
      since the window server ignores those fields there) and not to a scroll's gesture, which
      `GestureScrollSimulator.m` gives only 55, 110, 116, 119 and 132, as ours does. Set on the
      scroll's gesture anyway, 41 and 134 changed nothing a guest could tell apart from its
      noise (14 of 20 turned without, 8 of 20 with). Swipes of a trackpad's shape, 10 points a
      report, or 150 ms at 1 to 3 points a millisecond, turned 30 of 30 at 60 and at 120 on
      this Mac. So nothing re-paces the reports.
    - *What 120 was, in a guest.* On macOS 26.6 in a tart guest the same test turned the page
      in 1 run of 7. The guest's scheduler held posts up to 45 ms and then let two go at once,
      and the lift's verdict reads the events' timestamps, which were the moments they were
      posted: 18 of 36 swipes turned. Posted at the client's spacing instead, 34 of 36 did
      ("A gesture's events keep the client's spacing").
    The pairing stays behind the tile's toggle, off by default, so a stream scrolls as before
    until someone asks for its gestures. The toggle is read once per gesture, at its first
    phase, so turning it over mid-swipe never leaves AppKit a gesture with no end or an end
    with no start (`a_scroll_gesture_keeps_its_pairing_to_its_end`), and a stream that moves to
    another display keeps it (`a_display_switch_keeps_the_gestures_sent`, `slopty-workerd`). A
    scroll whose start never reached this injector goes on unpaired
    (`a_scroll_that_never_opened_here_goes_unpaired`), and a stream that ends mid-gesture
    cancels each gesture under way, its coast and a pinch or rotation too
    (`a_stream_ending_mid_gesture_cancels_it`); the client starts a new pinch on each start,
    whatever the last one left (`a_pinch_keeps_its_side_when_the_gestures_turn_over`).
  - *Proof, live.* `tests/inject.rs` posts through the real injector and the real
    `CGEventPostToPid` to an application the test starts itself (`tests/support/gesture_app.rs`)
    and to nothing else: the injector's route is checked to be that pid before anything is
    posted. It runs in a guest of the VM lane (`cargo xtask vm live -p slopty-input --test
    inject`), never on a Mac someone works at. The app's window is clear, shadowless and at the
    desktop's level, and it reads each event off its queue before AppKit dispatches it, so what
    is checked is the event AppKit made: every scroll phase and coast as posted, pinch and
    rotation phases with their amounts, one smart zoom, and exactly one swipe per swipe with its
    direction. Whether a view gets them is the next entries' ("A window stream's pointer
    reaches the view"). Swipe tracking needs a gesture's point on a display: 8 000 points off
    every screen it cancels at once.
  - *The client.* A pinch over a picture on the Mac zooms the picture, as the 2026-09-28 ruling
    has it and as Jump Desktop and Screens do with theirs. A tile can instead send its
    trackpad's gestures to the remote app (`screen::ToggleRemoteGestures`, "Gestures to the
    remote app"): the worker is told (`Gestures`), a pinch that begins on the picture then goes
    as `Magnify` with GPUI's phases (its may-begin folded into began), at the stream pixel
    under the fingers, followed to its end off the picture at the picture's nearest edge (as a
    drag is), and the picture keeps its zoom. Off by default until the palette offers the
    command, which lives with the workspace's keymap.
    Rotation, smart zoom and the swipe reach nothing yet: the pinned gpui-fast (`f994c34`)
    registers no `rotateWithEvent:` or `smartMagnifyWithEvent:`, has no `PlatformInput` for
    either, and turns `NSEventTypeSwipe` into a `Navigate` mouse button on its end alone, with
    no vertical swipe and nothing to tell it from a mouse's side button.
  - Tests: `a_gesture_reads_back_as_appkit_reads_the_trackpad` (the backend, read back through
    `NSEvent`; a scroll's gesture shows its type and each phase, and AppKit has no accessor for
    its travel: `deltaX` and `deltaY` read 0 and `gestureAmount` raises), `gestures_reach_the_target_as_the_trackpad_makes_them` and
    `a_trackpad_scroll_comes_with_its_gesture_and_its_coast_alone` (the injector),
    `each_gesture_reaches_the_app_as_a_trackpad_s_does`,
    `a_swipe_between_pages_follows_the_fingers_only_with_their_gesture` and
    `a_trackpad_scroll_s_post_cost` (live, `tests/inject.rs`), and
    `a_pinch_goes_to_the_remote_app_when_its_gestures_are_sent` (`slopty-ui`).

- ✅ **A press, its drags and its release share one event number** (2026-09-30, M1 Max, macOS
  27.0). The injector never set `kCGMouseEventNumber`, and on macOS 27 AppKit follows a drag by
  it: a press, the drags after it and its release must carry the same one, or the drag is lost
  (a window dragged by its title bar among them).
  - *What went.* Read back by the test's own app, a press, its drags and its release posted
    without it all carry event number 0: no press of their own at all.
  - *Now.* Each press takes the next number of a counter the whole worker process shares, so
    no two presses on two streams share one; its drags (whichever button drags, which also now
    carries its own button number rather than the left's) and its release carry it, and
    `release_all` lets go under it. A move with nothing held carries none.
  - *Proof.* `a_press_its_drags_and_its_release_share_one_number` (the injector, every button);
    live, `a_drag_reaches_the_app_under_its_press_number` (`tests/inject.rs`): the app reads the
    press, its drags (AppKit coalesces the ones that wait in its queue) and its release under
    one number and the next press under another. The app reads presses and does not dispatch
    them, and the test's backend takes the app to be active, so nothing raises it over the desktop
    of whoever is at the Mac; whether AppKit's own drag tracking refuses number 0 is therefore
    not seen here, only that the numbers now are what a real press carries.

- ✅ **A window stream's pointer reaches the view: bound to its window, its app switched to by
  the window server** (2026-09-30, macOS 26.6.2 in a tart guest; the drag-and-drop P0 (4) left
  it to this lane, `docs/decisions/audio.md`). A pointer event posted to a pid reaches the
  app's queue with window 0, and AppKit hands it to no view. A regular app's key window is no
  exception, so no click, scroll or pinch of a window stream reached any view.
  - *Probe* (a regular app of the test's own at the normal level, 2 × 5 × 2 cases: its window
    uncovered or covered by another of its kind; not activated, asked through
    `NSRunningApplication`, switched to through SkyLight with and without its make-key record,
    or clicked on its title through the HID tap; each event posted plain, or bound). Posted
    plain, nothing reached the view in any of the 20. Bound (the event's window in field 51 and
    its point in the window through `CGEventSetWindowLocation`, both CoreGraphics' own and in
    no header): the scroll reaches the view always, covered or not, active or not; a press on
    an app not active is AppKit's click-through question (`acceptsFirstMouse:`), and with the
    common answer NO it reaches no view and raises nothing; on an active app whose window is not
    key the first press makes it key, and the rest reach the view; on an active app with its
    window key, every press, drag, release, scroll and pinch reaches the view.
  - *Activation.* Asked through `NSRunningApplication` from a process in the background,
    which the worker is, macOS never activated the app while another app was active: since
    macOS 14 an app becomes active only when the active one yields to it. When no app was
    active it sometimes did, which is how it seemed to work before. The window server's own
    switch does it at once (active and key in 11 ms): `_SLPSSetFrontProcessWithOptions` with
    the window and `kCPSUserGenerated`, then the two make-key records `SLPSPostEventRecordTo`
    takes, as yabai, AltTab and Hammerspoon switch windows. They are private to SkyLight, so
    `backend::front` finds them at run time and falls back to `NSRunningApplication` without
    them.
  - *Now.* A window stream's pointer events go bound to the window where its bounds last put it
    (`Route::Window`); its keys and text go to the pid, which delivers them to the key window as
    a keyboard's are. Right presses still go through the HID tap for the menu. Before a press or
    a key, an owner that is not the active app is switched to with the streamed window key, on
    the same 250 ms check as before, and so is every owner when the tile takes focus (`Focus`).
    So the first click on a window stream of an app in the background reaches the view, and so
    does its key. An owner already active with another of its windows key keeps that one until
    the tile's focus or a click makes the streamed one key.
  - Tests (the VM lane, `cargo xtask vm live -p slopty-input --test inject`):
    `a_window_stream_s_pointer_reaches_a_regular_app_s_view_bound_to_its_window`,
    `only_the_window_server_s_switch_activates_from_the_background` and
    `a_click_on_an_app_in_the_background_reaches_its_view` (the real backend: press, drag,
    release and a key reach the view of an app that was behind another's);
    `a_window_stream_s_pointer_is_bound_to_its_window_and_its_keys_are_not` (the injector). The
    probe's matrix is in MEASUREMENTS, "a window stream's pointer reaches the view".
  - Open: macOS 27 (no 27 guest yet). The switch posts two records the app reads as a press and
    a release with no point (`x=NaN`), as yabai's do; no view takes them.

- ✅ **A gesture's events keep the client's spacing** (2026-09-30, macOS 26.6.2 in a tart guest).
  The app a gesture reaches judges it by its events' timestamps: whether a swipe between pages
  turns the page when the fingers lift, how fast a scroll or a pinch was going. Stamped as it is
  posted, an event carries the path's delay: a report held up by the network or the worker's
  scheduler and then posted with the next turns a steady swipe into a stop and a jerk.
  - *Wire.* `ScreenInput::Scroll`, `Magnify` and `Rotate` carry `time_us`: when the client's
    view read them, microseconds on its own monotonic clock, low 32 bits
    (`ScreenInput::time_us`). GPUI hands over no time of AppKit's, so the view's own moment it
    is. Goldens `client_screen_scroll`, `client_screen_magnify`, `client_screen_rotate`.
  - *Worker.* The injector's `Timeline` stamps each event of a gesture (a precise scroll, its
    gesture, its coast, a pinch, a rotation) at its client stamp plus the least delay any event
    of that gesture has had from the client so far, in nanoseconds of uptime, which
    `NSEvent.timestamp` reads in seconds. An event held up is stamped where it would have been
    with no hold-up, since one before it came faster; the delay itself is never taken off, so
    no stamp is later than its post, and none goes back. The two Macs' clocks are never
    compared: only the delay's changes within one gesture count. A gesture's first phase, or a
    step of more than a second or back, starts the timeline again. A wheel's lines keep the
    system's stamp.
  - *Numbers* (MEASUREMENTS, "a gesture's events keep the client's spacing"): swipes of twelve
    10-point reports at 120 a second, arriving steadily, jittered or in pairs through a guest
    that also holds posts up: 18 of 36 turned at post-time stamps, 34 of 36 at the client's
    spacing. It costs the input thread 70 ns a scroll, a read of the host clock.
  - Tests: `a_gesture_held_up_on_the_way_keeps_the_client_s_spacing`,
    `a_gesture_s_timeline_starts_again_and_its_coast_carries_on`,
    `a_hostile_client_s_stamps_never_go_back_or_ahead` (the injector);
    `a_stamped_scroll_or_gesture_carries_its_time` (the backend, read back through `NSEvent`);
    `a_gesture_s_reports_carry_when_they_were_read` (`slopty-ui`); live,
    `a_swipe_between_pages_follows_the_fingers_only_with_their_gesture`, now stamped by its
    client at 120 a second, which gives a paired swipe a second go since a guest can still hold
    a whole swipe up.
  - Open: a stamp read where AppKit made the event (a GPUI event time from the gpui-fast
    lane) would take the client's main thread out of it too. Pointer moves are not stamped.

- ✅ **The injector feeds a drag through the HID tap** (2026-09-30; phase P2 of "Drag and drop
  lands at the point", `docs/decisions/audio.md`, the injector's half only). The drag manager
  follows the real pointer, and a pid-posted drag starts no session (P0 (4)).
  - `Injector::enter_drag` switches a window stream's owner to the front (the HID tap reaches
    whatever window is on top), keeps where the real pointer is, and from then on sends every
    pointer event of the stream through the HID tap; keys still go to the pid. `press_at` posts
    the left press at the source and a drag `DRAG_START` (2) points on, under one new number
    (`press_number`), so the source's view begins its session. `nudge(now)` moves a drag resting
    in one place a point off and back, once per `NUDGE_EVERY` (100 ms) since the last drag event,
    which springs a spring-loaded target (P0 (8b)); the release lands where the drag rests.
    `cancel_drag` posts Escape through the HID tap before the release (P0 (3)), with no
    modifier but Caps Lock's, since ⌘ or ⌥⌘ held from the drag would make it a system shortcut.
    `leave_drag` lets go of a press still held where the drag rests (the drop), puts a window
    stream's real pointer back, and returns the stream to its own route. A stream that ends
    mid-drag cancels it (`release_all`, and dropping the injector), so nothing is dropped that
    nobody dropped. A press held on the stream's own route before the drag is let go there
    first, and a `press_at` that fails leaves no drag on.
  - Cost: a move in a drag is 69 ns of the injector's own work against 145 ns outside one (no
    placed pointer to write).
  - Tests: `a_drag_goes_through_the_hid_tap_under_one_number_and_the_pointer_goes_back`,
    `a_display_stream_s_drag_leaves_the_pointer_where_it_ended`,
    `the_first_drag_stays_on_the_target_and_each_press_is_new`,
    `a_resting_drag_is_nudged_a_point_each_way`, `cancelling_a_drag_escapes_before_the_release`,
    `a_drag_s_escape_carries_no_modifier_but_caps_lock`, `a_press_at_a_window_gone_leaves_no_drag`,
    `a_drag_lets_go_of_a_press_on_its_own_route_first` and `a_stream_ending_mid_drag_cancels_it`.
    The worker's `DragIn` and the helper that use it are the rest of P2.

- ✅ **The remote pointer is the system cursor; its shape follows the window server's seed**
  (2026-09-30). Gap-audit items 1 and 7. The pointer over a remote tile was drawn by the view,
  so every move cost one app frame (19 ms notify → glass) before the hand saw it, and the
  worker read its picture from `NSCursor.currentSystemCursor` every 33 ms, which the header
  says "will always be `nil` in a future version".
  - *The client (fork).* gpui-fast gains `CursorStyle::Image(id)`: the application points an
    id at a `CursorImage` (premultiplied BGRA, hotspot, pixels per point) with
    `App::set_cursor_image`, and macOS shows it as an `NSCursor` in the view's cursor rect.
    The window server moves that cursor with the hand, as it moves a local one, so a move
    costs no frame. A new picture under an id that is showing is set at once, before any
    frame; the last 64 cursors built are kept by content. No upstream pull request existed
    (zed and longbridge searched for image or custom cursors). Branch `cursor-image` on
    aislopware/gpui-fast, merged into its main at `132dbc1`, which Slopty pins.
  - *The fork's review* (2026-10-01) fixed two things before the merge. A new picture used to
    invalidate the cursor rects of the key window only, so another window showing the same id
    kept the old cursor when it was key again; every window whose view shows the id is
    invalidated now, and the cursor is set at once over the key window's view only. The
    cache matched on a 64-bit hash of the picture alone; a hit now compares the pixels too, so
    two pictures whose keys collide never share a cursor. Checked and left as they were: the
    `NSCursor` lives while an id or the cache holds it and the rect retains its own; everything
    runs on the main thread; the hotspot is in points from the top left; a 1× picture on a 2×
    display keeps its size in points; the cursor rect is the view's whole bounds, so a scroll
    moves nothing, and the video's native host is non-interactive and under the GPUI view, so
    it never takes the cursor; nothing hides or unhides the cursor; an inactive application
    sets nothing; a tile losing focus changes nothing, since the style follows the hovered
    hitbox.
  - *The view.* On macOS the system pointer is the worker's picture whenever the pointer is
    this client's own: always on a window stream, and for the hold after a move on a display.
    The view draws the worker's picture only where the worker moves the pointer itself (its
    own user, an app's warp, another client) and in trackpad mode, as the Chrome Remote
    Desktop model has it (`design-input-fidelity.md` §4.1). The switch costs one frame at the
    transition, none per move. The pointer keeps its size in points, as a local one does,
    whatever the tile's zoom. iOS keeps the drawn pointer: `UIPointerShape` takes paths, not
    pictures.
  - *The worker.* `CGSCurrentCursorSeed`, polled at 120 Hz, says when the cursor changed; only
    then is the picture read, with `CGSGetGlobalCursorDataSize`/`CGSGetGlobalCursorData`.
    Both are private, CoreGraphics re-exports of SkyLight, found with `dlsym` under the rule
    above. The deprecated reading is gone, not kept behind them: the floor has the calls, and
    on a macOS without them the worker sends no picture and the client shows its own arrow.
    The picture is the one the window server draws, at the scale of the display it is on,
    which `CursorShape::scale` carries; a client on a Retina display shows a 1× worker's
    cursor scaled up, as Parsec does. The data's layout (premultiplied, a host-order ARGB
    word, hotspot and rect in points) is pinned by comparing it pixel for pixel with
    `currentSystemCursor` while that still answers.
  - *Colour order, checked* (2026-10-01). A test app the test spawns
    (`slopty-cursor-app`) takes the cursor from the background (`SetsCursorInBackground`) and
    shows a 16-point square of red, green, blue and half-covered white with its hotspot at
    (3, 5). The worker's reading gives red as BGRA `[0, 0, 255, 255]`, blue as
    `[255, 0, 0, 255]` and the white as `[128, 128, 128, 128]`, hotspot (3, 5) at 1×, so the
    bytes are BGRA, premultiplied, as `OSXvnc` said. On the client, the fork draws a cursor's
    picture into an sRGB RGBA bitmap and gets the colours back as given, which a byte-for-byte
    check could not tell from swapped channels.
  - *Hardware checks.* The calls were found and read on macOS 27.0.1 (26A434), a 1× display.
    The 2× units are proved on synthetic data at each layer (the reading, the view's
    conversion, the fork's cursor): a 64-pixel picture at 2× with its hotspot on pixel
    (11, 12) is a 32-point cursor with its hotspot at (5.5, 6) points. Owed: a real 2×
    display, and the pointer by hand over a window and a display tile.
  - Numbers: MEASUREMENTS, "the remote pointer as the system cursor, and the cursor seed".
    Tests: fork `the_pointer_takes_the_picture_as_it_enters_and_moving_over_it_draws_nothing`,
    `an_id_is_pointed_at_pictures_and_forgotten_without_a_frame`,
    `a_cursor_is_its_picture_in_points_with_the_hotspot_in_points`,
    `an_id_pointed_back_at_a_picture_it_showed_builds_nothing`; `slopty-capture`
    `this_macos_exports_the_window_servers_cursor_calls`,
    `the_global_cursor_is_the_system_cursors_picture_and_its_seed_costs_nanoseconds`,
    `the_global_data_is_read_at_its_displays_scale_with_the_hotspot_in_pixels`,
    `a_coloured_cursor_reads_back_in_bgra_with_its_hotspot_at_its_scale`; fork
    `a_cursor_draws_its_pictures_colours_not_its_byte_order`; `slopty-ui`
    `a_window_streams_pointer_is_the_system_pointer_in_the_workers_picture`,
    `the_system_pointer_takes_the_workers_scale_so_the_hotspot_stays_on_its_point`;
    `slopty-worker` `the_shape_loop_reads_the_picture_only_when_the_seed_moves`.

- ✅ **A paste is the layout's V, on both sides** (2026-10-03, readiness C10). The client
  read ⌘V by the V *position*, so on a Dvorak Mac (V where US has a period) ⌘V went out as an
  ordinary chord the worker did not hold behind the clipboard, and its pasteboard offer could
  land after the paste. Now the view knows a paste by its character (`key_char` "v" under ⌘,
  whatever key carries it) and sends `ScreenInput::PasteChord { code, mods }`: the key that
  was pressed, so the worker posts that same key under the client's source, and the word that
  it is the paste, so the worker holds it behind the clipboard offer and nothing else. The
  release goes as an ordinary `Key`. "Press" in the palette sends the same pair for ⌘V.
  `PasteChord` is a new wire variant at the end of `ScreenInput` (golden
  `client_screen_paste_chord`, 10 bytes framed, two fewer than a `Key`'s 12), and `is_paste_chord` is now a
  match on it. Nothing new runs per key: the character is already in the event. Tests:
  `screen::keyboard::tests::a_paste_is_the_layouts_v_wherever_its_key_sits` (Dvorak),
  `slopty-workerd` `conn::a_paste_holds_its_own_window_and_no_other` (⌘ at the V position is
  not a paste), `slopty-client` `window_input_is_numbered_per_stream_with_its_in_order_count`.

- ✅ **The input method's candidates sit under the worker's caret** (2026-10-03, readiness
  C10). The candidate window of a composition on the client hung at the pointer, since the
  view had no idea where the worker's text caret was. The worker now tells it: 60 ms after
  input that can move the caret (a focus, a key, a paste, text, a button up; `FIELD_AFTER`), it
  reads the focused element through the accessibility API (`AXFocusedApplication` →
  `AXFocusedUIElement` → `AXBoundsForRange` of `AXSelectedTextRange`, 100 ms per-element
  messaging timeout, off the runtime on a blocking thread) and sends
  `ScreenEvent::Field { stream, field }` only when it changed. The field carries the caret in
  stream pixels, only when it is inside the streamed window or display and, for a window, only
  when the focused app owns it, and `secure` for an `AXSecureTextField`. The view's
  `bounds_for_range` answers with the caret, so AppKit places the candidates below it; with
  no caret it falls back to the pointer as before. The read is never on the key path: it
  waits for the keys to stop for 60 ms, and a read still running is not started again.
  Tests: `slopty-worker` `a_fields_caret_is_told_in_stream_pixels_over_the_target`,
  `slopty-workerd` `the_field_with_the_keyboard_is_told_after_typing_and_only_as_it_changes`,
  `slopty-ui` `the_input_method_hangs_its_candidates_under_the_workers_caret`, golden
  `worker_screen_field`.

- ✅ **A worker can keep its own input source** (2026-10-03, readiness C10). `[worker]
  input_source_sync = false` makes the worker refuse every claim (`Sources::follow_clients`):
  a client whose source is already the worker's is answered `applied: true`, any other
  `applied: false` and composes on its side, as for a source the worker cannot select. For a
  worker whose person types at it too and wants their source left alone. Applied as the file
  changes (2026-10-05): turned off, every claim is let go, so the worker's own source comes
  back at once and each stream hears the switch and tells its client to compose. Tests:
  `sources::tests::a_worker_that_refuses_claims_keeps_its_own_source`,
  `syncing_turned_off_while_running_gives_the_worker_its_source_back`.

- ✅ **A remote window keeps its chords and reaches ours with ⌃** (2026-10-03, readiness
  A22). With a remote window focused, ⌘⇧P, ⌘⇧M and ⌘⇧I are the remote app's (VS Code's
  palette), so the palette, the mute and the stats could not be reached without leaving the
  tile. They take ⌃ on top there: ⌃⌘⇧P, ⌃⌘⇧M and ⌃⌘⇧I, bound in `Workspace > Screen` (Zed's
  predicates read a plain `Workspace && Screen` against the last context only, so it never
  matched), and rebindable like every command. In a tile sending system shortcuts to its
  worker, ⌘Q, ⌘H and ⌘⌥H go there too (`keymap::app_chords`, bound in `!SystemKeys`): they
  quit or hide the remote app, as on the worker's own keyboard. A click or ⌃Tab out of the
  tile gives them back, and ⌃⌘Q stays on this Mac. Test: `slopty-ui`
  `desktop::a_remote_window_keeps_its_chords_and_reaches_ours_with_control`.

- ✅ **Secure keyboard entry, balanced** (2026-10-03, readiness A24). Terminal and Ghostty
  keep a password from other programs' event taps with secure event input. Slopty turns it on
  while the focused tile takes a password and an app window is in front: a terminal whose
  program has turned echo off in canonical mode (`TerminalView::at_password_prompt`), or a
  remote window whose focused field the worker reports `secure`. `[terminal]
  secure_keyboard_entry` makes it "At passwords" (the default), "Always" (while a window is
  in front) or "Never". `EnableSecureEventInput` is counted per process, and one left over
  keeps every other app's shortcuts dead, so `slopty_platform::secure_input::SecureInput`
  owns it: it reaches the system only on a change, never enables twice, and turns off as it
  drops. The workspace follows it from its render (focus, the window coming forward or going
  back), a terminal's change and a stream's. Tests:
  `secure_input::tests::secure_input_is_balanced_whatever_is_asked` (a counting switch),
  `workspace::tests::secure_entry_holds_while_the_focused_shell_reads_a_password`,
  `desktop::a_remote_password_field_holds_secure_entry`.

- ✅ **No page chord on iOS that does nothing** (2026-10-04). ⌘Z, ⌘⇧Z, ⌘X and ⌘A in a page that
  holds the keyboard are the page's own edits (`browser::Edit`), which the Mac's keymap does
  because AppKit's menu would take them elsewhere. UIKit hands a hardware keyboard's keys to the
  web view that holds them, and the page edits on its own, so on iOS the keymap never hears
  them and those entries only listed chords that did nothing. They are bound on macOS only.

- ✅ **An iPad is a daily client: Meta and paging on the key bar, menus with a keyboard**
  (2026-10-04, readiness N24). On an iPad the key bar had no Alt, Page Up or Page Down. With a
  keyboard attached, the menu bar and the held-⌘ sheet were empty: `gpui_ios`'s `set_menus`
  was a stub, and the app never called it. The keyboard shortcuts were reachable only from the
  Mac's Help menu.
  - *Key bar.* ⌥ arms Alt for the next key as Meta, whatever the ⌥ setting: a typed letter
    goes as Alt and that letter, case kept, which the worker sends as `ESC b`, and ← → go as
    the Mac's word motion. PgUp and PgDn page; ⇞ and ⇟ were tried and draw too small to read.
    Home and End are not added: the armed ⌘ with ← → is already the line's start and end.
    The modifiers became the glyphs iPadOS's own shortcut sheet uses (⌃ ⌥ ⌘), square caps.
    The row spreads into its groups from an 11-inch iPad (820 pt), and on the narrowest one
    (744 pt) it now scrolls a little, its trailing end fading. A screen reader still hears
    each key's name ("Control", "Alt", "Page up").
  - *Menus.* The menus are one builder (`slopty_app::menus`) for the Mac and the iPad. The iPad
    gets no Window menu and none of the Mac's own items. Its Upload and Download are the Files
    picker's, named as the palette names them. The iPad app sets them after the workspace
    opens, as the Mac app does. The fork turns them into `UIMainMenuSystem` menus with key
    commands (aislopware/gpui-fast#21, `feat/ios-menus`), and they show once Slopty pins that
    commit; until then the call meets the old stub.
  - *Palette.* "Keyboard shortcuts" is a palette line on every device.
  - Tests: `sticky_alt_sends_the_next_key_as_meta` (`slopty-ui`);
    `an_ipads_menus_are_the_macs_without_the_macs_own`,
    `the_palette_offers_the_keyboard_shortcuts`, and the key bar's width and layout tests
    (`slopty-app`); the fork's host menu tests and its simulator `menu-test`.

- ✅ **An iPad's trackpad clicks as a mouse, and its pointer takes the cursor's shape**
  (2026-10-10, readiness audit "carried": no right-click and no pointer shapes on an iPad).
  The work is in the gpui-fast fork's `gpui_ios` (`93fa22a7`).
  - **Clicks.** A `UITouchTypeIndirectPointer` touch, which a trackpad or mouse makes with
    `UIApplicationSupportsIndirectInputEvents` set, goes to GPUI as mouse events of its
    button, as an `NSEvent` click does. It is the right button when the event's `buttonMask`
    holds the secondary (a two-finger click or tap, a mouse's right button), and the left
    otherwise. The button is read when the touch begins and kept until it ends, since the
    mask is empty by the release. Double and triple clicks are counted there (500 ms, 4 pt),
    because UIKit leaves that to the app. Before, a pointer click was a touch, so gpui core's
    recognizer made a click-drag a pan that scrolled rather than selecting, and a two-finger
    click was a plain tap. Fingers and the pencil stay touches.
  - **What reaches Slopty.** Nothing in Slopty changed for the menus: `kit::menu_press` opens
    a thing's menu on a right mouse down as well as a long press, so the tile, tab, project,
    machine and row menus open on a secondary click. The terminal's and the remote desktop's
    right button paths are the Mac's.
  - **Pointer shapes.** A `UIPointerInteraction` on the window's view answers with the look
    GPUI's cursor style last set, and is invalidated when the look changes. Text gets a beam
    (lying down for vertical text). `CursorStyle::None` hides the pointer, which is what the
    remote desktop sets while it draws the far side's cursor. The resize cursors get outline
    double arrows along their axis, and the crosshair a cross. The hands, the drag badges and
    a picture keep iPadOS's own pointer, because iPadOS draws shapes, not cursor images, and
    iPad apps keep its pointer over buttons.
  - Tests: in the fork, `gpui_ios::pointer` on the host (each cursor's look, the outlines
    centred on the hot spot, a pointer touch's button, the click count). In Slopty, the live
    simulator e2e `ios_uikit::a_trackpad_clicks_and_right_clicks_on_the_simulator` drives
    `UiPointerClick` through the fork's own delivery: a primary click focuses a pane, a
    secondary click on its tab opens the tile's menu (on a phone, whose pane has no tab, the shell's own menu on its rows), and the pointer
    reads `beam` over the shell and `system` over a button (`Dump::pointer`).

- ✅ **A shortcut goes by its character** (2026-10-10, readiness audit item on ⌘ chords under
  a different layout). Refines "Keys go by position" above. While the worker was not under the
  client's input source, a ⌘ or ⌃ chord went by its character's place on a US keyboard, which an
  AZERTY or QWERTZ worker reads as another letter: ⌘A became ⌘Q and quit the app, ⌘Z on a German
  keyboard became ⌘Y.
  - *The wire.* `ScreenInput::Key` gains `chord: Option<String>`: for a ⌘ or ⌃ chord while
    the worker has not answered `applied`, the character the client's layout puts on the key,
    ignoring every modifier but Shift. Every other key leaves it `None` and goes by position
    alone.
  - *The worker, Mac.* `slopty_input::chords::ChordTable` maps each character to the virtual
    key that types it under the worker's current layout, lowercased: a key that types it bare
    wins over one that needs Shift, then the lower code, so the main row beats the keypad. It
    is built from `UCKeyTranslate` over the 128 virtual keys as each goes down with ⌘ held, and
    with ⌘ and Shift, dead keys off, from `TISCopyCurrentKeyboardLayoutInputSource`'s
    `kTISPropertyUnicodeKeyLayoutData` (`slopty_platform::input_source::layout_keys`). With ⌘,
    because that is what an app matches a shortcut against, and layouts differ there: Russian
    types Latin letters at their US places under ⌘, and "Dvorak - QWERTY ⌘" types QWERTY, while
    bare they type Cyrillic and Dvorak. It is rebuilt where the worker hears its
    source change (`Sources::heard`, on the main run loop, which HIToolbox needs), and shared
    by every stream through one `KeyLayout`. The injector presses a chord at the place the
    table gives, else at the position the client sent, and remembers each held key's virtual
    key, so its repeats and release hit the key its press went to even when the layout
    changes between them. A press costs one read lock and one hash lookup with no allocation,
    about 220 ns (`docs/MEASUREMENTS.md`, "a chord by its character").
  - *Linux.* Not applicable yet: a Linux worker has no screen input injector (the platform
    gives `NoInput` there), so there is nothing to place a chord for. The xkb keymap's
    keysym-to-keycode table is where it goes when one lands.
  - *The client* fills `chord` with the key's own character from the keystroke (GPUI's `key`,
    which on a Mac and an iPad alike is what the person's layout puts on the key, modifiers
    but Shift aside), lowercased, single characters only, so a named key such as an arrow
    names none. It names one on a press and a repeat, never on a release, which the worker
    sends where the press went; the key bar's ⌘ and ⌃ do the same. The position it sends
    stays the character's US place, the worker's fallback for a character its layout lacks.
    The paste chord carries none: it goes by the person's own V. Test:
    `screen::keyboard::tests::a_chord_the_worker_reads_under_its_own_source_goes_by_character`
    (slopty-ui).
  - Tests: `chords::tests` (AZERTY `a` on Q's place and `q` on A's, QWERTZ `z` on Y's, bare
    before shifted, the main row before the keypad, uppercase and several characters) and
    `a_chord_goes_by_its_character_and_lets_go_where_it_went` (`slopty-input`);
    `a_layout_names_the_key_each_character_is_on` (`slopty-platform`'s main-thread harness),
    which reads French, German, US, Russian and both Dvorak layouts macOS ships without
    selecting any; the `client_screen_key` golden (`slopty-proto`).
