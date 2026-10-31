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
  xtask is every lane's tool and `cargo xtask` builds it from this tree, so an xtask edit enters
  the tree only once it compiles (written in the lane's scratch copy first). A lane whose
  `cargo xtask` does not build runs the last good build, `target/xtask/debug/xtask`, which a
  failed build leaves in place: `remote` checks `HEAD` plus its own `--only` paths either way.
- A lane checks its edits with `SLOPTY_LANE=<its name> cargo xtask remote -p <its crates> --only
  <its paths>`, quick (no `--full`): fmt, host clippy, rustdoc and the tests, here in about a
  minute. The gate is the full check; `--full` (iOS clippy here, Linux clippy on a runner) is
  for a change to `cfg(target_os = "ios")` or Linux-only code, not for every checkpoint. Crates
  that test on Linux may go to runners with `--place runner`, quick only: a full check there
  takes a runner Mac for the Mac's own tests.
  `remote` formats the lane's changed Rust files under `--only` and runs the gate's tool steps
  its change reaches before it checks, and the first red ends the check, so a red comes back in
  seconds: fix it and check again. No cargo in the checkout itself, and no runner or VM on
  this Mac. Checks run in the background while the lane moves on, and it reports a checkpoint
  on green.
- E2e runs once per round, after every lane's work has landed, on hosted runners: the cases the
  round's commits reach and one golden accept from the run's artifacts, reviewed by eye, with
  its reds going back to the lanes that own them. A red e2e case is a bug in the product or the
  test, never retried. A checkpoint lands on its quick check; a behaviour is proved by a
  headless GPUI or model test in that check, and a new e2e case is written then but first runs in
  the round's suite. A lane runs a filtered case (`--e2e app -E 'test(...)'`; after a red `-E
  failed`) only to chase a bug no headless test can reach, and accepts no goldens itself.
- This Mac's cores are the limit, not the number of agents: at most 4 lanes at once, at most 2 of
  them building GUI crates here. A lane spawns no sub-agents and keeps one check in flight under
  one name, the same every time: `<dir>-<lane>` with the directory its brief names (`L-H`,
  `M-X`). Checks sharing a directory take it in turn only while their work there runs, and one
  that moves the lock works in a warm clone of its own.
- Lanes do not commit. A checkpoint is a green `--only` check of exactly its paths, named by
  the `<name>@<sha>` its verdict prints: the orchestrator stages what that check passed
  (`cargo xtask stage <name>@<sha>`, never `git add`),
  runs one quick gate over the batch, commits by owner and lands at once. A red gate is fixed
  forward by the lane that owns it.

- Each lane runs as the agent its work needs (`.claude/agents/`; `docs/decisions/tooling.md`,
  "Lanes run on the model their work needs"): `lane` (Opus) for anything that needs judgment
  (causes, flakes, design, UI and golden review, upstream adoption, synthesis) and whenever the
  shape is unclear; `mechanic` (Sonnet) only for a change the brief specifies site by site with
  the check that proves it; `runner` (Sonnet) to run long checks and report them; `reader`
  (Sonnet) to gather facts into a file. A `mechanic` that meets a choice the brief did not make
  stops and reports, and an Opus lane takes it on from its notes. Every Sonnet diff gets the
  orchestrator's adversarial review before it is staged, and a runner's report is judged by
  Opus, never acted on as read.
- Every turn rereads a lane's whole context, so a lane reads logs by their verdict lines, never
  prints what it has already read, batches small reads into one command and never polls; and
  follow-up work in a lane's own files goes to that lane while its context is warm, not to a
  new one.
