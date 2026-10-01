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
  `Upsert`; superseded 2026-09-27 by field ops, multi-client.md "An item change carries only the
  field it changes"); a session's item is made when it opens and removed when it closes; notes stay
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
  by default there), including when a removal leaves one; amended 2026-09-26: it fills the
  width instead (below), and the overview centres a strip
  that fits the zoomed-out window without moving the view (`shown_view_pos`). The layout's
  unit tests pin the maths.
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
  | ⌘⌥Home / ⌘⌥End | focus the first / last column |
  | ⌘⌥⇧Home / ⌘⌥⇧End | move the column to the start / end of the strip |
  | ⌘1 … ⌘9 | focus column N |
  | ⌘⌥⇞ / ⌘⌥⇟ | focus the workspace above / below |
  | ⌘⌥⇧⇞ / ⌘⌥⇧⇟ | move the workspace up / down |
  | ⌃⌘⌥⇞ / ⌃⌘⌥⇟ | carry the column to the workspace above / below |
  | ⌘⌥1 … ⌘⌥9 | focus workspace N (past the last, the trailing empty one) |
  | ⌃⌘⌥1 … ⌃⌘⌥9 | carry the column to workspace N |
  | ⌘⌥` | back to the workspace focused before |
  | ⌘[ / ⌘] | consume into, or expel from, the column left / right |
  | ⌘R / ⌘⇧R | next / previous preset width |
  | ⌘⌥- / ⌘⌥= | column 10 % narrower / wider |
  | ⌘⇧↩ | maximize the column (full width, again to restore) |
  | ⌃⌘F | fullscreen tile |
  | ⌘⌥C | centre the column |
  | ⌘⌥⇧C | centre the fully visible columns as a group |
  | ⌘⌥⇧F | widen the column over the room the visible columns leave |
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
  | ⌘L | the page's address (with no page focused, "Open URL…") |
  | ⌘← / ⌘→ | page back / forward, while the page itself does not hold the keyboard |
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
  Terminal, screen and file bodies are `Entity::cached` views, so a video frame, a flooding
  shell or the cursor blink repaints only its own tile; the app's once-a-second RTT tick
  notifies only when the shown value changed. A file view redraws on every change of its
  editor (it observes it), and a terminal is drawn afresh on the frame its zoom starts or stops
  moving, since that changes its paint at the same bounds. A browser body is not cached: it
  measures where it was drawn on every frame so the native page can follow it, and it costs
  one element. The cache saves little today (MEASUREMENTS 2026-09-25, "the view cache"): a
  terminal whose output did not change redraws from the word cache in well under a
  millisecond, and six streaming shells are all dirty on every frame anyway.

- ✅ **A browser tile is a native page that follows its tile** (2026-09-25). A forwarded
  port is usually a dev server, and the user wanted it beside the shell that runs it rather
  than in another app. `ItemKind::Browser { url }` is a tile like any other (placed, moved,
  closed and taken back the same way), and its page is the platform's `WKWebView`. GPUI draws
  the whole window into one Metal layer, so the page cannot be a GPUI element; it is a native
  subview that always draws on top. Four rules followed from that, all in the pure
  `browser::placement` with its tests. (Superseded 2026-09-30: the window composes the page
  under GPUI's layer now, so the rules below, the covers and the monitor are gone; see ui.md,
  "A browser tile's page is composed by the window, not laid over it".)
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
  the palette is the address bar. (Amended 2026-09-27: the header has an address field now,
  ⌘L or a click on the address; see ui.md, "A page's address is its header's field".)
  A page with the keyboard gets ⌘C, ⌘X, ⌘V, ⌘A, ⌘Z and ⇧⌘Z (ui.md, "A browser tile's page is
  composed by the window, not laid over it", has how).

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

- ✅ **What is done to a worker's tiles while it is away lands when it is back** (2026-09-26).
  A close, a rename or a note written while a worker's link was down used to show here at once
  and never reach the worker: its next snapshot put the tile back, and a shell closed within
  the undo window ran on there unseen. Refusing the action with a notice was the other way,
  and the worse one: a link drops for seconds at a time on a laptop's Wi-Fi, and a workspace
  that will not close a tile or keep a note meanwhile feels remote, which Slopty is not meant
  to. So item ops and a session's close are queued per worker (`Worker::send_or_queue`),
  bounded at 1024, and replayed in order over the worker's first snapshot after the link comes
  back, locally first and then on the wire; a close for a session the worker no longer runs is
  dropped. A save is the exception: whether it landed matters now, so ⌘S with the worker away
  says at once that it cannot reach it, a save whose link drops before the answer says the
  link dropped, and the tile is never left waiting for a reply (which held ⌘S, "Overwrite" and
  "Reload"). The file cards read their files again when the link returns. The focused shell's
  view goes with the link, so the workspace takes the keyboard and ⌘W still answers.

- ✅ **A closed file tile keeps its edit for the undo window** (2026-09-26). A note's text is
  in the registry, so a closed note comes back as it was; a file's edit is only in its editor,
  and ⌘Z brought back the disk's text. The closed tile now holds its editor for the five
  seconds, ⌘Z puts that editor back with its edit and dirty mark, and the file is read again
  so the edit is weighed against the disk as usual. A save answered while the tile is closed
  goes to the held editor. A confirm on close was the alternative; it adds a step to every
  close of a dirty file for a case the undo already covers.

- ✅ **A frame does only the work its drawing needs** (2026-09-26). Every terminal and video
  frame redraws the workspace, and its `render` was walking the registry each time: note
  texts cloned and file lists rebuilt to match views to items, the layout's frame built twice
  (once for the bar's dots before the clock moved, so they lagged a frame), and the agents
  waiting on the human worked out four times. Views are now matched to items only on the frame
  after a registry or a link changed; a note's view takes another client's text itself, now
  or when its editor lets go; the clock moves and the frame is built once for the bar and the
  strip; the waiting agents are counted once per frame. Note bodies are drawn from GPUI's view
  cache, as file cards were. Numbers in MEASUREMENTS.md (2026-09-26, "a workspace frame over
  a large registry").

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
  workspaces makes one. Deviation added 2026-09-25: the zoom is the largest, at most 0.5 and
  at least 0.25, that fits every workspace with a gap around each (`overview_fit`), so the
  trailing empty workspace is never cut off, and a stack that fits stays still while the focus
  moves; a change in the count springs to the new fit.
- **Feel.** A new column opens right of the focused one and becomes active, the view moving
  only as needed; closing a just-opened column returns to the one left of it at its exact
  offset; moving or resizing keeps the camera still while neighbours spring by the delta; a
  retarget starts from the running animation's target.

- ✅ **A lone column fills the width** (2026-09-26). This amends the 2026-09-25 deviation that
  centred a lone column. A column alone in its workspace kept its proportion, half the window
  by default. It then stood in the middle of the canvas with bare canvas on both sides, a pane
  floating where the panes are meant to meet every edge. It now shows at the full working
  width, as a compact window's columns do. Its own width is kept for when a neighbour arrives.
  A preset or a ±10 % step made while it is alone changes that stored width and shows once
  there are two. The second column springs it back to its width. Losing the neighbour fills
  the window again, and the view comes to rest on it.
  - **In the model.** `Geom::lone`, set by `Workspace::geom` from the column count, joins
    `compact` in `normal_width`. Every workspace-level measurement goes through
    `Workspace::geom`, so the frame, the snaps, the drop targets and the overview agree. The
    lone-column branch of `fit_offset` is gone: a column as wide as the view already rests
    flush. `center_column` still centres a column narrower than the view.
  - **Cost.** Opening a second column now resizes the first one's PTY, where a centred half
    column kept its size. That is a resize per first split, not per frame.
  - Tests: client `a_lone_column_fills_the_width_and_keeps_its_own_for_a_neighbour`, and the
    width tests read the stored width (`stored_width_of`) where their column is alone.

- ✅ **The view never rests past the last column** (2026-09-27, UI wave 2). niri lets the view
  rest where the last fit left it, so closing the last column, or widening the window, left
  bare canvas right of the strip (a 150 pt void beside the note golden). `fit_offset` now
  clamps: the view comes back until the last column ends on the working area's right edge,
  or the first starts on its left when every column fits. Closing any column re-fits the view
  on the active one, not only when one column is left.
  Test: client `the_view_never_rests_past_the_last_column`.

- ✅ **A double-click on a column's divider resets its width** (2026-09-27, UI wave 2, the
  study's column sash). The handle round the 1 pt divider is 12 pt, up from 6, so a pointer
  finds it without hunting. A double-click puts the column on its left back at the width a
  column opens at (`LayoutConfig::default_width`, its preset named again) and the focused
  column holds still on screen, as a drag of the same divider keeps it. Remote windows in the
  column are asked to follow, as after a drag.
  Tests: client `a_reset_column_takes_the_opening_width_back`, ui
  `a_double_click_on_a_divider_resets_its_column`.

- ✅ **A drop hint says how much room, not only where** (2026-09-27, UI wave 2). A drop that
  opens a column keeps its 2 pt line on the divider and adds a faint wash as wide as the
  column the tile brings (a moved tile keeps its column's width). A drop into a stacked column
  washes the share the tile would take, the lower half of a column of one; into a tabbed
  column, all of it. Both are `accent_fill`, the accent as a mark.
  Test: ui `the_drop_line_sits_on_the_divider_and_a_join_washes_its_share`.

- ✅ **Every ported niri op has a key and a palette line** (2026-09-28). The 2026-09-28 gap
  audit found layout ops that only tests could reach: focusing a workspace by step, by number
  or back to the previous one, carrying a column to another workspace, moving a workspace,
  moving a column to either end, centring the visible columns and filling the free width. All
  of them are useful here, so none was deleted; each is bound (the key table above) and
  listed in the palette. The keys translate niri's defaults under one rule: ⌘⌥ is niri's Mod
  for the layout, and ⇧ moves the focused thing at the level the key names. So Home and End
  are the strip's ends, as niri's Mod+Home/End, and ⌘⌥⇧Home carries the column there
  (niri's Mod+Ctrl+Home). Page Up and Down are the workspace level, as niri's Mod+Page_Up.
  With ⇧ they move the workspace itself (niri's Mod+Shift), and with ⌃ they carry the column
  (niri's Mod+Ctrl). Digits follow the same split: ⌘N was already column N, so ⌘⌥N is
  workspace N (niri's Mod+N). The column's carry to workspace N takes ⌃ too, because ⇧ on a
  digit reaches the app as its symbol on most layouts. ⌘⌥` goes back to the previous
  workspace, since niri's Mod+Tab would be macOS's app switcher. ⌘⌥⇧C and ⌘⌥⇧F are niri's
  Mod+Ctrl+C and Mod+Ctrl+F, with ⇧ standing for Ctrl as on the arrows. A column carried to a
  workspace past the last lands in the trailing empty one, which then gets a new empty one
  after it. Moving a single tile to another workspace needs no key of its own, since
  ⌘⌥⇧↑/↓ already does it from the top or bottom tile.
  Tests: client `moving_a_column_to_a_numbered_workspace_clamps_to_the_trailing_one`, ui
  `workspace::tests::niri_keys` (a key per family, and a palette line with its key per op).

- ✅ **A file tile edits any file up to 16 MiB** (2026-09-28). The tile turned into a viewer
  past 2 000 lines or 512 KiB, and most source files in a mature repository are past the line
  count, so the one editor Slopty has refused the files a fix is usually in. Now the worker
  reads the whole file up to `FILE_BYTES`, 16 MiB, and the tile edits all of it.
  - **Two ways down, two ways up.** A text of 64 KiB or less (`INLINE_FILE_BYTES`, the
    clipboard's inline limit) still rides the control stream in `FileRead::Text`. A larger one
    is announced there as `FileRead::Streamed { xfer, … }`, and its text follows on a bulk
    stream (`Purpose::FileText`, since 2026-10-01 `Purpose::FileBody`, which also carries a
    picture or a PDF) under the same transfer, at the bulk priority uploads and
    clipboard fetches use. The control stream also carries keystrokes, and a frame of a few
    megabytes on it would hold every key behind it. A save goes up the same way: over 64 KiB,
    the client's link turns `ClientMsg::WriteFile` into a bulk stream (`Purpose::Save { path,
    base_modified_ms }`), and the worker answers it with `WorkerMsg::Written`, as it answers an
    inline save. A save stream that breaks is answered as a failed save, from whichever side
    saw it break.
  - **The join is the link's.** The announcement and its text arrive on different streams,
    in either order. `slopty_client::link::files::Join` holds whichever comes first and hands
    the tile a plain `FileRead::Text` once both are there, so nothing above the link knows a
    read was streamed. The order is the control stream's: a later read of the same path, inline
    or streamed, supersedes one still arriving, and its text is dropped when it lands, so a
    slow stream never puts back an older file. A broken stream reaches the tile as a missing
    file with the reason, so a first read never waits for good.
  - **The watch and the conflict path are unchanged, at any size.** The worker sends a
    watched file again when its stamp moves. A streamed send finishes before the next file is
    looked at, so a file rewritten faster than the link carries it is sent as it stands when
    the last send ends, not once per change. The conflict check is the modification time, as
    before. A file on disk past the cap no longer refuses a save, since a tile only ever holds
    whole files now. One that grows past the cap under an edit reads as `TooLarge`, which the
    tile treats like any change on disk: the edit stays and the bar offers "Reload" and
    "Overwrite".
  - **Why 16 MiB.** It is the codec's frame limit, so the number already bounds every message
    in the protocol, but that is not what sets it: a bulk stream has no frame. What sets it is
    what a tile holds and does with the text. The editor's rope, the base the edit is weighed
    against, the find's case fold and a save's copy make four to five copies, about 80 MB for
    a 16 MiB file, which an iPhone can hold for one tile. On a 20 Mbit/s tailnet path, the text
    takes about 7 s to arrive. VS Code's own large-file mode starts at 20 MB or 300 000 lines
    (`LARGE_FILE_SIZE_THRESHOLD` in `textModel.ts`, read 2026-09-28) and turns off its
    tokenizing there, which is the same trade the colour limit below makes.
  - **Past the cap** the tile says so plainly ("Too large to edit here: 40.0 MB, past
    16.0 MB") and offers "Open in editor" and "Open in pager". Each opens a terminal tile on
    the file's worker, in the file's directory, running the terminal's
    `url::editor_command` (`${EDITOR:-vi} +line 'path'`) or `${PAGER:-less} 'path'` through
    the user's login shell (`file::terminal_command`), so the shell's rc files set `$EDITOR`,
    `$PAGER` and `PATH`. A new tile rather than typing into the last shell, because that shell
    may be on another worker or running something.
  - **Colours stop at 2 MiB** (`file::COLOURED_BYTES`). The highlighter parses the whole text
    off the UI thread once typing pauses, at 80–160 µs a line. That is 3 s for 20 000 lines
    and 30 s for 200 000, and a parse that has started runs to its end, so on a 200 000-line
    file the pauses of ordinary typing kept several cores parsing at once (a profile showed
    five threads in `highlight::spans` for the whole run). A larger file is plain text. An
    incremental parse, from the edited line until the parser's state matches the last parse's,
    would lift the limit.
  - **Find** counts its hits in the whole text at once (`file::hit_lines`: one case fold,
    one pass for the matches, the newlines between them counted), not a copy of every line. A
    reload's diff gets 8 ms before it settles for an approximate answer (`DIFF_WITHIN`), and a
    small change to a large file ends well before that, since the common head and tail are
    trimmed first.
  - **gpui-kit.** The editor has no line limit. Its doc's "about 50K lines" is not enforced,
    and 200 000 lines type and page within a frame. Two costs grew with the file and are fixed
    in the fork (not yet landed). `TextWrapper::set_font` rewrapped every line on any font
    change even with soft wrap off, which cost about 130 ms per frame while the overview zooms
    a 200 000-line tile. `Input` copied the whole rope into its accessibility value on every
    frame while an accessibility client was attached, which cost about 4 ms per frame at
    14 MB. Frame time for 2 000, 20 000 and 200 000 lines, with and without the fork fix, is
    in MEASUREMENTS.md (2026-09-28, "a file tile of any size").
  - Tests: proto goldens `worker_file`, `worker_file_streamed`, `worker_file_too_large`,
    `uni_bulk_file_text`, `uni_bulk_save`. Worker `file` unit tests: 200 000 lines read whole,
    the inclusive cap and `TooLarge` past it, what `announce` streams, and a save past the cap
    refused. Client `link::files` unit tests: the join in either order, a newer read dropping
    a stale text, and a broken stream. Worker e2e
    `a_large_file_streams_down_whole_and_its_save_streams_up`: a 200 000-line file through the
    real link, its watched change, a streamed save landing, a stale one conflicting, and
    `TooLarge`. UI: `a_twenty_thousand_line_file_stays_editable`,
    `a_file_past_the_cap_offers_a_terminal_instead`,
    `a_file_grown_past_the_cap_under_an_edit_keeps_it`, `a_file_past_the_colour_limit_is_plain`
    and `hit_lines_agree_with_find_hits`. App e2e
    `a_twenty_thousand_line_file_is_edited_near_its_end_and_saved`.

- ✅ **A file tile saves a copy onto this device** (2026-09-28). The user wanted to keep a
  worker's file here, not just edit it there. "Save a copy…" is in the palette (`SaveCopy`).
  File tiles have no context menu, and the header gets no button.
  - **It is a download, not the editor's text.** The file comes down through
    `Remote::download`, the path a file dragged out of the app already takes: bulk streams,
    resumed after a cut and checked against the worker's digest. So the copy is the bytes on
    the worker's disk, whatever the tile shows. That covers a text past the inline limit, a
    binary file, and one past `FILE_BYTES`, which the tile will not open. An unsaved edit is
    not in the copy; ⌘S puts it on the worker first.
  - **Mac:** the save panel (GPUI's `prompt_for_new_path`) opens in `~/Downloads` on the
    file's name. The file lands in a hidden directory beside the chosen path and is renamed
    into place, as a kept drag promise is, so a half-arrived file never carries the chosen
    name. **iPhone and iPad:** GPUI has no save panel there, so the file comes into the
    outbox, and `UIDocumentPickerViewController` in export mode (`file_drop::picker::export`,
    the folder tile's "Save to Files…") copies it where the person chooses. The copy here is
    deleted once the sheet is done. A failure is a notice: "name was not saved: why".
  - **A tile with no editor still takes the keyboard.** A file tile gave its editor the
    focus even while the body showed a notice (not text, too large, not readable) and drew no
    editor. The focus then sat on nothing drawn, and a palette action fell to the window's root.
    Those are the files a copy is most wanted for. The tile now tracks a focus handle of its own
    that holds the keyboard while no editor is drawn. The editor takes it over once text
    arrives, and gives it back if the text goes.
  - Test: `workspace::tests::save_copy::a_file_tiles_copy_is_saved_whole_where_the_panel_says`
    (headless workspace; the test platform's save panel is answered with a path, and nothing
    is shown).

- ✅ **The overview draws miniatures** (2026-09-28, design critique round 3 #16). Below zoom
  0.5 the overview laid each shell's, file's and note's own surface over its body and wrote a
  cover on it: glyph, title, one meta line and a few of its last lines at chrome size. The
  overview read as a list of words, and no pane looked like the tile it stood for. niri and
  Mission Control show the windows themselves, scaled. Now nothing covers the body.
  - **What a miniature is.** Each tile is its own body at the overview's zoom. A shell's
    grid, a file's editor, a note's Markdown and a conversation face are laid out at their
    resting size and painted scaled. While the zoom moves, the terminal and the chrome paint
    from the raster ladder. A remote window or display is its last decoded frame, which is
    already a texture. A page is the snapshot `takeSnapshot` leaves when the overview covers
    it. Colours, the cursor, a note's checkboxes and a face's last turn are all there, because
    this is the tile's own drawing.
  - **Nothing is built for it.** An unfocused body is an `Entity::cached` view. The frame
    replays it until its own content changes, so a still miniature costs a replay and new
    output redraws only its tile. The bodies were already drawn under the covers, so dropping
    the covers took the only per-frame cost off. Held open over five floods, a frame's p50
    fell from 4.3 ms to 1.7 ms. The opening spring's worst frame fell from 8.9–9.5 ms (over a
    120 Hz frame) to 5.7–6.2 ms (MEASUREMENTS.md, 2026-09-28, "the overview's miniatures").
    With the overview closed there is no miniature.
  - **The label.** Round 4's facts stay, as one thin line at chrome size across each
    miniature's foot: the state (else the kind) in the glyph slot, the title, then the place
    and the worker where there are several, in the meta size. It sits on the content surface
    under a subtle hairline, its glyph on the edge where the workspace's name starts. It
    fades in with the overview's other words once the zoom has all but landed, is not drawn
    while the overview closes, and under Reduce Motion is drawn at once. It takes no clicks,
    so a click on it goes to the tile.
  - **Rejected: a separate miniature cache.** The brief asked for a raster of each tile or its
    rows drawn small, rebuilt at about 2 Hz while the overview is open. GPUI's glyph
    rasteriser is crate-private, so a CPU raster could only be coloured blocks, not text.
    Lines drawn small would repeat the terminal element's work with less in them: no colour,
    no cursor, no images. Either one would draw on top of the body, which stays drawn to keep
    the keyboard.
  - **Not done: holding a flooding shell to 2 Hz while the overview is open.** It would belong
    in `TerminalView`'s own notify. The held-open numbers above do not need it.
  - Supersedes, in `ui.md`'s "The overview is shapes on lifted cards", the body's surface laid
    over its view and the centred icon and title.
  - Tests: ui `an_open_overview_draws_a_miniature_per_tile_and_none_while_closed`,
    `a_small_overview_draws_tiles_as_miniatures` and
    `overview_labels_say_state_place_and_worker`. Smooth
    `twenty_mixed_tiles_open_hold_and_close_the_overview_on_the_mac`.

- ✅ **The focused tile is replayed too** (2026-09-28, hot-path audit). A tile's body is an
  `Entity::cached` view, but the focused one was always drawn afresh. Any view's notify marks
  the workspace dirty, so a stream, a flooding neighbour or an animation step drew the focused
  shell, face or file again with every frame. GPUI already redraws every view when focus
  moves (`Window::focus` and `blur` refresh the window, and a cached view is never replayed
  while refreshing), so staleness was never the reason.
  - **The rule** (`WorkspaceView::cacheable`). A body is drawn afresh in the frame the focus
    comes to it or leaves it, and a focused body in the frame the keyboard moves at all, even
    within the tile: a replayed body replays its input handler and key listeners, so a shell
    replayed after ⌘E put the keyboard in its header's rename field took the typed name
    (`a_tile_is_named_from_its_header`). Amended 2026-09-30: the keyboard's half of the rule
    is gone, since gpui-fast builds again the views whose focus answers changed (`ui.md`, "The
    workspace reads no focus as a whole"). Otherwise it is replayed, whatever inside it has the
    keys. That is the view itself (a shell's grid, a remote window, a folder, a file's editor
    the file view observes) or a gpui-kit field, and both notify for every key, caret blink,
    selection and input-method change.
  - **A face and a note too, while their fields have the keys** (amended 2026-09-28). The
    first cut drew a body afresh with every frame while a text field inside it had the keys,
    on the belief that gpui-kit's input state is a model a cached view never sees change. It
    is a view: `Textarea` and `Input` render the state entity itself, so its notify dirties
    every view above it in the last frame's dispatch tree, cached ones included
    (`Window::mark_view_dirty`). `ConversationView` (composer, deny field, find field) and
    `NoteView` (its editor) also observe their fields, which says the dependency in their own
    code. A neighbour's echo beside a
    focused face fell from 0.72–1.28 ms to 0.27–0.50 ms p50, and the face rendered in none of
    420 frames against all of them. The new tests fail under the old rule and still pass with
    the observers removed, which is how the premise was found wrong. The same holds for a shell's find bar, so
    the keyboard's place inside a tile no longer matters to the rule
    (`a_focused_shell_whose_find_bar_has_the_keys_is_replayed_until_it_is_typed_in`).
  - **A note reads as a list.** A read note was a column of every segment (prose, task, fence),
    clipped by the tile. Each segment carries a gpui-kit `TextView`, which wraps itself in a
    `track_focus` div, and GPUI puts every tracked focus handle in the frame's tab-stop map,
    tab stop or not. A replayed view replays that map's whole insertion history, so a 64 KiB
    note of task lines (about 3 200 segments) re-inserted about 3 200 nodes into a sum tree on
    every frame another tile caused. `NoteView` now draws its segments in a gpui `list`: a frame
    lays out and paints the rows in view, and a long note scrolls, as the strip already lets a
    vertical swipe scroll what is under it. A changed text re-measures only the rows between
    the first and last that differ, so ticking a box keeps the place. A shell's echo beside two
    such notes fell from 30.7–43.2 ms to 0.49–0.82 ms p50. The note's whole text stays its
    `Document` node's value, so a screen reader reaches every task; the boxes in the tree are
    those drawn, and a task out of view is ticked by scrolling to it or in the editor.
  - **Titles are worked out when they can change.** The twins' numbers and every item's derived
    title are worked out after the workspace's own notify (`observe_self`), a command starting
    or ending, or a window being named. A frame another tile causes changes no title. The pass
    used to read every shell's view each frame, and GPUI wakes a window for any entity its
    last frame read, so an off-screen shell's output cost a frame. A program's new title
    notifies the chrome only when its tile's title follows it. A note's title and task count
    are kept per text, refreshed by `item_changed`.
  - Numbers in MEASUREMENTS.md, 2026-09-28, "the focused tile cached" and "a note drawn as a
    list, a focused face and note replayed". Tests: ui
    `the_focused_shell_is_replayed_while_a_neighbour_draws`,
    `the_focused_face_is_replayed_while_a_neighbour_draws`,
    `the_focused_note_is_replayed_while_a_neighbour_draws`,
    `note::tests::a_long_note_draws_the_rows_in_view_and_scrolls`,
    `a_focused_shell_whose_find_bar_has_the_keys_is_replayed_until_it_is_typed_in`,
    `a_programs_title_draws_the_chrome_only_when_the_tiles_title_follows`,
    `markdown::tests::task_counts_agree_with_the_segments`.

- ✅ **File tiles follow the disk on the kernel's events** (2026-09-30, gap audit #4). The
  worker looked at every watched file's size and time once a second, so an agent's edit reached
  its tile up to a second late. `slopty_worker::fswatch` now follows the files on kqueue(2) on
  macOS and inotify(7) on Linux. The wire is unchanged: `ClientMsg::WatchFiles` names the files,
  and each change comes back as `WorkerMsg::File`, as before.
  - **Not `FSEvents`, alone or through `notify`.** The choice was measured, not assumed.
    fseventsd delivers an event about 11.5 ms after the change, even with the flags
    `notify` 8.2 already sets (latency 0, `NoDefer`, `FileEvents`). A kqueue event arrives in
    about 0.1 ms. An `FSEvents` stream also carries its whole subtree, and a non-recursive
    watch only filters afterwards. A tile of `~/notes.md` would then hear every write under the
    home folder: 19.7 ms of the worker's CPU for 5 000 writes in a sibling folder, against
    0.7 ms. `notify`'s callback also panics, inside an `extern "C"` function (so the worker
    aborts), on a path that is not UTF-8 or on an event flag its bitflags do not know. On Linux
    it would use the same inotify with a thread of its own. Both backends go through `rustix`,
    which the worker already had. There is no new runtime dependency: `notify` is a macOS
    dev-dependency, kept only for the comparison test.
  - **What is watched.** For each path the worker watches the deepest directory of it that
    exists, so a `build/` deleted and made again is followed back down. It also watches the
    directory a symlink ends in. On macOS it watches the file itself too (`O_EVTONLY`, which
    does not hold the volume mounted), since there a write to a file does not touch its
    directory. A directory's watch sees what a watch on the file's inode loses: an editor's
    temporary file renamed over the path, a delete, and a file made again. A client has one
    kqueue or inotify descriptor, and tokio's reactor wakes on it (`AsyncFd`). No thread
    waits, and an idle watch costs nothing, where the poll made a stat a second per file.
  - **One save, one send.** An event only says "look". A file is looked at 2 ms after its
    events stop (`QUIET`), or after 50 ms of steady writes (`HOLD`). It is sent only when its
    stamp moved. The stamp is device, inode, size, modification time and change time, so a copy
    that keeps the old time and size is still a change. A file truncated to nothing waits up
    to `HOLD` for the writes that usually follow, so a slow writer's empty moment is never
    sent. On Linux, `IN_CLOSE_WRITE` and `IN_MOVED_TO` say the change is whole, and the file is
    looked at at once. Two sends of one file are at least `HOLD` apart, so a log written a line
    a millisecond is sent at most 20 times a second. As before, one file is sent at a time, and
    a file that changes during a send is sent once more when it ends.
  - **Where events cannot reach, the poll stays.** Some volumes cannot hear another machine's
    writes: those not `MNT_LOCAL` or on FUSE on macOS, and NFS, SMB, FUSE, 9P, Ceph, AFS,
    virtiofs and vboxsf by their `statfs` magic on Linux. A file on one is watched and also
    looked at every second. A file past a client's 64 watches is polled every second, the old
    behaviour, and so is a directory the worker may not open. On macOS every watch is a
    descriptor, and a launchd daemon starts at a soft limit of 256 for all of them. When
    kqueue or inotify itself fails (inotify's `max_user_instances`, or `ENOSPC` past
    `max_user_watches` for one directory), the paths it would have covered are polled.
  - **Not done yet.** Folder tiles still refresh only on focus (done 2026-10-01: "Folder
    tiles follow their directory" below). Following them needs a wire
    message, which is the other half of gap #4. A dangling symlink's target that appears later
    is seen only on the next change in the link's own directory.
  - Numbers in MEASUREMENTS.md, 2026-09-30, "file tiles on kernel events". Tests: worker
    `fswatch` unit tests (the volume classifier, the anchor of a missing path and of a
    symlink, when a touch is due) and `tests/fswatch.rs` on the real file system:
    `every_kind_of_save_reaches_the_follower_once`, `a_burst_of_writes_is_one_report`,
    `a_truncate_waits_for_the_writes_after_it`,
    `a_directory_deleted_and_made_again_is_followed`, `a_symlink_follows_its_target`,
    `past_the_watch_limit_a_file_is_polled`, `an_unwatchable_directory_is_polled` (macOS),
    `the_follower_ends_with_its_list`, and the measurement
    `edit_to_report_against_fsevents_and_bare_kqueue`.

- ✅ **A file tile shows a picture or a PDF** (2026-10-01). Opening a screenshot, a photo or a
  PDF an agent wrote gave one line, "Binary file", so the tile could not stand in for Quick
  Look on the remote Mac. Now the worker sniffs a file's first 32 bytes
  (`slopty_worker::file::media`: PNG, JPEG, GIF, WebP, HEIC/HEIF/AVIF by their `ftyp` brand,
  TIFF, JPEG XL, BMP, ICO, PSD and PDF) and sends such a file as its bytes,
  `FileRead::Media { media_type, bytes, modified_ms }`, up to 128 MiB (`MEDIA_BYTES`; past it,
  still "Binary"). A text past 16 MiB stays `TooLarge`, since a scanned PDF or a camera's raw
  file is larger than any text worth editing.
  - **One stream for either body.** Past 64 KiB the bytes follow on the bulk stream the large
    text already used, renamed `Purpose::FileBody`, and the announcement says what they are
    (`FileRead::Streamed { body: Body::Text { final_newline } | Body::Media { media_type } }`).
    The link's join (`slopty_client::link::files::Join`) hands the tile a `Text` or a `Media`
    once both halves are there, so the tile never sees a stream. A picture announced as text
    that is not UTF-8 arrives as a missing file with the reason, as before.
  - **The platform decodes, at the size drawn.** `slopty-ui::file::decode` asks ImageIO for a
    thumbnail of the longest side the tile covers in device pixels
    (`CGImageSourceCreateThumbnailAtIndex`, with the EXIF orientation applied), so a 12 MP
    photo in a 600-point tile decodes 1 200 pixels, not 12 million. The bytes are handed to
    ImageIO without a copy (a `CGDataProvider` whose release callback owns the `Bytes`). A
    picture is fitted inside the tile at its own resolution at most: a Retina screenshot
    (144 dpi) shows at its real size in points, not doubled. It decodes again only when the
    tile grows past the decode or shrinks to less than half of it. An animated GIF, APNG,
    WebP or HEIC sequence keeps its frames and delays (up to 256 MiB of frames), and GPUI's
    `img` plays them unless Reduce Motion is on. The pixels are drawn into premultiplied BGRA
    and unpremultiplied, since GPUI's `RenderImage` takes straight alpha; an opaque picture
    skips the pass, which was 12 ms of a 12 MP decode.
  - **A PDF is pages, drawn lazily.** CoreGraphics opens it (`CGPDFDocument`, an empty password
    tried on an encrypted one) and each page is a row of a virtualized `list`, its height from
    its crop box and rotation, so scrolling a 500-page manual lays out 500 rows and draws the
    few in view. A page is drawn at the tile's width in device pixels on a background thread,
    one at a time in order, on white, and the twelve seen last stay drawn. Text is not
    selectable, and the keyboard does not scroll the pages yet.
  - **Why not the `image` crate or pdfium.** ImageIO and CoreGraphics are on every Mac and
    iPhone, decode HEIC and PDF (which no pure-Rust decoder does well), use the hardware JPEG
    and HEIC decoders, and add no dependency but `objc2-image-io`. A static library's `#[link]`
    records nothing in its objects, so the iOS app's link line is now rustc's own
    `native-static-libs` list (`xtask ios`) rather than a framework list kept by hand, which is
    what left ImageIO out of the first simulator build.
  - Numbers in MEASUREMENTS.md, 2026-10-01, "file tile pictures and PDFs". Tests: worker
    `each_signature_names_its_type_and_text_names_none`,
    `a_picture_or_pdf_is_read_as_media_up_to_its_own_cap`, e2e
    `a_picture_and_a_pdf_come_down_as_media`; client
    `a_streamed_picture_is_media_and_a_streamed_text_must_be_text`; ui `file::decode`
    (`a_picture_decodes_to_the_size_asked_with_its_colours`,
    `an_animation_keeps_its_frames_and_delays`, `a_pdf_page_is_drawn_at_the_width_asked`,
    `nothing_and_garbage_are_unreadable`,
    `unpremultiplying_restores_straight_colour_and_leaves_opaque_and_clear_alone`),
    `file::preview` (fitting and when a decode serves), `file::tests`
    (`a_picture_is_decoded_at_the_size_it_is_drawn`,
    `a_pdf_shows_its_pages_drawn_at_the_tiles_width`,
    `a_picture_the_platform_cannot_read_says_so`); xtask
    `the_link_line_is_the_frameworks_rustc_lists_and_other_diagnostics_are_shown`; app goldens
    `file-picture` and `file-pdf`.

- ✅ **Folder tiles follow their directory** (2026-10-01, the rest of gap audit #4). A folder
  tile listed its directory when it opened and again only on focus, so a file an agent wrote
  did not show until the tile was clicked. The client now sends each linked worker the set of
  directories its folder tiles show (`ClientMsg::WatchFolders { paths }`, the whole set each
  time it changes, from `workspace::folders::reconcile_folders`), and the worker lists one
  again, as `WorkerMsg::Folder`, when an entry in it is made, removed or renamed.
  - **The same follower as file tiles.** `fswatch::follow_folders` is `fswatch`'s kqueue or
    inotify follower with a folder's own directory watched, and any event in it, not only one
    naming a file, counting as a change. The parent is watched too, so a folder deleted and
    made again is followed. A write inside a file of the folder changes no entry and sends
    nothing, though on macOS it also leaves a size in the listing stale until the next
    change. The `QUIET` and `HOLD` pacing is the file tiles', so an `npm install` into a
    folder shown is a listing every 50 ms at most.
  - **The tile keeps its place.** A new listing of the same directory keeps the selection on
    the same name, or on the same row when that entry went, and does not scroll
    (`FolderView::set_listing`).
  - Numbers in MEASUREMENTS.md, 2026-10-01, "folder tiles on kernel events". Tests: worker
    `tests/fswatch.rs` `each_change_of_a_folders_entries_is_one_report`,
    `a_write_inside_a_file_of_the_folder_is_no_report`,
    `a_folder_deleted_and_made_again_is_followed` and the measurement
    `folder_change_to_report`; e2e `a_followed_folder_is_listed_again_when_its_entries_change`;
    ui `a_folder_tile_follows_its_directory_and_keeps_its_place`; proto golden
    `client_watch_folders`; app scenario
    `a_folder_tile_browses_the_worker_and_opens_a_file_beside_it`, which now writes a file
    and waits for its row.

- ✅ **Quick open answers from an index of the worktree** (2026-10-01). Each keystroke of the
  palette's file search walked the directory again, up to 20 000 entries and 8 levels, and
  kept a path only when it held the query as a substring. In a large repository that was a
  walk per key that still missed every file past the first 20 000, and `vwpane` found nothing
  where `view/pane.rs` was meant.
  - **One index per worktree, shared.** A query under a directory inside a git worktree
    (the nearest directory up holding `.git`) is answered from `find::Index`: every path of
    the worktree, sorted, from one parallel walk (`ignore`'s, honouring `.gitignore`,
    `.ignore` and the repository's excludes; hidden entries skipped), up to 2 million paths.
    The worker keeps the eight worktrees asked about last, dropping one idle for 15 minutes,
    in one static every client's query shares. A query under a subfolder is the run of
    sorted paths under it. Outside a worktree (a home directory) the bounded walk stays.
  - **Kept fresh by events, caught up when asked.** On macOS one `FSEvents` stream covers
    the worktree however deep, which is what `FSEvents` is for; the file tiles chose kqueue
    for its 0.1 ms against 11 ms, but a kqueue watch is a descriptor a directory. The stream
    only notes which directories changed; the next query lists those again, and a new
    directory is walked whole. Its 11 ms never shows, since a person does not type that fast
    after a save. A changed `.gitignore`, or events the system dropped, walk the tree again;
    a build churning an ignored `target/` costs nothing, since a directory the index does not
    hold is not noted. Elsewhere the index looks at its directories' modification times, at
    most every 500 ms, before a query.
  - **Fuzzy, fzf's scoring.** `nucleo-matcher` (Helix's port of fzf's algorithm) in its path
    mode, smart case: each word typed must match, its letters in order, with bonuses at the
    start of a name or a word in it; ties go to the shorter path. A list past 32 768 paths is
    scored across the cores (`std::thread::scope`, one matcher a thread). A query typed on
    from the last one (same directory, the tree unchanged, no `!^$'\` typed) scores only the
    last one's matches, so each keystroke costs less than the one before.
  - Numbers in MEASUREMENTS.md, 2026-10-01, "quick open's worktree index". Tests: worker
    `find` (`the_best_match_leads_and_a_name_beats_a_scatter`,
    `typing_on_narrows_and_a_special_character_or_a_deletion_does_not`,
    `split_across_threads_the_ranking_is_the_one_thread_ranking`,
    `outside_a_worktree_a_bounded_walk_answers_and_ignored_paths_are_skipped`) and
    `find::index` (`a_worktree_is_walked_once_and_asked_from_any_folder_in_it`,
    `the_index_follows_the_tree`, `typing_on_scores_the_last_matches_and_a_change_starts_again`,
    `without_events_the_times_of_the_folders_say_what_changed`,
    `the_worker_keeps_one_index_a_worktree_and_lets_the_oldest_go`) and the measurement
    `quick_open_costs`.
