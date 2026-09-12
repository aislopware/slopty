# Decisions — Canvas

See `docs/DECISIONS.md` for the legend. Newest entries go at the end.

- ✅ **Infinite canvas is the product.** slop-desk built and retired one; its reasons were
  AppKit-specific (a libghostty surface cannot live under a scaled ancestor) and product-fit
  (undiscoverable, not keyboard-navigable). We own the renderer, so zoom is real, and we add
  keyboard navigation, zoom-to-fit/selection, grid snap, arrange-by-repo and a minimap (kolu).

- ✅ **Canvas navigation: ⌘1 fit all, ⌘2 fit the active item, ⌘0 back to 100 %, ⌘⇧R arrange**
  (2026-09-06). ⌘1 and ⌘0 already existed; ⌘2 is the natural sibling of ⌘1 and ⌘⇧R is free
  (⌘⇧A is next-attention, ⌘⇧M mute, ⌘⇧I stats, ⌘⇧T/N/L the agent, the note and the
  conversation). ⌘0 no longer resets about the middle of the *viewport* but keeps the **active
  item** centred, which is what "back to 100 %" means when something is being worked on; with
  nothing active it behaves as before. ⌘2 with nothing active does nothing — ⌘1 is the action
  for "show me everything". There is no multi-selection on the canvas, so "zoom to selection"
  is zoom to the active item; `CanvasView::active_rect` is the one place a selection would join.

- ✅ **Camera moves are a pure flight the render loop advances** (2026-09-06):
  `slopty_client::canvas::Flight` interpolates between two cameras over `FLIGHT` = 180 ms with
  a cubic ease-out, and the canvas element's prepaint advances it by the time since the last
  frame. No timer anywhere — the frame *is* the clock — and a landed flight stops asking for
  frames, so a still canvas costs nothing. Zoom interpolates **geometrically** and the
  viewport's centre travels in a straight line between the two centres, so a simultaneous
  zoom-out and pan reads as one movement instead of a swing. The self-test turns animation off
  (`CanvasView::set_animation`, `#[cfg(feature = "e2e")]`): there a frame is a step, not a
  moment, and a dump taken mid-flight would report where the camera was passing through.
  A flight is the camera's default move, never a lock: pan, wheel, pinch, ⌘=/⌘- and a minimap
  scrub all abandon it (`CanvasView::take_camera`), so the 180 ms of an animation are never
  180 ms of ignored input.
  Tested with a clock of its own in `slopty-client` (ease, exact landing, straight centre) and
  through the actions headlessly.

- ✅ **Arrange by repository is a pure layout function, keyed on the working directory**
  (2026-09-06). `slopty_client::arrange::arrange_by_repo` takes items (id, size, cwd, activity)
  and returns origins plus one `Heading` per block: no document, no camera, no clock, so
  determinism, no-overlap, size-preservation and block order are ordinary unit tests. Blocks
  run left to right by most recent activity, which is the item's **z order** — raising an item
  on focus is already the recency the canvas keeps, so nothing new is stored. Inside a block
  items fill a square-ish grid, most recent first, ties broken on the item id. Gutters are
  `GAP` inside a block and `3 × GAP` between blocks; the heading is a 28-pt band above each one,
  drawn in canvas coordinates with role `Heading` so it pans, zooms and reads aloud.
  The key is the **repository root the host resolved** (see the protocol 14 ruling below), kept
  current as the shell moves: `TermEvent::Cwd` was being dropped by the terminal view, so it now
  reaches the canvas (`TerminalViewEvent::Cwd` → `CanvasView::session_moved`) and a shell that
  `cd`s into another checkout arranges under the new one instead of the directory it opened in.
  Each `Heading` carries the ids of its block, so a block whose items have all closed loses its
  heading on the next reconcile: otherwise ⌘1 would keep fitting an empty rectangle and a screen
  reader would keep reading a label for nothing. `CanvasItem.group` stays unused: it is the
  server's field for server-side groups, and this layout is a client view.
  Arranging is **not undoable**: the canvas has no undo stack, for this or for a drag.
  Goldens: `arrange-by-repo` is new and `note` was re-accepted for ⌘0's new centre (2.19 % of
  pixels, a pan). The four **iOS** goldens were re-accepted too — they predate the ghostty
  metrics ruling above, and `ios-phone-terminal` had drifted to 0.9–1.1 % against a 1 %
  tolerance, i.e. it was failing on some runs and passing on others before this branch touched
  anything. Refreshed they are all 0.000 %.

- ✅ **The repository root comes from the host, protocol 14** (2026-09-06). Only the machine a
  shell runs on can see its `.git`, so `slopty_host::repo::root_of` resolves it there: walk up
  from the working directory to the nearest ancestor holding a `.git` **entry**, and stop.
  No `git` subprocess and no libgit — a handful of `stat` calls per `cd`, on the session actor's
  own thread, cached in the actor and recomputed only when OSC 7 says the directory changed.
  The entry is a directory in an ordinary checkout and a *file* in a worktree or a submodule;
  the `gitdir:` link inside that file is deliberately **not** followed, so a worktree is its own
  repository and not the checkout it was made from — two worktrees of one project are two places
  to work. A repository nested inside another wins, because the walk stops at the first entry.
  The path is canonicalised first, so two shells that reached one checkout through different
  symlinks land in the same block; a directory that has since been removed answers `None` rather
  than guessing from the stale string. Wire: `SessionSummary.repo` and `TermEvent::Cwd` becoming
  `{ path, repo }`, both `Option<String>`; goldens `host_session_opened`, `host_term_cwd` and
  `host_term_cwd_no_repo` (new) and `client_hello` re-accepted, `PROTOCOL_VERSION` 13 → 14.
  `slopty_client::arrange` keys on the root when it is there and keeps the old containment
  heuristic only for the items the host resolved no root for — a directory outside any
  repository, or one that has since been removed — so a mixed canvas (a shell outside any
  repository next to shells inside one) still groups sensibly. It is **not** a compatibility
  path: the handshake compares `PROTOCOL_VERSION` for equality and refuses a mismatch, so a
  client that reads roots never sees session data from a host that does not send them.
  This replaces the deferral in the ruling above: sibling subdirectories with nothing checked
  out at the root are now one block, which the heuristic could never see. Tests:
  `slopty_host::repo` over a temp tree (checkout, nested subdirectory, worktree `.git` file,
  repository inside a repository, no repository, directory that is gone),
  `sibling_directories_of_one_repository_are_one_block` and
  `a_rootless_shell_does_not_join_a_rooted_block` in `slopty_client::arrange`, and
  `the_hosts_repository_root_decides_the_blocks` headlessly.

- ✅ Kind-aware culling: terminals keep state and stop painting off-screen; video pauses decode.

- ✅ **Host-authoritative document, optimistic client.** `slopty-host::CanvasStore` owns the
  document (JSON at `<data>/canvas.json`, atomic rename, serialised writers), validates every
  `CanvasOp` (finite, clamped geometry; unknown ids rejected), bumps a version and broadcasts a
  `CanvasSync::Delta { by }`. Clients apply their own ops immediately and recognise the echo by
  `by == me`. A new session gets a terminal item automatically (right of the rightmost, snapped
  to 16 units) so every client sees it in the same place; a closed session removes its item.

- ✅ Snapshot is pushed right after `HelloAck` on the control stream; no client request needed.

- ✅ Attention signal: `AgentEvent.attention` → `CanvasEvent::Attention` → `slopty_platform::attention()`,
  which is `AudioServicesPlayAlertSound(kSystemSoundID_UserPreferredAlert)` on macOS (the alert the
  user picked in System Settings, respects their volume) and `AudioServicesPlaySystemSound(kSystemSoundID_Vibrate)`
  on iOS (`objc2-audio-toolbox`, `AudioServices` feature; AudioToolbox.framework linked in the
  iOS spec). No `UNUserNotificationCenter`: it needs a signed bundle with the notification
  entitlement, which the bare macOS binary is not; revisit when the Mac app ships as a bundle
  (superseded 2026-09-05: bundled app ships via `cargo xtask bundle` with GPUI `SystemNotification`
  banners; see "Notification-centre banners for agents" below).

- ✅ Finding the agent that needs you: `CanvasView::needs_you` is the list of terminal items
  whose agent is `Blocked` (not `IdlePrompt`) and not yet answered from the badge, sorted by
  `(rect.y, rect.x)`. ⌘⇧A (`NextAttention`, Canvas key context, so it works with a terminal
  focused since ⌘ chords fall through) picks the entry after the active item, wrapping, else
  the first, and runs the same reveal + focus as the "answer" button. The count travels as
  `CanvasEvent::NeedsYou(n)` to the workspace, which draws the "N need you" pill in the top
  bar on both platforms (a tap on the phone, where there is no ⌘⇧A). Reading order rather than
  z or arrival time because it is the one order the user can predict from what they see; the
  picker (⌘O) uses the same order within its "needs you / other agents / shells" ranking. No
  proto change: the count is derived from the `HostMsg::Agent` table the client already has.

- ✅ **"+ agent" is a menu** (2026-09-12): "Terminal agent" / "Conversation" / "Resume
  conversation…" (`agent-terminal`, `agent-conversation`, `agent-resume`, each the action its
  shortcut runs), because the phone has no ⌘⌥T or ⌘⌥R and the bar has no room for two more
  pills next to the host name. The driven-agent scenarios open their first card through it
  (a click on the Mac, a `ui_tap` on the simulator) so the path a finger takes is the one
  tested. The pill's label and place are unchanged, so no golden moved.

- ✅ **A note is titled by its first line** (2026-09-12). Every note's title bar said "note",
  so the picker, the palette's "Go to …" lines and a canvas of cards could not tell them
  apart. Ruling: `note_title` takes the first non-empty line with Markdown's `#`, `-`, `*`,
  `>` stripped, cut to 40 characters with an ellipsis, "note" while empty — no stored title,
  so nothing to keep in sync and nothing on the wire. Test: `a_note_is_titled_by_its_first_line`.

- ✅ **A new shell or agent starts where the active shell is** (2026-09-12). ⌘N and ⌘⇧T sent
  `cwd: None` (the host's default, the home directory) while ⌘⌥T's driven card already took
  the active terminal's directory; every terminal opens a new tab in the current directory
  and a shell opened beside a shell belongs to the same work. Ruling: `OpenSession.cwd` is
  `CanvasView::active_cwd` — the active item's session cwd as the host last reported it
  (`session_moved` follows OSC 7), `None` with no active terminal (the empty canvas, a note,
  a window). No wire change. Test: `cmd_n_asks_the_host_for_a_shell_and_its_echo_places_and_focuses_it`
  reads the `OpenSession` for the empty canvas (`cwd: None`), then ⌘N and ⌘⇧T beside a shell
  the host placed in `/tmp/work`.

- ✅ **A command palette on ⌘⇧P** (2026-09-12). Warp, Zed and every editor since Sublime have
  one, and Slopty's shortcuts had grown past what a menu bar teaches (the phone has no menu
  bar at all); a hardware keyboard on an iPad had no way to an action it did not know the
  chord for. Rulings: (1) the palette is a canvas overlay like the picker, one `Input` over a
  list of every action with its keys, the keys read from the binding tables at build time
  (`PaletteItem::new` finds the first binding whose action `partial_eq`s) so a rebinding
  never desynchronises the label — shown in Apple's menu-bar order `⌃⌥⇧⌘` and the same on
  every platform (`palette::keys_label`, not GPUI's `Display`, which spells `cmd-` outside
  macOS); (2) the filter keeps a line when every word of the text is found in its label, any
  order, any case — no fuzzy scoring, since the list is thirty lines and a word narrows it to
  one or two; (3) the choice is not run from inside the palette: it closes, the focus goes
  back to where it was when ⌘⇧P was pressed (`window.focused` remembered), and the action is
  dispatched on the next frame from that element — so `Find in terminal` finds in the
  terminal that had the keyboard, and `New note` reaches the canvas the way ⌘⇧N does; (4)
  the app's own lines (settings, hosts) are appended by the app (`extend_palette`) since
  those actions live outside `slopty-ui`; (5) ↑/↓/Esc are caught in the capture phase from
  the field's own `MoveUp`/`MoveDown`/`Escape` actions, the pattern the composer uses; (6)
  the top bar ends with a "⋯" button (a11y "Commands") that opens it, since a phone without a
  hardware keyboard has no ⌘⇧P and the bar had no room for one button per action; on a
  phone-wide bar "+ window" yields its place to it (the palette lists "Add a window or
  display"), since with both the last button sat at x = 405 on a 402-pt screen — the iOS
  self-test taps it, types into the field through the soft keyboard and runs "New note";
  (7) the canvas's sessions are lines too — "Go to <title>" with the agent's status on the
  right, ordered as the picker orders them (waiting on the human first) and ahead of the
  actions, since on a wall of terminals the thing most often wanted is one of them; a
  session line reveals and focuses the terminal instead of returning the keyboard to where
  it was (`PaletteRun::Session` beside `PaletteRun::Action`); (8) file cards are lines after
  the sessions — "Go to main.rs · src" with "file" on the right (`PaletteRun::Item`, which
  activates and reveals the item; added 2026-09-12 with the cards), since a card put down
  while reading an agent's edit is soon off-screen and the palette is how a phone gets back
  to anything; (9) a path typed into the field is a line of its own, first — `Open
  src/lib.rs` with `line 7` (or `file`) on the right, offered when the text is one word with a
  `/` in it and no empty segment (`palette::path_query`; `note` stays a command, `a//b` is
  not a path), an optional `:N` split off as the line to land on; ↩ opens the card through
  `Canvas::open_file`, a relative path against the active shell's directory and `~` left for
  the host, which expands it to its own home (`slopty_host::file::expand_home`) since the
  client cannot know it. Added 2026-09-12: a file the agent never touched had no way onto
  the canvas but a shell command, and the palette already had a field; (10) the field is a
  quick open too (protocol 36, same day): a word of two characters or more that is not a
  rooted path (`palette::files_query`) is asked of the host as `ClientMsg::FindFiles { root,
  query }` — the root the active shell's directory, or `~` when no shell is active, which the
  host expands — answered with `HostMsg::FoundFiles` from the same `files::matching` walk the
  composer's `@` completion uses (best eight, `.gitignore` honoured); the files are `Open
  <relative>` lines after the commands the word matches (a command is still the likelier
  intent), directories left out, and each change of the field drops them until the answer
  for the new text arrives, so a stale list never sits under ↩; a file card counts as the
  active item's directory too (`cwd_of`: its parent, when the path is spelled from the root),
  so ⌘N beside a card and a lookup over it start where the file is. Tests:
  `a_path_in_the_field_is_told_from_a_command` (unit, with `files_query` and `found_file`),
  `a_tilde_is_the_hosts_home` (host unit), `a_path_typed_into_the_palette_opens_a_file_card`
  (headless: the typed path, the host lookup asked with the shell's root and with `~`, the
  found lines and ↩ on one); the app self-test's driven scenario types `note.t` into the
  palette and gets "Open note.txt" from the real host walk, ↩ bringing the card back with
  its two lines. Earlier tests:
  `keys_read_as_glyphs_and_the_filter_takes_every_word` (unit),
  `the_command_palette_runs_an_action_by_name` (headless: the Dialog and its lines with their
  keys in the a11y tree, the field focused, Esc closing with the canvas focused again and
  nothing run, `note` + ↩ leaving one note, "Go to shell" first once a shell is on the canvas
  and revealing it with the keyboard in its terminal, a file card's line after it and `main` +
  ↩ making the card active), and the app self-test's notes scenario,
  which now makes its note through the palette.

- ✅ **A file the agent touched is a card on the canvas, protocol 34** (2026-09-12). "Open in
  the editor" needs a shell and a keyboard; on a phone neither is comfortable, and the human
  mostly wants to *read* what the agent just changed, with the diff's context around it.
  Rulings: (1) a **file card** is an item (`ItemKind::File { path }`) so every client sees it
  where it was put, but the **text is not document state** — a file is the host's, can be
  large and changes under the agent, so each client asks (`ClientMsg::ReadFile`) and the host
  answers (`HostMsg::File`, `slopty-host::file::read`) with the first 512 KiB then the first
  2 000 lines, `Binary` on a NUL or invalid UTF-8, `Missing` with the OS's word; (2) **no path
  restriction** on the host: a paired client already has a shell there, so a read is nothing
  it could not do (logged like the rest); (3) **one card per path** — "view" on another call
  for the same file reveals the card and reads it again (`canvas::open_file`), because two
  copies of one file drift; (4) the card **reads again unasked when an agent's Edit or Write
  result lands** anywhere on the canvas (a result does not name its file, cards are few, and
  a stale card is worse than a spare read), and on its "reload" pill for edits made in a
  shell; (5) the card is read-only — editing is the editor's job (`open`), viewing is the
  card's; (6) drawn with `uniform_list` (line-numbered rows, the gutter as wide as the last
  number) so a 2 000-line file lays out only what is on screen; (7) the way in is a **"view"
  button on every edit, write and read in the agent card**, always shown (a card needs no
  shell, unlike "open"), a relative path made absolute against the agent's `cwd`
  (`TerminalView::view_file`) because Claude Code's tools take absolute paths but a fake or a
  hook need not; (8) a byte-capped read drops its last, possibly partial, line and counts it
  in `more_lines`, so the card never shows half a character; (9) a read that follows one
  **tints the lines that changed** (success tone, `file::changed_lines`: inserted and
  replaced lines, a deletion pointed at by the line now standing there) and scrolls the first
  into view, the summary saying "12 lines, 3 changed" — the whole point of a card that reads
  again is to show what the agent just did, and a 2 000-line file hides a one-line edit
  otherwise; the tint stays until the next read (nothing times out under the reader); (10)
  the card's title bar has the same **"ask" pill** a window has, which puts `@<path> ` into
  the agent's composer (`ask_agent`, the card the human is on, else the topmost, else a new
  one) — the mention is how a human names a file to Claude Code, and reading a file is
  usually a step before asking about it. Ids: `conversation-view-<entry>`
  (a11y "View <path> on the canvas"), `file-<item>` (a11y Document "File <path>" whose value
  is the summary: "212 lines", "12 lines, 40 more", "binary, 1.2 MB", "missing: No such
  file"), `reload-<item>` (a11y "Read the file again"). Goldens `client_read_file`,
  `host_file`, `host_file_missing`, `host_canvas_file`. Tests: `slopty-host::file` unit
  (text, binary, latin-1, missing, directory, line clip, byte clip); headless
  `a_tool_calls_path_opens_a_file_card` (one item for the absolute path, the read asked, the
  text drawn and read out, a second read after an Edit result, the same card on a second
  "view", ⌘W removes it and its view); the app self-test's driven scenario ("view" on the
  fake's edit → a card that says missing, the file written on disk → "reload" shows its two
  lines, ⌘W closes it).

- ✅ **⌘F in a file card finds lines** (2026-09-12). A 2 000-line card is scrolled with the
  wheel or not at all on a phone; the terminal already had a find bar, and the same chord
  with the card active should do the same thing. Rulings: (1) the canvas's ⌘F
  (`find_in_active`) reaches the active terminal as before, else the active file card
  (`FileView::find`); the bar is the terminal's in shape and position (top-right, the field,
  `n/total`, ↑ ↓ ✕) with its own key context `FileSearch` bound in the canvas's table (Esc
  closes, ⌘G/⌘⇧G step, ↩/⇧↩ step from the field) since no terminal is around it; (2) a hit
  is a line, not a span — plain case-insensitive `contains` on the client
  (`file::find_hits`), no regex and no host round trip, because the card already holds every
  line it draws and a line is what the gutter numbers; hit lines are tinted in the warn tone,
  the current one stronger, over the edit's accent and the change's success tints; (3) the
  first hit landed on is the first at or after the line the card opened at, since a search
  usually starts from the edit that opened it; the hits follow a re-read under an open bar
  and wrap at the ends; (4) closing the bar hands the keyboard to the canvas
  (`FileViewEvent::FindClosed` → `pending_focus_self`), the card having no focus of its own;
  (5) the title bar carries a "find" pill (`find-<item>`, a11y "Find in the file") beside
  "reload", the phone's ⌘F; (6) an "edit" pill (`edit-<item>`, a11y "Open the file in the
  editor", same day) types `${EDITOR:-vi} +line 'path'` into the shell the human was last in
  (`run_in_shell`, as the agent card's "open" does), the line being the current find hit,
  else the line the card opened at (`FileView::reading_line`) — the card is for reading, the
  editor for changing, and the pill is the step between; drawn only while a shell exists to
  take it; (7) the tinted line is a **reading line** (same day): with the card active and the
  canvas focused, ↑/↓ move it a line, ⇞/⇟ a page of the rows shown, Home/End (⌘↑/⌘↓) to the
  ends, kept in view with `ScrollStrategy::Nearest` (`FileView::move_line`, `LineMove`; the
  first key from no line lands on the top row shown), so a hardware keyboard on an iPad
  reads a 2 000-line card without a wheel and "edit" opens where the reading stopped. The
  bindings (`LineUp`… in `canvas::actions`) are scoped to `Canvas && file_card`, a flag the
  canvas puts on its key context only while a file card is active — a binding matches
  before a focused terminal's key handler runs, so an unscoped `up` took the arrows from
  every shell (the iOS hardware-keyboard self-test caught it; headless
  `arrow_keys_reach_a_focused_shell_beside_a_file_card` keeps it caught) — and do nothing
  under the palette or the picker. A click or a tap on a row (`file-line-<item>-<row>`) makes it the reading line
  too, and "ask" names it — `@path line N ` — when there is one, since the tinted line is
  what the question is about. The palette line reads "Find in terminal, conversation or file". Tests:
  `arrow_keys_move_a_file_cards_reading_line` (headless: first key → top, steps, a page,
  clamps at both ends, `reading_line` follows, a click on the sixth row); the iOS self-test's
  driven scenario writes the file, taps "reload" (two lines), taps "find", types `there`
  through the soft keyboard and sees line 2 become the reading line (`ItemInfo.file.line`),
  `a_file_cards_edit_pill_opens_the_editor_at_the_line_read` (headless: no pill without a
  shell, `+2` from the opening line, `+3` from the second hit),
  `hits_are_the_lines_holding_the_needle_in_any_case` (unit),
  `find_in_a_file_card_steps_through_its_lines` (headless: ⌘F opens the bar with the field
  focused, the count reads 1/2, ⌘G/↩/⇧↩ step and wrap, a re-read recounts, Esc closes and
  the canvas is focused).

- ✅ **A result line that names a file views it on the canvas** (2026-09-12). A grep's hits
  (`src/a.rs:12:fn x`), a compiler's locations (` --> src/b.rs:3:5`), a glob's paths: the
  agent's tool results are full of places the human wants to look at, and the card had "view"
  only on the call's header. Ruling: every line of a tool result that holds a path
  (`url::first_path`, the first whitespace-delimited token `path_range_at` accepts — the
  terminal's ⌘-click rule, so the same text reads the same in both) is a button
  (`result-path-<entry>-<line>`, a11y "View <path>:<line> on the canvas") that opens the file
  card at that line through `TerminalView::view_file` (relative to the agent's cwd); a line
  without one is text. A grep hit's `path:12:text` is cut at the text (`url::grep_cut`) so
  the path and line are read off it, while a bare `path:12:5` stays whole for the ⌘-click
  underline. The click stops propagation so it does not toggle the result's fold. Tests:
  `the_first_path_of_a_result_line_is_found_with_its_line` (unit),
  `a_results_path_lines_view_the_file` (headless: the two buttons, the text line, a click's
  `ViewFile` made absolute, the fold untouched).

- ✅ **A tool call's file opens in the canvas's shell** (2026-09-12). The card shows the
  agent editing `src/a.rs`; the human's next move is to look at that file, and finding it
  by hand meant a shell, a `cd` and a typed path. Ruling: an edit, a write and a read
  (`conversation::tool_path`, the three `ToolDetail`s that name one file) get an "open"
  button in the call's header, gated exactly as the answer's "run" button is (the canvas's
  `set_can_run_in_shell` flag: a plain shell exists) and going through the same
  `TerminalViewEvent::RunInShell` — the command is `url::editor_command`, the
  `${EDITOR:-vi} 'path'` line ⌘-click on a path in a terminal types, so one rule says what
  "open" means everywhere and the shell, not the client, expands `$EDITOR`. Claude Code's
  file tools take absolute paths, so the shell's directory does not matter. The button lives
  in the fold row and stops its click's propagation so opening does not toggle the call.
  A read that asked for a slice opens at its first line (`offset`); a diff carries no line
  on the wire, so an edit opens at the top. Ids `conversation-open-<entry>`,
  a11y "Open <path> in the editor". Test: headless
  `a_tool_calls_path_opens_in_the_canvas_shell` (no button with only the card; a shell joins
  and it appears; the click reveals the shell and its channel gets one `Paste` of the quoted
  editor command then `Key(Enter)`; the call's fold state is unchanged).

- ✅ **A fenced block runs in the canvas's shell, and the canvas decides which one**
  (2026-09-12, the question "A fenced block in an answer is its own element" left open).
  "Copy" got the command out of the answer but the human still had to find a shell and paste
  it, which on a phone is most of the work. Rulings: (1) the button is drawn only when a click
  on it would go somewhere — the canvas tells each terminal view whether it has a **plain
  shell** (`set_can_run_in_shell`, a flag pushed whenever the set of shells can have changed:
  a session opening or closing, the item set reconciling, an agent event) so the
  conversation's render stays a pure function of the view's own state and asks the canvas
  nothing; (2) a **plain shell** is an `ItemKind::Terminal` item, drawn here, whose session is
  a `SessionKind::Terminal` (not one the host drives as an agent) and which no coding agent
  has been seen in (`agents` has no entry) — an agent's terminal is a conversation, not a
  prompt, and typing a snippet into one answers whatever it was asking; (3) the target is the
  **most recently activated** such shell, else the newest, kept as one recency list
  (`shell_recency`: appended when a session opens, moved to the end when its item is
  activated, entries that are not shells any more skipped rather than removed), because "the
  shell I was just in" is the answer a human expects and "the newest" is the only defensible
  fallback before they have been in one; (4) the click is
  `TerminalViewEvent::RunInShell(String)` — the view knows the code, the canvas knows the
  shells — and the canvas reveals the target and types it exactly as the block menu's "rerun"
  does (`TerminalView::run_text`, now shared by both): a `TermRequest::Paste` of the whole
  body, then ↩ once as a key, so a multi-line block arrives whole under the shell's bracketed
  paste and runs the way the human would have run it. Ids
  `conversation-code-run-<entry>-<segment>`, a11y "Run in shell". Tests: headless
  `a_fenced_block_runs_in_the_canvas_shell` (no button with only an agent card on the canvas;
  a shell joins and the button appears; the click reveals the shell, whose channel gets
  `Paste("echo hi")` then one `Key(Enter)`; an agent seen in that shell takes the button away
  again), and the app self-test's driven scenario (`snippet`: the button is in the dump's
  a11y, a click on its bounds runs `echo hi` in the first shell and `hi` comes back in its
  rows).

- ✅ **A command block can be asked of the agent** (2026-09-12). The human reads a failing
  command in a shell and wants Claude's view of it; copying the block and pasting it into a
  card was four steps and a decision about which card. Rulings: (1) "Ask the agent" on the
  block menu puts the block into an agent's composer as a fence (`$ command` then the
  output, a blank line after for the question) and focuses it — it does not send, the human
  frames the question — a selection under the click goes instead of the block, since a
  human who selected three lines means those; (2) the card is the one the human is on when it is an agent, else the
  topmost agent card, else a new driven agent opened in the active shell's directory, the
  block waiting in `CanvasView::pending_ask` for the session to open and in
  `TerminalView::compose` for the card's first frame (a driven view opens its conversation
  only then); (3) the same `AskAgent` event carries any text, so a "send this to the agent"
  from elsewhere is one emit away. Tests: `block_markdown` in
  `a_right_click_on_a_block_offers_its_command_and_output` (the menu item and the fence) and
  `asking_the_agent_opens_a_card_when_there_is_none` (headless canvas: `OpenAgent` sent with
  the shell's cwd, the block lands in the composer of the card that opens).
- ✅ **A card takes a name, protocol 37** (2026-09-12). A canvas of six shells reads
  "zsh, zsh, zsh, cargo, zsh, claude"; the human knows them as "build box", "logs", "the
  worktree". Rulings: (1) the name is document state (`CanvasItem.name`, trimmed, at most
  `NAME_MAX` 128 characters, blank is none; the host sanitises and refuses a longer one rather
  than cutting it), so every client sees the same title and it survives a relaunch; (2) it
  overrides the derived title (`canvas::card_title` over `derived_title`: the shell's OSC title,
  a window's, "display N", a note's first line, a file's `name · parent`) without replacing it —
  clearing the name brings the derived title back, and the derived title is the field's
  placeholder; (3) the way in is ⌘E on the active card or a double-click on its title bar
  (`click_count == 2` on the same mouse-down that begins a move; the first click's move ended on
  its mouse-up), a gpui-kit `Input` in the title's place (`rename-<item>`, a11y "Card name"),
  ↩ keeps, Esc leaves it as it was, a click elsewhere leaves it too and whoever was clicked
  keeps the keyboard; after ↩ or Esc the keyboard goes back to whoever had it before the field
  (`Rename::return_to`, applied from `render` as the palette's return is), so naming a shell
  never costs its focus; (4) the palette lists a named terminal by its name ("Go to build box"),
  a file card by its title, and any other card only once it is named — a name is a wish to
  find the card again; "Name this card" is a palette line too; (5) the notification-centre
  banner for a named card's agent leads with the name ("build box · Claude wants to use
  Bash", `canvas::banner_title`) — with several agents on a canvas the badge text alone does
  not say which card wants the human. Tests:
  `a_card_is_named_from_its_title_bar` (headless), `a_name_is_trimmed_blank_is_none_and_too_long_is_refused`
  (host), goldens `host_canvas_named` and the re-accepted `host_canvas_file` / `client_hello`.

- ✅ **A note reads as Markdown until it is edited** (2026-09-12). A note is where a canvas
  keeps prose — a checklist, a link, a heading over a paragraph — and it was drawing that prose
  as the characters typed, in a textarea that never stopped being an editor. Rulings: (1) a note
  that is not being edited draws its text with `TextView::markdown`; the editor comes back when
  it has the keyboard, so the two states are exactly "has the caret" and "does not", read from
  the focus handle rather than a flag of our own; (2) an empty note keeps the textarea, since
  the "write…" placeholder is the editor's and there is nothing to render anyway; (3) the style
  is one function for every Markdown surface (`slopty_ui::markdown::style`, factored out of
  `terminal::conversation`), with a `scale` the note passes its zoom so headings and code blocks
  grow with the canvas instead of staying at chrome size — the chrome passes `1.0` and reads as
  it did; (4) a click on the rendered note focuses the editor with the caret **at the end of the
  text**: rendered Markdown has no offset to map a click back to, and carrying on where the
  writing stopped is what a note is for (`NoteView::focus`, which ⌘⇧N's own focus uses too); (5)
  gpui-kit draws Markdown as styled text and leaves nothing in the accessibility tree, so the
  rendered note reads itself out — `Role::Document`, label "Note", value the text — which is the
  same text its editor's `MultilineTextInput` reads, so a screen reader hears one note either
  way. Giving each heading inside it its own `Heading` node would need a block `MarkdownPlugin`
  in the gpui-kit fork, which rebuilds the parse on every registration; not worth a fork move
  for this; (6) no wire change: the text still lives in `ItemKind::Note` and is still committed
  400 ms after typing stops and on blur. Tests:
  `a_note_reads_as_markdown_until_it_is_edited` (headless: the rendered document and its text in
  the a11y tree with no editor, a click swapping in the `MultilineTextInput` on the same text,
  typing landing at the end, and the document holding the edited text once the reader is back)
  and the app self-test's notes scenario, whose note is empty and so still shows the editor and
  its placeholder — both render goldens unchanged.

- ✅ **⌘] and ⌘[ walk the cards in reading order** (2026-09-13). A canvas of eight cards had
  no keyboard way from one to the next: ⌘⇧A goes to the agents that need you, the palette
  goes to a card by name, and everything else was a scroll and a click. Rulings: (1) the
  order is the page's — rows by the top edge, top to bottom, left to right within a row —
  computed from the rects each time (`CanvasView::reading_order`), never from arrival or z:
  cards snap to the grid, so neighbours placed by hand share a row exactly, and an arranged
  block reads as its grid; (2) both keys wrap, and nothing active starts at the first (⌘])
  or the last (⌘[); (3) the step is the palette's "go to" (`go_to`): active, revealed, and a
  terminal takes the keyboard, so ⌘] into a shell is one key, not two. Tested headless
  (`the_cards_are_walked_in_reading_order`). Not done: a spatial walk (⌘⌥→ to the card on the
  right) — reading order covers a grid, and a free-form layout has no unambiguous "right".
