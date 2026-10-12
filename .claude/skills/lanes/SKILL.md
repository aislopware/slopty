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
- A lane checks its edits where they return soonest: `cargo clippy` and `cargo nextest` on its
  own crates here at full power, and with `SLOPTY_LANE=<its name>` set,
  `cargo xtask remote --only <its paths>` for the wide, Linux and iOS part (`--full` before a
  checkpoint, since a quick check skips rustdoc). Checks run in the background while the lane
  moves on, and it reports a checkpoint on green.
- Lanes do not commit. The orchestrator stages each checkpoint by path, runs one quick gate
  over the batch, commits by owner and lands at once. A red gate is fixed forward by the lane
  that owns it.
