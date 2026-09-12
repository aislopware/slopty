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

- ✅ **PTY custody in a tiny separate daemon** (`slopty-ptyd`), masters handed to hostd by
  `SCM_RIGHTS` (`nix` `sendmsg`/`recvmsg`; `sendfd` dropped — one fewer dependency, and macOS
  has no `MSG_CMSG_CLOEXEC` so CLOEXEC is set by hand either way). ptyd drains the master into a
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
  `text_links_come_with_their_columns` and the `host_lines_links` golden. macOS only for now:
  the phone key bar arms ⌘ for remote windows but not for terminals, so a tap has nothing to
  read; long-press stays selection. On the phone (2026-09-05) the terminal key bar has a ⌘
  key beside ⌃: it arms one tap (`TerminalView::set_sticky_command`), the next left press
  opens the link under it through `cx.open_url` (`gpui_ios`'s `UIApplication.openURL`, in the
  fork) and disarms; covered by `sticky_command_opens_the_link_under_the_next_tap`.

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
  reads, payloads over 32 bytes dropped, `ESC \` and BEL terminators, `A;k=s`/`k=c` not
  counted as starts); `write` feeds the terminal up to each mark, settles, and records the
  cursor's absolute line in `prompt_starts` / `exit_marks` (both pruned below `base`, both
  cleared with the epoch). A prompt row is `Prompt { exit }` only on a recorded start, else
  `PromptContinuation`; the status is the newest `D` within 4 rows above the start that no
  other start already claimed (the shell may print a blank line or the partial-line `%`
  between `D` and the prompt). Covered by
  `prompt_rows_carry_the_previous_commands_exit_status` (adjacent prompts, a `D` split
  across two writes, a gap row, a two-row prompt, the history path) and
  `captured_zsh_bytes_keep_output_rows_and_statuses` (bytes recorded from a real zsh:
  synchronized output, the `%` partial-line marker, `D` directly before the next `A`).
  *Wire:* `SemanticMark::Prompt` grew `exit: Option<u8>`; every other row still costs one
  byte, a prompt row with a status three (MEASUREMENTS.md: an all-prompt 80×24 frame is
  48 bytes larger than a blank one). The brief's `End { exit }` variant was not added: the
  `D` lands on the row the next prompt starts on, so a separate variant would collide with
  `Prompt` on the same row. `PROTOCOL_VERSION` 6 → 7, golden `host_lines_marks`; the other
  goldens are unchanged because `Unknown` is still variant 0 and `client_hello` only moved
  its version byte.
  *Client/UI:* `TermState::prompt_before/after` walk the cached lines (uncached history is
  skipped, not fetched), `scroll_to_line` puts a line at the top, `last_command_output` is
  the run of `Output` rows above the newest prompt start with the blank tail trimmed
  (`prompt_navigation_and_last_output_follow_the_marks`). ⌘↑/⌘↓/⌘⇧C are Terminal-context
  bindings (`PrevPrompt`, `NextPrompt`, `CopyLastOutput`); the separator is a 1 px quad on
  the prompt-start row's top edge from the same prepaint pass as the selection
  (`separator_color`: fg at 18 %, the theme's `surfaces.error` token at
  `alpha::SEPARATOR_ERROR` (70 %) when the status is non-zero; `crates/slopty-theme/src/lib.rs`,
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
  selection, `?`) and the `host_term_clipboard_write` golden.

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
  or a pick closes it, and the click is not reported to the program; (4) "Rerun" is a paste of
  the command followed by ↩ as a key — the shell sees exactly what the human would have typed
  (bracketed when it asked), so aliases, history and hooks all apply; (5) it is our own small
  menu (the tokens, the a11y roles, the tab ring) rather than gpui-kit's `ContextMenu`, whose
  element-state machinery adds nothing here. Wire: the two marks; goldens `host_lines_marks`
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
  tone on a non-zero status ("failed (1) 1 min 4 s"), between the chat pill and the agent
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
