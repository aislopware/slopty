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

## Tests (four layers, fastest first)
1. **Unit**, in every crate: pure logic behind traits with fakes (`slopty_input::Recorder`
   for the injector, `Camera`/`CanvasDoc` in `slopty-client`). Runs under `cargo gate`.
2. **Headless GPUI**, `#[gpui::test]` in `slopty-ui`: a `VisualTestContext` window with real
   layout, `simulate_keystrokes`/`simulate_click`, a channel for the host. Read the UI like
   a DOM: `cx.debug_bounds("item-<uuid>")` (set with `.debug_selector`), `window.painted_quads()`
   for colours and borders, view accessors (`rows()`, `zoom()`, `active_item()`),
   `slopty_ui::a11y::tree(window)` after `window.set_a11y_active(true)` for roles, labels and
   the focused node. No pixels, no process, runs under `cargo gate`. Every canvas/terminal
   behaviour gets a test here first.
3. **App self-test**, `cargo xtask e2e app` (`crates/slopty-e2e`, gate `SLOPTY_APP_E2E`):
   launches ptyd + hostd + the app (built with `--features slopty/e2e`) in a temp dir, pairs
   them, and drives the app over its own control socket (`SLOPTY_TEST_SOCKET`: keys, clicks,
   dump, render). `dump` is the structured state (items, focus, zoom, terminal rows, and
   `a11y`: the accessibility tree as role/label/value/focused/bounds in reading order);
   `render` is GPUI drawing its own window to a PNG, compared numerically with
   `crates/slopty-e2e/golden` (`--accept` writes missing and failing goldens, `--accept-all` rewrites every golden). Nothing touches another app.
   `cargo xtask e2e ios [--sim iphone|ipad]` (gate `SLOPTY_IOS_E2E`) is the same socket with
   the app in the simulator: the way to check anything on the phone or the tablet. There the
   socket also takes `ui_key_press` / `ui_touch` / `ui_pinch` / `ui_insert_text` /
   `ui_delete_backward` (`Driver::ui_key`, `ui_tap`, `ui_pan`, `ui_pinch`, `ui_insert_text`),
   delivered at the UIKit boundary by the fork (`tests/ios_uikit.rs`): use them for anything
   about how the phone's own keyboard, fingers or key bar reach the app.
   `cargo xtask e2e pair` (gate `SLOPTY_PAIR_E2E`, serial) runs two app processes on one host,
   each on its own socket and data dir, to prove many-clients-one-server: a terminal opened on
   one shows on the other, typing on both is serialised, attention badges both, a client dying
   leaves the other streaming and reattaches on relaunch, closing and notes propagate. The
   display scenario also needs `SLOPTY_SCREEN_E2E`. `cargo xtask e2e pair-ios [--sim iphone|ipad]`
   (gate `SLOPTY_PAIR_IOS_E2E`) puts the second client in the simulator: the Mac and the phone
   on one host. `cargo xtask e2e hosts` (gate `SLOPTY_HOST2_E2E`, `SLOPTY_HOST2=<ssh name>`,
   serial) is one client and two hosts on two machines: ptyd + hostd on a second Mac over ssh
   (temp root, private HOME, torn down after), to prove cross-host attention — the pill sums
   both hosts, a banner routes to the host holding its session, and a hook is played only
   through `slopty hook` over ssh, never a real agent.
4. **Live desktop**, `cargo xtask e2e host|screen|input|all` (gates `SLOPTY_SCREEN_E2E`,
   `SLOPTY_INPUT_E2E`): real capture and real event posting, own data dir under `target/e2e/`.
   The assertions live inside those tests.

Rules for a session:
- Never drive a check by hand (synthetic keys into a pid, per-window screenshots, reading
  screenshots back). That combination reads as surveillance tooling and got several sessions
  flagged. Add a test at the lowest layer that can see the behaviour and run it.
- Never open an image in a session, not even one the app rendered itself. Read the diff
  numbers (`differing/total`) and the `.diff.png` path from the test output; the golden
  workflow exists so the model never has to look at pixels.
- Never read, grep or dump old Claude Code session transcripts (`~/.claude/projects/**/*.jsonl`)
  into a session: they replay the flagged pattern. To investigate a flag, look at metadata
  only (timestamps, tool names, `model_refusal_fallback`), never at the payloads.
