# Decisions — Settings

See `docs/DECISIONS.md` for the legend. Newest entries go at the end.

- ✅ **One TOML file, every key optional** (2026-09-05): `<data dir>/settings.toml`, the
  directory the client identity already uses (`SLOPTY_DATA_DIR`, else
  `~/Library/Application Support/Slopty`; `slopty_settings::data_dir` is now the one copy the
  app and the CLI share). `slopty-settings` is serde + `toml` 1.1.5 only, no GPUI, so the CLI
  and tests use it without a window. `#[serde(default)]` on every struct makes the file and
  any subset of it valid; unknown keys are found by diffing the parsed table against the
  serialised defaults (no hand-kept key list) and reported as warnings; a parse or type error
  yields the defaults plus a one-line error (`Loaded { settings, warnings, error }`), never a
  crash. `slopty settings init` writes a commented file generated from `Settings::default()`
  and covered by a round-trip test. `terminal.scrollback_lines` is host-side and not here.

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
- ✅ **`[font] mono_line_height`, `[terminal] bell_alert | cursor_blink | paste_protection`** (2026-09-15): the line height
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
