# Decisions — Workspace

See `docs/DECISIONS.md` for the legend. Newest entries go at the end. This file supersedes the
camera, placement and navigation entries of `canvas.md` (the infinite plane, ⌘1/⌘2/⌘0 and
arrange, flights, the minimap, the reading-order walk, presence); the entries there about
notes, file cards, the palette, naming and agents still hold, read with "tile" for "card".

- ✅ **A scrollable tiling workspace replaces the infinite canvas** (2026-09-24; the strip is
  superseded 2026-10-06 by "Tiling replaces the scrolling strip" below). The canvas
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

- ✅ **Where a new tile goes** (2026-09-24; amended 2026-10-03: a tile from elsewhere goes by
  its project, not its worker, see "A tile from elsewhere goes to its project's workspace"
  below). An item this client asked for (its own echo:
  `ItemChange::Added { by_me: true }`) opens a column right of the focused one and takes the
  focus. An item from elsewhere (another client, the worker's own session list, the first
  snapshot) joins the end of the workspace that most recently held that worker's tiles, and
  the focus stays put; a worker with no tiles yet fills an empty active workspace, or gets a
  new workspace just above the trailing empty one. A worker that drops keeps its tiles, which
  say it is away and reconnecting; when it is back, a tile whose item is missing from the new
  snapshot leaves, the others stay where they were (`Layout::retain_worker`). A worker whose
  first snapshot of the run is empty is asked for one shell: a new tile goes to the focused
  tile's worker, so without it a newly added worker would have no way in.

- ✅ **The layout is a pure model ported from niri v26.04** (2026-09-24; superseded 2026-10-06 by
  "Tiling replaces the scrolling strip" below, and the port deleted in step 3 of its study).
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
  | ⌘⌥⇧Home / ⌘⌥⇧End | move the column to the start / end of the strip |
  | ⌘1 … ⌘9 | focus column N |
  | ⌘⌥⇞ / ⌘⌥⇟ | focus the workspace above / below |
  | ⌘⌥1 … ⌘⌥9 | focus workspace N (past the last, the trailing empty one) |
  | ⌘[ / ⌘] | consume into, or expel from, the column left / right |
  | ⌘R | next preset width |
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
  | ⌘F / ⌘⇧F | find in the tile / search the files, or with the scope chip the open tiles |
  | ⌘E | name the tile |
  | ⌘L | the page's address (with no page focused, "Open URL…") |
  | ⌘← / ⌘→ | page back / forward, while the page itself does not hold the keyboard |
  | ⌘S | save the file tile |
  | ⌘⇧A | next agent that needs you |
  | ⌘⇧U | the navigator at what needs you (the bell) |
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
  host loads as it is. The port list makes items with the worker's port.
  Tests: two links on one machine asking for the same worker port get two local ports that
  both reach it (`slopty-client` `remote_link`); two headless workspaces show one item at
  their own ports; the app e2e opens a page whose port the test's own server already holds
  here, so it loads through a forward on another port.
  (Amended 2026-10-01: any other host goes through the worker's proxy now; see "A browser
  tile reaches every host the worker reaches", next.)

- ✅ **A browser tile reaches every host the worker reaches** (2026-10-01, gap audit #10,
  MEASUREMENTS.md "a page through the worker's proxy"). A tunnel named only a port on the
  worker's loopback, so a page on a container, a LAN machine or a name only the worker's
  resolver knows did not load, and a loopback page's calls to such hosts failed with it. A
  tunnel now names its host (`TunnelOpen { host: TunnelHost, port }`: `Loopback`, a `Name`
  for the worker's resolver, `/etc/hosts` and DNS included, or an `Ip`). The worker dials it,
  each address the resolver gives for a name in turn, 5 s each, and a target it cannot reach
  resets the stream with a `TunnelRefusal` code (refused, unresolved, unreachable). It keeps a
  name's addresses for each client for a minute, the one that answered last first, since a
  lookup cost a loaded worker 1.2 ms and a page opens many connections at once; an address
  that stops answering is looked up again at once.
  - *The client serves the worker's network as a SOCKS5 proxy* (`tunnel::Proxy`, RFC 1928,
    CONNECT without authentication) on its loopback, one per link, started the first time a
    page asks (`Remote::proxy`). Each worker's pages keep a `WKWebsiteDataStore` of their own,
    named by the worker's key (`dataStoreForIdentifier:`), which also keeps two workers'
    `localhost:3000` cookies apart and lasts between launches. The workspace sets the store's
    `proxyConfigurations` to the link's proxy (`slopty_platform::web::route`) before any
    page of that link loads. WebKit hands back one store per identifier and applies a new
    configuration to the pages already open, so a new link reroutes them where they are.
    `proxyConfigurations` is macOS 14 and iOS 17, under the 26.5 floor. objc2-web-kit leaves it
    out (the header refines it for Swift), and objc2 binds no Network.framework, so the two
    `nw_*` constructors are declared in `web.rs`. HTTP CONNECT was the other choice; WebKit
    treats both alike, and SOCKS5 carries the name the page asked for with no parsing of HTTP.
  - *The loopback keeps its forwards and their port rewrite.* The research plan had the
    rewrite go. Measured on this Mac (macOS 26), WebKit never sends `localhost`, `localhost.`,
    `127.0.0.1` or `[::1]` to a proxy, whatever the configuration: clearing the excluded
    domains changes nothing, and matching `localhost` alone sends nothing through. Names under
    `.localhost`, `.local` and every other host do go, by name (address type 3), and an
    address by address. So `worker_port` still picks the loopback pages out, and every other
    address loads as it is.
  - *A CONNECT is answered at once*, before the worker dials. The page's request then leaves
    right behind the tunnel's header, so a new connection costs the round trip a forward's
    does, not two. The price is the error: a target the worker cannot reach resets a socket
    WebKit thought connected, and WebKit says only that the connection was lost. So the proxy
    keeps the worker's reason per host and port, written before the page's socket is reset
    and cleared at the target's first byte, and a page that fails on a proxied host asks for
    it (`BrowserEvent::Failed`, `Remote::refusal`): the tile then says "the worker finds no
    host named db", "nothing listens on port 8080 of db" or "the worker can't reach db".
  - *Nothing filters the hosts.* A client role can already run a shell on the worker, so
    reaching the worker's network through it grants nothing new; the tailnet's grants decide
    who is a client. The worker logs each tunnel's host and port at debug.
  - Tests: the wire's goldens (`tunnel_open`, `tunnel_open_name`, `tunnel_open_ip`) and the
    refusal codes (`slopty-proto` `units`); a named host, an address, and each refusal against
    the real worker (`a_tunnel_reaches_a_named_host_and_says_why_it_cannot`), and its kept
    lookups (`a_name_is_kept_answered_first_until_it_ages`); the proxy's
    address types, its refusal kept before the reset, and what it does not serve
    (`slopty-client` `tunnel::proxy`); the link's proxy port and its refusal
    (`remote_link`); the workspace loading a page on another host as it is through the proxy
    and saying why one failed (`a_page_on_any_other_host_loads_through_the_workers_proxy`);
    and the app e2e, where a page on `only-the-worker.localhost` first shows the worker's own
    refusal of a port nothing listens on, which proves the path, then loads through it as it
    is, and `nowhere.invalid` says the worker finds no such host
    (`a_page_on_a_host_only_the_worker_names_loads_through_its_proxy`).
  - Forgetting a worker deletes its pages' store, cookies and storage with it
    (`slopty_platform::web::forget`, 2026-10-01). Its pages go with its tiles at once: they
    used to wait for the workspace's next draw, which a hidden window never makes, so a
    forgotten worker's pages kept running and WebKit refused the removal as "in use".
    - Since 2026-10-05 nothing holds a page past its owner:
      - A dropped page closes at once (`-[WKWebView _close]`, private; Slopty ships only to
        internal TestFlight, and a test checks the selector still exists).
      - gpui-fast's frame keeps the views it may rebuild weakly (fork PR #29). Before, the last
        frame held a dropped tile, and with it the page's view, until the window next drew.
    - WebKit still refuses until the process that showed the page has answered its close, a
      few run-loop turns later (about 15 ms measured; seconds on a loaded machine). The
      removal is asked once, and a refusal is asked again each time the main run loop is
      about to wait, which is how that answer arrives. It gives up after 30 s; there is no
      ladder of timed retries.
    - Tests:
      - `a_forgotten_workers_store_goes_once_its_last_page_has` (slopty-platform
        `main_thread`): one ask right after the drop; 20 of 20 passed under twice as many CPU
        hogs as cores.
      - `a_removed_worker_with_open_tiles_leaves_nothing`: the page is gone before any draw.
      - The app e2e above, which forgets the worker and sees its store go from
        `~/Library/WebKit`.

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
  (Narrowed 2026-10-04: the rare ones are gone, see "The rare niri ops are gone".)

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
    name. **iPhone and iPad:** GPUI has no save panel there, so the Files picker asks for a
    folder first and the file comes down straight into it, listed with its progress and its
    stop (amended 2026-10-10; platform.md, "Save to Files comes down into the folder chosen").
    A failure is a notice: "name was not saved: why".
  - **A tile with no editor still takes the keyboard.** A file tile gave its editor the
    focus even while the body showed a notice (not text, too large, not readable) and drew no
    editor. The focus then sat on nothing drawn, and a palette action fell to the window's root.
    Those are the files a copy is most wanted for. The tile now tracks a focus handle of its own
    that holds the keyboard while no editor is drawn. The editor takes it over once text
    arrives, and gives it back if the text goes.
  - Test: `workspace::tests::save_copy::a_file_tiles_copy_is_saved_whole_where_the_panel_says`
    (headless workspace; the test platform's save panel is answered with a path, and nothing
    is shown).

- ✅ **The overview draws miniatures** (2026-09-28, design critique round 3 #16; for tiles of
  words, superseded 2026-10-06 by "The overview is a map of work" below). Below zoom
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
    skips the pass, which was 12 ms of a 12 MP decode. A HEIC asked for at less than two
    fifths of its longest side is decoded at two fifths and drawn down to the size: ImageIO
    decodes the whole HEIC and then scales it, and that scaling costs more the smaller the
    thumbnail, so a 400-pixel thumbnail of a 12 MP photo cost more than a 1 600-pixel one.
    Two steps take 14 to 32 % less CPU (`a_heic_is_shrunk_in_two_steps_to_the_size_asked`).
  - **A PDF is pages, drawn lazily.** CoreGraphics opens it (`CGPDFDocument`, an empty password
    tried on an encrypted one) and each page is a row of a virtualized `list`, its height from
    its crop box and rotation, so scrolling a 500-page manual lays out 500 rows and draws the
    few in view. A page is drawn at the tile's width in device pixels on a background thread,
    one at a time in order, on white, and the twelve seen last stay drawn. The keys and text
    selection came after ("A PDF's pages take the keys and select text").
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
    made again is followed. The `QUIET` and `HOLD` pacing is the file tiles', so an
    `npm install` into a folder shown is a listing every 50 ms at most.
  - **A write inside a file updates its size.** A write changes no entry, but it changes the
    size the listing shows. inotify's watch on the folder hears it (`IN_MODIFY`,
    `IN_CLOSE_WRITE`, `IN_ATTRIB`). A kqueue watch on a directory does not, so on macOS one
    `FSEvents` stream with file events covers the followed folders
    (`kqueue::Queue::follow_contents`, started again when the set changes). It rings the
    kqueue through an `EVFILT_USER` event, so the follower still waits on one descriptor. A
    write to a direct child counts, and so does an entry made in a subfolder, whose count
    the listing shows. Such a folder is listed again after `CONTENT_HOLD` (250 ms) and at
    that rate at most, though its own stamp did not move: a growing log is a listing four
    times a second, not one every 50 ms. `FSEvents`' 11 ms does not matter at that pace.
  - **The tile keeps its place.** A new listing of the same directory keeps the selection on
    the same name, or on the same row when that entry went, and does not scroll
    (`FolderView::set_listing`).
  - Numbers in MEASUREMENTS.md, 2026-10-01, "folder tiles on kernel events". Tests: worker
    `tests/fswatch.rs` `each_change_of_a_folders_entries_is_one_report`,
    `a_write_inside_a_file_of_the_folder_is_reported_for_its_size`,
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
    hold is not noted. On Linux inotify keeps it fresh ("Quick open's index on Linux follows
    inotify"). Elsewhere the index looks at its directories' modification times, at most
    every 500 ms, before a query.
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

- ✅ **Quick open's index on Linux follows inotify** (2026-10-01). A Linux worker's index
  looked at every directory's time before a query, which is 4 000 `stat`s a keystroke on a
  200 000-path tree. Now inotify(7) watches each directory the index holds, one watch a
  directory, read by a thread of the watch's own (`find::watch`, Linux).
  - **Why not `notify`.** Its recursive mode walks the top and watches every directory under
    it, ignored or not, so a `target/` or `node_modules/` would spend thousands of the
    user's watches on paths quick open never shows. The index already walks the tree with the
    ignore rules, so it watches what it walked and the directories made later.
  - **Nothing missed between the walk and the watches.** The watches are added after the
    walk, so each directory's time is then compared with the walk's, and one that moved in
    between is listed again. A queue overflow (`IN_Q_OVERFLOW`) walks the tree again.
  - **Out of watches, said and still right.** Past `fs.inotify.max_user_watches` (`ENOSPC`)
    the index lets the watch go whole, which frees its watches for the file tiles, and looks
    at its directories' times instead, slower but never stale. The person is told once, in
    the palette's answer: `WorkerMsg::FoundFiles` carries a `notice`, which the app shows.
    The same notice says when a tree past 2 million paths is held in part.
  - **The watch starts beside the walk.** Starting an `FSEvents` stream took 880 ms on a
    freshly made 200 000-path tree while `fseventsd` was busy, and the first answer waited
    for it. Now the watch starts on a thread of its own while the walk runs. Until it is up
    the index looks at directory times, and once it is, every directory's time is compared
    with the walk's. This holds on macOS and Linux alike.
  - **A change is merged, not sorted.** Catching up a file made pushed it on the 204 160
    sorted paths and sorted them all again, which was most of the 17 ms from a file made to
    its answer. New paths are now merged into the sorted list, a binary search each: 2.4 ms.
  - Numbers in MEASUREMENTS.md, 2026-10-01, "quick open's index on Linux, and the watch
    beside the walk". Tests: worker `find::index`
    (`past_its_watches_the_index_looks_at_times_and_stays_right`, and the ignored
    `past_the_kernels_watches_the_index_looks_at_times_and_stays_right`, run in a user
    namespace with the limit lowered, both Linux; `the_index_follows_the_tree`, which counts
    four watches on Linux; `new_paths_merge_into_the_sorted_ones`) and the proto golden
    `worker_found_files_notice`. The Linux tests run in a Debian container on binaries
    cross-built with `cargo zigbuild` (MEASUREMENTS.md has the commands).

- ✅ **A PDF's pages scroll by key; their text is not selected here** (2026-10-01, cut back
  2026-10-05 by prune #12). A PDF tile scrolled only with the pointer.
  - **Preview's scroll keys.** ↓ and ↑ scroll a few lines, Space and Page Down a screen less a
    line (⇧Space and Page Up back). They bind in the tile's key context while it shows a PDF
    (`FileEditor FilePages`). A press on the pages gives the tile the keyboard.
  - **Cut on 2026-10-05:** paging by → and ←, Home and End, and selecting and copying the text
    through `PDFKit` (`file::pdf_text`, with ⌘C and ⌘A), with their palette lines. A PDF here
    is read beside the work, and an agent reads the file itself; no frontier tool selects PDF
    text in its editor, and the audit had already said to stop investing in it. Deleted, not
    hidden, with the `objc2-pdf-kit` dependency.
  - **Bare keys where nothing takes typing.** The settings form refused a bare key outside
    the folder and project scopes, so it refused the arrows the PDF commands ship with. The
    rule is now per command: a key alone is allowed when the command's scope allows it, or
    when every context it binds in holds no text field (`TEXTLESS`: a PDF's pages, a folder
    tile, a project board). Moving the PDF commands into a scope of their own would have
    split one tile's commands across two settings tables, and would have left the next
    textless view inside a typing scope with the same problem.
  - Tests: ui `a_pdfs_keys_scroll_it`, keymap `a_bare_key_binds_where_nothing_takes_typing`.
    The e2e golden `file-pdf` is retaken.

- ✅ **A tile from elsewhere goes to its project's workspace** (2026-10-03, the organisation
  study `.research/organization-2026-10-04.md` §6.3). A workspace is most useful when it holds
  one body of work, whatever machines run it, so `Placement::Remote` carries the arriving
  tile's project (`slopty_client::groups::GroupKey`, worked out by the client over every tile of
  the layout and the arrivals together) and the layout puts it at the end of the workspace
  that most recently held a tile of that project. A tile with no project (a window, a display,
  a note, a page, a shell in its home directory: its project is its machine) goes to the
  workspace that most recently held any tile of its machine, so it lands where that machine's
  work is rather than in a strip of its own. Failing both, it fills an empty active workspace,
  else gets a new workspace just above the trailing empty one, as before. Each tile keeps the
  project it was last said to be in (`Tile::home`, said again before each placement by
  `Layout::set_homes`); a tile already placed never moves, so a shell that changes checkout
  changes its row in the navigator and not its place. The rule is not "one workspace per
  project": the person's own moves mix them freely, and a tile of this client's own still
  opens beside the focus. Tests: `layout::tests::a_remote_tile_joins_the_workspace_of_its_project`,
  `a_project_with_no_workspace_fills_an_empty_one_or_gets_one_above_the_trailing`,
  `a_placed_tile_never_moves_when_its_project_changes`,
  `a_tile_with_no_project_joins_its_machines_work`; the UI's view of it is in
  `docs/decisions/ui.md`, "The navigator groups by project; the machine is a facet".

- ✅ **A folder made again is reported at once, whatever its stream costs to start**
  (2026-10-03). On this Mac (macOS 27.0.1, 26A434) `a_folder_deleted_and_made_again_is_followed`
  failed: the folder came back, but its report did not within 3 s. kqueue was not the cause. A
  probe of a directory deleted and made again saw what macOS 26 gives: `NOTE_DELETE | NOTE_LINK`
  on the old descriptor and `NOTE_WRITE | NOTE_LINK` on the parent's, once for each step. The
  time went to `FSEventStreamStart`. The follower starts the folders' contents stream again
  whenever the set of folders changes, and it did so inside the look that sends the report. On
  macOS 27 a start took 0.3 to 2.7 s, and no flag, path, volume or `sinceWhen` changed that. A
  macOS 26.6.2 CI runner ran the whole test, two starts included, in 134 ms. The FSEvents.h
  of the 26.5 and 27.0 SDKs is the same, so nothing documents the change. While one start is
  in flight, every other `FSEvents` call of the process waits for it, even
  `FSEventsGetCurrentEventId` (0.27 to 0.6 s, against 0.08 ms alone). `FSEventStreamStop`
  does not wait.
  - **The stream starts off the follower** (`fsevents::Starting`). It starts on a global
    dispatch queue, which rings the kqueue once it is up, so no kqueue report waits for it on
    either OS. A folder made again is now reported in 52 ms, which is `HOLD` after its
    "gone" report. Before, it took 0.6 to 0.9 s when the report came at all.
  - **No gap while it starts.** The stream over the folders before stays until the new one is
    up, so a kept folder is heard the whole time. Once the new stream is up, each folder new to
    it is listed again, for whatever changed inside it meanwhile. Replaying from an event id
    taken when the stream was asked for (`sinceWhen`) was tried first and dropped: taking the
    id waits for any start in flight, and a volume that keeps no `FSEvents` history (FAT)
    replays nothing.
  - Linux is untouched: inotify's watch on the folder hears the writes inside its files, and
    there is no stream to start.
  - Numbers in MEASUREMENTS.md, 2026-10-03, "a folder tile's contents stream start". Tests:
    worker `fsevents` (`a_stream_says_when_it_is_up_and_hears_what_happens_then`,
    `a_stream_dropped_before_it_is_up_is_stopped_and_never_says_up`), and `tests/fswatch.rs`
    `a_folder_deleted_and_made_again_is_followed` (now also back within 500 ms, and its files'
    sizes followed anew) and `a_folder_kept_is_heard_while_the_stream_for_a_new_one_starts`.

- ✅ **⌘P opens a file** (2026-10-04, readiness N20). "Open file…" had no chord, and a test kept
  ⌘P free. ⌘P is the quick open of every editor in the Zed and VS Code school, so it is bound
  to `OpenFile` in the workspace (not in a remote window, whose app keeps its own ⌘P). Test:
  `a_chords_words_come_with_the_keymap`.

- ✅ **A file that is not there opens as a new one** (2026-10-04, readiness N20). A missing file
  showed "Cannot read" with no editor, so `$EDITOR new.md` in a worker's shell hung its program
  until "Give up", though the worker can make the file.
  - *Wire.* `FileRead::Absent { editorconfig }`: nothing at the path, in a folder that is there.
    It carries the `.editorconfig` the new file would have. A missing folder, a directory or a
    refusal stays `Missing` with the OS's word. Golden `worker_file_absent`.
  - *The tile.* An empty editor, its bar saying "Not on disk: saving makes it" (or the
    waiting program's line, whose "Done" makes it). ⌘S and "Done" save it unedited too, as
    `vi` writes an empty buffer; left empty it is made empty, with no newline, as `touch`
    makes it. The save is based on the epoch, so the worker writes it while nothing is there
    and calls it a conflict once something made the file meanwhile, rather than writing over
    it. An unsaved new file kept over a relaunch comes back without a conflict while the file
    is still absent.
  - *Deleted under a clean tile.* The text stays, unsaved, under the same bar, and ⌘S makes the
    file again with its line endings, from the epoch. Under an edit it is the usual conflict,
    which Compare shows as every line removed.
  - Tests: worker `a_missing_file_in_a_folder_is_one_to_make_and_is_made_by_its_save`;
    `slopty-ui` `a_missing_file_opens_as_a_new_one_and_its_save_makes_it`,
    `done_on_a_new_file_makes_it_then_answers` and
    `a_file_deleted_under_the_tile_keeps_its_text_and_a_save_makes_it_again`.

- ✅ **A folder's entries are made, moved and trashed on the worker, and a huge folder is
  paged** (2026-10-04, readiness N20; wire, worker and client; the tile's side is "The folder
  tile makes, renames, moves and trashes").
  A folder tile could only browse: the wire had no verb to change a file, and a folder past
  2000 entries showed its first 2000 and a count.
  - *Wire.* `ClientMsg::FsOp { request, op }`, answered with `WorkerMsg::FsDone { request,
    outcome }`. The op is `MakeDir { parent, name }`, `Move { from, to }` (a rename when both
    are in one folder) or `Trash { path }`. The outcome is `Done { path }` (where the entry now
    is, in the trash too), `Refused(why)` or `Failed { error }` with the OS's word. A refusal
    names its reason: not an absolute path or one that climbs, not one plain name, a protected
    place, a clash, a missing source or folder, a folder into itself, another volume, a volume
    with no trash. Goldens `golden_folder`.
  - *Nothing is replaced, nothing is unlinked.* A move is a rename that fails when its
    destination is taken, in one step (`renameat2` `RENAME_NOREPLACE`, `renamex_np`
    `RENAME_EXCL`; a look then a rename where the file system has neither). A name whose case
    alone changes, on a volume that ignores case, is the source itself and is renamed. A move
    across volumes is refused rather than copied and deleted, so a half-copied tree can never
    be left. Trash goes to the worker OS's own trash, where the person can put it back:
    `NSFileManager trashItemAtURL` on a Mac (the Finder's own, with Put Back), and the
    freedesktop.org trash specification on Linux (`$XDG_DATA_HOME/Trash`, else the volume's
    `.Trash/$uid` or `.Trash-$uid`, the info file written before the entry moves). The
    optional `directorysizes` cache is not written. `slopty_platform::trash`.
  - *What is protected.* An admitted client can open a shell on the worker, so the ops are not
    held to a few roots, which would only send the person to the shell. What no folder op
    moves or trashes is a place that holds others' work: the file system's root, a volume's
    root (a mount point), the home, and any folder holding it, looked at as named and as
    resolved, so a link to the home counts. Paths are absolute or `~/…`, and none climbs with
    `..`.
  - *In order.* A client's ops go through the queue its saves take, so a file saved and then
    moved is moved with what was saved. The folders they change reach their tiles through the
    folder watch, as any change on disk does.
  - *Pages.* `ClientMsg::FolderPage { path, after }` asks for the entries after one, in the
    folder's order, and `WorkerMsg::FolderPage` answers with them and the whole count. A
    cursor on the last entry, not an offset, so an entry made or removed meanwhile neither
    repeats nor skips one. `slopty_client::folders::FolderPages` joins the pages, asks for one
    more as the person wants it, and asks again for as many after a relist. `FsOps` numbers
    the ops and holds them until answered, and `sentence` says how one went.
  - *For agents and the CLI.* `Verb::FsChange { worker, op }` carries the same `FsOp` through
    the server, beside `ls`, `cat` and `stat`, and the worker answers it with
    `slopty_worker::fsop::apply`: `Outcome::FsDone { path }`, or an error in plain words that
    says what stood in the way and that nothing was touched (a clash is `Conflict`, a
    protected place `Forbidden`, another volume or no trash `Unsupported`, the rest
    `Invalid`; the OS's own refusal is `Failed`). The MCP tools are `make_dir`, `move_path`
    and `trash_path`, and the CLI's `slopty mkdir`, `mv` and `trash`, each printing where the
    entry now is. A folder is named by its whole path and split into its folder and name
    (`~` and `/` alone name none). Every one takes an idempotency key. Goldens
    `fs_change_make_dir`, `fs_change_move`, `fs_change_trash`, `fs_done`; tests
    `files_are_made_moved_and_trashed_and_a_refusal_is_said` (`slopty-tools`) and
    `a_worker_s_folders_are_made_and_moved_and_a_refusal_touches_nothing` (`slopty-cli`,
    through a real server and worker).
  - Tests: worker `fsop` (made once and by a plain name only; a move never replaces and never
    goes into itself; a case-only rename; the trash and its refusals; the protected places;
    a mount point), `listing` `the_pages_of_a_huge_folder_hold_every_entry_once_in_order`;
    `slopty_platform::trash` (the home trash and its info file, a link trashed itself, the
    escaping, the Finder's trash); `slopty_client::folders`; and through a real worker
    `folder_ops_make_move_and_trash_through_the_worker` and
    `a_huge_folder_is_paged_through_the_worker`.
- ✅ **The folder tile makes, renames, moves and trashes, and pages as it scrolls**
  (2026-10-05, readiness 10-05 G6). The wire, worker and client parts above had no UI: the
  tile only browsed, and stopped at a folder's first 2000 entries.
  - *Keys, as Finder's.* ⌘⇧N "New folder" writes the name in a field over the rows; "Rename
    or move…" (no default key, as Finder's ↩ opens here) writes it in the row itself, where a
    plain name renames and a path moves (`../done/`, `~/archive/a.txt`; a trailing `/` keeps
    the name, `folder::destination`); ⌘⌫ "Move to Trash". All three are palette lines while a
    folder tile has the keyboard. While a name is written the field has the keys, so ↩, ⌫ and
    the arrows edit it rather than open, go up or walk the rows; Esc or a click elsewhere puts
    it away and asks nothing. What was made or renamed is selected once the folder lists it.
  - *Shown at once.* The tile draws what it asked as done before the worker answers: a new
    folder as a row over the list, a renamed row with its new name, a row moved away or
    trashed faded nearly out. A change done stays drawn so until the folder's next listing
    (the watch sends it), so nothing flickers back between the answer and the listing; a
    refusal draws the row as it was at once and says why in a notice. A change done says
    nothing, the row being the word, except a trash, whose notice carries "Put back": a move
    from where the worker's trash put it back to where it was.
  - *A link that drops* takes the answers with it: the tile asks for the folder again on the
    next link, which shows whether each change was made, and a notice says the machine went
    out of reach before it said.
  - *Pages.* The list asks for the next page when the rows it draws come within 40 of the
    last listed, so scrolling never meets the end of a page; a relist keeps as many pages as
    were wanted (`FolderPages`). The workspace hands `WorkerMsg::{FolderPage, FsDone}` to
    `WorkspaceView::{folder_page, fs_done}`.
  - Tests: `workspace::tests::folders::a_folder_makes_renames_and_trashes_its_entries`,
    `a_long_folder_comes_a_page_at_a_time`, and `folder::tests` for the destination of a
    written path.

- ✅ **A conflict can be compared before it is settled** (2026-10-04, design B9). "Changed on
  disk" offered only Reload, which drops the edit, and Overwrite, which drops the disk's text,
  with no way to see what either loses.
  - "Compare" shows the disk's text against the edit as the thread view and the review tile
    draw a diff (`conversation::lines`, three lines of context, git's hunk headings), the disk
    as the old side, so what saving would change reads as additions. "Back to edit" returns.
  - The diff is worked out off the UI thread and redone only when the edit or the disk moves.
    When the disk's text is not in hand (a save refused without a read), Compare reads the file
    again with the edit kept.
  - Tests: `a_patch_is_the_hunks_from_the_disk_to_the_edit` and the file tile's compare test.

- ✅ **A program's links open for the machine it runs on** (2026-10-04, readiness N19). ⌘-click
  sent every link to this device's handler, so an OSC 8 `file://` link from `ls --hyperlink`,
  `rg` or `delta` named a path on this Mac, and `http://localhost:3000` a port on it.
  - `terminal::url::destination` sorts them. A `file://` link, with or without the host those
    tools put in it, is the path on the shell's machine, percent-decoded, and opens as a file
    tile as a ⌘-clicked path does. A page on the loopback (`localhost`, `127.0.0.0/8`, `[::1]`,
    `0.0.0.0`, the handoff's `Wary::Loopback`) opens in a page tile on that machine, which
    reaches the port through it. Anything else goes to this device's handler as before.
  - `open <file>` in a shell shows one existing file in a tile there (`CtlRequest::Edit`, not
    waiting): a PDF, a picture or a text, on Linux too, where the system's opener had no screen.
    A flag, an application, a folder or a saved page (`.html`) is still the system's.
  - Tests: `a_link_goes_to_the_machine_the_program_runs_on`,
    `a_link_opens_on_the_machine_the_shell_runs_on` (`slopty-ui`) and
    `open_shows_one_existing_file_in_a_tile` (`slopty-cli`).

- ✅ **The sticky block row does not repeat the title** (2026-10-04, design review #15). A shell
  running `cargo test` said "cargo test" in its header and again on the pinned block's row one
  line under it. The row is left out when its command is the command running now, which names
  the tile, or the title the program set. Test: `a_sticky_header_does_not_repeat_the_title`.


- ✅ **Upload and download from the keyboard on a Mac** (2026-10-04, readiness N21). Dragging was
  the only way files crossed on a Mac: the palette's Files picker lines were iOS-only, and a
  clashing upload landed in the drop directory, where it seemed to vanish.
  - "Upload…" and "Download…" are palette lines and File menu items. Upload asks the open panel
    for files and folders, which go up to the focused shell, folder or window as a drop would.
    Download asks the save panel, in `~/Downloads` on the entry's name, where the folder tile's
    selected entry goes. It comes down beside the chosen path and is renamed into place, so a
    half-arrived file never sits under that name, as "Save a copy…" does. The person hears
    "Saved ~/Downloads/report.pdf", or "<name> was not saved: <why>".
  - **One save in two words** (amended 2026-10-10, orchestrator-first study §C.8). "Save a
    copy…" of a file tile and "Download…" of a folder's entry were two paths with two notices
    for one act. Both now ask `ask_files`'s export, one kind of transfer (`Bringing::Save`), one
    notice; only the words that fit each tile stay apart.
  - A top-level entry whose name is taken where it goes lands beside it under the next free
    name, as Finder's Keep Both names it (`report 2.pdf`, `proj 2`, `archive 2.tar.gz`). A file
    is renamed into place only where nothing is, so nothing is written over. A folder drop
    that was renamed says so: "report 2.pdf (report.pdf was there already)".
  - A failed upload names what was sent and where: "report.pdf did not reach studio: <why>",
    or "3 files" for more than one.
  - Tests: worker `a_taken_name_is_numbered_as_finder_numbers_it`,
    `a_clash_lands_under_the_next_free_name_and_finish_names_the_entries`,
    `a_name_taken_while_the_file_came_is_kept_and_the_file_lands_beside_it`,
    `a_renamed_entry_resumes_into_the_name_it_was_given`; `slopty-ui`
    `download_brings_the_selected_entry_where_the_save_panel_says`,
    `a_drop_on_a_folder_goes_up_into_it_and_lists_it_again`, `an_upload_outlives_its_workers_link`,
    `upload_and_download_are_offered_on_every_device`; the app's File menu test.

- ✅ **`open <folder>` opens a folder tile** (2026-10-04, readiness N19). `open .` in a worker's
  shell ran the worker's own Finder, which nobody at the client sees, and `xdg-open` on a
  Linux worker had no screen at all.
  - The shell's `open` sends one folder as an absolute path ending in `/` (`..` resolved), not
    waiting. The client opens a path ending in `/` as a folder tile, as it does a typed one.
  - A package stays the system's to open: an `.app` and the other bundle extensions, and any
    folder with a `Contents` folder inside.
  - Tests: `open_shows_one_existing_file_or_folder_in_a_tile` (`slopty-cli`),
    `a_folder_a_shell_hands_over_opens_as_a_folder_tile` (`slopty-ui`).

- ✅ **An open on a machine out of reach is said, never dropped** (2026-10-04, readiness N18).
  ⌘T, a "New terminal in…" line and ⌘O sent their ask on a link that was not there, which
  dropped it with only a debug line, so the person saw nothing happen.
  - `open_session_on` and ⌘O check the link first (`reachable_for`). With none they make
    nothing and say "The terminal did not open: studio is unreachable". A shell is not queued
    for the link's return: it would open minutes later where the person no longer is. A note
    still goes through the item queue, since it lands where it was put.
  - ⌘O on a machine out of reach asks for no list, so no picker turns up unasked once the
    machine is back.
  - A machine "+" was pointed at is let go when the menu closes with nothing chosen (a click
    away, Esc, "+" again), so the next ⌘T goes where the focus is.
  - Test: `opens_on_a_worker_out_of_reach_are_said_and_not_kept` (`workspace/tests/away.rs`).

- ✅ **A cold launch draws every tile from its worker's items as last seen** (2026-10-04,
  readiness N2). The layout came back with each tile's place, but a tile's item came only with
  its worker's first snapshot. Every worker not yet linked was a hole: no name, no
  "Reconnecting…". After an app update every worker runs another build and never links, so
  the tile pill's Update, the one way out, was never drawn.
  - `slopty_client::items::ItemCache` keeps each worker's registry under the data dir
    (`items/<worker>.items`): postcard, 0600, replaced whole. It is written after every change
    off the UI thread, one write per worker at a time with the latest last.
  - `add_worker` seeds the registry from it at version 0. The tiles draw as they were under
    the pill that says where the worker is, Update included. The first snapshot replaces the
    seed whole, and a tile whose item it no longer has leaves, as on any snapshot. A file from
    another build reads as nothing.
  - **Update where the worker is named.** A worker on another build offers Update on its
    navigator row, at rest, in place of its readouts, and in the machine's menu. Each runs the
    app's update against the host the notice names, as the pill does, and neither offers it
    while one runs.
  - **A tile kept nowhere still stands.** The cache misses a tile when nothing was kept yet,
    or when this build cannot read what an older one kept, which is the morning after an
    update. While its worker is away such a tile draws the worker's name in its header over
    the same pill, with its Retry now, Wake or Update; it is never a gap. Once the worker is
    linked its registry decides, and a tile it lacks leaves with the snapshot.
  - **The start page names it too.** Its machines list offers Update on the row of a worker
    on another build, as the navigator does.
  - The self-test reads the layout as the app does: each stack has a data directory of its
    own, so a relaunch within a test puts its layout back as a person's would.
  - Tests: `the_items_kept_read_back_and_a_broken_file_goes` (`slopty-client`),
    `a_cold_launch_draws_the_kept_tiles_until_the_worker_is_back` and
    `a_tile_kept_nowhere_says_where_its_worker_is` (`tests/relaunch.rs`),
    `a_worker_on_another_build_offers_update_where_it_is_named` and
    `the_start_page_offers_a_worker_on_another_build_its_update` (`tests/bars.rs`), and the
    `tile-away` goldens in both themes (`gallery.rs`
    `a_tile_kept_nowhere_says_its_worker_is_away`).

- ✅ **The away pill says why and offers the way back** (2026-10-04, readiness N9). The pill
  said only "Reconnecting…", so a machine the tailnet policy shuts out, one asleep and a
  dropped link all looked alike, and the way back was buried in the hosts popover.
  - A dropped link shows the first line of its reason beside the pill's text. A machine the
    policy turns away says "studio does not let this device in".
  - Buttons, only while the tile is away: Retry now runs the app's connect for the host;
    Wake runs its wake while the machine is unreachable or gone; Copy grant puts the tailnet
    grant the app derives for this device on the clipboard when the policy turns it away, and
    says where it goes. A button the app cannot run is not drawn.
  - Test: `the_away_pill_says_why_and_offers_the_way_back` (`tests/away.rs`).
  - **A worker whose own writes fail says so.** A full or read-only disk cost a restart its
    terminals' kept screens and its agents' thread logs, and showed only in the worker's log.
    Each kind of write marks itself failing or written (`slopty_worker::caps::not_written`,
    `wrote`). The first that fails, with its error, goes out in `WorkerCaps::writes_failing`
    at the next 5 s probe, and clears once that kind of write goes through again.
    The worker's doctor prints it as a ✘ line, and its machine's row in the navigator leads
    its warn line with it. Tests:
    `a_failing_write_is_said_until_one_goes_through` (`slopty-worker`),
    `doctor_report_names_the_binary_and_flags_missing_permissions` (`slopty-cli`),
    `a_workers_health_shows_only_when_something_is_wrong` (`tests/facts.rs`).

- ✅ **The clipboard is shared or stopped per machine from the palette** (2026-10-04,
  readiness N25). Sharing per machine was a settings key, so stopping it for one machine meant
  editing the file, and nothing showed which machines were left out.
  - The palette has "Stop sharing the clipboard with studio" for each machine it is shared
    with and "Share the clipboard with studio" for the rest. Running one changes the live
    sharing at once, says so, and the app writes the choice under `[clipboard.workers]` by
    the machine's name. The text edit keeps the rest of the file as the person wrote it.
  - A machine the clipboard is not shared with carries a clipboard glyph in its navigator
    row, named for a screen reader. Shared machines carry nothing, since sharing is the
    default and the quiet state.
  - The machine's row in the hosts popover offers the same under the pointer: "Unshare
    clipboard" or "Share clipboard", which runs the palette's action.
  - On macOS, files copied on a machine that has no location in Finder here (no File
    Provider domain) paste into shells only. The first such copy from each machine says so,
    so a paste in Finder that does nothing is not a mystery.
    Tests: `a_workers_file_names_never_go_on_this_pasteboard` (client),
    `files_a_worker_copied_paste_into_its_shell_or_travel_to_another` (ui).
  - Test: `the_clipboard_is_stopped_and_shared_with_one_machine_from_the_palette_or_its_row`
    (`tests/bars.rs`), `the_clipboard_is_kept_off_for_one_machine_by_name` (`slopty-app`).

- ✅ **A layout this build cannot read is set aside and said** (2026-10-04, readiness N29).
  The read gave nothing for a file that would not parse, and the next save overwrote it. So a
  layout from another build was lost without a word.
  - A file that does not parse is renamed `layout.json.bad`, replacing an older one, and the
    workspace starts empty with "The last layout could not be read; it was kept as
    layout.json.bad". No reader for older shapes is kept (pre-release). A missing file is
    still a first launch, said nothing about.
  - The unsaved-edit backup says "Unsaved edits can't be kept on this device" with the first
    line of the error once three passes in a row have failed. One failure is a passing hiccup
    the next pass retries; three are a disk that will not take them. A success resets the
    count, so a new run of failures is said again.
  - Tests: `the_layout_is_saved_and_restored` (`tests.rs`),
    `a_failed_write_is_tried_again_without_another_edit` (`tests/unsaved.rs`).

- ✅ **A yes or no is answered from wherever it is seen** (2026-10-04, readiness N6 and N14).
  A thread agent's approval could be answered only inside its thread. The inbox and the
  notification offered Allow and Deny for terminal agents alone. On iOS, an answer tapped on
  the lock screen could be cut off when the app was suspended before it was sent.
  - A thread's request is answerable outside the thread when it is an approval with a plain
    allow and a plain deny, the same pair the thread's own keys pick. Anything with more
    choices (a standing grant, edit before allowing, a questionnaire) opens the thread
    instead, because a one-word answer would choose for the person.
  - Its inbox row and its note carry Allow and Deny. Either sends the answer intent to the
    thread's agent once, and the row and the note go when the thread hears it.
  - The app holds a background grace from the tap until every answer tapped is settled (sent,
    or given up as unreachable), plus two seconds for the bytes to leave. Then iOS may suspend
    it again.
  - **The system waits for the same word.** The notification delegate used to say it was done
    with a button's response as soon as it handed the tap on, before the app had even taken
    its grace, so the system could suspend the app in between. Now a button answered in the
    background (`Tap::finished_later`: Allow, Deny) keeps its completion handler owed, and the
    app says it (`notify::taps_finished`) when its grace goes. A tap that names no request
    settles at once, so nothing is owed for long. The note itself and Show are done when handed
    on, as before.
  - **Settled on the worker's word** (2026-10-11, the orchestrator-first study, item 22).
    The grace ended a fixed two seconds after the answer was handed to the link, not when
    the worker took it. A server's failure read "no longer waiting" and hid the request for
    the run, and the app's word to the system released every press owed, not its own. Now:
    - A note's answer settles when its worker acknowledges the intent. One it turns down, or
      never acknowledges within the note's hold (`HOLD_VERDICT`, 15 s), says "Your answer was
      not sent" (`ANSWER_NOT_SENT`) and clears its sent mark, so the request can be answered
      again. The fixed `ANSWER_FLUSH` is deleted.
    - Through the server, a lost link (`ServerUnreachable`) is tried again each second within
      the same hold. A request the server no longer finds is "no longer waiting"; a call that
      fails is "not sent", and the request may be answered again.
    - The system's completion handlers are owed by note id (`notify::tap_finished`). The app
      releases only the taps it handed to the workspace, so a press still on its way keeps
      the app awake for its own answer. `taps_finished` stays for the windowless answer,
      which gives up on all.
    - Still to come with the server's push word to clients: a phone nothing can be pushed to
      says so and keeps listening through its grace.
  - Tests: `a_thread_s_yes_or_no_is_answered_from_its_row_and_its_note`
    (`tests/thread_waits.rs`), `a_notes_answer_waits_for_its_request` (`tests/attention.rs`),
    `a_notes_answer_goes_through_the_server_while_its_worker_is_away` (`tests/approvals.rs`),
    `only_an_answer_in_the_background_is_finished_later` and
    `the_system_hears_a_tap_is_done_once_its_answer_is_out` (`slopty-platform` `notify`).

- ✅ **Every transfer is on one list in the status bar, and survives a relaunch** (2026-10-04,
  readiness N22). The bar counted only uploads, as one percentage. A download (Save a copy,
  Download, a drag out) showed nowhere in the app, and nothing could be stopped from the bar.
  Every transfer in flight was lost with the app.
  - **The list.** The bar's readout counts both ways (`1 upload`, `2 downloads`, `3 transfers`,
    with the percentage of their bytes together). It shows while a transfer is not the focused
    tile's own upload, which its header says with its stop. Clicked, a popover lists each one:
    its way's mark, its name, its machine, `42% · 3.1 MB/s · 12 s left` and a hairline of how
    far it got, and its Cancel under the pointer or the keyboard. It closes once nothing is
    left. The rate is the bytes landed over the last five seconds, measured up to now, so a
    stall reads as slowing. It is said only after the first second, which is mostly
    handshake.
  - **Downloads are seen.** `Remote::download` takes a `xfer::Download`: the transfer id the
    UI chose, so its Cancel reaches it on whichever link it is, and a watch the client's
    `Fetch` tells of each byte count. The UI draws it at most four times a second.
  - **The ledger.** A transfer that still means something to a new run is written to
    `transfers.json` in the data directory (`slopty_client::xfer::ledger`). That is a drop on a
    shell or a folder, or a worker file brought down to a place the person chose. A paste, an
    attachment and a drag over a remote window end with their run. It is written whole, off
    the main thread, newest only, and goes when the last one ends. At the next launch each is
    listed as waiting for its machine. When the machine links, an upload begins again under
    its id with `again` set: every file first asks the worker what it holds, so it resumes
    where the worker's partial file stands. A download keeps the version (size and mtime) of
    each file the worker named, and is fetched again into its staging directory, named for the
    transfer. Each file of a kept version is asked from where its `.partial` stands, so it
    resumes mid-file as an upload does. A file the worker changed since is sent whole.
    A taken-up upload says "a.txt reached studio" at its end, and types nothing: the shell's
    prompt has moved on since.
  - Tests: ui `the_transfers_list_shows_both_ways_and_stops_one`,
    `transfers_in_flight_are_taken_up_at_the_next_launch`,
    `a_transfers_rate_follows_its_last_seconds`, statusbar `the_readouts_say_what_they_count`;
    client `transfers_kept_come_back_and_the_file_goes_with_the_last`,
    `an_upload_begun_again_after_a_relaunch_sends_from_what_the_worker_holds`,
    `a_download_taken_up_after_a_relaunch_resumes_from_its_partial_file`,
    `a_download_lands_in_place_and_answers_the_blocking_caller` (its watch ends at the size).

- ✅ **A failed drag out says why; a file that never came says it waits for its machine**
  (2026-10-04, readiness N30). A file promise that could not be kept left only Finder's bare
  error. A drag out of a machine that is away did nothing. A file tile whose machine went away
  before its text came sat on "Reading…".
  - A drag out from a machine that is away says "studio is away; a.txt was not dragged out".
    A promise whose download fails says "a.txt was not dragged out: why". A drag out is now a
    download on the transfers list too.
  - A file tile not read yet whose link drops says "Opens when the machine is back". The next
    read ends it. One opened while the machine is away shows the away pill over an empty
    body, as every tile of an away machine does.
  - Tests: `a_drag_out_that_fails_says_why`,
    `a_file_whose_machine_went_away_unread_says_it_waits_for_it`.

- ✅ **Snooze is gone** (2026-10-04, the orchestrator's cuts after the feature audit
  `.research/feature-audit-2026-10-04.md`). Supersedes ui.md's "Snooze, honestly" and "Snooze
  is the server's, with presets".
  - Before: a finish could be put off from the inbox, first for an hour in one client's
    memory, then (the server's version) until a preset time in the person's zone, kept in
    `snoozes.json` and sent to every client.
  - Why it goes: the person directs and reviews agents and reads what finished when they get
    to it, and failures and finishes are already quiet (an inbox row, a dot, the bell's count).
    A way to hide a quiet row for a while was one more thing to learn and keep, and nobody
    used it. A feature not worth its place is deleted, not hidden.
  - Deleted, with no shim: the client's `SNOOZE_FOR` and local wake, the inbox's H key and its
    presets, `slopty_proto::snooze`, `Verb::{Snooze, Unsnooze}`, `Outcome::Snoozed`,
    `FromServer::Snoozes` and the client's `Change::Snoozes`, the server's snooze list, its
    `snoozes.json` store and its loopback test, and the goldens of all of them.
  - A `snoozes.json` left in a server's data directory is not read; it can be removed.

- ✅ **The corner points only at what needs you** (2026-10-04, building ui.md's "The corner
  points at what needs you, and only that").
  - A shell's or an agent's failure or finish no longer says anything in the corner. It is a
    dot on its tile, and for an agent's turn a row under *To review* and the bell's count.
  - The corner speaks when a terminal's agent, or a thread its worker's table says waits,
    comes to need the person. The app must be in front and the tile off screen. A thread is pointed at only on a change its client saw, never on the table it
    first hears.
  - Its one action is "Go". Each time the agents move, any word whose tile no longer needs
    the person goes: it was answered in the tile, its row, a note or another client.
  - Tests: `workspace::tests::needs_you::an_agent_off_screen_that_needs_you_says_so_in_the_corner`,
    `workspace::tests::thread_waits::a_thread_off_screen_that_comes_to_need_you_is_pointed_at`.

- ✅ **A key under a resting pointer keeps a notice held** (2026-10-04, the Ely study
  `.research/ely-gpui-components-2026-10-04.md` §6.1).
  - A notice under the pointer stays until the pointer leaves. GPUI's default hover listener
    treats a key press as the pointer leaving until the mouse moves again, so typing under a
    resting pointer started the countdown of the notice being read.
  - The notice's hover listener now runs in `HoverListenerMode::InputModalityIndependent`, as
    Ely's toast does. The status bar's and the navigator's hovers keep the default: a key
    should hand those back to their readouts.
  - Test: `workspace::tests::toasts::typing_under_a_resting_pointer_keeps_the_hold`.

- ✅ **A modal keeps the keyboard, and gives it back on every way out** (2026-10-04, the Ely
  study `.research/ely-gpui-components-2026-10-04.md` §5 #1 and §6.2).
  - Before: Tab ran `window.focus_next` across the whole window, so from a button in the
    palette, the settings, the "New project" sheet, a picker or project search it walked out
    into the tiles behind. The About panel and a picker gave the keyboard to the workspace's
    own handle when they closed, which left a shell's cursor hollow. A keyboard whose holder
    left the frame stayed nowhere.
  - One contract, after Ely's `FocusScope::trap`, `take_focus` and `give_back`
    (`a11y::trap`, `a11y::hold`, `a11y::step`, `a11y::reclaim`):
    - Each modal's surface tracks a scope handle and registers it with the handle its keyboard
      lives at (the field, the form).
    - Tab and ⇧Tab, everywhere they are handled (each stop, the workspace, the settings'
      chord recorder, ⌃Tab), take one `step`. Inside a drawn trap the step walks its stops and
      wraps there. A trap with no stop keeps the keyboard where it is.
    - A trap is only one while it is drawn: a marker action on its element says so, so a
      closed modal's handle traps nothing.
  - **Every way out gives it back.** The About panel and a picker now give the keyboard back
    where the focused tile keeps it (`return_keyboard`), as the palette, the settings and the
    sheet already did. The palette's chosen action still runs after that, from there.
  - **Lost, it comes back.** The workspace listens for a keyboard that has gone nowhere
    (`on_focus_lost`). With a modal open, the modal takes it at its home; otherwise it goes
    where the focused tile keeps it.
  - Tests: `a11y::tests::{tab_walks_inside_an_open_trap, a_trap_with_no_stop_keeps_the_keyboard,
    a_closed_trap_lets_tab_walk_the_window, a_lost_keyboard_comes_back_to_the_open_trap}`
    (after Ely's overlay tests), and
    `workspace::tests::modal_focus::{the_palette_holds_the_keyboard_and_hands_it_back_to_the_shell,
    a_lost_keyboard_comes_back_where_it_belongs}`.

- ✅ **The title bar's empty span moves the window** (2026-10-04, the Ely study §6.3).
  - Before: the window is `appears_transparent`, so our bar is drawn where macOS's title bar
    was, and nothing in Slopty asked the system to move or zoom the window from it.
  - A press on the bar's empty span that moves calls `start_window_move`, and a double-click
    calls `titlebar_double_click`, which follows the person's "double-click a window's title
    bar to" setting. A press on any of the bar's buttons stops before it reaches the bar.
  - Test: `workspace::tests::bars::the_title_bars_empty_span_moves_and_zooms_the_window` (the
    test platform keeps the asks rather than performing them).

- ✅ **The palette, the picker and a file's symbols rank fuzzily** (2026-10-04, the Ely study
  §6.4).
  - Before: a line matched when each word of the query was a substring of it, in list order,
    so "nwt" found nothing and the line wanted sat under a dozen that only named the folder.
  - Now `fuzzy::Fuzzy` scores with `nucleo-matcher` (fzf's scorer, as Helix runs it). Every
    word must match, in any order and any case. A word matches a line's name fuzzily but its
    place (worker, folder, what its agent was asked) only as spelled, since scattered letters
    find something in any long text. A name spelled whole leads, then a word in the name beats
    one only in the place, then nucleo's score. The palette orders its groups by their best
    line, and highlights only the name's matched characters.
  - Tests: `fuzzy::tests::*` (five), and the palette's
    `a_query_ranks_each_section_and_shows_what_it_matched`.

- ✅ **A worker's thread hub knows which agents it can start** (2026-10-04, from the thread
  view's lane). The hub is told when it is made, and again when the worker connects, goes, or
  reports other agents in its caps, so its start menu never offers an agent the machine lacks.
  Test: `workspace::tests::thread_start::a_workers_hub_knows_the_agents_it_can_start`.

- ✅ **Pointing is gone** (2026-10-04, the feature audit's cuts).
  - ⌘⇧O and the palette's "Point other devices at this tile" sent `ClientMsg::Point`, which the
    worker relayed as `ItemSync::Pointed` so every other client showed a toast with Go. The
    orchestration verb `PointAt`, the `point_at` tool and `slopty item point` did the same for
    an agent. The person works alone across their devices and never used it, so it carried a
    wire message, a toast kind, a chord and a tool for nothing.
  - All of it is deleted, with its goldens and tests. The worker's flood tests now flood with
    renames of one note, which reach every client the same way. ⌘⇧O stays the editor's symbol
    list.

- ✅ **The inbox is the navigator's attention sections** (2026-10-04, the feature audit's cuts).
  - Before: the bell opened a popover of its own, with *Unread* and *All* views, a history of
    the last 200 finishes, mark read, mark all read and a keyboard of its own (J/K, E, U, ⌘↵,
    ⌘⌫). The navigator listed the same agents again under *Needs you*, *To review* and
    *Working*, but only those whose own row was out of sight, and the status bar counted every
    worker's agents a third time. The person reads one list, so two of the three went.
  - Now the bell (and ⌘⇧U, "Show what needs you") shows the navigator, docked where it docks,
    with its list at the top and any filter emptied; a scope stays. *Needs you* and *To review*
    list every wait and every turn left to review, its tile's row in view or not, since they are
    what the bell counts. A waiting row carries "Deny" and "Allow" for a yes or no held here.
  - The popover, its views and history, mark read, mark unread, "Mark all read", its key scope
    and its notices' Undo are deleted. A turn to review is read by looking at its tile.
  - *Working* and its once-a-second clock are gone: an agent at work is marked on its tile's
    row and header, and nothing ticks for it. The status bar's per-worker agent counts are gone.
  - A shell's finish is its tile's dot alone. It no longer counts on the bell or the Dock, and
    `SLOW_COMMAND` (a finish worth a mark, a turn worth a review) went from 5 s to 30 s, past a
    test run's or a build's usual wait.
  - Tests: `workspace::tests::needs_you::the_bell_counts_what_needs_you_and_shows_the_navigator_at_it`,
    `workspace::tests::frame::the_bell_counts_what_needs_you_and_a_rows_tile_clears_it`,
    `workspace::tests::attention::an_approval_is_answered_from_the_note_and_its_row_where_they_are`,
    `workspace::tests::thread_waits::a_thread_s_yes_or_no_is_answered_from_its_row_and_its_note`,
    `workspace::tests::chrome::agents_at_their_turn_draw_no_section_and_no_clock`.

- ✅ **A machine's actions are on its row** (2026-10-04, the feature audit's cuts and the T3
  Code study's §13).
  - Before: what could be done to a machine lived in a popover over the status bar's right end
    (opened by "N machines" while one was down, or the "…" menu's Machines), and the palette
    had "List machines" as well, beside the navigator that already listed every machine.
  - Now a machine's row in the navigator has "…" beside its chevron and "+", reachable by the
    keyboard, which hangs a menu from it. The menu leads with the machine as it reports itself,
    read-only: its system and load, then each coding agent installed there with its version
    (the number from what the agent prints, so "codex-cli 0.48.0" reads "Codex 0.48.0"). Then
    Update while it runs another build, Connect while its link is down, Wake while it can be
    woken, the clipboard shared or not, and Forget for one added by address (since 2026-10-04,
    for one that is not online, through the server's `ForgetWorker`). Update also stays
    on the row at rest, since a machine on another build links only once updated.
  - Nothing there signs in, installs or probes an agent: what it printed is all it shows.
  - The hosts popover, the status bar's machine count, the "…" menu's Machines row and "List
    machines" are deleted. The palette's Workers section still lists each machine.
  - Tests: `workspace::tests::bars::{the_keyboard_reaches_a_machines_menu,
    a_machines_menu_says_what_it_runs_and_does_what_the_app_lets_it,
    a_sleeping_worker_is_woken_from_the_palette_and_its_row,
    the_clipboard_is_stopped_and_shared_with_one_machine_from_the_palette_or_its_row}`.

- ✅ **The rare niri ops are gone** (2026-10-04, the feature audit's cuts). The person wants
  nothing kept that is not used or not worth its place. Of the niri ops bound on 2026-09-28,
  these were judged rare here, with no usage count behind it since Slopty keeps none: the
  first and last column (⌘1 and the arrows reach them), back to the previous workspace,
  carrying a column to the workspace above, below or N, moving a workspace up or down, the
  previous preset width (⌘R cycles all three), a column 10 % narrower or wider (a drag on the
  gap sizes it), and centring or filling the visible columns. Their actions, keys and palette
  lines are deleted, and so are the layout model's operations nothing else called
  (`set_width_delta`, `expand_to_available_width`, `center_visible_columns`,
  `move_workspace_up`/`down`, `move_column_to_workspace*`, `focus_column_first`/`last`,
  `focus_workspace_previous` and the previous workspace it remembered). A tile still reaches
  another workspace: moved past the top or bottom of its column (⌘⌥⇧↑/↓), or dragged by its
  header. The palette keeps column left and right, the workspace steps, the first workspace
  and the column's ends.
  Tests: ui `workspace::tests::niri_keys`, client `centring_puts_the_active_column_mid_view`
  and `presets_cycle_both_ways_from_a_preset_and_from_any_width`.

- ✅ **One way to ask an agent its task** (2026-10-04, the feature audit's cuts, §1.7). The
  empty workspace's question (a composer-shaped field with a machine chip and a directory
  chip, `workspace/ask.rs`) and the starting tile's first-message field did the same job: a
  first message, and where to run it. The person wants one of each thing, so the question is
  gone. The empty page now leads with a "New agent" row, wearing the selected fill as the row
  ↵ runs, with ⇧⌘T printed beside it. It, or ↵ while the workspace holds the keyboard there,
  opens the starting tile at once: the machine "+" chose, else the one in context, its usual
  agent, in its latest place, the field there asking the task. Choosing another agent, machine
  or folder is ⇧⌘T's steps. Tests: `workspace::tests::palette::{
  the_empty_workspace_leads_with_a_new_agent,
  the_empty_workspaces_agent_starts_in_the_machines_latest_place}`.

- ✅ **One About, in the settings** (2026-10-04, the feature audit's cuts, §1.8). The About
  panel (the mark, the name, the version and the build) said again what Settings › About says.
  "About Slopty" is now an app command, in the app menu's first place, the palette and the
  keyboard settings, and it opens the settings on their About page. The panel, its action and
  its golden are deleted; the mark stays over the empty workspace. Tests: app
  `about_slopty_opens_the_settings_on_their_about_page`, ui
  `workspace::tests::modal_focus` (the palette now stands for a modal there).

- ✅ **A shell's header shows no ports** (2026-10-04, the feature audit's cuts). Each listening
  port was two pills on its shell's header, beside the status bar's count of the same ports
  and the palette's list of them. The header's pills are deleted; the count opens the list,
  where each port opens in a browser tile or the default browser, and the notice still says
  when a port is served on another number here. Test:
  `workspace::tests::remote::forwarded_ports_are_counted_and_listed_off_the_tile`.

- ✅ **The palette no longer reruns commands** (2026-10-04, the feature audit's cuts). The
  palette listed the last five commands of the shell a run would go to as "Rerun <command>".
  A rerun from there was judged rare against the shell's own history and the block menu's
  Rerun, which stay with ⌘⇧↩'s "Rerun last command". The lines, `PaletteRun::Rerun` and
  `TermState::recent_commands` are deleted. Test:
  `palette::tests::an_empty_field_lists_the_tiles_and_the_recent_commands`.

- ✅ **⌘Z takes a tile back only while its notice is up** (2026-10-04, the feature audit's
  cuts). ⌘Z was the workspace's "Undo close" all the time, so wherever no view took it first
  it brought back a tile closed long ago. It now binds in `Workspace && ClosingOffered`, a
  context the workspace adds only while a closed tile's notice stands (`CLOSING_CTX`). After
  that the palette's "Reopen" lines still bring a closed tile back. Test:
  `workspace::tests::a_closed_shell_can_be_taken_back`.
- ✅ **A thread is found again: the "Earlier" fold, past sessions, and a closed agent reopened**
  (2026-10-05, readiness 10-05 G2). The navigator listed a thread with no tile only while it
  was at work, a closed agent's tile ended its session after its notice and left the "Reopen"
  list, and the worker's past sessions (`ThreadRequest::Sessions`) were asked by nobody. So a
  thread at rest, or one whose tile was closed, could not be found again.
  - *Earlier.* The navigator ends with one fold, "Earlier", over the threads at rest (idle, or
    their agent exited) with no tile here and no tile of their terminal, newest first, each
    with how long it has rested. It starts folded. Open, it lists the newest 8 and a "Show N
    more" line; while the filter holds words it lists every match, as a fold hides nothing
    then. A scope narrows it to the project's threads, and a facet empties it, as for tiles. A
    row opens the thread's tile (an exited agent's thread offers Resume there). One fold at the
    end was chosen over a fold in each project: a project whose tiles are all closed would
    otherwise keep a header for its old threads, and the list would grow with every thread a
    worker ever kept.
  - *Resume a past session…* ends "New agent…"'s folder step. It asks the machine for the
    agent's sessions from the agent's own record, last prompted first (up to 50, every folder),
    and the next step opens at once saying it reads them, then lists each by its title, else
    its last prompt, with its folder and age; typing finds a session by its prompts too. The
    answer may say why there are none (the agent keeps no list), and the step says that. A
    session whose kept thread runs opens that thread's tile, since one writer holds a session;
    one whose agent exited opens and is taken up again as below; one with no thread here starts
    its agent on it in the agent's own words (`PastSession::resume`), and the worker finds the
    thread kept of it before making another. The app hands `WorkerMsg::Sessions` to
    `WorkspaceView::past_sessions`; an answer no open step waits on is dropped.
  - *A closed agent tile.* Its session still ends after the notice, so a closed agent stops as
    the person meant, but the tile stays on the "Reopen" list with its thread. Reopened, it
    comes back where it was as its thread's tile, and the thread hub takes the agent up again
    through its own door once the thread is known here (`ThreadHub::resume_when_known`, by
    `exited::gone`): Claude Code and Codex by a start of their session, pi and an ACP agent by
    the next message, nothing for an agent that cannot load its sessions.
  - Tests: `workspace::tests::thread_waits::threads_at_rest_wait_under_the_earlier_fold`,
    `a_closed_agent_tile_comes_back_as_its_thread_taken_up_again`;
    `workspace::tests::thread_start::a_past_session_is_found_and_taken_up_again`.

- ✅ **A new agent can start in a worktree of its own** (2026-10-05, readiness 10-05 G9). Several
  agents on one repository at once is a daily pattern, and only a project's tasks could have a
  worktree made for them. `Start` now carries `worktree`, a name, and the worker makes it for any
  start, a client's or a task's, through the one path the tasks used (`repo::worktrees::enter`
  over `make`): `.claude/worktrees/<name>` on branch `worktree-<name>`, reopened as it is when
  it is there, as Claude Code's own `--worktree` would. `Verb::StartThread` lost its own
  `worktree` field, which this replaces. (Its base: the entry "A new worktree starts current"
  below, which replaced `origin`'s default branch.)
  - *From the folder, not the root.* The clone is the main checkout of the repository the
    start's folder is in, so a start from inside another worktree makes one beside it rather
    than one nested in it. The agent stands where the folder stood in the clone, when that
    folder is in the new worktree too, so a start in a monorepo's `web/` stays in `web/`.
  - *In "New agent…".* After the folders, the folder step offers "New worktree of <repo>" once
    for each repository its folders are in, as a shell standing there reported it, from the most
    recent folder in it. The tile says "in a new worktree of …" while it asks for the first
    message. The client names the worktree after the agent and the last six hex digits of its
    tile's id, the random end of a v7 id, so two starts a moment apart never share one.
  - *Freed by the person.* A project task's worktree is removed once its work is merged and
    clean. A person's own has no such point, so it stays until the person removes it with
    "Remove this worktree" (the entry "A person's worktree is freed on their word" below).
  - A folder in no repository is refused in words before any agent opens.
  - Tests: `slopty-worker` `repo::worktrees::tests::a_start_enters_its_worktree_where_its_folder_stood`,
    `slopty-workerd` `threads::a_start_in_a_worktree_opens_its_agent_there`,
    `workspace::tests::thread_start::a_start_can_take_a_new_worktree_of_a_repository`; goldens
    `client_start` (changed), `client_start_in_worktree` (new), `start_thread` (changed).

- ✅ **A folder's changes are reviewed with no thread** (2026-10-05, readiness 10-05 G10). A
  review covered one thread's turns only, so work from an agent outside Slopty, from several
  threads, or from the person's own hands could not be read in the review tile.
  - *On the wire.* `ReviewScope::WorkingTree(Against)` is the working tree now, new files and
    all, against `HEAD` (what is not committed) or against the branch's base (all of the
    branch's work). The base is the merge base of `HEAD` with the first of `origin/HEAD`,
    `origin/main`, `origin/master`, `main` and `master` that shares history with it. A branch
    with none says so in the review's `absent`. A folder asks it as the person's git op,
    `GitOp::Changes`, answered with `GitDone::Changes(Review)`. That path already numbers,
    routes and keeps answers per repository (`GitBook`), so no new message and no new arm in the
    app were needed. A thread's review takes the same scope (`Snapshots::review`). A folder's
    changes tile is `ItemKind::Changes { path }`, placed, restored and closed like any tile.
  - *The person's index is never touched.* The tree is written through a scratch copy of the
    repository's own index (`repo::snapshot::working_tree`), so git hashes only what changed
    since it last looked, and the copy goes with the review. This was chosen over a persistent
    index per repository, which two clients could lock at once, and over a fresh index, which
    hashes every file on each review.
  - *The tile.* "Review changes" in the palette opens the focused folder's changes, or the
    changes of the repository the focused shell stands in, on that machine. If a changes tile is
    already open for that folder, it takes the focus instead. The tile is the review tile with
    no thread (`Reviewed::Folder`). Its switch offers "Uncommitted" and "Whole branch". With no
    agent to tell, it has no keep, put back or agent review. It takes comments, which its foot
    sends to a new agent in the folder (amended 2026-10-06, `ui.md`, "A folder's review sends
    its comments to a new agent there"). The commit sheet,
    the pull request and who wrote each line are there as for a thread. Its changes are read
    when asked, so a refresh button stands at the switch's end. They are also read again after
    a commit or merge from its sheet.
  - Tests: `slopty-worker` `review::a_folders_working_tree_is_reviewed_with_no_thread`,
    `workspace::tests::review_tile::a_folders_changes_open_as_a_tile_with_no_thread`; goldens
    `client_git_changes_head`, `client_git_changes_base`, `worker_git_changes`,
    `worker_git_changes_absent` and `worker_item_changes` (all new; no golden changed).

- ✅ **Wide work opens wide, and one focus mode gives it the window** (2026-10-05, design
  critique #01). A board or a review opened in a third of the strip read as cramped lanes and a
  wrapped diff, and the person spent the first moments of every review resizing panes.
  - *Wide work.* A tile showing a project's board, a review or a folder's changes asks the
    layout for room (`Layout::suit`): its column takes two thirds of the working width, and at
    least 720 pt where the strip has them (`WIDE_SHARE`, `WIDE_LEAST`). When the work leaves the
    tile (the board turns back to its terminal) the column goes back to the width a column opens
    at. The ask is made on the change only, so it never fights the person. A width the person
    chose is theirs and stays: a preset, a drag, a reset, focus mode or fullscreen all mark it.
    A neighbouring terminal stays one column away and the strip scrolls to it. A phone's columns
    are its width already.
  - *Focus mode.* "Maximize column" became "Focus mode" (⇧⌘↩, the Layout menu, the palette). On,
    the focused column takes the working width and a docked navigator steps aside, so the work
    has the whole window. Off, both come back as they were: the column's own width rule was
    never replaced, only overridden. If the column stops taking the full width some other way
    (a preset, fullscreen and back, the tile closing), the navigator comes back too. A navigator
    the person had already put away stays away. An overlaid navigator takes no width, so it is
    left alone. One reversible action was chosen over a second "maximise" beside fullscreen,
    which would have left the person two ways to almost do the same thing.
  - Tests: `slopty-client` `layout::tests::wide_work_takes_two_thirds_and_at_least_720_unless_the_person_sized_it`,
    `workspace::tests::niri_keys::focus_mode_gives_the_work_the_width_and_puts_everything_back`.

- ✅ **The strip shows no bare band and no sliver of cut words** (2026-10-05, from the goldens
  retaken after wide work). Two scenes broke the first proportions. In the first, the board
  opened right-aligned with an empty band between the navigator and it (`project-lanes`). In
  the second, a review that took 720 of a 752 pt strip left its thread peeking as 32 pt of cut
  glyphs (`review-agent`). Three rules now hold, all in `slopty_client::layout`.
  - *No room past either end.* The view never rests before the first column, and it rests past
    the last only once the whole strip fits (`Workspace::fit_offset`). A strip narrower than the
    view starts at its leading edge, and a cancelled drag settles there rather than right-aligned
    with bare room before the strip. niri lets that room show; Slopty does not, because the
    space reads as broken.
  - *Wide work meets the leading edge.* Focused work that opens wide (`Layout::suit`) puts its
    column's leading edge on the working area's, with its neighbour after it, as far as the end
    of the strip allows. That was chosen over keeping niri's least-movement fit, which
    right-aligned a column that grew past the view's end.
  - *No sliver beside wide work.* Wide work that would leave less than `PEEK_LEAST` (280 pt, a
    board's lane, the narrowest column that reads as work) beside it takes the whole working
    width, and the neighbour is one column away. Masking a sliver to its ground was the other
    choice. It was turned down because it is still a strip that holds nothing, and it would hide
    work the camera could simply leave out.
  - *The neighbour fills the room wide work leaves* (2026-10-06). The person saw a thread
    beside a board (720 of a 1032 pt strip) cut at the window's edge: its header, its empty
    state and its composer ended mid-word, because the thread kept the half width a column
    opens at. Now the column beside wide work takes the room that work leaves when nobody chose
    its width, so the pair meets both edges and each lays itself out at the width it shows at
    (`Workspace::settle_beside`, run on every change). Beside is the column after the wide one,
    or the one before when the wide one is last, as the camera shows them. It goes back to the
    opening width when the work leaves, stops being wide, or another column comes between, and
    a width the person chose stays theirs. Both widths are saved as the opening width, since the
    work suits them again when it shows. niri's peek of a cut neighbour was the alternative. It
    reads as a broken layout here, where a column holds a composer and words, not a window.
  - Tests: `layout::tests::{wide_work_meets_the_leading_edge_and_leaves_no_bare_band,
    a_strip_narrower_than_the_view_starts_at_its_edge, wide_work_leaves_no_sliver_beside_it,
    the_column_beside_wide_work_fills_the_room_it_leaves,
    a_fling_past_either_end_stops_at_the_end_snap}`.

- ✅ **New work starts where the focused work is** (2026-10-05, readiness 10-06 N16 and N14).
  - *From any tile that knows a folder.* ⌘T, "New agent…" and "Open folder…" start in the
    focused tile's folder. That was a terminal's, a file's or a folder tile's. It is now also a
    thread tile's or a review's, taken from where the thread's agent works as its worker's table
    says (`ThreadPlace::cwd`), and a folder's changes, taken from their path. Before, a shell
    opened from a Codex thread or from a review landed in home.
  - *A typed folder.* The folder step of "New agent…" takes a folder typed from its root
    (`/…`, `~/…`, `~`) as a line of its own, "Start in" and the path as typed. That uses a
    step's own typed lines (`CommandPalette::set_typed`), in place of the main palette's file
    and folder lines, which in a step would have started something else.
  - *Worktrees of every repository known.* "New worktree of" is offered for a folder whose
    repository a shell or a thread there reported. It is also offered for a folder inside such a
    repository, and for a folder tile that lists a `.git`. The worker already resolves the clone
    from any folder in it, and refuses in words for a folder in no repository.
  - Tests: `workspace::tests::review_tile::a_shell_opened_from_a_thread_or_its_review_starts_where_the_agent_works`,
    `workspace::tests::thread_start::the_folder_step_takes_a_typed_folder_and_a_threads_repository`.

- ✅ **A block or a selection goes to any thread, by thread** (2026-10-05, readiness 10-06 N15).
  - *Any agent.* "Attach block to agent" and "Attach selection to agent" picked their target
    among terminals with an agent in them. A Codex or pi thread driven over its protocol, which
    has a tile of its own and no terminal, was never one, though files and review comments
    already reached every thread. The target is now a thread (`WorkspaceView::block_target`
    returns a `ThreadId`): the agent's in that terminal, else that of the agent tile focused last
    on the same worker, a terminal on its thread or a thread's own tile. Only a thread whose
    agent is live takes one, since an exited thread's composer has given way to Resume.
  - *One way in.* Blocks, selections and review comments all go through `quote_to_thread`,
    which brings up whichever tile holds a composer of the thread, or opens one. The quote that
    waited on a terminal's face by its session (`quote_to_agent`, `QuoteFor`) is deleted.
  - Test: `workspace::tests::attach_block::{a_block_from_a_shell_lands_in_the_last_agents_draft,
    a_block_goes_to_a_thread_tile_whatever_its_agent}`.

- ✅ **A person's worktree is freed on their word** (2026-10-05, readiness 10-06 N13).
  - *Why.* A worktree started from "New agent…" stayed forever: the removal existed only for a
    project's tasks, and nothing on screen asked for it. A few parallel agents a day piled up
    `.claude/worktrees/*` and their `worktree-*` branches.
  - *Where.* "Remove this worktree" is offered while the focus works in an agent's worktree
    under its clone's `.claude/worktrees/`: a folder or changes tile in it, or a thread's or a
    review's tile whose agent works there. It asks `GitOp::RemoveWorktree` of the worktree's
    root, read from any folder in it. A shell is not a way in: one standing there is what keeps
    the worktree.
  - *Buttons* (2026-10-06). Where it is wanted at a glance, the removal is a button too, and it
    takes no shortcut of its own. A thread whose agent exited offers "Remove worktree" beside
    its way back (the exited strip, or the line over a composer that starts the agent again),
    once its agent worked in a worktree. A folder tile in one carries an icon in its path bar,
    named "Remove this worktree" for its tooltip and for VoiceOver. Both go the palette line's
    way, refusals included. A live agent's thread offers none, because nothing could free the
    worktree while it runs.
  - *Refused in words while it holds work.* Here, while an agent that has not exited works in
    it: an agent driven over its protocol has no terminal the worker could see. On the worker,
    as for a task (`repo::worktrees::remove`), while a terminal works in it or anything in it is
    not committed, with git's own lines. `git worktree remove` runs without `--force`, so git
    refuses whatever this missed. Ignored files go with the folder, as they do by hand.
  - *The branch.* It goes once every commit on it is in `origin`'s default branch or in the
    branch the clone has checked out (`git cherry`, so a rebase counts). A squash merge leaves
    no commit `git cherry` matches, so the branch also goes when the person's own `gh` says its
    pull request merged at the commit the branch ends at; gh is asked only when the commits do
    not settle it. Otherwise the branch stays and the notice says so, so no commit is lost.
    Refusing the removal while the branch holds unmerged commits was the other choice. It was
    turned down because the folder holds nothing the branch does not.
  - *Merge from inside the worktree.* The commit sheet's merge runs gh in the worktree. gh
    (2.102) merges, deletes the remote branch with `--delete-branch`, and skips the local branch
    checked out there with a warning, so the merge never fails on it; the removal then takes
    the branch, gh saying its pull request merged.
  - *After.* What went and what stayed is said in a notice ("Removed the worktree; kept its
    branch …, which holds work not merged"), and the folder and changes tiles in it close. A
    removal answered either way asks no new status of a folder that may be gone.
  - Tests: `slopty-worker`
    `repo::worktrees::tests::a_persons_worktree_is_freed_after_its_pull_request_merges_from_inside_it`;
    `slopty-ui`
    `workspace::tests::worktrees::a_worktree_is_removed_from_a_folder_in_it_once_no_agent_works_there`,
    `workspace::tests::worktrees::the_exited_thread_and_the_folder_in_a_worktree_offer_its_removal`,
    `workspace::worktrees::tests::a_worktrees_root_is_read_from_any_folder_in_it`; goldens
    `client_git_remove_worktree`, `worker_git_worktree_removed`.

- ✅ **The overview is a map of work** (2026-10-06, `.research/design-critique-astra-2026-10-05.md`
  finding 19 and bold idea 3). The miniatures made each tile look like itself, but a shell's
  or a conversation's text a few points high is texture, so the overview could not say why a
  workspace deserved opening. It now shows what each tile is doing and keeps the arrangement.
  - **A tile of words is summed up** (a shell, a conversation, a file, a folder, a review, a
    change set). Once the overview has landed, a well set into the block covers the tile's
    body: its kind's symbol or its agent's mark and its title at the task-title size (14 pt,
    medium), then three lines of facts (12/18 pt) and a quote of its own text. The well is the
    band, a hair off the block's surface in both variants, inset a hairline-width gap from its
    neighbours, so each tile reads as its own place with no outline round it. The block keeps
    its one ring.
  - **Three facts, never a blank.** A first cut said a name and at most two facts, and a review
    found its cards emptier than the miniatures they replaced: a shell said "Terminal" in a tall
    grey box. Each line now always says something, a quieter fact standing in where the first
    choice has nothing. How it stands comes first, in `text`, so the card reads state
    first; then what it did last and where it is, in `text_muted`.
    - A shell: at its prompt, running a command with its clock, or exited with its code; the
      command it ran last and how that ended ("cargo test · Exit 1"), else "No commands yet";
      its directory and branch with its working tree's changes, else its machine. A program
      run bare says "Running cat", then its arguments or its size.
    - An agent: its state by the rows' precedence (its question, failed, working, idle); its
      newest step or last line, else the agent's name; where it works.
    - A file: its kind and length ("Markdown · 5 lines"); its task list's progress ("1 of 3
      done"), else whether its edit is on disk ("Unsaved", "Changed on disk", "Saved"); its
      folder.
  - **The quote gives the card weight.** Under a hairline, in the mono face at the caption
    size and `text_muted`, up to six lines: a shell's last rows, an agent's last words, a
    file's first lines. A line the card cuts ends in an ellipsis, as the facts do. A shell's
    or agent's longer text stands on the card's foot so the newest line is never clipped; a
    short one hangs from the top as a fresh screen's does. It is the one chrome use of the mono face besides the settings file and an address,
    since it quotes a body whose own face is mono (`kit.rs`'s lint lists it).
  - **Copied, never read while drawing.** The quote and what only the text says (a file's
    length, kind and task list) are copied out of the body when the overview opens, and again
    while it shows when that body's facts change: a command starts or ends, a file's edit is
    saved or conflicts, an agent's table row moves. A line of output changes no fact, so a
    flood under a resting overview still draws no frame; what is quoted is as of the last
    command boundary. Closed, the overview lets the copies go.
  - **A tile known by sight keeps its picture:** a remote window or display, a page, a picture,
    a PDF, a film. It stays its live miniature with the thin label at its foot.
  - **Each workspace says what it holds** beside its name: the machines its tiles are on,
    unless that is its name already, then the first thing in it that needs the person ("Claude
    Code · Has a question"), after its rollup mark. The tile count is gone; the block shows the
    tiles.
  - **Covered output costs nothing.** While the zoom moves, each tile is its own body at the
    zoom, so the tile is seen shrinking into place. Once the overview rests, a body under its
    summary is not drawn at all; only the focused one is, as it keeps the keyboard. Held open
    over five floods, the overview drew 300 frames in five seconds at 1.3 ms p50 and now draws
    none (`docs/MEASUREMENTS.md`, "the overview as a map of work"). The price is the landing
    frame, which shapes every card's text anew: 7.1–7.5 ms at worst with twenty tiles, against
    5.6–5.8 ms for the old miniatures, still under a 120 Hz frame.
  - The words line up as before: a summary pads its glyph by `spacing.md`, and the workspace's
    name and "New workspace" start on that edge.
  - Tests: `workspace::tests::miniatures::an_open_overview_draws_a_miniature_per_tile_and_none_while_closed`,
    `workspace::tests::miniatures::a_summary_says_three_facts_and_quotes_its_text`,
    `workspace::tests::facts::overview_labels_say_state_place_and_worker`,
    `workspace::tests::strip_marks::the_overview_lifts_each_workspace_and_offers_a_new_one`; in
    `slopty-e2e`'s `smooth`, the overview scenarios (g), (i) and (m) now hold that a resting
    overview draws no frame for the output it covers. Goldens: `overview`, `overview-dark`.

- ✅ **The palette finds words said in any thread, on every machine** (2026-10-06, journeys
  audit J2, `.research/journeys-2026-10-06.md`). A thread's find bar could already ask its
  worker about turns it no longer held (`ThreadRequest::Search`), but nothing asked about every
  thread at once. The person had to remember which agent, on which machine, had talked about
  something. Now the palette's field asks it of every machine: a **Threads** section at the
  end of the list shows a line per thread where the words were said, with its title, its
  agent's mark and the words round the match. Choosing a line opens the thread at that turn
  (`PaletteRun::Thread`, `open_thread_at`).
  - **Asked once the field rests.** The workspace's own palette (`is_live`) waits
    `find::ASK_AFTER` (120 ms) after the last key, then asks each worker linked at that
    moment. It asks only for two characters or more (`find::ASK_FROM`) and never for a search
    of the commands alone (`>`). The find bar shares both constants. A worker searches 20 000
    items in under 5 ms (`docs/MEASUREMENTS.md`, "what a search of a worker's threads costs"),
    so the wait is not there to spare it. It turns a burst of keys into one ask.
  - **Out-of-order answers are dropped by their words.** Every answer carries the words it
    was asked for (`ThreadHits::query`), and it is kept only while those are still the
    field's. New words drop the last answers and the ask still waiting, so a slow machine's
    answer to an older keystroke never lands under newer words.
  - **An offline machine is not asked.** Nothing waits for it and no line or error stands in
    for it. Its threads come back the next time the field changes after its link returns.
  - **The list never moves under the person.** The section comes last, so answers arriving
    never shift a line above it. On a thread's line, the choice follows that thread wherever
    a later answer puts it. Each machine's threads keep its own order, best first, and the
    machines take turns, so no machine's best match waits behind another's worst. With two
    machines linked, each line names its machine.
  - Tests: `workspace::tests::palette_threads::the_palette_asks_every_linked_workers_threads_once_the_field_rests`
    and `workspace::tests::palette_threads::the_palette_asks_no_threads_for_commands_or_a_single_character`.

- ✅ **The overview holds work, not boxes** (2026-10-06, `.research/elegance-icons-2026-10-06.md`
  §5.8, `.research/status-color-2026-10-06.md` §7.2; amends "The overview is a map of work"
  above). The active block was lifted and then ringed by a hairline outside a gap (Geist's
  double ring). Inside it, each tile was a band-filled well with a rule over its quote: a box
  in a ring in a box.
  - **No ring.** The active block keeps `kit::elevate` alone: its hairline and its shade.
    Its name is in the text's ink at 13/500, and the others' names are in the secondary ink.
    `overview_ring` and `OVERVIEW_GAP` are deleted.
  - **The summaries have no fill of their own.** A summed-up tile sits on the block's content
    ground, padded `spacing.md` from its edges, so its words still start on the panes'
    glyphs' edge. Tiles are parted by the space their words keep. The quote sits
    `spacing.sm` under the facts with no rule, and a pictured tile's label has no rule over it.
  - **Cost.** No different. Drawing 20 mixed tiles with the ring, fills and rules back, under a
    temporary switch, gave the same opening frames (`docs/MEASUREMENTS.md`, "the overview
    without its ring").
  - Tests: `workspace::tests::strip_marks::the_overview_lifts_each_workspace_and_offers_a_new_one`
    (no edge rings either block, and no accent edge). The ring check is gone from
    `workspace::tests::tiles::the_overview_words_start_on_the_panes_glyphs`. Goldens:
    `overview`, `overview-dark`.

- ✅ **A new worktree starts current, from its base, with the ignored files it needs**
  (2026-10-06, `.research/readiness-2026-10-07.md` ranks 3, 4 and 5). A worktree made from
  `origin/HEAD` as last fetched was stale, lacked the person's commits not yet pushed, ignored
  which branch the folder had checked out, and was cut from the default branch for a project
  whose target is another; and it had none of the ignored files a checkout needs to run
  (`.env`, local certificates). `Start::worktree` is now a `NewWorktree { name, base }`:
  - *The base* is the branch `base` names: the project's target for a task's agent, the branch
    the clone has checked out for a person's start, `HEAD` for a detached clone. The worker
    fetches it from `origin` for at most ten seconds, then starts from `origin`'s copy when it
    holds every commit of the clone's own, else from the clone's branch: current, and nothing
    unpushed lost. A slow or unreachable remote costs at most the ten seconds; a branch neither
    side has is refused in words (`repo::worktrees::base_of`).
  - *`.worktreeinclude`*, in `.gitignore`'s syntax in the clone's root as Claude Code reads it,
    names the files a new worktree gets a copy of. Only those git ignores are copied: a tracked
    file is there already, and an untracked one git does not ignore would show as a change.
    Git does the matching (`ls-files --others --ignored`, once with the standard excludes and
    once with the include file), so the syntax is git's own. Nothing is overwritten and a copy
    that fails is passed over: the worktree is made either way (`carry_ignored`). Orca and
    Claude Code do the same.
  - *A Claude Code task* too: the server sends `Verb::SpawnAgent::worktree` beside the
    `--worktree <name>` it puts in the arguments, and the worker makes the worktree first, so
    Claude Code's own flag opens it rather than making one. Claude Code reopens an existing
    `.claude/worktrees/<name>` (its worktrees page, "Reuse a worktree name"), and it makes its
    own from `origin`'s default branch fetched at most daily, never from a project's target.
    One it did not make should keep its tip, as one whose state it cannot verify; were it
    reset to the default branch instead, the task would start where it did before this. A
    Codex task still runs `codex --worktree`.
  - Tests: `repo::worktrees::tests::a_new_worktree_starts_current_and_carries_the_ignored_files_it_names`,
    and the project task spawn test in `hub::project_tests` for the Claude Code task's worktree.

- ✅ **Tiling replaces the scrolling strip: projects hold tabs, each tab holds a split layout**
  (2026-10-06, the person's ruling; `.research/tiling-2026-10-06.md`). After long work on the
  strip, the person ruled it out in favour of tiling, the way the leading tools of the day arrange
  their work. The study weighed Zed's and VS Code's shape (splits whose panes are tab groups)
  against the shape MonoCode, Warp, iTerm2, Ghostty and cmux share (tabs of layouts). It took
  tabs of layouts: an agent's work is one tab, the title bar's tab strip says which agents work
  and which are done, and a tile that arrives from elsewhere becomes a background tab rather than
  reshaping a layout.
  - **The tree.** Each tab holds an n-ary split tree whose leaves are panes. A pane holds one
    tile, or several as its own tabs. Shares sum to 1. The tree is normalised after every edit:
    no single-child split, no split directly in a split of its axis, and no empty pane. It is a
    pure model in `slopty-client::layout`.
  - **Projects own their tabs.** Switching a project brings back the tab it was left on. The
    stacked workspaces, the overview, its miniatures, the column thumb, the preset widths,
    centring, fullscreen tile, consume and expel, and every swipe and wheel step are deleted.
  - **Panes meet edge to edge** on one ground, at a 1 pt sash, as MonoCode's do
    (`.research/monocode-system-2026-10-06.md`, geometry). The study's "panels and gutters
    kept" is overruled: panels on a canvas were the strip's look.
  - **The navigator is the only dock.** Review, diff, terminal and page are panes.
  - **Where new work opens.** ⌘T opens an agent's draft in a new tab and ⌘⇧T a terminal. ⌘D and
    ⌘⇧D split a terminal off. ⌘⌥T shows or hides the tab's terminal pane, which is a pane in the
    tree, not the drop-down deleted on 2026-09-30. What opens from a tile goes beside its source
    by the room rule (right while panes keep 520 pt, else down while halves keep 300 pt, else a
    tab).
  - **Zoom** (⇧⌘↩) replaces Focus mode. ⌘⌥ and an arrow move the focus, ⌘⌥⇧ and an arrow move
    the tile, ⌘1–9 pick a tab, and ⌘[ and ⌘] go back and forward. A drop within a fifth of a
    pane's side splits it; one in the middle joins its tabs.
  - **iPhone** shows one pane at a time. **iPad** splits by the touch minimum (480 pt).
  - An old `layout.json` is set aside unread (pre-release). The per-worker item cache is
    unchanged. No wire type changes.
  - Before it lands, a sash drag beside five flooding shells is measured against today's
    divider drag, with a budget of no frame over 8.3 ms. The ten-step migration is in the study,
    §4.2.

- ✅ **The panes are drawn and the strip is gone** (2026-10-07, step 3 of the study's §4.2).
  The workspace draws the tab on show as its panes, each at its rectangle in the tree's frame,
  with the sashes over their edges. The title bar draws the shown project's tabs. The strip, its
  column marks, the overview and its miniatures, the swipe tracker and the springs are deleted,
  with their tests. The keys for columns and workspaces went with them (step 4's deletions). What
  the drawing taught:
  - **A project takes the group its tiles turn out to share.** A project made at a machine's
    home, before its shells said which repository they are in, moves to that repository's group
    once every tile in it shares one. What arrives of the repository then joins it. A project
    holding a machine's own work beside a shell keeps its home
    (`Tiling::rehome`, `WorkspaceView::rehome_projects`).
  - **An emptied project gives the window back and goes.** When a project's last tab closes,
    the window shows the tab visited before it, else the first project with tabs. An emptied
    project with no name of its own is removed; a named one stays for its next tab.
  - **A zoom ends when another pane takes the focus**, by click, key or navigator row, so the
    focus never lands on a pane the zoom hides.
  - **The title bar's tabs are a view of their own.** A working spinner on a tab redraws that
    view alone. The title bar is no longer cached: a cached view is rebuilt whole when any view
    inside it is, so a cached title bar rebuilt itself on every spinner step.
  - **The focus is read after the panes are built.** Each body decides whether it can replay
    from the focus it was last drawn with. Setting the new focus before the bodies were built
    replayed the two tiles the focus had moved between, and their headers showed the old focus.
  - **A sash drag builds each shell at most twice a step.** A changed row count is drawn in the
    frame after the one that measured it (`TerminalView::fitted`), so a shell the drag resizes
    is built once for the new width and once for its rows. The retained oracle checks the panes
    drawn after each step of a drag against the same state drawn from scratch.
  - **At the e2e window (900 × 600) there is no room beside a tile**, so what opens from it is a
    tab of its pane. The live tests read a tile's place from the dump as its project, its tab
    and its pane in reading order, and the overview's field is gone. The step-9 dump (a pane's
    path in the tree and a tile's index in its pane) replaces this interim one.
  - Tests: `layout::tiling::tests::a_project_rehomed_keeps_its_tabs_and_takes_no_others_home`,
    `a_project_emptied_hands_the_window_back_and_goes`, the zoom test in the same file,
    `workspace::tests::retained::a_sash_drag_is_drawn_as_from_scratch_and_builds_each_shell_once`,
    `the_pane_keys_are_drawn_as_from_scratch`,
    `chrome::a_sash_drag_is_no_news_for_the_chrome`,
    `bodies::a_double_click_on_a_sash_makes_its_panes_equal`,
    `tiles::a_pane_of_tabs_draws_a_tab_per_tile`, and `rooms::nothing_escapes_its_tile_at_any_room`
    at every pane width. The cost of a sash drag is in `MEASUREMENTS.md` (2026-10-07).

- ✅ **The tab and pane keys** (2026-10-07, step 4 of the study's §4.2). ⌘T opens an agent's
  composer in a tab of its own. It starts on the focused tile's machine, in its folder, else
  where that machine last worked, with the agent last started there. "New agent…" keeps its
  steps for a pick across machines and has no key of its own. ⌘⇧T opens a terminal in a tab of
  its own. ⌘D and ⌘⇧D split a terminal off the focused pane, right and down. ⌘N is no longer a
  second "new shell". An editor's ⌘D (the next occurrence) is bound deeper and wins in its
  text. The palette adds "Other tabs…", "Close other tabs" and "Move to project…", each
  offered only while it would do something.
  - **A shell goes where its ask said once its item comes.** The worker makes a shell's item,
    so its place is decided when the item arrives, not when it is asked for. Each ask is queued
    on its worker with where it goes, and the shells this client asked a worker for come back
    in the order they were asked. An ask the worker refuses (`WorkerMsg::Failed` under its
    request) leaves the queue, and a link that drops takes its asks with it, so one lost answer
    never shifts every later shell into the wrong place.
  - Tests: `workspace::tests::new_shells_open_in_a_tab_or_split_off_the_focused_pane`,
    `a_refused_shell_leaves_the_next_one_its_own_place`, and
    `tab_commands::{cmd_t_starts_an_agent_in_a_tab_of_its_own_where_the_focus_works,
    other_tabs_lists_the_tabs_and_close_other_tabs_keeps_the_one_on_show,
    move_to_project_takes_the_tile_to_a_tab_of_the_project_picked}`.
- ✅ **Carrying a tile or a tab past the area** (2026-10-07, step 5 of the study's §4.2). A
  pane's header or tab, a navigator tile row, or a title tab is pressed and then moved 4 pt.
  From then on it carries its tile, or for a title tab the whole tab.
  - Where it can land:
    - On a pane (tiles only), as before: it splits the pane or joins its tabs.
    - On the title strip, a tile becomes a tab of its own and a title tab moves. Either lands
      before the tab whose middle lies past the pointer, and a 1.5 pt focus mark stands at
      that edge.
    - On another project's row in the navigator, either goes to that project and that project
      shows it. Over its own project's row it lands nowhere.
    - A tile alone in its tab, dropped on the strip in its own project, moves that tab rather
      than making a new one.
  - **One pointer follow for the whole window, on the capture phase.** The listeners left the
    area's div for a canvas at the workspace's root. It registers capture-phase move and
    release listeners in every frame, as the navigator's width handle does. So nothing under
    the pointer can keep a move from the drag (a terminal, the title bar's window drag, a
    list), and the first move after a press is followed without waiting for a frame. While
    nothing is pressed, each move costs one read of the drag state.
  - **The chrome says where it takes a drop as it lays out.** The title strip, each title tab
    and each project row record their window bounds with a canvas prepaint. The records go
    into shared `DropSpots`, so the drop logic stays out of the drawing code and reads no
    layout of its own.
    - The cached chrome views reuse their last paint, and with it the bounds they last wrote.
    - A surface that stops drawing clears its own record: the title bar when it shows no tabs,
      the frame when the navigator is not drawn. Records left behind can therefore never take
      a drop.
    - A finger's move in the navigator scrolls its list, so on touch density a tile row is not
      carried.
  - Tests: `workspace::tests::drag::{a_header_dropped_on_the_title_strip_is_a_tab_where_it_fell,
    a_title_tab_carried_along_the_strip_moves,
    a_tile_row_dropped_on_a_project_row_moves_the_tile_there,
    a_title_tab_dropped_on_a_project_row_moves_there_whole}`, and
    `slopty_client::layout::tiling::tests::{a_tile_dropped_on_the_strip_is_a_tab_where_it_fell,
    a_title_tab_moves_along_the_strip, a_title_tab_moves_whole_to_another_project}`.
- ✅ **A project's row goes to the project** (2026-10-07, step 6 of the study's §4.2). The
  navigator's project header was a fold toggle. Now a click on it shows the project on the tab
  it was left on, as the rail's project button and the breadcrumb's project menu already did.
  The chevron, which shows under the pointer and stands at rest on touch, is now a button of
  its own that folds; the row's context menu folds too.
  - A project with no tile here, whose agents' threads run elsewhere, opens an agent's
    composer in a tab of its own in that project, in its place on the machine. A row then
    never ends in "has no tile here" while the project has a place to start in. One with no
    place on a linked machine still says so.
  - A tile's row goes to its project, its tab and its pane. Carried to a pane's edge, it splits
    that pane (step 5).
  - Tests: `workspace::tests::nav_projects::{a_project_row_shows_its_project_on_the_tab_it_was_left_on,
    a_tile_row_goes_to_its_tab_and_pane, a_tile_row_dragged_to_a_panes_edge_splits_it,
    a_project_row_with_no_tile_here_opens_a_composer_there}`. The fold tests in `nav_rows.rs`
    fold by the chevron.
- ✅ **The tab's terminal** (2026-10-07, step 7 of the study's §4.2). ⌘⌥T, "Show or hide the
  tab's terminal" in the palette, and Layout ▸ Tab Terminal in the menu bar control it.
  - The first press asks for a shell on the focused tile's machine, in its folder. That shell
    becomes the tab's terminal: a pane below the whole tab, a third of its height, focused.
  - Later presses put it away, with the focus back on the work, and bring the same shell back.
    Its shell runs on while it is hidden.
  - A shell is the worker's to make, so the ask waits in its worker's queue as `Opening::Terminal`.
    Presses before the shell arrives ask for nothing more.
  - The tab it joins is the one on show when the shell arrives. If no tab is on show by then, it
    becomes a tab of its own.
  - Zoom already replaced Focus mode in step 3. Fullscreen tile went with the strip's keys in
    step 4.
  - Tests: `workspace::tests::tab_commands::{cmd_alt_t_shows_hides_and_shows_the_same_shell,
    a_zoom_fills_the_tab_and_both_come_back}` (the zoom puts the docked navigator away and both
    come back), and `layout::tiling::tests` for the terminal's toggle.
- ✅ **The phone shows one pane; the iPad splits by the touch room** (2026-10-07, step 8 of the
  study's §4.2).
  - **Constants by device.** On iOS the tiling takes `TilingConfig::TOUCH`: panes of the touch
    minimum (480 pt), and a phone's model below 900 pt. A Mac takes `TilingConfig::POINTER`
    (520 pt panes, a phone's model only below 700 pt).
    - A 1024 pt iPad therefore opens a shell beside the focus in a pane to its right, where a
      pointer's minimum would put it below.
    - Split View narrower than 900 pt is drawn as a phone. The navigator's modes already used
      the same threshold, so the layout, the bar and the navigator change model at one width.
  - **A phone's title is the way to what it does not draw.** The focused pane fills the tab.
    While the tab has other panes, or the project other tabs, the title gains the breadcrumb's
    chevron. It opens "Panes and tabs": the tab's panes by their shown tile, then the project's
    tabs, with the focused pane and the tab on show ticked. A pane picked is focused and drawn;
    a tab picked is shown.
  - **Menu keys stay unique.** Two rows of one name, such as two shells in one folder, get keys
    of their own, so a bar menu never holds two rows with one id.
  - **Deferred: lifting a header or a tab with a long press on iPad.** A long press there
    already opens the tile's menu, as iPadOS's context menus do. A press then a move already
    carries a header or a tab, as it does with a pointer.
  - Tests: `workspace::tests::touch::{a_phone_shows_one_pane_and_its_title_goes_to_the_others,
    an_ipad_splits_by_the_touch_room_and_in_split_view_shows_one_pane}`. The iOS goldens are
    retaken in the simulator lane.
- ✅ **The dump names projects, tabs and panes; the smooth suite drags a sash** (2026-10-07,
  step 9 of the study's §4.2).
  - **Item places.** An item's place in `ItemInfo` is now its project, its tab, its pane's path
    in the tab's split tree (a child's index at each split), and its place among the pane's
    tabs. This replaces step 3's interim index of the pane in reading order. `Dump.shown` names
    the project and tab on show.
  - **Test helpers.** Tests compare places through `ItemInfo::place`, `same_pane` and
    `same_tab`, and count panes with `Dump::panes_on_show`. A tree path says which pane is the
    left one, where a reading-order index changed whenever a split was made above it.
  - **PTY size counts.** `TerminalInfo.resizes` counts the PTY sizes each shell asked the worker
    for (`TermState::resizes`), so a drag's cost to the PTYs is a number.
  - **New test-socket commands.** `Command::Press`, `DragTo` and `Release` hold a drag across
    several commands, which a sash drag held for seconds needs. `Drag` keeps the one-shot form.
  - **Live tests brought to the panes.** `settings` presses only inside the Settings dialog,
    since a pane's own "Terminal" tab shares a section's name. ⌘⇧T's shell is checked as a
    title tab of its own. The folder test reads its pane's two tabs. The palette test types for
    "New note", since an empty field lists the tiles first (`palette::brief`).
  - Tests: `smooth::a_sash_drag_beside_five_flooding_shells_on_the_mac` (numbers in
    `MEASUREMENTS.md`, 2026-10-07), with the app suite's tests above.
- ✅ **The title bar goes back and forward, and a title tab has its own menu** (2026-10-07,
  items 15 and 23 of `.research/ui-audit-monocode-2026-10-06.md`; amended 2026-10-10 by
  ui.md, "The MonoCode port: one pill, one line, bars on the ground": while the navigator is docked the arrows and the toggle sit in
  its top row, and the breadcrumb and the bell leave the bar).
  - **Back and forward.** Two arrows after the navigator's toggle step through the tabs visited
    (⌘[ ⌘]), as `MonoCode`'s `TabVisitNav` does. The pair always stands, so the tabs after it
    never move. A way with nowhere to go is drawn in the muted ink and takes no press. The
    tiling answers whether a way leads anywhere (`Tiling::can_go_back`) by the same walk
    `go_back` takes, past the tab on show and the tabs since closed.
  - **A title tab's menu.** A right click or a long press on a title tab opens Close tab, Close
    other tabs, Close tabs to the right and Close tabs to the left, each only while there are
    tabs that way. Each closes what the tabs hold as ⌘W does, so a running shell still asks
    first, and the tab pressed is left on show.
  - **Not taken: Archive.** `MonoCode`'s tab menu archives its sessions. Slopty has no archive:
    an agent's own session is the source of truth, and closing its tile leaves it there to come
    back to (*Recent threads* below, "Resume a past session…").
  - Tests: `workspace::tests::context_menus::a_title_tabs_menu_closes_the_tabs_beside_it` (the
    menu, then the arrows), and `layout::tiling::tests::back_and_forward_retrace_the_tabs_visited`.
- ✅ **The palette splits three ways, and opens on the threads worked in lately** (2026-10-07,
  item 10 of the MonoCode audit with readiness R17, drawn to the Zed and Warp amendment's
  palette, `.research/zed-warp-system-2026-10-06.md` row 8 and ranked item 9).
  - **Three keys.** ⌘K searches everything: the tiles, the threads worked in lately and the
    words said in any thread, the projects, the machines, the files and the commands. ⌘⇧P is the
    same search opened at `>`, so it lists every command and nothing else; ⌫ widens it to
    everything. ⌘P searches the files alone: the ones open in tiles at once, then what is typed,
    asked of the files under the focused tile's directory on its machine. "Open folder…" keeps
    the palette seeded with that directory. The View menu has Search… and Commands….
  - **⌘K inside a terminal still clears it.** Terminal.app, iTerm2 and Ghostty all clear on ⌘K,
    and the terminal stays the best there is, so its deeper binding wins there. From a terminal
    the search is ⌘⇧P then ⌫, the title bar's Search, or the View menu.
  - **Esc on what it was opened with closes.** Esc empties a typed query first, as before. A
    field that still holds only what the palette was opened with (`>`, a folder) closes at once.
  - **Recent threads (R17).** With nothing typed, the search lists up to five threads, newest
    first, under *Recent threads*: the linked machines' top-level threads that no tile shows.
    ↩ opens one where it stands (`PaletteRun::Thread` with no turn).
  - **The frame, from Zed and Warp.** 640 pt wide (Warp 640, Zed 608) where the audit said 560;
    the later amendment wins. It sits 12 % down the window (Warp 117 pt, Zed 80 pt), not a fifth.
    Its query row is 36 pt over a hairline, and its rows are 28 pt. The pickers share the width,
    the anchor, the query row and the rows. It still has no scrim on a desktop. The opaque float,
    its ring and its shadow are the kit's.
  - **Not done here: motion.** The audit's ranked item 7 opens menus, popovers and the palette
    on the first frame. A pointer's palette still fades in until that lands for every float at
    once.
  - Tests: `workspace::tests::palette::{the_palette_splits_into_everything_commands_and_files,
    the_palette_hangs_near_the_top_and_is_a_sheet_on_a_phone}`, and the keymap's
    `a_chords_words_come_with_the_keymap` and `the_files_chord_wins_over_a_deeper_default`.
- ✅ **A navigator row says its state in words, at the second line's end** (2026-10-07, item 11
  of the MonoCode audit, `MonoCode`'s session card).
  - **Where.** The title has the first line to itself, so a long one is never cut for the
    state. The second line ends in the pull request, the changes, then the state: its glyph and
    its word in the word tone (`Status::word`): "Needs approval" or "Has a question", "Working",
    "Failed", "Away", and "Done" in the finish's green check for one not yet looked at. A running
    command's clock stands before its glyph and says it in place of a word. With no state the
    age is there.
  - **Why the second line.** The 2026-10-02 showcase ruling took the word off the title's line,
    where "Needs approval" took half a 240 pt row. `MonoCode` puts its status on a line of its
    own, away from the title, and so does this. The muted words before it give way first.
  - **The close.** Under the pointer the title's line ends in the row's close, over nothing,
    so neither line moves.
  - **Not taken: the model on the second line.** It is in the composer's foot and the agent's
    meters, and a 248 pt row has no room left for it beside the state's word.
  - Tests: `workspace::tests::nav_rows::{a_tile_row_reads_its_age_or_its_state_then_its_place,
    a_tile_row_leads_with_its_state_and_closes_from_under_the_pointer}` and
    `agent_tile::an_agents_tile_leads_with_its_mark_and_ends_with_its_state`.
- ✅ **A project's head shows its changes, and pins and mutes it** (2026-10-07, item 17 of the
  MonoCode audit, `MonoCode`'s project card).
  - **Changes.** The head ends in what its working trees have added and removed (`+12 −3`,
    the kit's figures). Each checkout counts once, keyed by machine and repository, however many
    of its shells are listed. They give way under the pointer to the fold and "+", as the rest
    of the end does.
  - **Pin.** "Pin to top" in the head's menu puts the project above the rest, the pinned in the
    order they were pinned, then the others by name. "Unpin" puts it back. A quiet pin glyph
    after the name says it.
  - **Mute.** "Mute notifications" stops the project's moments from posting a system
    notification here, its waits, its finished turns and its server notices alike. The bell,
    *Needs you* and the navigator still say them. A quiet bell-off glyph after the name says it.
  - **Saved.** Both are kept per device with the navigator in `layout.json`
    (`Navigator::pinned`, `Navigator::muted`), by the project's group key.
  - Two Tabler glyphs come in for them: `pin` and `bell-off`, redrawn at 1.75.
  - Tests: `workspace::tests::nav_projects::a_projects_head_shows_its_changes_and_pins_and_mutes_it`,
    `workspace::attention::tests::a_muted_projects_moments_post_nothing`, and
    `layout::tests::the_tiling_and_the_navigator_are_saved_and_restored`.
- ✅ **A thread's row warms on its press and drops on a pane's edge** (2026-10-07, item 12 of
  the MonoCode audit, `MonoCode`'s session card).
  - **Warm on the press.** Pressing a navigator row of a thread with no tile here follows the
    thread at once (`ThreadHub::open`), so the tile the click opens finds it on its way and
    draws it from the first frame. The hold lets go at the next press or after 10 s
    (`WARM_HOLD`); by then the tile's own view holds the thread.
  - **Drop on a pane.** Pressed and moved, the row carries the thread (`Carried::Thread`). Over
    a pane of the tab on show it washes where it would land, as a tile does. Let go there, its
    tile opens (the live terminal's where its agent runs in one) and lands at the drop once the
    worker's list has it (`Opening::At`, kept per worker until the item comes). A thread whose
    tile is already here moves that tile. The title strip and the projects' rows take no thread.
  - A tile's row already carried its tile onto a pane's edge (step 5).
  - Tests: `workspace::tests::drag::a_threads_row_warms_on_its_press_and_drops_on_a_panes_edge`.
- ❌ **The title tabs fade where they run past the bar, and are set at the chrome's size**
  (2026-10-07, after the goldens retake; superseded 2026-10-10 by ui.md, "The MonoCode port: one pill, one line, bars on the ground":
  chevrons, decided at prepaint, and no fade).
  - **Why.** The gallery's transfers frame was stale: the title strip drew its "Later tabs"
    chevron only from what the last frame laid out (`ScrollHandle::max_offset`, read in
    render). The frame in which the tabs first ran past the bar laid them out without it, and
    nothing drew the strip again, so the frame drawn from scratch had the chevron and the
    frame shown did not. Not a gpui-fast bug: the view read layout state that changes after
    its render.
  - **What.** The chevrons are gone. An end past which tabs lie fades out as deep as they run
    past it (`gpui::edge_fade(..).hidden_by_scroll`), as a pane's tabs already did. The fade
    reads the scroll as the strip prepaints, so it is right in the frame that lays the strip
    out. The strip still scrolls under the wheel and a trackpad.
  - **Size.** A title tab is set in the chrome role (13/19), the shown one in the action role
    (13/19 medium), as the breadcrumb beside it. It had inherited the window's 16 pt. The
    Zed/Warp system (item 3) names the tab's tone and weight, not its size.
  - Tests: `workspace::tests::retained::title_tabs_that_overflow_are_drawn_as_from_scratch`.
- ✅ **A list steered from a field marks its row with the fill alone** (2026-10-07).
  - The palette's and the pickers' selected row is the keyed wash with no focus line
    (`Plate::plain`), as `MonoCode` marks its palette's row. The field has the keyboard and its
    own ring, so a second green line round the row only shouted, worst on the phone. A list
    that holds the keyboard itself (the navigator) keeps the line, its keyboard's cursor.
  - Tests: `palette::tests::the_selected_line_is_the_fill_alone`.
- ✅ **The phone's palette is a sheet with its field at the foot, over a scrim** (2026-10-07,
  checked after the retake).
  - The field sits at the sheet's foot, just above the soft keyboard, and what it finds lists
    above it. That is where the thumb already is when the keyboard is up, as iOS's own bottom
    search fields (Safari, Spotlight) put it. The field at the top would sit a hand's
    length from the keys it is typed with. The empty ground under the sheet in
    `ios-phone-palette` is the keyboard's place: a render holds the app's window, not the
    keyboard's.
  - The phone keeps the scrim. The desktop's palette is a float typed at, with no scrim. The
    phone's is a presented sheet nearly the screen's height, and the dimmed strip above it is
    both iOS's sign of a presented sheet and the one place a finger taps to close it, since
    glass has no Esc.
- ✅ **A file opened in passing is a preview tab** (2026-10-09, MonoCode audit row 14).
  - A file opened from a thread's call, a folder's tree, a review, a path in a shell or a search
    hit opens as the preview, its name in italics. The next file opened that way takes its place:
    it joins the old one's pane right after it, and the old one goes, so reading through a turn's
    files leaves one tab, not a row of them. MonoCode's `layout.ts` `preview` does the same, as
    does VS Code.
  - An edit, a double-click on its name, or naming it keeps it. A double-click on a kept tab's
    name still names it. A file picked on purpose (the palette, a new note, a shell's handoff,
    a machine's settings, the self-test) opens kept, and keeps a preview it lands on.
  - Only a preview in the tab on show is replaced. Opening a file never reaches into a tab out
    of sight; a preview there stays, and the new one opens beside the focus.
  - The preview is the client's own and is not saved with the layout: after a relaunch every
    tab is kept.
  - Tests: `workspace::tests::preview::a_file_opened_in_passing_takes_the_previews_place`,
    `workspace::tests::preview::a_preview_out_of_sight_stays_where_it_is`.
- ✅ **The title tabs are drawn in the frame that lays them out, all at once too** (2026-10-09).
  - Three scheduled e2e frames were stale in the title tabs: a forwarded port and an upload,
    a relaunch onto twenty tiles, and files pasted into a shell. All three had one cause. The
    "Later tabs" chevron was shown from the scroll as the last frame laid it out, so the frame
    where the tabs first ran past the bar lacked it, and the strip's clip, the cut tab's fill
    and its glyphs differed from a frame drawn from scratch. The fade that replaced the chevrons
    resolves at prepaint (see the entry above).
  - Tests: `workspace::tests::retained::title_tabs_that_overflow_at_once_are_drawn_as_from_scratch`
    (twelve tabs in one snapshot, as a relaunch lays them out, then a narrower window). It and
    the one-by-one test fail when an element sized from the last frame's scroll is put back.
- ✅ **A note's yes or no goes through the server while its worker is not linked here**
  (2026-10-09, the client half of `agents.md`'s "A yes or no waits for a pocketed phone").
  - A pushed note's Allow or Deny wakes the app, and its link to the worker can take seconds to
    come up, longer than the system lets a background tap run. While that link is down and the
    server's ladder has the thread waiting on the person, the tap is answered through the
    server and does not wait for the worker. `Verb::ReadThread` reads the request's choices,
    and `Verb::AnswerRequest` answers with the plain allow or deny, once
    (`slopty_proto::thread::once`). A request the server no longer finds is said as one no
    longer waiting. The app is told it may sleep (`TapsSettled`) only once the answer is out.
  - With the worker linked, the answer goes through its thread hub as before.
  - Tests: `workspace::tests::approvals::a_notes_answer_goes_through_the_server_while_its_worker_is_away`.
- ✅ **An orchestrator's helpers open beside it, and their rows sit under its row**
  (2026-10-09, MonoCode audit row 21, M27 `queueWorkerPanes` and
  `OrchestrationSidebarAgents`). Superseded on 2026-10-10 by "Task agents are rows, not
  tiles" in `ui.md`: a helper no longer takes a pane; only the navigator's nesting of one the
  person opened stays. `Tiling::arrive_beside` and its test were deleted on 2026-10-11.
  - **Their tiles.** Any other tile from elsewhere is a background tab of its project. A
    task's agent started by an orchestrator instead takes a pane right of the orchestrator's
    tile, in that tile's tab. The next ones join that pane as its tabs, so a lead with many
    helpers keeps one column beside it rather than slivers
    (`slopty_client::layout::Tiling::arrive_beside`, `workspace::seating`). The person's focus
    stays where it was, in that tab and in the workspace.
  - **When the server names it late.** A tile can come before the project says it is a
    task's agent. It waits alone in its background tab and is seated with the rest once the
    project names it. A tile the person has shown, moved or given company is theirs, and the
    naming moves nothing. Tiles restored at a relaunch are never moved.
  - **On a phone** one pane is on show, so every arrival stays a tab.
  - **In the navigator** a helper's row follows its orchestrator's row, set in one more step
    with its own guide under the lead's glyph. One level only: a helper that orchestrates
    keeps its own helpers flat.
  - **Not done here.** The plan that starts the workers is a card with "Confirm & start".
    That belongs to the thread's plan card (`conversation/`), which is lane W's.
  - Tests: `workspace::tests::seating::*`, `slopty_client`'s
    `layout::tiling::tests::a_leads_helpers_arrive_beside_it_in_one_column`.
- ✅ **A path typed into the folder step completes from the machine's folders** (2026-10-09,
  readiness R14, its first half).
  - Typing `~/w` or `/Users/c/work/sl` where a start asks for its folder lists, after the line
    for the path as typed, up to twelve folders of the path's parent that begin with its last
    part, in any case (`workspace::folder_typing`).
  - The parent is asked of the machine once per step with `ClientMsg::ListFolder`, as a folder
    tile asks, and the lines follow its answer. Typing on in the same folder asks nothing more.
  - Hidden folders show only for a part that begins with a dot. A trailing `/` lists the
    folder's own folders.
  - Cloning a repository known on another machine is its second half, below.
  - Tests: `workspace::tests::folder_typing::a_typed_path_completes_from_the_machines_folders`;
    `workspace::folder_typing::tests::*`.

- ✅ **The folder step clones a repository another machine has** (2026-10-09, readiness R14,
  its second half; the wire is `projects.md`'s "A client asks a worker for a clone directly").
  - **What is offered.** After the worktree lines, a line "Clone `origin` into `~/…`" for each
    repository whose clone another machine's shells stand in or threads work in, and that no
    checkout known on the step's machine is (by origin, else first commit). The address is
    the checkout's credential-free `RepoId::url`; a repository with none, or no origin, is not
    offered. One line each: the shortest root found wins, so a clone is taken over its
    worktrees. It goes to the same place under the home there (`~/work/slopty` for
    `/Users/l/work/slopty`), else to the home under its own name (`workspace::clone_here`).
  - **The step.** Picked, the machine is asked (`ClientMsg::CloneRepo`) and a step opens at
    once saying "Cloning … into … on studio…", then git's own phase and percent as the machine
    says them; only the latest counts. When the clone is there the step goes and the agent's
    start opens in it, from where the keyboard was, as a pick of that folder would. A refusal
    or git's failure, and a link lost on the way, are said in the step; one that comes after
    the step was put away is a failure notice. A step put away first starts nothing: the
    clone that lands then is said as a notice, so its place is known. Asked again, the machine
    answers with the clone it finds there.
  - **Why a step rather than a tile at once.** The start's tile opens with the agent's composer,
    and the agent runs in a folder that does not exist until git is done. A step that says how
    far git is, then the usual start, keeps the start as it always is.
  - Tests: `workspace::tests::clone_start::*` (offered, asked, said at once and as it goes,
    started in the clone; refused; the link lost; a late clone with its step gone);
    `workspace::clone_here::tests::*`.

- ✅ **"Resume a past session…" asks every machine** (2026-10-09, readiness R25; closes
  `agents.md`'s "Not yet: a search of every machine's prompts at once").
  - **What.** The session step opened from a machine's folder step asks that machine and every
    other one up that has the agent, with no words and then for the field's words once it
    rests. The step's own machine's sessions lead; a session on another names it after its
    folder ("src/web on laptop"). The sessions found for the words come first, from every
    machine, then the listed ones they leave out. Picked, a session is taken up on its own
    machine.
  - **Why.** The person remembers what was asked, not where. A session started on the laptop
    was unreachable from a step opened on the studio, and the person had to guess the machine
    first.
  - **Empty states** say "No machine has past … sessions" and "No past … prompt on any machine
    says that" once several were asked; with one, they name it as before. The step reads while
    any machine that has not said it has none is still answering.
  - Test: `workspace::tests::past_sessions::a_past_session_is_found_on_every_machine`.

- ✅ **Run opens the repository's own run scripts** (2026-10-10, readiness rank 15, the UI half;
  the wire is `projects.md`'s run and archive scripts entry).
  - **Where.** "Run" is offered in the palette where the focus works in a folder: a folder or
    changes tile's folder, a terminal's directory, or where a thread's agent works. It asks
    that machine for the scripts of the checkout there (`GitOp::Scripts`).
  - **One or several.** One script opens at once in a terminal beside the focus, with the
    script's own command, folder, environment and name as `OpenSession` carries them. Several
    are a step that lists each by its name, its script muted beside it, the default first.
  - **Local feel.** The scripts read last for that folder answer at once while the machine
    reads them again, so a second Run is a keystroke. A step that showed the last read takes
    the fresh list in its place and opens nothing the person did not pick. With none read yet,
    the step says "Reading the run scripts…" until the answer comes, then lists them, opens the
    one there is, says the repository keeps none, says why they could not be read, or says
    the machine went out of reach.
  - **No button.** The palette is where actions live, and the thread and folder headers stay
    quiet; a Run button would earn its place only once a script's terminal is something the
    person watches there.
  - Tests: `workspace::tests::run_scripts::*` (offered, asked, the reading step, a list
    picked and opened with its spec, the last read at once and the fresh list in its place;
    one opening at once, none, a refusal, the link lost); the keyboard settings golden lists
    its row.

- ✅ **What the workspace sends a worker is never dropped on a full stream** (2026-10-10,
  readiness audit item 2).
  - **The defect.** `Worker::send` put each message on the link's bounded control stream
    with `try_send` and reported it sent even when the stream was full. The terminal and
    screen views fill that same stream on purpose while they stream, so a burst of output or
    input could drop a thread's intent (Send, Answer, Interrupt), a follow, a start, a
    terminal's attach, a file's save, a watch set or a screen's close. The UI then showed
    "saving" or "watched" for a message that never left. The thread outbox resends only on a
    relink, so nothing put it right.
  - **The fix.** Each link gets an outbox (`crate::outbox::Outbox`, taken out of the screen
    view, which already had one for its input). A message that finds the stream full waits
    behind what already waits, and a task on the UI executor moves it in, in order, as room
    frees. "Sent" now means queued, so the "saving" and "watched" marks stay true. The outbox
    goes with the link; the next link sends the watch sets, attaches and follows afresh, as
    before.
  - **Latest value only where only the latest matters.** A whole watch set (files or
    folders), the handoffs taken, a liveness probe, and a terminal's or a display's size take
    the place of the waiting message of their kind, at the back (`outbox::hold_control`).
    Nothing else is dropped, however much waits: the link's own deadlines end a stream that
    never drains. The screen view keeps its own policy, which drops input past 256 waiting
    unless that input lets go of something.
  - Tests: `outbox::tests::what_waits_keeps_everything_but_an_outdated_latest` and
    `workspace::tests::outbox::a_full_control_stream_holds_what_must_arrive_in_order` (a stream
    filled to its depth, then an attach, a save, two watch sets and a close: all arrive in
    order after the fill, the first watch set outdated; a worker down reports nothing sent).
