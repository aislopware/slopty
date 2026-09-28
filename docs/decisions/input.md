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
  Not done: releasing modifiers when the view loses focus while one is down (⌘-tab away with
  ⌘ held) — the release does arrive from GPUI when the key goes up in this window, and a
  focus-loss sweep belongs with a sweep of `held` too. Test: headless
  `modifier_keys_go_to_the_worker_as_they_move`.

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
    the worker's picture would remove that, and is not built. Tests:
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
  MEASUREMENTS ("a zoomed stream's frame"). Not built: panning a zoomed picture on the Mac,
  where a trackpad pinch reports no centroid travel.

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
