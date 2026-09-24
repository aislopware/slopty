# Decisions — Multi-client

See `docs/DECISIONS.md` for the legend. Newest entries go at the end.

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

- ✅ **Where each client looks is drawn on the others' canvases, protocol 38** (2026-09-13).
  Two people on one canvas could not tell where the other was: a phone zoomed on a shell and a
  Mac panning the whole plane were invisible to each other. The client tells the host its
  viewport in canvas units (`ClientMsg::Look { view }`) once the camera has rested for
  `LOOK_EVERY` (100 ms): a pan of many frames is one message, and an unmoved viewport is never
  repeated. The host keeps a `slopty_host::presence::Presence` table — ephemeral, never in the
  document or its version — and fans each change out as `CanvasSync::Presence { client, kind,
  name, view }` to every connection; a newcomer hears the table right after the snapshot, and
  a connection's drop announces `view: None`. The client's `CanvasDoc` keeps the lookers
  (never itself: its own echo is `CanvasChange::Echo`) and the canvas draws each as an outline
  in a colour picked from the client id with its name in the corner, over the items; the
  outline has no listeners so it is never in the way, and the name is a button that flies the
  camera to what that client sees ("go where they look" without asking where that is), and
  keeps following: every move they report is flown to until this client moves the camera
  itself (wheel, pinch, drag, minimap, ⌘0/1/2, arrange) or they leave — a follow is a mode
  you leave by doing anything, never one you have to remember to switch off. The palette
  lists every other client as "Follow <name>" with its device on the right, for a client
  whose outline is off this screen. The
  minimap fits the lookers' viewports too and outlines each in its colour, so a client off
  this screen is still in the overview, and a "here" row at the top right names every other
  client with its colour dot, on screen or not — who is on this canvas, at a glance, with the
  same follow on a click (nothing new on the wire: it is the lookers the document already
  keeps). Tested at every layer: the table, the document, the
  headless canvas (the outline's a11y node and bounds; one `Look` per rest; the minimap's
  fit; the here row's pills and their follow), and two clients on a live hostd (`where_a_client_looks_reaches_the_others`). Not done on purpose: cursors
  (a viewport is what the other person can see, which is what matters for "look at this";
  a pointer at 20 Hz is a stream), and presence across hosts (a canvas is per host).

- ✅ **"Look here": one client points the others at a card, protocol 39** (2026-09-13).
  Presence says where each person is, not what they want the other to see: "look at this
  shell" still meant reading out a title. `ClientMsg::Point { item }` (⌘⇧O or the palette's
  "Point the others at this card", for the active card) is relayed by the host as
  `CanvasSync::Pointed { client, name, item }` to every connection, unchecked and unkept —
  ephemeral like presence, never in the document, and a card the host or a client no longer
  has is that client's to ignore. Each other client shows a toast at the top ("<name> points
  at <title>", a button) that goes to the card on a click (active, revealed) and otherwise
  leaves by itself after `POINT_FOR` (8 s); a newer pointing replaces it. The pointer gets
  the same toast as a status line — "pointed <name> at <title>", "pointed N others at …" —
  and with nobody else on the canvas nothing is sent and the toast says "nobody else is
  here", so ⌘⇧O never does nothing in silence. While somebody else is on the canvas the
  active card's title bar carries a "point" pill that does the same, for a phone without
  ⌘⇧O; alone, there is no pill. A toast rather
  than a jump: moving someone's camera without asking would take the canvas from under a
  drag or a read, and a follow already exists for those who want to be moved. Tested at
  every layer: the document (a pointing is `CanvasChange::Pointed`, an own echo nothing,
  the version untouched), the headless canvas (the toast's a11y node, its click, its clock,
  no toast for an unknown card or an echo; the key and the palette line send the active
  card, nothing active sends nothing), and two clients on a live hostd
  (`where_a_client_looks_reaches_the_others`). Not done: pointing at a region or a line —
  a card is the unit the canvas names, and a line in a file is the file card's reading
  line, which a name could carry later.

- ✅ **A joiner takes nothing from the others, and a slow viewer is skipped, never dropped**
  (2026-09-24). An attach built its full frame with the frame path everyone shares. That
  consumed the dirty rows and took a sequence number, but only the joiner saw the frame. The
  other viewers found a gap and asked to resync, which again took the diff the first had
  waited for. On a busy terminal two clients traded resyncs about once a round trip. A viewer
  whose sink filled (256 events, about two seconds of frames) was removed from the actor. Its
  connection still held the sink, so it got no frames, no event and no gap to notice, while
  its keys still reached the shell. Now an attach first sends the others the diff they are
  owed. Then the joiner's frame is built at their sequence number without touching the dirty
  state (`GhosttyEngine::join_frame`), with the images it needs from a fresh ledger. A viewer
  whose sink is full is marked behind and sent nothing. A task waits for half its sink to
  drain, and the viewer is then introduced again: the driver line, title, directory, colours,
  a whole frame at the others' sequence number, and the exit. Frames are encoded once for
  every viewer (`session::Outbound`), not cloned and encoded per viewer. Tests: actor
  `viewers_joining_a_busy_session_never_make_the_others_resync`,
  `a_slow_viewer_is_skipped_then_caught_up_never_dropped`; engine
  `a_joiners_frame_takes_nothing_from_the_other_viewers`.

- ✅ **Superseded 2026-09-24: geometry, presence and camera are gone from the wire** (see
  `workspace.md`). The arrangement is now each device's own layout, so there is nothing for
  two clients to share about where an item sits: the entry on item geometry and z as host
  state, and the one drawing where each client looks (`ClientMsg::Look`,
  `CanvasSync::Presence`, the outlines, the "here" row, following) no longer hold. The host
  keeps the item registry (`ItemSync`), and pointing stays: ⌘⇧O relays `ItemSync::Pointed` to
  the other clients of the focused tile's worker, whose toast goes to that tile in their own
  layout. Tested by `a_pointing_reaches_the_others` against a live hostd.
