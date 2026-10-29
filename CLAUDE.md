# Slopty

One app on macOS, iPhone and iPad for working on many remote machines as if they were one local
one: coding agents in their own TUIs (Claude Code, Codex, pi), terminals, remote windows and
desktops at Parsec quality or better, clipboard and files, in a sidebar and a side pane. Pure
Rust on GPUI, over Tailscale or a VPN. Terminal and TUI first: the person works in each agent's
own TUI, always complete and current, and the GUI supports it with what needs them at a glance
and a read-only inspector of the running session that keeps only what helps inspect it, down to
the TUI's own prompt box live (its draft and pasted images). Every surface feels
local and the editor stays light, with no LSP. Its interface starts from zeron's UI (MIT),
copied and made Slopty's own, and takes the best of the premium references wherever they are
more elegant; `cargo xtask upstream adopt` keeps taking zeron's interface improvements. The
person hands over whole goals and reviews only results; frontier models get open tools to
orchestrate with, never a prescribed workflow.

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
- Slopty reads an agent only through its own records and published doors (hooks, transcript),
  never its screen; types into an agent's terminal only what the person (or an orchestrating
  agent's `agent_tell`) wrote, never at a dialog; never answers for the person; and never
  touches a credential (`docs/decisions/agents.md`, "TUI-first").
- A decision worth a paragraph goes in `docs/decisions/`, an optimisation's number in
  `docs/MEASUREMENTS.md`, in the same change. Docs paraphrase the person, never quote them.
