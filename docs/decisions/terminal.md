# Decisions — Terminal

See `docs/DECISIONS.md` for the legend. Newest entries go at the end.

- ✅ **VT state on the host, rows on the wire.** See ARCHITECTURE §2. Precedents: mosh, zellij,
  wezterm mux. Rejected: raw bytes + client VT (replay on reconnect, iOS needs the engine, slow
  links pile up bytes).

- ✅ **Engine: libghostty-vt via `Uzaaft/libghostty-rs` ≥ `5988a0b7`.** Verified 2026-09-04:
  crates.io 0.2.1 (2026-07-18), repo pushed 2026-09-02, soundness issue #75 fixed by PR #81
  (null-pointer slice in `ClipboardWrite::contents`, `from_utf8_unchecked` on OSC 52 payload).
  Author is an active ghostty contributor (27 upstream contributions). The C API header says
  "not yet stable" — we pin one ghostty commit and vendor the source (`GHOSTTY_SOURCE_DIR`, no
  network at build time). Only the host builds it. Zig 0.16.0 required (installed).
  Alternative kept as a differential-test oracle: `rio-vt` 0.5.26 (pure Rust, extracted 2026-07).
  2026-09-12: **ghostty bumped 752 commits to main `44f2a44df` (2026-09-10) through a binding
  fork** `aislopware/libghostty-rs` branch `slopty` (upstream `Uzaaft/libghostty-rs` never moved
  past our pin, and its bindings are checked in, not generated at build time). The fork commit
  re-pins `GHOSTTY_COMMIT`, regenerates `bindings.rs` with the repo's own `gen-bindings` tool
  (`GHOSTTY_SOURCE_DIR=<ghostty> cargo run -p libghostty-vt-sys --features bindgen-tool --bin
  gen-bindings`) and adapts the wrapper: every C enum is typed `: int` now, so the wrapper enums
  are `repr(i32)`; `ghostty_render_state_colors_get` is gone, colors come through
  `ghostty_render_state_get(COLORS)`. The wrapper's 34 tests pass; slopty-engine and slopty-pty
  compile and test unchanged (the terminfo source did not move, only gained a content hash).
  `xtask/upstream.toml` tracks the binding as a third fork: `check` also prints whether the
  binding's `GHOSTTY_COMMIT` equals `vendor/ghostty` and how far the vendored source is behind
  ghostty main; `sync` build-checks it with `cargo check -p libghostty-vt --all-targets`
  (`GHOSTTY_SOURCE_DIR` comes from our `.cargo/config.toml`, so the checkout under `.research/`
  builds against `vendor/ghostty`). A ghostty bump is therefore: move the submodule, re-pin +
  regenerate in the binding fork, push, `cargo update -p libghostty-vt`, gate.
  New C API worth adopting: native search (`ghostty_search_new/set/tick/get`, whole-terminal,
  incremental) — ⏸ after reading `search.h` at `44f2a44d` (2026-09-12): its matching is
  "byte-exact except ASCII letters", so a plain needle would fold case only for ASCII where
  ours folds Unicode, and regex needles would still take our path, which leaves two searches
  with two answers for one needle; the prize is the formatter's 10 ms of a 16 ms search over
  50 000 lines (MEASUREMENTS "search over a full history"), paid only while typing a needle
  into a history that size. Revisit if ghostty adds Unicode folding or the search bar becomes
  a live follow of a streaming scrollback (the incremental feed/tick split is built for that);
  `row_iterator_next_dirty`
  for the apply path; `ghostty_terminal_paste` with its Kitty clipboard/paste safety checks
  (`GHOSTTY_REJECTED`); semantic prompt state read straight from the C API; Kitty clipboard
  protocol reads (`clipboard_read` effect) behind a permission prompt.

- ✅ **Rendering is our GPUI element**, not sugarloaf or ghostty's renderer. Glyph shaping via
  GPUI's text system with our own cell layout, sprite glyphs for box drawing, per-row dirty
  tracking (zed's terminal element reshapes every frame; we won't).

- ✅ **Font metrics follow ghostty's `src/font/Metrics.zig`**, ported as the pure function
  `slopty_ui::terminal::metrics` (`Face` in, `Metrics` out; no window, no IO), verified
  2026-09-05 against the vendored Zig. The derivation, all in **device pixels** (the face is
  measured at `font_size × scale`, and `Grid` divides back to points so a cell is a whole
  number of device pixels):

  ```
  cell_width  = round(advance('M'))                       min 1
  cell_height = round(ascent - descent + line_gap)         min 1
  baseline    = round((line_gap/2 - descent) - (cell_height - face_height)/2)   [up from the bottom]
  underline_thickness     = ceil(post.thickness ?? 0.15 · ex)   min 1
  underline_position      = round((cell_height - baseline) - (post.position ?? -thickness))
  strikethrough_thickness = underline_thickness
  strikethrough_position  = round((cell_height - baseline) - (ex + unrounded thickness)/2)
  overline y = 0, overline/box thickness = underline_thickness, cursor_thickness = 1
  ```

  Rounding, not ceiling: the error stays under half a pixel and the apparent spacing matches
  between a 1× and a 2× display; the baseline is then centred in the rounded cell, so the text
  is inset (or overhangs) equally top and bottom. `Metrics::set_cell_height` is ghostty's
  `adjust-cell-height`: it splits the added pixels between top and bottom, giving the odd one
  to the side the text sits nearer. Estimates fill in what a font does not say — cap = 0.75 ·
  ascent, ex = 0.75 · cap, underline thickness = 0.15 · ex, underline position = −thickness.
  (Until 2026-09-06 GPUI's `TextSystem` exposed neither the line gap nor the `post` table, so
  those estimates always applied in production; the fork now exposes them, see "The font's
  own line gap and underline" below.) Two fonts, two sizes, two displays, in device pixels
  (`cargo nextest run -p slopty-ui terminal::metrics`; ghostty's formula recomputed
  independently gives the same numbers). The first four JetBrains Mono rows are the face
  without its `post` table (the estimates); the "tables" rows are what the app measures:

  | font | pt | DPR | w | h | baseline | underline y/thick | strike y/thick |
  | --- | --- | --- | --- | --- | --- | --- | --- |
  | JetBrains Mono (no post) | 13 | 1 | 8 | 17 | 4 | 14 / 2 | 9 / 2 |
  | JetBrains Mono (no post) | 13 | 2 | 16 | 34 | 7 | 29 / 3 | 19 / 3 |
  | JetBrains Mono (no post) | 15 | 1 | 9 | 20 | 4 | 17 / 2 | 12 / 2 |
  | JetBrains Mono (no post) | 15 | 2 | 18 | 39 | 8 | 33 / 3 | 22 / 3 |
  | JetBrains Mono (tables) | 13 | 1 | 8 | 17 | 4 | 15 / 1 | 9 / 1 |
  | JetBrains Mono (tables) | 13 | 2 | 16 | 34 | 7 | 31 / 2 | 20 / 2 |
  | JetBrains Mono (tables) | 15 | 1 | 9 | 20 | 4 | 18 / 1 | 12 / 1 |
  | JetBrains Mono (tables) | 15 | 2 | 18 | 39 | 8 | 36 / 2 | 23 / 2 |
  | Menlo | 13 | 1 | 8 | 15 | 3 | 13 / 1 | 8 / 1 |
  | Menlo | 13 | 2 | 16 | 30 | 6 | 26 / 2 | 16 / 2 |
  | Menlo | 15 | 1 | 9 | 17 | 4 | 14 / 1 | 9 / 1 |
  | Menlo | 15 | 2 | 18 | 35 | 7 | 30 / 2 | 19 / 2 |

  Consequences in the element (`crates/slopty-ui/src/terminal/element.rs`): the row height is
  the font's own, so `typography.mono_line_height` **defaults to 1.0** and now means ghostty's
  `adjust-cell-height` percentage rather than a multiplier over the point size; underline and
  strikethrough are painted as quads at these offsets because GPUI hardcodes its own
  (`text_system/line.rs`: underline at `baseline + descent · 0.618`), leaving only the curly
  underline to GPUI (drawing a wave is its alone); the bar and underline cursors take
  `cursor_thickness` (one device pixel, as ghostty); and `TermSize.metrics` on the wire is now
  the cell in **device** pixels, which is what `ws_xpixel`/`ws_ypixel` are supposed to carry —
  and so is the offset in a pixel mouse report (`CellMetrics::pixel_at`), since the host divides
  one by the other. Two more consequences of deriving rather than guessing: the glyphs are
  painted on the derived baseline (GPUI centres a line in the box it is given, which is close to
  but not the font's baseline, so the origin is offset by the difference — exact, and per row,
  so a fallback font still lines up with its decorations); and a zoomed grid is the unzoomed one
  **scaled**, never re-derived, because the columns that fit were counted with the unzoomed
  cell — re-deriving rounds the cell up and clips the last column (at 13 pt, DPR 2, zoom 0.3 the
  cell came out 2.5 pt against the 1.2 pt the item had room for).

- ✅ **The font's own line gap and underline, through the fork** (2026-09-06, fork commit
  `876a96f`). font-kit's Core Text loader had always read `CTFontGetLeading`,
  `CTFontGetUnderlinePosition` and `CTFontGetUnderlineThickness` (the `hhea` line gap and the
  `post` table) into GPUI's `FontMetrics`; only the accessor was missing. The fork adds
  `TextSystem::font_metrics(font_id) -> FontMetrics` plus per-size `line_gap`,
  `underline_position`, `underline_thickness` beside `ascent`/`descent`, on macOS and iOS alike
  (both platform text systems go through font-kit). `measure` in the terminal element now
  fills `Face::{line_gap, underline_position, underline_thickness}` from them and leaves an
  estimate only where the font says zero (ghostty's rule: `post.thickness ?? 0.15 · ex`).
  Effect on the bundled JetBrains Mono (`post` underlinePosition −155, thickness 50 per 1000
  em; `hhea` line gap 0): the underline moves one pixel down and thins from 2 to 1 device
  pixel at 13 pt on a 1× display (2 px at 2×), the strikethrough thins with it; the cell,
  baseline and line height do not change (no line gap). Menlo (`post` −130 / 90, line gap 0)
  already matched the estimate's rounding at these sizes. Verified end to end, not by pixels:
  `dump.terminals[].face` reports the measured face (device pixels per em, ascent, descent,
  line gap, underline position/thickness or `null` for an estimate) and the macOS and
  simulator self-tests assert JetBrains Mono's numbers to the em (`check_jetbrains_mono_face`:
  ascent 1.020, descent −0.300, line gap 0, underline −0.155 / 0.050). Goldens: see
  MEASUREMENTS.md "font truth" for the moved pixels.

- ✅ **PTY custody in a tiny separate daemon** (`slopty-ptyd`), masters handed to the worker by
  `SCM_RIGHTS` (rustix `sendmsg`/`recvmsg`, which hand back owned descriptors; `sendfd` and
  later `nix` dropped, since rustix already covered the rest, and macOS has no
  `MSG_CMSG_CLOEXEC` so CLOEXEC is set by hand either way). ptyd drains the master into a
  bounded ring (4 MiB default) while detached. Verified 2026-09-04 by an end-to-end test
  (`apps/slopty-ptyd/tests/roundtrip.rs`): spawn → detached backlog → attach → connection loss →
  resume → reattach → exit status. macOS detail: `TIOCSWINSZ` on a fresh master fails with
  `ENOTTY` until the slave has been opened once; `Pty::open` sizes through the slave.

- ✅ **Absolute line numbering via a tracked grid ref** (see ARCHITECTURE §2). Verified by engine
  tests: 20 lines through a 4-line scrollback keep `epoch` 0 and contiguous indices.

- ✅ **`KeyCode` is generated from ghostty's key list** (`for_each_key_code!` in slopty-proto) so
  the engine mapping is exhaustive at compile time. Our hand-written W3C list had drifted
  (letters named `KeyA` vs `A`, missing numpad/browser keys).

- ✅ **Terminal type**: `xterm-ghostty` when its terminfo is installed, else `xterm-256color`.
  ghostty's entry (270 capabilities, three names) is ported to `slopty_pty::terminfo` as const
  data and rendered exactly as `Source.zig` renders it; the rendered source is an insta
  snapshot (`crates/slopty-pty/tests/snapshots`), so bumping the vendored ghostty shows up as a
  diff instead of a silent change in what programs are told. **ptyd compiles it on start-up**
  (2026-09-05): `tokio::spawn` right after `bind`, `/usr/bin/tic -x -o <db> -` with the source
  on stdin — the absolute path, never a `tic` from `PATH`. The database is `$HOME/.terminfo`,
  or `$SLOPTY_TERMINFO_DIR` when a test or a sandboxed run names one — and when it is set it is
  the *only* database consulted, so such a run answers from its own and not from whatever the
  machine happens to have (the app self-test points both it and `TERMINFO_DIRS` at the stack's
  temp dir, so no run touches the developer's home).
  A child is given `TERMINFO=<dir>` when — and only when — that override is in play: `TERM` and
  the search path have to agree, and a database ncurses knows nothing about would otherwise
  leave the shell advertising `xterm-ghostty` and unable to find it. Without the override the
  inherited `TERMINFO` is cleared, since `default_term` answered from the places ncurses
  searches by itself.
  Idempotent: `installed()` looks for `78/xterm-ghostty` or `x/xterm-ghostty` under the same
  directories the lookup searches, and does nothing when it is there. Nothing blocks on it —
  `default_term()` is read per spawn, so a shell that starts before `tic` finishes simply gets
  `xterm-256color`. On macOS `tic` warns about the description field and still exits 0, so only
  a non-zero status is an error.

- ✅ **⌘-click on a file path opens it in the shell's editor** (2026-09-12). A compiler, a
  linter or a grep prints `src/main.rs:12:5`; every Mac terminal lets the human go there
  with ⌘. Rulings: (1) `url::path_at_col` reads the run under the cell as a path when it has
  a `/` or a known source extension and no URL scheme (`SOURCE_EXTENSIONS`, so prose like
  "e.g." is left alone), takes a trailing `:line[:col]` off it and drops prose punctuation
  after it; a URL under the cell still wins; (2) the click types `${EDITOR:-vi} +12
  'src/main.rs'` at the prompt and ↩ — the editor is the host's, the path is relative to the
  shell, and single quotes keep any name whole (`url::shell_word`) — rather than asking the
  host for the file: what opens is the human's editor in the human's shell, on the phone too;
  (3) while a command is running there is no prompt to type at (`TermState::command_running`,
  from the shell integration marks), so the path opens as a **file card** on the canvas
  instead (amended 2026-09-12 once cards existed; before that it went to the clipboard, which
  left the human to find a place to paste) — the view emits `ViewFile` with the path as
  printed and the canvas makes it absolute against the shell's directory as the host last
  reported it (`canvas::absolute_in_session`: OSC 7, else where the shell started), since only
  the canvas holds the session summaries; (4) ⌘-hover underlines a path as it does a link
  (`link_highlight`). Tests: `paths_are_found_with_their_line_and_nothing_else_is` (the
  reader), `cmd_click_on_a_path_opens_it_in_the_shells_editor` (the click, headless),
  `cmd_click_on_a_path_while_a_command_runs_views_it` (nothing typed, `ViewFile` raised),
  `a_shells_relative_path_opens_a_card_in_its_directory` (the canvas resolves it).
  Amended 2026-09-28 with folder tiles: a path printed with a trailing `/` (`ls -F`, a
  completion) is a directory, which no editor opens, so it too goes to the canvas as
  `ViewFile`, and the canvas opens a folder tile
  (`cmd_click_on_a_directory_views_it_instead_of_typing`).
- ✅ **Links: OSC 8 first, text scan second** (2026-09-05). The engine reads the URI of every
  linked cell with `ghostty_grid_ref_hyperlink_uri`, gated on the row's `has_hyperlink` page
  flag (a false positive costs one extra check per cell, a clean row costs nothing) and on the
  cell's own flag, both on the render path (`Point::Viewport`, the host never scrolls the
  viewport) and on the scrollback fetch path (`Point::Screen`). Runs, not ids: `Line::links`
  is `Vec<Hyperlink { col, len, uri }>` and the per-cell `Option<HyperlinkId>` that
  `slopty-grid` had carried unused is gone. Measured (MEASUREMENTS.md, same date): a
  link-free 80×24 full frame went from 15 477 to 13 581 bytes (−12 %), because postcard
  spends one byte per `None` and the empty run list costs one byte per *row*. A spacer tail
  continues its wide character's run. `PROTOCOL_VERSION` 5 → 6. Client: `url::link_at_col`
  returns the OSC 8 run when there is one, else the plain-text URL with the columns it covers
  (`text_link_at_col`, offsets mapped back to cells, a wide cell's spacer inside the span);
  ⌘-click opens it, and while ⌘ is held the run under the pointer is underlined
  (`TerminalView::link_highlight` → a 1 px quad in `TerminalElement::paint`, so the shaped-line
  cache is untouched; `on_modifiers_changed` plus the modifiers on every move keep the state
  right whether ⌘ goes down before or after the pointer arrives). Covered by
  `osc8_links_become_runs_on_screen_and_in_history`, `plain_rows_carry_no_link_runs`,
  `links_are_found_by_column_and_clipped_on_resize`, `osc8_runs_win_over_the_text_scan`,
  `text_links_come_with_their_columns` and the `worker_lines_links` golden. macOS only for now:
  the phone key bar arms ⌘ for remote windows but not for terminals, so a tap has nothing to
  read; long-press stays selection. On the phone (2026-09-05) the terminal key bar has a ⌘
  key beside ⌃: it arms one tap (`TerminalView::set_sticky_command`), the next left press
  opens the link under it through `cx.open_url` (`gpui_ios`'s `UIApplication.openURL`, in the
  fork) and disarms; covered by `sticky_command_opens_the_link_under_the_next_tap`. A path
  under that tap (2026-09-12) opens as a **file card**, prompt or not — the editor ⌘-click
  types on a Mac is `vi` in a phone-sized grid, and the card is what the phone can read —
  covered by `sticky_command_views_the_path_under_the_next_tap`. The armed ⌘ with the bar's
  ↑ / ↓ (2026-09-13) is ⌘↑ / ⌘↓ — between prompts, in a shell or a conversation — and
  disarms; nothing goes to the program (`TerminalView::press`, in
  `a_driven_view_steps_between_its_prompts`).

- ✅ **Command blocks from OSC 133, shell integration injected by ptyd** (2026-09-05).
  *Injection:* the `ZDOTDIR` bootstrap every terminal uses (Kitty, Ghostty, WezTerm); the
  scripts are Slopty's own (Ghostty's zsh files are GPLv3, inherited from Kitty, so they
  were not copied). `.zshenv` restores `ZDOTDIR` (the original travels in
  `SLOPTY_ZSH_ZDOTDIR`), sources the user's `.zshenv`, then `slopty-integration.zsh` for
  interactive shells; `.zprofile`/`.zshrc`/`.zlogin` load from the user's directory as
  before. `A` and `B` live inside `PS1` (`%{…%}`) and `A;k=s`/`B` inside `PS2`, so zle
  redraws keep them; the precmd hook moves itself to the end of `precmd_functions` each
  time so a theme that rebuilds `PS1` in its own precmd cannot drop them; `C` is printed by
  preexec, `D;$?` by precmd only when a `C` is open (so a bare Enter reports nothing).
  Learned: `status` is a read-only zsh special, use another name. `install` writes the
  scripts (compiled in with `include_str!`) under `$SLOPTY_DATA_DIR/shell` (else `shell/`
  beside the socket) on every daemon start, rewriting an edited or stale file, so a running
  install never reads the source tree; it also reads the daemon's environment once into a
  `ShellIntegration` (`enabled`, the daemon's own `ZDOTDIR` and `XDG_DATA_DIRS`) so the
  per-spawn decision `apply` is pure: it takes the program, argv, arg0 and the session's
  environment and returns an `Injection` (argv, arg0, extra variables) that `Pty::spawn_with`
  applies. zsh: only when the resolved program's basename is `zsh` (the login shell, an
  explicit `zsh`, and the `$SHELL -lic` path from 52725da all qualify; `-c` shells load the
  hooks but never reach precmd, so they print no marks). Opt-out
  `SLOPTY_NO_SHELL_INTEGRATION=1` (anything but empty or `0`) in the daemon's environment or
  the session's, for all three shells. Verified by `an_interactive_zsh_emits_prompt_marks`
  (real `/bin/zsh -i` on a PTY: A/B/C, `D;1` after `false`, `ZDOTDIR` empty again, the
  user's `.zshenv` ran), `install_writes_the_bundled_scripts_and_is_idempotent` and
  `opt_out_from_the_daemon_or_the_session`.
  *bash* (2026-09-05): bash has no `ZDOTDIR`; the only hook that leaves the user's files
  alone is `--rcfile`, which bash reads *instead of* `~/.bashrc`, and which it ignores for
  login shells. So `apply` puts `--rcfile <shell>/bash/slopty.bash` at the front of argv
  (GNU long options must precede the short ones or bash says `--: invalid option`), strips
  `-l`/`--login` and the leading dash of arg0 and hands that fact over as
  `SLOPTY_BASH_LOGIN=1` (likewise `--noprofile`/`--norc` → `SLOPTY_BASH_NOPROFILE`/`NORC`);
  the rcfile then does what bash would have done (`/etc/profile`, the first of
  `.bash_profile`/`.bash_login`/`.profile` for a login shell, else `.bashrc`), unsets the
  variables, and returns unless interactive. Shells given a script, `-`, `-c`, `-o`,
  `--posix` or their own `--rcfile`/`--init-file` are left untouched. Marks: `A`/`B` inside
  `PS1` (`\[…\]`) and `A;k=s` in `PS2`, wrapped by the *last* `PROMPT_COMMAND` entry so a
  prompt theme that rebuilds `PS1` in its own entry still gets them; `D;$?` by the *first*
  entry (bash 5's `PROMPT_COMMAND` array and bash 3.2's string both handled). `C` needs
  preexec, which bash lacks: a `DEBUG` trap of Slopty's own (MIT-clean, not bash-preexec's
  code) fires once per prompt, armed by the prompt hook and disarmed by the first command it
  sees, so a prompt with a five-command `PROMPT_COMMAND` prints one `C`, not five. If the
  user already loads bash-preexec (`__bp_imported`), Slopty registers with its
  `preexec_functions`/`precmd_functions` instead; if some other `DEBUG` trap is installed,
  Slopty keeps its hands off it and emits prompt marks only (no `C`/`D`). Works on the
  system bash 3.2 and Homebrew's 5.3. Verified on both by
  `an_interactive_bash_emits_prompt_marks_and_runs_the_users_bashrc` (A/B/C, `D;1` after
  `false`, `.bashrc` ran, `.bash_profile` did not, the `SLOPTY_BASH_*` variables gone) and
  `a_login_bash_reads_its_profile_and_still_marks` (arg0 `-bash`: the profile ran, `.bashrc`
  did not, still marks).
  *fish* (2026-09-05): fish sources every `<dir>/fish/vendor_conf.d/*.fish` for each entry of
  `XDG_DATA_DIRS`, so `apply` prepends `<shell>/fish` to the session's (else the daemon's,
  else the `/usr/local/share:/usr/share` default) `XDG_DATA_DIRS`; the user's `config.fish`
  loads as before, after the vendor files. fish ≥ 4.0 prints OSC 133 itself (ST-terminated,
  `A;click_events=1`, `C;cmdline_url=…`; the engine's scanner accepts both terminators and
  ignores the parameters), so on 4.x the snippet only sets `__slopty_integrated 1` and steps
  aside; on 3.x it wraps `fish_prompt` once (`functions --copy`) with `A`/`B` and hooks
  `fish_preexec`/`fish_postexec` for `C`/`D;$status`. Verified with the installed fish by
  `an_interactive_fish_emits_prompt_marks_and_runs_the_users_config` (skipped with a note
  when no fish is on the machine). Learned: fish answers `DA1`/`CPR` queries at start and
  waits up to 10 s for the reply, so a PTY test must answer them; and it walks `cwd` for
  `mise` configs, so the test chroots its `cwd` to the temp home.
  *Engine:* libghostty-vt's per-row `semantic_prompt` flag says "prompt row" but cannot
  separate two prompts on adjacent rows (a command with no output), and the `D` status is
  not exposed at all. `slopty_engine::osc133::Scanner` watches the bytes (state kept across
  reads, OSCs framed as libghostty frames them: see "The prompt-mark scanner reads OSCs as
  libghostty does", `A;k=s`/`k=c` not counted as starts); `write` feeds the terminal up to
  each mark, settles, and records the cursor's absolute line in `prompt_starts` /
  `exit_marks` (both pruned below `base`, both cleared with the epoch). A prompt row is
  `Prompt { exit }` only on a recorded start, else `PromptContinuation`; the status is the
  newest `D` within 4 rows above the start that no other start already claimed (the shell may
  print a blank line or the partial-line `%` between `D` and the prompt). Covered by
  `prompt_rows_carry_the_previous_commands_exit_status` (adjacent prompts, a `D` split
  across two writes, a gap row, a two-row prompt, the history path) and
  `captured_zsh_bytes_keep_output_rows_and_statuses` (bytes recorded from a real zsh:
  synchronized output, the `%` partial-line marker, `D` directly before the next `A`).
  *Wire:* `SemanticMark::Prompt` grew `exit: Option<u8>`; every other row still costs one
  byte, a prompt row with a status three (MEASUREMENTS.md: an all-prompt 80×24 frame is
  48 bytes larger than a blank one). The brief's `End { exit }` variant was not added: the
  `D` lands on the row the next prompt starts on, so a separate variant would collide with
  `Prompt` on the same row. `PROTOCOL_VERSION` 6 → 7, golden `worker_lines_marks`; the other
  goldens are unchanged because `Unknown` is still variant 0 and `client_hello` only moved
  its version byte.
  *Client/UI:* `TermState::prompt_before/after` walk the cached lines (uncached history is
  skipped, not fetched), `scroll_to_line` puts a line at the top, `last_command_output` is
  the run of `Output` rows above the newest prompt start with the blank tail trimmed
  (`prompt_navigation_and_last_output_follow_the_marks`). ⌘↑/⌘↓/⌘⇧C are Terminal-context
  bindings (`PrevPrompt`, `NextPrompt`, `CopyLastOutput`); the separator is a 1 px quad on
  the prompt-start row's top edge from the same prepaint pass as the selection
  (`separator_color`: fg at `alpha::FAINT` (12 %), the theme's `surfaces.error` token at
  `alpha::STRONG` (70 %) when the status is non-zero; `crates/slopty-theme/src/lib.rs`,
  `crates/slopty-ui/src/terminal/element.rs`), never on line 0. Search bar and selection are
  untouched. Headless:
  `cmd_up_and_down_walk_the_prompts_and_separators_follow` (separator rows and colours read
  from `painted_quads`, the three jumps and the return to following output) and
  `cmd_shift_c_copies_the_last_commands_output` (clipboard untouched without marks).

- ✅ **OSC 52: write only, system clipboard only, ≤ `MAX_CLIPBOARD_BYTES`** (2026-09-05).
  libghostty's `clipboard_write` callback (registered in `install_callbacks` next to the bell)
  hands over a normalised, decoded write; the engine keeps the `text/plain` representation of
  a `Standard` write and answers `Unsupported` for selection/primary (X11 notions with no
  client counterpart), the session drops writes over the pasteboard-sync ceiling (the same
  256 KiB constant from `slopty_proto::screen`; a whole file pasted through OSC 52 would sit
  ahead of every frame on the session stream) and broadcasts `TermEvent::ClipboardWrite` to
  every attached client, whose canvas writes it with `cx.write_to_clipboard`. Reads are not
  implemented, on purpose: libghostty never forwards a `?` request, and the dead
  `TermEvent::ClipboardReadRequest` / `TermRequest::ClipboardRead` pair (the client used to
  answer it with its clipboard, unprompted) is removed from the protocol so a future host
  cannot ask. Covered by `osc52_writes_to_the_system_clipboard_only` (standard, primary,
  selection, `?`) and the `worker_term_clipboard_write` golden.

- ✅ **A command block has a menu: copy the command, copy the output, run it again, select
  it — and the rows know where the command starts, protocol 28** (2026-09-12). Warp's
  blocks are the point of shell integration, and Slopty had only ⌘⇧C for the newest block's
  output; the typed command could not be told from the prompt at all, since the marks said
  which *row* a prompt started on but not which *column* the input began at (`133;B`). Read
  from libghostty: every cell carries a `CellSemanticContent` (`Prompt`, `Input`, `Output`),
  so the engine now notes the first `Input` cell's column on each prompt row. Rulings: (1)
  `SemanticMark::Prompt { exit, input }` and `PromptContinuation { input }` carry that
  column (`None` until something is typed) — on the continuation too, because a real zsh
  prompt is three rows and the command lands on the third; (2)
  `TermState::command_block(line)` reads a block from any of its rows: the prompt's rows up
  to the one with the input column (the command from there; later `Input` rows continue a
  multi-line command) and the output rows to the next prompt, blank tail trimmed; (3) a right
  click on a block row — when the program has not asked for the mouse, ⇧ overriding as for
  selection — opens a small menu at the pointer (`block-menu`, role Menu; items only for what
  applies: "Copy command", "Copy output", "Rerun", always "Select block"); Esc, any click
  or a pick closes it, and the click is not reported to the program; on the phone, which has
  no right button, the key bar's armed ⌘ then a tap on a block row with no link or path
  under it opens the same menu (2026-09-13; the link and the path keep their precedence, so
  the armed tap still reads what is under it first); (4) "Rerun" is a paste of
  the command followed by ↩ as a key — the shell sees exactly what the human would have typed
  (bracketed when it asked), so aliases, history and hooks all apply; (5) it is our own small
  menu (the tokens, the a11y roles, the tab ring) rather than gpui-kit's `ContextMenu`, whose
  element-state machinery adds nothing here. Wire: the two marks; goldens `worker_lines_marks`
  / `client_hello` re-accepted, PROTOCOL_VERSION 27 → 28. Tests:
  `prompt_rows_carry_the_previous_commands_exit_status` and
  `captured_zsh_bytes_keep_output_rows_and_statuses` (`engine`: the column on the row the
  command was typed on, `None` before typing, on both the screen and history paths),
  `prompt_navigation_and_last_output_follow_the_marks` (`client`: blocks from any row, the
  open prompt with no command), headless `a_right_click_on_a_block_offers_its_command_and_output`
  (the menu in the tree, each pick's effect on the clipboard, the paste + ↩, the selection,
  Esc and a click elsewhere).

- ✅ **A block scrolled past its prompt keeps its command in a sticky header** (2026-09-12).
  Warp pins the command of the block you are reading to the top of the viewport, so a
  screenful of output is never anonymous; without it a long `cargo test` or a paged log
  reads the same as any other wall of text. Rulings: (1) it is a GPUI child over the grid,
  not a row the element paints — one row high (`CellMetrics::line_height`), full width,
  panel colour, the command in the mono face and muted text, ruled under with the block's
  separator colour (red after a failure) so the header and the rule below the output agree;
  (2) it shows only when the top row is an output row of a block with a typed command
  (`TerminalView::block_header`: `command_block(index_at_row(0))` with its prompt above and a
  non-empty command) — on any prompt row the prompt itself is visible and the header would
  duplicate it; (3) it is a Button whose click scrolls the prompt to the top (`jump_to`, the
  same path as ⌘↑), so a reader lost in output has a one-click way back to what produced it;
  (4) the conversation view (⌘⇧L) never shows it — its transcript has no rows; (5) it reads
  `TermState::block_head` (the prompt's rows alone: prompt, exit, command, where the body
  starts), not `command_block`, which joins the block's whole output into a `String` — read
  every frame under a streaming `cat`, that was a 50 000-row copy per frame for one label.
  No wire change.
  Test: `a_block_scrolled_past_its_prompt_keeps_its_command_in_a_sticky_header` reads the
  header's bounds (`debug_bounds("block-header")`, the terminal's origin and width, one line
  high), its a11y Button label, its absence once ⌘↑ puts the prompt at the top, and the
  click.

- ✅ **⌘K clears the screen and the history, protocol 31** (2026-09-12). Every Mac terminal
  has it; here the engine lives on the host, so it is a request. Rulings: (1) the scrollback
  is erased by feeding `CSI 3 J` through the host session's own output path (engine, tap,
  `after_output`) as if the program had printed it — a checkpoint replay then lands in the
  same place, and no engine API is needed; (2) the screen is left to the shell: ⌃L as PTY
  input, which zsh, bash and fish all bind to repaint the prompt at the top (`CSI 2 J` erases
  in place in ghostty, so nothing is pushed back into history); a program that is not a
  shell gets the ⌃L it would have got from the keyboard; (3) the view resets its scroll
  offset and sends one `TermRequest::Clear`; ⌘K in the terminal context, "Clear the screen
  and history" in the palette. Tests: golden `client_term_clear`, the ⌘K assertion in
  `cmd_shift_c_copies_the_last_commands_output`, and the app self-test's ⌘K step (a real zsh
  through ptyd: the echoed output is gone and the cursor is back on the row a fresh prompt
  puts it — the prompt's height is measured before anything is typed, so a three-line theme
  passes too). What the step then caught: after the clear, the `sleep 6` badge never came.
  An erase in place keeps the line numbers, but the engine kept the `133;A`/`133;D` marks it
  had recorded on the erased rows, so the next prompt drawn over them read as a continuation
  of a start that no longer existed, and the client only counted a prompt as new when its
  index grew. Rulings: (4) the engine drops a recorded start (and the status on its row) the
  moment its row is no longer a prompt row in ghostty's eyes (the frame walk sees every dirty
  row, so an erase is noticed on the next frame; a row scrolled into history is untouched);
  (5) the client treats any change of the newest prompt's index as a new prompt, not only an
  increase. Tests: `a_screen_erased_in_place_drops_the_marks_of_its_rows` (engine bytes) and
  the ⌃L frames in `a_command_is_reported_when_it_leaves_its_prompt_and_when_the_next_prompt_starts`.

- ✅ **A slow command's row says how long it took** (2026-09-14). Warp writes a block's
  duration in its header; here the badge for a command that ended unwatched carried the
  time, and a watched one said nothing, so "did the build take three seconds or thirty"
  was a guess. Rulings: (1) `Effect::CommandFinished` names the row the command was typed
  at (`prompt`), and the view keeps the elapsed time by that row (`TerminalView::set_took`
  / `took`), only from [`TOOK_MIN`] (1 s) up — a quick command says nothing worth a caption;
  (2) the caption (now `kit::duration`: `3.2 s`, `2m 3s`, `1h 2m`; it was `took_label`'s
  `3.3 s`, `2 m 03 s`) is drawn at the right end
  of that prompt row by the element's prepaint as an overlay glyph run, the foreground at
  `alpha::TINT`, flush with the grid's right edge, and left out when the command's
  text comes within a cell of it (the text wins); the row keeps it through history; (3) rows are numbered
  per epoch, so a new epoch (a reflow, a reset, the alt screen) empties the map; the host
  is not asked (the marks carry no time); (4) the sticky block header carries the same
  caption at its right end (`block-header-took`), so a long output scrolled past its prompt
  still says how long its command took. Tests: kit `a_duration_reads_one_way`,
  headless `a_slow_commands_row_says_how_long_it_took` (the element's captions read back
  through a test-only counter on the shape cache),
  `a_block_scrolled_past_its_prompt_keeps_its_command_in_a_sticky_header` (the header's
  caption), `prompt` in the client's command test.

- ✅ **⌘⇧↩ reruns the last command** (2026-09-12). The block menu's "Rerun" needs a right
  click on the block; the command a human reruns most is the one that just finished, and
  Warp puts that on a key. Ruling: `TermState::last_command` is the command of the block
  before the newest prompt (marks only, `None` without shell integration or before the
  first command), and `TerminalView::rerun_last` types it through `run_text` (one paste,
  one ↩) exactly as the menu and the answer's "run" button do. ⌘⇧R was taken (arrange by
  repository), so ⌘⇧↩; the palette lists "Rerun last command". Test: an assertion in
  `prompt_navigation_and_last_output_follow_the_marks`.

- ✅ **A long shell command that ends unwatched badges its item** (2026-09-12). Agents
  already badge their title bar when they need the human; a `cargo build` or a test run left
  in a shell the human has panned away from ended silently, and Warp/iTerm both notify on
  exactly that. Rulings: (1) it is read from the shell-integration marks the client already
  holds, no wire change: `TermState::track_command` runs every frame on the prompt's rows
  alone (`block_head`), treats a command as running once the cursor has left the rows it was
  typed on (Enter moves it to the output or the next prompt; typing, including a multi-line
  command on `Input` rows, never does) and as finished when a newer prompt start appears,
  whose `exit` is the status the shell reported (`OSC 133;D`); a command that starts and
  ends inside one frame reports nothing, which is fine — it could not have been slow; (2)
  the first prompt of a new epoch (reflow, reset, alt screen) ends nothing: the numbering
  changed, so "newer" means nothing across it (`vim` returning from the alt screen therefore
  never badges, right for an interactive program); (3) the client measures with wall time
  in the view (`command_started: Instant`), not the host: the badge is about the human's
  attention on this client, and the state machine stays pure; (4) the canvas badges only
  when the command ran at least `SLOW_COMMAND` (5 s: shorter commands end before anyone has
  looked away) and its item is not the active one; `set_slow_command` exists for tests and a
  future setting; (5) the badge is a Button in the success tone ("done 12.3 s") or the warn
  tone on a non-zero status ("failed (1) 1 m 04 s", the row caption's `took_label` since 2026-09-14), between the chat pill and the agent
  badge, and a press activates the item, which is also what clears it (`activate`), so the
  human's look is the acknowledgement; (6) no system notification and no `needs-you` count:
  those mean an agent is waiting on the human, and a finished command is waiting on nobody.
  Tests: `a_command_is_reported_when_it_leaves_its_prompt_and_when_the_next_prompt_starts`
  (`slopty-client`, the state machine: typed-not-entered, entered, still running, the next
  prompt with its status, a new epoch), `a_long_command_that_ends_unwatched_badges_its_item`
  (headless canvas: the active item badges nothing, the other item's badge reads "done
  0.0 s" as a Button, a press activates and clears), `a_finished_badge_says_the_status_and_the_time`,
  and the app self-test's first scenario (`sleep 6` in the first shell, ⌘N to a second: the
  badge appears from the real zsh marks through ptyd, the engine and the wire, and the click
  back on the first card clears it).

- ✅ **A selection drags past the edge and ⇧-click moves its end; a scrollbar over the
  grid** (2026-09-13). Selecting more than a screen of output was impossible: GPUI's
  `on_mouse_move` on a div fires only while its hitbox is hovered, so the head froze at the
  edge, and nothing scrolled. Rulings: (1) the element registers a window-level
  `MouseMoveEvent` listener from `paint` (as the long press already does) that forwards to
  `TerminalView::drag_move` whenever a button is down, so a drag is followed anywhere; the
  div's own listener keeps the hover work; (2) past the top or bottom the drag scrolls
  `AUTOSCROLL_TICK` (50 ms) at one line per row of distance, at most `AUTOSCROLL_MAX` (8) —
  the pace every Mac terminal uses — and the head rides the row that came into view, so the
  selection grows with the scroll; the loop is a `cx.spawn` on the background timer, ended
  by the release or the pointer coming back inside (`autoscroll` is the flag; a new drag
  drops the old task); (3) ⇧-click moves the head of an existing selection (iTerm, ghostty,
  Terminal.app all do), the anchor kept, and a drag from there continues it; with no
  selection ⇧-click starts one as before (⇧ also bypasses mouse tracking, unchanged); (4)
  the scrollbar is a thumb drawn by the element over the grid's right edge
  (`scrollbar_thumb`: the screen's share of screen-plus-history, at least a row and a half,
  its top where the viewport is), shown while there is history and the pointer is over the
  grid, the viewport is in the history, or the thumb is held — never on a card the pointer
  is not over, since the grid is the content; the thumb drags (`offset_for_thumb`, the
  inverse), the track pages a screen towards the click, and both take the click before any
  selection so the text under the bar stays selectable by a click beside it. Tests: element
  `the_scrollbar_thumb_tracks_the_viewport_and_maps_back`, headless
  `a_shift_click_extends_the_selection`, `a_drag_past_the_top_scrolls_into_history`
  (ticks on the test clock, fetches for the lines scrolled in),
  `the_scrollbar_drags_and_pages_the_viewport`.

- ✅ **The wheel adds up, and reaches the program that wants it** (2026-09-13). Two faults
  in one handler: a trackpad's fractional lines were rounded per event, so a slow scroll
  moved nothing and — the event not consumed when it rounded to zero — panned the canvas
  under the pointer instead; and the wheel never left the client, though the wire
  (`MouseAction::Wheel`) and the engine (button 4/5 presses) were ready, so `vim` with mouse
  mode, `less`, `tmux` and every full-screen program saw no wheel at all. Rulings: (1) the
  fraction short of a line is carried (`wheel_remainder`) and a new gesture (`Started`)
  drops it, so 0.4 + 0.4 + 0.4 is one line and the 0.2 rides on; (2) the grid consumes a
  wheel event, whole line or not, while it can use it — a program wants it, or there is
  history in that direction — and lets it through otherwise, so a scroll over a grid at the
  end of its history pans the canvas instead of dying, and ⌘-wheel is always the canvas's
  zoom (the smooth probe pans from a point off every card since this, `background_point`:
  before, its sub-line wheels over a shell passed through by the rounding accident); (3) the rows go to the host when the program tracks the mouse (⇧ keeps them for
  the cache, as in every terminal) or the screen is the alternate one, which has no history
  on this side to scroll; (4) the host encodes button presses for a tracking program as
  before, and on the alternate screen without tracking turns the rows into cursor keys
  while mode 1007 (alternate scroll, on by default in ghostty) is set — the engine's
  business, since the mode lives in the terminal; the encoder has no such option
  (`mouse.rs`, checked at `44f2a44d`), so it is done through `encode_key`; on the primary
  screen an un-tracked wheel encodes nothing; (5) on the phone a finger pan arrives as the
  same scroll events, so a finger over a shell scrolls its history while there is some that
  way and moves the canvas otherwise, the way a list inside a page scrolls; a pan with no
  vertical part passes through. Tests: engine
  `the_wheel_is_arrow_keys_on_the_alternate_screen_and_presses_when_tracked`, headless
  `the_wheel_adds_up_fractions_and_reaches_a_program_that_wants_it`, canvas
  `a_finger_pan_over_a_shell_scrolls_its_history_before_the_canvas`.
  Found on the way: the binding's `Encoder::encode_to_vec` (key and mouse alike) reserved
  `required - remaining` on a vector with too little spare room, then handed the encoder the
  old capacity, so an encode into a reused buffer failed with `OutOfSpace`; fixed in the fork
  (`aislopware/libghostty-rs` `2c9c61e`: `reserve(required)`, a test in `mouse.rs`) and pinned.

- ✅ **The cache indexes its prompts** (2026-09-13). The sticky block header asks for the
  prompt above the top row on every render, and `TermState::prompt_before` walked the cache
  a line at a time: in a flooding shell whose prompt had scrolled out of the 20 000-line
  cache that was 20 000 map lookups a frame, and twenty such shells zooming dropped 26
  frames of 640 (MEASUREMENTS 2026-09-13, the 20-shell zoom; bisected to the header
  commit). Ruling: `Scrollback` keeps a `BTreeSet<LineIndex>` of the cached prompt starts,
  maintained where a line enters (insert or replacement: a row erased in place leaves the
  set), is evicted by the capacity, or is dropped by the host's extent, and
  `prompt_before`/`prompt_after` are range queries; the client's two functions delegate,
  `prompt_after` still bounded by the newest row. Not the alternatives: caching the header
  per top row (a flood moves the top row every frame); computing the header only when the
  marks change (the same walk, just less often, and the walk is wrong at any rate for a
  20 000-line cache). Tests: grid `prompts_are_indexed_through_replacement_and_eviction`,
  client `prompt_navigation_and_last_output_follow_the_marks` (unchanged, the semantics).

- ✅ **One blink clock per view, running only while a frame blinks** (2026-09-13). The engine
  reported the cursor's blink flag (DECSCUSR odd shapes, ghostty's default) and SGR 5 on a
  cell since the first frame, and the element drew both steady. Ruling: the view keeps a
  phase (`blink_on`), and the element, having prepared a frame, tells it whether that frame
  held a blinking cursor or an SGR 5 cell; the first such frame starts a task that flips the
  phase every 600 ms (ghostty's cadence) and notifies, the first frame without stops it with
  the phase on, so twenty idle shells tick nothing and a background shell never ticks for its
  cursor (unfocused, it draws the steady hollow block, as ghostty does). A keystroke pins the
  phase on for a full half so the cursor stays solid while typing. SGR 5 text blinks on the
  same clock — a divergence from ghostty, which draws it steady; a word with a blinking cell
  is shaped once per phase (the phase joins its cache key only then), its underline and
  strikethrough hide with the glyphs. Not the alternatives: a per-frame `request_animation_
  frame` (the whole window repaints at 120 Hz for a cursor); a clock on the app (a view whose
  frame has no blinking content would still be asked). Tests: element
  `a_word_hashes_the_same_wherever_it_sits` (the phase in the key, the hidden colour), headless
  `the_blink_clock_ticks_only_while_something_blinks`.

- ✅ **Glyphs are placed by their cell, not by GPUI's forced width** (2026-09-13). `shape_cells`
  shaped a word with `shape_line(.., Some(cell_width))`, which moves every *base* glyph (one
  whose shaped x advanced more than half a cell past the last base) to base index × cell and
  hangs the rest off it, and added a spacer space after a wide cell of one code point so the
  wide glyph took two bases. A wide cluster of several code points — `❤️` (VS16), a ZWJ
  family, a flag — got no spacer and, when GPUI's font fallback shaped it to several base
  glyphs, pushed the rest of the word along by that many cells (the family emoji put the
  next glyph five cells on, test). Ruling: shape with no forced width and place each glyph
  at the column of the cell its byte index falls in (`starts`: the first byte and column of
  every drawing cell), keeping its shaped offset from that cell's first glyph. A wide cell is
  two columns whatever it shaped to; a ligature (`->` in JetBrains Mono, one glyph for two
  cells) keeps its cells since the next cell has its own column; a combining mark stays on its
  base at its shaped offset; a cluster the fallback split into several glyphs overdraws its
  neighbour instead of shifting the row (ghostty does the same). The `Word` now holds the
  placed glyphs (font, id, position, emoji, colour) — the shaped line is dropped after
  placing, so painting walks one flat vector. Test: element
  `a_wide_cluster_takes_two_cells_whatever_its_code_points` (CJK, emoji, VS16, ZWJ, flag).

- ✅ **The cursor covers a wide character whole; underlines lie under the glyphs** (2026-09-15).
  Two ghostty rules slop-desk had to relearn (`docs/knowledge-from-slop-desk.md` §1, §2) and
  the element had not taken. (1) The block, hollow and underline cursors were one cell wide
  on every cell, so over a CJK glyph or an emoji they stopped halfway through it; now the
  cursor spans the columns of the cell under it (`cursor_span`: two for a `Wide` head, one
  for anything else — ghostty's `cursor_wide`). (2) Underlines and strikethroughs were both
  painted after the glyph layer, so a `g` or `p` on an underlined word was cut by the line.
  Each `Decoration` now carries its `Layer`: underlines (single, double, curly, and the SGR
  58 colour) paint before the glyphs so descenders cross them, strikethroughs after so they
  stay over the ink; a stroke joins only a run on its own layer. Tests: element
  `the_cursor_covers_a_wide_character_whole`,
  `underlines_lie_under_the_glyphs_and_strikethroughs_over`.

- ✅ **Box drawing, blocks, Braille and Powerline are drawn from the cell, not the font**
  (2026-09-15). A font fits each of these glyphs to its own em box, so two cells cannot agree
  on where the ink stops: a `│` border seams at every row once the line height is not the
  font's, `─` weights change across a fallback boundary, and Claude Code's `╭──╮` prompt
  box showed both. ghostty draws them itself (`src/font/sprite/draw/`); slop-desk ported
  that (`docs/knowledge-from-slop-desk.md` §1). Rulings: (1) `terminal::sprite::shapes`
  answers the geometry of one cell — rectangles, stroked polylines, a quarter arc, a polygon
  — in points snapped to device pixels, from the cell size and the underline thickness
  (ghostty's `box_thickness`); a heavy line is three light ones, a double line two light
  ones a light one apart. (2) Junctions are generic, not a table of 128 drawings: each arm is
  a bar from its edge towards the centre, and where it stops depends on what it meets — past
  the centre by half the thickest perpendicular arm so a corner closes, at the near double
  bar so a single stem hangs from it (`╤`), through both when it has an opposite (`╫`), and
  for a double arm the outer bar reaches the outer perpendicular bar and the inner one stops
  at the inner (`╔` is an L, `╬` an open square). Dashes cut the same bars into two, three or
  four with a gap of one thickness. (3) `╭╮╯╰` are an arc of radius half the cell's shorter
  side with two stubs; `╱╲╳` are strokes; blocks are fractions of the cell, the shades the
  text colour at ¼ ½ ¾; Braille dots are discs on the quarter and eighth points; Powerline
  U+E0B0–3 are the two triangles and the two chevrons. (4) The element treats such a cell as
  a word boundary (`drawn_here`), so the font never shapes it and the word cache never holds
  it; the row keeps a `SpriteCell` (column, character, text colour) and paints the shapes
  between the underlines and the glyphs. Not drawn here: Powerline's other private-use
  glyphs — the font keeps those (sextants and the legacy computing symbols followed, below). Tests:
  `terminal::sprite::tests` (ranges, light/heavy/double bars, corners and junctions, dashes,
  arcs, blocks, Braille, Powerline, device-pixel snapping) and element
  `a_box_drawing_cell_is_drawn_not_shaped`.

- ✅ **⌥-drag selects a rectangle** (2026-09-15). Every terminal has it (ghostty, iTerm,
  Terminal.app) and a column of a table or a stack of prefixes is what people reach for it
  for. `Selection` gained `block`: set from the ⌥ modifier on the press that starts a drag;
  `columns` then answers the same span — between the two corners' columns, clipped to the
  grid — on every line of the rectangle, so painting and `selected_text` needed no change
  (each line still trims its trailing blanks). Word and line clicks, ⇧-click and the block
  menu's selection stay runs. Tests: `a_block_selection_is_the_same_columns_on_every_line`,
  the block case in `selected_text_spans_rows_and_trims`.

- ✅ **OSC 9 / 777 / 99 notifications reach the desktop** (2026-09-15). A long build's
  `printf '\e]9;done\a'` or a kitty `notify` used to end at `EngineEvent::Bell`'s neighbour
  and vanish; ghostty, kitty, WezTerm and iTerm2 all post a banner. libghostty's
  `desktop_notification` callback (registered in `install_callbacks` next to the bell) hands
  over the parsed title and body for every dialect, so the engine needs no OSC parsing of
  its own: it emits `EngineEvent::Notification { title, body }` with each field cut to
  `NOTIFICATION_CHARS` (512; a banner shows a line or two and a program can write anything
  into an OSC), the session broadcasts `TermEvent::Notification` (appended last, protocol 42)
  to every attached client, and the canvas treats it as an agent's attention: a
  notification-centre banner only when no window is active (`program_banner` leads the title
  with the card's name and says "Terminal" when the protocol carried none, as OSC 9 does),
  tagged by the session so a click reveals the card through the existing response path, and
  `CanvasEvent::Attention` for the Dock bounce either way. No sound and no urgency: the bell
  already has its own path. Tests: engine `desktop_notifications_are_events`, client
  `apply` mapping, `worker_term_notification` golden, canvas
  `a_programs_banner_has_a_title_even_when_the_protocol_gave_none`.

- ✅ **The bell is seen, and heard only when the human is elsewhere** (2026-09-15). BEL
  travelled the whole way (engine → `TermEvent::Bell` → `TerminalViewEvent::Bell` →
  `CanvasEvent::Bell`) and the app dropped it. Now the view tints its grid with the text
  colour (`alpha::FAINT`) for 150 ms — a visual bell, the one every terminal offers and the only
  one that names *which* card rang on a canvas of many — a second bell inside the flash
  restarts it, and the app, when no window of ours is active, plays the user's alert sound and
  bounces the Dock through the attention path an agent's block takes. In front of the window
  nothing sounds: a `printf '\a'` in a loop must not be a siren. Tests: view
  `a_bell_flashes_the_view_briefly`.

- ✅ **Sextants and octants are mosaics drawn from the cell** (2026-09-15). Charts and
  graphs in today's TUIs (btop, plotting libraries, Rust TUI canvases) tile the
  cell two by three (U+1FB00–1FB3B) and two by four (U+1CD00–1CDE5, Unicode 16), and a font
  either lacks them or fits each to its own metrics with seams between neighbours — the same
  reason the box-drawing set is drawn (`87c008d`). `sprite::mosaic` cuts the cell at snapped
  device-pixel boundaries and fills the named tiles in reading order; sextants take ghostty's
  index arithmetic (the block skips the four patterns block elements already draw), octants a
  230-entry mask table transcribed from ghostty's `octants.txt` (there is no formula: the
  block skips every pattern another character already draws). Tests:
  `sextants_and_octants_are_mosaics_of_the_cell`, the ranges test.

- ✅ **The legacy computing symbols are drawn too** (2026-09-15). U+1FB3C–1FBAF and the
  centre quarter blocks U+1FBE4–7 sit next to the sextants in every TUI's toolbox (wedge
  triangles for smooth chart edges, eighth bars for finer gauges, shaded halves and
  checkerboards for textures, hatching, the corner diagonals) and fonts treat them the way
  they treat sextants: missing, or fitted with seams. `sprite::wedge` fills a polygon through
  the ten vertices ghostty's table names (corners, thirds of the sides, centre top and bottom;
  a 44-entry mask table derived from its patterns, collinear vertices folded as it folds
  them); `sprite::legacy` covers the rest with rectangles, edge triangles to the snapped
  centre, a translucent `Ink::Shade` on the shaded halves and corners (so `Shape::Poly` now
  carries an ink), a checkerboard of four columns by as many rows as keep the tiles square,
  and hatching whose lines are clipped to the cell (`clip_x`) since the painter does not clip
  a sprite. U+1FBAF is a box junction (heavy stem, light bar) through `box_arms`; U+1FB93 is
  unallocated and draws nothing; U+1FBB0 onward are symbols the font keeps. Tests:
  `wedges_and_edge_triangles_are_polygons_through_the_cells_thirds_and_centre`,
  `legacy_bars_blocks_and_shades_fill_their_eighths`,
  `checkerboards_alternate_and_hatching_stays_inside_the_cell`,
  `corner_diagonals_run_from_the_edge_midpoints`.

- ✅ **The pointer says what a click would do** (2026-09-15). The grid showed the arrow
  everywhere. Now the card's root div sets the pointer from `TerminalView::pointer`: an
  I-beam over text (the selection every terminal offers), a pointing hand over the run
  `link_highlight` would open (⌘ held, over a URL or a path), and the arrow while a program
  reports the mouse — unless ⇧ is held, which is what also keeps a click and the wheel from
  the program. The conversation view keeps the arrow. `set_pointer` repaints only when the
  shape or the underline changes, and tracks ⇧ beside ⌘. Test:
  `the_pointer_is_an_i_beam_a_hand_over_a_link_and_an_arrow_for_a_program`.

- ✅ **A ⌘-hover shows where the link goes** (2026-09-15). An OSC 8 label can say anything
  (`docs`, the file's name, a shortened URL) and the underline alone does not say what a
  click opens — the reason browsers show the target in the status bar and ghostty draws it
  at the corner. `TerminalView::link_target` names it — the OSC 8 URI, the URL as printed,
  or the path with its `:line` — and the card draws it as a chip at its bottom-left
  (`link-preview`) while ⌘ is held over the run, muted panel text, clipped to the card,
  gone with the modifier. Test: `a_cmd_hover_previews_the_links_target`.

- ✅ **Images are placed by the host and painted by the client** (2026-09-15). Kitty
  graphics is how today's tools show a picture in the terminal (image previews, plots,
  `timg`, agent screenshots). libghostty already parses the protocol and holds the images;
  what Slopty adds is the wire. The host does the layout (`placement_render_info`, at the
  client's cell pixels, which `TermSize::metrics` already carries for mouse reports), so
  the client never learns kitty semantics: a `Frame` lists placements as cells, offsets, a
  painted size and a source rectangle, and pixels come once per image generation as
  `TermEvent::Image` — RGBA, converted from whatever was stored (RGB, gray, decoded PNG),
  sampled down by a whole factor when a single image would not fit the codec's 16 MiB
  frame. Pixels are sent ahead of the first frame that places them (the client paints
  nothing for a placement whose pixels it lacks, and repaints when they land), and again
  after a full frame, because an attaching client holds nothing. Both sides run the same
  cache rule (`IMAGE_CACHE_BYTES`, least recently placed first) so the host's ledger of
  what a client holds stays right without an acknowledgement; a client that attached
  mid-stream may hold more than the ledger says, which is harmless. The storage generation
  is part of the engine's dirty check, so deleting a placement makes a frame although no
  cell moved. The PNG decoder is Slopty's own (the binding's `RustPngDecoder` has no
  constructor); it expands every colour type to RGBA. Tests: engine
  `a_transmitted_image_is_placed_and_uploaded_once`, `a_placement_change_alone_makes_a_frame`,
  `rgb_and_png_transmissions_arrive_as_rgba`, `graphics::tests`; client
  `images_are_kept_for_their_placements_and_the_oldest_placed_go_first`; view
  `a_placed_image_has_one_texture_until_its_pixels_are_forgotten`; element
  `a_placement_is_painted_at_its_cell_in_the_workers_pixels`; goldens `worker_frame`,
  `worker_term_image` (protocol 43).

- ✅ **Unicode placeholders are placed by the host** (2026-09-15). A virtual placement
  (`U=1`) is how an image survives a multiplexer or a scrolling pager: the program prints
  U+10EEEE cells and the terminal draws the image where they are (kitty, ghostty, WezTerm
  do; `timg -pk`, kitty's own `icat` under tmux, chafa's kitty mode rely on it). libghostty
  stores the placement and flags the row but draws nothing, and its C API exposes no
  placeholder logic, so the host ports ghostty's: `engine::placeholder` decodes a cell
  (image id from the foreground colour — palette index or `r<<16|g<<8|b` — placement id
  from the underline colour, tile row and column from the first two combining diacritics
  of kitty's 297-entry table, the image id's high byte from a third), joins consecutive
  cells into runs by ghostty's rules (same image and placement, same row or none, the next
  column or none) and turns each run into an ordinary `Placement`: the image scaled to fit
  the placement's grid keeping its aspect, centred, the run showing its own strip (a run in
  the letterbox places nothing). An unsized grid (`c`/`r` of 0) takes the cells the image
  needs at the client's cell pixels. The rows are scanned every frame whether dirty or not
  (a run that did not change still places its image), only when the row's placeholder flag
  is set. The placeholder cell goes out blank, so the client paints the image and never
  the character; nothing changes on the wire. Ruled out: doing it on the client (it would
  need the raw cells, the placement table and the diacritics, all of which the host has).
  Tests: `placeholder::tests` (decoding, run joining, the letterboxed strip), engine
  `unicode_placeholders_place_the_virtual_image_by_cell`.

- ✅ **Colour queries are answered with the dark theme** (2026-09-15). A TUI that picks its
  look from the terminal's background (neovim's `background`, helix, delta, bat) asks with
  OSC 11 `?`, and libghostty answers only when the embedder set a default colour: the engine
  set none, so the question went unanswered and the program waited its timeout or guessed;
  OSC 4 answered with ghostty's built-in palette, not the one the client paints. Now the
  engine sets the dark theme's foreground, background, cursor and ANSI 0–15 as libghostty's
  defaults at start (`set_theme_colors`), and the answers say what a client in the default
  theme shows. Cells keep their symbolic colours on the wire, so a light-theme client still
  paints its own palette. Test: `colour_queries_are_answered_with_the_dark_theme`.

- ✅ **The driver's colours answer the queries** (2026-09-15, protocol 44). A light-theme
  phone asking neovim for its `background` got the dark answer above, so the editor chose a
  dark look on a white card. Clients may differ while the host answers once per session, so
  the rule follows the PTY size: the driver decides. Every client sends
  `TermRequest::Colors(TermColors)` (fg, bg, cursor, ANSI 0–15 as RGB triples) right after
  its `Attach` and whenever its theme changes; the session keeps each viewer's colours (a
  re-attach keeps them) and applies the driver's — on its attach, when it claims the wheel,
  when they change — through `VtEngine::set_colors`. A viewer's are recorded, never
  applied; a driver that never said any leaves the dark defaults. Ruled out: an `Attach`
  field (the resync attach comes from the client model, which knows no theme, and a theme
  change would need a message anyway) and per-client answers (one PTY, one program, one
  answer). Tests: proto golden `client_colors`; engine
  `colour_queries_answer_with_the_drivers_colours_once_set`; session actor
  `the_drivers_colours_answer_a_colour_query` (a raw-mode shell prints the OSC 11 reply
  through `cat -v`); canvas
  `cmd_n_asks_the_host_for_a_shell_and_its_echo_places_and_focuses_it` (colours follow the
  attach and a theme change).

- ✅ **The colour scheme is the driver's background** (2026-09-15). neovim 0.10+, and the
  editors following it, ask `CSI ? 996 n` for light or dark and set mode 2031 to be told of
  a change, in preference to reading OSC 11. libghostty routes the query to the embedder
  and encodes the report; the engine answers from the luma of the driver's background
  (BT.601 weights, over half is light) and, when the mode is on and the scheme actually
  flips on a `set_colors`, sends the report unprompted. Test:
  `the_colour_scheme_follows_the_drivers_background`.
- ✅ **A program's colour changes reach every client** (2026-09-15, protocol 45). Themes
  scripts (base16-shell, vim's `termguicolors` off, Emacs' `xterm-color` hooks) set OSC 4/10/11/12
  and the terminal is expected to paint with them, not just answer queries with them; a shared
  session must show the same colours on every client, including one attaching later. libghostty
  keeps the current colours next to the defaults but has no change callback, so
  `GhosttyEngine::write` reads fg/bg/cursor/palette against the defaults after each chunk
  (three reads and two 768-byte palette copies) and a difference is `EngineEvent::Colors` with
  the whole `ColorOverrides` set: whole, so a late attach and a viewer that saw every change
  paint alike, and so a reset (OSC 104/110/111/112; RIS keeps them, as in xterm and libghostty)
  needs no second message shape. The
  session keeps the last set and sends it to an attach ahead of the full frame. The client
  paints through `slopty_theme::Colors`: the theme with fg/bg/cursor/ANSI 0–15 replaced and a
  map for cube entries (base16 sets 16–21), hashed into the shaped-word cache key so a change
  never replays old colours; a program-set cursor colour takes black or white text under it
  by BT.601 luma, since the theme's `cursor_text` was picked against the theme's cursor. The driver's palette changing under the program's is not a change
  of the program's, so a theme swap on the driver does not re-send it. Not taken: a diff per
  change (smaller, but attach needs the whole set anyway) and a change callback in the fork (a
  polling read per chunk is cheaper than a binding patch to carry).
  Found on the way (2026-09-15): the checkpoint's `with_palette(true)` had libghostty's
  formatter write all 256 entries as OSC 4 sets, so a restored session (hostd restart, ptyd
  replay) would have reported the previous driver's ANSI 0–15 as the program's changes and
  every client would have painted them over its theme for good. The checkpoint now carries
  the program's changes as the sequences that made them (`colour_sets`) and no palette dump;
  test `a_checkpoint_carries_the_programs_colours_not_the_drivers`. And the actor now reads
  what the replay said at construction instead of at the first output: the title, the
  directory (the checkpoint's OSC 7) and the colours are kept for the first attach, while the
  replay's bells, notifications, clipboard writes and query answers are dropped — the last
  host delivered them, and an answer written now would land in a shell that is not asking.
  Test `a_restored_session_tells_the_first_attach_what_the_replay_said`.

- ✅ **A running command survives a reflow** (2026-09-15). App e2e run 298 lost the "done"
  badge of a `sleep 6` whose prompt had come back (second flake of that test; run 287 was the
  first): the client's `track_command` treated the first prompt of a new epoch as saying
  nothing about what ran before it, so a reflow (a resize while a command runs), a reset or an
  alt-screen round trip between Enter and the next prompt dropped the badge, the took caption
  and the block's end — and left `running` set, so the *next* command's start was swallowed
  too. Ruled: on the first frame of an epoch with a command running, the newest prompt's block
  is compared with it by its typed text — the same command means it runs on, re-keyed to its
  new row; a different one (or none) means it finished, with that prompt's exit status and
  `CommandFinished { prompt: None }` when its own prompt is no longer among the rows held (no
  caption to place, still a badge). The e2e dump now carries `epoch` and the view logs a
  numbering change at info, so the next flake names its cause. Test:
  `a_running_command_survives_a_reflow_and_finishes_after_one`.

- ✅ **A minimum contrast and copy-on-select, both off by default** (2026-09-15). Two
  `[terminal]` keys in `settings.toml`, the first non-appearance settings. `minimum_contrast`
  (1.0–21.0, ghostty's `minimum-contrast`) is the least WCAG ratio a cell's text may have
  against its background; under it the text is black or white, whichever contrasts more.
  Judged per cell in `cell_color` from the painted pair (inverse video swaps the slots first),
  through `slopty_theme::Colors::text_over`, so a program's OSC 10/11 colours are held to it
  as well; faint and blink still apply after. The ratio rides on `TerminalPalette` in
  hundredths (the palette is `Copy + Eq + Hash`, keyed into the shaped-word cache, and a
  float would break that), so a settings change repaints every view through `set_theme` like
  a colour would. `copy_on_select` puts a selection on the clipboard as it ends: a drag on
  release, a ⇧-click, a double or triple click, a phone long press; a click without a drag
  selects nothing and copies nothing. It hangs off `Theme::behaviour`, a block for the
  settings that are neither colours nor type but need the same delivery. Both default off:
  1.0 is ghostty's default and the Mac's ⌘C is the convention (ghostty's own macOS default
  copies only to the selection clipboard, which the Mac has none of). A typo in the ratio
  (0, 40, `inf`) reads as off, as the font sizes do. Tests:
  `text_under_the_minimum_contrast_moves_toward_black_or_white`,
  `text_is_held_to_the_minimum_contrast`, `terminal_settings_ride_on_the_theme`,
  `a_selection_is_copied_as_it_is_made_when_asked`, `terminal_keys`.
  Superseded in part 2026-09-25: the minimum defaults to 3.0 and a colour under it moves
  toward black or white only as far as needed, hue kept; a typo reads as that default. The
  light theme's golden showed why: a prompt's own 24-bit mint (`#80FFEA`, 1.2:1 on white),
  chosen for a dark terminal, was unreadable, and the snap to black would have lost its hue
  (ui.md, "The de-slop pass").

- ✅ **A paste that would run waits for a confirmation** (2026-09-15, ghostty's
  `clipboard-paste-protection`, iTerm2's multi-line warning). A copied snippet with a
  newline pasted into a shell without bracketed paste runs its first line the moment it
  lands; ghostty refuses such a paste until confirmed, and so does the view: ⌘V holds the
  text (`pending_paste`) and shows a strip at the card's corner ("Paste N lines that would
  run?", `terminal-paste-confirm` / `terminal-paste-cancel`); ↩ sends it whole, Esc drops
  it, any other key drops it and goes on to the program (nothing typed is swallowed). The
  rule is `paste_is_safe`: outside bracketed paste a `\n` or `\r` is unsafe; inside it only
  the bracket's end sequence (`ESC [ 201 ~`) is, so an editor or a shell with mode 2004
  (zsh, fish, bash 5.1+) takes a multi-line paste straight, as ghostty does. Deliberate
  pastes (`run_text`: Rerun, the agent's typed command) bypass it. `[terminal]
  paste_protection` turns it off (`Theme::behaviour.paste_protection`, default on). Since
  2026-09-30 the worker applies the rule to the mode as it is when the paste arrives (ui.md,
  "A paste is judged by the mode when it arrives"). Test:
  `a_paste_the_worker_holds_back_waits_for_a_confirmation`.

- ✅ **Bold is bright, ligatures, pointer hiding and the scroll multiplier are settings**
  (2026-09-15). Four small ghostty/Terminal.app conveniences the views had fixed: `[terminal] bold_is_bright`
  (xterm's `boldColors`, off as in ghostty; `Colors::bold_slot` lifts ANSI 0–7 to 8–15 for
  a bold cell before the inverse swap, so the many schemes whose bright half is a lighter
  tint read as they were drawn), `[font] ligatures` (on: the font's own; off: `calt`
  disabled through GPUI's `FontFeatures::disable_ligatures`, so `=>` stays two glyphs —
  part of the shaped-word cache key, since the same cells shape differently) and
  `[terminal] hide_pointer_while_typing` (on, as Terminal.app and iTerm2: a key that goes
  to the program calls `NSCursor.setHiddenUntilMouseMoves`, AppKit unhides on the next
  move; nothing on iOS, and nothing without a running `NSApplication`: in a headless test
  the AppKit call stalled ten seconds on a window server connection, which is how the
  guard was found) and `[terminal] scroll_multiplier` (ghostty's `mouse-scroll-multiplier`,
  `Behaviour::scroll_multiplier` in hundredths; applied to the wheel's lines before the
  fraction carry, so a program on the alternate screen sees the multiplied rows too).
  Tests: `bold_is_bright_lifts_only_the_named_eight`, `bold_text_is_painted_bright_when_asked`,
  `the_wheel_scrolls_by_the_multiplier`; the pointer hide is a platform call with no
  headless observer, so only its plumbing is tested (`terminal_settings_ride_on_the_theme`).

- ✅ **The cursor's blink can be overridden** (2026-09-15, ghostty's `cursor-style-blink`).
  `[terminal] cursor_blink = program | always | never` (`Behaviour::cursor_blink`):
  `program` leaves DECSCUSR to the shell or editor (the default: shells are steady, editors
  often blink), `always` and `never` override it either way. Applied where the element
  decides whether the cursor ticks the blink clock, so an unfocused card stays a steady
  hollow block as before. Test: the blink-clock test's last two frames. Its companion
  `cursor_style = program | block | bar | underline` (ghostty's `cursor-style`,
  `Behaviour::cursor_style`, `element::cursor_shape_for`) fixes the focused shape the same
  way; unfocused stays the hollow block. Test: `the_cursor_style_fixes_the_shape_or_leaves_it`.

- ✅ **A click on the input line moves the shell's cursor** (2026-09-15, ghostty's
  `cursor-click-to-move`, on by default there and here). A plain left click released without
  a drag, on a row the shell marked as its input (the prompt row from its OSC 133;B column,
  an `Input` row), sends the arrow keys that carry the cursor there: rows first (↑/↓ across a
  hard continuation, the column kept from each line's start as the shell's own ↑/↓ do), then
  cells (←/→, a wide character once, its spacer never). Soft-wrapped rows are one line to the
  shell, so a click on the wrapped part is a cell count. The click is held to the input (not
  into the prompt's text, not past the typed text, where zsh would take → as accepting a
  suggestion), and nothing is sent when the cursor itself is not in the line editor (a
  program running: its row is output), on the alternate screen, under mouse reporting, or
  with the cursor hidden. The path is computed on the client (`TermState::cursor_path_to`,
  the marks are already there) and sent as ordinary `TermRequest::Key`s through `press`, so
  application cursor mode and the kitty protocol encode them on the host as they would a
  keypress. Not taken: sending bytes (the host owns key encoding) and ghostty's press-time
  trigger (a drag would start with a cursor move). Tests:
  `a_click_on_the_input_is_a_path_for_the_cursor`,
  `a_click_on_the_input_line_moves_the_cursor_there`.

- ✅ **Option is Alt when asked** (2026-09-15, ghostty's `macos-option-as-alt`, off there and
  here). macOS translates ⌥ into the layout's symbol (⌥b is `∫`), so the Emacs bindings zsh,
  bash, fish and every readline program answer to (⌥b/⌥f words, ⌥d, ⌥.) sent a glyph
  instead of `ESC b`. `[terminal] option_as_alt = false | true | left | right` chooses
  whether the key is a modifier (the escape prefix on the key, ghostty's `legacyAltPrefix`)
  or the layout's key (`false`, the default: accents and symbols stay typeable);
  `left`/`right` keep one side for each. It takes both ends, as in ghostty: the client
  resolves the side (`OptionAsAlt::applies`; the Mac reads the right key's device bit off
  the current `NSEvent`, since GPUI does not tell sides; iOS has none and reads as left)
  and, when the key is Alt, sends the key without ⌥ (a letter, shifted when Shift is down;
  anything else no text, the encoder prefixes the unshifted codepoint) with ⌥ not consumed
  and `KeyEvent::option_as_alt` set (protocol 47); the host puts that on libghostty's
  encoder after `setopt_from_terminal`, which resets it. Per key rather than per session
  because the session is shared: two clients with different keyboards get each their own.
  Tests: `option_as_alt_prefixes_escape_on_the_worker` (engine),
  `option_as_alt_rides_on_the_event` (client keys), `terminal_keys` (settings).

- ✅ **The right click works on every row** (2026-09-13). The block menu opened only on a
  command block's rows, so without shell integration (a raw `ssh`, a program's screen) a
  right click did nothing, and even with it a mouse-only reader had no Paste. The menu now
  opens on any row: the block's items first when the row is in one, then the terminal's own —
  Copy (with a selection; off a block a selection also offers "Ask the agent" and "Save as
  note", the selection fenced), Paste, Find…, Clear screen — what the mouse-only reader
  reaches for, each the same code as its shortcut so nothing new to test on the host side.
  `BlockMenu::block` is optional; the aria name says which menu opened. Not a full copy of
  the Edit menu: the rest (fonts, splits, agents) is the palette's. Tests:
  `a_right_click_off_a_block_offers_the_terminals_own_items`,
  `a_right_click_on_a_block_offers_its_command_and_output`.

- ✅ **Keyboard scrolling and ⌘A** (2026-09-13). The wheel, the scrollbar and ⌘↑/⌘↓ were the
  only ways through history; a keyboard reader (and a program that eats the wheel) had none.
  ⇧⇞ / ⇧⇟ page by the viewport's rows, ⇧⇱ / ⇧⇲ go to the oldest line / the output, as in
  ghostty, plus ⌘⇱ / ⌘⇲ since that is what Terminal.app taught the Mac; all bindings in the
  Terminal context, so a program never sees them (ghostty binds them unconditionally too).
  ⌘A selects from the host's oldest line to the newest and fetches every gap in the cache
  at once, so the copy that follows a beat later has the whole scrollback; lines that have
  not arrived yet copy blank rather than waiting. Test:
  `the_keys_page_through_history_and_select_it_all`.

- ✅ **⇧-arrows adjust a selection** (2026-09-13). A selection could only be shaped by the
  pointer (drag, ⇧-click); ghostty's `adjust_selection` gives the keyboard the same. With a
  selection, ⇧←/→ move its head a cell (wrapping at a row's ends), ⇧↑/↓ a row, within the
  lines the host keeps; the shell sees none of them. With no selection the keys stay the
  program's, as any other key — no mode to enter or leave, and any plain key still drops
  the selection. Test: `a_right_click_off_a_block_offers_the_terminals_own_items`.

- ✅ **The typed command is read from its cell, not a character count** (2026-09-13). App
  e2e run 343 logged every command one letter short (`cho e2e-42`, `leep 6`): the block
  head sliced the prompt row's text with `chars().skip(input_col)`, and the prompt on this
  machine has a wide glyph before the input column (a starship segment, as any Powerline
  prompt), which is two cells but one character. `Line::text_from(col)` walks the cells from
  the column, skipping spacer tails as `text()` does, and `block_head` reads the command
  with it. A wrong first letter poisoned everything downstream: the block head, the menu's
  "copy command", "run again", the badge's caption and the agent's fence. Tests:
  `text_trims_trailing_blanks_and_skips_spacers` (grid),
  `a_prompt_with_a_wide_glyph_keeps_the_commands_first_letter` (client).

- ✅ **Closing a busy shell asks first** (2026-09-13). ⌘W (or "Close item") on a terminal
  whose command is still running (the marks say so: `command_running`) ended it on the
  spot, a build or a long test gone with one chord meant for another card. Every terminal
  asks here (ghostty's `confirm-close-surface`, iTerm's, Terminal.app's). Ruling: the
  view's bar at the card's bottom-left — the same one the held-back paste uses, one
  `Pending` for both — names the command and offers Close / Keep; ↩ closes, Esc keeps, any
  other key keeps and goes on to the program. The canvas asks the view before sending
  `Close` and sends it on `TerminalViewEvent::CloseConfirmed`; an idle shell, an exited one
  and every other card kind close as before. `[terminal] confirm_close = false` turns it
  off. Tests:
  `closing_a_busy_shell_asks_first` (view: asks while running, ↩ confirms, Esc keeps, off
  by setting), `a_busy_shell_closes_only_when_confirmed` (canvas: no `Close` until the
  view says so).

- ✅ **A closed shell can be taken back for five seconds** (2026-09-13). The bar above
  guards a running command; an idle shell still went on ⌘W, with its history, its
  directory and whatever was half-typed. ghostty keeps a closed surface for
  `undo-timeout` (5 s) and ⌘Z brings it back; Warp and Chrome reopen a closed tab. Ruling:
  the canvas takes the card off the document (`CanvasOp::Remove`, so every client sees it
  go) but sends no `Close`; the session and the view stay (`reconcile` keeps a closed
  shell's view attached, so the picture is the one the human left) for `UNDO_CLOSE`
  (5 s), during which ⌘Z, "Undo close" in the palette, or the toast's button (`closed
  <title> · take back ⌘Z`) upserts the item as it was, active and focused. When the time
  passes the host is sent `Close` and the view is dropped. A shell whose program exited
  closes at once: there is nothing to take back, since the host cannot replay its rows.
  Widened 2026-09-14 to every other card kind, which has no such problem — a window, a
  display, a note and a file card are each entirely their document item, so `remember_closed`
  holds the item and the take-back upserts it (`ClosedCard.session` is `None` for them, and
  nothing is sent to the host when the time passes). A note was the case that forced it: its
  text lives only in the item, so the old immediate `Remove` was the one destructive action on
  the canvas with no way back. Its editor commits on a timer, so the close reads the live
  field (`NoteView::live_text`) rather than the document, or the take-back would return the
  note without the line that prompted it. The stack is
  per client and holds every close within the window, newest taken back first; a session
  the host reports gone leaves it. Tests: `a_closed_shell_can_be_taken_back_for_five_seconds`
  (canvas: ⌘W removes and keeps, ⌘Z and the toast put back the same view at the same
  rect, the clock passing sends `Close` and empties the stack),
  `a_closed_note_comes_back_with_its_text` (the item and its text return, and the offer
  lapses with the clock), `a_note_closed_before_its_commit_keeps_what_was_typed`.

- ✅ **A `133;C` on its own is a frame** (2026-09-13). App e2e run 343 timed out waiting for
  `sleep 6` to badge its shell: the client saw the command start only when it ended
  (`elapsed` 76 ms for a six-second sleep), so nothing was slow and nothing was badged. The
  PTY bytes (traced at `slopty_worker::session=trace`, run 345) show zsh writing `\r\r\n`
  and then `133;C` as two writes. The linefeed from the input row leaves the new row a
  prompt continuation (libghostty's guess for shells without `k=s`), and the `C` takes it
  out again without touching a cell, so the row was never dirty: the frame after the
  linefeed said "prompt continuation, cursor here", and no frame followed until the
  command printed or its prompt came back. The client's block tracking (a command runs
  once the cursor is past the prompt's rows) rightly saw a prompt row under the cursor.
  Ruling: the engine's OSC 133 scanner reports `C` too (`Mark::OutputStart`), the engine
  keeps the cursor's line in `forced_rows`, and the next frame carries that row whether or
  not libghostty dirtied it — its prompt flag read from the live grid (`grid_ref`), since
  the render state only copies rows the terminal dirtied. The set is cleared on every
  frame and every epoch. Tests: `a_133_c_on_its_own_puts_its_row_in_a_frame` (engine: the
  linefeed's frame says continuation, the `C`'s frame says output, once), the host actor
  against a raw-mode `sh` that writes the linefeed and the `C` separately
  (`a_silent_command_after_a_clear_is_seen_running_at_once`: the client state built from
  the actor's frames reports the command running within a second of ↩, the cursor below
  it), and the scanner's `other_sequences_are_ignored`. The `pty read` trace stays: the
  bytes a real shell writes are the evidence every ruling in this section rests on.

- ✅ **The line is edited with the Mac's keys** (2026-09-13, ghostty's macOS "natural text
  editing" defaults). ⌘← and ⌘→ went up to the app as unbound chords and did nothing; ⌥←
  and ⌥→ reached the host's encoder as `CSI 1;3 D/C`, which zsh, bash and fish bind to
  nothing, so a word could not be stepped over without ⌥b/⌥f, which need option-as-alt.
  Ruling: with `[terminal] natural_editing` on (the default) the view sends what ghostty's
  macOS keybinds send, as raw bytes: ⌘← `^A`, ⌘→ `^E`, ⌘⌫ `^U`, ⌥← `ESC b`, ⌥→ `ESC f`,
  and ⌥⌫ `ESC DEL` (readline's and zle's backward-kill-word; ghostty leaves that one to
  its encoder, which sends it only with option-as-alt). They are taken after the search
  field, the composer and a selection's ⇧-arrows have had their say, so nothing else moves;
  a program under the alternate screen gets the same bytes, as under ghostty. Off, the
  chords go where they went. On the phone the key bar's armed ⌘ with ← → ⌫ is the same
  chord (`press`), so a line can be edited without a hardware keyboard. Tests:
  `natural_editing_keys` (keys: the table, other chords none),
  `the_macs_editing_keys_edit_the_line` (view: ⌘← is `^A` on the wire, ⌥⌫ `ESC DEL`, the
  armed ⌘ too, off nothing), `terminal_keys` (settings).

- ✅ **`sudo` keeps the terminfo, and zsh's cursor says its keymap** (2026-09-13, ghostty's
  `sudo` and `cursor` shell-integration features). `TERM=xterm-ghostty` with `TERMINFO`
  pointing at our compiled entry left `sudo vim` (and any root shell) without a terminal
  description, since sudo's default policy drops `TERMINFO`. Ruling: when `TERMINFO` is set,
  the three snippets define a `sudo` function that runs `command sudo --preserve-env=TERMINFO`;
  sudoedit (`-e`/`--edit`) is left alone, and fish keeps a `sudo` the user already made a
  function or an alias. zsh additionally prints a blinking bar (`CSI 5 q`) when zle starts a
  line or the keymap changes to insert, a blinking block (`CSI 1 q`) in `vicmd`/`visual`, and
  `CSI 0 q` from preexec so the command starts with the program's shape; the widgets are
  wired as ghostty wires them (add-zle-hook-widget when the widget is already such a hook, else
  wrapping whatever widget was there). bash has no keymap hook; fish shapes its own cursor.
  Titles (ghostty's `title` feature) are not emitted: the card already shows the directory
  and the running command from the marks. Tests: the live-shell tests in
  `shell_integration.rs` run with a `TERMINFO` and check `sudo` is a function in zsh, bash and
  fish, and that zsh writes `CSI 5 q` at the prompt and `CSI 0 q` ahead of its `133;C`.

- ✅ **A prompt that lost its marks is marked in place** (2026-09-13, ghostty's zle-line-init
  fallback). The marks live in PS1, put there by our precmd, which also moves itself to the
  end of `precmd_functions` so a theme's precmd cannot rebuild PS1 behind it — but on the
  first prompt the order is not yet ours, so a theme registered after us (from `.zshrc`;
  `.zshenv` loads us first) draws a prompt without marks: no block, no click-to-move, and
  the first command's status lands nowhere. Ruling: when zle starts reading a line and PS1
  has no `133;A`, the widget prints `133;P;k=i` (a prompt start that draws nothing and
  moves no cursor, the prompt being already on screen) and `133;B`; the scanner reports `P`
  as a prompt start like `A`, `k=s`/`k=c` continuations excepted. Tests:
  `a_theme_that_rebuilds_ps1_still_gets_its_prompt_marked` (a `.zshrc` precmd that sets
  PS1: the first prompt is marked by `P`, the second in PS1) and the scanner's
  `prompt_starts_but_not_continuations`.

- ✅ **A gesture belongs to whichever surface could use its first movement** (2026-09-15). The
  grid used to judge every wheel event on its own: it took the event if a program wanted the
  mouse or there was history that way, and let it through otherwise, so the canvas could pan
  under a terminal that had nothing left to scroll. Per event, that rule breaks in the middle of
  a fling. Flick a grid ten lines from the bottom of its history and the first few events scroll
  it; the rest of the momentum — which on macOS keeps arriving for the best part of a second
  after the fingers lift — falls through to the canvas, and the whole workspace slides away from
  under the pointer. Nobody asked for that pan, and it lands after the gesture that caused it is
  over, which is the worst kind of surprise on a surface built for direct manipulation.

  Ruling: the first event of a gesture that actually *moves* decides the owner, and
  `wheel_gesture` holds it until the next `TouchPhase::Started` clears it. A fling that began in
  the grid stays in the grid and is absorbed at the end of the scrollback; a pan that began over
  a bottomed-out grid goes on panning even as it crosses history the grid could have used. ⌘
  means the canvas's zoom, and taking it drops the latch rather than fighting it.

  Three facts decide the shape, and getting any of them wrong makes the change a no-op or worse.

  **`Started` cannot be the event that decides.** The first event of a gesture is a finger
  landing: gpui's touch recogniser and a trackpad both deliver it with a zero delta. Latching
  there reads "no movement, so the grid cannot use this" and hands every finger pan straight to
  the canvas. The first version of this ruling did exactly that, and
  `a_finger_pan_over_a_shell_scrolls_its_history_before_the_canvas` — written months earlier for
  a different reason — failed inside the hour. So `Started` only clears the latch, and the first
  non-zero delta sets it. The exception is a program that wants the mouse: it owns the gesture
  whichever way the fingers go, so there is nothing to wait to see and the landing decides. Not
  making that exception leaks the landing to the canvas, whose `take_camera` would drop a
  camera-follow that a scroll inside vim has no business touching.

  **The latch must outlive `Ended`.** In the fork's `gpui_macos/src/events.rs` (read at
  `a07e5cf`), `NSScrollWheel` maps `NSEvent.phase()` to `TouchPhase` and never reads
  `momentumPhase()`, so a fling's momentum — which keeps arriving for the best part of a second
  after the fingers lift — is a run of plain `Moved` events *after* `Ended`. Releasing the latch
  on `Ended` would drop it at the one moment it exists for. iOS's `UIPanGestureRecognizer` sends
  no momentum, so a latch left standing there is simply never read again.

  **`ScrollDelta` is what tells a wheel from a gesture.** The same file picks `Pixels` when
  `hasPreciseScrollingDeltas` is set and `Lines` otherwise, which on macOS makes `Lines` a mouse
  wheel and nothing else. A notch has no gesture to belong to, so it neither reads nor writes the
  latch — otherwise a wheel would be stuck with whatever the last trackpad fling decided.

  Tests: `the_wheel_adds_up_fractions_and_reaches_a_program_that_wants_it` drives both directions
  of the latch through the real phase order (`Started`, `Moved`, `Ended`, then momentum `Moved`s)
  and checks a `Lines` notch still scrolls a grid whose latch says canvas;
  `a_finger_pan_over_a_shell_scrolls_its_history_before_the_canvas` holds the zero-delta start,
  from the canvas's side where the consequence is visible.

- ✅ **The terminal paints from caches that hold what the screen showed** (2026-09-24). A read of
  the client's hot paths found the element doing per-frame work ghostty does once, and four
  places where it drew the wrong thing. Rulings:
  (1) **The word cache is least recently used under a budget, not swept.** It used to forget a
  word two frames after its last use, so panning or scrolling back to text shown a moment ago
  shaped all of it again (574 words for one 100 × 40 screen; the audit tied this to the
  44–65 ms zoom p99s in MEASUREMENTS, not re-run end to end here); the sweep also walked two
  maps every frame and read the frame probe's diagnostics counter to know what a frame was.
  Now each prepaint stamps the words it uses, nothing is swept, and past 2¹⁸ glyphs (about
  10 MB) the oldest quarter goes, which never touches the current frame's words. GPUI has no
  public frame id, and the stamps make one unnecessary. Keys are FxHash (`rustc-hash`): on the
  terminal's many short writes it measured 46–56 ns a word against foldhash's 57–68 ns and
  SipHash's 176–192 ns. The key drops the focus, which nothing shaped depends on (a click
  reshaped the grid), and gains the cell width the glyphs are placed on, which differs between
  a 1× and a 2× display. Test-only counters left the hot cache for a `cfg(test)` global.
  (2) **Sprites are rasterised once into the atlas.** Box drawing, blocks, Braille and
  Powerline were rebuilt as quads and tessellated paths every frame, with MSAA on each path.
  Each is now an SVG mask, written once per (character, cell in device pixels, thickness) and
  handed to `Window::paint_svg`, whose atlas key is the name plus the size, and tinted with the
  cell's colour. The mask is drawn in device pixels, so a one-pixel line fills one pixel of the
  tile. While the zoom is in motion the geometry is painted instead, so the atlas does not fill
  up with a tile for every size along the way. No fork change was needed.
  (3) **A guess is a cell.** The local echo was a `ShapedLine` painted on GPUI's centred
  baseline in the default style, with a `Font` built and the pending guesses cloned every
  frame. Guesses are now written into their row's cells (faint, single underline), so they are
  shaped, cached, placed on `grid.baseline` and painted in the glyph layer like the host's text.
  (4) **The block cursor shows its character.** Its cell's glyphs and sprite are painted in the
  theme's `cursor_text`, as in ghostty; before, the character under a block read at about 1.9:1.
  (5) **A held key is a typed key.** Auto-repeat skipped the predictor, the latency meter and
  the jump to the bottom; on a slow link the host's echo of the repeats then contradicted the
  predictor and muted it for two seconds. `key_down` sends both through one path.
  (6) **A guess expires by age when it is drawn**, not only when a frame reconciles it, so a
  quiet link cannot leave an unconfirmed guess on screen.
  (7) **Keystroke latency is read at the glass** (revised 2026-09-25). Presentation is
  vsync-synced and the compositor adds about a refresh, so a paint reaches the display a
  refresh or more after its own clock. A paint that holds a waiting key hands the work to
  `slopty_ui::shown::after_paint`, which runs it at `presented_at` of the first frame submitted
  after the paint (`Window::on_frame_presented`, from the fork), and keeps the window's
  presentation reports on only while something waits. The next display tick, used before,
  read 15 ms low on this Mac (MEASUREMENTS 2026-09-25, "keystrokes timed at the glass"). The
  iOS simulator reports no presentation, so there the next tick still stands in.
  Tests: element `the_word_cache_forgets_the_least_recently_used_past_its_budget`,
  `a_pass_never_evicts_its_own_words`, `the_word_key_holds_the_cell_width_and_not_the_focus`,
  `a_guess_is_a_cell_of_its_row`, `text_under_a_block_cursor_takes_the_cursor_text_colour`,
  `the_contrast_check_is_remembered_per_pair`; sprite `a_sprite_mask_covers_whole_device_pixels`
  (the mask as GPUI's renderer rasterises it, pixel by pixel); view
  `a_screen_shown_again_shapes_nothing`, `a_sprite_is_masked_once_and_not_while_zooming`,
  `a_held_key_is_predicted_timed_and_follows_the_output`,
  `a_key_is_timed_when_its_frame_is_presented`; shown
  `a_paint_is_timed_at_the_first_frame_after_it_that_is_shown`; predict
  `a_stale_guess_is_hidden_without_a_frame`.

- ✅ **libghostty-vt at ghostty `7c40388b2`: synchronized output is a render hold** (2026-09-24).
  `vendor/ghostty` moved 145 commits (from `5252b193c`); the libghostty-rs fork pins the same
  commit and its bindings are regenerated. Upstream touched the VT library in six places:
  mode lookup by a comptime sorted set (faster `mode()`), DECRQM answering ANSI modes and not
  truncating 16-bit ones, Unicode 18 widths, `RESIZE_PULL_SCROLLBACK` (for ConPTY; a Unix pty
  keeps no screen of its own, so the default stays), and `GHOSTTY_TERMINAL_OPT_RENDER_HOLD`.
  That last one replaced our mode-2026 polling. The engine checked `SYNC_OUTPUT` when it built
  a frame, so a program that ended a frame and began the next inside one PTY read was seen as
  held, and its finished frame was lost. The timeout ran from the first frame that saw the
  mode set, and it only fired when more output came. Now the fork wraps the callback
  (`Terminal::on_render_hold`) and a way to read the render state without updating it
  (`RenderState::snapshot`). The engine captures the frame into the render state when a hold
  begins, as ghostty's header advises, and builds frames from that capture until the hold ends.
  After `SYNC_OUTPUT_TIMEOUT` (1 s) it ends the hold itself by resetting the mode. A program
  cannot push that deadline back, because setting the mode again during a hold starts nothing.
  The session actor arms a timer for the deadline, so a program that sets 2026 and falls
  silent is still let go. The capture keeps the history length it was numbered against. One
  edge stays: at the scrollback cap, lines evicted after the capture shift its absolute
  numbering until the next frame. There is still no colour-change callback. The eight reads
  and two palette copies that find the program's colour changes now run only after a write
  that carries an OSC or a RIS, plus the write after it. Tests: engine
  `a_hold_begun_again_in_the_same_read_ships_the_finished_frame`,
  `colour_changes_are_seen_even_split_across_writes`; actor
  `a_hold_that_is_never_released_times_out_without_more_output`; fork
  `render_hold_reports_mode_2026_and_sees_the_finished_frame`.

- ✅ **Input waits in the actor, and the exit is the child's own** (2026-09-24). The actor wrote
  a request with `write_all` and awaited it inside its select, so while a paste larger than the
  tty's input queue went in, nothing read the PTY. A program that echoes as it reads (`cat`,
  `tr`, a shell) then blocked on its full output, and the session hung. Input now goes into a
  queue. The tty takes what fits with a non-blocking write, the rest waits for writability in
  its own arm of the select, and reads go on between them. A key is acknowledged
  (`input_ack`) once its last byte is written. Query answers the engine produces join the
  same queue, in order. At PTY EOF the actor used to tell viewers `Exited { status: 0 }`,
  while the real status sat unread in ptyd's socket: `PtydClient::pump` had no caller. A task
  now reads the ptyd connection the whole time and hands replies to requests in order. An
  `Exited` goes to the host as it arrives, and the host passes it to the session, which tells
  the viewers the real status. A session adopted after its child died starts with the status
  ptyd recorded. A resize is sent to ptyd on the tap queue, followed by a checkpoint at the
  new size, so a replacement host replays at the size the program draws for. The ptyd protocol
  is 3: `Detach` and `Signal` had no sender and are gone. Tests: actor
  `a_large_paste_into_an_echoing_program_does_not_deadlock`,
  `the_viewers_are_told_the_real_exit_status_of_the_child`,
  `a_resize_reaches_ptyd_and_the_next_checkpoint_is_at_it`; ptyd
  `an_exit_arrives_without_a_request_to_carry_it`.

- ✅ **A key draws no frame until the tile has something new to show** (2026-09-25). Every
  key made the view notify, so each keystroke drew a frame of its own, and on loopback the
  echo came back 2 ms later. The echo's frame then waited for the next vsync tick, because the
  fork draws an idle window at once only when no frame began within a refresh. It then waited
  one refresh more behind the key's frame, because a `CAMetalLayer` shows each drawable for at
  least a refresh. An unpredicted key reached the glass at 33 ms on a 75 Hz display (release)
  where the floor plus the round trip is 21. The view now notifies on a key only for what shows
  before the answer: a guess, the jump to the bottom, a cursor back from its blink, a cleared
  selection, a sticky modifier used. Committed input-method text follows the same rule. The
  self-test `type` and `keys` commands no longer refresh the window: a keyboard does not, and
  the forced frame put the same refresh back into every reading. A fork fix goes with it: a
  wake that ran only next-frame callbacks and drew nothing gives the immediate frame back, so
  the draw that follows it still goes at once. The unpredicted key now takes 22.3–22.6 ms. A
  predicted key still draws its guess at once (about 21 ms). Its echo, when the round trip is
  under a refresh, is the second frame in that refresh and shows a refresh later. That shows
  only where the guess was wrong. Rejected: presenting through the Core Animation transaction,
  which the fork's `frame_latency echo` measured worse for both frames. The keystroke meter
  splits every key into hops (key → arrived → applied → painted → submitted → glass) so the
  next regression names its hop. The typing scenarios run the shell with an empty `ZDOTDIR`,
  because the user's zsh plugins cost 8 ms a key here, and that number belongs to the shell,
  not to Slopty (MEASUREMENTS 2026-09-25, "keystroke to glass, hop by hop"). Tests: view
  `a_key_draws_a_frame_only_when_the_tile_shows_something_new`; latency
  `the_hops_of_an_echo_and_a_guess_add_up_to_their_totals`; e2e smooth
  `typing_is_timed_with_and_without_the_local_echo_on_the_mac` (prints the hops).

- ✅ **The typing check counts an echo that beats its guess as a key answered in time**
  (2026-09-25). The smooth suite's check wanted 58 of 60 keys drawn as guesses and saw 57 under
  load. On loopback the echo can be applied before the next paint. That frame then shows the
  echo, and the guess never appears, which is correct. The meter now marks each key the
  predictor guessed at when it is pressed. It counts `echo_first` when such a key's echo is on
  the first frame painted after it. It counts `guess_late` when a frame painted after the key
  shows neither its guess nor its echo. The check wants guesses plus echoes-first to cover the
  typed keys, at least one guess drawn, and no late guess. A lower threshold was rejected,
  because it would also pass a predictor that drew fewer guesses. Tests: latency
  `an_echo_that_beats_its_guess_is_told_apart_from_a_late_guess`; e2e smooth
  `typing_is_timed_with_and_without_the_local_echo_on_the_mac`.

- ✅ **An echo beside a window that draws every refresh waits for the next tick** (2026-09-25).
  With six shells flooding, the window draws on every display-link tick, and a typed key's
  echo waits for the next one: 28.3 ms from key to glass, against 21.3 in an idle window. The
  fork's immediate frame only goes to a window that drew nothing for a refresh, and that stays.
  A frame drawn for the echo off the tick, while the tick's frame is still in flight, reaches
  the glass a refresh after that frame, because a `CAMetalLayer` shows its drawables in order,
  each for at least a refresh. That is the refresh the next tick's frame would have reached
  anyway, and the frame after it then queues. The fork's `frame_latency flood` measured the
  echo 1–4 ms later that way, with frames queued. Drawing each tick's frame later, nearer the
  compositor's deadline, was measured too. The deadline sits 2–4 ms after the tick and moves
  with load, and the app's draw already uses 1.1–1.4 ms of that. The gain is at most about a
  millisecond, it was inside the noise, and each miss costs a repeated frame and a queue. Both
  were rejected. What remains is half a refresh of waiting on average, and a faster display
  shortens it (MEASUREMENTS 2026-09-25, "an echo beside floods waits for the tick"). Probe:
  fork `frame_latency flood`; e2e smooth `six_streaming_shells_in_view_on_the_mac` (h).

- ✅ **An echo is framed as it is read, a viewer has two frames in flight, and a guess waits
  for its echo** (2026-09-25). Three defects on the path from the PTY to the glass. (1) Output
  read within 8 ms of the last frame waited for the pace, even when it was a key's echo. In a
  shell beside a spinner or a redrawing TUI every echo waited up to 8 ms, and a two-write echo
  (a highlighter that recolours the line after it) showed its first write for a frame. Now a
  viewer's input, once written, buys two frames ahead of the pace for output read within 50 ms
  of it (`EchoBurst`). A flood beside the typing stays paced, because the budget is per input.
  A short wait for the second write was rejected: tokio rounds a timer up to its next
  millisecond, so every single-write echo would pay 1–2 ms for a repaint that now goes out as
  soon as it is read anyway. (2) Frames were pushed every 8 ms whatever the link drained, into
  a sink 256 events deep, and a viewer counted as behind only when that filled. A throttled
  viewer showed the end of a flood 9.5 s late. While behind it was sent nothing at all, so a
  `Lines` or `Matches` reply was lost and fetched history stayed empty. Now each frame handed
  to a viewer carries a `Credit`, and the connection gives it back by dropping the event once
  written. A viewer has at most two frames not given back. The actor builds the next diff only
  for viewers with room, the engine keeps collecting what changes meanwhile, and a viewer that
  missed a diff is sent every row once it has room (`join_frame`, at the others' sequence
  number). With nobody who has room, the diff waits in the engine. Every other event goes in
  order and is never skipped: one the sink has no room for waits in the actor, and only past
  16 MiB of those is the queue dropped and the viewer told everything again. The same
  throttled viewer is now 147 ms behind. This replaces the "marked behind at a full sink" part
  of the multi-client entry on joining a busy session. Nothing changed in `slopty-proto` or in
  the connection. (3) The worker acknowledges every key written before the read a frame was
  cut from, and that read may hold other output instead of the echo. The predictor counted the
  guess as a miss and muted itself for 2 s, on exactly the slow links where guesses matter. A
  guess now remembers what its cell showed when it was made (the cursor's row of the last
  frame, kept as the `Arc` the screen already holds). An acknowledged guess whose cell still
  shows that stays pending until the echo lands or `STALE`, and only a cell showing something
  else is a miss. Still open: a frame the connection has written waits in noq's stream buffer,
  up to the 1.25 MB stream window, until the link carries it. The credit bounds what
  waits in the channel, not what waits in QUIC. Bounding that needs either a smaller window
  for session streams or a client acknowledgement of frames (MEASUREMENTS 2026-09-25, "echo
  pacing and frames in flight"). Tests: actor
  `an_echo_beside_a_flood_is_not_held_to_the_frame_pace`,
  `a_throttled_viewer_is_a_frame_or_two_behind_not_seconds`,
  `a_reply_reaches_a_viewer_that_fell_behind_the_frames`,
  `a_slow_viewer_is_skipped_then_caught_up_never_dropped`; session
  `input_buys_a_few_unpaced_frames_and_only_soon_after`; predict
  `background_output_does_not_mute_prediction`.

- ✅ **A scroll ships the rows that changed, and a row ends at its last cell** (2026-09-25).
  libghostty rebuilds every row when the viewport pin moves (`render.zig`, `beginUpdate`), and
  the worker's viewport follows the output, so every line scrolled in was a full frame. An
  Enter at a bottom prompt sent the whole screen again: 15.0 kB at 80 × 24 and 88.3 kB at
  200 × 60, for the four rows that changed. The engine now keeps the lines the viewers that
  follow the diffs hold (`ghostty::Shown`: the screen's first absolute line, the numbering,
  the width and each row's line). After a scroll it compares every rebuilt row with the line
  held at the same absolute index and sends only those that differ, in a frame that is not
  `full`. A client that sees `first_visible_line` move on a frame that is not full fills each
  row from its line cache at the new index (`TermState::readopt`) and then applies the rows the
  frame carries. The frame stays full when the numbering or the width changed, and for a
  joiner, a resize and a resync. A joiner holds the screen as it is when it joins, which can
  differ from what the others were sent, so a line that changed and then changed back is
  dropped from the record and sent again. Lines are compared, not hashed. `DefaultHasher` over
  every cell doubled the build (885 µs a scroll at 200 × 60 in release), and a comparison is
  exact. On the wire a `Line` is now its width and its cells up to the last one that is not
  blank; the rest come back blank on decode. A blank cell was 7 bytes, so a 200-column echo row
  (1.46 kB) took two packets at the 1252-byte MTU. Numbers: echo 1461 → 230 B at 200 columns,
  Enter 88.3 kB → 0.66 kB at 200 × 60, a one-line scroll 486–519 → 347–369 µs to build and encode
  (MEASUREMENTS 2026-09-25, "a scroll ships the rows it moved"). Protocol 57. Tests: engine
  `every_diff_applied_by_index_shows_the_terminal` (twelve seeded runs of 600 writes: scrolls,
  scroll regions, erases, wide characters, the alternate screen and joiners, each frame checked
  against a second engine fed the same bytes), `a_scroll_sends_the_lines_that_came_in`,
  `a_joiner_is_sent_a_line_that_changed_back_since_the_others_had_it`,
  `an_echo_and_an_enter_cost_what_changed`; grid
  `a_line_round_trips_without_its_trailing_blanks` (property),
  `a_line_travels_up_to_its_last_content_and_comes_back_whole`; client
  `a_scroll_moves_the_held_rows_up_and_takes_only_the_new_one`.

- ✅ **The alternate screen keeps the primary's numbering for its return** (2026-09-25). Entering
  it bumped the epoch, which also cleared the prompt starts and exit statuses the engine keeps
  per absolute line. Leaving it restored the anchor and the command blocks, then bumped the
  epoch again, and the client dropped its cache on either change. After `vim`, `less`, `man` or
  `htop` the history had to be fetched again, and the prompts above lost their marks, so a
  block's prompt and status were gone. ARCHITECTURE claimed the cache survived; it did not. Now
  the engine parks the primary's epoch, prompt starts and exit statuses with its anchor, and
  gives the alternate screen an epoch never used before (a counter, so an epoch is never handed
  out twice). Returning restores all of them. A reflow on the alternate screen, or an anchor
  that did not survive, gives the primary a new epoch instead. The client puts its line cache
  aside when a frame brings the alternate screen in a new epoch, and takes it back when the
  worker returns to that epoch; any other epoch drops it. Tests: engine
  `the_alternate_screen_gives_the_primary_its_numbering_and_marks_back`; client
  `the_primary_lines_come_back_after_the_alternate_screen`.

- ✅ **A closed sink keeps the driver's seat, and a replaced stream's late frames are dropped**
  (2026-09-25). A re-attach on the same connection aborts the old stream's pump before the new
  attach reaches the actor. If the actor wrote to the closed sink in between, it removed the
  viewer and handed the size to another viewer, so the client that re-attached had lost the
  driver. The actor now removes a viewer whose sink closed without handing anything on, and
  keeps the driver's seat for it along with that sink (`Actor::orphan`). An attach by the same
  client keeps the seat. A detach through that sink passes it on. That detach comes from the
  connection's teardown (`detach_sink`) or from the client's `Detach`. A detach through a sink
  that was replaced changes nothing. On the client, the old stream's buffered frames can arrive
  after the new stream's, which showed old rows until they changed. Frame numbers only grow
  within a session's actor, and a client's `TermState` lives for one connection, since the app
  rebuilds its views on a new one. So a frame numbered below the last one applied, or a diff at
  the same number, is dropped (`TermState::superseded`). A full frame at the same number is
  still taken, because a joiner's frame carries the others' number. Rejected: an attach
  generation in `UniHead::Session`. The frame number already carries the order. A generation
  would add a field that the connection, the stream framing and the client link all have to
  carry, and the actor still could not tell a closing sink from a leaving client without the
  seat rule above. Tests: actor
  `a_closed_sink_keeps_the_drivers_seat_until_its_connection_detaches_it`; client
  `a_frame_older_than_the_last_applied_is_dropped`.

- ✅ **What waits in the transport is bounded by markers the client answers** (2026-09-25). The
  frame credit counts a frame done once the connection has written it, but a write returns
  once the frame is in noq's stream buffer. That buffer holds up to the 1.25 MB stream window,
  five seconds at 250 kB/s. The worker now sends a `TermEvent::Marker { id }` after every
  16 KiB of frames to a viewer. The client answers `TermRequest::Reached { marker }` when it
  applies the marker, and every event before it has been applied by then. A viewer that has
  answered at least once is sent no further frame while more than `FRAMES_UNREACHED_BYTES`
  (64 KiB) of its frames are unconfirmed. It misses the diffs meanwhile and gets every row once
  an answer opens it again, as with the credit. A tool that reads the stream raw and never
  answers is not held back, and at most 16 markers are kept for it. Measured with a model of
  the stream buffer (writes return while the window has room, the link drains at 250 kB/s):
  the viewer showed a flood's end 5096 ms after the program printed it, with 1.24 MB waiting.
  Now it shows it after 265 ms, with at most 64 kB waiting (MEASUREMENTS 2026-09-25, "a scroll
  ships the rows it moved"). This closes the item the entry on echo pacing left open. Rejected: a
  smaller stream receive window set by the client. noq has no per-stream window, only
  `TransportConfig::stream_receive_window` for every stream of a connection, so downloads and
  tunnels would be capped at the window per round trip. Also rejected: acknowledging every
  frame, which would send a message upstream per frame. Tests: actor
  `a_throttled_viewer_behind_the_stream_window_is_under_a_second_behind`, session
  `a_viewer_is_held_to_the_frames_it_confirmed_once_it_answers`; client
  `a_marker_is_answered_once_the_events_before_it_are_applied`.

- ✅ **A dropped queue, a joiner on the alternate screen and a full outbound queue no longer
  stall or blank a viewer** (2026-09-25). Three holes in the frame credits, markers and scroll
  deltas above. (1) A viewer whose queue passed 16 MiB (a kitty upload, a large `Lines` or
  `Matches` reply) lost the frames and markers queued with it, but its `Reach` still counted
  those frames as sent. Once the sink drained it was more than `FRAMES_UNREACHED_BYTES` behind
  with no marker left to answer, so it was never sent a frame again. Its count now starts
  again when the queue is dropped, and a frame for a viewer already lost is neither claimed nor
  counted. (2) `Shown` is kept by the diffs, and a joiner only forgot rows from it when the
  record was of its own numbering. With no diff taken while a program took the alternate
  screen, a viewer joining there held nothing of the primary's numbering, yet the diff after
  the program left was not full, because the record still held that numbering. The viewer
  filled its screen from a cache it did not have and showed blank rows. A viewer catching up
  the same way restored an older cache. A joiner whose frame is in another numbering or size
  now clears the record, so that diff is full. The client also asks for every row when a diff
  arrives in a numbering it holds no lines for, as it does after a gap. (3) The view dropped
  a `Reached` answer when the link's queue was full, which could hold its frames until the
  next marker, and no marker comes while it is held. An answer now waits for room, and only
  the newest waits, since it confirms every marker before it. Tests: actor
  `a_viewer_whose_queue_was_dropped_gets_frames_again_once_it_reads`; engine
  `a_viewer_that_joined_on_the_alternate_screen_is_sent_the_primary_whole`, with
  `every_diff_applied_by_index_shows_the_terminal` now also joining while no diff is taken;
  client `a_diff_in_a_numbering_not_held_asks_for_every_row`; view
  `a_marker_answer_waits_for_room_in_a_full_outbound_queue`.

- ✅ **A mark that changes a prompt's status without touching its cells resends the prompt's
  row** (2026-09-25). A prompt row's `Prompt { exit }` is not in the grid. The engine derives
  it from its own `prompt_starts` and `exit_marks` when it builds the row. The status is that
  of the newest `133;D` up to `EXIT_LOOKBACK_ROWS` above, unless an `133;A` in between took
  it. A `D` or an `A` written on a row above a prompt the viewers already hold changes that
  prompt's status, but libghostty dirties no row for it, and a diff builds only dirty rows.
  The viewers kept the old status. This came before the scroll deltas. Every change to the
  marks now goes through `remark`, which compares each row the change can reach (the marked
  line and the `EXIT_LOOKBACK_ROWS` below it) before and after, and puts the rows that changed
  in `remarked_rows`. That covers a new `A` or `D`, pruning below `base`, and a prompt erased
  in place. The next diff builds those rows, reads their prompt flag from the live grid as it
  does for `forced_rows`, and sends the ones that differ from `Shown`. A joiner reads the set
  and leaves it for the diff, and a hold keeps it until the hold ends. Rejected: forcing every
  row a mark reaches into the frame. Most marks change nothing below them, and each prompt
  would send four more rows. Found by `every_diff_applied_by_index_shows_the_terminal` once
  its joins were drawn from the main generator (seed 10, step 431). The harness now draws them
  there and runs 20 seeds; 60 more passed by hand. The build time did not move. A one-line
  scroll at 200 × 60 took 356–385 µs before and 352–365 µs after, six interleaved release
  runs each, and echo and Enter bytes are unchanged. Test: engine
  `a_mark_above_a_sent_prompt_resends_its_status`.

- ✅ **The shell integration reports the working directory** (2026-09-25). The new tile
  header and status bar name where a shell is from `SessionSummary.cwd`. The worker only
  learned that from OSC 7, which most prompts never send; the user's zsh with starship does
  not. So every header read just "shell". The zsh and bash hooks now emit
  `OSC 7 file://$HOST<path>` before each prompt, and fish on `fish_prompt`, with `%` and space
  percent-encoded. The engine accepted a named host only when it matched `$HOSTNAME` or
  `/etc/hostname`, and a Mac has neither, so it dropped any OSC 7 that named this machine,
  zsh's included. It now compares against `uname`'s node name, read once. Tests: pty
  `an_interactive_zsh_emits_prompt_marks`,
  `an_interactive_bash_emits_prompt_marks_and_runs_the_users_bashrc` and the fish one, each
  moving into `a b%`; engine `cwd_from_osc7` cases, including this machine's name.

- ✅ **An exited shell stays until it is closed** (2026-09-25). The client sent `Close` the
  moment its view heard `Exited`, and the worker daemon ended the session on the child's exit
  by itself as well. So the tile's `Exited · code N` pill with Restart and Close showed for a
  round trip, and a build that failed took its last screen with it. Warp, Orca and ghostty
  keep a finished surface until the human closes it, and tmux does with `remain-on-exit`.
  Ruling: an exit ends nothing. The worker records it, tells the attached viewers on the
  session stream, and sends everyone else the session's summary again with
  `SessionState::Exited`. A repeated `SessionOpened` was already how a known session reports a
  change to the server, so the protocol is unchanged. The per-client dedupe (`Heard`) now
  remembers each session's state, so a summary in a new state gets through. The client
  marks its copy of the summary exited as soon as its view hears it. The tile keeps its view
  and its last screen, and the pill stays until Close or Restart. Close is ⌘W's path: the
  tile goes at once, the session after the five-second undo, which now works for an exited
  shell too, since its view is still attached. An exited shell never asks before it closes.
  A reconnecting client gets the exited session in its `HelloAck`, and a second client
  attaching gets the `Exited` event on attach. Both show the same pill, and a Close from any
  client closes the session for all of them, as with a live shell. Orchestration can still
  `ReadScreen` an exited terminal until something closes it.

  The bound: an exited session that no client has watched for `EXITED_UNWATCHED` (24 h) is
  closed by the worker daemon, with `CloseReason::Exited`. A sweep every ten minutes reads the
  viewers of the exited sessions only (`Worker::stale_exits`); any viewer restarts the clock.
  A day outlasts a night and a working day away from the machine, so a tile left on a screen
  is still there the next morning, while terminals opened by orchestration, which nobody
  attaches to, do not pile up. A session whose program exited while the daemon was down is
  no longer ended at start; the sweep takes it in its turn. Rejected: closing on the exit
  with the tile kept locally, since a second client or a reconnect would lose the screen, and
  a count bound, since what matters is whether anyone will come back to it. Tests: worker
  `an_exited_session_goes_only_after_the_bound_unwatched`; daemon
  `an_event_the_greeting_carried_is_not_told_again` (a new state is news once) and
  `a_worker_registers_answers_forwarded_verbs_and_comes_back` (the exit is announced as a
  summary, the screen stays readable, `Close` ends it); workspace
  `an_exited_shell_stays_until_it_is_closed`, `an_exited_shell_offers_restart_and_close`.

- ✅ **A view scrolled up holds still, and a pill counts the lines below** (2026-09-25). The
  view's offset counts lines up from the bottom, so the rows it showed slid up under the
  reader as output arrived, although `TermState::apply` said it kept them anchored. Every Mac
  terminal holds a scrolled view still. Ruling: when a frame moves the screen down the
  numbering in the same epoch while the view is scrolled up, the view keeps the top line it
  drew (`TerminalView::hold_top`) and the offset grows by what arrived. That is two reads of
  the state and no rows. A new epoch still returns to the bottom. The scrollbar's "scrolled"
  flash now keys on the top line drawn, not the offset, so output arriving under a still view
  does not flash it. The fix lives in the view because `slopty-client` belonged to another
  change this round; `TermState::apply` is where it should move.

  While scrolled up, a pill at the foot of the body says `N lines below · Back to live`. N is
  the offset, one read per frame. The whole pill is a button, and ⇧⇲ / ⌘⇲ do the same. It sits
  where the tile's state pill goes. The tile tells the view when its own pill shows
  (`set_covered`), and the view then draws none. The pill appears and goes with the scroll and
  has no motion of its own, so Reduce Motion has nothing to take away. It costs 15 µs of a
  170 µs frame (MEASUREMENTS, "the lines-below pill"). Tests: view
  `scrolled_up_the_pill_counts_the_lines_below_and_goes_back_to_live`,
  `the_lines_below_give_way_to_the_tiles_pill`; workspace
  `the_exited_pill_takes_the_place_of_the_lines_below`.

- ✅ **Local echo waits for an echo after any key it cannot follow** (2026-09-26). The
  predictor's only guard against drawing a password was `TermModes::ECHO_OFF`, which nothing
  set, and a key it did not guess at (an arrow, Enter, a line-editing chord sent as bytes, ⌥ as
  Alt) left its cursor where the key had moved away from. Ruling, after mosh: any key that is
  not a plain printable one drops the guesses and holds the next ones back until the worker
  acknowledges that key; bytes the predictor never sees (a chord, a paste) do the same for
  the key after them (`Predictor::interrupt`, called from the view's `send`). From then on
  guesses are *tentative*: made and checked, never drawn, until one is confirmed by the
  worker's echo. A view also opens tentative, since it may attach to a password prompt. A
  prompt that does not echo therefore never shows what is typed, whatever the link. ⌥ as Alt
  is a chord, not a printable. The two gates that disagreed are one,
  `TermModes::prediction_allowed` (alternate screen, echo off, cursor hidden, mouse tracking;
  the kitty keyboard protocol does not count, since fish turns it on at every prompt).
  The worker's reading of `termios` rides on the frames through
  `GhosttyEngine::set_line_discipline` (`ECHO_OFF`, `CANONICAL`); a change sends a frame
  though no cell moved. The worker calling it after each read belongs to the pty change.
  Tests: predict `a_prompt_after_enter_shows_nothing_typed_until_it_echoes`,
  `a_key_it_cannot_follow_pauses_guessing_until_acknowledged`; grid `prediction_gates`;
  engine `the_line_discipline_is_in_the_modes_and_sends_a_frame`; view
  `a_line_editing_chord_stops_the_guesses`.

- ✅ **A program that asked for the mouse hears the whole click and the drag** (2026-09-26).
  Only presses were sent, the middle button had no listener, and the engine counted presses
  up and releases down, so without releases a program saw every button held forever. The view
  now keeps the buttons whose press went to the program and sends their release (inside or
  outside the tile), a move while one is down when the program asked for drags (1002) or all
  moves (1003), and a move with no button under 1003; a move within the same cell is not
  news, and ⇧ keeps the pointer the reader's. The middle button reaches a program that asked.
  The frames carry `MOUSE_DRAG` and `MOUSE_MOTION` so the client asks nothing of a program
  that wants only clicks. The engine keeps the buttons held as a set, so a doubled press or a
  stray release cannot leave one down. Tests: engine
  `press_motion_and_release_follow_the_buttons_held`; view
  `a_program_hears_press_drag_and_release`.

- ✅ **The history cache keeps what the view is near, and a copy waits for its lines**
  (2026-09-26). The client's cache (20 000 lines, against the worker's 50 000) evicted the
  lowest index first, so a fetch far up the history could evict itself in the same insert;
  nothing remembered a fetch in flight, so every frame asked again while scrolled up; and ⌘A
  asked for the whole history in one request the worker caps at 4096 lines, then ⌘C copied
  blanks for whatever had not arrived. Ruling: past capacity the line farthest from the view's
  top goes first, never a line of the batch being inserted and never a screen row (a screen
  that moves down re-adopts its rows from the cache). Following the output that is still the
  oldest line. The cache stays at 20 000 lines rather than growing to the worker's 50 000,
  since a history of wide rows would cost hundreds of megabytes per shell. `TermState` keeps
  the fetches in flight with the numbering they were asked in: a range in flight is not asked
  again, an answer for a numbering that has gone is dropped, and a whole frame (a new stream)
  asks again for what is still missing. Requests are at most 4096 lines
  (`slopty_proto::terminal::MAX_FETCH_LINES`). ⌘C walks the selection in order and writes the
  clipboard once every line is in, asking for a line that never came; the text is built a
  line at a time (`SelectionText`), so it never needs the whole history cached at once.
  Found on the way: `GhosttyEngine::lines` answered a range wholly below its oldest line with
  the lines after it. Tests: grid
  `eviction_is_farthest_from_the_view_and_spares_the_batch_and_the_screen`; client
  `a_range_in_flight_is_asked_once`, `a_long_range_is_asked_in_chunks_and_stale_answers_are_dropped`;
  engine `a_range_below_the_oldest_line_is_empty`; view
  `the_keys_page_through_history_and_select_it_all`.

- ✅ **A wrapped line is one line to a link, a path and a triple click** (2026-09-26). ⌘-click
  and the ⌘-hover underline looked at one row, so a URL or a path the terminal wrapped opened
  its first half; three clicks selected one row. The link and path finders now read the
  logical line (the row and those soft-wrapped onto it, eight rows either way for a hover),
  an OSC 8 run carried on at the next row's start is one link, the underline spans the rows,
  and three clicks select the whole logical line. Tests: url `a_wrapped_link_or_path_is_one`;
  view `a_wrapped_link_is_one_and_three_clicks_take_the_wrapped_line`.

- ✅ **ptyd takes a session back however its holder goes, and an attach always fits one frame**
  (2026-09-26). A connection released what it held only when its loop ended cleanly. A worker
  that died while ptyd wrote its `Attached` reply, or a frame that did not decode, returned
  early and left the session claimed and its reader paused, so no later worker could attach
  and the child's output stopped draining. The release now lives in the connection's `Drop`,
  and the claim is recorded before the reader pauses. The reply itself could also outgrow the
  16 MiB frame: a 12 MiB checkpoint and a 4 MiB ring plus the envelope. The ring is now capped
  at 4 MiB (`--backlog-bytes` refuses more) and a checkpoint at what is left of a frame
  (`MAX_CHECKPOINT_BYTES`, about 12 MiB less 4 KiB). The worker sends nothing larger, and ptyd
  ignores a larger one and keeps the ring. Tests (ptyd):
  `a_worker_that_dies_mid_attach_leaves_the_session_to_the_next`,
  `a_frame_that_does_not_decode_hands_the_session_back`; `the_largest_attach_fits_one_frame`
  (`slopty-pty`).

- ✅ **A worker that loses ptyd exits** (2026-09-26). The reader of the ptyd connection ended
  quietly, and the worker served on with masters nobody kept for its successor and nothing to
  spawn into, logging a warning per tap. The channel of exits now ends with the connection,
  and the worker takes that as the end: it shuts down as it does on SIGTERM and exits with a
  failure, and launchd starts one that connects to ptyd again. The tap loop warns once, not
  per read. Tests: `the_exit_channel_ends_when_ptyd_goes` (ptyd),
  `a_worker_that_loses_ptyd_exits_to_be_restarted` (worker e2e).

- ✅ **Input waiting for a program is held to 16 MiB** (2026-09-26). The actor queues what the
  tty cannot take yet, so a paste into a program that echoes as it reads never deadlocks. A
  program that stops reading (a stopped job, a hung TUI) let that queue grow without bound.
  A request that would take it past 16 MiB is now refused whole, and the viewer that sent it
  gets a `TermEvent::Error` saying the program is not reading. An answer the engine owes a
  query is dropped with a warning, since nobody asked for it. Test:
  `input_past_the_queue_bound_is_refused_and_its_sender_told`.

- ✅ **The frames say when the tty stops echoing** (2026-09-26). Predictive echo must never
  draw a password. The engine's escape sequences cannot tell, but the tty can: a master shares
  its slave's termios, so one `tcgetattr` on it reads `ECHO` and `ICANON`
  (`slopty_pty::line_discipline`). The actor reads them after every read of output, since a
  program turns echo off before it prints the prompt it is for, and hands them to the engine
  (`GhosttyEngine::set_line_discipline`), which puts `TermModes::ECHO_OFF` and `CANONICAL` in
  the next frame's modes. It costs 0.28 µs a read (MEASUREMENTS, "the ptyd tap, framed
  once"). Tests: `the_master_sees_the_programs_echo_and_canonical_modes` (`slopty-pty`),
  `the_frames_say_when_the_tty_stops_echoing` (actor).

- ✅ **The ptyd tap is framed once, as byte strings** (2026-09-26). Each read went to ptyd as
  a copy into a `Vec`, then a copy into the frame, and serde wrote a `Vec<u8>` one call per
  byte. The actor now builds the frame where it read, and the protocol's byte fields are
  byte strings. Postcard writes those as it wrote the sequence, so the wire is unchanged. A
  64 KiB tap costs 1 to 5 µs instead of 70 to 210, and a 4 MiB checkpoint reaches ptyd in
  1 ms instead of 11 to 18 (MEASUREMENTS, "the ptyd tap, framed once"). Tests:
  `byte_strings_keep_the_wire_of_a_sequence_of_bytes`,
  `the_borrowed_output_frame_is_the_requests_frame`.

- ✅ **A failed block wears a bar and a wash, and says how it ended under the pointer**
  (2026-09-26). Amends the 2026-09-05 ruling that a failed command's separator turns red. A
  1 px red rule at the top of the next prompt was easy to miss, and it sat at the wrong end:
  it marked the prompt that reported the failure, not the command's own rows. As in Warp, a
  block whose command exited non-zero now carries a 2 pt bar of the `error` token down the
  element's left edge (in the inset beside the text, `spacing.xxs` wide) and a wash of
  `error` at `alpha::FAINT` across its rows, from its prompt to the next one. Every separator
  is the neutral rule now, never on the grid's top row. Rulings: (1) a block's status is its
  own command's, read from the next prompt's row (`TermState::block_exit`); `BlockHead::exit`
  and `CommandBlock::exit` now carry that instead of the status of the command before the
  prompt, which had turned the sticky header red under a block that succeeded;
  (2) `TermState::failed_runs` picks the rows once per frame from the marks. It walks up from
  the bottom carrying the status, starts from the first prompt below the view, and leaves
  rows above the first prompt to no block, so a block cut off by either edge is still washed.
  The alternate screen has no blocks; (3) under the pointer a finished or running block
  shows its facts at the right end of its prompt row: "Exit 1" in the error tone ("Exit 0"
  muted, "Running" before the next prompt), its duration however short, and a "…" button
  ("Block actions") that opens the block menu a right click opens. The facts replace the
  row's "took" caption while shown. When the prompt has scrolled above, the sticky header
  shows them in place of its caption. Hover is followed by the element's window-level move
  listener, so the facts and the header over the grid keep their block. Nothing is hovered
  on the line still being typed. The figures are tabular; (4) the sticky header sits on the
  content surface with the neutral rule under it, its text starts at the grid's first column
  (`spacing.inset()` at the zoom), and a failed block's bar and wash continue up into it.
  The bars and washes cost about 1 µs a frame and the hovered facts about 15 µs while shown
  (MEASUREMENTS, "failed blocks in the paint"). Tests:
  `failed_runs_cover_the_blocks_whose_command_failed` and the exits in
  `prompt_navigation_and_last_output_follow_the_marks` (client);
  `cmd_up_and_down_walk_the_prompts_and_separators_follow` (bars and washes read from the
  scene), `a_hovered_block_shows_its_status_duration_and_menu` (headless); golden
  `terminal-failed-block`.

- ✅ **The branch follows a shell to the navigator** (2026-09-26). The worker's
  `TermEvent::Cwd` carries the branch. `Effect::Cwd` and `TerminalViewEvent::Cwd` pass it on,
  `TermState::branch` and `TerminalView::branch` hold it, and the workspace writes it into
  the session's `SessionSummary::branch` alongside the directory and the repository. A `cd`
  or a checkout therefore reaches the navigator and the status bar without a new summary.
  Tests: `events_are_kept_and_re_emitted_as_effects` (client),
  `a_cwd_with_a_new_branch_updates_the_summary` (headless workspace).

- ✅ **An echo owed for want of room goes as soon as room returns** (2026-09-26). The input
  exemption (`EchoBurst`) let an echo past the 8 ms pace only as it was read. An echo read
  while its viewer had both frames in flight, or its unconfirmed bytes used up, was owed, and
  `frame_room` then held it to the pace counted from the last frame: 5–6 ms p50 and up to
  10 ms, on exactly the slow links where frames wait for room (typing into a TUI beside an
  agent's output). Now `frame_room` spends the burst on an owed diff as the read does, and a
  read that finds no viewer with room keeps its budget for then (`Actor::echo_frame`). The
  owed echo goes 0.1 ms after the room (MEASUREMENTS, "an echo owed for want of room"). A flood
  stays paced: the budget is still two frames per input. Test:
  `an_echo_owed_for_want_of_room_goes_when_room_returns` (actor).

- ✅ **The checkpoint waits while someone types, and a forced one follows the frame**
  (2026-09-26). The actor formats the whole terminal for ptyd 500 ms after the last output,
  on its own thread. On a full 50 000-line history that is 13–19 ms, and a key typed after a
  half-second pause landed in it, its write, echo and frame waiting behind the formatter
  (MEASUREMENTS, "a key after a pause and the checkpoint"). The quiet-spell checkpoint now also
  waits `CHECKPOINT_AFTER_INPUT` (2 s) past a viewer's last input, so none runs while someone
  types: the keys went from 13–15 ms p50 to 0.2–0.9 ms. A crashed worker loses at most those two
  seconds more of what ptyd's ring does not hold. The checkpoint forced every 1 MiB, which a
  flood still needs, runs after the frame of the read that made it due
  (`Actor::checkpoint_if_owed`). Formatting only what changed since the last checkpoint would
  make each run sub-millisecond, and needs the formatter in `slopty-engine`; it is left open.
  Test: `a_key_after_a_pause_does_not_wait_for_the_checkpoint` (actor).

- ✅ **A state too large to keep stops forcing checkpoints** (2026-09-26). A full history of
  full-width coloured rows at 200 columns formats to 17.5 MB, past the 12.6 MB ptyd keeps. Plain
  text fits up to about 250 columns. The actor dropped such a state but left `tap_lost` set, so
  a session that had lost a tap owed a checkpoint on every read. Each read then formatted the
  whole state after its frame, 70–78 ms, and the taps stayed stopped (MEASUREMENTS, "a state too
  large to keep"). Now a state found too large sets `oversize`. That clears `tap_lost`, keeps the
  taps going past any later hole, and forces no checkpoint (neither for a lost tap nor every
  1 MiB) until one fits. The quiet spells still try, so a `clear` or a resize that shrinks the
  history brings the checkpoint back. Until then a replacement worker replays ptyd's ring, the
  newest 4 MiB, as after any overflow. The actor's time after an echo's frame went from 70–78 ms
  to 0.04–0.11 ms p50. Capping the history a checkpoint carries, or formatting only what changed,
  would let such a session be kept whole; both belong in `slopty-engine`. Test:
  `a_state_too_large_to_keep_is_not_formatted_for_every_read` (actor).

- ✅ **A line editor's prompt is guessed at, and guesses show from half a refresh**
  (2026-09-26), amending **The frames say when the tty stops echoing**. Two things kept every
  guess off the glass on a mesh:
  - zle, readline and fish turn off both `ECHO` and `ICANON` at their prompts and echo each
    key themselves, so since that ruling every shell prompt carried `ECHO_OFF`, and
    `prediction_allowed` refused it. A password prompt is echo off *with* the tty buffering
    the line (`CANONICAL`). Only that pair now refuses. A raw read that echoes nothing
    (`read -s -n1`, a pager's key prompt) is guessed at like a line editor: its guess is a
    miss the next frame corrects, and a miss mutes the predictor, as on any link.
  - The draw threshold was mosh's 25 ms, never measured here, above the tailnet's 10–12 ms
    echo. It is now half the display's refresh (`Predictor::set_refresh`, read from
    `slopty_platform::display_refresh`; 60 Hz until known), after the warm-up hits and with
    the mute as before. Through a shaped link, adaptive prediction put the guess on the glass
    14–16 ms ahead of the echo at 10 and 15 ms, with no misses. The old threshold drew none up
    to 20 ms (MEASUREMENTS, "the prediction threshold over a shaped link").
  Tests: `prediction_gates` (grid), `the_line_discipline_is_in_the_modes_and_sends_a_frame`
  (engine), `guesses_show_from_half_a_refresh` (predict).

- ✅ **A summary carries its repository's changes, counted off every latency path**
  (2026-09-27). A person running agents on several Macs checks first what each has changed, and
  the summary had the repository and the branch but not that. `SessionSummary.changes` is the
  working tree against `HEAD`: files, lines added and lines removed.
  - **Counted by git, never where a key waits.** A session actor only sends which repository it
    is in over a channel, when it reads the branch (a directory reported, a command ended) and
    when it starts. `slopty_worker::changes` runs `git diff --numstat HEAD` and `git ls-files
    --others --exclude-standard` on a Tokio task, one run per repository at a time and none
    sooner than 2 s after the last began. Touches during a run fold into one more. A count of
    32–40 ms here and 170 ms on ghostty's tree (MEASUREMENTS.md, "counting a working tree's
    changes") therefore costs a background core at most a twelfth of its time.
  - **No lock, no dialog.** Both runs pass `--no-optional-locks`, so a count never takes the
    index lock from a person's own `git`. The git run is the first on `PATH`, Homebrew's, or the
    Command Line Tools' or Xcode's own, never `/usr/bin/git`: on a Mac without the developer
    tools that shim offers to install them, and a daemon must not put up a dialog. No git, no
    `HEAD`, a failure or a run over 10 s gives `None`.
  - **A changed summary reaches a direct client.** A client connection told a session's summary
    again only when its state changed (the program exited). The directory and branch reach
    viewers as `TermEvent::Cwd`, but the changes have no event. `Heard` now remembers the whole
    summary it sent and lets through one that differs.
  - Tests: `numstat_counts_files_and_lines_and_a_binary_as_a_file`,
    `touches_fold_into_one_more_count_and_only_a_change_is_announced`,
    `a_working_tree_is_counted_against_head`, the worker's
    `a_shell_in_a_repository_carries_its_changes` and
    `an_event_the_greeting_carried_is_not_told_again`, and the `worker_session_opened` golden.

- ✅ **A shell forgets the Claude Code session its daemon ran in** (2026-09-27). Claude Code
  hands the programs it starts variables naming its session: `CLAUDECODE`,
  `CLAUDE_CODE_CHILD_SESSION`, `CLAUDE_CODE_SESSION_ID`, `CLAUDE_CODE_MESSAGING_SOCKET` and its
  token, `CLAUDE_PID` and a few more. A ptyd started from inside a session (a developer's, or a
  test run by an agent) passed them to every shell. A `claude` started there then took itself
  for that session's child: it saved no transcript and reported to the parent's inbox. Found
  while measuring Claude Mods. `slopty_pty` now removes those names before the session's own
  environment is applied. The user's Claude Code settings in the environment
  (`CLAUDE_CODE_USE_BEDROCK` and the like) pass through. Test:
  `a_shell_forgets_the_claude_session_the_daemon_ran_in`.

- ✅ **A terminal with fewer rows trims blank rows before it scrolls into history**
  (2026-09-27). On the iPad, the soft keyboard going away and coming back (71 rows, then 51 again)
  left a shell showing only its prompt, with its earlier output pushed into scrollback and the
  cursor on the top row. Every height shrink on the Mac did the same. Ghostty trims blank rows
  off the bottom first on a shrink, as every terminal does, but it never trims a row that holds a
  tracked pin, and the engine kept its line-numbering anchors pinned to the bottom active row.
  So nothing was trimmed, and the whole screen scrolled up. `GhosttyEngine::resize` now drops
  both anchors before `term.resize` whenever the size changes. The resize starts a new numbering
  and pins again afterwards, so the anchors held nothing a resize would keep. Test:
  `fewer_rows_trim_blank_rows_before_scrolling_into_history` (shrink, grow and shrink, and a
  resize of the primary screen under the alternate one). The iOS e2e now asserts the echo is
  still on screen before the columns golden and the Split View golden.

- ✅ **A guess looks like the text it continues, and is marked only when unsure** (2026-09-27),
  amending (3) of **A guess is a cell**. Every guess was drawn faint and underlined, so a key
  looked final only when its echo came. Over a 10 ms round trip the guess was on the glass at
  21 ms and the key as it stays at 35 ms, and every key changed its look once on the way. mosh
  draws its predictions in the terminal's own style and underlines them only when it flags them.
  Slopty now does the same (`Predictor::marked`). A guess takes the style of the glyph before it,
  or, after a blank such as the space after a prompt, the style of the cell it covers. It is marked
  (faint, underlined) in three cases, with mosh's thresholds: on a link of 80 ms or more, until the
  link falls under 50 ms (`FLAG_TRIGGER_HIGH` / `LOW`); while a guess has waited past 250 ms
  (`GLITCH_THRESHOLD`); and after an echo slower than that or a miss, until ten guesses in a row
  have been echoed within it (`GLITCH_REPAIR_COUNT`). A wrong unmarked guess shows for one round
  trip, then the miss clears it and mutes the predictor, as before. On the tailnet's 10–60 ms, a
  key now looks final 21 ms after it is pressed at 10, 15 and 20 ms, where it took 35–42 ms. In
  the same change the app passes the handshake's round trip to the predictors at link-up rather
  than 500 ms later, so adaptive prediction draws 58 of 60 keys instead of 55 (MEASUREMENTS,
  "typing over a shaped link: guesses that look final"). Tests: predict
  `a_guess_is_marked_only_when_it_is_unsure`; element `a_guess_is_a_cell_of_its_row`; e2e smooth
  `typing_over_a_shaped_round_trip_on_the_mac`, which now wants the adaptive policy to draw at
  least half the keys past half a 60 Hz refresh.

- ✅ **A prompt steps back so its command reads first, and a block spans its tile**
  (2026-09-27, design direction wave 3). Warp sets a block's prompt apart from the command and
  runs its blocks, rules and failure wash across the whole pane.
  - **The prompt.** On a prompt row the cells before the command's column (`OSC 133;B`,
    `SemanticMark::input_col`) that the shell drew in the default colours take the faint
    attribute (SGR 2), as if the shell had dimmed them; with nothing typed yet the prompt ends
    at the shell's own cursor, and a prompt row off the cursor's row is prompt to its end. A
    prompt the shell coloured keeps its colours, and the command and its output stay as they
    are. The cursor read is the shell's, never the predictor's, so a guessed key is not set
    back with the prompt. The row is copied only when a cell changes.
  - **Edge to edge.** The rule over a prompt, a failed block's wash and the sticky header's
    wash span the element. Clipped at the grid's last column they left a ragged strip at the
    right, up to a cell plus the inset, that changed with the tile's width, while the bar sat
    on the left edge. Block padding in line units (Warp's 1.1, 0.5 and 1.0) stays open: it
    changes how the grid maps rows.
  - Cost: about 2.4 µs a frame with ten prompts on screen, nothing measurable without prompts
    (MEASUREMENTS, "a prompt set back, and blocks edge to edge"). Tests: element
    `a_prompt_steps_back_and_its_command_reads_first`; view
    `cmd_up_and_down_walk_the_prompts_and_separators_follow` and the failed-block tests, whose
    scene readers now want the rule and the wash edge to edge.

- ✅ **A block's head is a surface, and its rule is that surface's top edge** (2026-09-27,
  design critique round 2, finding 9). The rule over a prompt lay on the prompt row's top edge,
  2 px over the prompt's capitals and 3 px under the output's baseline. It read as a line under
  the output. Warp gives each block padding in line units (1.1 above, 0.5 between command and
  output, 1.0 below) because every Warp block is its own grid. Slopty draws one grid whose rows
  are the PTY's rows, so the options were weighed against that:
  - **Pixels between rows: rejected.** With g points over each prompt on screen the grid is
    `rows × line + g × prompts` tall, and the count of prompts changes as output scrolls. The
    PTY would either give up the worst case for good (4 pt a prompt is a quarter of every row
    at 17 pt) or overflow, so that the shell's top rows sit under the tile header even at the
    bottom of history, where an inline program (fzf `--height`, a progress bar redrawn with
    cursor-up) writes. Every pixel-to-cell map (mouse reports, selection, links, hover, the
    input method's caret, images spanning rows, the cursor) would turn into a sum over the
    marks, and a new line would scroll by one row or by one row plus g. That is uneven motion
    on the path that comes first.
  - **The rule in a gap row: rejected as the answer.** It has space only where the shell leaves
    a blank row (starship's `add_newline`), which the client cannot add. Most prompts would
    keep the old look.
  - **Chosen: a head surface.** The rows a command was typed on, meaning its prompt rows and
    any `Input` rows continuing the command under them, sit on `surfaces.panel` edge to edge
    (`head_color`). That is the composer's step, the conversation face's input. The rule stays
    where it was, drawn over the surface as its top edge. The space problem goes away because
    a band fills whole rows: its edges fall outside the glyphs' ink by the cell's own leading.
    The rule is enclosed with the command, so it reads with the command below it and no longer
    with the output above. That is the common-region rule, and it outweighs nearness.
    Heads with nothing between them, such as a `cd` followed by the next prompt, share one band
    and are split by their rules. The prompt that opens a terminal and the one on the grid's
    top row get the surface without a rule, as before. A failed block's wash lies over its
    head. The alternate screen has no heads. The sticky header takes the same surface, so a
    head that scrolls away is replaced by a header that looks like it. `raised` was not used:
    it is the hover fill of the "…" button that sits on the head row. No row moves, so neither
    the scroll mapping nor any hit test changes.
  - Cost: about 1 µs a frame with ten heads on screen; the dense screen with no marks is
    unchanged. Computing the heads inside the row loop cost the dense screen's
    screen-a-frame case 80–100 µs, which is why they come from their own pass over the marks
    (MEASUREMENTS, "a block's head on its own surface"). Test: view
    `a_heads_band_is_its_own_edge_and_a_rule_parts_heads_that_touch` (bands and rules read
    from the scene, rule painted over its band).
  - Superseded in part on 2026-09-28 (next entry): the band is `surfaces.band`, not `panel`,
    and a rule is drawn only between heads that touch.

- ✅ **The head band is seen, and the rule parts only heads** (2026-09-28,
  `.research/design-critique-round3-2026-09-28.md` §2 #2, #14). The band of the entry above
  did not land in light. `panel` on white is `F9F9F9` on `FFFFFF`, which most displays do not
  show, while the rule at `DFDFE0` is about eight times its contrast. The eye saw only the rule,
  under the output as before. In an unfocused tile the band was the header's own `panel`, so
  the tile read as two stacked headers.
  - **The band is `surfaces.band`** (`head_color`), wave A's step: 3.5 L* off the content in
    both variants and more than a unit off `panel`. The sticky header takes it too.
  - **A rule only where two heads touch.** After output, the band's top edge is the boundary,
    so the rule goes. It is drawn only where a prompt starts inside a head run, where the head
    above printed nothing and no band edge parts them. The terminal's opening prompt needs no
    rule of its own, because nothing precedes it. The sticky header loses its bottom rule for
    the same reason: it is a band over output. Warp's separators are lighter than its blocks'
    fill; here the fill does the work and the line comes only where fill cannot. A failed block
    keeps its bar and wash.
  - **The rules are a list from the head runs,** painted after the bands, not a field that
    every prepared row carries. Deciding them in the row loop cost the dense screen's
    screen-a-frame case about 100 µs with no prompt on screen, the codegen effect the 09-27
    entry saw. From their own pass both benches are unchanged within the load
    (MEASUREMENTS, "the head band on its own step").
  - **One duration format.** The caption and the sticky header's time are `kit::duration`
    ("6 s", "1m 5s"), so the terminal no longer writes "6.0 s" and "2 m 03 s" beside the
    conversation's "1m 5s". `took_label` survives only as a wrapper for the workspace's callers
    until they move.
  - **The find bar's caret is its focus,** as for every other field: its accent border went
    (`kit::the_accent_text_tone_is_never_a_fill` holds the terminal too).
  - Tests: view `a_heads_band_is_its_own_edge_and_a_rule_parts_heads_that_touch`,
    `cmd_up_and_down_walk_the_prompts_and_separators_follow` (a rule only under `false`, which
    printed nothing), `a_slow_commands_row_says_how_long_it_took` ("3.2 s").

- ✅ **A row the element built is kept while its line is** (2026-09-28, MEASUREMENTS "rows
  kept across frames, pixels in the texture's format"). The terminal element keeps each
  prepared row (cell backgrounds, strokes, sprites and the shaped words' places) keyed by the
  row's `Arc<Line>`, which the screen and the history share and never change in place; the
  cache holds the `Arc`, so the address cannot name another line while an entry lives. The key
  adds what restyles a row besides its cells: the frame's font, size, cell, palette and stroke
  geometry, the faint prompt's end, the local-echo guesses on it, the blink phase (only for a
  row with SGR 5) and the sprite tiles' device geometry (only for a row with sprites). The
  selection and search hits are per-frame marks painted over the kept backgrounds, so dragging
  a selection builds nothing. Only the rows drawn in the last frame are kept.
  - It pays where the 2026-09-06 ruling (ui.md, "the installed fonts are listed once per app")
    measured it would not: that ruling timed the zoom cycle, where the per-cell loop was 0.4 %
    of the main thread. On a dense 200 × 60 screen the loop is about half of an unchanged
    frame, and the cache takes the unchanged frame from 450–470 µs to 247–258 µs, a line a
    frame from 490–515 µs to 280–295 µs, a painted screen from 640–750 µs to 360–385 µs. A
    screen replaced whole every frame is unchanged within the load.
  - The prompt's faint copy and the guesses' copy of a row's cells (`faint_prompt`,
    `predicted_cells`) are now made only when the row is built, so an unchanged prompt row
    copies nothing.
  - The four faces are built once per family and ligature setting on the `ShapeCache`, not per
    terminal per frame.
  - The hovered block and the sticky header's visibility read `TermState::block_prompt` (the
    prompt row and whether a command was typed), not `block_head`, whose command `String` is
    now built only for the header that shows it.
  - Tests: view `a_frame_builds_only_the_rows_that_changed`, client
    `prompt_navigation_and_last_output_follow_the_marks` (`block_prompt` agrees with
    `block_head` on every row).

- ✅ **An image travels as the texture's format, as one byte string** (2026-09-28,
  MEASUREMENTS "rows kept across frames, pixels in the texture's format"). `TermEvent::Image`
  carries premultiplied BGRA (`bgra`), made on the worker's session actor as the event is built;
  the client keeps the `Vec` it decoded and the view copies it into the texture, with no
  conversion on the UI thread. The pixels and the cursor shape's `bgra` are
  `#[serde(with = "serde_bytes")]`: postcard writes a byte string as a length and the bytes,
  which is what a sequence of `u8`s was, so the goldens are byte-identical; the per-byte
  (de)serialiser calls are gone (a 12 MiB image 15–16.5 ms → 0.75–0.9 ms to encode and decode, and 1.6–1.8 ms → 0.4–0.5 ms
  for its texture on the UI thread).
  The field's meaning changed (RGBA → premultiplied BGRA), not its bytes on the wire.
  - Tests: worker `an_image_is_sent_as_premultiplied_bgra`, proto
    `a_byte_string_is_the_wire_a_sequence_of_bytes_was`, the `worker_term_image` and
    `worker_screen_cursor` goldens unchanged.

- ✅ **The worker sends a title once until it changes** (2026-09-28). A prompt that sets the
  title on every draw no longer broadcasts the same `TermEvent::Title` to every viewer each
  time. Test: session actor `a_title_is_sent_once_until_it_changes`.

- ✅ **A prompt the shell redraws is cleared before a resize** (2026-09-28). The `folder` golden
  showed zsh's prompt path twice after the column narrowed: the head of the old prompt above
  the redrawn one. Two layers lost ghostty's clear-on-resize (`Screen.clearPromptForRedraw`).
  - libghostty-vt's C API starts every terminal with `shell_redraws_prompt = false`
    (`c/terminal.zig`: embedders may not run ghostty's shell integration), so the clear
    happens only for a shell that opts in with `133;A;redraw=`. The integrations now do, as
    ghostty's rules have it: zsh `redraw=1` (zle redraws the whole prompt), bash
    `redraw=last` (readline redraws the last row only; `redraw=1` would lose the rows above),
    fish 3 `redraw=1` in its wrapped prompt, and fish 4, which prints its own `A` without the
    option, gets one `133;A;redraw=1` ahead of its first prompt.
  - Opted in, libghostty still cleared only the cursor's row. Its reflow copies a row's
    prompt mark onto every row the row wraps onto (`PageList` `copyRowMetadata`), and its
    clear starts at the nearest prompt start above the cursor, which after a narrowing is the
    cursor's own row. Upstream bug; vendor/ghostty is a pristine submodule, so the engine
    works around it: before a change of width, with `redraw=1` in force, the cursor in a
    prompt (the last mark an `A`) and the parser at ground (judged from the last escape of
    the bytes fed at the prompt), it clears the prompt at the old width from its first row
    down (`CUP` + `EL 2`), as libghostty's `promptIterator` finds it on the marks the shell
    printed, and puts the cursor at the start of its row. The blank prompt rows stay rows
    through the reflow, so zsh's count up lands on the prompt's first row: no stale copy and
    no blank row left above it. Delete `ghostty/redraw.rs` when ghostty's reflow marks the
    wrapped rows as continuations.
  - Tests: engine `a_prompt_the_shell_redraws_is_cleared_on_resize` (no option leaves the
    stale head; `redraw=1` redraws in place; a resize inside a split sequence writes
    nothing; bash's `redraw=last` keeps its first row), `ground_is_judged_from_the_last_escape_on`,
    `a_prompt_start_carries_what_the_shell_redraws`; the real zsh, bash and fish tests in
    `slopty-pty` assert the option each shell prints.
- ✅ **A terminal's errors are typed, and a person sees them** (2026-09-28, audit finding 18).
  - **`TermError` replaces the string in `TermEvent::Error`.** Its variants are `InputFull`,
    `NoSuchSession`, `Write`, `Engine` and `Stream`. `InputFull` is sent when a program stopped
    reading its input and the worker refused what was typed. Before, it was a string that the
    terminal view only logged, so the person typing never learned their keys were dropped. Now
    the workspace shows every `TermError` as a notice on the tile, in sentence case.
    `WorkerError::term_error` maps the worker's own errors onto it.
  - **The pty daemon's failures are `slopty_proto::ptyd::PtydError`.** Its variants are
    `SessionExists`, `NoSuchSession`, `AttachedElsewhere` and `Os`, so the worker can tell a gone
    session from one an older connection still holds. `slopty-ptyd` builds each refusal from
    it. Until `slopty-pty` carries the type in `PtydEvent::Error` and `PtyError::Daemon`, it
    still travels as its text.
  - **One `LineDiscipline` (`echo`, `canonical`) lives in `slopty_proto::terminal`**, for the
    engine and the pty crate to share. `MouseButton::bit()` is the one encoding of a held
    button.
  - Goldens: `worker_term_error_input_full` and `worker_term_error_engine`.
- ✅ **`ssh` out of a Slopty shell keeps a terminal the far side knows** (2026-09-29, product
  gap 6). A Slopty shell has `TERM=xterm-ghostty`, and a host without that entry left vim,
  less and htop with an unknown terminal.
  - **What ghostty does, and what we took.** Ghostty 1.4 wraps `ssh` in a CLI action
    (`ghostty +ssh`, `src/cli/ssh.zig`). It resolves the destination with `ssh -G`, pipes its
    entry to `tic -x -` on the host once, caches each success by `user@hostname` and entry
    version, and falls back to `xterm-256color` when the install fails. It also asks to
    `SendEnv` `COLORTERM` and `TERM_PROGRAM`. Slopty does the same in Rust
    (`slopty_pty::ssh`, run as `slopty ssh`). Each shell's hooks define an `ssh` function
    that calls the CLI, which ptyd names in `SLOPTY_CLI`: the `slopty` beside it, which every
    worker install carries. A shell without that variable keeps the plain `ssh`.
  - **Different from ghostty.** Ghostty sends no `TERM` for an `ssh` with a remote command,
    and still runs the install; here such an `ssh` passes through untouched, like `-N`, `-W`,
    `-O`, `-G`, `-V` and `-Q`. So `ssh host cat f | …` never prints or waits for a terminfo
    install. `-t` makes a command interactive again. The command line is cut the way OpenSSH
    reads it, with options after the destination too, so the install never runs the
    person's command. `TERM` goes in the `ssh` child's environment, which every OpenSSH
    sends, where ghostty uses `SetEnv=TERM`. The install runs through `sh -c`, so a fish or
    csh login shell on the host reads it the same way. The cache is `<data dir>/ssh-terminfo`,
    keyed by a digest of our entry's source, so a changed entry goes out again. A `TERM` that
    is not ours (inside tmux, say) is left alone.
  - **On by default, where ghostty's `ssh-env` and `ssh-terminfo` are opt-in.** Slopty's
    shells are a developer reaching into their own machines, and without the wrapper every
    such `ssh` is broken. The install writes only the user's `~/.terminfo`, and it says so on
    the first connection. `SLOPTY_NO_SSH_TERMINFO=1` never touches a host and gives every
    session `xterm-256color`. An `ssh` alias or function of the user's own wins, and
    `command ssh` is always the plain one. No setting was added: an environment variable
    covers the one choice there is.
  - **Cost.** The first login to a host is two connections (install, then the session). With
    password authentication that means typing the password twice, once per host and entry
    version. Keys, an agent, `ControlMaster` or Tailscale SSH hide it.
  - Tests: `slopty-pty` `ssh::tests` drive a fake `ssh` that records its calls. They cover
    the install then the cache, a failed `tic` falling back without caching, and what passes
    through untouched. They also cut the command line as OpenSSH does and read `ssh -G`.
    `a_typed_ssh_goes_through_the_cli` types `ssh` into real zsh, bash and fish and checks
    that a user's alias wins. `the_cli_travels_to_integrated_shells_only` covers the variable.
- ✅ **A program's progress (`OSC 9;4`) is session state on the wire** (2026-09-29, product gap
  4). cargo-style tools, dnf5, systemd and Claude Code's turn bar (`terminalProgressBarEnabled`)
  report progress with ConEmu's `OSC 9;4`. libghostty-vt parses it, and the binding fork
  already has `Terminal::on_progress_report`, so no fork change was needed.
  - **Shape.** `slopty_proto::terminal::Progress { state, percent }`, with `ProgressState`
    `None`, `Set`, `Error`, `Indeterminate` or `Paused`. It travels as
    `TermEvent::Progress` when it changes and is sent on attach while one stands, as the title
    is. It is not in `SessionSummary`: the tile and the Dock read an attached session, and
    adding a field there would touch every summary built in the tree.
  - **ConEmu's rules for a missing value.** A set with no value is zero. An error or a pause
    with none keeps the last value, as ConEmu and Windows Terminal do. Values over 100 are
    clamped by the parser. A report that changes nothing sends nothing.
  - **The prompt ends it.** Ghostty drops a bar nobody updated for 15 s, which cuts off a
    long step of a build that reports rarely. Here the next prompt (`133;A`) clears it: the
    shell has the terminal back, so the program that reported has ended, cleared or not. The
    program's exit clears it too. A shell without the integration keeps a stale bar until the
    next report, which is the cost of the rule.
  - **Not kept across a worker restart.** The checkpoint carries no progress, so a restarted
    worker shows none until the program reports again. Claude Code reports at the next turn.
  - Client: `TermState::progress()`, updated by `apply` with no `Effect` of its own, so the
    UI reads it after each apply. Tests: `progress_reports_are_events` and
    `claude_codes_turn_bar_and_a_prompt_end_progress` (engine),
    `progress_reaches_the_viewers_and_a_late_attach` (actor), `progress_and_restored_are_kept`
    (client). Goldens: `worker_term_progress`, `worker_term_progress_indeterminate`.
  - **The Dock shows one bar for all of them** (2026-09-29), the system's
    `NSProgressIndicator` under the app icon, as Finder shows a copy. The bar is the report
    furthest behind, so it fills only when every program is done. A failure is left to the
    tile's red bar and the finished-command note, because the Dock bar has no tone. The app
    keeps each terminal's last report and compares it on the terminal's redraw, so a redraw
    that did not change it costs one comparison. iOS has no equivalent short of a Live
    Activity. Test: `slopty_platform::dock` (`the_bar_is_the_report_furthest_behind`).
- ✅ **Sessions come back after a reboot** (2026-09-29, product gap 14). ptyd keeps shells alive
  across a worker restart, but a reboot or ptyd ending lost every shell. The items stayed,
  pointing at sessions that no longer existed.
  - **Prior art.** macOS Terminal.app reopens each window with a new shell in the old
    directory and prints the old contents above a "Restored session" line. Zellij serializes
    each session to its cache and holds the old commands behind "Press ENTER to run".
    tmux-resurrect restores panes and directories on request and reruns only an allowlist of
    programs. Warp and iTerm2 restore windows and their contents on launch, with new shells.
    Slopty takes Terminal.app's shape (a new shell, the old text above a divider) and
    Zellij's rule that nothing reruns unasked.
  - **The worker keeps them, not ptyd.** The gap note had ptyd write the recipe. The worker
    does it instead (`slopty_worker::restore::Keeper`): it makes the checkpoints, knows the
    OSC 7 directory and the title, and has `slopty_platform::fs::replace`. ptyd stays without
    VT parsing and without `slopty-platform`, and its protocol does not change. Per session,
    `<data dir>/sessions/<id>.json` holds the recipe: command, the request's own environment,
    directory, title, size, when the screen was saved, and whether the session was itself
    restored. `<id>.vt` holds the newest checkpoint. The directory is 0700 because a
    scrollback holds whatever was printed. The recipe is written at open. Each checkpoint
    updates the directory, title and size, and is written at most every 10 s. The worker
    writes what it holds on its way down, which covers a reboot (launchd stops it with
    SIGTERM) and ptyd ending (it exits after its ptyd). Closing a session deletes both files.
    Cost: MEASUREMENTS 2026-09-29, "Keeping a session's screen on disk".
  - **Reopened under the same id.** On start, `Worker::connect` adopts what ptyd holds, and
    `Worker::restore` reopens every kept session ptyd does not hold under its old
    `SessionId`, so every item keeps its tile with no item change. It spawns a new shell in
    the last directory (home when that directory is gone). The environment is the worker's
    current session environment plus the request's own. The engine replays the kept screen,
    then `GhosttyEngine::mark_restored` closes it off. That call switches off what the old
    programs left on (alternate screen, mouse and focus reports, bracketed paste, application
    keys, kitty keyboard flags, a hidden cursor, the program's colours). It then draws a faint
    "── Restored after restart ───" rule on the row below the last one with text, not at the
    cursor, because an inline TUI like Claude Code leaves its cursor above its status rows.
    The new shell's prompt follows. The next checkpoint folds the pair together, so a later
    worker restart does not draw a second divider.
  - **Nothing reruns.** The new session runs the login shell. The one exception is a session
    opened on a shell alone that `/etc/shells` lists (`/bin/zsh`), which gets that shell. A
    program is never started again. `TermEvent::Restored { saved_ms, command }` tells each
    viewer on attach that the session was restored and what it ran before (empty when that
    was the shell), so the UI can offer the command again. The chain survives a second loss.
    A Claude Code conversation left running is the other exception: it comes back resumed
    (claude-code.md, "An agent comes back after a reboot").
  - **Exited sessions come back as shells too.** An exited session stays until closed, and
    its record with it, so after a reboot it reopens as a live shell in its directory. The
    gap note's alternative, a read-only ended item with "Start a shell here", needs a session
    with no PTY, which the worker cannot hold. That alternative stays open.
  - Tests: `a_session_whose_shell_was_lost_comes_back_in_its_directory` (apps/slopty-worker
    e2e) starts the test's own ptyd and worker and opens `/bin/sh` in a temporary directory.
    It `cd`s deeper, reports OSC 7, prints a marker and starts a program that logs each start.
    Then it kills ptyd, starts both again and attaches. It checks that the session is listed
    under its id, that `Restored` arrives, that the marker is above the divider, that
    `pwd -P` in the new shell is the deeper directory, and that the program started once.
    `restore::tests` cover the files, the forgetting, the write pacing and the shell rule.
    `ghostty::restored::tests` cover the divider and the mode reset. Golden:
    `worker_term_restored`.
- ✅ **libghostty-vt at ghostty `12752b2ac`** (2026-09-29). `vendor/ghostty` moved 31 commits
  (from `6301810a4`). The libghostty-rs fork pins the same commit with regenerated bindings,
  and it is rebased on upstream `8953a74` (one CI commit). Cargo.lock pins the fork at
  `d1a57e4`. Of the 31 commits, these touch the VT library:
  - **OSC integers are parsed strictly** (`lib.parseInt`, #14417). `4_2` is no longer 42, and
    an unsigned field takes no sign, in OSC 4/5/104/105 colour indexes, kitty colours, OSC 9
    sleep and progress, OSC 3008 fields, OSC 66, OSC 99 and the OSC 133 `D` exit code.
    Nothing to adopt. The engine's own OSC 133 scanner already reads the exit status with
    `str::parse::<u8>`, which rejects `_`.
  - **Kitty `o=z` payloads are inflated by wuffs** (#14422), with the image's size as the
    first allocation, in place of `std.compress.flate`. It is a faster decoder in the same
    place, so there is no API to adopt. No engine test sent a compressed image before, so
    engine `a_zlib_compressed_transmission_arrives_inflated` now proves `o=z` through the
    engine.
  - **The mouse pointer shape an application asks for with OSC 22** is readable
    (`GHOSTTY_TERMINAL_DATA_MOUSE_SHAPE`, #14371). The fork wraps it as
    `Terminal::mouse_shape()` and `mouse::Shape` (fork test `mouse_shape_follows_osc_22`).
    Slopty does not carry it yet. The shape would travel in the frame (slopty-grid and
    slopty-proto) and be set by the terminal view, and none of those crates were part of
    this bump. Until then, OSC 22 is parsed and has no visible effect, as before.
  - **`ghostty_search_tick` refuses once its terminal is freed** (#14437). The engine's
    search is its own (`search.rs`), so nothing changes.
  - The rest is the macOS app, fonts, tmux control mode, nix and themes.
  - **The reflow bug is not fixed.** No commit touches `PageList`, `Screen` or the prompt
    marks, so a wrapped prompt row still carries its prompt mark onto the rows it wraps onto,
    and `ghostty/redraw.rs` stays. No issue draft about it was found under `.research/`, and
    upstream has no open issue or PR on it.
  - Cost: the engine's `*_cost` series held within 1 % (MEASUREMENTS 2026-09-29, "ghostty
    `12752b2ac`"); the budgets are unchanged.
- ✅ **A frame's rows are shared, not copied** (2026-09-29, audit finding 17). `RowUpdate.line`
  is an `Arc<Line>`. Serde writes the line itself (`rc`), so the wire is unchanged, and every
  golden passed without a new snapshot.
  - **Worker.** The engine keeps a record of the rows its viewers hold (`Shown`), and a
    frame's changed row went into it as a second copy of its cells. Both now hold one
    allocation. Once the frame is encoded and dropped, the record is that row's only holder.
    When the row changes again, the new line moves into the old allocation (`Arc::get_mut`),
    and the old cells become the next row's read. The record's list of rows is reused from
    frame to frame too. A row still held elsewhere, such as a joiner's frame in flight, gets
    a fresh allocation and nothing shared is written.
  - **Client.** `TermState` puts the decoded allocation on the screen and in the scrollback
    as it came. `Screen::apply_shared` is gone because `Screen::apply` now takes the shared
    line.
  - Cost: an echo's `take_frame` fell from 4 blocks to 1 and from 9.5 KB (80×24) or 23 KB
    (200×60) to 128 B. An Enter at the bottom fell from 10 blocks to 8. Echo fan-out fell
    from 6 blocks to 3. Applying a row on the client fell from 1 block to 0. `take_frame`
    fell 10 % in instructions. MEASUREMENTS 2026-09-29, "Rows shared between the frame and
    the record". The budgets in `slopty-engine/tests/allocs.rs`, `slopty-grid/tests/allocs.rs`
    and `xtask/budgets.toml` are tightened to match.
- ✅ **The program's pointer shape (`OSC 22`) is session state on the wire** (2026-09-29).
  Programs ask for a pointer over their UI with `OSC 22` (a hand over a clickable span, a
  splitter's resize arrows). libghostty-vt tracks it, and the binding fork reads it as
  `Terminal::mouse_shape()`.
  - **An event, not a frame field.** The ghostty bump's note had the shape travelling in the
    frame. It travels as `TermEvent::Pointer(PointerShape)` instead, like the colours and the
    progress. The event is sent when the shape changes and on attach while it is not the
    I-beam. `OSC 22` dirties no row, so a frame field would need a frame built for it, and it
    would add a byte to every frame for a value that changes a few times a session.
  - **Read after a write that could change it.** libghostty has no callback for it. The engine
    reads it where it already checks the colours: after a write carrying an OSC or a reset,
    and once more after that, for a sequence split across two writes. That is one extra FFI
    read on those writes and none on plain output.
  - **The wire carries every W3C name** (34 of them), so the worker never decides what a
    client can draw. `PointerShape::Text` is the default because the terminal starts there.
    A shape a newer libghostty adds reads as `Default` until it is named.
  - **The view picks the pointer in this order.** A hand over the link ⌘ would open comes
    first. Then the I-beam while ⇧ takes the mouse back from a program. Then the program's
    shape, if it is not the I-beam. Then the arrow while a program reports the mouse, and the
    I-beam otherwise. Shapes GPUI has no match for (`help`, `progress`, `wait`, zoom) draw the
    arrow.
  - Tests: `a_pointer_shape_is_reported_when_it_changes` (engine),
    `the_programs_pointer_shape_is_kept` (client),
    `the_programs_pointer_shape_is_the_pointer_over_the_grid` (view). Goldens:
    `worker_term_pointer`, `worker_term_pointer_zoom_out`.
- ✅ **The terminal draws its program's progress and the restored chip itself** (2026-09-29).
  - **Progress.** A bar `spacing.xxs` tall runs along the terminal view's top edge, over the
    grid's first pixels, rather than in the tile header, which the streaming work owns. It
    takes the state's tone: the accent while it runs, `warn` paused, `error` failed. A figure
    sets the share filled, and a report without one fills the edge. An indeterminate report
    sweeps a segment across at the working mark's pace (`SPIN_STEP`, twelve steps a second).
    It steps rather than glides. Under Reduce Motion it stands still over the whole edge,
    set back to `alpha::STRONG`.
  - **The sweep never delays an echo.** Its timer lives in the view, so a step redraws the
    terminal (a child entity would not help, because GPUI marks every ancestor of a notified
    view dirty). The step is computed from the clock at render time. A step that falls due
    while a typed key waits for its echo wakes nothing, and the echo's frame draws the sweep
    where it has got to. A view that was not drawn since the last step (scrolled off, or
    under the face) is not woken until it is drawn again. This mirrors the working mark's
    `hold_steps`. The mark's clock is private to `icons.rs`, so the bar keeps its own.
  - **Restored.** A session reopened after its shell was lost shows a chip at the terminal's
    top right. The chip says "Restored" and, when the session ran a command, offers "Run
    again: <command>" with each word shell-quoted. The command is typed and run only when the
    person clicks. The chip goes once it is used or dismissed. A login shell has nothing to
    run, so its chip only says "Restored".
  - Not done here: a Dock badge for progress. It needs the app's Dock tile and the workspace,
    which other work owns.
  - Tests: `progress::tests` (tones, shares, the sweep, Reduce Motion),
    `a_progress_report_fills_its_share_of_the_top_edge`,
    `an_indeterminate_report_sweeps_unless_motion_is_reduced`,
    `a_restored_session_runs_its_command_again_only_on_a_click` and
    `a_restored_login_shell_has_nothing_to_run_and_is_dismissed`.
- ✅ **Decoded lines are bounded** (2026-09-29).
  - **The bug.** A line leaves its trailing blanks off the wire and gets them back on decode,
    padded to the width the peer claims. A blank line was 7 bytes at any width. 100 of them
    claiming 65 535 columns (703 bytes of `TermEvent::Lines`) decoded into 314 MB. One 16 MB
    frame of them asked for terabytes. Seven bytes of `Resized` or a frame's `cols` × `rows`
    made the client build a 65 535 × 65 535 screen. `orchestration::Line` is text, bounded by
    its bytes, and was never affected.
  - **A size ceiling.** `slopty_grid::MAX_COLS` (2048) and `MAX_ROWS` (1024). A full-screen
    window at the smallest font is about 630 × 350 on a 6K display, so this leaves room for a
    window across several. A line wider than `MAX_COLS` does not decode, and neither do a
    `Frame` or a `Resized` past the ceiling. A `TermSize` decodes clamped to it, so a window
    wider than any terminal gets the widest, and the PTY and the engine get the same size.
    The engine refuses a size past it (`check_size`). A client's `Screen` clamps as a
    backstop.
  - **A cell budget per message.** The ceiling alone left 2.8 M blank lines per 16 MB frame,
    each 2048 cells. `codec::decode_body` now runs every decode under
    `slopty_grid::with_cell_budget(MAX_DECODED_CELLS)`. Serde gives a `Deserialize` no
    context, so a thread-local carries the budget down. Each decoded line takes its width
    from it, and the line that would overdraw it fails, which fails the message.
    `MAX_DECODED_CELLS` is `MAX_FETCH_LINES` × `MAX_COLS` (8 Mi cells, 384 MiB). That is a
    full answer to `FetchLines` at the widest, the largest message an honest worker sends, so
    no honest message meets it. Outside a decode nothing is counted, so the worker's own
    files and the tests are not limited.
  - **Why not a budget per byte.** An honest blank line is already the worst ratio: its bytes
    say nothing about its width. Any per-byte factor that lets an honest 2048-column scrollback
    through lets an attacker's copy of it through too. Only a ceiling on the message's cells
    bounds it.
  - **Rejected: not putting the blanks back at decode.** Padding at placement would bound
    decoded memory by the bytes. But a `Line` is its `cells`, and every reader (renderer,
    selection, search, the scrollback cache) counts on `cols` of them, so it would change the
    type across the client and the UI. The wire and the goldens are unchanged here.
  - **Cost.** One thread-local read and write per decoded line, and two per message.
    MEASUREMENTS, "Decoding a frame".
  - **The fuzz targets now bound the heap too.** A decoding target (the four streams and
    `term_datagram`) runs under `slopty_testkit::alloc::Counting`. It fails when a decode
    takes more than 64 bytes of heap per body byte, plus 48 per cell its lines took, plus
    2 MiB (serde preallocates a sequence up to 1 MiB before reading it). With the fix undone,
    the 703-byte input kept under `fuzz/regressions/{uni_stream,term_datagram}/` fails
    with 314 573 600 heap bytes.
  - Tests: `decode_bounds.rs` in slopty-proto covers the 703-byte input, a 16 MB frame of
    blank 2048-column lines (it stops at exactly `MAX_DECODED_CELLS`), frames and `Resized`
    past the ceiling, and `TermSize` clamped. In slopty-grid:
    `decoded_lines_stay_within_the_cell_budget`,
    `a_line_wider_than_the_ceiling_does_not_decode` and `a_screen_is_never_past_the_ceiling`.
    In the engine: `the_size_ceiling_is_the_largest_terminal`.

- ✅ **A progress percent moves the summary at most four times a second** (2026-09-29). Every
  `OSC 9;4` change said the session's summary moved, and each move is a summary read on the
  session's actor and a `SessionChanged` to every client and the server, so a build reporting
  each file flooded them. A report that starts, ends or changes kind (an error, a pause) still
  says so at once; a percent says so at most every 250 ms (`session::PROGRESS_EVERY`), the
  latest held back until its time, so the last value always goes out. The daemon also takes the
  moves queued while it works in one batch, each session once. Ten thousand reports over 1.5 s
  moved the summary 7 times (`a_flood_of_progress_moves_the_summary_a_bounded_number_of_times`).

- ✅ **The prompt-mark scanner reads OSCs as libghostty does** (2026-09-30; superseded the same
  day: the scanner is gone, see "Prompt marks come from libghostty's semantic prompt effect"). ghostty `0538f7535`
  (fork `29bbc6a`) makes CAN and SUB cancel an OSC in progress, as xterm does: `ESC ] 2 ; t CAN`
  no longer sets the title. The engine's OSC 133 scanner still counted a mark cut off that way,
  so a cancelled `133;D;1;` gave the next prompt a status the terminal never took. Checked
  against `parse_table.zig` and the stream's OSC fast path, the scanner also differed in three
  older ways, now fixed:
  - An OSC ends on ESC whatever follows it, and that ESC starts the next sequence. The scanner
    dropped an OSC whose ESC was not followed by `\`, where libghostty acts on it. The mark is
    now reported at the ESC, and the `\` is fed after it.
  - The other C0 controls are dropped inside an OSC, not collected into its payload. Inside an
    escape they run without ending it, so `ESC ENQ ] 133;A BEL` is a mark.
  - A payload may run to 128 bytes, up from 32. ghostty's own bash integration writes
    `133;A;redraw=last;cl=line;aid=<pid>`, which is over 32 bytes, so that prompt start was
    lost.

  An OSC that is not a mark now drops the scanner back to waiting for the next ESC, since only
  an ESC can start a mark. That removes the skip state and its `memchr2`, and the scan costs
  113 instructions per OSC, down from 171 (MEASUREMENTS, "ghostty `0538f7535`").
  - **Rejected: libghostty's new unknown-sequence callback for OSC** (#14452). It reports only
    the OSC numbers libghostty does not implement. 133 is one it implements, so the marks
    never reach the callback. No other OSC Slopty reads goes through a scanner of its own:
    title, working directory, notifications, progress, clipboard and pointer shape each have
    an effect callback or a getter. The Rust wrapper has no binding for the callback, and
    none was added.
  - Tests: `the_mark_scanner_frames_an_osc_as_libghostty_does` feeds an OSC 2 title and a
    mark with the same framing (BEL, ST, ESC `[`, a C0 control after the ESC, CAN, SUB, and
    an escape cancelled before `]`), and asserts that libghostty set the title exactly when
    the scanner reported the mark. `a_cancelled_command_end_leaves_no_status` and
    `an_osc_cancelled_by_can_or_sub_has_no_effect` cover the title, directory, notification,
    progress and OSC 52 events, and a cancel split across reads. The scanner's own tests are
    `can_and_sub_cancel_a_mark`, `any_escape_ends_the_mark_and_starts_the_next_sequence` and
    `other_controls_do_not_end_a_mark`. The first two engine tests fail with the old scanner.
  - The same bump clears the kitty placeholder flag when a whole row is erased (#14449). Frame
    building reads that flag before it compares each cell with U+10EEEE, so an erased row is
    no longer walked for placeholders. Output is unchanged, since the walk found nothing
    there. The shared render device state (#14446) is in ghostty's renderer, which Slopty
    does not build.

- ✅ **ghostty comes from aislopware/ghostty, which carries the open PRs Slopty needs**
  (2026-09-30). Slopty does not wait for upstream to merge a ghostty change it needs. It
  finishes the change in a fork and reconciles later. `vendor/ghostty` now tracks
  `aislopware/ghostty` (`.gitmodules`), whose `main` is ghostty `main` with our commits on top,
  as in the other forks. In the checkout, `origin` is ghostty-org and `fork` is aislopware. The
  libghostty-rs fork fetches the same repository at `GHOSTTY_COMMIT`
  (`GHOSTTY_REPO` in `libghostty-vt-sys/build.rs`). To bump it, rebase `main` onto ghostty
  `main`, push it to `fork`, and follow the steps in "Dev loop". Four PRs were weighed:
  - **Taken: #14362, the alternate screen modes from the terminal's state** (korikhin; four
    commits, rebased onto `0538f7535`, plus one of ours; it supersedes #14200). Before it, a
    program that entered with `?47h` and left with `?1049l` left mode 47's bit set on the
    primary screen. DECRQM then reported it set, and libghostty's formatter wrote `?47h` into
    a checkpoint, so a restored session came back on the alternate screen with the primary's
    rows drawn over it. With the PR, the modes 47, 1047 and 1049 answer from the active
    screen, 1048 answers from whether a cursor is saved, and XTSAVE/XTRESTORE follow xterm.
    `?1049r` no longer erases. The upstream review asked for a match with xterm, and the PR
    shows one against xterm 411. Our commit `7d0734aa8` resolves the conflict with the render
    hold, replaces the PR's review notes with comments, and adds `Terminal.modeGet`. That was
    needed because the PR left the C API's mode get reading the bits, which are now always
    false. It has a Zig test (`get mode answers the alternate screen modes from the active
    screen`). The engine test is `leaving_the_alternate_screen_by_another_mode_leaves_it_everywhere`,
    which covers the checkpoint and DECRQM and failed before the change. The one difference
    from xterm left in place: xterm reports 1048 set from startup because it saves a cursor
    for each screen at startup, while ghostty reports it reset until a cursor is saved.
  - **Not taken: #14167, colon subparameters on any CSI.** ghostty already keeps colons on
    SGR, which is the only place Slopty needs them. The PR opens them to every final for
    kitty's multiple-cursor protocol, which xterm does not have and Slopty does not use. It
    reworks the CSI parameter path, which is hot, and the maintainers still want benchmarks.
    `colon_underline_styles_and_colours_reach_the_cells` shows `4:3`, `4:4`, `4:5`, `21`,
    `58:2::r:g:b`, `58:5:n` and `58;2;r;g;b` reaching the cells the clients paint.
  - **Not taken: #14133, the PNG hook reporting extra allocated bytes.** libghostty takes
    ownership of the decoded buffer and frees it, so a decoder cannot reuse it, and rounding
    the size up gains nothing. Doing the decode right removed the reason for the PR. The
    decoder now allocates the RGBA buffer libghostty will keep, decodes into its front, and
    widens RGB, gray and gray-alpha to RGBA in place from the back. Before, it made a
    decode `Vec`, an RGBA `Vec` and a copy into libghostty's buffer. That is one allocation
    of the image where there were three, and 17 % less time for RGBA, 6 % for RGB
    (MEASUREMENTS, "PNG decode in place"). `every_png_colour_type_decodes_to_rgba` checks
    each 8-bit colour type against `to_rgba`.
  - The libghostty-rs `Bytes` exposed the fresh allocation as `[u8]` while it was
    uninitialized. It is now zeroed first (fork `c18747b`), at a cost of about 1.5 % of the
    RGBA decode.

- ✅ **A shell's browser and editor are the client's** (2026-09-30, revised the same day after
  an adversarial review; wire change). A program in a worker's shell that opens a web page
  (`gh auth login`, Claude Code's `/login`, `cargo doc --open`, `open https://…`) opened it on the
  worker's screen, which nobody in front of the client can see. A program that runs `$EDITOR`
  (`git commit`, `git rebase -i`, `crontab -e`, Claude Code's Ctrl+G) got whatever editor the
  shell had, in the terminal.
  - **What a session gets.** ptyd's shell integration links the `slopty` CLI under three names:
    `open` (`xdg-open` on Linux), `slopty-browser` and `slopty-editor`. The CLI tells them apart
    by `argv[0]`. Every session gets the links' directory first on `PATH` (`SLOPTY_BIN`), and
    `BROWSER` and `EDITOR` naming the two commands by absolute path. The shell scripts put the
    directory back in front at every prompt in zsh, bash and fish, since macOS's `path_helper`
    in `/etc/zprofile` moves the system directories ahead of anything inherited.
  - **Defaults only, and never `VISUAL`.** git, `crontab`, `less`, `sudoedit`, zsh's
    `edit-command-line` and Claude Code all read `VISUAL` ahead of `EDITOR`. Setting `VISUAL`
    would beat an `EDITOR=nvim` that the user's rc exports, so it is not set. An `EDITOR` or
    `VISUAL` that the rc exports wins by itself, and so do git's `GIT_EDITOR` and `core.editor`.
    Nothing is set when the daemon's own environment already names a browser or an editor. One
    of Slopty's own commands inherited that way (a daemon started inside a Slopty shell) counts
    as none and is removed. `SLOPTY_NO_SHELL_INTEGRATION` turns all of it off.
  - **One word each, by absolute path.** Claude Code runs `BROWSER` as one executable with the
    URL as its only argument (`spawn(BROWSER || "open", [url])`). gh shlex-splits it. Python's
    `webbrowser` reads it as a list of commands. git hands `EDITOR` to `sh -c`. An absolute
    path with no space reads the same to all of them, and a program run with its own `PATH`
    (`env PATH=/usr/bin:/bin git commit`) still finds it. The scripts' directory on macOS is
    under Application Support, which has a space, so the commands then live in
    `$TMPDIR/slopty-<uid>/bin`. That directory is made 0700 and is refused if it is a link, has
    another owner, or can be reached by the group or others (`shell_integration::bin_dir`).
    When no such place can be made, the commands go by bare name.
  - **Only clients that say so are asked.** A client declares what it takes with
    `ClientMsg::HandoffCaps { open, edit }` right after its hello, and again when that changes.
    It is a message of its own rather than a hello field for two reasons. The control stream
    is ordered, so it arrives ahead of anything the worker could hand over. And what a client
    takes changes while it is connected, for example when the app's last window closes.
    - The worker asks only clients that declared the kind of handoff, in this order:
      1. clients focused on the session's tile, the latest first;
      2. the client that last typed into it;
      3. clients focused on something else;
      4. the rest, by recency.
    - With no client connected, or none that takes it, the program is answered at once with
      `Handed::Nobody { why }`. `why` is `none_connected`, `none_capable`, `refused` or
      `timed_out`, and the CLI says it on stderr.
    - Each asked client has `TAKE_WAIT` = 3 s. When it runs out, the client is sent
      `Withdrawn` and the next is asked. Replies carry their `ClientId`. A take that comes in
      late from an earlier client still counts, since its page is already open, and then the
      later one is withdrawn instead.
    - Handoff ids start from the wall clock in microseconds each run, so a restarted worker
      never reuses one that a client still holds.
    - Focus is the terminal's own focus report (`TermRequest::Focus`, DEC 1004). The session
      actor folds its viewers' reports into one, and a reconnecting client starts focused on
      nothing.
  - **A page opens only on a person's say; otherwise it is offered.** Any process running as
    the user can reach the control socket, and so can any program in any session, so a
    worker must not be able to drive the person's browser. A page opens without asking only
    when all of these hold:
    - the client asked typed into that session within `TYPED_RECENTLY` = 8 s. The Enter that
      starts a login comes a moment before its page, and a login that starts a runtime first
      (`az`, `gcloud`) takes a few seconds more;
    - the address is not one to be wary of;
    - fewer than 3 pages opened that way in the last 10 s.

    Otherwise the page goes out with `offer: Some(reason)`. The client shows a notice naming
    the host, answers `Offered { why }`, and the CLI prints where it was offered.
    - Addresses to be wary of (`handoff::Wary`) are always offered, whoever typed:
      - a name or password before the host (userinfo);
      - an internationalised name, shown in its `xn--` form;
      - loopback (`localhost`, `127/8`, `::1`, `0.0.0.0`). On the client that is the client's
        own machine;
      - private ranges: `10/8`, `172.16/12`, `192.168/16`, `100.64/10`, `fc00::/7` and
        `.ts.net`;
      - link-local: `169.254/16` and `fe80::/10`;
      - local names: a single label, `.local`, `.lan`, `.home.arpa` and `.internal`.
    - Both ends parse the address with the `url` crate (WHATWG, as a browser does), and what
      is opened is that parse's serialisation. So `https://evil.com\@github.com` has host
      `evil.com`, and it opens as `https://evil.com/@github.com`, so the host shown is the
      host visited. Anything but `http`/`https` with a host, over 8 KiB, or holding whitespace
      or a control character (which a browser would silently drop) is refused. The client
      also offers rather than opens a page it acts on more than `LATE_AFTER` = 2 s after its
      link read it, since by then the worker may be about to withdraw it. That wait is
      measured on the client's own clock: the link stamps each handoff with an `Instant` as it
      reads it (`LinkEvent::Handoff { received }`), and the worker's `asked_ms` plays no part.
      An earlier version compared `asked_ms` with the client's wall clock, so a worker whose
      clock ran two seconds behind had every page offered, and one running ahead had a page
      held up here opened anyway. The notice's "asked … ago" counts from the same stamp. Test:
      `slopty-client` `handoff::tests::lateness_is_measured_on_this_clients_clock`.
  - **The editor waits for the tile, and the save lands where the program reads it.**
    `slopty edit --wait [+line] <file>` sends `CtlRequest::Edit` and keeps its socket open. A
    client that takes it shows the file beside the session's tile. The worker answers when the
    person is done: `Edited { Done }` exits 0, and `Edited { Cancelled }` exits 1, which makes
    `git commit` stop.
    - A file a waiting edit shows is saved **in place**: written over from the start, cut to
      length and fsynced, keeping its inode, mode, owner and xattrs (`file::Rewrite::InPlace`).
      BSD `crontab -e` and `visudo` read the edited file back through the descriptor they
      opened before the editor ran. The usual temp-and-rename save left that descriptor on the
      old contents, and the edit was silently lost. Other saves still replace atomically.
    - The client's `Edited` goes through the connection's save queue behind its `WriteFile`s.
      That queue now outlives the connection, so an edit ended just as the client drops still
      lets the program go.
    - A client that drops has `LOST_AFTER` = 5 min to come back, and is asked again under the
      same id. After that the CLI hears `Lost` (exit 1). Closing the CLI withdraws the edit.
      An edit that ends while its client is away is withdrawn when the client returns.
    - A waiting program is never left hanging by its tile going. Closing the tile answers
      as "Done" would: save, then tell. When that save is refused, fails or loses its link,
      the program hears `Cancelled` once ⌘Z's window has passed, and the edit stays kept on
      the client (`docs/decisions/ui.md`, "An unsaved edit survives a quit or a crash").
      While the save is still out, the window is extended until the answer arrives, so the
      program hears how it ended. A tile removed by another client, or lost with its worker,
      answers `Cancelled` too. Before this, such a tile went silently. Its client was still
      connected, so `LOST_AFTER` never ran, and `git commit` waited with no end. A wait whose
      tile is not made yet is looked up by path from the client's `Handoffs` when the tile
      appears. The earlier map keyed by tile leaked an entry whenever the tile never came.
      Tests (`slopty-ui` `workspace::tests::handoffs`):
      `a_waiting_tile_closed_with_its_save_refused_gives_up_and_keeps_the_edit`,
      `a_waiting_tile_removed_elsewhere_gives_up`,
      `an_edit_withdrawn_before_its_tile_is_made_leaves_no_wait`.
  - **Fallbacks run here, at once.** With no worker, or `Nobody`, a page goes to the system's
    opener: the first `open` on `PATH` that is not ours, else `/usr/bin/open`. A file goes to
    `vi` with the same arguments, the editor git itself falls back to. Both are exec'd, so
    their exit status is the program's. A page opened on a client is never also opened here:
    when the worker stops answering partway through a list of URLs, only the ones not yet
    handed over fall back.
  - Tests:
    - `slopty-proto`:
      - `handoff::tests::only_web_addresses_with_a_host_are_openable`.
      - `the_host_is_the_one_a_browser_visits_and_tricky_ones_are_marked`: backslash and
        userinfo tricks, IDN, hex, octal and decimal IPv4, mapped IPv6, the private and local
        classes.
      - Goldens: `worker_handoff_*`, `client_handoff_{taken,offered,refused,edited,caps}`,
        `ctl_request_{open,edit,wake}`, `ctl_reply_handoff` (every `Handed` and `NoClient`)
        and `ctl_reply_wake`.
    - `slopty-worker` `handoff::tests`:
      - routing order;
      - `only_a_client_that_takes_it_is_asked`;
      - `a_page_opens_only_right_after_its_client_typed` (typing window, wary addresses,
        burst limit);
      - `a_late_answer_counts_and_the_others_are_withdrawn`;
      - the re-ask after a reconnect;
      - `an_edit_ended_while_its_client_was_away_is_withdrawn_on_return`;
      - `a_new_run_numbers_past_the_last`;
      - presence, rejoin and `a_gone_session_is_forgotten`.
    - `slopty-worker` `file::tests::a_save_in_place_reaches_a_descriptor_held_open`. It also
      shows that a replacing save does not reach a held descriptor.
    - `slopty-pty`:
      - `every_session_gets_the_handoff_commands`;
      - `the_handoff_commands_live_where_no_space_splits_them` (including a planted directory
        and a link);
      - `the_handoff_commands_come_first_and_yield_to_the_users_editor`: zsh, bash and fish,
        with no rc, an rc exporting `EDITOR=nvim`, and one exporting `VISUAL=nvim`.
    - `slopty-client` `handoff::tests`.
    - `slopty-cli` `tests/handoff.rs`: `open_steps_aside_for_what_is_not_a_web_page`,
      `with_no_worker_the_systems_own_take_over` and `the_systems_exit_status_comes_through`.
    - `slopty-workerd` `tests/handoff.rs` (real ptyd, worker and clients):
      - `a_page_opens_on_the_client_that_just_typed`
      - `a_page_nobody_typed_for_is_offered`
      - `a_silent_or_refusing_client_is_passed_over`
      - `with_no_client_to_take_it_a_page_opens_here_at_once`
      - `an_editor_waits_for_the_tile_and_the_program_reads_the_save`: a `sh` that holds the
        file open like `crontab` reads the save through its descriptor and by name.
      - `an_edit_ended_as_the_client_leaves_still_lets_the_program_go`: failed 3 of 3 runs
        with the old save task.
      - `an_editor_given_up_withdraws_the_edit`
      - `an_editor_with_no_client_to_show_it_is_vi_here`
  - **The app's side** is in ui.md, "What a shell hands over shows beside it, and a page not
    asked for waits for a yes": the declaration on every link, the page opened or held back in
    a notice, the file tile beside its shell with Done and Give up, and the focus reports.

- ✅ **The libghostty-rs PR stack is taken whole, our commits on top; frames read a row's cells
  at once** (2026-09-30). Upstream opened twelve stacked PRs (#84–#95, on #83, #98 and #99). A
  stack is taken or left from the bottom up, and none of them was worth breaking it for, so
  the fork's `master` is now upstream `stack/ghostty-examples` with our commits rebased onto
  it (fork `fe69e05`, then `f050a4c` for the ghostty pin below). Four of ours were already
  upstream and are dropped: the zeroed `Bytes` (#83), the render hold callback and the OSC 22
  pointer shape (#99), and the OSC parser's NULL command and C-string title (#93). The old head
  `6c2bf45` stays reachable as `archive/master-2026-09-30-pre-stack`, since the staged
  Cargo.lock names it. What each PR is to Slopty:
  - **#84, ghostty `f9e8270`: taken, pin kept on our fork.** `f9e8270` is `0538f7535` plus a
    Windows DLL constructor, so the generated bindings are byte for byte ours. `GHOSTTY_REPO`
    stays `aislopware/ghostty`.
  - **#85, bulk render state: taken, and it is the win.** `build_frame` took every field of
    every cell of a dirty row through the cell iterator, one call into libghostty each. It
    now takes the row's raw cells in one read (`cells_raw`) and positions the iterator only
    for a style or a cluster; the cursor comes in one call (`Snapshot::cursor`). Row ids,
    overscan, `next_dirty` and `clean` are not used: the engine never scrolls the viewport,
    every row is visited anyway to keep the viewers' record, and the per-row `set_dirty` it
    already does is what `clean` would do.
  - **Finished ourselves: the packed cell decoded in Rust** (fork `32963c5`, `ce05b45`,
    `5ed65f3`, `fe69e05`). ghostty's `screen.h` documents the cell as a packed `u64` whose
    layout `ghostty_type_json()` describes for the linked build, and supports decoding it
    from there instead of `ghostty_cell_get` per field. `screen::CellLayout` reads the
    positions from the manifest once (a small JSON reader in the fork, no dependency) and
    refuses one it cannot read, `Cell::fields` falls back to the getters then, and debug
    builds check every decoded cell against the getters, so every engine test is also a
    layout test. Together with #85: a scroll frame −44 %, a keystroke's frame −38 %, a 4096-row
    history fetch −17 % (MEASUREMENTS, "frames read a row's cells at once").
  - **#91, streaming formatter: taken.** `plain_rows` (search's copy of the history, the text
    reads) formats into its own `Vec` and skips the copy out of libghostty's buffer; the
    "blank rows format to nothing, reported as out of memory" workaround goes with it. Search
    formatting −10 %. The VT checkpoint still formats into a buffer, since it edits the
    output in the middle (margins, row padding) before it is sent.
  - **#87, ground state and protocol settings: taken.** The prompt clear before a reflow asked a
    byte heuristic whether the stream was at ground (`redraw::ends_at_ground`, wrong towards
    "open", and wrong for a sequence cancelled by CAN or SUB); it asks libghostty's parser now
    (`vt_ground`), and the heuristic is deleted. The worker's checkpoint pacing asks the same
    (`GhosttyEngine::at_ground`) in place of `boundary::Boundary`, a byte-level guess, now
    deleted, that also started from ground in a new worker whatever the replayed backlog
    ended in. A kitty
    clipboard write (OSC 5522) is capped at `MAX_OSC52_BYTES` while it is still arriving
    (`set_clipboard_write_max_bytes`); libghostty buffered up to 64 MiB of one before, only
    for the session to drop it. `set_terminfo_name` answers XTGETTCAP `TN` with the `TERM`
    the shell really has. `Pty::spawn_with` decides it once (the spec's own `TERM`, else
    `default_term()`) and returns it with the child. ptyd keeps it per session and hands it
    over in `PtydEvent::Attached` beside `started_ms`, and the worker sets it from
    `SessionStart::term`, never recomputing. A worker cannot know what an older shell was
    told: `default_term()` changes once the terminfo is installed, and a spec may set its own.
    A `TERM` longer than `MAX_TERM_BYTES` (255, `NAME_MAX`, since a terminfo entry is a file
    named after its terminal) is refused at spawn, which keeps `Attached` inside its envelope.
    Tests: `the_term_reported_is_the_one_the_child_sees` (slopty-pty),
    `attach_names_the_term_the_shell_was_given` (ptyd), and
    `an_adopted_shell_is_answered_for_the_term_ptyd_gave_it` (worker against a real ptyd, a
    shell spawned as `xterm-256color` whichever name this host would give). `vt_write_until_ground` and
    `cursor_at_prompt` have no user: the engine writes its own sequences only at a prompt, and
    its prompt state comes from its mark scanner.
  - **#86, synchronous clipboard reads: taken in the binding, not answered.** "OSC 52: write
    only" stands. The callback runs on the session's thread with the VT stream stopped until
    it replies, and the clipboard that would answer is on a client a network away behind a
    prompt. Without the callback libghostty answers nothing and reports mode 5522 as
    unrecognised, so kitty paste events stay off and programs fall back to bracketed paste.
  - **#89, `Terminal::paste`: taken in the binding, not used.** It chooses between a kitty paste
    event and bracketed text from the terminal's modes; without #86 it is `paste::encode` with
    its output routed through the pty-write callback, so `encode_paste` keeps `encode`. Its
    injection check is the worker's, against the modes as they are when the paste arrives
    (`Engine::paste_is_safe`; see "A paste is judged by the mode when it arrives" in ui.md).
  - **#88, search wrapper: taken in the binding, not used.** The 2026-09-12 ruling on native
    search holds: its matching still folds ASCII case only.
  - **#92, unsupported-sequence callbacks: taken in the binding, still rejected** for the
    reasons in "The prompt-mark scanner reads OSCs as libghostty does".
  - **#90, decoded snapshot continuation: taken, and the binary snapshot measured.** A
    snapshot keeps both screens and an unfinished sequence, which the VT checkpoint cannot, and
    writes 3.3 times faster (580 µs against 1.9 ms for 10 000 lines). It is also three times
    the bytes (2.07 MB against 0.69 MB) held in ptyd and sent at every checkpoint, and restores
    slower (2.6 ms against 2.3 ms). ⏸ Revisit when the checkpoint's own workarounds (primary
    screen kept for the alternate one, mode resets, row padding) cost more than the bytes, or
    the format shrinks; restore lives in the worker and ptyd.
  - **#93 OSC parser commands, #94 secure random source, #95 examples workspace: taken, no
    use.** The engine does not run libghostty's standalone OSC parser, and macOS has a random
    source.
  - **Follow-up the same day: a scrolled row that reads as its line is kept.** Every line of
    output at the bottom moves the viewport's pin, and libghostty then marks all rows dirty and
    clears the page's row dirty bits, so row ids (#85) and the dirty flag cannot tell a row that
    moved from one that changed. The engine keeps a print of each held line instead: the raw
    cells and the wrap and prompt flags it was built from. A dirty row equal to its print keeps
    the held line without being built or compared; its styles are resolved again first, since
    ghostty frees a style id when no cell uses it and a new style can take it while a cell's
    bits come back the same (`a_scroll_ships_what_changed_and_keeps_what_did_not` fails
    without that check). Rows with links, clusters, placeholders or a forced mark are always
    built. A scroll frame is −75 % (80×24) and −88 % (200×60), a keystroke's +3 %.
  Tests: fork `decoded_cells_match_the_getters` (every content tag, width, semantic content,
  a style, a link and a protected cell),
  `a_manifest_that_does_not_describe_the_cell_is_refused`, the manifest reader's own; engine
  `ground_is_where_the_parser_stands`, the CAN case in
  `a_prompt_the_shell_redraws_is_cleared_on_resize` (fails with the byte heuristic),
  `a_kitty_clipboard_write_is_bounded_by_the_osc52_ceiling` (EFBIG past the ceiling),
  `xtgettcap_names_the_terminfo_entry`; worker `session_actor`
  `xtgettcap_is_answered_with_the_shells_term`, and the existing
  `a_checkpoint_waits_for_the_end_of_an_escape_sequence` over the new ground check.

- ✅ **A pending wrap does not survive a resize that makes room: ghostty #14458, now merged**
  (2026-09-30). A line that ends in the last column leaves the cursor waiting to wrap. When a
  resize then left room after it, ghostty kept the wait, so the next character started a new
  row: `123456789|`, widened, then `X`, printed `X` on the next row; narrowed to 8 columns it
  left a blank row. Tiles resize all the time (strip springs, window drags, a remote size),
  so output that ends at the edge hits it. fornwall's PR clears the wait and moves the cursor
  to where the next character goes, as ghostty already did for the saved cursor. We carried
  it on `aislopware/ghostty` until upstream merged it the same day (`26e64dfb4`). The
  rebase onto that merge dropped our copy (the old `7d0734aa8` is kept as
  `archive/main-2026-09-30-pre-f9e8270`). Test: engine
  `a_pending_wrap_does_not_survive_a_resize_that_makes_room` (both examples from the PR).
  The PR steps the cursor past the last character even with autowrap (DECAWM) off. There,
  ghostty's print never wraps and overwrites the cell under the cursor, as xterm does, so after
  a widening resize the next character landed one cell too far, and a combining mark attached
  to the wrong cell. Our commit on top (`741a800e8` after the rebase) gives `Screen.Resize` a `wraparound` flag,
  which the terminal passes for both screens. Without autowrap it clears the stale wait but
  leaves the live and saved cursors on the last cell, so turning autowrap back on does not
  wrap mid-row. Its Zig tests also cover the alternate screen (resized without reflow) and a
  wide character that reflow moves to the next row. `vendor/ghostty` and the binding's
  `GHOSTTY_COMMIT` point at `741a800e8`; the headers did not move. Upstream has no PR for this
  yet: a draft waits for the user in `.research/ghostty-upstream-prs.md`. Engine test:
  `without_autowrap_a_resize_keeps_the_cursor_on_the_last_cell` (it failed on `038609517`
  with `123456789|XY`). The same day, ghostty's open terminal PRs were weighed and none was
  both proven and small. DECSTR soft reset (#13333) and XTQMODKEYS (#13332) wait on
  requested changes. The mouse-mode grouping (#14439) is a large refactor. #11711 fixes a
  prompt click that Slopty handles itself.

- ✅ **A saved cursor stays after its line through reflow: ghostty #14478 plus our follow-up**
  (2026-09-30; upstream merged the PR the same day as `dc3f73a69`). A cursor saved (DECSC) while
  waiting to wrap moved into the blank after its line on one widening, and a second widening
  pulled it back onto the line's last character: `AAA|` then `X` printed `AAX|`. Reflow bounded
  a tracked pin in a row's trailing blanks by the column the previous line ended at, even when
  a hard line break or a pending wrap starts the row at column 0. fornwall's PR measures from
  column 0 in those cases. Review found the same bound wrong in a
  second way it leaves alone: it counts from where the row starts, so when narrowing wraps the
  content past the first row, the bound falls inside the content and a restored cursor
  overwrites a character (`abcdef`, saved at column 8, narrowed to 4: `abcX`/`ef` instead of
  `abcd`/`ef X`). Our commit on top (`5543af3d1` since the rebase onto the merge) bounds the
  pin by the end of the row where
  the content ends, computed once per row so several pins get the same bound in any order.
  Its Zig tests cover two pins, content filling a row exactly, a far pin, an overflowing soft
  continuation, the live and restored cursor, and saved cursors on both screens (mode 1049)
  through repeated widening; `zig build test-lib-vt` passes (6564 tests). The alternate screen
  resizes without reflow and never had the bug. The fork was rebased onto upstream
  `4da7523fa` (old head kept as `archive/main-2026-09-30-pre-4da7523`); the binding pins it at
  `5f58ea39` (headers unchanged) and `vendor/ghostty` sits at `61eea99c7`. Engine tests:
  `a_saved_cursor_survives_repeated_widening` (failed with `AAX|`) and
  `a_saved_cursor_after_a_line_that_narrowing_wraps_stays_after_it` (failed with `abcX`). A
  draft of the follow-up for upstream is in `.research/ghostty-upstream-prs.md`.

- ✅ **Prompt marks come from libghostty's semantic prompt effect; our OSC 133 scanner is gone**
  (2026-09-30, ghostty #14479, merged as `36ec90a07`). The engine scanned every PTY read for
  OSC 133 on its own and fed the terminal up to each mark, so it knew the cursor at the mark.
  That was a second parse on the output path, kept in step with libghostty's by hand, and it
  held state libghostty also holds. Mitchell's PR reports each step the shell writes (prompt,
  input, output, command end with its exit code and `err`, the decoded command line) and a
  full reset (RIS), as effects called from inside `vt_write`. Review against the parser and
  the stream:
  - **Order**: the effect runs at the sequence's terminator, after the terminal applied it
    and before any later byte. The PR's tests show the bytes before were applied; our fork's
    test (`74bc7d05e`) feeds text, a `D`, text, a `P`, text, a RIS and text as one slice and
    reads the cursor in each call.
  - **Re-entrancy**: a callback may read the terminal but must not write to it. The Rust
    binding hands it a `&Terminal`, so it cannot call `vt_write`, `reset` or `resize`.
  - **What it leaves out**: `aid`, `cl`, `click_events`, `special_key`, and `redraw`. The
    event describes what happened, not how the shell said it. Slopty uses none of these
    except `redraw`, which now comes from the terminal's state (below). The exit code is
    there as an `i32`, and the engine keeps it only when it fits a `u8`, as the scanner did.
  - **Bug: a full reset turned prompt clearing back on.** libghostty-vt terminals start with
    `shell_redraws_prompt = .false`, but `fullReset` rebuilt the flags from their Zig
    default, `.true`. After a `reset` in bash, a resize cleared the prompt and bash did not
    draw it again. Fixed on the fork (`7a0978f4f`): `Terminal.Options.default_prompt_redraw`,
    which init applies and a reset restores. The C API passes `.false` for new terminals and
    for terminals decoded from a snapshot.
  - **Added: `GHOSTTY_TERMINAL_DATA_PROMPT_REDRAW`** (`bf9ba3300`), so the engine reads what
    the terminal will clear on a resize instead of parsing `redraw=` itself. With the
    existing `cursor_at_prompt` getter, the engine keeps no copy of either.
  - **Bug: a step whose options overflow the OSC buffer was lost.** OSC 133 is captured into
    the parser's fixed 2048-byte buffer, and a longer one went invalid. fish 4 sends the
    command line as `C;cmdline_url=…`, so a command over about 2 KB lost its output start:
    the row stayed in the prompt and no step was reported. Our scanner lost it past 128 bytes.
    Fixed on the fork (`3ccf1b06a`): an overflowing 133 keeps the options that fit whole and
    drops the one that was cut.

  The engine now queues each mark with the cursor row tracked (`track_grid_ref`) and folds the
  queue in order once the write has settled, so a mark keeps its row through whatever the
  rest of the write scrolled or evicted. A RIS in the queue starts a new numbering and drops
  the marks, statuses and blocks. Folding after the write showed that a renumbering (a
  resize, a full reset, history evicted past the anchor) dropped an open command: a command
  typed after a tile resize at a bash prompt was never tracked, and one running through a
  resize never ended, so a waiter on it hung. A renumbering now keeps the newest block when
  it has not ended, reopened at the cursor. That also lets a `reset` command end. Numbers
  (MEASUREMENTS, "OSC 133 marks from libghostty's semantic prompt effect"): the engine's
  overhead over the bare write fell 25 % on plain output and 40 % per OSC on OSC-heavy output.
  One typed byte costs 5 % fewer instructions. Tests: `a_full_reset_drops_the_command_state`,
  `a_full_reset_forgets_that_the_shell_redraws_its_prompt`,
  `a_command_that_resets_the_terminal_still_ends`, `a_long_command_line_still_starts_the_output`,
  `a_command_running_through_a_resize_still_ends` and
  `a_command_typed_after_a_resize_at_the_prompt_is_tracked`. The last five failed on the
  scanner engine. `a_command_whose_prompt_the_same_write_evicts_still_ends` and
  `a_command_end_counts_when_libghostty_acts_on_it` replace the scanner's framing test. The
  binding (`aislopware/libghostty-rs`) gains `on_semantic_prompt`, `on_reset` and
  `prompt_redraw`. The event is borrowed for the call and allocates nothing. The command line
  is bytes, since a decoded `cmdline_url` need not be UTF-8. Not taken yet: using the shell's
  own command line for `CommandBlock.command`. Our shell integrations send none.
- ✅ **RIS resets the palette: ghostty #14480** (korikhin, 2026-09-30, carried on the fork as
  `efc6b01fe` and `cfd6fa8f7`). xterm's `ReallyReset` resets the ANSI colours on RIS and on
  DECSTR, and keeps the dynamic ones (OSC 10, 11, 12). ghostty kept an OSC 4 change through
  RIS. The PR has no test; the fork adds one (`f96ae0c96`). ghostty does not do a full DECSTR
  yet (#13333 waits on requested changes), so only RIS resets the palette. The engine reports
  the reset palette as a colour change: `a_full_reset_returns_the_palette_to_the_default`, and
  `the_programs_colour_changes_are_reported_as_a_whole_set` now expects it.

- ✅ **A shell starts from our own fork** (2026-09-30, crash report). `slopty-ptyd` died of
  SIGABRT on 2026-09-29 with "crashed on child side of fork pre-exec", parent `launchd`
  (`slopty-ptyd-2026-09-29-231734.ips`). The frames are std's `Command::spawn`+2832 calling
  `process::abort`: that offset, in a build of the same std, follows the 78-byte message
  "fatal runtime error: assertion failed: output.write(&bytes).is_ok(), aborting". The forked
  child could not `chdir` (its session's directory gone) and std writes the errno to the
  parent over a pipe, `rtassert!`ing the write; the parent had been killed mid-spawn, so the
  pipe had no reader, `SIGPIPE` was still ignored (std resets it only after `chdir`), and the
  write's `EPIPE` became an abort. Reproduced: killing a spawner of `current_dir`-missing
  commands 300 times gave 5 such reports with std's fork path, 0 with `posix_spawn`.
  `posix_spawn` was measured and rejected: 388 µs against 919 µs p50 for fork and exec, but
  on macOS its file actions run before `POSIX_SPAWN_SETSID`, so the tty never becomes the
  controlling terminal (`zsh -c`, `dash -c`, `cat /dev/tty` all get `ENXIO`; bash hides it by
  opening its tty at start-up; fish refuses to run). `crates/slopty-pty/src/spawn.rs` forks
  itself: everything (program path, `argv`, `envp`, directory) is built before the fork, the
  child makes system calls only (`chdir` first so a missing directory leaves the tty alone,
  `setsid`, `dup2`, `TIOCSCTTY`, every other descriptor closed, every signal back to default,
  `execve`, and `/bin/sh` for a file the kernel cannot run, as `execvp` does), and a failed
  step is reported and ends in `_exit(127)`. Signals are blocked across the fork, so no
  handler of the daemon runs in the child. The child is a `slopty_pty::Child` reaped on
  `SIGCHLD`.

  How the parent learns the outcome (a second review): a report pipe is close-on-exec only a
  moment after it exists on macOS (no `pipe2`), and std and tokio start commands with
  `posix_spawn`, which copies every descriptor not yet close-on-exec. A child the worker
  started in that moment (`git`, `ps`, a long `ssh`) held the write end, the pipe never read as
  closed, and the spawn blocked its tokio thread for as long as that child lived. So on macOS
  there is no pipe: the child stores its report in a page shared with the parent (one word,
  async-signal-safe), and the parent learns of the `exec` or the exit from the kernel through
  `EVFILT_PROC` with `NOTE_EXEC | NOTE_EXIT`. An `exec` before the watch began shows as
  `PROC_FLAG_EXEC` (a probe: set after `execve`, clear in a child just forked from a parent
  that has it), read through `PROC_PIDT_SHORTBSDINFO`. The full `PROC_PIDTBSDINFO` is refused
  with `EPERM` for a process whose effective user is not ours, which a setuid program is
  right after its `exec` (a probe on `/usr/bin/login`: `EPERM` from the full view, the flag
  from the short one). A third review caught that: a `sudo` or `login` that ran before the
  watch looked not yet run, and the spawn waited on it for as long as it waited for someone
  to type. A child gone before the watch makes the watch fail with `ESRCH`; one that exits
  between the watch and the looks shows through `waitid(WNOWAIT)`. Linux keeps the pipe, made
  close-on-exec from the start with `pipe2`. With nothing to guard, the lock that serialised
  spawns is gone.

  A hostile review against std's `do_exec` (1.98), portable-pty, alacritty and wezterm found
  six faults in the first cut; each has a test that failed before its fix.
  - A failed `fork` (the user at the process limit) became a `Child` of pid -1. rustix's
    `Pid::from_raw` asserts against a negative only in debug builds, so a release build
    would later `waitpid(-1)`, reaping any child, and `kill(-1, SIGKILL)`, killing every
    process of the user. The -1 is checked first now.
  - The master became close-on-exec only after `posix_openpt` returned, and a fork by
    another thread in between handed it to that shell for good: closing the master's tile
    then never hung its shell up. The test leaked one in 2 of 3 runs. The master is now
    opened as `/dev/ptmx` with `O_CLOEXEC`, which is what `posix_openpt` does inside on macOS
    and what rustix does on Linux (rustix passes `O_CLOEXEC` to `posix_openpt` only on Linux
    and the BSDs). Under concurrent opens that `open` sometimes fails with errno -6, XNU's
    in-kernel `EREDRIVEOPEN` ("open again") leaking out. A probe of 80 000 opens from four
    threads saw it once through `open` and once through `posix_openpt`, so it predates the
    change. With eight threads it came 23 times in 32 000 opens, never twice in a row. It
    made the stress tests flaky, and a soak met it after 1195 cycles. `Pty::open` now makes
    the open again, yielding in between, up to 64 times, and past that fails with a
    `ResourceBusy` that names `EREDRIVEOPEN` rather than a bare -6
    (`an_open_the_kernel_asks_to_redo_is_redone_then_given_up_clearly`).
  - Close-on-exec alone does not cover a descriptor that gets it a moment after it exists,
    and macOS has no atomic way for an accepted socket (no `accept4`), a received one (no
    `MSG_CMSG_CLOEXEC`) or a pipe (no `pipe2`). So the child closes everything above 2 but
    a report pipe (Linux), as wezterm and iTerm2 do. macOS has no `closefrom` and `OPEN_MAX` is a
    million here, so the child lists its own descriptors with `proc_pidinfo(PROC_PIDLISTFDS)`
    into a stack buffer. Linux uses `close_range`. An earlier reading that XNU leaks even
    `O_CLOEXEC` descriptors across a fork was wrong: the spawn returns at `execve`, and the
    new program's loader briefly opens files of its own. The test reads the descriptors once
    `cat` has echoed a line.
  - With fds 0 to 2 closed, the slave and the report pipe land on them. A slave on 1 kept
    its close-on-exec through a `dup2` onto itself, and the shell lost its stdout. A pipe on
    1 was overwritten by the tty, so a failed `exec` read as a success. Both move above 2
    before the fork.
  - A bare program name was looked up on the daemon's `PATH`, where std and `execvp` use the
    child's, so a session's own `PATH` lost. A name on no `PATH` ran as a file of that name
    in the session's directory. Now the child's `PATH` is searched first, then the daemon's,
    and a name on neither is `NotFound`. The second review found three more gaps against
    `execvp`, now closed. A relative or empty `PATH` entry was checked from the daemon's
    directory, where the child's `execvp` reads it from the session's. A file on `PATH` that
    may not be run gave `NotFound` where `execvp` gives `EACCES`. A file with no `#!` line
    failed with `ENOEXEC` where `execvp` runs it with `/bin/sh`.
  - Only caught signals went back to default. A daemon started under `nohup` or a script's
    `&` gave every shell `SIGHUP`, or `SIGINT` and `SIGQUIT`, ignored, and `exec` keeps them
    ignored. All are reset now, as ghostty, alacritty and wezterm do.

  Checked and sound: nothing between the fork and the `exec` allocates, locks, panics or
  runs a `Drop`. `setsid` comes before `TIOCSCTTY`. The report read retries `EINTR`, and its
  five bytes are one atomic pipe write. A child that exits before its waiter subscribes to
  `SIGCHLD` is found by the look after subscribing. A pid stays ours until we reap it. If the
  thread's signal mask cannot be put back after the fork, the child is killed and reaped
  rather than left without an owner. `vfork` was not taken: Rust has no `returns_twice`, and
  libc marks `vfork` deprecated on Linux for the memory corruption that causes
  (rust-lang/libc#1596). One gap is left open: a master the worker receives over its socket
  becomes close-on-exec a moment after it arrives (macOS has no `MSG_CMSG_CLOEXEC`), and a
  `git` the worker starts in that moment holds it until it exits. Cost: `MEASUREMENTS.md`,
  "spawning a shell: the fork against std's".

  Tests: `a_child_that_cannot_start_with_its_parent_gone_exits_quietly`,
  `a_child_that_cannot_start_is_a_spawn_error`, `the_tty_is_the_controlling_terminal_of_the_child`
  (fails without `TIOCSCTTY`), `a_bare_name_on_no_path_is_not_run_from_the_directory`,
  `a_path_hit_that_may_not_run_is_permission_denied`, and the three ways the handshake
  catches up with a child that got ahead of the watch, forced by a test hook that waits
  inside the spawn for the child's state rather than sleeping:
  `a_setuid_program_that_ran_before_the_watch_is_seen_running` (hung for good with the full
  view), `a_child_gone_before_the_watch_is_caught_up_with` and
  `a_child_that_exits_while_the_watch_is_made_is_caught_up_with`; `tests/spawn.rs`: 120 shells from four
  threads while two others allocate under a lock, `no_descriptor_of_the_daemon_leaks_into_a_shell`,
  `a_bare_program_is_found_on_the_path_of_the_child`,
  `a_relative_path_entry_is_taken_from_the_directory_of_the_child`,
  `a_script_without_an_interpreter_line_runs_in_the_shell`,
  `a_shell_starts_with_every_signal_at_its_default` and `an_environment_with_a_nul_is_refused`.
  `tests/spawn_process_state.rs` runs, in one process with no other children: failed spawns
  leave no child; closed standard descriptors; a `fork` at `RLIMIT_NPROC` 1; 301 descriptors
  without close-on-exec, one at fd 5000, past the child's batch of 256; and, through
  `pthread_atfork` handlers, the crash itself made certain rather than timed (every pipe's
  read end swapped for `/dev/null` before the child reports: it exits 127 every time, or on
  macOS the spawn names the step). The same handlers hold a copy of every pipe's write end
  at the fork, as a `posix_spawn`ed child would, and the spawn must still return within 5 s:
  the pipe design failed it, and on macOS there is now no pipe to hold, so it stays as a
  guard against one coming back. The ignored control `std_command_aborts_when_its_parent_is_gone`,
  which writes a crash report, shows std's child dying of `SIGABRT` under the same harness.

- ✅ **ghostty on 76895d97b; libghostty-rs takes the stack's refresh, fixes six soundness and
  cost faults, and reads a frame's dirty flags in one call** (2026-09-30). The ghostty fork
  (`aislopware/ghostty` `89c9624f0`) is our twelve commits rebased onto ghostty `76895d97b`,
  eight commits past `dc3f73a69`. Our copy of #14480 (the palette reset on RIS) went in the
  previous rebase, and only its test is still carried. #14481 (an OSC command's data with a
  NULL command) and #14482 (a same-size resize is no no-op) came in clean. `zig build
  test-lib-vt` passes. #14482 changes only the header's words: libghostty always did the rest
  of a resize (pixel size, the in-band size report, synchronized output off) when the grid
  size stayed, and it reports that the render hold ended. The engine never passes a resize
  it already has to libghostty (`resize` returns when the `TermSize` is equal), so no frame
  pays for one. A new cell size alone is still a resize, and it ends a hold, which
  `a_resize_ends_a_render_hold_and_the_same_size_does_not` pins.
  - **A frame's dirty flags in one call** (ghostty `89c9624f0`,
    `GHOSTTY_RENDER_STATE_DATA_ROW_DIRTY`; libghostty-rs `Snapshot::dirty_rows`). It returns a
    view of the render state's own per-row flags, indexed like the row iteration, overscan
    included. Nothing is copied, and the flags are not read row by row through the C API. The
    Rust view holds a pointer, not a `&[bool]`: `RowIteration::set_dirty` and
    `Snapshot::clean` write the flags through `&self` while it lives, which a shared slice
    would forbid. It borrows the render state as the snapshot does, so no update can move
    them. `build_frame` reads it where it asked each row for its flag. That is the path a
    scroll takes, since every row is visited there. The flags count only under
    `Dirty::Partial`, as with `next_dirty`. What changed reaches the client as it did before,
    as the rows a `Frame` carries.
  - **The stack, refreshed.** Upstream force-pushed #92–#95. Their only change from what we
    carried is a `cfg(not(miri))` on two counting-allocator tests. The fork is now rebased
    onto `stack/ghostty-examples` `102f47f` and carries upstream's copies. The earlier head is
    kept as `backup/master-2026-10-01-pre-stack-refresh` in `.research/libghostty-rs`. The
    safe `format_vec` (in place of the unsafe `format_write`, for code that forbids unsafe)
    is kept.
  - **Found in review and fixed in the fork** (a line-by-line read of the stack and of our
    commits; each fix has a test that fails without it):
    - `DesktopNotification::title`/`body` built a `&str` without checking it. The OSC 9 and
      777 parsers pass the program's bytes on as they are, so safe code could hold a string
      that is not UTF-8. They are bytes now. The engine decodes them lossily, and a
      malformed banner shows replacement characters (`desktop_notifications_are_events`).
    - `RenderState::snapshot` read over an update that was begun and never ended (an
      `Update` forgotten in safe code), so its styles were stale. It ends any pending update
      first. With nothing pending that is one call, and it now returns `Result`.
    - The callback table was a `Box` whose pointee C held as userdata. Moving the `Terminal`
      retagged it (a Stacked Borrows violation, reproduced under Miri on a model of it). Every
      callback also built a throwaway `Terminal` view that allocated and freed an empty table:
      two allocations per synchronized-output frame, which Claude Code's TUI draws each
      frame. The table is now a pointer from `Box::into_raw`, allocated with the first
      callback, and the view holds none. `tests/callbacks.rs` counts 300 allocations before
      the fix and 0 after.
    - `From<A> for Allocator` pointed libghostty at its by-value parameter, which was gone
      once `from` returned. It borrows `&A` now, and its vtable is an inline `const`.
    - `to_reader` handed a `Read` a `&mut [u8]` over a buffer Zig may leave uninitialised.
      The buffer is zeroed first (snapshot decoding only, off the frame path).
    - `CellLayout` trusted the packed cell's enum fields to hold the C enums. It now requires
      the manifest to name those types and number them as the bindings do, and it falls back
      to the getters otherwise.
  - **Left open.** The `*_get_multi` calls have no Rust wrappers. The per-row reads (`raw_row`,
    `cells_raw`) would drop by one call per row. That is not worth it until a frame series
    shows the calls. ghostty's `search.h` says `ghostty_search_tick` never touches the
    terminal, but `c/search.zig` reads a pin the terminal tracks, so `Search` stays
    non-`Send`. Numbers: MEASUREMENTS, "ghostty on 76895d97b, libghostty-rs fixes, and a
    frame's dirty flags in one call". Plain output and OSC-heavy output cost what they did.
    Scroll frames are 1–2 % cheaper.

- ✅ **The grid's rows are painted under keys** (2026-09-30, MEASUREMENTS "the grid's rows under
  keys"). The terminal element paints each row in five passes (backgrounds with the selection
  and search hits, underlines, sprites, words, strikethroughs), and each pass over a row is a
  stretch under a key (`Window::paint_keyed`, gpui-fast). GPUI draws a stretch whose key it
  painted last frame again from that frame's scene: in place, or moved when the row only moved
  by whole device pixels, as rows do when output scrolls. Every other thing the grid paints
  (the cursor, the block bands and rules, images, the ⌘-hover link, the input method's text,
  captions, the scrollbar, the bell) is painted every frame between the passes, so the order
  on the glass is what painting the grid whole gives.
  - The key stands for everything the stretch paints relative to its origin. It is the frame's
    part (the words' base: font size, cell width, family, ligatures and the palette with the
    minimum contrast and bold-as-bright; the cell, line, baseline, stroke geometry, sprite
    thickness and scale; the zoom and the raster size), the row's parts hashed by what they
    paint (every word's key and column, every background, stroke and sprite with its colour
    and tile), and what the pass paints on the row beyond them: the selection and search
    marks for the backgrounds, and the block cursor's text colour on its row for the words and
    sprites. The blink phase, the faint prompt and the local-echo guesses reach the key through
    the row's parts, which change with them. The element's size is not in it, so rows a
    resize leaves alone are drawn again. While the zoom is in motion nothing is keyed.
  - A row moved by whole device pixels matches one painted there only if nothing it paints
    rounds differently at the new place. GPUI rounds a quad's edges to the nearest device pixel
    and a glyph to the nearest quarter, so an edge on a half pixel (a 1.5 pt stroke at 1×, a
    grid at a fractional zoom, a tile at a fractional position) rounds either way by a float's
    last bit. The paint oracle found it: a 1.5× window, a scroll, an underline 1 device pixel
    thick drawn again where painting afresh made it 2. So the key holds the row's place unless
    every edge the rows paint (origin, cell, line, baseline, strokes) is within 1/64 of a
    device pixel, and off that grid a row is drawn again only where it was. Dotted and dashed
    runs start on a whole device pixel for the same reason.
  - Rejected: keying only with retention's whole-view replay (the terminal is notified by
    every frame of output, so the view is never replayed while it matters); snapping the grid's
    origin to device pixels so every row is on the grid (text in a tile that slides would move
    in whole pixels rather than GPUI's quarter-pixel glyph steps, a smoothness loss during
    every strip motion).
  - Tests: `terminal/view/paint_oracle.rs` runs two windows through the same random history of
    output, rewrites, scrolls, selections, search hits, cursor moves and blinks, input-method
    text, ⌘-hover, themes (colours, font size, line height, ligatures, minimum contrast, bold
    as bright), zooms, focus, resizes and scale factors of 1, 1.5 and 2, one with retention and
    one painting everything afresh, and requires every frame's primitives to match in order.
    Its text system sizes each glyph's raster by the glyph and face, since the scene keeps a
    sprite's bounds but not its tile. Dropping the cursor's colour, the marks or the place from
    the key each fail it within 150 steps. It also requires more than 1 000 stretches drawn
    again and 100 moved over its six seeds. `…_in_core_text` runs the same histories on the
    Mac's own text system, real fonts shaped and rasterised by Core Text, taken from
    `gpui_platform::text_system()` (gpui-fast `58fb467`, added for it: the platform, the
    only other way to that text system, panics off the main thread, and a test has a thread
    of its own). Both pass over 60 seeds. With the place left out of the key off the grid,
    Core Text fails at seed 2, step 97.

- ✅ **A checkpoint carries the engine's own marks in an OSC of Slopty's, and `WRAPPED` is the
  row above's wrap flag** (2026-10-01). A checkpoint is VT bytes that a fresh engine replays,
  so whatever it carries has to be bytes in that stream. libghostty holds each row's prompt
  flag and each cell's content, which the formatter now writes. The lines prompts started on,
  the statuses commands ended with and the command blocks are the engine's alone, so after
  each screen a checkpoint appends them as `OSC 6973` (`ghostty/carried.rs`), each line counted
  from that screen's oldest row. Only a fresh engine's first write reads it, and a program that
  writes one forges nothing it could not already forge with OSC 133.
  - `WRAPPED` was read from libghostty's `wrap_continuation`. A scrolling region, an inserted or
    deleted line, or history eviction leaves that cache stale. It is now the row above's own
    wrap flag, the one reflow and selection use. So a line's `WRAPPED` can change when only the
    row above it changed. A frame then sends the clean row below again with only the flag
    changed. A changed row whose row above is clean takes its wrap from its own line as last
    sent, which costs no grid lookup.
  - Rejected: a side field beside the checkpoint for the marks (a second format next to the VT
    stream every replay already reads); replaying the marks as OSC 133 (a replay
    would count them again and wake whatever waits on a command's end); walking every row to
    look up the row above (about 1 % of a keystroke's frame).
  - Tests: the terminal fuzzer holds every replayed line to the oracle, marks, links and wraps
    included, and `fuzz/regressions/terminal` keeps one input of each loss it found
    (decisions/testing.md, "What it found next"); `carried.rs`'s round-trip tests.

- ✅ **ghostty on 0081d4530: an empty `OSC 22` gives the pointer back** (2026-10-01). The ghostty
  fork (`aislopware/ghostty` `18fa7131b`) is our 30 commits (PRs #1–#4) rebased onto ghostty
  `0081d4530`, eight commits past `76895d97b`. The rebase was clean and `zig build test-lib-vt`
  passes. libghostty-rs `a49587b` only moves the pin. The headers did not change, so the
  bindings are byte for byte the same.
  - **#14495, an empty `OSC 22` resets the pointer: taken, and needed.** Before it, `OSC 22 ;`
    with no name was logged as an unknown shape and ignored. A program had no way to give its
    pointer back, and the session kept its last shape. libghostty-vt now sets its initial
    shape, `text`, which the engine reports as `PointerShape::Text`, the wire's default. The
    session's pointer follows, and an attach is told nothing. Ghostty's app picks `default`
    over `text` while mouse tracking is on, but that choice lives in its termio, not in
    libghostty-vt. Test: engine `an_empty_pointer_shape_gives_the_pointer_back` covers ST, BEL
    and a reset split across two writes.
  - **#14494, a paste fails after any refused write: no effect on Slopty.** The fix is in
    `ghostty_terminal_paste`. The engine encodes pastes with `paste::encode`
    (`ghostty_paste_encode`). The binding's `Terminal::paste` already failed a refused write
    itself, so that check is now redundant but harmless.
  - The rest is ghostty's OpenGL error message and its VOUCHED list.
  - **Left open.** A checkpoint does not carry the pointer shape: the formatter writes no
    `OSC 22`, and neither does `checkpoint`. So a shape a program set before the last
    checkpoint comes back as the I-beam after a worker restart, until the program sets it
    again.
  - Cost: every engine `*_cost` series stayed within its run-to-run spread, so no budget or
    MEASUREMENTS entry changes. On reruns, `take_frame_unchanged` and `write` move by 25–50
    instructions on their own.
