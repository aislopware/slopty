# Development

The repository, the commands and the loop. Rules of the game are in `CLAUDE.md`; the map is
`docs/ARCHITECTURE.md`; rulings and evidence are under `docs/decisions/`.

## Layout
`crates/*` libraries, `apps/*` binaries, `xtask/` automation, `vendor/ghostty` pinned submodule
(libghostty-vt source), `docs/` design + decisions. GPUI comes from `aislopware/zed`, gpui-kit
from `aislopware/gpui-kit` and libghostty-vt from `aislopware/libghostty-rs`, as rev-pinned git
dependencies. Each fork carries our commits on its default branch, rebased onto upstream.

## Dev loop
- Before coding, bring the ground up to date: `cargo xtask upstream check` and `sync` whatever
  is behind (the three forks and `vendor/ghostty`), `rustup update`, `cargo update -w`, and
  `cargo binstall -y <tool>` for any gate tool `cargo info <tool>` shows behind.
- `cargo xtask setup` installs tools (binstall) and initialises submodules.
- `bacon` for the watch loop; `cargo nextest run -p <crate>` for one crate.
- Format with `cargo xtask fmt` (nightly rustfmt; stable `cargo fmt` produces different output).
- `cargo xtask e2e <case>` runs the live tests (`docs/TESTING.md`); `cargo xtask e2e server`
  is the one for the server, its worker link, the CLI and MCP, and takes seconds.
  `--filter '<nextest filterset>'` narrows any case to the tests it picks (one golden, one live
  scenario), and `--no-build` reruns the last build as it is, without cargo. Every case
  runs on this Mac alone: `workers` starts its second worker here, behind a relay shaped like
  the tailnet, so nothing waits on another machine.
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
- `cargo xtask upstream check` shows how far the GPUI, gpui-kit and libghostty forks are behind
  upstream (bases in `xtask/upstream.toml`). Once a fork's `check_every_days` has run out (none
  for gpui-kit, which lands several changes a day, so every gate asks; a week for the others),
  the gate asks the upstream for its head (`git ls-remote`) and warns when it moved.
  `cargo xtask upstream sync` rebases the forks in `.research/` under the main checkout, build-checks, pushes them
  (`SSH_AUTH_SOCK` on the signing agent first) and moves the `Cargo.lock` pins, stopping on
  any conflict that is not `Cargo.lock`. Then gate, e2e app + ios, and a DECISIONS entry.

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
first (stage what they changed); `--in-place` checks the tree itself (CI). Per-lane times are
in the log.

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
every target dir under `target/`, the units no build has read for a day (with their `deps/`
and `build/` artifacts) and the incremental caches no compile has touched for a day. It runs
after every `check` and `gate`, skipping a dir whose build lock is held; by hand it waits for
the lock (`--idle-hours N`, `--dry-run`). How it knows a unit is in use:
`docs/decisions/tooling.md`, "target/ stays bounded".

## Deep checks (on a schedule, not per commit)
`cargo xtask deep <check>` runs what is too slow for the gate, each on its own target dir
under `target/deep/`:
- `miri` — the pure crates' tests under Miri (nightly; `PROPTEST_CASES=8`, isolation off for
  insta). `-p <crate>` narrows it.
- `sanitize [address|thread]` — the daemons' and codec's tests built with `-Zsanitizer` and
  `-Zbuild-std` on nightly.
- `features` — `cargo hack check --each-feature` over the workspace: every feature alone,
  none, and all.
- `coverage [--html]` — `cargo llvm-cov nextest` line coverage per crate (the live e2e crate
  left out).
- `mutants -p <crate> [--timeout s]` — `cargo mutants` on one crate; the surviving mutants are
  the lines no test would notice changing.

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
  `sanitize-address`, `sanitize-thread`, `coverage` and `features`. Each writes `<check>.log`
  and `<check>.json` under `target/nightly/<date>/`, beside a `summary.json`. A check whose tool
  is missing is skipped and says why. `cargo xtask nightly install` writes and loads the
  LaunchAgent `dev.aislopware.slopty.nightly`, which runs it at 03:00 at background priority;
  `cargo xtask nightly uninstall` removes it. A failing seed of `gpui-iterations` replays with
  `SEED=<n> cargo nextest run -p slopty-ui <test>`.
