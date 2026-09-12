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
