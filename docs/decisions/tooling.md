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

- ✅ Rust 1.98.1 pinned; edition 2024; resolver 3; `[workspace.lints]` with clippy
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
    cargo, but cargo 1.98.1 still ignores it without `-Zfeature-unification`.
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
    under its lock. rustc 1.98 locks with `fcntl(F_SETLK)` on macOS; beta and nightly lock with
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
