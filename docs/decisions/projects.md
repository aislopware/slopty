# Projects

A project is one goal worked on by many agents across the fleet. The user talks to one
orchestrator agent. It splits the goal into tasks, hands each task to an agent on a worker that
can do it (a Linux worker for the server and the Linux crates, a Mac for anything Apple), and
merges what they finish. Every agent in the tree can be seen and opened while it works. Nothing
runs where the user can't follow it.

The research behind these rulings, with sources, is in `.research/projects-research-2026-09-30.md`
(primitives, products, distribution, measured cost) and `.research/projects-foundations-2026-09-30.md`
(what Slopty already has).

**The orchestrator is a Claude Code session with Slopty's tools, not a driver.** ✅ 2026-09-30
- The orchestrator is an ordinary interactive `claude` session in a PTY tile. What Slopty adds is
  a tool surface: the existing MCP verbs (`spawn_agent`, `agent_status`, `read_conversation`,
  `answer_permission`, `events`, the file and terminal verbs), plus the project verbs below. The
  agent chooses to call them, and the TUI stays the source of truth, as for every agent.
- Rejected:
  - **Claude Code's own Projects as the backbone.** It is a public beta on Pro and Max, and its
    orchestrator conversation lives at claude.ai, off the tailnet.
  - **Agent teams.** They are experimental, off by default, allow no nested teams, and put no
    teammate in a worktree.
  - **Wrapping the CLI and parsing its screen.** Omnara abandoned exactly that as "unfeasible to
    maintain".
- Kept for later: bridging to Claude Code Projects through Remote Control, if the user wants to
  steer from claude.ai too.

**Two levels by default, and every level is a visible session.** ✅ 2026-09-30
- The tree is: orchestrator → one sub-orchestrator per area or host class → worker agents. Inside
  one host, a sub-orchestrator may fan out with Claude Code's own subagents or workflows, since
  those are cheap and share the host's worktrees.
- The measurements favour shallow, centralised trees. Google's scaling study measured
  independent agents amplifying errors 17.2× and centralised ones 4.4×, and coordination
  collapsed throughput in Cursor's experiments.
- Every session Slopty spawns is a PTY tile with the brief typed as its first prompt
  (`spawn_agent` already does this). A native subagent inside a session appears as a child node
  from its `SubagentStart`/`SubagentStop` hooks, and its transcript opens read-only in the
  conversation face.

**Project state lives on the server.** ✅ 2026-09-30
- The server holds, in a store beside `workers.json`:
  - the project: its name, repository, target branch, verifier command and orchestrator session;
  - its tasks: title, brief, owned paths, needs, parent task, state, assigned worker and
    session, branch, the verifier's result and the merge;
  - an append-only event timeline.
- Workers report; clients mirror, as they mirror the item registry. The project outlives any
  client, and a phone sees the same tree as the Mac.
- `AgentBranch` stops being dropped at the worker's server link, so the hub learns every PR and
  worktree.

**Code moves through a hub repository on the server, over Slopty's own link.** ✅ 2026-09-30
- The server keeps a bare repository per project. Each worker keeps a mirror of it and gives
  every task a `git worktree` on branch `slopty/<project>/<task>`.
- Fetch and push go through `git-remote-slopty`, a git remote helper that carries git's
  pack protocol over the worker's existing server link. So there is no SSH setup and no second
  credential, and the link is the one everything else already trusts.
- `.git` is never file-synced (Mutagen documents why not). Worktrees on one host share objects;
  across hosts they fetch.

**A task says what it needs, and the server places it.** ✅ 2026-09-30
- A task's needs are an OS (Linux or macOS), the Apple SDK, a display or capture, or a named
  worker. The server picks among the workers that meet them, by `WorkerCaps` (os, installed
  agents, cpus), current load and whether a mirror is already there. The orchestrator or the
  user can pin a task to a worker.
- Work that builds and tests on Linux goes to a Linux worker when one is up, which keeps the
  Macs free for Apple work.

**Tasks own disjoint paths.** ✅ 2026-09-30
- A task names the paths it owns, and the server refuses a claim that overlaps a live task's.
  This is the rule this repository already works by: one owner per crate or file, and a shared
  crate lands before the work that depends on it.
- Cognition and Cursor both found that parallel writers to shared code fail. Reads are free.

**A task is done when its verifier passes; the server merges one at a time.** ✅ 2026-09-30
- The project names its verifier (`cargo gate` here). A finished task's branch runs it on a host
  that can, and the result is recorded on the task.
- The server's merge queue then rebases each passing branch onto the target branch, runs the
  verifier again, and fast-forwards, one branch at a time. A conflict or a failure goes back to
  the owning agent as a message with the details. There is no integrator agent: Cursor found
  that role became the bottleneck.
- Before a merge, the orchestrator can ask a reviewer with fresh context (a new session that sees
  only the diff and the brief). Cognition reports that such a reviewer catches about two bugs per
  pull request.

**The user's plan quota bounds concurrency.** ✅ 2026-09-30
- All agents draw on one Claude plan (a 5-hour window and a weekly cap). Multi-agent runs cost
  7-15× the tokens of one session.
- A project caps its live agents (4 per host by default), and the project view shows each
  agent's tokens as the transcript reports them.

**What the user sees.** ✅ 2026-09-30
- A project opens as a tile. It shows:
  - the tree, each node with its worker (and OS), branch, state (working, waiting, blocked,
    verifying, merged), tokens and last line;
  - a timeline of the events that matter (spawned, blocked on you, verifier passed or failed,
    merged, conflict);
  - at the top, anything waiting on the user, such as an approval or a question.
- Clicking a node opens that agent's tile, as its TUI or its conversation face. The composer
  talks to the orchestrator only. Minimal, in the Warp and Linear school, and every colour a
  token.

**Tests use a stub agent.** ✅ 2026-09-30
- End-to-end tests run a stub `claude` (a test binary that speaks the hook protocol and makes
  scripted commits), never the real one. This proves the project end to end without spending
  quota:
  1. The orchestrator spawns.
  2. A task is placed on the Linux container worker (`cargo xtask linux run`) and pushes through
     `git-remote-slopty`.
  3. The verifier runs, the merge queue merges, and the tree and timeline show each step.

**Deferred.** ⏸ 2026-09-30
- A shared build cache across workers: sccache with the server as its backend. It helps only
  dependency rlibs, so measure it first.
- A channel that pushes completions into the orchestrator's session (a research preview).
- Moving a running agent to another worker (A8).
- Bridging to Claude Code Projects.

## Phases

1. **Wiring and state.**
   - Spawned agents get `slopty mcp` through `--mcp-config` and `SLOPTY_SERVER`,
     `SLOPTY_PROJECT` and `SLOPTY_TASK` in their environment.
   - The project and task store on the server, with verbs in `slopty-proto::orchestration`
     served through `slopty-tools`.
   - `spawn_agent` gains `project`, `task` and `parent`.
   - `SubagentStart`/`SubagentStop`/`TaskCreated`/`TaskCompleted` are forwarded, and so is
     `AgentBranch`.
   - A first project tile shows the tree and opens nodes.
2. **Code across machines.**
   - `git-remote-slopty` and the server's bare repositories.
   - A worker verb that prepares a mirror and a worktree for a task.
   - Placement by needs.
   - A worktree setup file (A3).
   - Linux worker hardening: systemd, x86_64 and a real network e2e.
3. **Verify and merge.**
   - The verifier runs per task, and the merge queue on the server.
   - The fresh-context reviewer.
   - The timeline, and tokens per agent.
4. **Scale.** The shared build cache, channel push and moving agents between workers, each only
   after it is measured.
