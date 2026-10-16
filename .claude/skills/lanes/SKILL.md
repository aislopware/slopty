---
name: lanes
description: How parallel coding lanes (subagents) work in this one checkout — ownership, remote checks, checkpoints and who commits. Use when starting or briefing a subagent, or when working as one.
---

# Lanes

The outcome: several agents move at once in one checkout, none waits on another, and none
breaks another's work. The detail and its evidence: `docs/decisions/tooling.md`, "Lanes never
wait"; the commands: `docs/DEV.md`, "Dev loop".

- Each lane owns a disjoint set of crates or files, named in its brief, and touches nothing
  else. A change to a shared crate (`slopty-proto`, `slopty-core`, the workspace `Cargo.toml`)
  has one owner and lands first; work that depends on it follows rather than runs beside it.
- A lane keeps its crates compiling between edits, since a half-edited crate breaks the others'
  remote checks. A lane blocked by another's half-edited crate reports rather than waits.
- A lane checks its edits with `SLOPTY_LANE=<its name> cargo xtask remote -p <its crates> --only
  <its paths>`: this Mac answers in about a minute, runners take the rest at once (`--full`
  before a checkpoint, since a quick check skips rustdoc). No cargo in the checkout itself.
  Checks run in the background while the lane moves on, and it reports a checkpoint on green.
- This Mac's cores are the limit, not the number of agents: at most 4 lanes at once, at most 2 of
  them building GUI crates here. A lane spawns no sub-agents, keeps
  one check in flight under its own lane name (a new name is a cold target dir), and sends
  crates that test on Linux to runners with `--place runner`; only GUI crates check here.
- Lanes do not commit. The orchestrator stages each checkpoint by path, runs one quick gate
  over the batch, commits by owner and lands at once. A red gate is fixed forward by the lane
  that owns it.
