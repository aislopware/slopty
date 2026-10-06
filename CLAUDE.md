# Slopty

One app on macOS, iPhone and iPad that reaches many remote hosts at once. It runs coding agents
(Claude Code, Codex, pi and any ACP agent) and terminals, streams windows and whole desktops at
Parsec quality or better, shares the clipboard and moves files either way. Everything sits in
tiled panes: each project holds tabs, and each tab a split layout. Hosts are reached over
Tailscale or a VPN, so the wire adds no encryption or pairing of its own. The aim is that
working on many remote machines feels like working on one local one. Pure Rust on GPUI.

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
- A commit is made only on a green `cargo gate`, and lands on main only once CI's full gate on
  the `gate` branch is green, with a Conventional Commits message. `committed` lints it, and
  releases derive from the messages, so `CHANGELOG.md` and the version are never edited by hand.
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
- **Priorities.** GUI-first and agent-native. The person mostly directs, watches and reviews
  agents, so the GUI leads: what needs them at a glance, fast review, everything else quiet,
  across every worker as if it were one machine. Every surface runs where the work is and
  streams to the client with local-feel latency (intents shown at once, incremental streams,
  client caches). The terminal stays the best there is and always one action away. The remote
  desktop, input, audio, clipboard and files stay polished to a fine grain. The editor stays
  light, with no LSP. Readiness comes before new performance projects.
- **Agents.** Every agent speaks one agent-neutral thread model, fed on the worker by one
  adapter per agent: Claude Code observed through its TUI by default, Codex over its app-server
  beside its own TUI, pi over its RPC mode, any other over ACP. The agent's own session is the
  source of truth: the worker's log is a cache rebuilt from it, one writer holds a session at a
  time, and its TUI can always take it over. Slopty acts only through the agent's published
  doors (its protocol, its hooks, or keys into its TUI on the person's word). It never reads
  the screen to control an agent, never types menu digits or cycles mode keys, and never
  answers for the person. It launches the user's own unmodified binary and never offers a login
  or touches a credential.
- **Design.** Minimal and modern, in MonoCode's school: one ground, panes meeting at hairlines,
  square where it is structure, rounding that grows with size. Every colour, size and spacing
  comes from the theme tokens, and the lint-as-tests in `crates/slopty-ui/src/kit.rs` enforce
  it. Honour Reduce Motion. Chrome text is sentence case. Keybindings go in the palette, not on
  buttons.
- **Autonomy.** The user hands over whole goals and reviews only the results. Work
  continuously without asking. Research, measure and improve everything from the foundation to
  the UI, and keep looking for new ideas. A session resumes from the memory's progress notes.
- **Start of a session: bring the ground up to date.** Run `cargo xtask upstream check` and
  `sync` what is behind (the gpui-fast and gpui-kit forks, libghostty-rs and `vendor/ghostty`),
  then `rustup update`, `cargo update`, and any gate tool that is behind (`docs/DEV.md` "Dev
  loop").
- **Upstream, continuously and never blindly.** gpui-fast lands commits every few minutes, so
  keep a watch on the forked and vendored upstreams (gpui-fast, gpui-kit, libghostty-rs, noq)
  and sync as they move. Check their open pull requests before building anything in them, so no
  work is duplicated. Judge every change taken, and every library on a hot path, on whether it
  and our use of it are optimal: measure, and improve at the root when they are not. Never wait
  on upstream. Finish a good unfinished idea (an open pull request) in our fork now, and
  reconcile when upstream lands it.
- **The quick gate checks the index here; CI's full gate decides what lands.** Stage exactly
  what you mean to land (`git add <paths>`). Then run
  `cargo gate -m '<message>' > target/logs/gate.log 2>&1; echo GATE_EXIT=$? >> target/logs/gate.log`:
  fmt, the tools, the locks and `committed`, which compile nothing and take seconds. Unstaged
  edits are invisible to it. Read `GATE_EXIT=0` or "gate passed" in the log, then
  `git commit -F target/gate/COMMIT_MSG` without restaging, and `cargo xtask land`. That lints
  the changed packages under `nice` (host and iOS clippy, rustdoc; no tests), then pushes to
  the `gate` branch, where CI runs every lane (clippy, tests, rustdoc) and fast-forwards main
  once all pass; the app's e2e runs there on a schedule. Keep working meanwhile. A red run
  names its lane and tests in the run's summary, and the fix lands on top. The wrapper's own
  exit code is the `echo`'s. Batch several changes per land.
- **Parallel work happens in this one checkout, with no worktrees.** Split the work by
  ownership. Each subagent owns a disjoint set of crates or files, named in its brief, and
  touches nothing else.
  - A change to a shared crate (`slopty-proto`, `slopty-core`, the workspace `Cargo.toml`) is
    owned by one agent and lands first. Work that depends on it is sequenced after it, not run
    alongside it.
  - An agent keeps its crates compiling between edits, because a half-edited crate breaks
    everyone's build. It finishes with host `cargo clippy -p <crate> --all-targets` and its own
    tests, then reports. It never runs `cargo xtask check` or any other lane across triples: the
    slow checks belong to the session that started it, once per batch. An agent blocked by
    another agent's half-edited crate reports rather than waiting for it.
  - Subagents do not commit. The session that started them stages the batch, runs one gate
    over it, and commits by owner.

## Session safety

- Checks run as tests, never by hand. That means no synthetic keys into a pid, no screenshots of
  other windows, and no reading images back: that pattern reads as surveillance tooling and has
  been flagged. A *pass or fail* is decided on the diff numbers, never by eye.
- To test an agent, play it with a stand-in the test starts: hook JSON to the worker's control
  socket (`CtlRequest::Hook`), `slopty hook` as the test's own child, or a stub that replays
  recorded stream-json, app-server or RPC fixtures. Never type a command into a shell under
  test, and never run a signed-in agent in a test.
- Never read old Claude Code session transcripts (`~/.claude/projects/**/*.jsonl`).
- One exception, granted 2026-09-15: Slopty's own renders (`crates/slopty-e2e/golden/*.png` and
  `target/e2e/artifacts/*.png`) may be opened **for design review**. They are this app's own
  output, not anybody's screen, and a numeric diff cannot see a defect the golden itself encodes.
  Everything above still holds: no other window, no image from outside those two directories, and
  a golden still passes or fails on its numbers.
- A second, granted 2026-10-06: design mockups an image model generates for reference (Codex's
  image generation) may be opened for design review, kept under `.research/mockups-*/`. They are
  generated pictures of a design, not anybody's screen; nothing else above changes.
- Daemons started for a measurement are killed when it ends. Worktrees and ports are shared
  with other sessions.
