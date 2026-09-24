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
  `CanvasOp::Place/Raise`, `ClientMsg::Look`, `WorkerMsg::Presence`), no shim.

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
  saved. Added 2026-09-25: a lone column is centred (niri's `always-center-single-column`, off
  by default there), including when a removal leaves one, and the overview centres a strip
  that fits the zoomed-out window without moving the view (`shown_view_pos`). 67 unit tests
  pin the maths.
  Deviation added 2026-09-25, compact width: below 700 pt (a phone, an iPad in Split View) no
  column is narrower than the viewport. Every column shows full width and the strip scrolls
  between them, as it always did on the phone, because niri's proportions there give a 231 pt
  terminal, about 25 columns, which is unusable. The proportions stay stored, so leaving Split
  View brings them back; a width preset or a resize made while compact changes the stored
  proportion (a resize stores a share of the window, not points, so it survives widening)
  while the column still shows full width. The strip indicator keeps counting every column.
  Rejected: shrinking the terminal text to fit two columns (the text is the one thing that
  must stay readable). `Column::normal_width` against `stored_width`; tests
  `a_compact_window_shows_every_column_full_width_and_keeps_the_proportions` and
  `a_resize_while_compact_changes_the_stored_proportion`.

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
  | ⌘S | save the file tile |
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
  gone. A tile is a hairline border on the theme radius, 8 pt apart; its 28 pt header has the
  title, the agent badge and, on hover or focus only, its actions (a dot before the title only
  while the tile's worker is away); the focused tile's hairline turns accent, drawn over the
  tile (so the content never moves by a point), an agent waiting on the human's warn. The
  add-worker panel is an overlay, except on the first run (ui.md, 2026-09-25). Reduce Motion
  lands every animation at once. Theme tokens only (`kit.rs` lint tests). The column marks
  became dots and the ring a hairline on 2026-09-25 (ui.md, "The de-slop pass").

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

- ✅ **A browser tile is a native page that follows its tile** (2026-09-25). A forwarded
  port is usually a dev server, and the user wanted it beside the shell that runs it rather
  than in another app. `ItemKind::Browser { url }` is a tile like any other (placed, moved,
  closed and taken back the same way), and its page is the platform's `WKWebView`. GPUI draws
  the whole window into one Metal layer, so the page cannot be a GPUI element; it is a native
  subview that always draws on top. Four rules follow from that, all in the pure
  `browser::placement` with its tests:
  - The page goes where the tile's body was drawn in this frame, measured in the body's
    prepaint and applied in a prepaint that runs after every tile's, so it never trails the
    strip by a frame. It is clipped to the strip.
  - Anything GPUI draws over the strip hides it: the palette, the picker, a menu, the
    overview (open or on its way), and the app's dialogs through `set_covered`. So does a
    tile that is off the strip, not drawn, or fading below `MIN_ALPHA`.
  - A toast at the foot of the strip cuts every page's clip short at the toast's top edge,
    measured where the toast was drawn in the same frame, so a toast is never under a page.
    A page entirely below that edge hides. Cutting the whole width, not just the toast's
    box, keeps the clip one rectangle; it costs the bottom few rows of a page for the
    seconds a toast shows.
  - A hidden page leaves its last snapshot in the body, taken when it hides and after each
    load, so covering it shows no hole and the renders the tests diff show the page.

  The page takes the keyboard when it is clicked, and its tile takes the focus. On the Mac, a
  click elsewhere, ⌃Tab (the ring's way out, as for a remote window) or Esc twice gives the
  keyboard back. One Esc still reaches the page, since pages use it. The ways in are the port
  chip, which is now two pills (the port opens its forward in a tile, "↗" still opens the
  default browser), "Open URL…" in the palette (the field starts at `http://localhost:`),
  and any http or https address typed into the palette. An existing tile for the same address
  is focused rather than opened twice. Chrome: the header shows the page's title, then the
  address as quiet text, "←" while there is history, and "↻". No address field and no tabs:
  the palette is the address bar.
  A page with the keyboard gets ⌘C, ⌘X, ⌘V, ⌘A, ⌘Z and ⇧⌘Z: the Mac's key monitor hands them
  to the page before the workspace's key equivalents can take them (ui.md, "The browser
  tile's native view").

- ✅ **A browser tile's address is the worker's** (2026-09-25). The item is shared by every
  client of the worker, while each client serves the worker's ports on its own loopback at
  whatever port it could get (5173 here, 5174 on a machine where 5173 was taken). So the item
  holds the address as the worker sees it (`http://localhost:5173/`), and each client rewrites
  it when the page loads (`browser::local_url`). A host that is the worker's loopback
  (`localhost`, `127.0.0.1`, `[::1]`) has its port served here: the workspace asks the
  worker's link for it (`Remote::forward`, which pins a forward in `tunnel::Forwards` until
  the link goes, whether or not a session still lists the port) and loads the page from the
  local port it gets. The host stays, so the page's origin, cookies and redirects are the
  same on every client, except `[::1]`, which becomes `localhost` because the forward listens
  on IPv4. A link that comes back is asked again, and an open page moves to the new port if
  it changed. The header, the dump and the address the page reports are put back on the
  worker's port (`browser::worker_url`), so every client names the page alike. Any other
  host loads as it is. The port chip and the port list make items with the worker's port.
  Tests: two links on one machine asking for the same worker port get two local ports that
  both reach it (`slopty-client` `remote_link`); two headless workspaces show one item at
  their own ports; the app e2e opens a page whose port the test's own server already holds
  here, so it loads through a forward on another port.

- ✅ **A file tile is an editor** (2026-09-25). The user wanted to fix a line where they read
  it rather than type `$EDITOR` into a shell, so the file tile's body is gpui-kit's code
  editor (ui.md, "The file tile's editor"). With the keyboard in it, the keys are the editor's,
  except ⌘F (the tile's find), ⌘⌥↑/↓ (focus up and down, not extra carets) and ⌘S (save). The
  reading line and its ↑/↓/⇞/⇟ keys, and the "edit" and "reload" pills, went: the caret is
  the reading line, and a change on disk reloads by itself. Opening a file from the palette
  puts the keyboard in the editor. The header's "drag" and "find" text pills went the same
  day, as the de-slop pass removed such pills elsewhere: find is ⌘F and the palette, and the
  file is dragged out by a small page glyph before its title (ui.md).

- ✅ **A terminal's agent badge starts from the worker's list** (2026-09-25). A client that
  connected after an agent started showed a plain shell until the agent's next hook event.
  Every `SessionSummary` now carries the session's agent and status, so on connect (and when
  a session opens) the workspace fills each terminal's agent entry from it
  (`WorkspaceView::seed_agents`). A seed never replaces an entry that an event has already
  made. The summary does not say whether the status came from hooks or from the process
  table, so a seed counts as the process table's until the first event.

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
