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
  change (test: the tail of `new_stream_settings_are_asked_of_a_live_stream`).
- ✅ **`[colors]` lays a palette over the theme** (2026-09-15). ghostty ships hundreds of
  schemes and every terminal takes a custom palette; Slopty's two variants were fixed. The
  section has `foreground | background | cursor | cursor_text | selection` and `ansi` (a
  list of up to 16), each a `"#rrggbb"` string (the `#` optional) or `""` for the theme's
  own (`Color(Option<[u8; 3]>)`, a malformed value is a parse error like an unknown
  appearance, so a typo never paints half a palette). `theme_for` lays them over
  `TerminalPalette` in both appearances (`colour_the_terminal`); a custom cursor takes
  black or white text under it (`Rgb::is_light`) unless `cursor_text` says otherwise. Ruled
  out: a scheme name (no bundled scheme table to pick from yet; a file of 16 hex strings
  is what every scheme repository exports) and per-appearance sections (the theme's
  appearance switch is for the chrome; a palette is chosen once). Since the driver's
  colours answer OSC queries and follow theme changes, a program asking for its
  background hears the custom one. Tests: `colour_keys`, `custom_colours_lay_over_the_theme`.

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
