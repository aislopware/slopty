# Decisions — Workspace

See `docs/DECISIONS.md` for the legend. Newest entries go at the end. This file supersedes the
camera, placement and navigation entries of `canvas.md` (the infinite plane, ⌘1/⌘2/⌘0 and
arrange, flights, the minimap, the reading-order walk, presence); the entries there about
notes, file cards, the palette, naming and agents still hold, read with "tile" for "card".

- ✅ **A scrollable tiling workspace replaces the infinite canvas** (2026-09-24). The canvas
  asked the human to place, size and find every item by hand, and the camera (zoom, fit,
  arrange, minimap, other clients' outlines) was the price of that freedom. The user asked for
  niri's model instead: each workspace is an endless strip of columns, a column holds one or
  more tiles, the view scrolls along the strip, and workspaces stack vertically with one empty
  workspace always at the end. Nothing is placed by hand; everything opens into the strip and
  is moved with keys, swipes or a header drag. Pre-release, so the canvas code and its
  protocol went outright (`CanvasView`, `slopty_client::canvas` and `::arrange`,
  `CanvasOp::Place/Raise`, `ClientMsg::Look`, `HostMsg::Presence`), no shim.

- ✅ **Workers, one server, many clients** (2026-09-24). The machines that run shells and
  stream windows are *workers* (what the code called hosts); a later server track will tell
  clients which workers exist. New UI and docs say "worker". The workspace keys a worker by
  `slopty_client::layout::WorkerKey`, an opaque 128-bit value; the app derives it from the
  transport identity in one place (`slopty_app::workers::worker_key`), so naming workers by
  something else is a one-line change.

- ✅ **The layout is this device's; the worker keeps only a registry** (2026-09-24). One
  `WorkspaceView` shows every worker at once (no host switcher): a tile is `(worker, item)`,
  and tiles of several workers sit in the same strips. Where a tile sits, how wide its column
  is and which workspace it is on is private to the device and saved as `layout.json` in the
  client's data directory (debounced 500 ms, atomic rename; a missing or broken file costs the
  arrangement, never the app). The worker holds `ItemStore` (`<data>/items.json`): items lose
  `rect`, `z` and `group`; the ops are `Upsert`, `Remove` and `Sleep` (a rename is an
  `Upsert`); a session's item is made when it opens and removed when it closes; notes stay
  worker items behind the same interface, so they can move to the server later without a
  special case. `ClientMsg::Point` stays (the "go" toast on the others). The wire module is
  `slopty_proto::items` (`ItemSync`, `ItemOp`); `PROTOCOL_VERSION` 51 with re-accepted goldens.

- ✅ **Where a new tile goes** (2026-09-24). An item this client asked for (its own echo:
  `ItemChange::Added { by_me: true }`) opens a column right of the focused one and takes the
  focus. An item from elsewhere (another client, the worker's own session list, the first
  snapshot) joins the end of the workspace that most recently held that worker's tiles, and
  the focus stays put; a worker with no tiles yet fills an empty active workspace, or gets a
  new workspace just above the trailing empty one. A worker that drops keeps its tiles, which
  say it is away and reconnecting; when it is back, a tile whose item is missing from the new
  snapshot leaves, the others stay where they were (`Layout::retain_worker`). A worker whose
  first snapshot of the run is empty is asked for one shell: a new tile goes to the focused
  tile's worker, so without it a newly added worker would have no way in.

- ✅ **The layout is a pure model ported from niri v26.04** (2026-09-24).
  `slopty_client::layout` (with `layout/spring.rs` and `layout/swipe.rs`) has no clock and no
  toolkit: the caller sets the time and the viewport and reads a `Frame` (every tile's rect,
  its resting rect `target`, whether it is near the view, the strip indicator, whether
  anything still moves). Column widths are proportions of the working width with presets
  1/3, 1/2, 2/3 (default 1/2) or fixed points; gaps 8 pt; a viewport under 700 pt is a phone,
  where a new column is full width and each neighbour shows 12 pt. The view offset, the
  snapping, the springs and the swipe tracker are niri's (numbers in the evidence below), with
  these deviations: moves animate FLIP-style from where each tile was drawn; a retargeted
  spring keeps its velocity; a cancelled strip gesture settles where it is and brings the focus
  back into view; one workspace of gesture travel is 1.1 viewport heights; fullscreen is not
  saved. 65 unit tests pin the maths.

- ✅ **The keys** (2026-09-24). ⌘ is the app's modifier; ⌃ and ⌥ without ⌘ belong to the
  terminal. Where the old keys conflicted the table won: ⌘⌥←/→ no longer switch host, ⌘[/⌘]
  no longer walk cards in reading order, ⌘⇧↩ is no longer "rerun last command" (it is in the
  palette), ⌘1 no longer fits all and ⌘-/⌘=/⌘0 size the terminal text instead of zooming. The
  full table, bound in `slopty_ui::workspace::key_bindings` and listed in the palette:

  | Keys | Action |
  |---|---|
  | ⌘⌥← / ⌘⌥→ | focus the column left / right |
  | ⌘⌥↑ / ⌘⌥↓ | focus the tile above / below, else the workspace above / below |
  | ⌘⌥⇧← / ⌘⌥⇧→ | move the column left / right |
  | ⌘⌥⇧↑ / ⌘⌥⇧↓ | move the tile up / down, else to the workspace above / below |
  | ⌘1 … ⌘9 | focus column N |
  | ⌘[ / ⌘] | consume into, or expel from, the column left / right |
  | ⌘R / ⌘⇧R | next / previous preset width |
  | ⌘⌥- / ⌘⌥= | column 10 % narrower / wider |
  | ⌘⇧↩ | maximize the column (full width, again to restore) |
  | ⌃⌘F | fullscreen tile |
  | ⌘⌥C | centre the column |
  | ⌘⌥T | tabbed column |
  | ⌘⌥O (or a pinch in) | overview |
  | ⌘T, ⌘N | new shell |
  | ⌘⇧T | new agent |
  | ⌘⇧N | new note |
  | ⌘O | add a window or display |
  | ⌘W / ⌘Z | close the tile / take it back (5 s) |
  | ⌘⇧P | command palette |
  | ⌘F / ⌘⇧F | find in the tile / in every tile |
  | ⌘E | name the tile |
  | ⌘⇧A | next agent that needs you |
  | ⌘⇧O | point the others at the tile |
  | ⌘⇧M / ⌘⇧I | mute a window / stream stats |
  | ⌘= / ⌘- / ⌘0 | terminal text larger / smaller / default |
  | ⌃Tab / ⌃⇧Tab | the keyboard ring (the way out of a remote window) |
  | ⌘, / ⌘⇧H | settings / add a worker (the app's) |

- ✅ **Gestures** (2026-09-24). Every scroll over the strip is seen in the capture phase
  (`window.on_mouse_event` from the strip's paint) before a tile's own handler. A two-finger
  swipe's axis is decided after 16 pt and kept: horizontal drags the strip and snaps with the
  tracker's fling when the fingers lift; vertical scrolls what is under it (a terminal's
  history, a note), and over the bare strip switches workspace. Until the axis is decided a
  sideways step is held back and an up-or-down one reaches the content, so a vertical scroll
  loses nothing. The momentum macOS sends after the lift (`Moved` after `Ended`; only
  `NSEvent.phase` is read) is swallowed after a strip or workspace swipe, which has already
  snapped. A focused remote window keeps every swipe over its picture (on a phone every remote
  picture does; it moves by its header). ⌘⌥ and the wheel step columns and workspaces (one per
  50 pt, workspaces at most every 150 ms). A header drag moves the tile: into a column, or a
  new column between two, with the edges of the view scrolling the strip; the gap right of a
  column drags its width. A pinch in opens the overview, out closes it; in the overview a click
  on a tile goes there and closes it.

- ✅ **The chrome** (2026-09-24). The titlebar carries only: on the left, the active
  workspace's name (a click opens the overview) and, quietly, any worker that is down; in the
  middle, a mark per column of the strip with the view bracketed and the active column filled
  (a click goes there); on the right, the round trip to the focused tile's worker, the agents
  that need you, "+" (shell, agent, note, window, file) and "…" (palette, overview, stream
  stats, then the app's: settings, add a worker, forget each worker). The top bar's text
  buttons, the zoom percentage, the minimap, arrange headings and other clients' outlines are
  gone. A tile is a hairline border on the theme radius, 8 pt apart; its 28 pt header has a
  status dot, the title, the agent badge and, on hover or focus only, its actions; the focused
  tile wears a 2 pt accent ring drawn over the tile (so the content never moves by a point),
  an agent waiting on the human a warn ring. The add-worker panel is an overlay. Reduce Motion
  lands every animation at once. Theme tokens only (`kit.rs` lint tests).

- ✅ **Only what is near the view costs anything** (2026-09-24). A tile is laid out only when
  its column meets the viewport, is next to one that does, or is focused: an off-screen
  terminal has no element and prepares no rows. A terminal's grid is sized from the tile's
  resting rect and clipped to the moving one, so a width spring resizes no PTY frame by frame.
  A remote window or display off screen for 5 s (`STREAM_GRACE`) lets its stream go and asks
  for it again when it is back in view (a timer wakes the strip for it, since an idle strip
  draws nothing). Frames are requested only while the frame says something still moves.
  Terminal and screen bodies are `Entity::cached` views, so a video frame, a flooding shell or
  the cursor blink repaints only its own tile; the app's once-a-second RTT tick notifies only
  when the shown value changed.

## Evidence: niri v26.04

Read from niri's source (`src/layout/{scrolling,monitor}.rs`, tag v26.04).

- **Model.** A monitor holds a never-empty list of workspaces with one empty workspace always
  at the bottom (adding a tile to the last appends a new empty one; empty unnamed workspaces
  other than the active and the last are cleaned up after a switch). Each workspace is a
  scrolling space: columns, an active column, a view offset (static, animated or under a
  gesture) relative to the active column, the column to go back to when a just-opened one
  closes, and the offset to restore. `column_x(i)` is the sum of the widths and gaps before
  it; the view sits at `column_x(active) + view_offset`. A column's width is a proportion
  (resolved `(working_w − gaps) × p − gaps`) or fixed; full width is proportion 1. Tile heights
  are auto (by weight), fixed or preset; tabbed columns show one tile. niri's defaults: gaps
  16, presets 1/3, 1/2, 2/3, default 1/2.
- **View offset** (`compute_new_view_offset`). A column wider than the view left-aligns; else
  the padding is `clamp((view_w − col_w) / 2, 0, gaps)`, a column already in view keeps the
  view, and otherwise whichever of left- or right-aligning moves the view less wins, measured
  from where a running animation will land; within 1 px nothing moves.
- **Actions.** Focus and move by column, tile and workspace; consume-or-expel; preset widths
  (next preset larger than the current by more than 1 px); ±10 % (fixed converted to a
  proportion); maximize (full width); fullscreen (expelled first from a stacked column);
  centre; tabbed; overview.
- **Animations.** Critically damped springs (damping = ratio × 2√stiffness): workspace switch
  1.0 / 1000 / 0.0001; view movement, window movement, resize and the overview
  1.0 / 800 / 0.0001. Open: 150 ms ease-out-expo, alpha 0 → 1, scale 0.5 → 1 about the centre.
  Close: 150 ms ease-out-quad, alpha 1 → 0, scale 1 → 0.8. Size changes of 10 px or less do
  not animate; spring output is clamped to ten ranges around its start.
- **Gestures.** Axis lock after 16 px. The swipe tracker keeps 150 ms of events; the
  projected end is `pos − v / (1000 × ln 0.997)`. A strip gesture ends on the snap point
  (each column left- or right-aligned) nearest the projected end, focusing the furthest column
  fully visible in the direction of travel, springing with the tracker's velocity. Workspaces
  clamp to one either side with a rubber band `(1 − 1/(x·c/d + 1))·d`, c 0.5, d 0.05. Wheel
  steps accumulate, with a 150 ms cooldown between workspace switches. Drag-and-drop edge
  scrolling: 30 px band, 100 ms delay, up to 1500 px/s.
- **Overview.** Zoom 0.5 on the overview spring; workspaces stacked with a gap of 0.1 × the
  view height; a click on a tile activates it and closes the overview; a drop between
  workspaces makes one.
- **Feel.** A new column opens right of the focused one and becomes active, the view moving
  only as needed; closing a just-opened column returns to the one left of it at its exact
  offset; moving or resizing keeps the camera still while neighbours spring by the delta; a
  retarget starts from the running animation's target.
