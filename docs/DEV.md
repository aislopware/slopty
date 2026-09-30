# Development

The repository, the commands and the loop. Rules of the game are in `CLAUDE.md`; the map is
`docs/ARCHITECTURE.md`; rulings and evidence are under `docs/decisions/`.

## Layout
`crates/*` libraries, `apps/*` binaries, `xtask/` automation, `vendor/ghostty` the libghostty-vt
source, `docs/` design + decisions. GPUI comes from `aislopware/gpui-fast`, gpui-kit from
`aislopware/gpui-kit` and libghostty-vt from `aislopware/libghostty-rs`, as rev-pinned git
dependencies. `vendor/ghostty` is a submodule on our fork `aislopware/ghostty`, not on
ghostty-org's repository, and libghostty-rs pins the same commit (`GHOSTTY_COMMIT`). Each fork
carries our commits on its default branch: gpui-fast merges longbridge's branch in, the other
three are rebased onto theirs. gpui-fast is GPUI imported flat out of zed, and the fork imports
zed itself so it is never behind zed while longbridge lags.

## Dev loop
- Before coding, bring the ground up to date: `cargo xtask upstream check` and `sync` whatever
  is behind (the four forks and zed; ghostty is `vendor/ghostty`), `rustup update`, `cargo update -w`, and
  `cargo binstall -y <tool>` for any gate tool `cargo info <tool>` shows behind.
- `cargo xtask setup` installs tools (binstall) and initialises submodules, then runs
  `cargo xtask doctor`: it times the first launch of fresh binaries, and when `XProtect` scans
  them it names the app to switch on under System Settings → Privacy & Security → Developer
  Tools (the terminal, or the multiplexer if builds run inside one; only you can switch it). It
  also checks the free space against the floor below.
- `bacon` for the watch loop; `cargo nextest run -p <crate>` for one crate.
- Format with `cargo xtask fmt` (nightly rustfmt; stable `cargo fmt` produces different output).
- `cargo xtask e2e <case>` runs the live tests (`docs/TESTING.md`); `cargo xtask e2e server`
  is the one for the server, its worker link, the CLI and MCP, and takes seconds.
  `--filter '<nextest filterset>'` narrows any case to the tests it picks (one golden, one live
  scenario), and `--no-build` reruns the last build as it is, without cargo. Every case
  runs on this Mac alone: `workers` starts its second worker here, behind a relay shaped like
  the tailnet, so nothing waits on another machine.
- The UI draws under gpui-fast's view retention: a view is built again only when it was told
  of a change or something it read changed (`docs/ARCHITECTURE.md` §6, "Drawing under
  retention"). A view that shows an old state is a missing notify; `GPUI_VIEW_RETENTION=0` on the
  app turns retention off to confirm it, and a step in `workspace/tests/retained.rs` is how it
  stays fixed.
- `cargo xtask fixtures claude [--only <name>]` records the conversation fixtures from a real
  session (it uses the model and your login), and `cargo xtask fixtures claude-mod` records the
  mod's against a canned local API with no account. Both run the official Claude Code build
  that xtask fetches from npm into `target/claude/<version>/`, checked against the registry's
  sha512; `SLOPTY_CLAUDE` points at another binary of the same pinned version.
- `cargo xtask linux` cross-builds the terminal-only Linux worker (`slopty-ptyd`,
  `slopty-worker`, `slopty`) for `aarch64-unknown-linux-gnu` under `target/linux`, with
  `cargo zigbuild` (`cargo binstall cargo-zigbuild`; zig is the one libghostty-vt takes).
  `cargo xtask linux run` starts it in a fresh Debian container on Docker Desktop (the
  `desktop-linux` context) and prints the loopback address to dial, such as
  `slopty ping --worker 127.0.0.1:<port>`; Ctrl-C removes the container. `cargo xtask linux e2e`
  runs the Linux end-to-end test against it (`docs/TESTING.md`). The daemons' logs go under
  `target/logs/linux/<container>/`.
- `cargo xtask run worker|app` to launch; `cargo xtask ios sim [--sim ipad]|device` for the phone/tablet;
  `cargo xtask bundle` builds a signed `Slopty.app` (app + daemons + CLI) under `target/bundle`
  with the icon rendered from `assets/icon.svg` (`cargo xtask icon` previews it);
  `cargo xtask ime [id]` switches the macOS input source for input-method tests.
- `cargo xtask sign` gives the dev daemons a Developer ID signature under their LaunchAgent
  identifiers, so one approval of Screen Recording and Accessibility survives every later build
  (`run worker` does it for you). Without it a rebuilt daemon is a new executable to TCC and
  loses both, which surfaces as ScreenCaptureKit `-3801` and no prompt (`docs/decisions/input.md`).
- `cargo xtask upstream check` shows how far the gpui-fast, gpui-kit, ghostty and libghostty-rs
  forks are behind upstream, and how far zed is ahead of gpui-fast's import (bases in `xtask/upstream.toml`).
  For zed it reads `zed_commit` from the fork's `UPSTREAM`, counts the zed commits since that touch
  the tracked directories, and lists what an import would ask for by hand: crates joining or
  leaving the tracked set, and redirected files zed changed. Once a source's `check_every_days`
  has run out (none for gpui-fast and gpui-kit, which land changes most days, so every gate asks;
  a day for ghostty; a week for zed and libghostty-rs), the gate asks the upstream for its head
  (`git ls-remote`) and warns when it moved. A source with `paths` (ghostty) warns only when
  GitHub's compare says the move changed a file under them. For ghostty, `check` also lists the
  upstream commits since the base that touch those paths (the terminal, its C API and headers,
  SIMD, Unicode, the key and mouse encoders, the lib-vt build, `build.zig.zon`), and where
  `vendor/ghostty` stands against the fork head and the commit this repository records.
- `cargo xtask upstream watch [--interval 300] [--once]` prints a line whenever a watched
  upstream's default branch moves (its subject and how many commits it is past our base) or a
  pull request there is opened, updated, merged, closed or reopened. It watches the forks'
  upstreams and the vendored noq and objc2 (`xtask/upstream.toml`), and never syncs. For ghostty
  it names a head move only when the commits since the last look touch its `paths` (the line
  lists the files), and a pull request only when it does. What it saw
  is kept in `target/upstream-watch/state.json`, so a restart does not announce it again, and
  an upstream that does not answer is skipped for the round.
- `cargo xtask upstream sync [--only <fork>]` works in the checkouts under `.research/` in the
  main checkout. It rebases gpui-kit and libghostty-rs and merges longbridge's branch into
  gpui-fast. Then it imports zed into gpui-fast by the procedure in gpui-fast's
  `docs/upstream-sync.md`: a vendor commit `zed: import <short>` on the last one (`import_commit`)
  holding zed's tracked directories byte for byte, built from the zed checkout (`.research/zed-main`,
  fetched, its work tree untouched), merged into the fork with `UPSTREAM` rewritten. It then runs
  `script/check-upstream` and the build checks, pushes (`SSH_AUTH_SOCK` on the signing agent
  first) and moves the `Cargo.lock` pins. A conflict that is not `Cargo.lock` stops it. So does an
  import that needs a hand: a conflict, a file zed changed that a `#[path = "fast/…"]` redirect
  replaces (port it into our copy), or a crate the workspace manifest must add or drop. It stops
  mid-merge with `UPSTREAM` already staged and prints the list; finish there, `git commit`, and
  run `sync` again. `--only zed` is `--only gpui-fast`. Then gate, e2e app + ios, and a
  DECISIONS entry.
- ghostty goes through the same `sync` (`--only ghostty` brings libghostty-rs with it). It never
  rebases in `vendor/ghostty`, which every build compiles: the fork's `main` is checked out in
  the worktree `.research/ghostty` and rebased onto ghostty-org's `main` there, so a conflict
  stops the sync with the file list and `vendor/ghostty`, both forks and every pin untouched;
  resolve it in that worktree, `git rebase --continue`, and run `sync` again. Everything is
  then built and checked before anything is pushed: libghostty-rs is rebased onto its upstream,
  its `GHOSTTY_COMMIT` moves to the rebased head, the bindings are regenerated from that tree
  (the crate's `gen-bindings`) and committed, and `cargo check` and `cargo test -p
  libghostty-vt` run with `GHOSTTY_SOURCE_DIR` on it. Only then does it publish, in the order
  the pins need and confirming each: push the ghostty fork, move `vendor/ghostty` (detached)
  to that head, push libghostty-rs, `cargo update -p libghostty-vt` and check the lock pins
  the pushed binding. Stage `vendor/ghostty` with `Cargo.lock`. `--no-push` stops after the
  checks with `vendor/ghostty` and the lock as they were. `sync --only libghostty-rs` pins
  whatever `vendor/ghostty` holds, and refuses a commit the ghostty fork does not have.

## Gate
`cargo gate` is fmt, clippy `-D warnings` on all targets and all three Apple triples, clippy for
Linux (`x86_64-unknown-linux-gnu` and `aarch64-unknown-linux-gnu` on the server's crates and
the worker's that build there, `xtask/src/tools.rs` `LINUX_CRATES`, and
`x86_64-unknown-linux-musl` on the server's), nextest,
doctests, rustdoc, deny, hakari, shear, typos, taplo and `committed`. It checks the **index**,
not the working tree: the staged blobs are synced into `target/gate/tree` (submodules checked
out at the commit the index pins, under `target/gate/modules`) and checked in parallel lanes on
`target/gate/*` target dirs. Several agents edit this one checkout at once, so stage exactly
the change you mean to land (`git add <paths>`), gate it, and commit it; the tree stays free to
edit meanwhile. `--quick` is fmt + host clippy + tests; `--fix` runs the fixers on the tree
first (stage what they changed); `--in-place` checks the tree itself; `--lane <name>` (repeat
for several: `tools`, which carries fmt, `clippy-host`, `clippy-ios`, `tests`, `rustdoc`) runs
only those lanes, and a lane that runs alone takes every core. Per-lane times are in the log.

fmt and the tool checks (deny, hakari, shear, typos, taplo, `committed`) take seconds, so they
run first, side by side, and a failure among them ends the gate before any compile. Then the
compile lanes run. In the tests lane, nextest and the doctests run side by side once the test
binaries are built, since cargo holds its lock only while it builds.

A lane that passed leaves a record of its inputs under `target/gate/pass/`: the index entries it
reads (`mode sha path`, submodules at their pinned commit), the toolchain (`rustc -vV`), the
xtask binary, the environment cargo and the tests read (values hashed), the macOS and Xcode
builds, and the tools it runs (for the tools lane also `HEAD`, the last tag and the day, so
`cargo deny` fetches advisories daily). When a lane's inputs match its record it is skipped
("inputs unchanged since it last passed"), so rerunning a gate that did not change costs
seconds. The cargo lanes leave out paths nothing cargo builds or tests reads (`docs/`, the root
`*.md`, the tools' own configs, `xtask/src/gate/pass.rs` `inert`), so a change to the docs
alone skips them; an xtask test fails if a crate starts reading one.
`cargo gate --since-pass` narrows the tests further: when only files inside packages changed
since the tests lane last passed, nextest runs the tests of those packages, of every package
that depends on them (from `cargo metadata`, dev-dependencies included) and xtask's. A changed
file outside the packages (a manifest at the root, `Cargo.lock`, cargo's config, a vendored
tree) runs them all. Clippy and rustdoc need no flag for this, because cargo already rebuilds
only the changed crates and their dependents. Either way the gate checks the snapshot of the
index, and a skipped lane or test has already passed on the same inputs.

The gate's nextest profile is `gate` (`.config/nextest.toml`). It retries the tests named
there as timing-sensitive, which have failed under the gate's load and pass alone, up to twice.
A pass on a retry shows as FLAKY in the log. Only a named test gets retries, never a pattern.
The `ci` profile inherits `gate` (the same named retries and no others) and adds a runner's
longer timeouts and a JUnit report.

CI (`.github/workflows/ci.yml`) is the same gate, one lane per hosted runner: a matrix job per
lane runs `cargo xtask setup --lane <lane>` (only that lane's tools) and
`cargo xtask gate --ci --lane <lane>`. `--ci` checks in place, since the runner's tree is the
commit, and gives the tests lane the `ci` profile. That profile's `default-filter` leaves out
the tests that read hardware a runner's virtual Mac lacks, each named with the reason; nextest
lists them as skipped and every local gate still runs them. Only missing hardware puts a test
there, never timing and never a pattern. Compiled units come from sccache on the Actions cache
(`SCCACHE_GHA_ENABLED`); `rust-cache` keeps the registry, git checkouts and installed tools
per lane, failures included. The job's post step prints sccache's hit rate.

`cargo xtask check -p <crate>…` runs the same steps on named crates only, on the working tree:
what an agent that owns those crates runs before it reports. Its builds name `workspace-hack`
beside the crates, so every crate set resolves the third-party dependencies with the features
the gate gives them and reuses one build of each; a bare `cargo nextest run -p <crate>`
resolves its own and builds its own copies (add `-p workspace-hack` to share).
`cargo hakari generate` rewrites the hack when a manifest changes its dependencies
(`cargo gate --fix` runs it; the gate fails on a stale one).

## Disk
`target/` would grow without end: cargo never deletes a unit, and every `cargo update`, fork
rebase, toolchain or new feature set leaves the old ones behind. `cargo xtask prune` deletes, in
every target dir under `target/` and in both of cargo's layouts (to 1.99 and from 1.100):
- the units no build has read for a day, with their artifacts;
- the incremental caches no compile has touched for a day;
- the object files a binary's earlier compiles left beside it.

Then, when `target/` is over its budget (160 GB, `SLOPTY_TARGET_BUDGET_GB` or `--budget-gb`) or
its volume has less than the floor free (50 GB, `SLOPTY_DISK_FLOOR_GB` or `--floor-gb`), it
deletes the least recently used units and caches until both hold with a tenth to spare. Nothing
used in the last hour goes that way. It runs after every `check` and `gate`, skipping a dir whose
build lock is held; by hand it waits for the lock (`--idle-hours N`, `--dry-run`). It prints
what went, the largest entries and how much was used how recently. The gate will not start
under the floor: it prunes first, and if that is not enough it names what takes the space. How
prune knows a unit is in use: `docs/decisions/tooling.md`, "target/ stays under a byte budget".

A test binary runs slowly from a large directory: every process that opens `VideoToolbox` or
`CoreAudio` pays for the entries beside its executable, and `deps/` holds tens of thousands
(`docs/decisions/tooling.md`, "A test binary's directory, not the volume"). The gate's tests lane
and `cargo xtask check` therefore run each test binary through `cargo xtask test-runner`,
which runs it from a hard link in `<profile>/run/`. A bare `cargo nextest run` does not do this.
To get it, set `CARGO_TARGET_AARCH64_APPLE_DARWIN_RUNNER="$PWD/target/xtask/debug/xtask-runner
test-runner"`.

## Deep checks (on a schedule, not per commit)
`cargo xtask deep <check>` runs what is too slow for the gate, each on its own target dir
under `target/deep/`:
- `miri` — the pure crates' tests under Miri (nightly; `PROPTEST_CASES=8`, isolation off for
  insta). `-p <crate>` narrows it.
- `sanitize [address|thread]` — the tests of every crate with `unsafe` built with
  `-Zsanitizer` and `-Zbuild-std` on nightly (`SANITIZED` in `xtask/src/deep.rs`).
- `sanitize realtime` — the codec's tests under `RealtimeSanitizer`, with the audio render
  callback marked real-time (`--cfg slopty_rtsan`): an allocation, lock or blocking call
  reached from it aborts. One test proves it trips, on an allocation armed in a child process.
- `features` — `cargo hack check --each-feature` over the workspace: every feature alone,
  none, and all.
- `coverage [--html]` — `cargo llvm-cov nextest` line coverage per crate (the live e2e crate
  left out).
- `mutants -p <crate> [--timeout s]` — `cargo mutants` on one crate; the surviving mutants are
  the lines no test would notice changing.
- `fuzz [--time s]` — every fuzz target for 30 s (`cargo xtask fuzz` below).
- `loom` — the audio ring on loom's atomics (`--cfg slopty_loom`), every interleaving of its
  scenarios checked.
- `leaks` — the daemon and wire test binaries under `leaks --atExit`: a leaked allocation fails.
- `metal [--filter …]` — the app self-test with Metal API and shader validation on.

`cargo xtask fuzz [<target>] [--time s] [--jobs n]` builds `fuzz/` with cargo-fuzz (nightly,
AddressSanitizer, debug assertions; `cargo binstall cargo-fuzz`) and runs each target, or the one
named, for `--time` seconds (60 by default) under `nice`, in libFuzzer's fork mode so a crash is
written and the run goes on. It replays the kept regression inputs first. The corpus of each
target grows under `target/fuzz/corpus/<target>`. Beside it, seeds are rewritten every run from
the wire goldens in `crates/slopty-proto/tests/snapshots`. Anything found lands under
`target/fuzz/artifacts/<target>/`, with the log in `target/fuzz/logs/`, and fails the run.
`--keep <artifact>` minimises one into `fuzz/regressions/<target>/`, which the fuzz crate's
`tests/regressions.rs` replays on a plain build: `cargo xtask fuzz --replay`, no nightly needed.
A new target is a function in `fuzz/src`, an entry in its `TARGETS`, a file in
`fuzz/fuzz_targets` and a `[[bin]]`; that test fails when the three disagree. The crate has its
own `Cargo.lock`, so `cargo update --manifest-path fuzz/Cargo.toml` moves its dependencies, and
the gate checks its formatting.

`cargo xtask profile -- <command…>` records a CPU profile of any command with samply into
`target/profile/<epoch>.json.gz`; `samply load <file>` opens it in the Firefox Profiler.

## Budgets, the bench, the soak and the nightly
- The allocation budgets are ordinary tests (`tests/allocs.rs` in `slopty-media`,
  `slopty-engine` and `slopty-grid`), so `cargo gate` runs them. A broken one prints what the
  path allocated now; a lower number is lowered in the test, a higher one is a finding.
- `cargo xtask bench [--filter <name>] [--update-budgets] [--wall]` runs every `*_cost`
  measurement of the crates that take `slopty-testkit` as a dev-dependency, in release, and holds
  each series' retired instructions per operation to `xtask/budgets.toml` (5 % slack). It fails
  on a series over budget, one with no budget, and a budget nothing measured. After a change
  that makes a path cheaper, or a new measurement, `--update-budgets` records the run, and the
  diff of `xtask/budgets.toml` goes in the commit with the change. `--wall` appends the wall
  times to `target/nightly/bench.jsonl` and prints how they moved since the last run there.
  The printed table carries the rows for `docs/MEASUREMENTS.md`.
- A new measurement is an `#[ignore]`d test named `*_cost` that times its samples with
  `slopty_testkit::bench::Bench` (one `series` per thing timed, `report()` at the end). A series
  whose samples run other threads is `wall_only()`: the instruction count is the process's.
- `cargo xtask soak [--seconds 60] [--interval 2] [--stacks] [--debug] [--out <dir>]` starts the
  server, ptyd and worker from a temporary HOME and drives open, flood, hook, read and close
  cycles through the CLI. It first runs 1 536 cycles, four at a time, so the bounded stores
  (the server's event log, the worker's idempotency ledger) are full before the baseline, and
  the slope is taken over the load alone. It fails on footprint growth, a peak over budget, descriptors or
  threads left behind, or a leak `leaks` finds. The samples, logs, `leaks` reports and
  `summary.json` go to `target/deep/soak/last`. The daemons it runs are copies under
  `target/deep/soak/bin`, signed ad hoc for `leaks`; the build's own binaries keep their
  signature. `--stacks` adds `MallocStackLogging`, so the reports show where a leak came from.
- `cargo xtask nightly [run] [--only <check>] [--skip <check>] [--soak-minutes 20]
  [--proptest-cases 4096] [--iterations 50]` runs the heavy lanes one after another under
  `nice`: `soak`, `bench` (with `--wall`), `proptest`, `gpui-iterations`, `miri`,
  `sanitize-address`, `sanitize-thread`, `coverage`, `features` and `fuzz`. Each writes
  `<check>.log` and `<check>.json` under `target/nightly/<date>/`, beside a `summary.json`. A
  check whose tool is missing is skipped and says why. `cargo xtask nightly install` writes and
  loads the
  LaunchAgent `dev.aislopware.slopty.nightly`, which runs it at 03:00 at background priority;
  `cargo xtask nightly uninstall` removes it. A failing seed of `gpui-iterations` replays with
  `SEED=<n> cargo nextest run -p slopty-ui <test>`.
