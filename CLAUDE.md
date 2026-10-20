# Slopty

One app on macOS, iPhone and iPad for working on many remote machines as if they were one local
one: coding agents (Claude Code, Codex, pi, any ACP agent), terminals, remote windows and
desktops at Parsec quality or better, clipboard and files, in tabs and a side pane. Pure Rust on
GPUI, over Tailscale or a VPN. The person directs and reviews agents, so the GUI leads: what
needs them at a glance, fast review, everything else quiet, every surface feeling local. The
terminal is one action away and the editor stays light, with no LSP. Its interface starts from zeron's UI
(MIT), copied and made Slopty's own, and takes the best of the premium references (Claude Code
desktop, T3 Code, opencode, Amp) wherever they are more elegant; `cargo xtask upstream adopt`
keeps taking zeron's interface improvements. The person hands over whole goals and reviews only results; frontier models get
open tools to orchestrate with, never a prescribed workflow.

Maps: `docs/ARCHITECTURE.md`, `docs/DECISIONS.md`, `docs/MEASUREMENTS.md`, `docs/DEV.md`,
`docs/TESTING.md`. Progress lives in the memory's notes.

## Hard lines

- Checks run where they return soonest: `cargo xtask remote -p <crate>` checks a lane's crates
  on this Mac at full power in the lane's own warm directory, while runners take iOS, Linux,
  rustdoc, e2e and probes at once; no cargo in the checkout itself. Landing is `cargo gate`,
  commit, `cargo xtask land`; CI's gate decides (`docs/DEV.md`). Parallel agents share this
  checkout: the `lanes` skill.
- Pre-release: no backward compatibility, no versioning. Replace cleanly and delete the old.
- Rust everywhere, scripts too. Latest toolchain, dependencies and forks; lints never loosened.
- Slopty drives an agent only through its published doors, never by reading its screen, never
  answers for the person, and never touches a credential (`docs/decisions/agents.md`).
- A decision worth a paragraph goes in `docs/decisions/`, an optimisation's number in
  `docs/MEASUREMENTS.md`, in the same change. Docs paraphrase the person, never quote them.
