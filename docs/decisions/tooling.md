# Decisions — Tooling

See `docs/DECISIONS.md` for the legend. Newest entries go at the end.

- ✅ **The gate has a budget: 5–10 minutes warm, without losing a check** (user, 2026-09-05).
  What changed to meet it: every step prints its wall time (`step`), so a slow gate names its
  culprit; the tools that never touch `target/` (deny, shear, typos, taplo, committed) run on
  a second thread beside the cargo steps with their output captured and printed whole
  (`quiet_step`), since the cargo steps serialise on the build lock anyway; the two iOS
  triples are one `cargo clippy --target … --target …` invocation (one resolve, both targets
  compiled side by side) without `--all-targets` — tests never run on iOS and their code is
  the host's, so the iOS pass checks libraries and binaries; `rustc-wrapper = "sccache"` in
  `.cargo/config.toml` shares dependency compiles between the main checkout, the parallel
  sessions' worktrees and the triples (workspace crates stay incremental, which sccache passes
  through). Timings in MEASUREMENTS.md "gate wall time". Not done: a separate target dir per
  step to overlap cargo invocations (would double the disk and the cold builds).

- ✅ Rust 1.99.0 pinned; edition 2024; resolver 3; `[workspace.lints]` with clippy
  all/pedantic/nursery/cargo + curated restriction lints; `panic = "unwind"` everywhere
  (`panic = "abort"` turns any ObjC exception crossing objc2 into a process abort).

- ✅ **Dependency refresh is a routine, not an event** (2026-09-12): `cargo upgrade --dry-run
  --incompatible` (cargo-edit) lists what the reqs hold back, `cargo update` moves the rest;
  bump the reqs in `Cargo.toml` and the tool floors in `xtask::setup`, then gate and e2e.
  The one crate that must not follow crates.io is `core-video`: `gpui::SurfaceSource:
  From<CVPixelBuffer>` is typed against the fork's version (0.5.2 while zed stays there), so a
  bump to 0.6 fails `slopty-ui` — it moves when the fork's does (comment beside the req).

- ✅ nextest 0.9.146 · insta 1.48 · proptest 1.11 · cargo-mutants 27.1 · cargo-llvm-cov 0.9 ·
  cargo-deny 0.20.2 · cargo-shear 1.14.0 · cargo-hack 0.6.45 · cargo-semver-checks 0.50 ·
  typos 1.50.3 · taplo 0.10 · bacon 3.25 · samply 0.13.1 · tracing-tracy 0.12.

- ✅ **Releases from Conventional Commits**: `committed` 1.1.11 lints every message
  (`cargo gate` over the range since the last tag); `git-cliff` 2.14.2 computes
  the next version (`--bumped-version`, pre-1.0 rules: breaking → minor, feat → patch) and writes
  `CHANGELOG.md`; `cargo xtask release` glues them and tags `vX.Y.Z`. Rejected: cocogitto
  (last release 2026-03, overlaps both), release-plz / cargo-release (crates.io-centric; nothing
  here is published).

- ✅ **App icon rendered from one SVG at build time** (2026-09-05). `assets/icon.svg` (dark
  full-bleed square, faint dot grid for the canvas, an accent `❯` and a green cursor block) is
  rasterised by `xtask::icon` with `resvg` 0.48.1 + `tiny-skia` 0.12 (MPL-2.0/BSD, already
  allowed) into `Slopty.icns` (16…512 pt at 1× and 2× via `icns` 0.4.0) for `cargo xtask
  bundle` (`CFBundleIconFile`) and a 1024 px PNG in a generated `Assets.xcassets` for `cargo
  xtask ios` (`ASSETCATALOG_COMPILER_APPICON_NAME`). No `iconutil`/`sips`/design tool; `cargo
  xtask icon [dir]` writes the PNG ladder for a look. Full bleed on purpose: macOS 26 and iOS
  apply their own squircle mask, so baked-in rounded corners would double up. Verified: the
  Dock shows the icon for the debug bundle and the iOS 26.5 simulator home screen shows it
  after `cargo xtask ios sim`.

- ✅ Miri only for pure crates (cannot cross objc2 FFI); cargo-careful + ASan/TSan nightly lane for
  the rest; `leaks --atExit` for framework wrappers (Valgrind does not exist on Apple silicon).

- ✅ No mold/lld on macOS (Apple's ld-prime is competitive; mold's Mach-O port is commercial).

- ✅ objc2 0.6.4 / objc2-* 0.3.2 / block2 0.6.2 / dispatch2 0.3.1, pinned exact, `CFRetained`
  ownership in the type system; one counted `from_raw`/`retain` admission per wrapper crate.

- ✅ **The gate checks a snapshot, in parallel lanes** (2026-09-12, at the user's request that
  the gate be as fast as possible and never waited on). Two costs were paid every cycle: the
  cargo steps ran one after another because they share `target/`'s build lock (MEASUREMENTS
  2026-09-06 (iii)), and nothing could be edited while they ran because the tree under test
  was the tree being edited. Rulings: (1) `cargo gate` syncs the tree (`git ls-files
  --cached --others --exclude-standard`, so untracked new files count and ignored ones do
  not) into `target/gate/tree` — a file is copied only when missing or different, so cargo's
  mtime fingerprints in the snapshot rebuild exactly what changed; files gone from the tree
  are pruned; `vendor/ghostty` is a symlink to the pinned submodule — and every check runs
  there, so the working tree is free the moment the gate starts; (2) the cargo steps are
  lanes on their own target dirs (`target/gate/{clippy-host,clippy-ios,tests,rustdoc}`, jobs
  4/4/6/3 so four builds share ten cores without thrashing) beside the tools lane (deny,
  shear, typos on the snapshot; taplo and committed on the working tree, which they read
  through git); `sccache` keeps the dependency compiles shared across the lanes, so the extra
  dirs cost disk (workspace crates and links) and one cold run, not repeated dependency
  builds; (3) fmt is checked first and alone — a second, and a formatting slip should not
  cost a build; `--fix` runs the fixers on the working tree before the snapshot is taken;
  (4) each lane's output is captured and printed whole with its time (`quiet_step`), so the
  log reads as before and a failure names its lane; `--in-place` keeps the old behaviour for
  CI. What did not change: the checks. Numbers in MEASUREMENTS "gate wall time, third look".
  One trap, hit on the second run: `std::fs::copy` clones the source's mtime on APFS, and a
  file edited *while* a lane was building the old bytes then carries a stamp older than that
  lane's fingerprint, so cargo calls the stale build fresh (the iOS lane kept a `slopty-proto`
  without `TermRequest::Clear` while the host lane, which had built earlier, rebuilt). A copied
  file is therefore stamped with the time of the copy.

- ✅ **The guardrails, surveyed and tightened to what catches something** (2026-09-12, at the
  user's request for the strictest tooling that still finds real defects). What was already in place: clippy
  all/pedantic/nursery/cargo, a curated restriction set, rustfmt nightly, taplo, typos,
  committed, cargo-deny (advisories, licences, bans, sources), cargo-shear, nextest with
  per-test timeouts, rustdoc `-D warnings`, overflow checks in release, `target-cpu` per
  triple, every tool at its latest release (checked against crates.io the same day). The
  survey: every allow-by-default rustc and clippy lint was switched on once over the whole
  tree and counted (`/tmp/lints.log`, 1 439 warnings). Rulings: (1) the panic family
  (`unwrap_used`, `expect_used`, `panic`, `unreachable`, `todo`, `unimplemented`,
  `indexing_slicing`, `arithmetic_side_effects`) is `deny` in the manifest, not only under
  `-D warnings` — a library never panics, tests may (`clippy.toml`); (2) 9 rustc and 35 clippy
  lints adopted, each of which fired nowhere or at a handful of sites fixed in the same change:
  `string_slice` (four `&s[..n]` that could split a UTF-8 character, now `strip_prefix`/`get`),
  `missing_copy_implementations` (17 types that are `Copy` now, and three `clone()` calls gone
  with them), `unused_trait_names` (20 imports now `as _`), `mod_module_files`
  (`terminal/mod.rs` → `terminal.rs`), `get_unwrap`, `deref_by_slicing`,
  `map_with_unused_argument_over_ranges` (`repeat_with().take()`), `needless_raw_strings`,
  `error_impl_error` (`settings::Error` → `SettingsError`), `doc_paragraphs_missing_punctuation`,
  and the zero-hit ones (`mutex_atomic`, `mutex_integer`, `string_add`, `format_push_string`,
  `large_stack_frames`, `unchecked_time_subtraction`, `set_contains_or_insert`,
  `non_std_lazy_statics`, `let_underscore_drop`, `unit_bindings`, `redundant_lifetimes`, …);
  (3) fifteen rejected, each with its count and reason in the manifest comment
  (`pattern_type_mismatch` 342, `default_numeric_fallback` 309, `elided_lifetimes_in_paths`
  282, `wildcard_enum_match_arm` 119, `integer_division` 77, `ffi_unwind_calls` 34 — the
  `C-unwind` ABI is what lets objc2 catch an exception —, `iter_over_hash_type` 17 order-free
  loops, `non_ascii_idents` firing on zerocopy's derive output, …); (4) `#![forbid(unsafe_code)]`
  on every crate that has none (eleven more: the Mac app and daemons, agent, client, core,
  engine, net, predict; not the iOS crate, whose `#[unsafe(no_mangle)]` exports are unsafe code
  by definition), so `unsafe` cannot creep in unseen — it lives in capture, codec, pty, platform,
  input, host, ui and e2e, each block with its rule; (5) rustfmt gains the stable house-style
  options (`use_field_init_shorthand`, `use_try_shorthand`, `hex_literal_case = "Lower"`,
  `condense_wildcard_suffixes`) and the nightly normalisers (`normalize_comments`,
  `normalize_doc_attributes`, `format_macro_matchers`, `format_macro_bodies`,
  `doc_comment_code_block_width`) — measured first: each at 0–4 files of churn;
  `hex_literal_case` touched 52; not `error_on_line_overflow`/`error_on_unformatted` (229 lines
  rustfmt leaves alone on purpose: long string literals, `if` inside arguments) and not
  `match_block_trailing_comma`/`overflow_delimited_expr` (1 057 and 138 sites of pure style);
  (6) the slow checks the gate cannot afford are `cargo xtask deep <check>` (`xtask/src/deep.rs`)
  and a weekly workflow (`.github/workflows/deep.yml`, one runner per check): Miri over the
  pure crates, ThreadSanitizer and AddressSanitizer over the daemons and the codec with
  `-Zbuild-std`, `cargo hack --each-feature`, `cargo llvm-cov` coverage, and `cargo mutants`
  per crate on demand; `cargo xtask profile -- <cmd>` records with samply (pure Rust, Firefox
  Profiler). What was verified on this machine: `deep features` and `deep miri -p slopty-proto
  -p slopty-core` (numbers in MEASUREMENTS "deep checks"); the sanitizer and coverage builds
  are the workflow's, not yet run here. Not adopted, with the reason: `cargo vet`/`crev`
  (a review ledger for ~600 crates nobody here would keep honest; deny's advisories, sources
  and licence gates are the supply-chain check), `cargo-audit` (deny covers it), `cargo-udeps`
  (shear), `cargo-fuzz` (libFuzzer is a C++ runtime; proptest covers the codec and the grid),
  release `debug-assertions` (cost on every frame for what the test profile already runs).

- ✅ **No git hooks; the gate checks the index** (2026-09-24). Several agents now edit one
  checkout at once, since worktrees each cost a cold GPUI build.
  - **Why prek's hooks went.** Its pre-commit hook stashed unstaged changes, ran
    `cargo fmt --all --check` over the *working tree*, then restored them. That failed a commit
    on another agent's half-edited file and raced that agent's writes. The stash/restore cycle
    can silently drop an edit that lands in between.
  - **What covers it now.** `cargo gate` already runs fmt, taplo, typos and `committed`, and it
    now snapshots the **index** (`git cat-file --batch` into `target/gate/tree`, submodules at
    their pinned commits), so what it passes is exactly what the commit records.
  - Removed: the prek hooks, `.pre-commit-config.yaml` and prek from `xtask setup`.

- ✅ **target/ stays bounded: one feature set per dependency, and a sweep of what nothing uses**
  (2026-09-26). `target/` reached 374 GB on the Lacie drive and 212 GB a day after a manual
  clean (`debug/deps` 85 GB, `debug/incremental` 25 GB, the gate lanes 74 GB), with 150–285
  incremental caches per crate and 41 builds of `gpui`.
  - **Why it grew.** Cargo unifies features only over the packages one invocation selects.
    Every `cargo xtask check -p <set>` therefore resolved `syn`, `proc-macro2`, `serde_core`,
    `tokio`, `libc` and the rest with other features (`cargo tree -e features`: 8 of the 26
    packages under `-p slopty-proto` differ from `--workspace`, 14–35 under other sets, on all
    three triples). A dependency's features are part of its unit hash and of every hash above
    it, so each new set compiled and kept its own copy of the dependencies and of the crates
    that use them. On top of that, cargo never deletes a unit: a `cargo update`, a fork
    rebase or a new toolchain leaves the old ones behind in every target dir, gate lanes
    included. `resolver.feature-unification = "workspace"` would settle the first half inside
    cargo, but cargo 1.99.0 still ignores it without `-Zfeature-unification`.
  - **One feature set.** `workspace-hack/` is a `cargo hakari` crate: the feature union of every
    third-party dependency per triple, for normal and build dependencies. No crate depends on
    it, which is the unusual part. The union carries the tests' features: GPUI's
    `test-support`, tokio's `test-util` (its clock then takes a lock on every read) and
    `serde_json`'s `preserve_order`. As a dependency of every member, as hakari's docs set it
    up, they would ship in the app and the daemons. Instead `cargo xtask check` names it
    beside the checked crates in every build step: `cargo tree` then shows 0 third-party
    packages whose features differ from `--workspace`, for every set tried, on the host and
    both iOS triples, so a check builds exactly what the gate builds and shipped builds are
    untouched. The gate's tools lane runs `cargo hakari generate --diff`, `cargo gate --fix`
    regenerates it, and taplo leaves the generated manifest alone. The cost: the first check
    in a target dir builds the whole union once, GPUI included (687 s for a clippy pass and
    1 900 s for a full `check` under a load average above 100); every check shares it after
    that.
  - **A sweep.** `cargo xtask prune`, run after every `check` and `gate`, deletes the units no
    build has read for a day (the `.fingerprint` dir with its `deps/` and `build/` artifacts,
    and any artifact whose unit is gone) and the incremental caches no compile has touched
    for a day, in every target dir under `target/`. Each dir is pruned under cargo's own
    `.cargo-lock`, so no build runs in it meanwhile. After a check or gate the pass skips a
    dir whose lock is held; by hand it waits. Use comes from access times, because cargo reads
    a unit's fingerprint files on every build that includes it, fresh or not. APFS updates an
    access time only while it is not later than the modification time, so the sweep sets the
    access times to the epoch when each window opens. Any read in the window then stamps
    them, and a unit is gone after one to two idle days. A wanted unit that goes is rebuilt,
    its dependencies from sccache.
  - **Not done.** `cargo-sweep`: its `--time` reads access times as they are, and APFS stops
    updating those once a file has been read after its last write, so it would delete live
    units. The hakari hack as every member's dependency, for the reason above.
    `CARGO_INCREMENTAL=0` in the gate lanes: a gate after a small edit is fast because of
    the lanes' incremental caches, and the sweep bounds them. A `cargo clean` of the lanes
    when `Cargo.lock` changes would make every gate after a sync cold. Numbers:
    MEASUREMENTS "target/ growth under varied check sets".
  - **The fork checkouts are swept too** (2026-09-30). The volume filled to 32 MB free because
    nothing pruned `.research/*/target*`. The gpui-fast fork held 87 GB of earlier compiles'
    object files and gpui-kit 88 GB. Every `target*` dir under `.research/` that has cargo's
    `CACHEDIR.TAG` gets the same sweep under its own budget (`SLOPTY_FORK_BUDGET_GB`, 40 GB).
    The forks go first, in `prune`, after `check`/`gate`, and in the gate's floor check, since
    their build output rebuilds without holding up a gate. One sweep took the volume from 56 to
    168 GB free.

- ✅ **Each fork has its own check interval; gpui-kit is asked at every gate** (2026-09-27).
  gpui-kit lands several changes a day (four between one morning's sync and the afternoon:
  centred inputs and dialogs, a perf pass over six components), so a flat seven days let it
  drift a week behind a UI built on it. `xtask/upstream.toml` gives each fork
  `check_every_days`: 0 for gpui-kit, 7 for the zed fork and libghostty-rs. Once a fork's
  interval has run out, the gate asks its upstream for the branch head with `git ls-remote`
  (about 1 s, no fetch, given up after 5 s of a stalled link) and warns only when the head moved
  past `base`, naming the `sync --only` that follows it. An unreachable upstream falls back to
  the date warning.
  - **Not done.** Syncing from the gate: a sync rebases, pushes and moves pins, which is a
    change to read and gate on its own, not a side effect of checking another one.
  - **The same day's sync** onto upstream `5b55691e`: a single-line input centres in its frame
    and a dialog popup centres and keeps presses on it. Slopty draws its own dialogs and centres
    its field rows itself, so nothing here was a workaround to delete; the perf pass over six
    components (context menu, notification, accordion, searchable list) comes for free.

- ✅ **The gate skips what it already passed, and xtask builds apart** (2026-09-28, at the user's
  request to stop waiting on gates). Each lane records its inputs when it passes (the index's
  `mode sha path` lines, submodules at their pins, `rustc -vV`, the xtask binary's hash, the
  environment cargo and the tests read, the macOS/Xcode builds and tool binaries; for the tools
  lane also HEAD, the last tag and the day) under `target/gate/pass/`, and a lane whose inputs
  are identical is skipped: a no-op re-gate went from about 73 s to 2.5 s. Every lane still runs
  on the index snapshot, so what passes is exactly what is staged. fmt and the tool checks run
  first and stop the gate before any compile. `cargo gate --since-pass` tests only the packages
  changed since the tests lane last passed plus their dependents (one slopty-ui change: 72 s to
  22 s). The `gate` nextest profile retries the two session-actor tests that time a loaded
  machine, reporting a retried pass as flaky. `cargo xtask e2e --filter <filterset>` reruns one
  test (and accepts only the goldens it renders); `--no-build` reuses the last build. The `xtask`
  and `gate` aliases build xtask in `target/xtask`: in the shared `target/` it waited 2 min 05 s
  behind other builds just to start, apart it starts in about 1 s.

**A test that leaves a process behind fails, and CI gets its sccache.** ✅ 2026-09-29
- nextest's `leak-timeout` (500 ms, result `fail`) in the default profile, which every other
  profile inherits: a test whose child (ptyd, a worker, a shell) still holds its stdout or stderr
  half a second after the test ends is a failure, not a quiet orphan that eats a core. The whole
  suite ran clean with it (2,090 tests, 69 s) before it was turned on.
- CI and Deep had failed on every run since `.cargo/config.toml` began wrapping rustc in sccache:
  the runners had no `sccache`, so `cargo xtask setup` died before compiling. Both workflows now
  install it (`mozilla-actions/sccache-action`) and cache to the Actions cache
  (`SCCACHE_GHA_ENABLED`), rather than turning the wrapper off, so a runner also shares compiles.

**The pure crates forbid `unsafe`, and every library warns on an unreachable `pub`.** ✅ 2026-09-29
- Every library and binary with no `unsafe` of its own declares `#![forbid(unsafe_code)]` at its
  root. New `unsafe` in one of them then shows up in review as the removal of that line.
  `slopty-proto`'s zerocopy derives build under it, so it needed no `deny`. `slopty-tailnet` is the one `deny`: its IOKit and `getifaddrs` calls sit in one module
  that expects the lint.
- Every library crate declares `#![warn(unreachable_pub)]`, which the gate's `-D warnings`
  makes an error. The workspace keeps the lint at `allow` only because a binary's items are all
  unreachable by definition. A library item that nothing outside its crate can name is
  `pub(crate)`, so `dead_code` sees it. A reachable item that nothing uses is invisible to both
  lints, so the crates were also searched by name across the workspace, and what nothing called
  was deleted.
- clippy's nursery `redundant_pub_crate` contradicts it. For an item a private module shares
  with its crate, that lint asks for `pub` and `unreachable_pub` forbids it. Each crate that turns
  `unreachable_pub` on allows `redundant_pub_crate` beside it, with that reason, so `pub(crate)`
  is the one spelling. A module nested deeper shares with its parent as `pub(super)`.
- A method only its own crate's unit tests call is `#[cfg(test)]` and private (the client's
  `lines_in_flight`). An item another crate's tests use stays `pub`.

**A frame is encoded in the thread's scratch buffer and copied out at its size.** ✅ 2026-09-29
- `slopty_proto::codec::encode` serialises into a thread-local `Vec` it reuses and copies the
  frame into one exactly sized block. An echo went from 8 blocks to 1 and from 6 747 to 4 148
  instructions (MEASUREMENTS, "Encoding a frame"). The allocation budgets in
  `crates/slopty-engine/tests/allocs.rs` and `engine.encode_cost` hold it.
- Sizing the message first (postcard's `Size` flavour) also gives one block, but it serialises
  twice: 58 % more instructions for a full 200×60 screen. Copying 28 KB costs far less than a
  second walk over its cells.
- The buffer is kept only up to 64 KiB, which holds a full 200×60 screen with room to spare, so
  a thread holds no more than that. A bigger frame, such as a checkpoint, leaves as the buffer
  it grew, as before. A `Serialize` impl that encodes a frame while it is being encoded gets a
  fresh buffer instead of a borrow panic.

**gpui-kit moves to upstream `201b55a4`, GPUI to the fork's `f994c345`.** ✅ 2026-09-30
- gpui-kit's one new commit (#3320) takes `notify` 8.2 and keeps the theme watcher alive when
  a watched theme file is replaced rather than written. Slopty watches no gpui-kit theme and
  uses `notify` nowhere itself, so nothing changes here but the lock.
- gpui-kit takes GPUI from the fork's branch, so the pin moved with it to fork main `f994c345`:
  zed's GPUI-owned hang monitor (0f9c923e), upstream's hook rules (#19), and the fork's own
  "take a line's content mask once, not once a glyph".

**gpui-kit moves to upstream `3a142844` (fork `1cc5914d`), GPUI to the fork's `37b6e9a1`.** ✅
2026-10-01
- gpui-kit's ten commits rebased under our 37 without a conflict, and the sync's clippy and
  tests pass. Judged against Slopty:
  - #3343 sizes a single-line Input's root to at least its line (`min_h_full`). Before it, a
    line taller than the frame's content box was clipped at the top and bottom. That reaches
    every field Slopty draws.
  - #3330 puts back `rows` setting a plain Textarea's height.
  - #3329 has a `TextView` without a `.style()` take its container's text colour. Slopty
    passes its own style (`markdown::style`), so nothing changes here.
  - #3322 lets fenced code scroll inside a height cap. Slopty draws fenced blocks itself.
  - #3318 stops remeasuring a scroll table's columns, #3281 adds `source` and
    `range_for_source` to Markdown, and #3331 keeps a disabled radio disabled in a group.
  - The rest touches docs and the shell's QuickJS.
  - `slopty-ui` builds and passes clippy against the new kit with no edit.
- gpui-fast's `base` lagged at `1b381adb`, although the fork had merged longbridge `92a9b0f`
  (#24–#34) in its PR #6. The sync confirmed the fork holds it and that zed `74c134a3` changed
  nothing in the imported directories, and recorded both bases. Fork main `37b6e9a1` adds only
  formatting CI over `4afc87b`. The gpui-pre compat crates were still locked at `4afc87b` and now
  share the one gpui-fast source.

**The ghostty fork moves to upstream `26e64dfb`.** ✅ 2026-09-30
- Upstream merged #14458 (a live pending wrap is repaired after a resize), which the fork had
  carried as a copy since `0386095`. The rebase dropped the copy for the merged commit and kept
  the fork's own six on top: the five alt-screen commits and a new one for autowrap off. With
  DECAWM off, the next character overwrites the last cell, as in xterm, so the fork leaves the
  cursor there instead of moving it past the margin as #14458 does. That fix is drafted for
  upstream in `.research/ghostty-upstream-prs.md`, not opened.
- Pins: ghostty fork `741a800e`, libghostty-rs `8a452227` (bindings unchanged).
  `without_autowrap_a_resize_keeps_the_cursor_on_the_last_cell` in slopty-engine fails on the
  old pin and passes on the new one.
- **Adopted, with no code in Slopty:** the rest of the batch. It is a prompt-click fix in the
  app's `Surface.zig`, a GTK DPI warning, a renderer shader fix, list updates and an esctest
  harness; only #14458 touches what libghostty-vt builds from.
- The sync's lock refresh ran `git ls-files --error-unmatch Cargo.lock` in a repository without
  a lock and printed git's pathspec error. It now stays quiet, and the missing lock still means
  there is nothing to refresh.

**The zed fork moves to upstream `dd510f99`, gpui-kit to `25d59c06`.** ✅ 2026-09-29
- The zed fork now carries 33 commits on upstream `dd510f99`. The last sync had already
  rebased it onto `ee43be11` without moving `base` in `xtask/upstream.toml`, so 19 of the 27
  commits the check reported were new. One conflict, in `crates/gpui/src/window.rs`: upstream's
  `InputPreference` import landed beside the fork's `PresentedFrame` imports, and both were kept.
  No upstream commit touched `gpui_macos`, `gpui_apple` or `gpui_ios`, so the display-link,
  presentation and iOS patches replayed as they were.
- gpui-kit now carries its 7 commits on upstream `25d59c06`. Upstream's #3294 makes
  `TextViewState::set_text` append Markdown that extends the current text, which our
  "parse only the last block" commit already did. The merged `set_text` keeps our version and
  adds upstream's two guards. A parse that failed, or a text that is still empty, gets a whole
  parse again rather than an append, so an append can no longer land on a document that is
  missing text. It also keeps our MDX guard, the synchronous parse of a small append, and the
  whole parse when the text holds a definition or frontmatter. Upstream's four new tests pass on
  the merge (86 of 86 in `text::state`).
- **Adopted, with no code in Slopty:** the `set_text` guard above, which the conversation face's
  streamed answers go through. gpui's `PriorityQueue` receivers now end when the last sender
  drops instead of blocking, but only the threaded dispatcher uses it, not the GCD one on Apple.
- **Not adopted:**
  - `KeystrokeEvent::input_preference`: only Windows sets `prefer_character_input` (for AltGr),
    and every Apple backend passes `false`. Slopty's keystroke interceptor would always see
    `KeyBindings`.
  - gpui's bench kit: `CountingAllocator` in `bench_main!`, the seeded randomized element tree
    and `bench_text_system`. They are Criterion harness pieces behind `bench-support`. Slopty
    already budgets allocations in its own tests, and a frame is timed end to end on the real
    window, which a synthetic tree cannot stand in for.
  - gpui-kit's button focus lines (#3299, #3300). Slopty turns `focus_ring` off, but it uses no
    gpui-kit `Button` and draws its own focus hairlines. The kit `Input` takes a focus style
    only when it draws its own border, which is still just tinted, so no field changes.
  - Everything else: chart appear motion, `text` anchor summaries, `gpui_web`, `gpui_wgpu`, and
    the editor, agent and git crates. None of it is on a path Slopty builds.

**The zed fork moves to upstream `bd747337`, gpui-kit to `2ec5696c`.** ✅ 2026-09-29
- Both rebased with no conflict. Zed's one new commit drops the language extension's special
  case for old TOML and Zig extensions, which Slopty does not build. gpui-kit's three are the
  plot appear scope, keyboard focus on `DataTable` with menu highlights kept on key presses
  (#3307), and the gallery font subsets. Slopty uses none of `DataTable`, the kit menus or
  `plot`, so nothing is adopted.

**The app, the UI and the tools warn on an unreachable `pub` too.** ✅ 2026-09-29
- `slopty-app`, `slopty-ui` and `slopty-tools` declare `#![warn(unreachable_pub)]` with the
  `redundant_pub_crate` allow beside it, as the other libraries do. `slopty-app` already forbids
  `unsafe`; `slopty-ui` cannot, because `screen` wraps a `CVPixelBuffer`.
- In `slopty-ui`, `screen` and `workspace` expect `unreachable_pub` for now. Narrowing them
  meant editing `screen/{health,touch,zoom}.rs` and `workspace/{desktop,tile}.rs`, which the
  streaming work was changing at the same time. The `expect` fails once nothing there trips
  the lint, so it cannot outlive that work. Everything else in the crate is linted, and the
  non-streaming files under `workspace/` are already narrowed.
- The CLI and the other binaries stay without it. Nothing outside a binary can name its items,
  so `dead_code` already sees every one of them, and the lint would only respell `pub` as
  `pub(crate)`.
- A view accessor only its own crate's tests read is `#[cfg(test)]` and private, or
  `pub(super)` when the tests sit in a sibling module (`toast_texts`, `navigator_filter`,
  `palette_open`, `relay_notice`, `url_at` and others).
- The accessors the self-test socket reads (`toast_text`, `reading_line`, `live_page`,
  `upload_on`, `tile_bounds`, `pick_window`, …) stay in every build. `slopty-app` compiles its
  `e2e` module in every build so that the gate's clippy, which runs on default features, lints
  it. Gating the accessors on the `e2e` feature would mean gating that module too, and then no
  gate lane would compile it. Without the feature, nothing calls them and the linker strips
  them.
- The token lint-as-tests in `kit.rs` read each file up to its first `#[cfg(test)]`. So one
  test-only accessor high in a file hid the rest of that file from them: 61 of the 7,755 lines of
  `terminal/view.rs` were read, and 73 of `workspace.rs`. They now stop only at a
  `#[cfg(test)]` over a `mod`, which reads 59,433 lines of the UI and the app where they read
  46,594. The added lines all passed.

**GPUI comes from the gpui-fast fork, which imports zed itself.** ✅ 2026-09-29
- The user dropped the zed fork for `aislopware/gpui-fast`, a fork of longbridge/gpui-fast (GPUI
  imported flat out of zed with no shared history, plus Retained Mode). longbridge takes a newer
  zed only now and then (it sat on zed `7960b2a7` of 2026-09-12 while zed was 51 commits further
  in the tracked directories), so the fork does it too, and is never behind zed.
- `cargo xtask upstream sync` takes longbridge's branch first and zed second, so whatever
  longbridge already imported is never imported again. The zed step follows gpui-fast's
  `docs/upstream-sync.md`. It builds a vendor commit on the last one (`import_commit`) whose
  tracked directories are zed's at the new head, byte for byte. The commit is made with a scratch
  index and work tree, and the tool compares `git ls-tree` of both sides before it commits. It
  merges that commit with `UPSTREAM` rewritten, then runs `script/check-upstream` and the build
  checks before it pushes. Crates that join the tracked set are the path dependencies (normal,
  dev and build, any target) of the tracked crates, followed through zed's
  `[workspace.dependencies]`. On zed `bd747337` this finds the same change the hand import did:
  `bench_metrics` in, `media` out.
- It stops mid-merge, with `UPSTREAM` staged and a list, when an import needs a hand: conflicts,
  a file zed changed that a `#[path = "fast/…"]` redirect replaces (the merge cannot carry that
  change), or a crate the workspace manifest must add or drop. Committing the merge finishes it,
  and the next `sync` carries on.
- gpui-fast merges longbridge's branch rather than rebasing onto it. The vendor commits sit beside
  the fork's branch and reach it through merges. A rebase would drop the merges and replay the
  vendor commits onto longbridge's tree, leaving `import_commit` pointing at a commit the fork no
  longer has. gpui-kit and libghostty-rs still rebase. The fork's pushes are now fast-forwards.
- The gate watches gpui-fast's upstream at every gate and zed weekly, with `git ls-remote`.
  `upstream.toml`'s `[zed]` base is the zed commit the fork is known current with.
- Not done: taking only up to what longbridge imported. The fork follows zed's `main`, as the zed
  fork did.

**Slopty draws under gpui-fast's view retention, pinned at `6d80f2d` with gpui-kit
`d24129e4`.** ✅ 2026-09-29
- Why the fork. It draws a view again only when the view was told of a change or something it
  read changed, and replays every other view from the last frame. Slopty's frames are mostly
  that case: a strip of tiles where one shell echoes, one stream shows a frame or one spring
  moves. It also composes native views and layers into the frame (a web view, a video layer,
  ordered and clipped with what GPUI paints around them), which the browser tile now does by
  hand with a web view over the Metal view; adopting it is later work.
- What it asks of Slopty. A view that shows a state must hear of it, and must not read what
  changes more often than it does. The strip and the chrome became views that build from a read
  of the workspace and read copied facts rather than the bodies (`docs/decisions/ui.md`, "The
  strip and the chrome read facts, never a tile's body"), and a value measured while drawing is
  sent as a notify after the frame. The switch audit's findings each have that fix.
- Evidence. On the same binary with retention turned off (`GPUI_VIEW_RETENTION=0`), the strip's
  p95 draw is 3.1 ms against 2.1 and its p99 5.6 against 2.3; against the zed fork before the
  switch, an echo's p50 is 0.3–0.4 ms against 1.6 (MEASUREMENTS, "UI frames on gpui-fast,
  retention on and off").
- How it is kept honest. `retained::stale` draws the same state from scratch and diffs the
  painted quads and sprites against the frame shown; the headless steps and every e2e dump run
  it, and the e2e harness draws only through notifies, so a golden is the frame the app draws
  (TESTING.md, "Retained frames").
- Left open: the fork has no per-draw hook, so the frame probe is a root view that reads a marker
  its own paint writes, and is built in every frame for that alone. gpui-kit's text input
  writes its state in every render, which counts as news for every view that reads that state.

**gpui-fast's scroll layers stay compiled out on Apple.** ✅ 2026-10-01, superseded the same day
by "Scroll layers are on for macOS and iOS: the navigator and the face composite"

*What changed.* longbridge's scroll layers (#24 with #25 Metal and #27 list rows, #26 with #29)
are merged into the fork. `fast::layers::COMPILED` is off on macOS and iOS. The fork's Metal
fixes for them landed with the merge: tiles drew nothing in frames without paths, debug bounds
were lost, tile scenes were built once per tile, and layers were promoted while their owner
animated.

*Why.* Correct is not enough; layers are not a net win for Slopty today.

- Where they win, the Metal composite is cheaper. In gpui_perf, `list-uniform-scroll` goes
  from 3.4M to 1.55M instructions a frame. On the GPU, a composited frame takes 227 µs against
  274 µs drawn directly.
- Slopty's lists never composite. Measured over 600 wheel frames:
  - the conversation face goes from 4.17M to 4.32M instructions a frame (it was +20% before the
    animation fix);
  - the navigator goes from 4.06M to 4.08M.
- The cause is in Slopty. The view holding each list reads an entity in its render that is
  written while the window draws every scrolled frame. For the face that is most likely its
  prompt rail. So every composited frame is a repaint until the layer is demoted.

*What it asks of Slopty.* To turn layers on, those writes must stop during a scroll.
`the_navigator_draws_only_the_rows_in_view` must also allow a list layer's overscan rows.

*A cost that stays.* #24's conservative glyph mask test costs the terminal strip 2 to 2.7%
instructions a frame (`strip-scroll` 7.71M to 7.91M), whether layers are compiled in or not.

**Scroll layers are on for macOS and iOS: the navigator and the face composite.** ✅ 2026-10-01

*What changed.* The fork's `fast::layers::COMPILED` now names macOS and iOS (aislopware/gpui-fast
#9, pinned at `f71b3fe`), which share the Metal renderer. Every scroll container in the app is
now eligible. Before the switch, neither list people scroll most ever composited, so layers on
Apple only cost: the face paid 7%. Four fixes got there, each measured in 8 alternating rounds
(MEASUREMENTS, "scroll layers on Apple: gpui-fast f71b3fe, gpui-kit 25b62b08"):

- **The navigator's render read its list's scroll offset.** `NavList::set_rows` read
  `logical_scroll_top` on every render, just to keep a list at its top when rows are spliced in
  above it. A render that reads the offset makes every scroll a change of the content. It now
  reads it only when it splices, and 500 of 600 wheel frames composite.
- **The selection plate was a canvas under the list.** With the plate there, the background
  under the viewport was not one opaque quad, so the layer never baked (`defer_unbaked`). Now
  `Plate::seat` paints the plate inside the selected row while it is still, and the glide still
  paints under the list while it moves. 600 of 600 frames composite, and a frame goes from
  0.343 to 0.221 ms.
- **A GPUI Kit text view changed its state when it was first built** (aislopware/gpui-kit#3,
  pinned at `25b62b08`). Its first parse notified inside the holder's render, and the parser's
  acknowledgement made a no-op update. Every row a pan uncovered therefore changed the face's
  content. With the fix, a finished face panned at rest goes from 0.479 to 0.4215 ms. It
  composites every frame except those of the composer's 160 ms morph, during which a holder
  asking for animation frames is not promoted.
- **A layer-on panic in the fork** (#8). When the navigator's holding view was copied from the
  last frame, the shift of its rows' paint ranges subtracted the window's scene index from the
  layer's own and overflowed. This is debug only.

*Step 4 found nothing to fix.* With the fixes in, a profile of the face panning with layers on
puts the layer bookkeeping at 0.8% of the thread. Panning and following move +1.7% and +0.8%,
inside a round-to-round spread of ±15%. Every other frame, the 28 terminal cases included, stays
within noise.

*What keeps it true.* `a_scroll_of_the_navigator_composites_its_layer` and
`a_face_panned_at_rest_composites_its_layer` assert that every frame composites and nothing is
demoted. The face test runs under Reduce Motion, so the wall-clock morph does not count. A render
that reads a scroll offset, a canvas under a list, or a component that writes its own state while
it is drawn fails one of them. `cargo test -p gpui_apple fast::layers` checks the Metal composite
pixel for pixel.

*The tooling.* `target/scratch-layers` holds the scratch tree, its scripts (`refresh.sh`,
`build.sh`, `suite2.sh`, `table.sh`) and `trace5.patch`. That patch prints each frame's decision
under `GPUI_LAYER_TRACE=1`, with the check that failed. All of it lives under `target/`, which
`xtask prune` may clear, and none of it is meant to land.


*Not candidates.* The terminal's scrollback is a custom element that moves its own grid, the
strip's motion is a layout spring, and a screen tile holds a surface. None of them is a scroll
container, so they gain nothing from layers.

**Native views compose through the fork's own mechanism, not longbridge #30.** ✅ 2026-10-01

*The two mechanisms.*

- **The fork's.** It places natives under GPUI's single drawable and cuts antialiased holes in
  painter's order. So anything drawn after a native is above it: the palette, menus, toasts,
  focus rings and headers. Clipping, rounding, fade, hit testing and focus come from GPUI's
  frame, and a present is transactional only when a native changes.
- **longbridge #30 (zed#62379).** It stacks a base drawable, the native, and one extra
  full-window `CAMetalLayer` for overlays. Only deferred and window-level draws go above the
  native. The app places natives by hand, and every frame draws the overlay surface, empty or
  not.

*The numbers.* Measured on a 3024×1964 window, per frame
(`composition_overlay_gpu_cost` in `gpui_apple`):

| Case | Fork, GPU | #30, GPU | Fork, CPU instructions | #30, CPU instructions |
|---|---|---|---|---|
| Palette over a browser tile | 1.23 ms | 1.81 ms | 90K | 142K |
| Palette over a remote screen | 1.40 ms | 1.78 ms | 90K | 142K |
| Palette closed | 0.75–0.93 ms | 0.72 ms | 80K | 129K |

With the palette closed, #30 saves the hole's blend but still pays 50K more CPU instructions.
Each overlay surface also holds up to 71 MB of drawables, and the WindowServer composites one
more full-window layer.

*What Slopty keeps.* `native_view` and `VideoLayer` stay as they are.

- ✅ **A golden holding a `serde_json::Value` puts its keys in order** (2026-09-29).
  `golden__ctl__ctl_reply_permission_answer` passed in the workspace and failed under
  `cargo test -p slopty-proto`: whether a `Value` keeps the order its keys were written in is
  `serde_json`'s `preserve_order`, which the workspace's tests get through feature unification
  and the crate alone does not. Declaring the feature in `slopty-proto` would have fixed the
  test by shipping it in the daemons and the CLI, which build without it, against the rule
  above that the tests' features stay out of shipped builds. The ctl goldens instead sort every
  object's keys before encoding (`sorted` in `tests/golden.rs`), so both runs write the same
  line.

- ✅ **CI runs the gate one lane per runner, and skips only what a runner's hardware lacks**
  (2026-09-29). CI had failed on every push since at least 2026-09-13. Until 7b0e09a0 the
  runner had no sccache for the rustc wrapper, so every run died within minutes, at the first
  `rustc -vV`. The first run with sccache, 36533060670 (68fe132a), ran the whole gate on one
  `macos-26` runner (three cores, 7 GB) for 79 minutes. The five lanes compiled side by side there, each on its own
  target dir: nextest build 53 min, host clippy 59, the iOS and Linux clippy 66 and rustdoc 68.
  Its tests then failed. sccache hit 2 of 3 490 compiles because the Actions cache was empty.
  The second run, 36558264642 (79734c2a), took 67 minutes and hit 894 (25.6 %). The zed and
  gpui-kit forks had moved between the two commits, so their crates and everything above them
  missed. The hits are what did not change. The cache works; a gate that sits on the zed fork
  misses whenever the fork moves.
  - Each lane runs on a runner of its own: a matrix job per lane runs
    `cargo xtask setup --lane <lane>` (that lane's tools only; the tools lane alone builds taplo,
    which has no binary for binstall) and `cargo xtask gate --ci --lane <lane>`. A lane alone
    sets no `CARGO_BUILD_JOBS` and takes every core. It keeps its target dir name, and sccache
    leaves `CARGO_BUILD_JOBS` out of its key, so local lanes and CI lanes keep their cache
    entries. `--ci` checks in place and gives the tests lane nextest's `ci` profile. With no
    `--lane` the local gate is what it was. `rust-cache` is keyed per lane and saved on failure
    too, so the registry and the installed tools survive a red test.
  - The `ci` profile's `default-filter` leaves out three tests the runner's virtual Mac cannot
    pass. `slopty-testkit`'s `a_series_reports_per_operation_and_as_json` and
    `this_process_reads_back` read retired instructions, and the guest has no performance
    counters (`Usage { instructions: 0, cycles: 0, … }` in both runs).
    `this_mac_says_whether_it_wakes_on_lan` reads `womp` from `pmset -g`, which the guest does
    not have. They are listed as skipped there and still run in every local gate. Only missing
    hardware earns a place on that list, never timing, and no test gets a retry for CI.
  - Found on the runner and left to their owners, since they are not missing hardware.
    `a_full_chroma_stream_arrives_as_444_and_follows_the_rate` streams the host's first
    display at a quarter scale. The runner's display is 1024 × 768, so the stream is 256 × 192,
    whose leave line (`full_chroma_band`) is under the 1.2 Mbit/s that eight cuts from 12 Mbit/s
    reach, and 4:4:4 never falls back. `quality_changes_decode_without_a_refresh` saw one
    refresh with nothing lost or broken (the guest has no `AppleM2ScalerParavirtDriver`). In the
    first run, `shell_round_trip_over_quic` took a `Caps` before `SessionOpened` (fixed in
    79734c2a) and `viewers_joining_a_busy_session_never_make_the_others_resync` missed its 10 s
    deadline under the five lanes' load; it passed in the second run.

- ✅ **target/ stays under a byte budget and its volume above a free-space floor, in both of
  cargo's layouts** (2026-09-30). On 2026-09-29 `target/` reached 310 GB and the Lacie volume
  was 98 % full: `debug/incremental` alone held 161 GB in 3 887 caches and `debug/deps` 95 GB.
  The one-day idle sweep above could not keep up with several agents and five gate lanes
  building all day, and `cargo xtask prune --idle-hours 6` freed 274 GB by hand. `xtask prune`
  now does the following, under cargo's locks except for a busy directory's caches:
  - **The budget and the floor.** After the idle sweep, when `target/` holds more than the
    budget (160 GB, `SLOPTY_TARGET_BUDGET_GB`) or its volume has less than the floor free
    (50 GB, `SLOPTY_DISK_FLOOR_GB`), it deletes units and incremental caches least recently used
    first, until both hold with a tenth to spare. Nothing used in the last hour goes, since that
    is the build that just ran and the tests about to run from it; what that leaves short is
    reported. Why these numbers: units and caches used in the last 24 hours came to 129 GB
    (37 GB in the last hour, 103 GB in the last six) and the gate's lanes to 36 GB of it, so 160
    GB keeps a day's work with room for one toolchain or fork bump. A cold full gate writes
    about 36 GB, so a gate that starts above 50 GB free finishes. Numbers: MEASUREMENTS
    "target/ under a budget".
  - **The gate refuses to start under the floor.** It first runs the budget pass (without
    waiting on busy directories); if the volume is still under the floor, it stops and prints
    the free space, the floor and the largest entries under `target/`, with what to delete. It
    names a busy directory only when that build held something the pass would have deleted, so
    "busy" in the refusal is the reason it fell short. A full disk would otherwise fail a lane
    halfway.
  - **A busy directory still loses its caches.** Agents build in `target/debug` all day, so its
    cargo lock was almost never free and the pass skipped it whole. On 2026-09-30 the gate
    refused on the floor with `debug: busy, skipped` while `debug/incremental` held 70 GB
    untouched for over three hours; deleting them by hand freed 62 GB. In a directory a build
    holds, the pass now keeps the units and object files (the build may link any unit's rlib)
    but deletes idle caches, and caches the budget picks, one session at a time as rustc's own
    collector does (`rustc_incremental::persist::fs`, `garbage_collect_session_directories`).
    Each session `s-<time>-<random>-<svh>` (`-working` while a compile writes it) has a lock
    file `s-<time>-<random>.lock` beside it. A compile holds it exclusively while it writes and
    shared while it reads. The pass takes it exclusively without waiting, deletes the session
    and then the lock file, and skips and counts a session it cannot lock. As in rustc, a
    session with no lock file is debris and goes, and so does a lock file with no session,
    under its lock. rustc 1.98 and 1.99 lock with `fcntl(F_SETLK)` on macOS; nightly locks with
    `flock` (std's `File::try_lock`). Darwin keeps both kinds in one lock list, so one `flock`
    attempt sees either (checked on this Mac across two processes; a test holds each kind).
    The crate's cache directory stays even when empty, because rustc creates it and then its
    lock file inside, and a directory removed between the two fails that compile. Names rustc
    does not write, and directories the pass may not read, stay without stopping the pass. The
    busy directory's units are only read, to count what the build held back for the refusal;
    its ledger and access times are left alone. The report reads `debug: busy, units kept; 94
    caches swept, 3.4 GB`, and "busy" now means only that the units were not pruned. By hand,
    `cargo xtask prune` still waits for a busy directory.
  - **The evidence is a ledger.** Each pass records every unit's last use in `.xtask-prune`
    and sets the fingerprint files' access times back to the epoch, so the next read stamps them
    again (APFS stamps an access time only while it is older than the modification time; a test
    checks that on this Mac). The old code reset them once a day, which gave one timestamp per
    unit per day, too coarse to rank for a budget. A directory with no ledger yet deletes no unit
    for idleness, and starts every unit's clock at that pass.
  - **The object files of earlier compiles.** On macOS a test or binary keeps its debug info in
    `<stem>.<cgu>.<invocation>.rcgu.o` files beside it, and every compile writes a new
    invocation's set without deleting the last one. They were 99 782 of the 102 568 entries of the
    tests lane's `deps/` and 132 805 of `debug/deps`. The invocation holding a unit's newest file
    is the one the binary links (checked against the binary's `N_OSO` entries with `nm -ap`); the
    others go. A tie keeps both, since an object reused from the incremental cache keeps its old
    time. Only a unit compiled since the last pass is looked at. The first pass deleted 134 510
    files (19.7 GB).
  - **Cargo 1.100's layout.** Cargo 1.100 (2026-11-12) "now uses a new directory layout for
    intermediate build artifacts", and the build-cache page says "The build-dir layout was
    changed in Cargo 1.100.0" (https://doc.rust-lang.org/nightly/cargo/CHANGELOG.html,
    cargo#17354; https://doc.rust-lang.org/nightly/cargo/reference/build-cache.html). A build
    with nightly 1.101 on 2026-09-30 shows it: each unit is `<profile>/build/<package>/<hash>/`
    with `fingerprint/`, `out/` and, for a build script's run, `run/`. There is no `.fingerprint`
    or `deps/`, and `incremental/` is where it was. `target/deep/realtime` already has this
    layout, since that lane builds on nightly. Prune reads both, finds profile directories by
    `.cargo-lock` rather than `.fingerprint`, and stops with an error on a directory that is in
    neither layout, rather than pruning nothing. Since cargo 1.96 a build holds `.cargo-lock`
    shared and `.cargo-build-lock` exclusive ("Split build-dir concurrency file lock into a
    dedicated lock while keeping a shared lock on `.cargo-lock`", cargo#16708). Prune takes both
    exclusively, and when it waits it waits on one at a time while holding none, so it cannot
    deadlock against either order. The repository sets no `build.build-dir`, so every build
    directory is under `target/`.
  - Tests: `xtask/src/prune/tests.rs` builds fixture profile directories in both layouts with
    staged times. It covers the idle sweep, the first pass, the objects, the budget's order and
    its one-hour guard, the floor and the gate's refusal, a held lock of either kind, a dry run
    and an unknown layout. In a busy directory it covers the session-by-session sweep, including
    a session locked by `flock` and one locked by `fcntl` from a child process (a process drops
    its own `fcntl` locks on a file when it closes any descriptor of it). It also covers the
    budget taking a busy directory's caches while its units stay, and odd entries in
    `incremental/` that stay without stopping the pass. No test runs cargo.
  - Not done: moving the gate's lanes to the internal disk (next entry).

- ✅ **A test binary's directory, not the volume, is what slowed `VideoToolbox`; the gate's lanes
  stay on the repo volume** (2026-09-30). This supersedes the cause in `testing.md`'s 2026-09-15
  entry ("The repo's volume must be mounted with ownership on"). The plan was to move the gate's
  lanes to the internal disk, which would also have taken 36 GB off the full volume. Measured
  first, one variable at a time, on `hevc_encode_then_decode`:
  - The gate's own binary, run in place from `target/gate/tests/debug/deps`, took 2.6–6.5 s. A
    copy on the internal disk took 0.43–0.69 s. But a copy in a small directory on the same
    external volume took 0.37–0.50 s, and so did a hard link of the same inode (0.40 s).
  - A copy on the internal disk, in a directory holding 100 000 empty files, took 2.0–6.2 s.
  - The codec's whole suite under nextest took 2.7–3.3 s from a `deps/` of 1 092 entries on
    either volume and 25–36 s with 100 000 extra entries, on either volume (11 092 entries:
    3.6 s; 31 092: 6.3 s).

  So a process that opens `VideoToolbox` or `CoreAudio` pays for the size of its executable's
  directory. The mount flag and the disk do not matter. The 2026-09-15 comparisons each moved
  the binary into a small directory as well (`/tmp`, a fresh disk image), which is the variable
  that counted. `deps/` holds every test binary beside every artifact and every compile's object
  files. The gate therefore stays on the Lacie volume, and the fix goes where the cost is:
  - `cargo xtask test-runner` is cargo's target runner in the gate's tests lane and in `xtask
    check` (`CARGO_TARGET_AARCH64_APPLE_DARWIN_RUNNER`, set only for nextest). It runs a `deps/`
    binary through a hard link in `<profile>/run/`, the same file in a directory of one entry per
    test binary. `current_exe()` stays one level below the profile directory, which
    `ptyd_link.rs` relies on, and the debug info still points into `deps/`. The runner itself is
    a hard link of the xtask binary beside it, swapped in atomically, so another session
    rebuilding xtask mid-run cannot leave nextest without it. The codec suite with 101 092
    entries: 4.8–5.4 s through the runner, 33.2 s without.
  - Prune deletes the objects of earlier compiles (entry above), which took the tests lane's
    `deps/` from 102 568 entries to 62 296. That alone still leaves the binary at 3.4–5.5 s,
    which is why the runner exists.
  - From Cargo 1.100 each test binary sits in its own unit directory and the runner leaves it
    where it is.
  - `sudo diskutil enableOwnership` is no longer the advice: it would not have helped.

- ✅ **`XProtect`: `cargo xtask doctor` measures the first-launch scan, and only the user can
  switch it off** (2026-09-30). macOS scans each new executable on its first launch, and
  `XprotectService` runs "in a single thread, so if you try to launch 10 new binaries at once,
  the slowdown will be more than a second" (cargo#15908, "Enabled | ~180ms" against "Disabled |
  ~9ms"). nextest's install page says "For optimal performance, add your terminal to Developer
  Tools", and adds that a multiplexer must be listed itself
  (https://nexte.st/docs/installation/macos/). A full tests lane launches about 100 new test
  binaries, and a build script is new after every `cargo update`.
  - `cargo xtask doctor` (and `setup` at its end) compiles three binaries with constants no
    binary had before and times each first launch against its second. It names what to list:
    the outermost app among its ancestors, or else the outermost process, such as a multiplexer
    server that launchd started. It prints the exact path in System Settings. Here, from
    herdr, the median was 261 ms a binary; the switch was not on.
  - It changes no setting. Developer Tools is the user's to switch on for the app it names.

- ✅ **`RealtimeSanitizer` on the audio render callback, in the nightly lane** (2026-09-30).
  `render` in `slopty-codec/src/audio.rs` is promised to be wait-free: no lock, no allocation,
  no system call. No functional test can see a break of that promise, which is heard as a
  crackle. Nightly's `-Zsanitizer=realtime` aborts on any of them in a function marked
  `#[sanitize(realtime = "nonblocking")]` ("Functions marked with the
  `#[sanitize(realtime = "nonblocking")]` attribute are considered real-time functions",
  https://doc.rust-lang.org/nightly/unstable-book/compiler-flags/sanitizer.html). It works on
  aarch64-apple-darwin without `-Zbuild-std`, since its runtime intercepts `malloc` and the rest
  whatever calls them.
  - `cargo xtask deep sanitize realtime` (and the nightly run's `sanitize-realtime`) builds the
    codec with `--cfg slopty_rtsan`, which marks `render` and turns on
    `#![feature(sanitize)]`. Stable never sees either (`cfg(slopty_rtsan)` is declared in the
    workspace's `check-cfg`).
  - The proof it watches the callback: under the cfg, a test arms a probe that makes the next
    render allocate and runs it in a child process. The child must die of RealtimeSanitizer's
    `malloc` report naming `render` ("Intercepted call to real-time unsafe function `malloc` in
    real-time context! … slopty_codec5audio6render audio.rs:1310"). The real callback passes
    under it: the ring's tests render through it, and so does `player_starts_and_drains` on the
    device's I/O thread.

- ✅ **ghostty is a synced fork: `upstream sync` rebases it, re-pins the binding and moves
  `vendor/ghostty`** (user, 2026-09-30). `vendor/ghostty` now follows `aislopware/ghostty`,
  whose `main` carries our five alternate-screen commits on ghostty-org `main` `0538f7535`, and
  libghostty-rs fetches the same fork at `GHOSTTY_COMMIT`. A bump used to be by hand: move the
  submodule, edit the pin, run `gen-bindings`, commit, push, then `sync --only libghostty-rs`.
  The user wants every upstream taken continuously and read before it lands, and ghostty-org
  lands about 14 commits a day (1691 on `main` from 2026-06-01 to 2026-09-29).
  - `[ghostty]` in `xtask/upstream.toml` makes it a fifth source (strategy `rebase`), and
    `--only ghostty` brings libghostty-rs, which pins it. The rebase runs in a linked worktree,
    `.research/ghostty`, never in `vendor/ghostty`: that is `GHOSTTY_SOURCE_DIR` for every build
    in the checkout, and a conflict there would leave markers in all of them. A conflict stops
    the sync with the file list, and nothing has been pushed or moved yet.
  - Every step is checked on this machine before anything leaves it (`plan` in
    `xtask/src/upstream.rs`, with a unit test of the order). libghostty-rs is rebased, its pin
    rewritten (the build script must fetch from the fork, and the id must be a full one), the
    bindings regenerated from the rebased tree and committed. Then `cargo check` and
    `cargo test -p libghostty-vt` run with `GHOSTTY_SOURCE_DIR` on that tree. After that it
    publishes in the order the pins need, confirming each step: the ghostty fork's push (read
    back with `ls-remote`), `vendor/ghostty` moved detached to that head, libghostty-rs pushed,
    `Cargo.lock` moved and read back. A pin never names a commit its repository lacks.
    `--no-push` stops before publishing and leaves `vendor/ghostty` and the lock alone.
  - `paths` sets how often it asks. It names what libghostty-vt builds from: `src/terminal/`,
    `src/lib_vt.zig`, `include/ghostty/` (the bindings' input), `src/simd/` and
    `src/unicode/` (the parser's and grid's hot helpers), `src/input/` (the key and mouse
    encoders Slopty calls), `src/build/GhosttyLibVt.zig` and `build.zig.zon` (the Zig version
    and dependency pins). A third of upstream's commits touch these paths: 567 of the 1691, on
    93 of 121 days. The rest are the macOS and GTK apps, the renderer, fonts and config.
  - The gate asks once a day (`check_every_days = 1`). When the head moved, it reads GitHub's
    compare and warns only if a changed file falls under `paths`. A bump is a Zig rebuild plus
    a read of what moved and the terminal benches, so a day's batch (about three commits in
    September) is the unit worth one review. At every gate it would ask for a sync after each
    commit; at a week, a batch would be too big to read. `watch` coalesces moves by its interval and
    prints a ghostty head move only when the commits since its last look touch `paths`. The
    line names the files. A pull request is printed only when its files do. A file list that
    reaches GitHub's cap (300 for compare, 100 for `gh pr list`) counts as touching, since the
    files past the cap are unknown, and so does a compare GitHub does not answer.
  - `check` lists the upstream commits since the base that touch `paths`, one line each, and
    says where `vendor/ghostty` stands against the fork head and the commit this repository
    records. The review starts there. A commit in `src/terminal/` or `src/simd/` is measured
    with `cargo xtask bench --filter engine` before its bump lands.

- ✅ **The audio ring is model-checked with loom** (2026-09-30). The ring between the decoder's
  thread and the device's render callback (`crates/slopty-codec/src/audio.rs`, `Ring`) takes no
  lock. Its samples are relaxed cells, `write` is published with a release store, and the device
  and a mute both stop the run word with a compare-and-swap. A missing release or acquire there
  shows only as a click or a torn sample on some interleaving. A test that runs the two threads
  for real cannot be relied on to hit one, and ThreadSanitizer reports no race on atomics. So
  `audio::ring_model` runs the ring's own code on loom's atomics (`--cfg slopty_loom`, with a
  ring of four frames and a one-frame fade). `cargo xtask deep loom` explores three scenarios
  exhaustively, with no preemption bound: a run filling while it plays, a mute racing a render
  and the next run's fade-in, and running dry racing a mute. It checks every played sample
  against where it came from. Why loom, checked on 2026-09-30:
  - It is the only exhaustive checker for Rust that runs on this Mac and lets a load read a
    stale store (any of the last seven of each atomic), which is where these bugs live.
  - Shuttle models every atomic as `SeqCst`, so it cannot see a missing release.
  - Kani compiles concurrent code as sequential.
  - Miri's GenMC mode is more faithful, but it runs only on Linux, from a source build of Miri.
  - Miri's `-Zmiri-many-seeds` samples interleavings at random. It also needs a test that runs
    both threads without CoreAudio, which the codec does not have.

  Loom's own limit is that it never reorders a thread's own relaxed operations. The release store
  that publishes `write` makes that moot here. The model was proven against three planted bugs,
  and each was caught within milliseconds:
  - `write` published relaxed: a torn frame played.
  - A run started relaxed: its front played before its fade-in, a click.
  - The device not checking `run` again after its copy: the next run played unfaded after a mute.

  The three scenarios take 61 s (MEASUREMENTS, "Deep checks widened, a stream soak, a loom
  model"). The cfg is our own, because a global `--cfg loom` switches tokio's and other crates'
  internals to loom as well.

- ✅ **Bundles are signed with a stable identity** (2026-10-01, readiness audit item 15).
  `cargo xtask bundle` signed ad hoc unless `--sign` named an identity, and nothing named one.
  An ad hoc signature's designated requirement is the binary's hash, so every update of a Mac,
  ours or a remote one where a deploy copies the daemons to `<data dir>/bin`, was a new program
  to TCC. Someone had to be at that Mac to grant Screen Recording and Accessibility again.
  - **Ruling.** A bundle is signed with `--sign`, else `$SLOPTY_SIGN_IDENTITY`, else the
    keychain's Developer ID Application certificate, as `cargo xtask sign` already picks one
    (`sign::resolve_identity`). Here that is the one Developer ID in the keychain, team
    UK58J62H8L. Each daemon is signed under its `LaunchAgent` label as identifier
    (`dev.aislopware.slopty.worker`, `.ptyd`, `.server`, and `.cli` for the CLI); the app is
    signed as the bundle, `dev.aislopware.slopty`. The designated requirement is then the
    identifier and the team's certificate, which every later build of either shares, wherever
    the binary runs from. The dev daemons `xtask sign` signs carry the same identifiers, so a
    grant given to one is the other's too. A real identity signs with a secure timestamp and the
    hardened runtime, which notarisation requires; no entitlements are needed, since nothing is
    sandboxed, JIT-compiled or loads another team's library. The bundle step fails unless the
    worker's requirement names its identifier and `certificate leaf[subject.OU]`, and no
    `cdhash`.
  - **Without the identity.** With no Developer ID certificate, or `--ad-hoc`, the bundle is
    signed ad hoc and the step says so: it runs, and each update asks again for both grants.
    An Apple Development certificate is not taken, because it expires within the year and its
    grants with it. A person building Slopty for themselves can use their own Developer ID
    through `$SLOPTY_SIGN_IDENTITY`: grants then follow that team.
  - Tests: `bundle::tests::each_binary_is_signed_under_its_identifier` (the arguments for an
    identity and for ad hoc) and `a_stable_requirement_names_the_identifier_and_team`; the
    requirement check itself runs on every signed bundle.

- ✅ **A release is what `cargo xtask dist` builds** (2026-10-01, readiness audit item 8).
  The release job tarred `slopty-worker` and `slopty-ptyd` for macOS and nothing else: no app,
  server, CLI or Linux build, and nothing signed, while every install needs the CLI and the app
  is how a person starts.
  - **What it builds.** The bundle (above), with the Linux workers and servers inside it; then
    `Slopty-<v>-macos-arm64.zip` (`ditto`, which keeps the signatures), the Mac CLI, daemons
    and server as a tarball, a worker and a server tarball per Linux CPU, the dSYMs and
    `SHA256SUMS`, under `target/dist-out`.
  - **It checks what it built**, from the files: the bundle's signature (`--strict --deep`),
    every Mac binary arm64 (`lipo`), every Linux binary's ELF machine for its CPU, no
    `GLIBC_` version past 2.28 in a worker, none at all in the static server, and every archive
    listing something.
  - **Notarisation when it can.** With a real identity and `notarytool` credentials
    (`SLOPTY_NOTARY_PROFILE`, or an App Store Connect key in `APPLE_API_KEY_PATH`,
    `APPLE_API_KEY_ID` and `APPLE_API_ISSUER`) it submits, waits, staples and asks Gatekeeper
    (`spctl --assess`). Otherwise it says which is missing and goes on: an un-notarised app runs
    after the person confirms its first open in System Settings.
  - **CI.** The `release` job runs `cargo xtask dist --out dist` for a gated tag, with a keychain
    of its own holding the Developer ID from the `MACOS_CERTIFICATE` secrets and the notary key
    from the `APPLE_API_KEY` secrets; without them it builds ad hoc and the run's summary says
    so. It installs the Linux targets, zig and cargo-zigbuild for the cross-builds. Publishing
    stays a tag's: nothing is uploaded otherwise.
  - Tests: `dist::tests` (when notarisation runs, where its credentials come from, and reading a
    binary's CPU and newest glibc). Run here on 2026-10-01 with `--no-notarize`: see the
    workers entry of the same day for the Linux builds' smoke runs.

- ✅ **Tests build the workspace's own crates at opt-level 0** (2026-10-02). The tests lane
  bounded every CI run (27–48 minutes over 22 runs), and 27 minutes of it was the build. That
  build is CPU-bound on a three-core runner, although sccache served 99 % of what it can cache:
  test harnesses, binaries, proc macros and build scripts compile on every run, about 4 100 of
  its 4 830 CPU-seconds. `[profile.test]` now sets opt-level 0, and dependencies stay at 3
  (dev's `package."*"`, which `test` inherits). Numbers in MEASUREMENTS, "the tests lane's
  build, and the test profile at opt-level 0".
  - **What it saves.** `slopty-agent`'s library and test harness took 3.8 times less CPU (110–116
    user-seconds at 1, 28–30 at 0).
  - **What it costs.** The suites of the UI, the worker, the daemon, the client and the shaper
    ran no slower, twice each at both levels on this Mac, and every timing-sensitive test named
    in `.config/nextest.toml` passed at 0. Code that computes runs about twice as slowly at 0
    (`slopty-proto`'s property tests, 1.7 s against 3.1 s), so the crates whose tests compute
    keep the dev level: `slopty-engine` (VT diffing, 37–48 s a test on CI), `slopty-grid`,
    `slopty-predict` (its random-editing test, 21.5 s), `slopty-codec`, `slopty-media` (FEC),
    `slopty-shape` and `slopty-net` (the link and rate simulations). The grid and the shaper sit
    under every terminal and network test. `slopty-e2e` stays at 1 too: its harness compares
    every golden pixel by pixel, in loops that opt-level 0 leaves as calls.
  - **The spawned binaries follow.** `cargo build` is `dev`, which no longer shares a workspace
    unit with the tests, so the daemons and stand-ins tests spawn are built with
    `--profile test` (the tests lane, `xtask spawned-bins`, and `slopty_testkit::bins`' own
    build). What runs for its own sake (the app, `xtask e2e`, the daemons, the benches) keeps
    dev's opt-level 1, so frame times and the e2e suites measure what they did.
  - **Accept** on the next land against run 36963072630: the tests lane's `nextest build` (1 613.7
    s) drops by at least 35 % and nextest's run (435.9 s) grows by less than 15 %.

- ✅ **A newer push to `gate` waits behind the run in progress** (2026-10-02). It used to cancel
  it, on the grounds that the newer commit's green covers both. But 10 of the last 41 gate runs
  were cancelled, two of them at 47.6 and 48.3 minutes, a few minutes from promoting main, and
  their minutes bought nothing. Now `cancel-in-progress` holds only for pull requests. GitHub
  keeps one run in progress and one pending per group, and a newer push replaces the pending
  one, so the queue never holds more than the newest commit. A pending run replaced that way
  ends as cancelled, which `land --wait` reports as such.

- ✅ **The tests lane runs in three shards** (2026-10-02). After the test profile, the lane's
  build is still the longest job, and it is CPU-bound, so it is split by package across three
  runners. `xtask gate --lane tests --shard ui|worker|rest` builds and tests one shard's
  packages, with `workspace-hack` beside them so third-party crates resolve as in every other
  build (`xtask check -p` builds the same way, so each crate's tests already pass resolved
  alone). The shards even out the build's CPU per package from the lane's `cargo-timing.html`:
  `ui` (the UI and the apps over it), `worker` (the daemon, sessions, capture, codecs, input,
  files) and `rest` (the server, the CLI, the wire, the client core, the engine, xtask). A test
  fails when a member is in no shard or two, and when `ci.yml`'s matrix leaves a shard out.
  - **The spawned binaries in a shard.** Building them `--workspace --tests` would build every
    package's tests, so a shard selects its packages and the binaries' own, with `--examples`
    for `--tests`: no member has an example, and either makes cargo resolve features with the
    selected packages' dev-dependencies, as the test build did. A shard none of whose tests
    spawns one (`ui`) builds none; a test checks the list of the packages whose tests do
    against the sources.
  - **Five Macs.** The Free plan runs five macOS jobs at once. The three shards and the two
    clippy lanes take them; rustdoc runs after host clippy on its runner (even when clippy
    failed), with its dependencies from sccache; the tools lane runs on `ubuntu-24.04`. Every
    tool there reads text alone: fmt, taplo, typos and `committed`; `cargo deny` with the
    targets in `deny.toml`; hakari with the platforms in `.config/hakari.toml`; shear. Its
    free-space floor is 5 GB (`SLOPTY_DISK_FLOOR_GB`), as it builds xtask alone.
  - **The cache.** rust-cache keys an entry by the runner's OS, so the Linux lane saves its own
    and host clippy saves the Macs' after `cargo fetch`, which gets every platform's packages.
  - `promote` still needs every job of the matrix. Each shard uploads `timings-<shard>`: its
    JUnit report, its `cargo-timing.html` and its pseudo-terminal samples.
  - Rejected: nextest's `--partition`, which splits only the run (7 of the lane's 33 minutes)
    and leaves every shard the whole build.

- ✅ **The daemon, the command line and xtask are libraries under a one-call binary**
  (2026-10-02). sccache caches a library but never a binary, so these three compiled in full on
  every CI run: `slopty-workerd`'s binary took 332 CPU-seconds and its test harness 219,
  `slopty-cli`'s 86 and 84, and every job's `cargo xtask setup` spent about two minutes on
  xtask. Each is now `src/lib.rs` with a `pub fn main`, and `src/main.rs` calls it. When the
  package is unchanged the library comes from the cache and the binary is a link. Binary names,
  `CARGO_BIN_EXE_*` and the command lines are unchanged; the unit tests and the docs are the
  library's, so each binary has `test = false` and `doc = false`.
  - The daemon's binary had `doc = false`, so its docs had never been built. As a library,
    rustdoc failed on seven links: four into private items, which went once its modules became
    private and its `Daemon` `pub(crate)` (a library's `pub` is API, and the binary had none),
    and three it could not resolve, now fixed. A constant only a Mac reads is marked so for
    Linux, which the `pub mod` around it used to hide from the dead-code lint.
  - The crash tests find `main` as `slopty_workerd::main` and `slopty_cli::main`, in each
    `lib.rs`.

- ✅ **`land` runs the changed packages' tests before it pushes** (2026-10-02; widened by "land
  checks the changed packages first", 2026-10-05). 16 of the last
  41 gate runs failed, each after the better part of an hour, and at least 7 failed on a test
  that fails every time: the UI's menus, palette and file tests and the CLI's MCP and projects
  tests. A minute here finds those. `cargo xtask land` now takes the packages its commits change
  since `origin/main` and every package that depends on them (`gate::pass::affected`, as
  `--since-pass` does), snapshots HEAD into `target/gate/tree` under the gate's lock, and runs
  their tests there, in the tests lane's target dir, under `nice -n 10`.
  - It is a net, not the gate. nextest's `land` profile leaves out the tests that time
    themselves against the machine's load or start CoreAudio or shells in bulk, each by name;
    CI runs them. A change outside every package (a root manifest, the lockfile, cargo's
    config) reaches all of them, and that is CI's to run. Docs alone run nothing.
  - A run that passed is recorded like a gate lane (`target/gate/pass/land`), so landing the
    same commits again runs nothing. `--no-tests` pushes without it.
  - Track the red-run rate (16 of 41 before), the cancelled-run rate (10 of 41) and the median
    time from push to promote.

- ✅ **The Linux worker is built and tested on a Linux runner** (2026-10-03, readiness audit C5).
  Every gate lane but the tools' ran on a Mac: the Linux crates were linted for Linux (the
  clippy-ios lane's `lint_linux`) and cross-built, but no test of theirs ever ran on Linux.
  - **The lane.** `cargo xtask gate --lane linux`, on a Linux host only (a full gate on a Mac
    leaves it out, and naming it there says to use `cargo xtask linux e2e`): the tests of every
    `LINUX_CRATES` crate but `LINUX_UNTESTED`'s built, then the worker, its ptyd, the CLI, the
    server and the stand-ins the tests spawn, built natively in the tests' profile, then nextest
    and the doctests. No workspace hack, whose features pull in GPUI.
  - **The job.** `linux` in `.github/workflows/ci.yml`, on `ubuntu-24.04` (x86_64), beside the
    gate's matrix, so the run takes no longer. zig comes from `mlugg/setup-zig` at 0.16.0, the
    minimum the vendored ghostty names. The image's unused SDKs are deleted first, since it keeps
    about 14 GB free.
  - **Every job's zig is pinned.** The Mac lanes, the release and the deep runs took
    `brew install zig`, which became 0.17 on 2026-10-03, and Ghostty's `build.zig` does not
    build under 0.17. They take `mlugg/setup-zig` at 0.16.0 too, as `cargo xtask setup` asks
    locally (`tools::ZIG`).
  - **What the runner lacks.** The job installs zsh, fish and tmux for the PTY's shell tests and
    raises `net.core.rmem_max` to 8 MiB for the endpoint's receive buffer (2026-10-03, after
    the first run; `docs/decisions/platform.md`, "The Linux lane's first run").
  - **Required.** `promote` needs it beside the gate's matrix (`needs: [gate, linux]`), since it
    passed two runs in a row after its last fix (37106871278, 37111546057). It took 12.7 and
    12.8 min there, inside the run's critical path (clippy-ios, 18 to 21 min), so requiring it
    costs no wall time.
  - **No Actions compile cache.** The repository's Actions cache held 10.78 GB in 8 309 entries
    on 2026-10-03, over its 10 GB quota, and GitHub evicts the least recently used: Linux units
    there would push out the Mac lanes' and lengthen the critical path. The job's sccache keeps
    its cache on the runner, so it compiles cold each run, and still ends inside the run.
  - **Next.** arm64 on `ubuntu-24.04-arm` once the cache question is settled (aarch64 runs here
    in the Docker e2e meanwhile). Once required, the Linux clippy could move here from the
    clippy-ios lane, the longest on the critical path.
  - Test: `gate::tests::ci_runs_the_linux_lane_on_linux`.


- ✅ **A tag's release is signed with a Developer ID and notarised, or it fails** (2026-10-04,
  readiness audit D1). `xtask dist` signed ad hoc when no identity was at hand, and skipped
  notarisation when there were no credentials. That was right for a local build. For a tag, it
  would publish an app that asks again for every grant on each update, and that Gatekeeper
  stops at first open. Under the release job (`GITHUB_REF_TYPE=tag`), `dist` now fails before
  the build when the signing is ad hoc, and after it when notarisation was skipped, naming what
  is missing (`dist::publishable`). Any other `dist` builds as before. Test: `xtask`
  `dist::tests::a_tag_is_published_only_signed_and_notarised`.

- ❌ **`land` checks the changed packages first: tests, rustdoc, and iOS and Linux clippy**
  (2026-10-05, `.research/dev-speed-2026-10-05.md`; now opt-in as `land --check`, superseded by
  "Nothing heavy runs here before a land" below). Of the last 34 gate runs, 14 were red, and
  9 of those failed on rustdoc or on the iOS and Linux clippy. Both fail every time, and neither
  the quick gate nor `land`'s tests ran them, so each cost a ~22-minute run, a fix and another
  run: about 5.7 minutes per land on average.
  - `gate::land_checks` runs three steps side by side on the affected packages: their tests (as
    before), `check::rustdoc` in the rustdoc lane's target dir, and `check::clippy_ios`,
    `tools::lint_linux` (plus xtask's Linux clippy when xtask changed) in the iOS clippy lane's.
    `xtask check -p` uses the same two steps.
  - The whole process is reniced to 10 first, so every cargo under it yields to the work on
    this Mac. Before, each command carried its own `nice -n 10`. That keeps the hybrid-gate ruling
    (2026-10-01): no full lane runs locally, only the affected crates, incremental, at a low
    priority. The first run after a dependency bump compiles the iOS and Linux dependency
    metadata once.
  - `--no-tests` now prints that CI is the first to build the push.
  - Track the share of red runs whose failed step is rustdoc or clippy-ios. The target is near
    zero.

- ✅ **Nothing heavy runs here before a land** (2026-10-05). The user wants this Mac's cores
  on the work, and the heavy checks on GitHub Actions. Before, each land compiled for minutes
  here, beside two agents' builds: host clippy in the quick gate, the changed packages' tests,
  rustdoc and iOS and Linux clippy in `land`, and the app's e2e by hand.
  - The quick gate is the tools lane alone (`gate::QUICK`): fmt, taplo, deny, hakari, shear,
    typos, `committed`, and both lockfiles `--locked` (`gate::locked`, the check host clippy's
    `--locked` made). It compiles nothing and takes seconds.
  - `land` pushes at once. `land --check` keeps the old checks for a change likely to go red.
  - CI already ran host clippy. It now also runs the app's e2e (`e2e.yml`, `cargo xtask e2e
    app --review`, a changed frame's render and diff in the `e2e-app` artifact). It is not a
    gate lane until its renders on a hosted Mac's virtual GPU are shown to match the goldens,
    which a Mac with its own GPU draws. It is a sixth macOS job beside five, so it starts as
    soon as a lane ends.
  - It is a workflow of its own, with its own concurrency group. In `ci.yml` its cold build
    (over 30 minutes on its first run, 37341280953) kept the run going after every gate lane
    had passed. That held the next push's run in the queue, and `xtask promote`, which waited
    for the whole run. `promote` now asks only that every gate lane passed.
  - A run in progress there finishes, and only the newest push waits. When each newer push
    cancelled it, its first runs never reported, since a land comes oftener than its 40
    minutes.
  - The cost is a red run found later: about 5.7 minutes per land on CI's wall clock (the entry
    above), while the agents keep working, against minutes of every core here per land. Agents
    still run clippy and their own crate's tests as they code.
  - Test: `gate::tests::the_quick_gate_compiles_nothing_and_ci_runs_the_app_e2e`.

- ✅ **binstall gets the job's token on CI** (2026-10-05). In 33 of 111 test-lane setups,
  binstall's unauthenticated GitHub API calls hit the runner's shared rate limit and timed out,
  so it compiled nextest from source: 5 to 8 minutes against 3 seconds (run 37245352908's
  worker shard: 312 s). Every `cargo xtask setup` and binstall step now gets
  `GITHUB_TOKEN: ${{ github.token }}`, the job's read-only token. The compile fallback stays:
  a lane that fails outright costs a whole rerun, and one that runs slow costs minutes. Verify
  that no setup step takes more than 150 s over the next 20 runs.

- ✅ **Linux clippy runs on a Linux runner, rustdoc beside iOS clippy, and xtask's tests in the
  ui shard** (2026-10-05, `.research/dev-speed-2026-10-05.md` item 5). Only five of the twenty
  jobs a public repository runs at once can be Macs, so a job that needs no Mac shouldn't take
  one. Linux clippy was 488 s of the clippy-ios job's 720 s, and that job finished last in 7 of
  18 runs.
  - `--lane clippy-linux` (`LaneId::ClippyLinux`) is its own job on `ubuntu-24.04` with the three
    Linux triples. Locally, a full gate runs it in the iOS lane's target dir, so there is one
    tree of cross metadata.
  - Clippy never links, but build scripts compile C for every triple. On the Mac, `cc` is clang,
    which cross-compiles for any target. Ubuntu's `cc` is gcc, which builds only for the host, so
    the job sets `CC=clang`. Checked in the Ubuntu 24.04 image on arm64, the harder case, where
    no Linux triple but one is the host: every step passed (369 s on 6 cores).
  - rustdoc moves from host clippy's runner, which also lints the fuzz crate, to iOS clippy's.
  - xtask's tests move from the rest shard to the ui shard: its icon test waits on actool for
    about 148 s, and the rest was the longer of the two.
  - Verify with the job durations, and with which job finishes last.

- ✅ **The tests lane keeps both runs' JUnit reports** (2026-10-05). On a hosted Mac the
  VideoToolbox tests run in a second nextest run after the main one, under the same profile, and
  nextest writes one `junit.xml` per profile. So the second report replaced the first: the run's
  summary named only failed encoder tests, and CI's timings for the rest and ui shards held no
  test at all. The main run's report is now renamed to `junit-main.xml` before the second run.
  The summary reads both, and CI uploads both.

- ✅ **libghostty-vt is built once per set of inputs** (2026-10-05,
  `.research/dev-speed-2026-10-05.md` item 4; numbers in MEASUREMENTS, same day). Every target
  dir ran its own minute-long zig build of the same source: every gate lane and agent here, and
  most CI jobs, where it headed the build's critical path.
  - Our libghostty-rs fork (5ad52ee) takes `LIBGHOSTTY_VT_SYS_PREBUILT_DIR`. It keys the install
    prefix on everything the build reads: the source's commit, the zig version, target and host,
    optimize mode, CPU, link mode, the Apple SDKs, the deployment targets, and the build script.
    It copies a matching entry instead of building, and publishes a new one by a rename. A
    vendored tree with uncommitted edits to tracked files is always built. An entry holds its
    whole key, so a collision is a miss.
  - `.cargo/config.toml` sets it to `target/ghostty-prebuilt`. The gate's lanes build in the
    index snapshot, whose sync deletes what the index lacks, so `lane_shell` points them at the
    checkout's. `xtask prune` drops entries unused for 14 days (a use touches the key file) and
    staging directories a dead build left.
  - CI caches the directory per lane, keyed by the ghostty and binding revisions, and setup-zig
    no longer caches zig's own directories, which this build never read: 2.3 GB of the
    10 GB quota went back to sccache.
  - `fuzz/Cargo.lock` was one binding revision behind main against the same vendored ghostty.
    `upstream sync` now moves it with the root lock.

- ✅ **The Deep checks take one Mac at a time** (2026-10-05, `.research/dev-speed-2026-10-05.md`
  item 3). With two at a time, the gate kept three of the five macOS slots for its five macOS
  jobs, and runs queued 10 to 20 minutes during the Deep window. Now it keeps four, and the
  night holds the six hours the checks take one after another. Two other ideas don't work:
  - Moving the checks that don't need macOS to Linux would drop their coverage of the macOS
    crates.
  - A job that waited inside itself for the gate to finish would hold its Mac while it waited.
- ❌ **Not taken: caching the xtask binary across CI jobs** (2026-10-05, the same study's item 7).
  `target/xtask` is 1.3 GB, and the Actions cache's 10 GB holds sccache's units, which save more
  per byte than the about 50 s each job spends compiling xtask.

- ✅ **A commit that changes a workflow is promoted from a checkout** (2026-10-05). GitHub never
  lets a run's own token create or update a file under `.github/workflows/`, so CI's `promote`
  job was refused on 10193e1a and on 373913ce after every lane had passed. Main stayed behind,
  and nothing said why except the push's error. `cargo xtask promote [commit]` makes the same
  checks the job does and pushes from here: the run on the commit is complete, every `gate`
  job in it passed, and main fast-forwards to the commit. `land --wait` calls it when the
  lanes passed and only the promote failed. The promote job now says in the run's summary that
  a workflow changed and which command to run. A token with the `workflows` scope in the
  repository's secrets would let CI do it itself; that is the person's call.

- ✅ **The Mac app says when a newer Slopty is out** (2026-10-05, readiness G11). Worker and
  server builds already follow the app, but the app itself never learned of a release.
  - **What it reads.** The GitHub latest-release feed of the repository Cargo names
    (`CARGO_PKG_REPOSITORY`), so a fork reads its own releases. It reads at launch and every
    24 hours after. Only a published, final release whose `major.minor.patch` is past this
    build counts: no release yet (GitHub answers 404), a draft, a pre-release, an answer that
    does not parse, or a page that is not https says nothing.
  - **How it is said.** As a quiet line in the status bar, "Slopty 0.2.0 is out", which opens
    the release page when clicked. It stays until this build is the latest. A toast would
    interrupt for something that can wait a day, and a notification would be louder still.
  - **What it does not do.** It downloads and installs nothing. The person updates from the
    page, at least until Developer ID signing and notarisation run on every release.
  - **Only the Mac asks.** An iPhone or iPad takes its builds from the App Store or TestFlight,
    not from a release page. A self-test reads no network.
  - **How it fetches.** One GET through `NSURLSession` (`slopty_platform::fetch`), so the
    system's TLS, proxy and network path apply, and there is no HTTP stack of our own for a
    request a day.
  - Tests: `slopty_client::update` `the_feed_is_the_repository_s_latest_release` and
    `only_a_newer_final_release_is_news`; `slopty_platform::fetch`
    `a_get_brings_the_body_or_why_not` (a loopback server: 200, 404, refused, not a URL);
    `slopty-ui` `a_newer_release_is_said_in_the_bar_and_opens_its_page`.

- ✅ **gpui-fast takes longbridge `5c31c703` and zed `96837d78`; gpui-kit takes upstream `8d8cc671`**
  (2026-10-05).
  - **What upstream changed.**
    - Longbridge's commit (#36) is the compat move to gpui-pre 0.3.8. The fork had already made
      it (`7e4b5a2`), so the merge is a no-op.
    - Zed's commit drops a reentrancy flag in `flush_effects` that `pending_updates == 1`
      already guards.
    - gpui-kit's #3371 sizes `TextView`'s headings and block spacing from the font size it
      inherits, not from a fixed 14 px base. It builds the heading hierarchy with weight more
      than size.
  - **Adopted:** the thread's Markdown is a `TextView`, so its headings now follow the
    thread's own text size. The goldens that show a heading or list were reviewed and retaken
    with this sync.
  - **Our own commit on gpui-kit:** a guard test that an opening bracket stays on the line of
    the inline code after it (`12daeb01`).

- ✅ **gpui-fast takes zed `dff53544` (with #64209) and our device-pixel masks** (2026-10-05,
  `52b465bc`).
  - **zed#64209, "Hash a GlobalElementId once, when it is built",** is upstream's own form of
    the fork's `fast::global_id`. It uses an `ElementIdStack` with incremental path hashes and
    `GlobalElementId::with_hash`. The fork takes it and deletes `fast/global_id.rs` whole.
    - The fork's id cache handed out the same `Arc` across frames. Measured against upstream's
      allocating `global_id()` with `gpui_perf --headless --frames 300` over seven scenarios,
      it saved 1–11 % of allocations per frame but no instructions (−1.9 % to +0.7 %), so it
      went too.
    - `gpui_perf --verify` finds the painted frames identical in all 44 checks.
  - **The dff53544 import** made `gpui_macos`'s Core Media and ScreenCaptureKit deps optional
    behind `screen-capture`, and moved `taffy`, `font-kit`, `bytemuck` and six more into
    `[workspace.dependencies]`. The fork adds those entries. It keeps `objc2-core-graphics`
    non-optional, since its own cursor and scroll code uses it.
  - **Ours:** `Window::paint_mask` paints a monochrome mask at exact device pixels and
    rasterises it once per key and size (`fast/mask.rs`). The SF Symbols path in
    `slopty-platform::symbols` is built on it.

- ✅ **gpui-fast `28d2801` and gpui-kit `43f20bd2`, for the SF Symbols chrome** (2026-10-05).
  - gpui-fast: `paint_mask` takes a transform (identity at rest, so a mask stays at exact
    device pixels while a disclosure chevron turns). The test-support
    `Window::render_to_image_at` draws the window offscreen at another scale, which makes the
    `thread@2x` golden possible on CI's 1x runner.
  - gpui-kit: `IconPainter`, a global that the kit's `Icon` asks before drawing its SVG. This
    lets the questionnaire check, the input's chevrons and its clear button draw as the
    chrome's SF Symbols. Upstream has no open PR doing this. #3360 touches `icon.rs`, but not
    rendering.

- ✅ **gpui-fast `20538a6`: a frame holds the views it may rebuild weakly** (2026-10-05, fork PR
  #29). A frame's rebuild records held each view strongly (`fast/splice.rs`), so a dropped tile,
  its native host and its WKWebView lived until the window drew again, and WebKit refused to
  delete a forgotten worker's page store as in use. The records hold `AnyWeakView` now, and a
  gap that cannot upgrade builds the view around it. Upstream has nothing open on it.
