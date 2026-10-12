# Slopty

One app on macOS, iPhone and iPad for working on many remote machines as if they were one local
one: coding agents (Claude Code, Codex, pi, any ACP agent), terminals, remote windows and
desktops at Parsec quality or better, clipboard and files, all in tiled panes. Pure Rust on GPUI,
over Tailscale or a VPN. The person directs and reviews agents, so the GUI leads: what needs them
at a glance, fast review, everything else quiet, every surface feeling local. The terminal is one
action away and the editor stays light, with no LSP. It looks like Zed and Delta, Zed's agent
app. The person hands over whole goals and reviews only results.

Maps: `docs/ARCHITECTURE.md`, `docs/DECISIONS.md`, `docs/MEASUREMENTS.md`, `docs/DEV.md`,
`docs/TESTING.md`. Progress lives in the memory's notes.

## Hard lines

- Checks run where they return soonest: this Mac for a lane's own crates and Mac-only tests,
  GitHub runners (`cargo xtask remote`) for wide, Linux and iOS work. The Mac is the person's and
  they use it over Parsec, so local cargo runs one at a time under `nice -n 19` with
  `CARGO_BUILD_JOBS=4`, and never makes it hot or felt; `target/` stays under 30 GB. Landing
  is `cargo gate`, commit, `cargo xtask land`; CI's gate decides (`docs/DEV.md`). Parallel
  agents share this checkout: the `lanes` skill.
- Pre-release: no backward compatibility, no versioning. Replace cleanly and delete the old.
- Rust everywhere, scripts too. Latest toolchain, dependencies and forks; lints never loosened.
- Slopty drives an agent only through its published doors, never by reading its screen, never
  answers for the person, and never touches a credential (`docs/decisions/agents.md`).
- A decision worth a paragraph goes in `docs/decisions/`, an optimisation's number in
  `docs/MEASUREMENTS.md`, in the same change. Docs paraphrase the person, never quote them.

## Session safety

- Tests decide on numbers. No synthetic input into a process, no screenshots of other windows,
  no reading images back: that pattern has been flagged as surveillance tooling.
- Agents under test are stand-ins the test starts, never a signed-in agent.
- Never read old Claude Code transcripts (`~/.claude/projects/**/*.jsonl`).
- Images may be opened only for design review: Slopty's renders (`crates/slopty-e2e/golden/`,
  `target/e2e/artifacts/`) and `.research/mockups-*/`.
- Delta may be taken apart, and run here signed in with the person's GitHub on scratch folders
  only, capturing only its windows; values only, none of its code or assets. Its one key is the
  person's CPA proxy, from `$CPA_API_KEY` via the clipboard, never typed or printed.
- Kill daemons a measurement started. Worktrees and ports are shared with other sessions.
