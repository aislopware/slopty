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

- ✅ **A golden holding a `serde_json::Value` puts its keys in order** (2026-09-29).
  `golden__ctl__ctl_reply_permission_answer` passed in the workspace and failed under
  `cargo test -p slopty-proto`: whether a `Value` keeps the order its keys were written in is
  `serde_json`'s `preserve_order`, which the workspace's tests get through feature unification
  and the crate alone does not. Declaring the feature in `slopty-proto` would have fixed the
  test by shipping it in the daemons and the CLI, which build without it, against the rule
  above that the tests' features stay out of shipped builds. The ctl goldens instead sort every
  object's keys before encoding (`sorted` in `tests/golden.rs`), so both runs write the same
  line.
