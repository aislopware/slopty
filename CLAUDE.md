# Slopty

One app on macOS, iPhone and iPad that reaches many remote hosts at once. It runs terminals and
Claude Code agents, streams windows and whole desktops at Parsec quality or better, shares the
clipboard and moves files either way. Everything sits on a niri-style scrolling workspace.
Hosts are reached over Tailscale or a VPN, so the wire adds no encryption or pairing of its own.
The aim is that working on a remote Mac feels local. Pure Rust on GPUI.

Maps: `docs/ARCHITECTURE.md` (how it is built), `docs/decisions/` (rulings with their evidence;
`docs/DECISIONS.md` is the index), `docs/MEASUREMENTS.md` (numbers and the commands behind them),
`docs/DEV.md` (commands and the loop), `docs/TESTING.md` (the test layers).
`docs/knowledge-from-slop-desk.md` is unverified prior art from an earlier attempt; trust nothing
in it until checked here.

## What must hold

- Pure Rust, everywhere: the app, the daemons, and every script (`cargo xtask …`).
- Floor macOS 26.5 / iOS 26.5, Apple silicon; no availability checks, no fallbacks.
- Latest stable toolchain, dependencies and forks. The strictest lints, all of them on. An
  `#[allow]` carries a `reason`. `[workspace.lints]` is never loosened to make something build.
- Latency and smoothness come before features. A change on the input, terminal or frame path
  comes with a number, and an optimisation follows a measurement recorded in
  `docs/MEASUREMENTS.md`.
- A commit lands only on a green `cargo gate`, with a Conventional Commits message. `committed`
  lints it, and releases derive from the messages, so `CHANGELOG.md` and the version are never
  edited by hand.
- Every `unsafe` block states the framework or ABI rule it relies on. Apple constants come from
  the objc2 statics. A `CFSTR` macro constant is spelled once, next to its use, naming its header.
- Wire types live in `slopty-proto` with insta goldens. A changed golden is a wire change. Nothing
  is versioned: every binary is rebuilt together (pre-release).
- Libraries hold no `std::sync::Mutex`, no `thread::sleep`, no `unwrap`/`expect`.
- Every behaviour has a test at the lowest layer that can see it (`docs/TESTING.md`). A decision
  worth a paragraph gets its entry under `docs/decisions/` in the same change. Docs paraphrase
  the user in English and never quote their prompts.

## How the work goes

- **Pre-release, so no backward compatibility.** Replace a format, protocol, setting or API
  cleanly. Delete the old path, shims, serde defaults kept for old files, and aliases. Never
  layer compatibility on top.
- **Priorities.** The terminal and the remote desktop come first: streaming, input, audio,
  clipboard and files, polished to a fine grain. Claude Code support stays at the status level:
  which shell runs an agent, and whether it is working, waiting or blocked. Nothing drives an
  agent or renders its transcript; agents are TUI-only.
- **Design.** Minimal and modern, in the Warp and Zed school. Every colour, size and spacing
  comes from the theme tokens, and the lint-as-tests in `crates/slopty-ui/src/kit.rs` enforce
  it. Honour Reduce Motion. Chrome text is sentence case. Keybindings go in the palette, not on
  buttons.
- **Start of a session: bring the ground up to date.** Run `cargo xtask upstream check` and
  `sync` what is behind (the zed and gpui-kit forks, libghostty-rs and `vendor/ghostty`), then
  `rustup update`, `cargo update`, and any gate tool that is behind. Read what changed upstream
  and adopt what helps; do not just move pins (`docs/DEV.md` "Dev loop").
- **The gate checks the index, in the background.** Stage exactly what you mean to land
  (`git add <paths>`). Then launch
  `cargo gate > target/logs/gate.log 2>&1; echo GATE_EXIT=$? >> target/logs/gate.log`. Keep
  working while it runs; unstaged edits are invisible to it. Read `GATE_EXIT=0` or "gate
  passed" in the log, then `git commit` without restaging. The wrapper's own exit code is the
  `echo`'s. Batch several changes per gate.
- **Parallel work happens in this one checkout, with no worktrees.** Split the work by
  ownership. Each subagent owns a disjoint set of crates or files, named in its brief, and
  touches nothing else.
  - A change to a shared crate (`slopty-proto`, `slopty-core`, the workspace `Cargo.toml`) is
    owned by one agent and lands first. Work that depends on it is sequenced after it, not run
    alongside it.
  - An agent keeps its crates compiling between edits and checks only its own crates, because
    a half-edited crate breaks everyone's build. `cargo xtask check -p <crate>…` runs the gate's
    steps (fmt, clippy on every triple, tests, rustdoc, shear, typos) on just those crates; a
    clean run is what an agent reports.
  - Subagents do not commit. The session that started them stages one subagent's files, gates
    that index, commits it, and moves on to the next.

## Session safety

- Checks run as tests, never by hand. That means no synthetic keys into a pid, no screenshots of
  other windows, and no reading images back: that pattern reads as surveillance tooling and has
  been flagged. A *pass or fail* is decided on the diff numbers, never by eye.
- To test agent status, send hook JSON to the worker's control socket (`CtlRequest::Hook`) or spawn
  `slopty hook` as the test's own child. Never type a command into a shell under test.
- Never read old Claude Code session transcripts (`~/.claude/projects/**/*.jsonl`).
- One exception, granted 2026-09-15: Slopty's own renders (`crates/slopty-e2e/golden/*.png` and
  `target/e2e/artifacts/*.png`) may be opened **for design review**. They are this app's own
  output, not anybody's screen, and a numeric diff cannot see a defect the golden itself encodes.
  Everything above still holds: no other window, no image from outside those two directories, and
  a golden still passes or fails on its numbers.
- Daemons started for a measurement are killed when it ends. Worktrees and ports are shared
  with other sessions.
