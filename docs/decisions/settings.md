# Decisions — Settings

See `docs/DECISIONS.md` for the legend. Newest entries go at the end.

- ✅ **One TOML file, every key optional** (2026-09-05): `<data dir>/settings.toml`, the
  directory the client identity already uses (`SLOPTY_DATA_DIR`, else
  `~/Library/Application Support/Slopty`; `slopty_platform::dirs::data_dir` is the one copy
  every binary shares). `slopty-settings` is serde + `toml` 1.1.5 only, no GPUI, so the CLI
  and tests use it without a window. `#[serde(default)]` on every struct makes the file and
  any subset of it valid; unknown keys are found by diffing the parsed table against the
  serialised defaults (no hand-kept key list) and reported as warnings; a parse or type error
  yields the defaults plus a one-line error (`Loaded { settings, warnings, error }`), never a
  crash. `slopty settings init` writes a commented file generated from `Settings::default()`
  and covered by a round-trip test. `terminal.scrollback_lines` is worker-side and not here.

- ✅ **Hot reload by a 1 s stamp poll, not `notify`** (2026-09-05): a GPUI foreground task
  sleeps on the background executor's timer and compares `(mtime, len, exists)`. Editors save
  atomically (temp file + rename), which orphans a per-file FSEvents/kqueue watch, so `notify`
  would need a directory watch plus filtering and a debounce for half-written files anyway;
  one `stat` a second is free, handles create / replace / delete alike and adds no dependency
  to the iOS triple. Verified: `mono_size` 13 → 20 while the app ran re-fit the shell from
  21×87 to 14×58 (`stty size`) within ~2 s and back again; a `[font` typo showed
  `settings: …: TOML parse error at line 1, column 6` in the bar for 6 s with the defaults in
  force; an unknown `[font]` key showed the unknown-key notice and loaded the rest (the example then was `ligatures`, a real key since 2026-09-15).

- ✅ **Theme swap is a push, not a global** (2026-09-05): `Workspace::rebuild_theme` derives
  the `Theme` (`slopty_app::settings::theme_for`: variant, the family ahead of the bundled
  fallbacks, sizes clamped to sane ranges) and calls `CanvasView::set_theme`, which fans out to
  every `TerminalView`, `ScreenView` and the picker; the terminal element measures from the
  view's theme on each frame, so a size change re-fits the grid through the existing `fitted`
  → `TermRequest::Resize` path with no new wire message. Found on the way: the shaped-row
  cache was keyed by text, style, focus and size only, so a swap replayed the old palette's
  default text colour; the key now includes the family and the whole `TerminalPalette`.

- ✅ **Light variant + `system` appearance** (2026-09-05): `Theme::new(Variant::Light)` with a
  GitHub-light terminal palette and near-white surfaces; `appearance = "system"` (the default)
  follows `window.appearance()` through `observe_window_appearance`, so a macOS appearance
  change re-themes without a restart. gpui-kit's own theme mode (the pairing input) is switched
  alongside. Verified on a Mac in light appearance: the app opened light, `appearance = "dark"`
  turned it dark on save.

- ✅ **"Settings…" (⌘,) opens the file in the default editor** via `App::open_with_system`
  (`open <path>` on a background thread in `gpui_macos`), after `Settings::init` so a first
  press has a file to edit. Verified: the menu item opened `settings.toml` in VS Code. iOS has
  no entry: the defaults apply there and the file, if present in the sandbox, is still read.

- ✅ **`[terminal]` is the first behaviour section** (2026-09-15): `minimum_contrast` and
  `copy_on_select`, ruled in decisions/terminal.md. They reach the views the way colours do:
  `theme_for` folds them into the `Theme` (`TerminalPalette::minimum_contrast` in hundredths,
  `Theme::behaviour.copy_on_select`) and the existing `set_theme` push delivers them, so a
  save applies within the poll second with no second channel. The unknown-key warning for a
  `[terminal]` key now names the key (`terminal.scrollback_lines`), as it does for `[font]`.
- ✅ **`[font] mono_line_height`, `[terminal] bell_alert | cursor_blink | cursor_style | paste_protection`** (2026-09-15): the line height
  (`Typography::mono_line_height`, ghostty's `adjust-cell-height`, default 1.0, held to
  0.5–2.0 by `theme_for` as the sizes are) was a theme knob with no key; it rides the same
  theme push. `bell_alert` (default on) is the one setting the theme does not carry: the app's
  bell handler reads it directly (`settings::bell_alerts`), since sounding the alert and
  bouncing the Dock is an app act, not a view's. A bell in front of an active window still
  only flashes the card, whatever the flag. `cursor_blink` is ghostty's `cursor-style-blink`
  (`Behaviour::cursor_blink`: `program` leaves DECSCUSR alone, `always`/`never` override it
  either way); the element applies it where it decides whether the cursor ticks the blink
  clock, so an unfocused card stays steady as before. `paste_protection` (default on) is
  the confirmation before a paste that would run, ruled in decisions/terminal.md.
- ✅ **`[font] ligatures`, `[terminal] bold_is_bright | hide_pointer_while_typing |
  scroll_multiplier`, `[remote] muted`** (2026-09-15): the terminal ones are ruled in
  decisions/terminal.md; ligatures ride on `Typography`, bold-is-bright on
  `TerminalPalette` (a colour rule, like the contrast floor), the pointer hide and the
  multiplier on `Theme::behaviour`. `[remote] muted` (`StreamPrefs::muted`, off) opens a
  stream silenced on this client; `ScreenView::set_theme` moves the switch only when the
  setting itself changes, so the title-bar pill's own toggle survives an unrelated theme
  change (test: the tail of `new_stream_settings_are_asked_of_a_live_stream`). `[remote]
  muted` was cut on 2026-10-05 ("`[remote]` is the bitrate ceiling alone", below).
- ✅ **`[colors]` lays a palette over the theme** (2026-09-15). ghostty ships hundreds of
  schemes and every terminal takes a custom palette; Slopty's two variants were fixed. The
  section has `foreground | background | cursor | cursor_text | selection` and `ansi` (a
  list of up to 16), each a `"#rrggbb"` string (the `#` optional) or `""` for the theme's
  own (`Color(Option<[u8; 3]>)`, a malformed value is a parse error like an unknown
  appearance, so a typo never paints half a palette). `theme_for` lays them over
  `TerminalPalette` in both appearances (`colour_the_terminal`); a custom cursor takes
  black or white text under it (`Rgb::is_light`) unless `cursor_text` says otherwise. Ruled
  out: a scheme name (no bundled scheme table to pick from yet; a file of 16 hex strings
  is what every scheme repository exports). Per-appearance sections were ruled out here
  too, and are now the rule: see the next entry. Since the driver's colours answer OSC
  queries and follow theme changes, a program asking for its background hears the custom
  one. Tests: `colour_keys`, `custom_colours_lay_over_the_theme`.
- ✅ **A palette per appearance: `[colors.light]` and `[colors.dark]`** (2026-10-04, light
  pass phase 4). One `[colors]` table laid over both appearances meant that a palette picked
  on black was painted on paper too, where its pale yellows and cyans fail contrast. Each
  appearance now has its own table with the same keys, and `theme_for` lays only the one
  for the variant it builds (`ColorSettings::for_dark`).
  - The flat `[colors]` table is gone, with no fallback. A file that still has it gets the
    unknown-key warning and the theme's own palette.
  - The settings schema walk now reads a nested table as a table of its own, named by its
    dotted path. Its keys are form rows like any other, and a key the layout does not name
    lands on its root table's page. The form shows a light group and a dark group, and an
    unset row's swatch is that appearance's own colour. `[server.projects]` keys become rows
    by the same walk.
  - Tests: `colour_keys` (a flat table warns), `every_key_is_a_field_with_its_default`
    (recursive), `custom_colours_lay_over_the_theme` (dark leaves light alone).

- ✅ **An in-app editor for `settings.toml`** (2026-09-15). The phone had no way to change
  a setting: ⌘, handed the file to the system editor, and iOS has none for a file in the
  app's sandbox. Now ⌘, (the "Settings…" menu item, the palette's "Open settings") opens
  `SettingsEditor` (slopty-ui): a dialog over the workspace with the file's text (the
  commented defaults when there is none) in a monospace field; ⌘↩ or Save hands the text
  to the app, which parses it first (`settings::save`) and only then writes it and applies
  it at once (the watcher would take a second), or shows the parser's line under the field
  and keeps the dialog open so the typo can be fixed in place; Escape or a click outside
  discards. The Mac keeps an "Open in editor" button for the old path (the file written
  with the defaults first). A form with a widget per key was ruled out: the file is the
  schema's one source of truth, a text field takes every key at once, and the commented
  defaults are the documentation. Tests: `the_editor_saves_only_what_parses` (the app's
  parse-then-write), `the_editor_saves_on_command_enter_and_shows_a_refusal` and
  `escape_and_cancel_dismiss_and_the_phone_has_no_external_editor` (slopty-ui, headless);
  the app e2e types a section into the editor and reads the file back.
- ✅ **`[terminal] option_as_alt`** (2026-09-15): ghostty's `macos-option-as-alt`, the one
  key setting that has to reach the worker (the encoder lives there); ruled in
  decisions/terminal.md. It rides on `Theme::behaviour.option_as_alt` like the other
  terminal settings and from there, resolved for the side held, on every key event, not on
  the session, since the session is shared between clients.

- ✅ **`[terminal] confirm_close`** (2026-09-13): ghostty's `confirm-close-surface`, default
  on, on `Theme::behaviour`; ruled in decisions/terminal.md ("Closing a busy shell asks").

- ✅ **`[terminal] natural_editing`** (2026-09-13): ghostty's macOS "natural text editing"
  keybinds (⌘← ⌘→ ⌘⌫ ⌥← ⌥→, plus ⌥⌫) as one switch, default on, on `Theme::behaviour`;
  ruled in decisions/terminal.md ("The line is edited with the Mac's keys").

- ✅ **`[remote] hdr` is gone** (2026-09-25): it stopped reaching a stream when the stream
  became 8-bit only, and it went with the wire fields that echoed it; ruled in
  decisions/video.md ("HDR is not carried"). A file that still names it gets the unknown-key
  warning.

- ✅ **Every key is rebindable in `[keys]`, per context** (2026-09-29). About 110 bindings were
  hard-coded in two lists, and the file had no say in them, where Warp and Zed let every key
  be moved. Now `slopty_ui::keymap` is the one table of every command a key can run, with its
  default chords, and `[keys.<context>] <action> = …` lays the file over it: a chord in the
  palette's syntax, a list of them, or `""`/`"none"` to unbind. What the file does not name
  keeps its default, so a default is written once and a changed default reaches everyone who
  did not move it. The contexts are the app's own (`app`, `workspace`, `terminal`, `file`,
  `conversation`, `folder`, `search`, `page`), each command bound in the GPUI contexts it
  needs (the workspace's ⇧⌘F in any text field too), so a rebind moves it everywhere it
  answered. Chords are read by one parser (`keymap::chord`, the quick terminal's until it was
  removed on 2026-09-30). One chord runs one command per context:
  the file's command takes it from a default, the first of two of the file's own keeps it, and
  either way the settings notice names both, as it names an unknown context, action or key; a
  nested context (a terminal inside the workspace) is not a clash, as GPUI's deeper binding
  wins there by design. A saved file rebinds at once: `keymap::install` replaces only the
  bindings tagged as the keymap's, keeping gpui-kit's and the menu's ahead of them. The
  settings form's Keyboard page records a chord through a keystroke interceptor, so a chord the
  app binds is recorded rather than run, with ⌫ to unbind, Esc to cancel and a reset that takes
  the line out of the file. Ruled out: Zed's JSON keymap file (a second file, and settings are
  one TOML file here), action names as GPUI spells them (`workspace::NewTerminal` needs quoting
  in TOML and names no context), and multi-key sequences (no command wants one yet). Tests:
  `key_bindings_by_context` (slopty-settings), `the_table_reads_and_holds_no_clash`,
  `the_file_is_told_what_it_named_wrong`, `the_file_overrides_unbinds_and_wins_a_clash`,
  `installing_rebinds_in_place`, `the_palette_shows_the_chord_in_effect` (slopty-ui keymap),
  `a_chord_is_recorded_into_the_file` (the form), `saved_keys_rebind_at_once` (slopty-app).

- ✅ **The file's chord wins wherever it binds, and only a chord the app can own is recorded**
  (2026-09-29). A default in a context nested inside the file's chord (the terminal's ⌘K
  under `[keys.workspace]`, any context under `[keys.app]`) would win there, as GPUI's deeper
  binding does, so the default gives the chord up and the notice says so; two defaults in
  nested contexts are still the table's layering. The Keyboard page records only a chord with
  ⌘ or ⌃, or an F key, except for a folder's commands, whose rows bare keys walk: a letter or
  Tab bound in the workspace would take it from every shell. Tab and ⇧Tab end recording and go
  along the ring; a key with no name says "This key can't be bound"; a keystroke in another
  window (a popped-out tile) is left to it. A file that does not parse changes nothing, so the
  last good keys stay rather than the defaults'. The menu bar is rebuilt from the keymap at every rebinding:
  AppKit runs a menu item's key equivalent before any binding, so a menu built once kept
  running the old chord (`slopty_app::set_app_menus`). The keymap words each command's chord
  once as it is made (`Keymap::label_of`), so a frame looks labels up. Tests:
  `the_files_chord_wins_over_a_deeper_default`, `a_chords_words_come_with_the_keymap`
  (keymap), `recording_takes_only_a_chord_the_app_can_own`,
  `a_folders_command_takes_a_key_alone` (the form), `the_menu_bar_follows_a_rebinding`,
  `a_broken_file_keeps_the_keys` (slopty-app).

- ✅ **gpui-kit's keys around every view stand aside in a shell and a remote window**
  (2026-10-03). GPUI runs a key's binding before any view hears the key, and gpui-kit binds
  chords in its window root, around every view: Tab and ⇧Tab walk the focus, and ⌃C copies a
  selection on every target but macOS, the iPhone and iPad among them. So Tab and ⇧Tab never
  reached a shell or a remote window, and once the terminal answered the Edit menu's Copy
  (`09cba6b3`), ⌃C on the simulator copied nothing and interrupted nothing. `keymap::install`
  now unbinds, by its action's name, every binding outside the table that holds around every
  view on a chord without ⌘, inside `Terminal` and `Screen` alone, where the program and the
  worker take every key the table leaves them. The kit's fields, menus and ⌘ chords keep
  theirs, and the list follows the kit rather than naming its chords. Ruled out: a `NoAction`
  binding, which in gpui-fast masks only bindings of a source as weak as its own (the
  keymap's meta is weaker than the kit's untagged bindings); and making the terminal's Copy
  pass when nothing is selected, which would still copy over ⌃C with a selection. gpui-kit's
  root should bind ⌘C on iOS as its text fields already do; that is the fork's to change.
  Amended the same day: the fork's roots now bind ⌘C on iOS as on macOS (gpui-kit
  `c9c4b5b2`), so on an iPad ⌘C copies the kit's selections and ⌃C was never the kit's. The
  derivation stays as it is and now releases only Tab and ⇧Tab there, since it follows the
  kit's bindings rather than naming them.
  Tests: `the_kits_keys_around_a_shell_go_to_the_program` (terminal),
  `the_kits_tab_around_a_remote_window_goes_to_the_worker` (screen),
  `a_hardware_keyboard_on_the_simulator_arrives_through_presses` and
  `the_soft_keyboard_on_the_simulator_types_through_insert_text` (`cargo xtask e2e ios`).

- ✅ **`[terminal] agent_alert`: an agent's alert sounds only from the background** (2026-10-01).
  An agent that needs the human (a permission, a question, done) sounds the alert sound and
  bounces the Dock icon only while no Slopty window is active, as Mail and Messages behave. In
  front, the tile's badge, the inbox and the status bar already say it. The setting (default
  on) turns it off altogether, beside `bell_alert` under Behaviour
  (`slopty_app::settings::agent_alerts`). Test: `an_agent_alerts_only_in_the_background_and_when_asked`.
  *Amended 2026-10-01 (GUI-first plan §3 item 8):* the switch is three ways, `"never"`,
  `"hidden"` (the default, the rule above) and `"always"`, which sounds in front of the window
  too for a person who works with Slopty behind a terminal of its own. A `true` or `false` left
  in a file is an error, as any other value the field does not take (`AgentAlert::sounds`).

- ✅ **`[clipboard] sync` and `[clipboard.workers]`: the clipboard is shared per worker**
  (2026-10-01). `sync` (default on) shares this device's clipboard with every worker.
  `[clipboard.workers]` maps a worker's name to on or off, and wins over `sync` for that
  worker. The form shows `sync` under Input ▸ Clipboard; the map is an open table, edited in
  the file. For a worker the clipboard is not shared with:
  - The worker is told at once that its clipboard is no longer wanted (`Watch(false)`).
  - Its offers are dropped, so its copies stay its own.
  - Its fetch of this device's clipboard is answered `Unavailable`.
  - A paste into its tiles carries nothing ahead of the keys, so ⌘V there pastes the
    worker's own clipboard, and a shell pastes this device's text as typing.
  The paste hooks read the set as they run, so a change holds for tiles opened before it.
  Tests: `the_clipboard_is_shared_by_default_and_per_worker_by_name` (slopty-settings) and
  `workspace::tests::remote::a_worker_the_clipboard_is_not_shared_with_neither_hears_nor_gives_it`.

- ✅ **`[web] inspector`: Web Inspector on a browser tile's page, on by default** (2026-10-02,
  readiness audit item 21). The inspector was on only in debug builds, so a shipped app could
  not debug a worker's dev pages, which is a browser tile's main use. An inspector one can open
  is what a developer expects of a page, so the key defaults to on, and a debug and a release
  build behave the same: no `cfg!(debug_assertions)` decides it anywhere.
  - **What it governs.** On, a page is open to Safari's Develop menu (`isInspectable`), its
    menu has "Inspect Element" (WebKit's developer extras), and "Inspect page" opens WebKit's
    own inspector window. Off, none of them.
  - **A change follows on open pages**, through the theme's behaviour the app derives from the
    file: `isInspectable` and the palette's command change at once. The developer extras are the
    page's configuration, copied when the web view is made, so "Inspect Element" in an open
    page's menu follows when the page opens again.
  - **In the form**: Streams ▸ Web pages, beside remote windows and desktops, since both show
    what runs on a worker.
  - Tests: `web_inspector_is_on_unless_the_file_says_not` (slopty-settings), and on the main
    thread, where a `WKWebView` is made,
    `main_thread::web_inspector_opens_on_a_page_only_while_the_setting_is_on`
    (slopty-platform): a page made with it on or off, then turned either way. It opens no
    window and no inspector.


- ⛔ **A description wraps to two lines and is never cut** (superseded 2026-10-06 by "The
  settings pane is a macOS form"; 2026-10-04,
  `.research/rulings-2026-10-04.md` §4c). Each row's description was cut to one line, with the
  rest in a hover hint, so on a touch screen the rest could not be reached. The golden showed
  "Any monospace; JetBrains Mono is b…". This supersedes "A row holds one line" (`ui.md`,
  2026-09-28).
  - A row is its label with its control on one line, the control centred on the label, and the
    description under them. It is as tall as its words. Beside the sidebar the words keep to two
    thirds of the row (`DESCRIPTION_SHARE`, as Zed's settings do), so they read as one column
    down the page. On a phone's sheet, which has no sidebar, they take the row, since a third of
    a phone's width left empty would force every description to half its words.
  - The hover hint is gone, because every word is on the row. The control is still told the
    whole description for VoiceOver.
  - A description longer than two lines at the narrowest sheets it is written for is a copy
    defect, and a test names it with its length. Those sheets are 320 pt (an iPad's Slide Over,
    the narrowest iOS gives) and the List overlay's width, the narrowest that keeps the sidebar.
    The copy pass that made them fit is in `slopty-settings`' doc comments' first lines.
  - Tests: the two description tests went with the descriptions (2026-10-06); their successor
    is `settings_editor::tests::every_row_is_one_line_at_the_narrowest_sheet`, shaping with the
    platform's own text system.

- ✅ **Fewer settings: one alert, no frame-rate ceiling, no inspector switch, no external
  editor** (2026-10-04, the day's cuts; `docs/decisions/ui.md`, "One way to branch, and
  continuing when a limit lifts").
  - `[terminal] bell_alert` and `agent_alert` are one `[terminal] alert`: "never", "hidden"
    (the default: only while no Slopty window is in front) or "always", for a terminal's bell
    and an agent that needs the person alike (`slopty_app::settings::alerts`). The two said
    the same thing in two ways.
  - `[remote] fps` is gone: a stream asks for its screen's refresh up to 120, as the default
    did, and nobody lowered it.
  - `[web] inspector` is gone: every page is open to Web Inspector.
  - The Mac's "Open in editor" in the settings dialog is gone; the dialog's file face is the
    editor. `slopty settings init` still writes the commented defaults.
  - These supersede the `bell_alert`, `agent_alert` and `[web] inspector` entries above, and
    the "Open in editor" button of the settings dialog's entry.
  - Tests: `slopty_app::settings::tests::an_alert_sounds_only_in_the_background_unless_asked`,
    `slopty_settings` `the_clipboard_is_shared_by_default_and_per_worker_by_name` (the alert
    keys), `terminal_keys`, `remote_keys`.

- ✅ **`[client] editor`: a link that opens a file in the person's own editor** (2026-10-05,
  readiness G7, after the ruling in `.research/rulings-2026-10-04.md` that Slopty stays a
  light editor and hands heavy editing to the person's IDE).
  - **A link, not a command.** VS Code, Cursor and Zed each publish a link form for remote
    editing over SSH, and an iPhone or iPad can open a link but cannot run a command. Any
    editor with a link scheme works, so the setting is an open template, not a list of
    editors.
  - **Placeholders.** `{path}` is the file or folder on its machine, percent-encoded so a
    space, `#` or `?` in a name stays part of the path. `{host}` is the machine's name; a
    person who signs in as someone else writes `me@{host}`. `{line}` is the line in view,
    else 1. Examples: `zed://ssh/{host}{path}`, `vscode://vscode-remote/ssh-remote+{host}{path}`.
  - **Empty is the system's handler** for the file's type, the default.
  - **Checked as it is typed.** A link must start with its scheme and hold `{path}`, since one
    that cannot name the file opens nothing. The parser refuses it with that reason, and the
    settings form shows the refusal on the row.
  - The row sits under "This app" on the Network page, beside the server.
  - Tests: `slopty_settings` `the_editor_link_opens_the_file_on_its_machine`,
    `default_file_round_trips`, and `schema::tests::a_value_is_checked_by_its_key`.

- ✅ **`[remote]` is the bitrate ceiling alone** (2026-10-05, readiness deletions, feature
  audit #13). `muted` and `sharp_text` are gone.
  - **`muted`.** The stream's pill silences a worker's sound for all its streams, and the
    choice holds on the connection. A default for new streams was a second way to the same
    switch, and it needed the client's "not chosen yet" state only to keep a later tile's
    preference from undoing a choice. The sound now plays until the pill silences it.
  - **`sharp_text`.** Every HEVC stream from a Mac now asks for 4:4:4, and the worker's
    `ChromaGate` grants it only while the rate makes it the sharper picture
    (`docs/decisions/video.md`, "Full chroma follows the rate"). An iPhone or iPad asks for
    4:2:0 until its decoder is proven to take 4:4:4, as that ruling requires
    (`slopty_ui::screen::ASKED_CHROMA`). The ask costs the encoder nothing in time: the 10-bit 4:4:4
    session is 0.25 ms slower at the 1080p p50 and the same at 5K (`docs/MEASUREMENTS.md`,
    2026-10-05). The cost of 4:4:4 is bits, and the gate, which sees the rate, weighs that
    better than a switch the person sets once.
  - A file that still has either key loads, with an unknown-key warning for each.
  - Tests: `slopty_settings` `remote_keys`; `slopty-ui`
    `new_stream_settings_are_asked_of_a_live_stream` (full chroma asked);
    `slopty-client` `a_workers_streams_share_one_sound_and_its_mute`.


- ✅ **"Open in `<editor>`" on the file, folder and review tiles** (2026-10-05, readiness G7,
  built on `[client] editor` above).
  - **What opens.** A file opens at the caret's line, or at the line in view when the tile
    shows no text. A folder opens its selected entry, or itself while none is selected, as
    Finder's "Open With" takes the selection. A review opens the folder it reviews. Each
    path is written out whole under the machine's home first, since an editor's remote link
    takes no `~`.
  - **Which machine.** `{host}` is how SSH reached the machine when it was installed from
    here (`[user@]host[:port]`, kept by the deployer), else the machine's name, which the
    tailnet resolves.
  - **No link set.** A Mac opens the file with the system's handler for its type: this Mac's
    own path for a tile of this Mac, or the machine's place in Finder (its File Provider
    domain) for a path in its home. An iPhone or iPad opens links only, so it offers
    nothing.
  - **Where it is.** One palette line, named for the editor the link's scheme names ("Open
    in Zed", "Open in VS Code", "Open in your editor" for another, "Open with default app"
    with none). A tile answers the action only
    while it would open something, so the line shows on these three tiles alone and never
    as a dead row. No button carries it. The tiles have no menus of their own: the header's
    buttons are the tile's, and its other actions are the palette's.
  - **How it is wired.** The app tells a global (`slopty_ui::file::open_with::Editors`)
    the link from the settings and what it knows of each machine: its name and home from
    its link, its SSH name, its place in Finder and whether it is this Mac from the
    directory. A tile asks the global when the action runs. Asking whether to offer it
    writes nothing out, since a tile asks every time it draws.
  - Tests: `slopty-ui` `file::open_with::tests` (the link, the system handler's fallback,
    the label, and the offer agreeing with the open) and `file::tests::open_with` (each
    tile opening through the action, and a tile offering nothing before its machine is
    known); `slopty-app` `editors::tests`.
- ✅ **The settings get room, and the file is an advanced path** (2026-10-06,
  `.research/design-critique-astra-2026-10-05.md` findings 21 and 22). The dialog spent much of
  a small panel on navigation and framing, so several controls sat below the fold. "Edit as
  TOML" also stood in the foot as prominently as Done, the one way out.
  - **Size.** The settings and the project search share the larger overlay, now 800 × 720 at
    most. The settings' page asks for 13 two-line rows, so with its title and foot the dialog
    is about 800 × 600 where the window has room and smaller where it has not. The sidebar
    stays 168 pt. The page beside it is never under 480 pt: a window narrower than
    `settings_form::sidebar_from` (168 + 480 + the margins) stacks the form in one column.
  - **Type and rhythm.** A row's label is the action role (13/19, medium), its description
    the metadata role (12/18). A group's rows sit 12 pt in from their card. A group's label
    stands 24 pt from the group above it.
  - **The file.** "Edit as TOML" moves to the sidebar's foot, a quiet row with its braces
    symbol, as Zed keeps its settings file under its sections. The dialog's foot holds Done
    alone. Stacked in one column there is no sidebar, so the foot offers the file again. The
    form still applies each change as it is made, and Done stays honest.
  - **Keyboard rows** already read as the critique asked: the palette's words in the action
    role with the keycaps at the row's end, the action's id in the row's hint and its
    accessible description and still found by a search, and a second word only where a
    default was changed. Clashes are not marked on the rows, because the keymap already
    takes a chord from a default the file gives elsewhere and says so above the page
    ("One chord runs one command in a context", `keymap.rs`).
  - Tests: `settings_editor::tests::the_file_is_the_sidebars_advanced_path`,
    `settings_editor::tests::every_row_is_one_line_at_the_narrowest_sheet` (the description
    tests went with the descriptions, 2026-10-06).
    Goldens: `settings`, `settings-dark`, `settings-form`, `settings-keyboard`,
    `settings-about`.
- ✅ **A map setting is edited entry by entry, in the form the file already gives it**
  (2026-10-06, readiness G20). Two keys are maps by name: `clipboard.workers` (a machine's
  own clipboard setting) and `worker.acp` (ACP agents beyond the known ones). The form had
  no row for them, and the clipboard's per-machine toggle quoted its key by hand.
  - **The form.** The schema reads a map from its JSON Schema (`"object"` with
    `additionalProperties`) as `Kind::Map(inner)`, for a switch, text, list or number inner
    kind. The row's control is an "Add by name" field (↩ adds the entry and focuses it).
    Under it, each entry is one kit row: the name, the inner control (a switch, or a field
    that writes once typed, as every text row does), and the kit's remove button. Nothing in
    it is hand-styled: every value comes from the kit and the theme tokens.
  - **The file.** `slopty_settings::edit::write_entry` and `remove_entry` change one entry
    and touch nothing else. The map can be written under its own header
    (`[clipboard.workers]`), as dotted keys under its parent, or inline (`{ … }`). An entry
    is set in place where it already exists. A new entry goes beside the last one, in the
    same form. When the map has no entries yet, it gets its own header. Inline tables are
    split by hand, so their order survives: `toml::Table` sorts its keys. A name that is not
    a bare key is quoted by `key_text`, the single place a key is ever quoted. The last
    entry out takes its header (and the blank line before it) or its inline key, so no
    empty table is left behind. `settings::with_clipboard_shared` in the app now calls
    `write_entry`.
  - Tests: `edit::tests::a_maps_entries_read_in_every_form`,
    `edit::tests::an_entry_is_written_where_the_map_is`,
    `edit::tests::the_last_entry_out_takes_its_table` (comments and the order of other keys
    kept), `settings_form::map_tests::a_maps_entries_are_lines_to_set_add_and_take_out`,
    `settings_form::map_tests::a_command_line_entry_is_written_once_typed`, and
    `settings::tests::the_clipboard_is_kept_off_for_one_machine_by_name` (a quoted name set
    twice stays one line). Golden: `settings-input`. `clipboard.workers` sits under "Share
    the clipboard" in the Input page's Clipboard group. `worker.acp` goes on its table's
    home page, as any key the layout does not name does.

- ✅ **The settings are a macOS 26 pane** (2026-10-06; the person asked for settings that read
  as a macOS 26 pane; `.research/status-color-2026-10-06.md` §7). A title bar over both
  columns, a footer under them, and a hairline between every pair of rows made the dialog a
  stack of boxes.
  - **No title bar and no foot.** The sidebar runs the dialog's full height. The page column
    heads itself, as System Settings does: the page's name in the panel title role, on the
    search field's line, with Done at the line's end. A search or a single column is headed
    "Settings". The file's face has its own head: "Settings" with the file's name muted, then
    "Edit with controls", Cancel and Save. Neither head has a rule under it; the page fades
    under the head only while some of it is scrolled there (`gpui::edge_fade`).
  - **Groups are titled and spaced, not ruled.** A group's heading is the section role in the
    text's ink. Its rows stay one card, parted by a step of space, since each row's
    description already makes it a block. Row words are the chrome role at the regular weight,
    as System Settings sets them; descriptions stay metadata in `text_muted`.
  - **Both heads fit their room** (`kit::priority_row`). Done, Cancel and Save never leave.
    The way to the file, or back to the controls, gives way to its glyph alone where its words
    do not fit (curly braces, a gear).
  - Tests: `settings_editor::tests::{the_columns_start_together,
    the_file_is_the_sidebars_advanced_path, a_narrow_sheet_keeps_its_heads_whole}` and
    `settings_form::tests::a_groups_rows_are_one_ring_under_its_label`. Goldens: `settings`,
    `settings-dark`, `settings-form`, `settings-keyboard`, `settings-about`, `settings-input`.

- ✅ **The settings pane is a macOS form** (2026-10-06,
  `.research/premium-foundations-2026-10-06.md` change 7). The page was a grey plane carrying
  white cards, and every row stood two lines, its description under its title, with nothing
  between rows. It read as a web form. Most descriptions only said the title again ("Follow the
  system, or stay light or dark", "Empty keeps the theme's own").
  - **The page lies on the content plane.** The sidebar keeps its tone step (`panel`).
  - **A group is a ring.** A `border` hairline at `radii.md` runs round its rows, with no
    fill, as t3code's settings groups are drawn. The ring bounds a region, so it takes the
    region's hairline; the rules between its rows take the quieter `border_subtle`. Drawn in
    `border_subtle` as well, the ring all but vanished on the dark sheet. Its head is the section role in the
    secondary tone, a row tall, at the group's leading inset.
  - **A row is one line**, as System Settings draws it: the title in the action role at the
    start and the control at the end, 36 pt tall on a pointer and 44 under a finger. Rows are
    parted by an inset `border_subtle` hairline that starts where the titles do. Under a row stands only what is
    wrong with it: a value that was not written, in the error's colour, or a login item the
    system holds off.
  - **What a group must say is its footer.** The descriptions that restated their titles are
    gone from the page. What only a sentence can say (an empty colour keeps the theme's own,
    loopback and the tailnet are always let in) is said once under its group's ring, in the
    metadata role and the muted tone (`settings_form_schema::FOOTERS`). Every row's own words
    remain its control's description for VoiceOver and what a search finds, along with its
    group's footer.
  - This overrules S §7's "no rule between settings rows": that ruling assumed two-line rows,
    which need none, and one-line rows do.
  - Tests: `settings_form::tests::a_groups_rows_are_one_ring_under_its_label`,
    `settings_editor::tests::every_row_is_one_line_at_the_narrowest_sheet`.
