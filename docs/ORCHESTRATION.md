# Orchestration: sessions, models, briefs

Slopty is built by several coding-agent sessions in parallel, each in its own git worktree and
`herdr` pane, coordinated by one orchestrator session that never edits code. This document is the
routing policy: which model does which kind of work, what a brief contains, and how work lands on
`main`.

## Roles

- **Orchestrator** (Claude Code, Fable 5.1): reads the resume state, cuts the work into independent
  tracks, writes one brief per track, starts the sessions through `herdr`, and lands each branch by
  `git rebase` onto `main` followed by one `cargo gate`. It listens for the single report message
  each worker sends and never polls.
- **Workers**: one session per track, one track per worktree under `slopty-wt/<track>` on branch
  `<kind>/<track>`. A worker never merges, never touches `main`, never pushes the Slopty repo,
  never edits another worktree, and verifies only through the test layers (unit, headless GPUI,
  app self-test, gated live `e2e`).

## Model tiers

Escalate a track by one tier only after a lower tier clearly tried with full context and still
got it wrong. That is a capability gap; more effort on the same model does not fix it.

| Tier | Agent | Best for | Never for |
|---|---|---|---|
| A | Claude Code `--model fable` | open-ended tracks across crates, new wire types, the GPUI fork and iOS platform work, fork rebases with conflicts, latency work that needs measurement judgment, anything with `unsafe` or `objc2` | small mechanical batches |
| B | Claude Code `--model opus` | clearly specified features inside one or two crates, subtle bugs in a known area, `DECISIONS.md` and `ARCHITECTURE.md` writing, review of tier C and D branches before landing | fork surgery |
| C | Claude Code `--model sonnet` | API-rename fallout after a pin bump, goldens, lint fixes, tests copied from a named pattern, `xtask` commands with an exact spec, doc formatting | design decisions, protocol changes |
| D | `pi` (Gemini 3.8 Flash, thinking high) | bounded jobs the gate can judge: pedantic `clippy` cleanup, fixture and test generation from a spec, mass edits from a shown diff, summarising an upstream changelog, `MEASUREMENTS.md` tables from logs, documents from given content | design, wire types, `unsafe`, the GPUI fork, anything that needs taste |

Verification: a tier C or D branch is reviewed by a tier B session and then gated; a tier A or B
branch is gated only.

## Starting a session

```text
git worktree add -b <kind>/<track> /Volumes/Lacie/Workspace/oss/slopty-wt/<track> main
git -C /Volumes/Lacie/Workspace/oss/slopty-wt/<track> submodule update --init --recursive
herdr workspace create --cwd <worktree> --label "<kind> <track>" --no-focus
herdr pane move <pane> --tab <orchestrator tab> --split right|down --target-pane <pane> --no-focus
herdr agent start <track> --kind claude --pane <pane> -- --model opus --effort high
herdr agent start <track> --kind pi --pane <pane> -- --thinking high
herdr agent prompt <track> "$(cat /tmp/slopty-brief-<track>.md)"
```

Reuse a clean worktree whose `target/` is warm instead of creating a new one when possible: a cold
GPUI build costs minutes.

## Brief shape

Every brief is self-contained. A worker sees nothing else.

1. **Where**: worktree, branch, the coordinator's session name, the sibling tracks and the files
   they own.
2. **Goal**: the finished state, numbered, with the user-visible behaviour per item.
3. **Context**: files to start from, prior commits to read, gotchas, isolation (`SLOPTY_DATA_DIR`
   under the worktree's `target/`), the pedantic `clippy` rules spelled out, the gate recipe
   (background run, read `GATE_EXIT=0` in the log, no edits while it runs), commit rules
   (Conventional Commits, `SSH_AUTH_SOCK=/tmp/scc-agent.sock`).
4. **Rules**: never merge, never `main`, never push, verify only through tests, never type commands
   into a running shell to simulate anything, never screenshot or open an image, never read old
   session transcripts.
5. **Done when**: the acceptance criteria and the single report message (branch head, commits, gate
   log path, tests added, what was left out and why).

Per tier:

- Tier A and B: "infer intent, carry to completion without asking"; judgment is expected.
- Tier C: add explicit acceptance criteria, the exact file list and a pattern to copy; say "do not
  redesign".
- Tier D (Gemini): direct, concise instructions; put the task and its checklist at the end of the
  prompt, after the context; positive rules ("use `u32::try_from`") instead of broad negatives; the
  exact verify commands (`cargo clippy -p <crate> --all-targets`, `cargo nextest run -p <crate>`,
  `typos`); ask for the report as a file under `/tmp` because the pane's alternate screen cannot be
  read back. `pi` runs with the user's full permissions: scope it to its worktree explicitly.

## Landing

1. Read the worker's report. For tier C and D, start a tier B review session on the branch first.
2. `git rebase main` in the worktree, then one `cargo gate` from the main checkout on the rebased
   branch, then fast-forward `main`. `main` stays linear; the gate runs `committed
   --no-merge-commit`.
3. When two tracks both bumped `PROTOCOL_VERSION`, the second lander re-bumps and re-accepts the
   `client_hello` golden.
4. After a fast-forward, diff `main~N..main -- docs` and check that no doc lines were lost.
5. Update the resume note and tell the remaining sessions to `git rebase main`.

## Isolation

Each session gets its own worktree, its own `target/`, its own `SLOPTY_DATA_DIR`, and the `e2e`
harness already uses temporary directories, so sessions never share daemons, sockets or ports.
