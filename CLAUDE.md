# Slopty — working rules

Read `docs/ARCHITECTURE.md` (the map) and `docs/DECISIONS.md` (rulings + evidence) first.
`docs/knowledge-from-slop-desk.md` is *unverified* prior art: treat every claim there as a
hypothesis until DECISIONS.md marks it verified.

## Non-negotiables
- Pure Rust. Scripts are `cargo xtask <cmd>`; never add shell scripts, Makefiles or a justfile.
- Floor macOS 26.5 / iOS 26.5. No availability checks, no fallbacks.
- Commit messages follow Conventional Commits, linted by `committed` (commit-msg hook + gate):
  `feat|fix|perf|refactor|docs|test|build|ci|chore|style|revert(scope)?: imperative summary`,
  `!` or a `BREAKING CHANGE:` footer for breaking changes. Releases are derived from them:
  `cargo xtask release` (git-cliff computes the version, regenerates `CHANGELOG.md`, commits
  `chore(release): vX.Y.Z`, tags). Never edit `CHANGELOG.md` or the workspace version by hand.
- `cargo gate` must pass before a commit: fmt, clippy `-D warnings` (all targets, all three
  triples), nextest, doc, deny, shear, typos, taplo. Never `#[allow]` a lint without a
  `reason = "..."`; never weaken `[workspace.lints]` to make something compile.
- Every `unsafe` block has a `// SAFETY:` comment naming the framework or ABI rule it relies on.
  Apple framework keys/constants come from the objc2 statics, never string literals.
- Wire types live in `slopty-proto` only, with golden byte snapshots (insta) under
  `crates/slopty-proto/tests/snapshots`. A changed snapshot is a protocol change: bump
  `PROTOCOL_VERSION` and accept it with `cargo insta review`.
- No `std::sync::Mutex`, no `thread::sleep` in libraries, no `unwrap`/`expect` outside tests.
- Measure before optimizing; the numbers go in `docs/MEASUREMENTS.md` with the command that
  produced them.

## Layout
`crates/*` libraries, `apps/*` binaries, `xtask/` automation, `vendor/` pinned submodules
(ghostty, zed fork, gpui-kit fork), `docs/` design + decisions.

## Dev loop
- `cargo xtask setup` installs tools (binstall) and initialises submodules.
- `bacon` for the watch loop; `cargo nextest run -p <crate>` for one crate.
- Format with `cargo xtask fmt` (nightly rustfmt; stable `cargo fmt` produces different output).
- `cargo xtask run host|app` to launch; `cargo xtask ios sim|device` for the phone.
