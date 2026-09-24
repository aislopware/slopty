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

- ✅ nextest 0.9.144 · insta 1.48 · proptest 1.11 · cargo-mutants 27.1 · cargo-llvm-cov 0.9 ·
  cargo-deny 0.20.2 · cargo-shear 1.13.4 · cargo-hack 0.6.45 · cargo-semver-checks 0.50 ·
  typos 1.50.1 · taplo 0.10 · bacon 3.25 · samply 0.13.1 · tracing-tracy 0.12.

- ✅ **Releases from Conventional Commits**: `committed` 1.1.11 lints every message
  (`cargo gate` over the range since the last tag); `git-cliff` 2.14.1 computes
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
