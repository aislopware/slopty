# Development

The repository, the commands and the loop. Rules of the game are in `CLAUDE.md`; the map is
`docs/ARCHITECTURE.md`; rulings and evidence are under `docs/decisions/`.

## Layout
`crates/*` libraries, `apps/*` binaries, `xtask/` automation, `vendor/ghostty` pinned submodule
(libghostty-vt source), `docs/` design + decisions. GPUI comes from `aislopware/zed` (branch
`slopty`) and gpui-kit from `aislopware/gpui-kit` (branch `slopty`) as rev-pinned git dependencies.

## Dev loop
- `cargo xtask setup` installs tools (binstall) and initialises submodules.
- `bacon` for the watch loop; `cargo nextest run -p <crate>` for one crate.
- Format with `cargo xtask fmt` (nightly rustfmt; stable `cargo fmt` produces different output).
- `cargo xtask run host|app` to launch; `cargo xtask ios sim [--sim ipad]|device` for the phone/tablet;
  `cargo xtask bundle` builds a signed `Slopty.app` (app + daemons + CLI) under `target/bundle`
  with the icon rendered from `assets/icon.svg` (`cargo xtask icon` previews it);
  `cargo xtask ime [id]` switches the macOS input source for input-method tests.
- `cargo xtask upstream check` shows how far the GPUI and gpui-kit forks are behind upstream
  (bases in `xtask/upstream.toml`; the gate warns past 7 days); `cargo xtask upstream sync`
  rebases the forks in `.research/` under the main checkout, build-checks, pushes them
  (`SSH_AUTH_SOCK` on the signing agent first) and moves the `Cargo.lock` pins, stopping on
  any conflict that is not `Cargo.lock`. Then gate, e2e app + ios, and a DECISIONS entry.

## Gate
`cargo gate` is fmt, clippy `-D warnings` on all targets and all three triples, nextest,
doctests, rustdoc, deny, shear, typos, taplo and `committed`. It syncs the working tree into
`target/gate/tree` and checks that snapshot in parallel lanes on `target/gate/*` target dirs,
so the tree stays free to edit while it runs; what passed is the tree as it was when the gate
started. `--quick` is fmt + host clippy + tests; `--fix` runs the fixers on the tree first;
`--in-place` checks the tree itself (CI). Per-lane times are in the log.

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
