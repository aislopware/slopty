# Layout

The terminal-first layout. It supersedes, for layout and components, "Zeron is the base; the
premium references set the bar" (`ui.md`) and the zeron copy as a starting point. The studies
behind it are in `.research/terminal-first-2026-10-30/`: `rex.md` (Superlogical Rex),
`agent-terminals.md` (herdr, cmux, Warp, Zellij, Otty and others) and `terminal-craft.md`
(Ghostty, iTerm2, WezTerm, kitty, Blink, Prompt). Each item below names its source section.

- ✅ **The window is a terminal, not an app with a terminal in it (2026-10-30).** Slopty's shell
  was zeron's GUI-first chat layout: a sidebar grouped by state, a title band over the
  terminal, and a permanent right column of tabs. The person asked to break from zeron and the
  GUI-first apps in layout, UI/UX and every component, after a deep study of the premium
  terminals, Rex first. All three studies arrive at the same shape, and it is this one:
  - **One row of chrome on the Mac** (rex §2.1, §12.1 #1; terminal-craft §1, #4). The titlebar
    row holds the traffic lights, the sidebar toggle, the session's identity (program icon,
    title, then machine and branch in secondary text, and its state mark), and at the trailing
    end the inbox, the palette and new session. Nothing else sits over the terminal: no title
    band, no status bar, no footer. The row takes the terminal's own background colour and
    follows the program's OSC 11, with the window's appearance chosen by that colour's
    luminance, as Ghostty's transparent titlebar does; the title dims to secondary when the
    window is not key. Glass is for chrome and popovers only; a terminal is always opaque.
  - **The terminal owns the rest** (agent-terminals §5 #5). With the sidebar open it gets at
    least 80 % of the window width, edge to edge under the row, padded only by its own cell
    padding.
  - **One session per view, splits on demand** (agent-terminals #17; rex §12.1 #11;
    terminal-craft #8). A split is for two things of one task (an agent beside its dev server,
    a terminal beside its Changes), on the Mac and on an iPad 1024 pt or wider; never on an
    iPhone. Ghostty's craft: a 1 pt divider with a 7 pt grab target, panes snapped to whole
    cells, double-click equalizes. With more than one pane, the unfocused ones dim by brightness
    alone (WezTerm, about 0.85, saturation kept, so no hue greys), painted in the terminal
    element, and an unfocused pane that needs the person wears a thin accent ring. Keys: ⌘D
    right, ⇧⌘D down, ⌘⏎ where there is most room, ⇧⌘⏎ zoom, ⌥⌘arrows and ⌘] ⌘[ move focus. A
    pane header (program icon, title, cwd · machine; its controls only on the focused or hovered
    pane) shows only when a view holds more than one pane.
  - **No permanent right column** (agent-terminals #9; rex §12.2 #2; terminal-craft #12).
    Inspector, Files, Changes and Browser are views summoned by their own key (⌘I, and the keys
    the palette shows for the others), and nothing is open by default. A summoned view docks at
    the right of the session as a pane of its split, so "a side pane" is a placement, not a
    region; it can also be split elsewhere or zoomed, and pressing its key again or Esc puts the
    focus back in the terminal. A file picked in Files opens as a pane of its own. The
    components inside the old right pane stay; the tab column that held them is deleted.
- ✅ **One state vocabulary on every surface (2026-10-30).** Rex's five states, which iTerm2,
  herdr and cmux name alike (rex §4, §12.1 #4; agent-terminals §4.1; terminal-craft #1): at work
  (a spinner, blue), needs you (amber, with the need in words: "Needs approval", "Has a
  question", "Needs a sign-in", from the agent's records), failed (red), done and not yet seen
  (green ✓, title in the heavier weight until the person looks), and at rest (no mark). The same
  mark and hue sit on the sidebar row, the titlebar identity, a pane header, the picker, the
  inbox, a notification and the Dock badge. Seen is one state across the person's devices: a
  result read on the iPhone stops calling on the Mac. Slopty's adapters for Claude Code, Codex
  and pi lead; OSC 7501 is the source for any program without one, and the fallback when an
  adapter is silent (rex §12.2 #10).
- ✅ **The session list is stable, and its marks carry the attention (2026-10-30).**
  - Rows keep their place: grouped by project (the folder and repository), in the order their
    sessions started, never re-sorted by state (agent-terminals #1; the person's own herdr runs
    the stable order). The "Needs you / Working / Done" groups are deleted: done is a mark, so a
    finished session stays where it lives. A session nobody touched for 72 hours folds into
    Inactive at the foot, shut. Past sessions are found by typing in the picker, not kept on
    screen (rex §12.2 #1).
  - A row is one line, about 28 pt: the program's own icon (Claude Code, Codex, pi, opencode,
    lazygit, nvim, the shells; a neutral glyph otherwise), the title the agent itself gave
    (`/rename`, its summary, else the first prompt; never a model call), and the state mark.
    A second quiet line only when it tells two rows apart or says branch and ±, from Slopty's
    own git read (agent-terminals #8, #15). What the agent wants, its last words and its ports
    go in the hover card, read from its records.
  - Machines feel like one: the machine is a quiet subtitle on a row and a filter, not a level
    (rex §12.2 #1). A machine that drops keeps its rows, dimmed and marked as cached, and
    refuses input until fresh state arrives; a login problem is a badge that retries when
    clicked, never a credential prompt (agent-terminals #4). Machines reconnect each on their
    own, and one stalled machine never holds input to another.
  - A shut project wears its loudest child's mark and a count. Putting the sidebar away leaves
    a rail of one rolled-up mark per project, so "which one needs me" still has an answer
    (agent-terminals #2).
- ✅ **Attention is an inbox and a jump, never a feed (2026-10-30)** (rex §3.6, §12.1 #3;
  agent-terminals #3, #6; terminal-craft #5).
  - The inbox (the titlebar's eye, with a dot while anything waits) lists what needs the person,
    what failed and what finished unseen, across every machine, ordered by urgency: needs you,
    failed, done unseen, then at work. Each row: icon, title, the need or summary in one line, its
    mark and its age. It works from the keyboard alone, filters by typing, and ⏎ lands on the
    session. It answers nothing: Slopty never answers for the person.
  - ⌘J jumps to the next session that needs the person, then the next done unseen, oldest first.
  - Notifications point: muted for the session on screen; a system notification only for a
    session in the background or an app in the background, and only after a real wait; taken
    back once the person looks; a click lands on that session with one 0.4 s accent fade. The
    Dock badge counts needs you plus failed. No sound (the person works over Parsec); on iPhone a
    push for needs you and failed, and a haptic when the app is open.
- ✅ **The picker and the palette are one keyboard door to everything (2026-10-30)** (rex §3.1,
  §3.7, §12.1 #6-#8; terminal-craft #7). ⌘K opens the picker under the titlebar identity: a
  "Filter or create…" field over every session on every machine, each with its machine, the
  current one ticked, ⌘1-9 on the first nine, filters by state, past sessions by typing, and a
  name that matches nothing creates a session there. The palette (⇧⌘P) takes Ghostty's geometry
  (5 % from the top, at most 500 pt wide, a 48 pt field, rows padded 8 pt), shows each entry's
  current value and its key, and ranks by frequency and recency (WezTerm). ⇧⌘G, Go to Directory,
  filters a machine's folders and opens a terminal or an agent there.
  - Keys: ⌘K, already the palette inside an agent's session, becomes the picker in every
    session, a plain shell's too, and the terminal's clear moves to ⌥⌘K. ⌘J, which toggled the
    GUI-first bottom terminal panel, is the jump; that toggle is deleted, since the terminal is
    always the main surface. Every key stays a ⌘ key: a ⌃ key belongs to the TUI.
- ✅ **The terminal's own craft (2026-10-30)** (terminal-craft #2, #6, #9-#11, #13-#16; rex
  §12.1 #9).
  - Output never scrolls the person away from what they read: the view follows the bottom only
    while it is at the bottom, a keystroke takes it home, and a "↓ N new lines" pill says the
    bottom moved. The scroller overlays, shown only once scrolled back.
  - Find is Ghostty's card: floating at the top right, snapping to the four corners, a "3/55"
    count, ⏎ and ⇧⏎, Esc keeps the search and returns focus, the query shared with the macOS find
    pasteboard.
  - A session that ended shows a bar at the foot of its pane, tinted as an error when the code is
    not zero or the run lasted under 250 ms ("Claude Code exited after 120 ms, code 1"), with one
    key to start it again and one to close.
  - A resize shows "120 × 40" in the centre for 750 ms, never on the first layout. A pending
    chord shows its keys so far in a capsule at the bottom centre. A URL under the pointer shows
    in a banner at the bottom left. One chord labels every path, URL and commit hash on screen;
    a label opens the path in Files, the hash in Changes and the URL in Browser.
  - A program's background is the pane's background, so a dark TUI stays dark in a light window,
    with the minimum-contrast pass keeping each hue while lifting it to a readable lightness.
  - When the remote terminal stops echoing (a password prompt), the worker says so from its PTY,
    never from the screen, and the Mac turns on Secure Keyboard Entry while that session has
    focus, with a lock badge.
- ✅ **iPhone is a list, then the terminal; iPad is the Mac (2026-10-30)** (agent-terminals #12,
  #13; rex §12.1 #10; terminal-craft #3, §16).
  - iPhone: the session list (rows as on the Mac, grouped by project, machine as subtitle and
    filter), then a session as its terminal edge to edge under one band (back, mark, title, "N
    need you", which jumps), with the inspector as a read-only sheet. No chat view and no
    composer: the TUI's prompt is the only prompt.
  - The key row is Blink's measure: 44 pt in portrait and 38 pt in landscape, keys on the system
    keyboard's columns, Esc ⌃ ⌥ fixed at the left, an arrow joystick, hide keyboard and a ⌘
    layer at the right, a scrolling middle. A modifier taps for one key and holds for a chord; a
    flick down gives a key's second symbol. The middle group follows what Slopty's records say is
    running (in Claude Code: ⇧⇥, Esc, ⌃C, /, @, ↑; in a shell: Tab, ⌃R, ⌃C, ↑) and sends only
    what the person presses. It hides when a hardware keyboard is attached; on an iPad it sits in
    the keyboard's own shortcut bar.
  - iPad in regular width is the Mac's layout; in compact width it is the iPhone's.
- ✅ **What the layout leaves out (2026-10-30).** Each was rated against the hard lines and
  against use (rex §12.3; agent-terminals §5 rejects; terminal-craft rejects):
  - Harmful: answering an agent's question or permission from a GUI form, auto-approve and
    auto-resume, a composer or prompt queue that types into a TUI, reading state from the screen,
    broadcast input, touch gestures that type.
  - Redundant: agent boards and kanbans, a notification feed or mailbox, card rows that quote
    notification text, usage meters and stats screens (they would need private endpoints), title
    emoji and bell borders, tab colours and tab groups.
  - Rare: a tabs-above mode beside the sidebar, Lua or user-drawn sidebars, key-hint bars and
    prefix modes, floating layers, a quick terminal, an ImGui-style terminal inspector, a Watch
    app or menu bar extra, the long tail of appearance settings.
  - zeron is no longer adopted: `cargo xtask upstream adopt` and `xtask/zeron.toml` go, and the
    `// slopty:` marker on shell edits is no longer asked for. The interface still credits the
    zeron code it was copied from, as its licence asks.
- ✅ **No pinned sessions (2026-10-30).** A pin was a second, hand-made order laid over the list:
  a Pinned group at its top, kept per profile on the server, reordered by dragging, set from a
  row's menu or a swipe on the phone. It existed to keep a session from moving away. In the
  stable list rows never move, and the picker's ⌘1-9 and the ⌘J jump reach any session sooner,
  so the pin has no reason left (redundant, by the bar of "What the layout leaves out").
  - Deleted: the Pinned group, its drag and the row's drag lift, the menu's Pin row, the phone's
    swipe to pin (the swipe archives, or brings an archived row back), the settings field
    `sidebarPinnedSessionIdsByProfile`, the view model's sidebar preferences with their watch
    and mutation, and the pin glyph. A settings file that still holds the field loads.
  - Tests: `settings::tests::a_file_holding_deleted_settings_still_loads` and
    `shell::compact::tests::a_rows_swipe_archives_past_its_travel`.
- ✅ **The five states, as built (2026-10-30).** `shell::marks` holds them: one `Mark` read from
  the view model's display status and session, one hue table that every mark
  `loaders::status_mark` draws takes (the inspector's, the subagent cards' and Add device's
  too), and the need in the words the agent's records give ("Has a question", "Plan ready for
  review", "Needs approval · run cargo test"), which the row's reason line says as well.
  - At work is the Blue accent's own tone in each appearance, whatever the accent: an amber or
    green accent would read as needs you or done.
  - An agent at rest on its own work (a background command, a subagent it left running) is at
    work: the row's clock glyph and its "Background work" status go, and its reason line says
    what it waits on. The row's old dot hues go with them, so done unseen is green everywhere,
    no longer the accent's blue.
  - Tests: `shell::marks::tests::{every_status_reads_as_one_of_five_states, one_hue_per_state}`.
