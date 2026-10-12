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
`docs/knowledge-from-slop-desk.md` is unverified prior art; trust nothing in it until checked.

## The outcome

- The person mostly directs, watches and reviews agents. The GUI shows what needs them at a
  glance, makes review fast and keeps everything else quiet, across every worker as if it were
  one machine.
- Every surface runs where the work is and feels local: intents show at once, streams arrive
  incrementally, clients cache. Latency and smoothness come before features, so a change on the
  input, terminal or frame path carries its number in `docs/MEASUREMENTS.md`.
- The terminal is the best there is and always one action away. The remote desktop, input,
  audio, clipboard and files are polished to a fine grain. The editor stays light, with no LSP.
- It looks and feels like Zed and Delta, Zed's agent app (`.claude/rules/design.md`).
- Readiness comes before new performance projects.

## Lines that hold

- Pure Rust everywhere, scripts included (`cargo xtask …`). Floor macOS 26.5 / iOS 26.5 on
  Apple silicon: no availability checks, no fallbacks. Latest stable toolchain, dependencies
  and forks; the strictest lints, never loosened to make something build.
- Pre-release, so no backward compatibility: a format, protocol, setting or API is replaced
  cleanly and the old path deleted. Wire types live in `slopty-proto` with insta goldens, and
  nothing is versioned, since every binary is rebuilt together.
- Agents speak one agent-neutral thread model, fed on the worker by one adapter per agent:
  Claude Code observed through its TUI, Codex over its app-server, pi over its RPC mode, any
  other over ACP. The agent's own session is the truth, and its TUI can always take it over.
  Slopty acts only through the agent's published doors, never reads the screen to control it,
  never answers for the person, and launches the user's own unmodified binary without touching
  a credential.
- Every `unsafe` block names the framework or ABI rule it relies on. Apple constants come from
  the objc2 statics; a `CFSTR` constant is spelled once, beside its use, naming its header.
- Every behaviour has a test at the lowest layer that can see it (`docs/TESTING.md`). A decision
  worth a paragraph gets its entry under `docs/decisions/` in the same change. Docs paraphrase
  the person in English and never quote them.
- Nothing compiles on this Mac: its cores and disk are the person's. Builds, lints and tests run
  on GitHub runners through `cargo xtask remote`; only the quick gate and xtask build here.
- What lands passes the quick `cargo gate` here, then CI's full gate on the `gate` branch
  (`docs/DEV.md`, "Landing a change"), with a Conventional Commits message. `CHANGELOG.md` and
  the version derive from the messages and are never edited by hand.
- Parallel agents share this one checkout, each owning a disjoint set of files, and only the
  session that started them commits (the `lanes` skill).

## How the work goes

- The person hands over whole goals and reviews results. Work continuously, rule on what comes
  up, and keep looking for better ideas, from the foundation to the UI. A session resumes from
  the memory's progress notes and starts by bringing the toolchain, dependencies and upstreams
  current (`docs/DEV.md`, "Dev loop").
- The forked and vendored upstreams (gpui-fast, gpui-kit, libghostty-rs, ghostty, noq) move
  daily. Stay synced, check their open pull requests before building in them, and never wait on
  them: finish a good open pull request in our fork. Judge what is taken, and every library on a
  hot path, by measurement.

## Session safety

- Checks run as tests and pass or fail on numbers. No synthetic input into a process, no
  screenshots of other windows, and no reading images back: that pattern reads as surveillance
  tooling and has been flagged.
- An agent under test is a stand-in the test starts: hook JSON on the worker's control socket
  (`CtlRequest::Hook`), `slopty hook` as the test's own child, or a stub replaying recorded
  fixtures. Never a signed-in agent, and never a command typed into a shell under test.
- Never read old Claude Code session transcripts (`~/.claude/projects/**/*.jsonl`).
- Images that may be opened, for design review only: Slopty's own renders
  (`crates/slopty-e2e/golden/`, `target/e2e/artifacts/`; granted 2026-09-15), generated mockups
  under `.research/mockups-*/` (2026-10-06), and Delta's under `.research/mockups-delta/`
  (2026-10-12). A golden still passes or fails on its numbers.
- Delta, at the person's request (2026-10-12), may be downloaded and taken apart, and run here
  signed in with their GitHub on scratch folders, never with organisation or private-repository
  access; only its own windows are captured. Values only: none of its code, icons or assets
  enter Slopty. Its only key is the person's CPA proxy (`cpa2.jmango360.dev`), pasted from
  `$CPA_API_KEY` through the clipboard, never typed or printed.
- Daemons started for a measurement die when it ends. Worktrees and ports are shared with other
  sessions.
