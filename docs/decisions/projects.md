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
  - its tasks: title, brief, kind, owned paths, placement rules, parent and dependencies, state
    and status text, assigned worker and session, branch, the verifier's result and the merge,
    and free-form metadata;
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

**Placement is open: rules over what each worker says of itself.** ✅ 2026-09-30
- A fixed list of needs (an OS, the Apple SDK, capture) cannot say "the box with the GPU", "a
  Mac on AC power" or "where the nightly toolchain is". So each worker reports open facts, and a
  task's placement is rules over them, in the phase 1 rulings below. The user asked for this
  after reviewing the first design.
- A pin is never overridden. An orchestrator that reads the facts and pins a task is as
  first-class as one that writes rules. `placement_suggest` shows the ranking with its reasons
  before anything starts.
- Mirror presence becomes a fact once mirrors exist (phase 2).

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
- A project caps its live agents per worker and in all. The person's bounds in the server's
  settings cap every project and the fleet as a whole. The project view shows each agent's
  tokens as the transcript reports them.

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

## Phase 1, as built (2026-09-30)

**A project is a slug; a task is a number within it.** ✅ 2026-09-30
- A project's id is 1-40 characters of `[a-z0-9-]`, neither starting nor ending with a dash. It
  is checked as it decodes, so a bad name never reaches the store. It names the branch
  `slopty/<project>/<task>` and reads well in a tool call.
- Tasks are numbered from 1 per project (`3` or `#3`). They are not global ids, because an
  agent says "task 3" and the orchestrator reads it back.
- Wire types are in `slopty-proto::project`. The verbs are in `slopty-proto::orchestration`:
  `ProjectCreate`, `ProjectSet`, `ProjectList`, `ProjectStatus` (a long poll on a timeline
  cursor), `TaskCreate`, `TaskClaim`, `TaskUpdate`, `TaskAssign`, `TaskSpawn`, `TaskReport`,
  `TaskGet`, `WorkingOn`, `PlacementSuggest` and `WorkerFacts`. They are served as MCP tools
  (`project_create` … `task_spawn`, `task_report`, `task_get`, `placement_suggest`, and
  `list_workers` with the facts) and as `slopty project …`, `slopty task …` (with
  `task suggest`, `task report` and `task get`) and `slopty workers --json`.
- A task's verifier run names the commits it judged (`VerifierRun { head, base }`, in hex), so
  a verdict never outlives the work it saw.

**The task model is open.** ✅ 2026-09-30
- A task has a free-form `kind` (up to 64 characters), a `status` text beside its fixed state
  (up to 512), and `metadata`, any JSON object up to 16 KiB. A project has metadata too.
- `depends_on` names other tasks of the project. The graph must stay acyclic, so a dependency
  that closes a cycle, or names an unknown task, is refused.
- Tasks nest to any depth up to the project's `depth` limit.
- A task runs a `Runner`: Claude Code with its prompt and arguments, or a command (any other
  agent's CLI, a build, a script; the login shell when empty). A task may name its own verifier
  over the project's.
- A read-only task owns no paths. It can run beside any writer, and a read-only task that
  names paths is refused.
- Every move between states is checked (`TaskState::may_become`). Merged is final, and only a
  done or verifying task merges. A move back into a live state claims the task's paths again,
  so reopening a finished task cannot slip past a claim made since.

**Workers report open facts.** ✅ 2026-09-30
- A fact is a flag, a number, a word, or a list or map of them (`Fact`). Each worker sends its
  facts on the server link (`ToServer::Facts`) when they change. They are gathered lazily and
  cached (`slopty-worker::facts`), never on a hot path:
  - installed agent CLIs and toolchains with their versions, Rust targets, GPUs;
  - AC or battery;
  - the person's `[worker.labels]` (read as `labels.<name>`);
  - `[worker.probes]` shell commands, run every 10 minutes at low priority with a time limit,
    read as `probes.<name>`.
- The server adds what it knows itself, over anything a worker sent under the same name: name,
  worker, os, os_version, arch, cpus, memory_mb, encoders, displays, capture and input, load,
  online, live_agents and repos. Facts travel beside `WorkerInfo`, not in it, so the directory's wire
  shape is unchanged. `list_workers` and `WorkerFacts` show them all.

**Placement rules are CEL.** ✅ 2026-09-30
- `Placement { pin, require, prefer: [(expr, weight)], near, avoid }`:
  - Every `require` rule must hold. A rule that errors, such as one reading a missing fact,
    does not hold, and says why.
  - Each `prefer` rule adds its weight when true, or its value times the weight when it is a
    number (`load / double(cpus)` weighted -20).
  - `near` and `avoid` name tasks or workers and move the score by 100. They steer and never
    refuse.
  - A worker at the project's per-worker limit does not fit.
  - The tie-breaks are fewer live agents, then load per cpu, then name.
- Every fact is a variable by its name, and all of them are also in a `facts` map, so
  `has(facts.gpus)` asks whether a worker reported one.
- Why CEL:
  - It is a published language that always terminates: no loops and no recursion, so its
    cost grows only with the rule and the facts it reads. Kubernetes (admission policies, CRD validation) and Envoy use it for this job,
    so agents already write it.
  - `cel` compiles a rule to a program and reports a bad one with its line and column.
    It evaluates over plain values, so facts map to it directly.
  - It gives arithmetic, `in`, map access and `has()` without our writing a grammar.
- Rejected:
  - **A hand-rolled `{fact, op, value}` matcher.** It would need its own `and`/`or`, lists,
    maps, arithmetic and error positions, and would grow with every need.
  - **Rhai or Lua.** Turing-complete, so every rule needs a sandbox and a fuel limit.
  - **JSONLogic.** Verbose for an agent to write, and it has no positions in its errors.
- The rules are agents' input, and the parser recurses. So a rule is at most 1024 bytes,
  nested at most 32 deep, and a placement has at most 32 rules. A test runs the worst of them
  on a 2 MiB thread. The cost is about 2.3 MB of server binary.
- CEL terminates, but a comprehension over a comprehension over a long list still costs its
  product. So a rule's worst case is counted as it compiles: every node once per element of
  each comprehension around it, a list read from the facts counted as 1024 elements, the most
  a worker's fact may hold. A rule over 65 536 steps, or a placement over 262 144, is refused
  before it runs, with the count. A worker's facts are cut to those bounds as they arrive (1024
  items per list or map, 4 KiB per text, 64 KiB in all).
- The same rule gives the same verdict every time: `cel` is patched to our fork at upstream
  master (`aislopware/cel-rust`), which iterates a map in key order and lets a comprehension
  absorb an error that a later element settles. The fork's own change sorts keys only when a
  comprehension walks a map. The fork also builds the standard library once per process
  instead of once per context, which placement builds per worker.
- Ranking runs outside the hub's lock, on the blocking pool, two at a time, and stops judging
  after 2 s; a rule not judged by then does not hold. Ranking 32 workers under four rules takes
  0.22 ms, compile included (`docs/MEASUREMENTS.md`). The chosen worker is then reserved
  under the lock, which checks the cap again.
- `placement_suggest` ranks every worker for a task or for rules given, each with its reasons
  (rule, held, points, detail). A pinned worker fits over every rule, and the rules it failed
  still show. A pin to a worker that is offline or full is refused; it never moves elsewhere.

**Every limit is a setting, under the person's bounds.** ✅ 2026-09-30
- A project's `Limits` are live agents per worker (4), live agents in the project (12), task
  depth (8) and timeline entries kept (4096). They are set at `project_create` and changed with
  `project_update`.
- The person's `[server.projects]` bounds in `settings.toml` cap them: live agents across the
  fleet (24), per worker (8), per project (24), depth (16) and timeline entries (65 536). The
  server reads the file at start (`Hub::set_policy`). Agents read the bounds and the live
  counts in `project_status`, and cannot raise them. A limit set above its bound is refused,
  naming the setting.
- Only protective rules stay hard: the plan quota, consent and approvals, and disjoint write
  claims.

**Counts follow live terminals, not task states.** ✅ 2026-09-30
- A task's run counts from the moment it is placed until its terminal ends, whatever its state
  says. A start whose caller left, an assign that failed, or an agent that marked its own task
  done cannot escape the cap. A start counts for up to 30 s after its worker answers, until
  the worker announces the terminal.
- The hub chooses the id of every terminal it starts (`SpawnAgent` and `OpenTerminal` carry
  it), and a caller never can. So a start whose answer was lost still counts for its 30 s, and
  when its worker announces that id the terminal goes on its task as if the answer had come.
  An agent cannot name another's terminal as its own start.
- A command task counts like an agent, since it may be another agent's CLI.
- The fleet bound counts every live terminal with an agent in it, in a project or not, plus
  every start in flight. A plain `spawn_agent` is refused at the bound as a task's start is.
- A project's orchestrator counts only while its terminal is live.
- A terminal its worker has opened but not yet announced may be assigned at once, and counts
  for the task's project from then.
- A terminal an agent opens counts against the fleet bound as a start does, and a terminal an
  agent typed into counts as an agent from then, so a plain shell cannot carry an agent past
  the bound. An assign is refused when the project is at its live or per-worker limit, unless
  the terminal is already counted there. An assign cannot take a terminal whose start another
  task still waits on.

**An agent never has more than the person gave it.** ✅ 2026-09-30
- Every link says whom it speaks for. A client is the person and an MCP surface is an agent.
  The CLI inside a Slopty terminal is an agent when an agent runs there, when the terminal works
  on a project, when an agent opened it or typed into it, or when the server does not know it.
  So an agent cannot borrow the person's word through a shell, its own or one it drives.
- A program proves which terminal it runs in with the token its worker made for that terminal
  (`SLOPTY_SESSION_TOKEN`, in every session's environment): a keyed blake3 of the session id
  under the worker's own key (`session.key`, 0600, in its data directory), which the worker
  sends the server when it registers. The worker checks the token itself for what it hands
  over locally, and the server checks it against every registered worker's key. Before, the
  server minted a token only for the terminals it started, so an agent in any other terminal
  proved nothing, and `slopty hook reports` took any session's batch from any same-user process.
- An agent changes the projects only from a terminal it proves works in one, and only within
  it (`hub::projects::agent_scope`). An unproven agent (an MCP surface, the CLI with no token)
  changes none.
  - An orchestrator works in its whole project. Only an orchestrator, or the person, makes a
    project or sets one.
  - A task's agent works in its own task and what is split from it. A task it creates goes
    under its own task unless it names a parent inside that subtree, so what it splits off
    counts against the project's depth like everything else. Before, it could root tasks at
    the top of the tree, past the depth limit.
  - A terminal an agent names as orchestrator or assignee is its own, one it opened, one the
    project holds (its orchestrator's, a task's, a start's for it), or one another agent of the
    project opened. The person's own terminals are the person's to give. Before, an agent could
    draw any live terminal into a project.
- The terminals an agent opened or typed into, and those a task's start holds, are kept in the
  projects store beside the projects (`Keep::Watch` and `Keep::Unwatch` in its log, `watched` in
  its snapshot). A server restart keeps them, so the shell an agent opened still speaks for an
  agent and stays its project's. An entry is dropped once its terminal is gone from a worker
  whose link is up, never from one that is away.
- Claude Code's folder trust is written only for a folder strictly inside the one Slopty makes
  its own in (`slopty_agent::trust::trust`, `within`). An ancestor, or a worktree whose `.git`
  file leads outside, is refused, since trusting its key would trust the person's repository.
- Only the person answers a permission (`answer_permission` from an agent is `Forbidden`),
  merges a task, records its verifier, or names a verifier for a project or a task: a verifier
  is the person's word on what counts as done. Only the person's read of a conversation holds its
  prompts, so an agent reading another's never hides a prompt from the TUI.
- Every way to start `claude` is judged by its arguments against an allowlist
  (`slopty_proto::project::SAFE_FLAGS`): `spawn_agent`, `task_spawn`, and a terminal whose
  command runs `claude`, directly, through a runtime (`node …/claude`) or anywhere in a shell
  line (`sh -c "cd x && claude …"`, after assignments or `exec`/`env`/`nohup`). The shell line is
  split as POSIX does, quotes and operators included (`slopty_agent::detect::shell_agent_args`).
  - A flag not on the list loosens, and so does a bare `--`, after which Claude Code reads
    anything. A new Claude Code flag is therefore refused until it is judged, instead of passing
    until someone adds it to a denylist.
  - Some flags are judged by their value: `--permission-mode` only as `default`, `plan`,
    `dontAsk` or `manual`; `--settings` only with keys that cannot widen a permission (deny and
    ask rules, the bypass lock, the model, the theme…), and hooks or a status line only when
    they run this worker's own `slopty hook …`; `--mcp-config` only when it names nothing but
    the `slopty mcp` server; `--plugin-dir` only as the worker's own mod.
  - `--debug-file` is not on the list, since it writes wherever it is pointed.
- They are allowed only for the projects the person names in `[server.projects]
  permission_flags`, and only to that project's own agents (`hub::projects::allowance`): an
  agent has the allowance of the project it proves it works in, and names no other's. No agent
  can start another with more than it has. Before, any agent that named such a project had it.
- An agent's environment for a new terminal cannot steer what runs there: `PATH`, `HOME`,
  `ZDOTDIR`, `SHELL`, `BASH_ENV`, `ENV`, `XDG_CONFIG_HOME` and anything starting `SLOPTY_`,
  `CLAUDE`, `ANTHROPIC_`, `NODE_`, `BUN_`, `DYLD_`, `LD_` or `GIT_CONFIG` are refused with
  `Limit`. The worker applies the request's environment first and its own last, so its hooks,
  mod and session id always win.
- Flags are one door. A settings file in the repository, which an agent may have written, is
  another, and keys typed into a TUI a third. So an agent the server starts also:
  - begins in `--permission-mode default` when its arguments name no mode;
  - has bypass mode locked off (`disableBypassPermissionsMode: "disable"` in the settings the
    worker adds, `slopty_agent::hooks::without_bypass`).
- Two backstops watch what actually runs, on every terminal the server started or an agent
  opened or typed into:
  - The mode each hook reports: a mode looser than those closes the terminal, with a note on its
    task saying why, unless the person allows looser modes for the project.
  - The worker judges the argv of the agent in each terminal's foreground with the same
    allowlist and reports what loosens it (`AgentReport::Loosened`, at most 16 items of 256
    bytes, sent when it changes). So `claude` started inside a shell by any means, which no
    start check sees, is closed the same way. The worker knows its own `slopty` and mod paths,
    so it can accept its own hooks and MCP config by value; the server cannot and refuses them.
- An agent may not type into another agent's TUI (`Forbidden`), except in the person's
  `permission_flags` projects: what reaches an agent from another goes through reports and
  hooks. Nothing may type into an agent that waits on the person (a permission, a question) or
  whose composer holds the person's unsent text (`AwaitsPerson`), nor into one no hook has yet
  spoken from (`AgentNotReady`).
- A report's hook ends its turn only once: at a `Stop` that a hook already held
  (`stop_hook_active`), reports wait for the next prompt or the inbox, so reports that keep
  coming never keep an agent from resting. A task that asks in a loop reaches its parent once a
  minute with its latest question, not once per report.
- The idempotency keys are scoped by caller and compared by a blake3 digest of the whole verb,
  so one caller cannot learn or replay another's answer under the same key. A start whose
  worker answered is replayed with that answer, and one that was lost in flight (interrupted,
  worker unreachable) is not remembered, so its retry starts it.
- What is left, known:
  - The CLI says it speaks for the person when it finds no Slopty variables in its
    environment (a session id that does not parse, or a token alone, speaks for an agent); a
    process outside Slopty's terminals that clears them is trusted as the person.
  - Allow rules in `.claude/settings.local.json`, and `/permissions` typed into a TUI, are
    Claude Code's to honour and are not read; the mode backstop still watches the result.
  - Anything running as the person's user can reach the worker's socket and speak as a
    client, and can read the worker's key and so any session's token. Tailscale bounds who
    reaches a host; the uid is the boundary on it.

**Claims are prefixes at component granularity, compared as APFS compares names.** ✅ 2026-09-30
- A path is normalised to `/`-separated components relative to the repository root, and to
  NFC. The empty path is the root and owns everything. `crates/a` overlaps `crates/a/src/x.rs`;
  it does not overlap `crates/ab`.
- Paths compare case-folded, as the default APFS volume does, so `Docs` and `docs` are one
  claim.
- A glob owns what comes before its first wildcard, so it never owns less than it matches.
  Any false conflicts are the safe kind.
- A task holds its paths until it is merged or failed. A claim that overlaps a live task's is
  refused with `Conflict`, naming the task and the path. Overlap is checked within a project
  only, since two projects are two repositories.
- Symlinks are not resolved: the server sees no worker's disk. The worktree that phase 2
  prepares can resolve them on the worker.

**A caller's own task is the server's record first.** ✅ 2026-09-30
- With no project or task named, a tool acts on the caller's own. That is the task whose live
  assignment is the caller's terminal (`SLOPTY_SESSION`), and `SLOPTY_TASK` only when the
  server has none. A stale or inherited environment therefore never moves the wrong task.
- The defaults apply only inside the caller's own project. A tool naming another project has
  no own task there and must name one.

**Task state follows its agent only while it works.** ✅ 2026-09-30
- In running, waiting or blocked, the task follows its agent's `AgentStatus` (working → running,
  waiting → waiting, blocked → blocked). Verifying, done, merged and failed are set by a verb,
  never by an agent's status.
- Only a move into blocked is written to the timeline. Working and waiting flip on every turn,
  and the kept entries would fill with them.
- When a session closes, the task's assignment ends with an `AgentGone` moment. The task keeps
  its state for the orchestrator to judge.
- A worker that registers again is reconciled against its session list. A task whose terminal
  ended while the server was away is freed, instead of waiting for a close it will never hear.

**`task_spawn` is the linkage; `spawn_agent` keeps its wire shape.** ✅ 2026-09-30
- The server places the task's run and forwards an ordinary `SpawnAgent`, or `OpenTerminal`
  for a command. `SLOPTY_PROJECT` and `SLOPTY_TASK` go last in its env, so they win over the
  caller's. The new terminal is then assigned to the task.
- The start is detached from its caller, so a caller that goes away mid-start still leaves its
  terminal on the task. A terminal that its task can no longer take (merged meanwhile, say) is
  closed, not left running outside any count.
- The MCP `spawn_agent` tool's `project`, `task` and `parent` arguments route through it. When
  no task is named, one is created and titled from the prompt's first line.
- The worker's own spawn adds `--mcp-config=<json>` naming `slopty mcp` on stdio. The flag is
  variadic in Claude Code, so the `=` form keeps the next argument from being taken as a second
  config.
- Every session a worker starts carries `SLOPTY_SERVER` when the worker has a server. So
  `slopty mcp` and `slopty` in any shell find it with no flags, and Claude Code passes it on to
  its MCP servers.
- A task's agent starts with a conversation id the server chose (`--session-id`, unless its
  arguments pick one), kept in its assignment (`Assignment.conversation`), so the task knows
  its transcript before the first hook.
- A task's agent is told its role (`--append-system-prompt`): its project and task, its paths,
  whom it reports to and how (`task_report`), and the project's `agent_rules` from its metadata
  when set. The orchestrator is told its own, with `orchestrator_rules`, as its first delivery
  (below), since the person started it and the server adds no flags to it. A rules text is at
  most 2 KiB.

**Workers report their agents' tree on the server link.** ✅ 2026-09-30
- `ToServer::Report(AgentReport)` carries:
  - `Branch(AgentBranch)`, which was dropped at the link before;
  - `SubagentStarted` and `SubagentStopped`;
  - `NativeTask`, from the `SubagentStart`, `SubagentStop`, `TaskCreated` and `TaskCompleted`
    hooks.
- Claude Code's internal helper subagents are skipped. A report lands on the task whose live
  agent is that session, or on the project whose orchestrator it is. Each node keeps its last
  256 natives, beside the tasks rather than inside them.
- A report that arrives before its session is assigned is held, for the last 256 sessions, and
  lands when the session is assigned.
- Natives do not go on the timeline. A stop updates its start's entry in the tree, so a
  hundred subagents cost a hundred nodes, not two hundred timeline entries.
- On connect, the worker sends every branch it knows. So a server restart does not lose a pull
  request.

**Project changes are deltas, with a sequence number to drop stale ones.** ✅ 2026-09-30
- Every change logs `Happening::Project(ProjectUpdate)` on the hub's event log. It carries
  only what changed: the project record, the one task, the one native, and the timeline entry
  when there is one, with the task as its card. A task's status change pushes 417 bytes, where
  the project's whole status is 506 KB at 10 projects of 200 tasks.
- A client gets `FromServer::Projects(ProjectsPart)` after `Terminals` on connect, and again
  after a lag. Its `seq` is the last event the snapshot includes, taken under the same lock. A
  client must drop any `Project` event at or below it, since a replay after a lag would
  otherwise undo newer state.
- Change verbs go through the hub's idempotency ledger (the last 1024 keys), so a retried
  `task_spawn` or `slopty open` never starts a second terminal; the key is the caller's own
  (above).
- The snapshot carries each task as its card (no brief, no natives; `task_get` has the rest)
  and comes in parts of at most 8 MiB, marked first and last; a project too large for one part
  goes on in the next. So a large fleet never builds a frame over the wire's bound. It is
  742 KB at 10 × 200 tasks, down from 1.83 MB.
- Everything a project holds is bounded as it comes in, so no agent can grow a frame past what
  the wire takes: a timeline page is at most 1 MiB, the entries kept at most 8 MiB, an update's
  events at most 8 MiB, with each field's own bound (title, note, artifacts, branch).

**The store appends a log and never loses a file.** ✅ 2026-09-30
- The hub sends every durable change to a keeper, which keeps its own replica and appends the
  change as one JSON line to `projects.log` once a burst settles (250 ms). Nothing the store
  does holds the hub's lock: a change costs the keeper about a microsecond, where cloning the
  state under the lock cost 0.7 ms four times a second (`docs/MEASUREMENTS.md`, round 2).
- The keeper writes the whole file again, compact and atomic (`slopty_platform::fs::replace`),
  when the log passes 8 MiB and at shutdown, then starts the log again. Each line is numbered
  and the snapshot names the last it holds, so a read replays only the lines past it.
- A file that fails to read for any reason other than not existing stops the server
  (`ServerError::State`), instead of starting empty and writing over it later.
- A snapshot that does not parse is renamed `<name>.bad-<ms>` (with `-n` when that name is
  taken), and the server starts empty. Every bad file is kept.
- A log line cut short at the log's end, a crash mid-append, is passed over. A bad line before
  the end sets the whole log aside as `.bad-<ms>`, since what follows it may depend on it.
- A record is recounted as it loads (its timeline's bytes), so no count is trusted from disk.
- `project_status` returns the latest 64 timeline entries unless given a cursor.

**Reports go up through hooks.** ✅ 2026-09-30
- A task's agent reports to the node that split its task off, its parent task's agent or the
  project's orchestrator (`task_report`: checkpoint, needs input, stuck, done, with a note,
  artifacts, a branch or a pull request). The report lands on the task's timeline at once.
- Reports wait per node on the server (`slopty-server::deliver`) and go when their kind says:
  a need at once; a block at once but at most once per task every 3 minutes; a finish once it
  has settled for 2 minutes, a later report replacing it; a checkpoint with whatever goes next,
  or after an hour. A batch is at most 9000 bytes, and each node has one batch outstanding.
- A batch goes to the worker of the node's live terminal (`FromServer::Deliver`), which keeps
  it in a file for that session beside its control socket. A node with no terminal waits,
  parked until its next terminal opens.
- The agent's own `SessionStart`, `UserPromptSubmit` and `Stop` hooks run
  `slopty hook reports`, synchronously. It asks the worker for its session's batch over the
  control socket with the session's token (`CtlRequest::Reports`), and the worker answers what
  to print: the batch as the hook's `additionalContext`, or for `Stop` as `decision: block`
  with the reports as the reason, so a finishing agent reads them before it ends its turn. The
  batch stays kept until the hook says it printed it (`CtlRequest::ReportsHanded`, with the
  token again). Only then does the worker drop it and send the `Delivered` report that acks
  it, and the server marks a `Delivered` moment for the tasks' reports it held. A hook that dies in between leaves the batch
  for the next one. Keeping, handing over and dropping run one at a time per worker, so a
  batch kept meanwhile is never dropped in its place. A batch not acked is sent again when the worker registers again,
  folded into the next one, or put back when the terminal closes.
- Report text cannot close its `<slopty-reports>` block, so a report never reads as the
  server's own words.
- Why hooks, not typing: the TUI is the source of truth and nothing types into it behind the
  person's back. Hooks are Claude Code's own door for context, and they reach an agent the
  moment it next thinks, at no cost while it is idle.

**The timeline shows reports delivered, not an orchestrator's role.** ✅ 2026-10-01
- The server's own words to an orchestrator, its role once it is named, go in a batch like a
  report's. Their `Delivered` moment landed wherever the agent's first hook happened to fall:
  before a client's snapshot or after it, so the `project-timeline` golden held 13 entries or
  14. A wait in the harness only hid it.
- The role is standing context, not an event: a batch of the server's words alone marks no
  moment, and `Delivered { reports }` counts only the tasks' reports in a batch
  (`deliver::reports_in`). A report delivered is still marked when it happens, so the timeline
  says what reached whom and nothing that depends on when an agent started.
- `a_report_reaches_the_orchestrator_through_its_worker` holds the moments to exactly the one
  report delivered.

**A repository is the same on every machine that has a clone.** ✅ 2026-10-01
- A path names a clone, not a repository: `/w/slopty` on the studio and `/home/c/slopty` on a
  Linux worker are one repository. A summary carries the repository's identity beside its
  path (`SessionSummary::repo_id`, `RepoId { origin, root }`):
  - `origin` is the origin remote's URL, else the first remote's, normalized to `host/path`:
    the host lowercased, with no scheme, user, port, `.git` or slashes around the path. HTTPS,
    SSH and scp-like spellings of one remote are one string. A clone of a local path has none.
    It is read from the config file every worktree shares (through `commondir`), with no git.
  - `root` is the first commit of `HEAD`'s first-parent chain, which every clone shares however
    far each has come. It is one `git rev-list --first-parent --max-parents=0 HEAD`. A shallow
    clone and a repository with no commit have none.
  - Two identities are one repository when either key matches (`RepoId::same`).
- The worker identifies a repository the first time a summary asks, in the background, and
  keeps it while a session is in it (`repo::Identities`). The sessions in it are sent again
  once it is known. One found without a first commit is asked again after 30 s.
- The server adds `repos` to each worker's facts: every key of every repository a shell there
  is in, mapped to the clone's path. `"github.com/o/r" in repos` places a task beside a clone,
  and `repos["github.com/o/r"]` says where. An orchestrator's role names its repository by that
  key and lists the workers with a clone and their paths.
- The navigator's "By repository" lens is to group by the same keys, so one repository cloned
  on two workers is one group. The grouping is `slopty_ui::repo_groups::group`, a pure
  function: clones join when their identities match or they share a path, transitively, and
  a group is keyed by its least origin, else its first commit, else its path, so a fold kept
  under the key holds whatever order the tiles come in. The navigator's lens groups by it
  (`docs/decisions/ui.md`, "The repository lens groups clones by what they are").
- Tests: `an_origin_is_the_same_however_it_was_spelled`,
  `the_origin_is_read_from_the_config_worktrees_share`, `clones_share_their_first_commit`
  (real git), `a_repository_is_identified_once_and_its_sessions_told`,
  `a_repository_is_a_fact_of_every_worker_with_a_clone` and
  `the_orchestrator_is_told_where_its_repository_is_cloned`.

**A task with no directory starts beside a clone, in a worktree of its own.** ✅ 2026-10-01
- An orchestrator could start a task only in a directory it named, and it cannot know the
  paths on another machine. Two agents started in one checkout also edit the same files.
- A project learns its repository's identity from the shell its orchestrator works in
  (`Project::repo_id`): when the project is created or its orchestrator named, and whenever
  that terminal's summary first names one. It is learned once, so the orchestrator walking into
  another checkout later does not move where tasks go.
- `task_spawn` with an empty `cwd` adds a rule to the task's placement: either key of that
  identity is in the worker's `repos`. The task then starts in the clone's root on the worker
  chosen. When no worker fits, the refusal says the task needed a clone, and of what.
- An agent that writes starts with `--worktree slopty-<project>-<task>`, unless its arguments
  name a worktree already. Claude Code makes the worktree under the clone's `.claude/worktrees/`
  on branch `worktree-<name>`, and reopens it when the task is started again. Its status line
  reports it, so the board shows the branch as for any agent. A read-only task runs in the clone
  itself. The agent's role says where it works and which branch to report.
- Why Claude Code's own `--worktree` rather than a worker verb that runs `git worktree add`:
  it is a safe flag the server already lets through. It reopens a worktree by name, it
  honours the person's `worktree.baseRef` and `WorktreeCreate` hook, and the worktree it makes
  already reaches the board through the status line. A verb of our own would duplicate all of
  that and still need the status line to report it.
- The orchestrator's role says this, and that work needing no Apple platform belongs on a
  Linux worker, said once as the project's needs (below).
- Tests: `a_task_with_no_directory_goes_beside_a_clone_in_a_worktree_of_its_own` (hub, with a
  Linux clone found by its first commit only), and
  `a_task_with_no_directory_starts_in_a_worktree_of_the_project_s_clone` in
  `apps/slopty-cli/tests/projects.rs`. In that test a real worker identifies a real git
  repository with an origin, the project learns it, and the stub `claude` starts in the clone
  with `--worktree slopty-demo-1`.
- That test found a race: a terminal opened through the server could be answered before the
  worker's summary of it arrived, so naming it a project's orchestrator at once was refused as
  an unknown terminal (2 of 30 runs at 8 at a time). The worker now sends the summary of a
  terminal it opened ahead of the answer, as it already did a resize's
  (`a_worker_registers_answers_forwarded_verbs_and_comes_back` holds the order); 40 of 40
  runs passed after.

**An idle agent is woken through its inbox, and the hooks still decide.** ✅ 2026-09-30
- Claude Code takes messages from other processes on a socket it names in each session's
  environment (`CLAUDE_CODE_MESSAGING_SOCKET`, 2.1.224; `CLAUDE_CODE_MESSAGING_TOKEN`, 2.1.228;
  checked against 2.1.285). A message starts a turn in an idle session and is read between tool
  calls in a busy one. The reports hook notes the session's socket and token on every run, and
  the worker posts each batch there as it arrives, marked with a fresh nonce.
- A post obeys the rules for typing into an agent (`orchestrate::may_type`): nothing is posted
  while the agent waits on a prompt that is the person's, while the person's unsent text is in
  its composer, before a hook has spoken from it, or after it exited. The batch then waits for
  the hook that comes once that clears (the person's prompt, the turn's end, the agent's
  start). Before, a post could start a turn in the middle of the person's answer or draft.
  A draft the person abandons unsent holds the batch until the agent's next hook.
- The post only wakes. Claude Code may hold or refuse what arrives (`crossSessionInbound`, or a
  session that bypasses prompts holding messages from one that does not) and answers nothing.
  So the batch stays in its file and the post is noted beside it. The next hook looks for the
  mark in its prompt or the transcript's last 4 MiB: when it is there the message was read and
  the hook only acknowledges it; when not, the hook hands the batch over itself. A report is
  never acknowledged unread, and read twice at worst.
- Rejected:
  - **`asyncRewake`.** A hook that exits 2 wakes an idle session, but it must keep running
    until a report comes, under the hook's own time limit, one process per agent.
  - **Channels.** An MCP server pushing into a session is the research preview's door, gated
    per account and started with a flag; the messaging socket is on in every session.
  - **Typing the reports.** It drives the TUI behind the person's back.
- `apps/slopty-worker/tests/server_link.rs` proves both paths against the stub `claude`, which
  speaks the socket: a woken agent's turn acknowledges the batch without a second copy, and a
  held message leaves the batch for the next prompt's hook. It also proves that no batch is
  posted while the agent waits on the person, and that a batch is asked for and acknowledged
  only under its own session's token.

**A hook that comes before its terminal's open returns waits for it.** ✅ 2026-10-01
- Claude Code fires `SessionStart` as it starts, which can be before the worker's open of its
  terminal has returned and put the session in the worker's table. The control socket used to
  refuse such a hook ("no such session"), so the agent never reported through its hooks and
  nothing was ever typed into it. A session being opened is now marked before the open starts,
  and a hook for it waits for the open to finish (`Worker::get_opened`). A session nobody is
  opening is still refused at once.
- Found as a load-sensitive failure of
  `an_agent_started_for_a_task_has_the_tools_and_grows_the_tree` (3 of 47 runs under a load
  average near 40). With the fix, 40 of 40 runs passed at load 40–56.

**Tests use a stub `claude`.** ✅ 2026-09-30
- `slopty-stub-claude` (in `slopty-testkit`) records its argv, env and MCP configs. It fires
  every hook its `--settings` registers for each payload it is given, as Claude Code does, and
  keeps what each printed. `STUB_LATER` fires more once a gate file appears, as a later turn's.
  It calls one tool through the `slopty` MCP server of its `--mcp-config`, after a gate file
  appears when a test sets `STUB_MCP_AFTER`.
- `apps/slopty-worker/tests/server_link.rs` proves reports end to end: a server batch reaches a
  stub agent through its next prompt's hook as context, the hook's ack comes back as
  `Delivered`, and nothing is typed into its terminal.
- `apps/slopty-cli/tests/projects.rs` runs it under a real server, ptyd and worker. It proves:
  - the env, the MCP config, the hooks, the tool defaulting to the agent's own project, and
    the tree with its natives and branch;
  - labels and probes from the worker's settings reaching `slopty workers --json`, a CLI-made
    command task placed by rules over them, ranked by `task suggest`, and run with its
    project and task in its env;
  - an agent started with a stale `SLOPTY_TASK`, then assigned to another task, updating the
    task the server says it is on.
- The model (`slopty-server::project`), the placement (`slopty-server::placement`) and the hub
  (`hub/project_tests.rs`) test each ruling above at their own layer. The hub tests cover
  concurrent starts at the cap, a caller that leaves, a lost start adopted, the fleet bound,
  the permission flags on every path (shell lines included), the mode and command-line
  backstops, a shell an agent drove speaking as an agent, an agent's environment, typing into an
  agent's TUI and naming a verifier refused, an agent's terminals under the fleet bound, an
  assign under the project's limits, keyed starts replayed and scoped to their caller, reports
  batched, paced and parked, and a restart's reconcile. They also cover an agent's scope: the
  terminals it may put to work, a project's allowance for its own agents only, the terminals
  it opened kept across a restart, and a task's agent splitting work only under its own task.
  The store tests replay a log past its snapshot, over a torn last line and a bad middle one.

**The board says each thing once, in one neutral scale.** ✅ 2026-09-30 (design review of
`project-tree`, `project-lanes` and `project-timeline` against Warp, Zed, T3 Code, Amp and
MonoCode)
- *Needs you* is a band that runs the tile's full width, one tone step up (`raised`) with no
  frame. Colour is kept for what it means: the heading and each mark are `warn`, and nothing
  else is. Its rows keep the tree's geometry, so their marks stand on the tree's column, and
  its heading sits on that column too. A row in the band says no state word, since the
  heading already says it (the inbox's rule). Before, it was a framed card inset from the
  tile, its marks one step right of the tree's, and each row said "Needs you" under the
  heading "Needs you".
- The lanes share the tile's width in equal columns, as many as fit at 232 pt each
  (`lanes_across`), and wrap onto a new row. Before, fixed-width lanes left an unused gap at
  the right and the next row started at an odd place. A lane that comes or goes moves no card
  sideways. Cards are a tone step with no border; hover and the picked card step up once more
  (`overlay`).
- A timeline entry for a task made under the title it still has says "Created", because the
  row already names the task. A task renamed since keeps "Created: *old title*".
- Tests: `the_board_says_each_thing_once_and_fills_its_tile` and
  `as_many_lanes_stand_across_as_fit_at_the_zoom` (`slopty-ui`). The e2e project stack now
  waits for the first run's own shell before the test opens anything, so the tiles stand in
  the same order on every run. Three runs rendered byte-identical goldens.

**A task placed where there is no clone gets one, made by that worker's own git.** ✅ 2026-10-01
- Before, a task with no `cwd` could start only on a worker that already had a clone, so an
  orchestrator could not send work to a fresh Linux box. Now, when no worker with a clone fits,
  the task goes where its rules would put it without the clone rule, and that worker clones
  the repository first. A pin to a worker with no clone does the same.
- The address comes from the orchestrator's clone: `RepoId::url`, the origin's URL as its
  config spells it, with an HTTP user and password and any other scheme's password left out
  (`repo::clone_url`). An SSH user such as `git@` stays, since it names the account the key
  logs in as. A clone of a local path has no address, and its refusal says so. The URL is not
  part of the identity: `RepoId::same` and the `repos` keys ignore it.
- The worker clones with its own git, so its person's credentials, SSH keys, credential
  helpers and `insteadOf` rules apply, and Slopty carries no token. Git never prompts
  (`GIT_TERMINAL_PROMPT=0`, no stdin). The clone goes to `~/slopty/clones/<host>/<path>`, built
  beside that folder and renamed into it once git finishes, so a failed or cut-short clone
  leaves nothing that looks like a clone. One that is already there and has the same origin
  is answered as it is. At most two clones run at once on a worker, one at a time per folder,
  and each has 30 minutes with its wait for a turn counted.
- Slopty made the clone, so it trusts it for Claude Code (`repo::cloning::trust`, kept only
  inside `~/slopty/clones`). Otherwise the agent would wait at the folder trust dialog and no
  hook would run. The worktrees under it share the clone's trust key.
- It is visible, as everything the server does for a task is: `Task::step` (`TaskStep`, also
  on the card) says what is being done, where, and how it is going. Git's own progress
  arrives as `ToServer::Cloning` and is shown in 5% steps. The step's start and its end
  (`Done` with the path, or `Failed` with git's `fatal:` line) go on the timeline as
  `Moment::Step`. The progress between them shows on the card only and is not kept. A clone
  that fails refuses the start with git's words.
- The server keeps the clones it had made per worker (`Steps::made_on`) and adds them to that
  worker's `repos` fact, so the next task finds the clone before any shell has opened in it.
  After a restart that list is empty, and a worker asked again answers from the clone it
  already has.
- A step under way when the server stops is marked failed as the store loads, so no card
  keeps saying it is cloning.
- Rejected: **cloning through the server** (a bare repository there, as the phase 2 design
  has it). It needs `git-remote-slopty` and a server-side copy of every repository. A worker
  can reach the forge the person already uses, with credentials the person already has.

**A finished task's branch comes home as a bundle, under a name only the server gives.**
✅ 2026-10-01
- When a task's agent reports `done` with a branch and it ran on a machine other than its
  orchestrator's, or in another clone, the server brings the branch to the orchestrator's
  clone, in the background:
  1. The worker it ran on bundles the branch's commits beyond its fork point from the target,
     taken from `origin/<target>` or else `<target>` (`Verb::BundleBranch`, `git bundle
     create`).
  2. The server reads the bundle in 4 MiB parts and uploads it into the other worker's
     `~/.cache/slopty/bundles` with the digest checked.
  3. That worker fetches it (`Verb::FetchBundle`).
  Neither worker needs a credential for the other, nothing is pushed to the forge, and the
  bundle is removed once it has been fetched. One left behind is swept after an hour.
- It lands as `slopty/<project>/<task>` (`Task::home_branch`), force-set. The name the agent
  reported is only the source. A report naming `main` therefore never moves the person's
  `main`. Landing under the reported name would also have let the next done report rewrite
  whatever branch it named.
- The receiving clone usually lacks the fork point, because a clone made later fetched a newer
  target. So when `git bundle verify` reports missing prerequisites, the receiver fetches its
  own `origin` once and verifies again. Only if the commits are still missing (no origin, or
  the forge unreachable) does the server send the whole branch. `--quiet` is not used on
  verify, because it hides the list of missing commits that tells the cases apart.
- The clone it goes to is the orchestrator's current checkout while that is still in the
  project's repository (`RepoId::same`), and otherwise any clone of it on that worker. The
  source is the task's worktree as its status line reports it, else the repository its
  terminal is in, else a clone on that worker.
- It shows as a `Home` step: bundling, sending with a percent, and then done (`<branch> as
  slopty/<project>/<task> at <short> in <clone>`) or failed with the reason. The step names the
  worker the branch goes to. A branch already in the orchestrator's clone, because the task
  ran there or in a worktree under it, has no step.
- One trip per task runs at a time. A second done report during a trip sends it again
  afterwards, for the newer commits.
- The orchestrator's role tells it where such branches arrive. Merging stays with the
  orchestrator and the merge queue (phase 3).

**`--worktree` works as the earlier ruling assumed, with one consequence for bases.** ✅
2026-10-01 (read from Claude Code 2.1.281's bundle)
- `--worktree <name>` makes `<repo>/.claude/worktrees/<name>` on branch `worktree-<name>`, with
  any `/` in the name as `+`. It runs `git worktree add --no-track -B worktree-<name> <path>
  <base>`. When the folder is already there, the worktree is reopened, not made again. The
  role's `worktree-slopty-<project>-<task>` names the branch it makes.
- The base is `origin/<default branch>`. Origin is fetched first when `FETCH_HEAD` is stale,
  and local `HEAD` is used when there is no origin. The person's `worktree.baseRef: "head"`
  uses local `HEAD` instead. So a task forks from the forge's default branch. It does not fork
  from the project's `target` when that is another branch, and it does not include the
  orchestrator's unpushed commits. That is why the bundle's fork point is taken from
  `origin/<target>` and the receiver fetches its origin.
- Known: `-B` resets the branch. A task started again after someone removed its worktree folder
  starts its branch over from the base. The commits are still in the reflog, and a branch
  already brought home is still kept as `slopty/<project>/<task>`.
- Tests: `a_task_on_a_worker_with_no_clone_gets_one_made_and_shown` and
  `a_finished_task_s_branch_is_brought_to_the_orchestrator_s_clone` (hub),
  `a_step_is_shown_as_it_goes_and_ends_with_the_server` (store),
  `a_clone_is_made_once_and_found_again`, `a_clone_without_an_origin_or_that_fails_leaves_nothing`
  and `a_branch_reaches_another_clone_through_a_bundle_of_its_own_commits` (real git), and
  `a_task_s_clone_is_made_where_it_runs_and_its_branch_comes_home` in
  `apps/slopty-cli/tests/projects.rs`. That one runs two real workers whose git reaches a
  local forge through each person's own `insteadOf`. It checks the clone made where the task
  runs, the trust kept for it, the agent started in it, and the branch arriving in the
  orchestrator's clone at the agent's commit, with origin fetched for the fork point.

Not built in Phase 1:
- merging a task's branch to the target (built in Phase 3, below);
- the known gaps under "An agent never has more than the person gave it".

## Phase 3, verify and merge, as built (2026-10-01)

**The queue tests the commit that becomes the target, and only moves it forward.** ✅
2026-10-01
- Research, read 2026-10-01:
  - bors and Graydon Hoare's "not rocket science rule": the target only ever holds a commit
    that passed, because the queue tests the merged result and not the branch alone.
  - GitHub's merge queue: each entry is tested as a temporary branch of the target, the entries
    ahead of it, and itself. An entry that fails is removed with its reason, and those behind
    it are rebuilt.
  - Mergify: batches and speculative checks, plus a direct merge when the branch is already up
    to date with the target.
  - Graphite and Aviator: stacks of dependent pull requests. Aviator adds "optimistic"
    parallel runs and bisection of a failed batch.
  - Claude Code's own worktrees: it makes them and leaves merging and pushing to the person or
    a pull request. It never merges or pushes a worktree branch itself.
- What they agree on, and what Slopty does:
  - Rebase onto the target, run the verifier on the rebased commit, and fast-forward the
    target to that commit (`Verb::Rebase`, `Verb::Verify`, `Verb::FastForward`, served by
    `slopty-worker::repo::verify`).
  - Fast-forward only, so the target never holds a merge commit the verifier did not see.
  - When the rebase leaves the commit that already passed, because the target had not moved,
    the verifier is not run again. This is Mergify's direct merge.
  - When the target moves between the rebase and the fast-forward, the queue rebases the same
    work again, up to 3 tries, and then holds.
- Moving the target is a compare and swap. `FastForward` names the commit the rebase went
  onto, and the worker refuses if the target is no longer there or the new commit does not
  descend from it.
  - When a worktree of the clone has the target checked out (the person's own checkout,
    usually), the worker runs `git merge --ff-only` in it. Its index and files then move with
    the branch, and git refuses rather than overwrite the person's uncommitted changes.
  - `git update-ref` is used only when no worktree has the target checked out. On a checked-out
    branch it would move the ref under that worktree and leave its index and files stale, so
    they would read as reverting the merge.
- The rebase runs in a detached checkout the project keeps in the orchestrator's clone
  (`~/slopty/verify/<project>`, a `git worktree add --detach`). It never runs in the person's
  checkout or the agent's worktree, so neither has a rebase stopped in it or files changed
  under it. A conflict lists the unmerged paths (`git diff --name-only --diff-filter=U`) and
  aborts the rebase. The checkout is reused between runs to keep builds warm.
- Rejected:
  - Speculative batches and bisection (GitHub's, Mergify's, Aviator's). They pay off when CI
    machines are plentiful and a run is long. Here a verifier runs on the orchestrator's
    machine, which is the person's, and the plan quota bounds the agents feeding the queue.
    Serial runs, with the retest skipped when nothing moved, keep each verdict about one
    task's work, and its reason goes to the one agent that can act on it.
  - `git replay` and `git merge-tree --write-tree`, which rebase without a checkout. The
    verifier needs the files checked out anyway. `replay` is still marked experimental (since
    git 2.44), and `merge-tree` (since 2.38) makes merges, not a linear history.
  - An integrator agent that merges. Cursor found that role became the bottleneck. The queue is
    the server's, and it gives the work back to the agent who wrote it.
  - Verifying in the agent's own worktree. The agent may still be writing there, and the
    verdict would judge a moving tree.
- Tests: `a_project_verifies_in_one_checkout_of_its_own_kept_warm`,
  `the_queue_rebases_in_the_project_s_checkout_and_names_conflicts` and
  `the_target_moves_only_forward_and_never_under_the_person_s_changes` (real git,
  `slopty-worker::repo::verify::tests`).

**A verifier is a terminal the person can watch.** ✅ 2026-10-01
- A task's branch reaches the orchestrator's clone (the `Home` trip, or the task worked there
  all along), and its state moves to verifying. The project's lane then runs the verifier, the
  task's own or else the project's, on the orchestrator's worker in the project's checkout.
  - It runs as a terminal titled `Verifier for <project> #<task>`. The command is
    `nice -n 10` around the person's login shell (`$SHELL -l -i -c <verifier>`), so their PATH
    and toolchains apply.
  - Its environment carries `SLOPTY_VERIFY_HEAD` and `SLOPTY_VERIFY_BASE`.
  - The task's `Verify` step names that terminal (`TaskStep.term`), and every client lists it,
    so the person can open it and watch.
- While the verifier runs, the step shows its last line, read every 2 s. The lane learns the
  exit from the session's own exit state.
- The result is a `VerifierRun`:
  - the commits it judged (`head` and `base`);
  - the exit code and how long it took;
  - the last lines of its output, kept to the summary's bound.
  It goes on the task and on the timeline (`Moment::Verified`).
- A pass closes the terminal. A failure keeps it, so the whole output can be read. It is closed
  when the task is judged again.
- A terminal the person closes before the verifier ends counts as a failure with no exit code.
  There is no timeout, because the person can see the run and stop it.
- A pass puts the task in the queue (`Task.merge = Merge::Queued { since_ms }`). The queue is
  its tasks, done and queued, in the order they joined. Verifying comes before merging, since
  an agent waits on each verdict and every pass feeds the queue.

**A failure goes back to the agent that can fix it, as a report.** ✅ 2026-10-01
- A failed verifier, a conflict, or anything else wrong with the work gives the task back:
  - it leaves the queue;
  - its step fails with the reason;
  - it waits at its agent's prompt when that agent still runs, and is planned otherwise.
- The agent is told through the delivery path every report takes (`deliver::Deliveries::notice`).
  That path is its `SessionStart`, `UserPromptSubmit` and `Stop` hooks, never typing into its TUI.
  - The report names what ran, on which commits, how it ended, the last lines, and what to do
    next: fix, commit, and report done again.
  - A conflict names its paths.
  - A newer notice about the same task replaces an older one that has not been delivered yet.
- The node above the task hears too, at once:
  - "given back" with the reason;
  - "merged into <target> at <short>", since what depends on the task can start then.
- A job that stops for a reason that is not the task's holds the lane with that reason on the
  step, and the task keeps its place. Examples: the worker is away, or the person's checkout of
  the target has changes in the way. A worker registering again starts the lanes once more, and
  so does the next change that concerns the project.
- Only the person asks for a merge outside a done report (`TaskMerge`, `slopty task merge`).
  An agent's call is `Forbidden`. A task with a verifier is verified first, and one with none
  joins the queue at once. `Verify`, `Rebase` and `FastForward` are the server's own verbs and
  `Forbidden` to every caller.

**The queue survives a restart.** ✅ 2026-10-01
- The lane keeps nothing the store does not. Each time, it reads its next job from the tasks
  (`Projects::next_job`): the longest verifying task, else the head of the queue.
- A step under way when the server stopped is marked failed on load ("the server stopped while
  it ran"). The task keeps its state and its place, and registration starts the lane again.
- Every move the lane makes is one store change with at most one timeline entry
  (`Projects::advance`), checked against `TaskState::may_become`.

**Pushing the target is the person's setting, off by default.** ✅ 2026-10-01
- `Project.push` (`slopty project create --push`, `slopty project update --push true`). When it
  is on, a merge pushes the target to `origin` without force. A push that fails leaves the
  merge done and says why on its step ("not pushed: …").
- It is off by default, and only the person can turn it on (an agent naming it is
  `Forbidden`), because:
  - a push publishes to a shared forge, and it may start CI and deploys;
  - the verifier's pass is a local verdict;
  - the push would use the person's credentials from a background process;
  - Claude Code itself never pushes a worktree's branch.

**No separate merge-queue view; the board is the queue.** ✅ 2026-10-01
- The board's lanes are the queue already: Verifying, Ready to merge, Merged. A second view
  would show the same tasks again. So the lanes take the queue's order:
  - Ready to merge runs as the queue does, the task waiting longest first, and so the one
    merging. Each card says where it stands ("Next to merge", "2nd to merge"). A done task the
    queue does not hold, because it has no verifier and the person has not asked, comes last.
  - Verifying puts the run under way first, then the rest in the order the server takes them.
- A task's verifier shows under it (`Board::verdict`, `ProjectView::check_block`):
  - its mark and word (Verifying, Passed, Failed);
  - the commits it judged ("4a7aa6d over c08d4c1"), its exit and how long it took;
  - for a failure, its last four lines in the mono face a terminal uses, trimmed to their first
    word. They are a glance, and the terminal keeps the whole log.
- The verdict shows only while it still describes the task as it is now: a pass while the task
  waits to merge, and a failure until it is verified again or merged. A stale one would read
  as current.
- Where it shows:
  - on the board, every card with a verdict shows the block;
  - in the tree, a run under way or a failure stands under its row, and a pass is a word in
    the row ("Passed at e5b0d17"), so a tree of finished work stays one line a task.
- "Output" on the block opens the verifier's terminal (`ProjectEvent::Output`): the run under
  way, or the failed run kept for its output. It does not open the agent the row opens, and a
  terminal that has closed says so.
- Tests: `the_queue_runs_in_its_order_and_a_verdict_speaks_while_it_holds` (model),
  `a_verifier_shows_on_its_task_and_opens_its_terminal` (workspace, drawn and clicked), and the
  `project-tree`, `project-lanes` and `project-timeline` goldens. Those now hold a failed
  verifier on an up-next task and a passed one waiting to merge.

**A task given back on another machine is sent the target to rebase onto.** ✅ 2026-10-01
- With pushing off, the forge never sees what the queue merged. An agent in a clone on another
  machine could not rebase onto the target the queue judged it against, because its clone has
  only the forge's target. So when the queue gives a task back after a conflict, or after a
  verifier that failed on the rebased work, the server first sends the target the other way.
  This is the `Home` trip reversed:
  - a bundle of the target's commits beyond the forge's `origin/<target>` in the
    orchestrator's clone;
  - uploaded to the agent's worker;
  - fetched into the agent's clone as `slopty/<project>/target` (`Task::target_branch`).
  That name is one only the server sets, and no task's number can take it.
- The report then names where the target is. Each case:
  - sent: "main as the queue has it is in your clone as slopty/demo/target at <short>:
    rebase onto slopty/demo/target";
  - the forge has it all (`ErrorCode::NothingNew`, a bundle with no commit beyond the forge's
    target): fetch origin and rebase onto `origin/<target>`;
  - the task works in the orchestrator's clone: the target as it is;
  - the target could not be sent: fetch origin, with the reason, and a warning that the
    forge may lack what the queue merged.
- While the target is sent, the merge step shows "Sending main to the task's clone" with a
  percent.
- The person's `slopty task merge` on such a task brings its branch home first. Only then is
  the merge asked, so the queue judges what the agent has now and not the commit an earlier
  trip carried.
- Tests: `the_target_reaches_a_task_s_clone_the_same_way_with_what_the_forge_lacks` (real git),
  and `a_conflict_on_another_machine_brings_it_the_target_to_rebase_onto` in
  `apps/slopty-cli/tests/projects.rs`. That one runs two workers with push off:
  1. The person commits on the orchestrator's `main`, and the task's work conflicts with it.
  2. The task is given back, and its clone has `slopty/demo/target` at the person's commit.
  3. The report waiting for the agent's hooks names that branch.
  4. The agent rebases onto it and resolves the conflict, and the person runs `slopty task
     merge`.
  5. The branch comes home again and is verified, and `main` fast-forwards to the resolved
     work with nothing pushed.

**A reviewer reads the work with fresh eyes before it merges.** ✅ 2026-10-01
- Research, read 2026-10-01:
  - Claude Code's `/review` (now an alias of `/code-review`) and `/security-review`. The
    security review reads the diff against the merge base with read-only tools and drops
    findings under 8 of 10 confidence. Anthropic's code-review plugin runs parallel finder
    agents, then a validating subagent per finding, and keeps those scored 80 or more.
  - Anthropic's managed Code Review never blocks: it is always a neutral check, with findings
    ranked Important, Nit and Pre-existing. Anthropic reports that pull requests with
    substantive comments rose from 16% to 54%, and that under 1% of findings were marked
    incorrect. claude-code-action cannot approve a pull request.
  - A Claude Code subagent that is not a fork starts with a fresh context. The docs' reviewer
    example has only `Read, Glob, Grep`.
  - CodeRabbit blocks only when `request_changes_workflow` is turned on, which it is not by
    default. The person can overrule it (`@coderabbitai resolve`, `approve`). Graphite's
    reviewer comments on a stack and never merges.
  - Cognition reports that Devin Review catches about 2 bugs per pull request, about 58% of
    them severe, and finds that a reviewer works best with none of the author's context.
  - Claude Code features that fit: `--session-id`, `--permission-mode`,
    `--append-system-prompt` and `--disallowedTools` work in an interactive session.
    `--json-schema` works only in print mode (`-p`).
- What they agree on, and what Slopty does:
  - The reviewer reads the diff against the base, with read-only tools, and has none of the
    author's context.
  - Each finding is structured (path, line, severity, blocking) and states what to do.
  - Only the top severity blocks, and the person can overrule it, with the overruling
    recorded.
- The person turns it on per project with a brief: on the board (the header's checks toggle, or
  "Verifier and review…" in the palette, opens a panel that sets the verifier and the review
  together), or `slopty project create --review <brief>` and `project update --review` (an
  empty brief turns it off). Only the person sets it, as with the verifier.
- Once the verifier passes (at once with no verifier), the task stays Verifying and the lane's
  next job is `Job::Review`:
  1. `Verb::ReviewCheckout` makes a detached checkout of the verified commit of its own
     (`~/slopty/verify/<project>-review-<task>`). Beside it is `.slopty-review.diff`, the
     output of `git diff base..head`.
  2. The server starts `claude` in it on the orchestrator's worker with `Verb::SpawnAgent`,
     with `--disallowedTools Edit,Write,NotebookEdit`, its own `--session-id`, a role through
     `--append-system-prompt` (the person's brief, the diff's path, how to answer), and
     `SLOPTY_PROJECT` and `SLOPTY_TASK`.
  3. The task's step is Review, running, naming the reviewer's terminal.
- The reviewer is a real Claude Code session in a terminal, and the person can open it from
  the board. The lane does not wait on it: the next task is verified alongside it.
- It answers with the `review_report` tool (`Verb::TaskReview`). Only that session, proven by
  its link, or the person may give the verdict; the task's own agent cannot. The run keeps at
  most 8 findings, the blocking ones first, and counts the rest (`ReviewRun::more`).
  - **Approved:** the task is Done and queued, and the reviewer's session closes.
  - **Changes asked:** the task is given back as a failed verifier is. Its agent's next prompt
    hook carries the findings, what blocks first ("What blocks the merge:", "Also noted:"). The
    reviewer's session is kept to be read.
  - **The person** (`slopty task review --approve | --changes --finding <path:line: words>`)
    may answer at any time, over the reviewer or after it. An approval needs a verifier that
    passed. It closes whichever reviewer session is open or kept.
- A reviewer that ends with no verdict leaves the step failed, "The reviewer ended without a
  verdict". The task waits for the person and is not read again on its own. `task merge`
  starts the checks afresh. So does a server restart that finds a review under way.
- A new head clears the old word. Each report of done, `task merge` and the person's ask for a
  merge drop the last verifier run, the last review and the step, and close a reviewer still
  reading.
- On the board, the review shows the way the verifier's run does (`Board::review`,
  `ProjectView::check_block`):
  - "Reviewing" while it reads;
  - "Approved" or "Changes asked", the commits read, "by you" when it was the person, the
    count of blocking findings, and how long it read;
  - for each finding, its severity, its file and line in the mono face, and its first line.
  - "Reviewer" opens the session (`ProjectEvent::Reviewer`).
  - The project's line says "reviewed before it merges".
  - An approval speaks while the task waits to merge. Changes asked speak until the work is
    checked again or merged.
- Rejected:
  - `claude -p --json-schema` as a hidden process. It would give a typed verdict for free,
    but the person could not watch the review or ask the reviewer anything. A session hidden
    behind the TUI breaks the rule that the TUI stays the source of truth.
  - Plan mode for the reviewer. `--disallowedTools` keeps the tools it may read with (Bash
    for `git log`, the tests) and removes those that write. Plan mode would also stop it at
    a plan prompt.
  - Blocking by default, and no reviewer by default. A project opts in with a brief. Once on,
    a blocker blocks, since that is what the person asked for, and the person can always
    overrule it.
  - Parallel finders with validators, as the code-review plugin has. They cost several
    sessions of the plan per task. One reviewer with a brief, told to report only what it
    would defend, is the measured start. Finders can come later, once misses show they are
    needed.
  - A reviewer subagent inside the task's own session. It would share the author's context,
    which is what review exists to avoid, and its verdict could not be proven to be anyone's
    but the author's.
- Measured, with the stub claude on one worker: 0.47 to 0.88 s from the agent's spawn to the
  reviewer's word (`docs/MEASUREMENTS.md`). That is the server's path and not the review itself.
- Tests:
  - `a_reviewer_reads_each_task_after_its_verifier_and_holds_only_its_own` (store): the job
    order, a reviewer holding only its own task, a restart, and the bound on findings.
  - `a_reviewer_reads_the_verified_work_and_its_verdict_decides_the_merge` and
    `a_reviewer_that_ends_without_a_verdict_leaves_it_to_the_person` (hub, scripted worker).
  - `a_reviewer_reads_the_task_s_own_diff_in_a_checkout_of_its_own` (worker, real git).
  - `a_reviewer_s_block_goes_back_to_the_agent_and_the_person_s_word_merges_it` (CLI): real
    workers, git and the stub claude as the reviewer.
  - `a_review_speaks_while_it_holds_and_says_what_it_found` (model) and
    `a_reviewer_shows_on_its_task_and_opens_its_session` (workspace).
  - The `project-*` goldens, which hold a task with changes asked.

Known gaps:
- The review's cost per task is not on the board yet. It comes with the tokens per agent.

Tests:
- `slopty-server::project::merge::tests`: the queue's order and its reload from the store's file
  and from its log replayed, and what the person may merge.
- `slopty-server::hub::queue::tests`, against a scripted worker:
  - verified and merged by fast-forward;
  - a failed verifier reaching its agent through hooks;
  - a rebased head verified again, and a hold for the person's changes;
  - a conflict with its paths.
- `apps/slopty-cli/tests/projects.rs`, with real workers, git and the stub claude:
  - `a_finished_task_is_verified_and_merged_into_the_orchestrator_s_clone` runs two workers:
    done, home, verified, queued, and `main` fast-forwarded in the orchestrator's clone, with
    nothing pushed;
  - `a_conflict_on_another_machine_brings_it_the_target_to_rebase_onto`, above;
  - `a_failed_verifier_and_a_conflict_go_back_to_the_agent` runs one worker: the failure's
    report read by the agent's next prompt hook, the kept terminal, then `slopty task merge`, a
    pass, and a rebase conflict naming `a.txt` with `main` untouched.

## Agents the person starts, and agents after a reboot (2026-10-01)

**An agent opened on `claude` is started the way Slopty starts its own.** ✅ 2026-10-01
- Before: ⌘⇧T, and any tile opened on `claude`, ran a bare `claude`. It had no hook relay, so
  its permission prompts never reached the face or the inbox, and its status was guessed from
  the title and the transcript. It had no Slopty tools, so an orchestrator the person started
  saw `project_*` and `task_*` only after a `claude mcp add` on each machine.
- Now the worker wires every open whose program is `claude` and which Slopty has not wired
  already (`Worker::as_agent`, beside the spawn path):
  - the relay on its `--settings`;
  - `--mcp-config` naming `slopty mcp`, when the worker has a server;
  - the mod;
  - `--session-id`, so it can come back after a reboot.
- It is the person's own agent, so the mode that asks no permission is not locked. The open's
  own flags and variables are kept.
- Left as asked:
  - a command that already carries the relay (the server's spawn, the person's own settings);
  - a `--print` run;
  - a worker with no `slopty` beside it to hand out.
- A `claude` typed at a shell prompt still goes through the shell integration's function.
- Test: `claude_opened_in_a_tile_is_started_as_slopty_starts_its_agents`
  (`crates/slopty-worker/tests/agent_open.rs`, a real ptyd and the stub agent).

**A resumed project agent keeps its tools, its role and its lock.** ✅ 2026-10-01
- Before: a reboot resumed a project's agent with its kept flags and the relay, but without its
  tools, its role or the lock on the mode that asks nothing. So the agent no longer knew its
  task, could not report, and could switch the lock off.
- What Slopty put on the command line is now noted in the kept conversation
  (`slopty_agent::resume::Resume`), never as the documents themselves, and given afresh:
  - `mcp`: an `--mcp-config` serving `slopty mcp`;
  - `locked`: `--settings` holding `permissions.disableBypassPermissionsMode`.
- One system prompt is kept: the one appended to an agent started with Slopty's tools, which is
  the server's role. A system prompt on any other agent is still never written down, since it
  may carry anything.
- A shell the person ran the agent in gets everything back but the role. The role spans lines,
  which a line typed at a prompt cannot carry.
- `--worktree` is not kept, because the resume runs in the directory the agent was in, which is
  the worktree. Kept, it would make another worktree.
- Tests: `slopty_s_own_wiring_is_noted_and_its_role_kept` (`slopty-agent`) and
  `a_project_s_agent_comes_back_with_its_tools_role_and_lock` (`slopty-worker::restore`).

## The board acts, and a project can be let go (2026-10-01)

**What moves a task on is a button on its row and its card.** ✅ 2026-10-01
- Before: the board only showed. Merging a task no check queued, trying a failed step again,
  or overruling a reviewer took `slopty task merge` or `slopty task review` in a shell.
- Now `Board::actions` names what the person can do to each task, and the row and the card
  draw it as small buttons. The palette and the keys `m`, `r` and `a` do the same to the task
  the board stands on:
  - **Merge** a finished task the merge queue does not hold;
  - **Retry** a failed home, verify, review or merge step. Both send `TaskMerge`, which runs
    the verifier again on the branch;
  - **Approve** over a reviewer that asked for changes or ended with nothing said, once the
    verifier passed. This sends `TaskReview`, approving, as the person.
- A task that only reads, or is merged, offers nothing. A failed clone is the worker's to make
  again, so it offers no Retry.
- A button holds back the row's own click, so pressing one never opens the agent. A refusal is
  shown as a notice in the server's own words. With no server linked, the board says so.
- The header gains the push toggle (`ProjectSet { push }`) and a button that turns the tile
  back to the orchestrator's terminal. Before, the orchestrator's own row was the only way
  back, and nothing showed it.
- Tests: `a_task_offers_what_moves_it_on` (`slopty-ui::project`) and
  `the_boards_actions_reach_the_server` (`slopty-ui::workspace`, over `ServerCaller::queued`).

**A project is the person's to let go: `ProjectDelete`.** ✅ 2026-10-01
- The verb removes the project, its tasks and its timeline from the store, with a `Forget`
  record in the log. Its reviewers' sessions close. The agents' terminals stay, since they
  may hold work the person still wants.
- An agent asking for it is refused with `Forbidden`. The verb is server-only: no worker
  carries it, and no MCP tool offers it.
- `ProjectUpdate` has no removal. So the server sends every client a fresh projects
  snapshot, and that snapshot's first part replaces the client's mirror. Adding a removal to
  the update would have been one more wire shape for a rare event.
- The board asks twice: the first "Delete the project" says what a second does, and only a
  second within 5 s sends the verb. The CLI's `slopty project delete <name>` sends it at once,
  since a typed name is deliberate.
- Tests: `the_person_lets_a_project_go_and_every_client_hears_it` (`slopty-server`), the
  `project_delete` golden, and the CLI reviewer e2e ending in `slopty project delete`.

**"Start a project here".** ✅ 2026-10-01
- The palette's line makes the focused terminal the orchestrator of a new project, and its
  board shows as soon as the server's word of it arrives.
- The project is named for the repository's directory, and kept clear of the names taken
  (`-2`, `-3`). It lands on the branch checked out, or `main`.
- A terminal that already orchestrates a project shows that project's board instead.
- Test: `a_project_starts_in_the_focused_terminal`.

**The board follows the design rules.** ✅ 2026-10-01
- Colour is spent only on what needs the person:
  - warn for *Needs you*, and error for *Failed*;
  - every other mark, state word and lane in the muted ink, with finished lanes a step
    brighter.
- "Passed", "Approved" and "Changes asked" are words with a glyph, not colours. Red is kept for
  a run that failed. A blocking finding is the default ink with a ✕.
- A row's or card's second line holds three facts at most, so two separators. They are what
  moves the task on, what its agent says, then where it runs. "Not placed" is gone, since it
  repeated on almost every row.
- The header's line is a sentence ("slopty → main, verified by cargo gate"). The live count
  and the progress are readouts at the right.
- The bar's hover says what each segment counts.
- A finding wraps to two lines, so it is no longer cut to a few characters in a narrow lane.

**Short lanes stack; the board never wraps a row of lanes under another.** ✅ 2026-10-03
(design review `.research/design-review-2026-10-03.md` #1, #4)
- Before: the lanes stood in equal columns, as many as fit at 232 pt, and wrapped onto a new
  row. Six lanes in a 1030 pt tile made a row of four and a row of two. The second row started
  under the tallest lane, so the short lanes stood over an empty gap, and Ready to merge (the
  lane the person acts on) sat at the fold with its Merge button cut.
- How others do it: Linear, GitHub Projects, Height, Plane and Trello give each column a fixed
  width and scroll the board sideways, and Linear and Plane also hide or fold empty and done
  columns. Sideways scrolling is not open to a tile here. A sideways swipe moves the strip
  (`workspace/strip.rs`), so lanes past the tile's edge could only be reached by a scroll bar.
  Folding a lane would hide cards the person may need.
- Now: the lanes keep their columns of at least 232 pt, one per lane while they fit. With more
  lanes than columns, neighbouring lanes stack in one column with a gap between them, in their
  order, down each column and then across (`project::view::stack_lanes`). The split makes the
  tallest column as short as it can be, and among splits as short, the most even. So a short
  lane joins a short neighbour rather than a tall one: in the showcase, Needs you over Failed,
  then Working, then Up next, then Ready to merge over Merged. Every lane stands in the first
  screen.
- A lane's height is counted, not measured: a card's lines from the parts it draws (title,
  second line, place, pipeline, check, its tail or findings, a subtask's word, its buttons),
  and the heading as two (`ProjectView::card_lines`). Only the balance rests on the count, and
  a card drawn a line taller or shorter than counted moves nothing else.
- A verdict names its verifier ("Verifier failed", "Verifier passed"), as the timeline does. A
  task the server sends back after a failed check, with no agent left to fix it, is Planned
  (`hub/queue.rs`), so it stands in Up next. There a bare "Failed" read as the Failed lane,
  which is a task given up.
- Tests: `short_lanes_stack_so_every_lane_stands_in_the_first_screenful` (`slopty-ui`), with
  `the_board_says_each_thing_once_and_fills_its_tile` unchanged. The `project-lanes` golden
  was retaken.

## Where tasks run, and starts that wait for the person (2026-10-02)

**The board has a machines lens, and every placement keeps its reasons.** ✅ 2026-10-02
- Before: the server ranked the workers at each spawn (`placement::rank`), then dropped the
  ranking. The board showed which worker a task was on, but never why it went there, nor which
  machines had room.
- Now `assign` records `Assignment.placed` (`Placed { pinned, score, why }`), taken from the
  chosen worker's `Suggestion`. `Suggestion::why` puts the ranking into words: "pinned" first,
  then the scored rules by weight (`os == "macos" +10`), then the required rules that held. For
  a worker that does not fit, it gives the failed rules' details. It keeps at most three parts
  and `STATUS_MAX` bytes. Rules every worker passes (`online`, the live limits) are left out,
  since naming them says nothing.
- The machines lens (key `4`) asks for `WorkerFacts` every 5 s while it is shown, and stops
  when it is not. It groups the tasks by host: first the workers holding tasks, then the online
  ones, then the ones away, each by name. A host's heading gives its kind ("macOS, 12 cores,
  64 GB"), its load and its live agents. Each task's row says why it went there. The tasks that
  have not started sit under "Not started".
- **Run on…** (key `o`) opens a picker on a task that has not started. It lists "Anywhere" and
  the workers in the server's own ranking order (`PlacementSuggest`), each with its `why`. A
  pick sends `TaskUpdate { run_on }` (`RunOn::Worker` or `RunOn::Anywhere`). This changes only
  `placement.pin`, so the task's other rules still hold. `TaskCard.pin` shows the pin to the
  board. The CLI's `slopty task update --run-on <worker|anywhere>` does the same.
- Tests: `a_ranking_says_what_decides_it` (`slopty-proto`),
  `a_task_runs_where_the_person_says_and_keeps_why_it_went_there` (`slopty-server`), and
  `the_machines_lens_shows_where_everything_runs_and_where_a_task_will` (`slopty-ui::workspace`).

**A project can ask before each task starts, and the person starts them.** ✅ 2026-10-02
- `Project.ask_to_start` is a per-project autonomy setting, and only the person sets it. An
  agent's `ProjectSet` that names it is refused with `Forbidden`, so an orchestrator cannot
  grant itself autonomy.
- Once it is set, an agent's `task_spawn` only proposes. The server checks the launch exactly
  as it would for a start (the loosening and the placement rules). Then it stores
  `Task.proposal`: the launch, plus `Proposed { since_ms, runs, on, why }`, giving the worker
  the ranking would choose now and why, or "no worker fits now: …". It logs
  `Moment::Proposed { on }`. No worker is asked and nothing is reserved, so a proposal costs
  nothing until it starts.
- `Verb::TaskStart { project, task, pin }` runs the stored launch through the same
  `start_task_once` as a direct spawn, so idempotency and reservation are shared. The person's
  pin wins over the agent's. `assign` clears the proposal. The verb is the person's alone: an
  agent is refused, no worker carries it, and no MCP tool offers it. The CLI has
  `slopty task start` (`--on <worker>` to pin), and `project create --ask-to-start` /
  `project update --ask-to-start <bool>`.
- The board draws the proposals as a plan band above the lens. Each row reads "Would start on
  studio: os == "macos"" and has **Start** (key `s`) and **Run on…**; a pick from the picker
  starts the task there. **Start all** sends one `TaskStart` per proposal. The orchestrator's
  row says "Waits on you to start N tasks". The header's hand toggle sets `ask_to_start`.
- The estimate on the band comes from this project's own timeline. It is the median time from
  a spawned assignment to Verifying or Done, over finished tasks of the same kind if there are
  any, otherwise over all of them. It says what it is drawn from ("about 20 min each, from 2
  finished of the kind"). The server keeps no token count, so the estimate is in time, not in
  money. Before any task has finished, the band says there is nothing to estimate from yet.
- "Start a project here" creates the project with `ask_to_start` on. Direct spawns stay the
  default for the CLI and the MCP `project_create`, where a script expects work to start.
- Tests: `proposed_start` (goldens `task_proposed`, `task_proposed_card` and `task_start`),
  `an_agent_s_start_waits_for_the_person_when_they_ask_to_start_tasks` (`slopty-server`),
  `a_plan_is_estimated_from_the_tasks_that_finished` (`slopty-ui::project`), and
  `a_plan_waits_for_the_person_who_starts_one_or_all` (`slopty-ui::workspace`).

## What changed while you were away, and what it cost (2026-10-02)

**A board opens onto what changed since this client last looked.** ✅ 2026-10-02
- Before: coming back to a project meant reading the timeline from the top and guessing where
  the last look ended.
- The cursor is this client's: the `seq` of the last timeline entry its board showed, and
  when it hid (`project::recap::Looked`). A board on show reads everything as it arrives, so
  the cursor moves only as the board hides, and at quit for a board still on show.
  `layout.json` keeps it per project beside the faces and pop-outs, so the recap spans
  launches. It is per device, as the plan asks: a look on the iPad does not empty the Mac's
  recap.
- Opening the board compares the cursor with the timeline. `Recap::of` keeps the kinds that
  move the person: changes asked, a verifier or a step that failed, a stuck report, an agent
  that ended before its work was done, a proposed start, and then merged, verified, started
  and created. Each kind is one line naming its tasks in the order they last moved
  ("Verifier failed on #5 and #6", "Merged #4 Write the decision"), and a line counts past
  three. What needs the person comes first, marked in the warning tone. Project-wide entries
  and notes are left to the timeline.
- The snapshot carries only the latest 64 entries, so a long absence falls before what the
  board holds. The recap then reads the gap back with `ProjectStatus { since }`, a page at a
  time and at most 8 pages. If the server no longer keeps the entries back to the cursor, the
  recap says so on its last line rather than passing for complete.
- The first look on a device has no recap: there is nothing to compare with, and the tree
  already shows everything. A cursor past the end belongs to an earlier project of the same
  name, so it is dropped.
- The band sits over "Needs you" until the person closes it or the board hides. Nothing on
  the server changes: it is the client's own reading.
- Tests: `a_recap_tells_what_needs_you_first_and_names_its_tasks` (`slopty-ui::project`) and
  `a_board_opens_onto_what_changed_since_you_last_looked` (`slopty-ui::workspace`, over
  `ServerCaller::queued`).

**Time at work per node and subtree, the orchestrator's share apart.** ✅ 2026-10-02
- Before: the board had no notion of what a task cost. Wall time from the timeline counted
  every wait at the prompt or on the person as work.
- The server already follows every agent's status for its task. It now also keeps `Spent {
  active_ms, since_ms }` on each task, and `Project.orchestrator_spent` for the orchestrator.
  A stretch begins when the agent starts working and ends when it stops (`Spent::follow`).
  Working, running a tool, and waiting on background work it started count as work
  (`Spent::works`). Idle at the prompt, done, blocked on the person, or holding only scheduled
  prompts does not.
- The stretch under way travels as `since_ms` and is counted by the reader (`Spent::at`), so a
  running clock costs no message per second. The board ticks once a minute while any of its
  agents works.
- An ended stretch is written to the store and a begun one is only pushed, so the time
  survives a restart for at most one extra write per turn. A restart drops a stretch that was
  under way, since the server cannot know the gap was work. A closed terminal ends its
  stretch.
- `TaskCard.spent` carries it to the board. A row shows its time beside its state, from a
  minute, its subtree's when it split work off ("worked 42m, 12m itself"). The header shows
  the project's total, with the tasks' and the orchestrator's shares on hover.
- The plan band's estimate now uses the finished tasks' time at work instead of the
  timeline's wall time, so it no longer depends on what the timeline still holds: "about 20
  min of work each".
- **Cost, context and quota are the agents' threads' word.** `slopty_proto::thread::Meters`
  carries a session's cost, its context and the plan's rate windows from the agent's own
  status line, so no credential is read. The workspace takes them per session
  (`WorkspaceView::thread_meters`), and the board rolls them up the same way:
  - cost per node and subtree, and the orchestrator's apart;
  - the context as a figure beside the time, hidden under 20 % and warning from 80 %;
  - each rate window in the header at the fullest any agent reports, warning from 80 %.
- A node whose thread this client has not heard shows its time alone. Tokens beyond the
  context are not summed, since the meters carry the context's size, not a running count.
- Tests: `spent_counts_the_stretches_at_work` (`slopty-proto`),
  `time_at_work_is_counted_per_task_and_apart_for_the_orchestrator` (`slopty-server`),
  `time_and_cost_roll_up_the_tree_with_the_orchestrator_apart` (`slopty-ui::project`) and
  `the_board_says_what_its_agents_spent` (`slopty-ui::workspace`). The goldens
  `project_snapshot`, `project_reply_status`, `task_merged_card` and others carry the new
  fields.

**The board's next steps are the person's words to the task's agent.** ✅ 2026-10-02
- Before: when a task's verifier failed, its review asked for changes or its rebase
  conflicted, the server told its agent once. If the agent stopped short, the person had to
  open its TUI and type. Retry checked the same work again, which fails the same way.
- `Verb::TaskTell { project, task, text }` carries the person's own words to a task's agent,
  through the same delivery the reports take: the worker hands them over through the agent's
  hooks, and nothing is typed into its terminal. `Deliveries` now tells who wrote an item:
  - an agent's report, paced as before;
  - the server's notice;
  - the person's words, which go at once, unpaced. A second message replaces the person's
    first while it is unread, and it never replaces the server's notice beside it.
  The agent reads them under "The person says:", with the reports' tag spelled apart. The
  timeline keeps `Moment::Told`.
- `TaskTell` is the person's alone, since an agent reports upward with `task_report`.
  It needs an agent running on the task, so a word is never parked for an agent that may
  never come. The CLI has `slopty task tell <words>`. No MCP tool offers it.
- While a task's agent runs, `Board::actions` puts its next step first on its row and card,
  and the palette offers each one for the task the board stands on:
  - **Fix CI** while its verifier's failure still speaks;
  - **Address comments** while its review, or its pull request's review, asks for changes;
  - **Resolve conflicts** after a rebase onto the target conflicted.
  `Board::told` writes the words from what the board knows: the verifier's command, commit
  and first line; the review's summary and up to five findings with their places; the pull
  request; and git's word on the conflict. Each ends with what to do next. Retry for a failed
  verifier or rebase waits until no agent runs, since checking unchanged work fails the same
  way.
- A conflict is now its own step, `StepKind::Rebase`, failed. Before, it was a failed merge
  that told the board nothing about which next step fits. The recap tells it as "Conflicts
  on #n".
- Fix CI reads the project's verifier as its CI, and a pull request's own checks too (below).
- Tests: `told_and_conflicted` (goldens `task_tell`, `moment_told`, `step_rebase_failed`),
  `the_person_s_words_go_at_once_beside_the_server_s` (`slopty-server::deliver`),
  `the_person_s_words_reach_the_task_s_agent` (`slopty-server`, through a worker's link),
  `a_running_agent_is_told_its_next_step_in_the_person_s_words` (`slopty-ui::project`) and
  `a_next_step_is_said_to_the_task_s_agent` (`slopty-ui::workspace`).

## A pull request's own checks, and the pipeline row (2026-10-02)

- **Who reads them.** The server watches every task that has a pull request and is not merged
  or failed (`Hub::watch_checks`). It asks the worker its agent ran on, in that task's
  worktree, with `Verb::PullChecks`. The worker runs the person's own forge command:
  `gh pr checks N --json name,bucket`, or `glab mr view N --output json` for a GitLab merge
  request, whose `head_pipeline.status` counts as one check (`slopty-worker::repo::checks`).
  - It runs as the worker's user, with prompts and update notices off. Nothing of the sign-in
    is read or passed. A command that is not signed in says so, and the card keeps its last
    word.
  - A worker without the command answers Unsupported. The command is looked for on `PATH`,
    then where Homebrew puts it, since a daemon started by launchd has little on its `PATH`.
  - `gh` ends 8 while checks run and 1 once one fails, with the same JSON either way, so the
    JSON is read whatever the exit code. "no checks reported" means no checks.
- **How often.** The watcher wakes every 10 s and reads only what is due. Checks still
  running are read again after 30 s, settled ones after 2 min (a push starts them again), and
  after 5 min when the forge could not be asked. The server does the polling, not each
  client, so a hundred boards cost one read.
- **What is kept.** `Checks` holds the state (none, pending, passing, failing), the counts and
  up to five failing names, each at most 128 bytes, on `Task` and `TaskCard`. A read that says
  the same as the last one changes nothing. The timeline logs `Moment::Checks` only when the
  state changes, so a poll never floods it. The recap tells a failure as "Checks failed on
  #n".
- **Fix CI** is offered when the checks fail and an agent is live. Its words name the failing
  checks and the command that shows them.
- **The pipeline row.** Once a task's work is on its way (verifying, done, judged, queued or
  with a pull request), its board card draws one row of quiet chips:
  - its branch;
  - what the verifier and the reviewer said;
  - its place in the queue;
  - "PR #n", its checks in words ("1 of 6 checks fail", "2 of 6 checks running", "checks
    pass") and "Changes requested", each a chip of its own so a narrow lane wraps them rather
    than cutting one long chip;
  - the to-dos still open.

  The chips use neutral text. A stage that holds the merge back is drawn in the stronger ink,
  never in red, because a red mark belongs to a run that failed and none of these is an alarm
  until someone has to act. The stage that the card's own check block already says, with its
  detail, is left off the row, and the card's meta line drops what the row says. A tree row
  says the pull request with its checks in its meta line.
- **Agents see it too.** `project_status` and `task_get` carry each task's `checks` (state,
  counts, failing names) and its time at work (`active_ms`, `at_work_since_ms`), so an
  orchestrator reads the same pipeline the person does. `slopty task get` prints the checks
  in a line.
- A card's buttons (Fix CI, Merge, Retry, ...) sit on a line of their own at its foot: a lane
  is too narrow for a title and two buttons, and the title was cut to a word.
- While agents work, the tree and the board move their time on every 10 s, so a readout
  that crosses a minute is never more than a moment late. Before, it moved once a minute and
  could be a minute behind.
- **To-dos block merge.** Claude Code's own task list is the agent's word for what is left.
  The merge queue gives back work whose list still has items open, naming up to five of them,
  before it rebases anything. The board hides Merge while they are open and shows "N to-dos
  open" on the row.
- Tests:
  - `pull_request_checks` (goldens `pull_checks`, `outcome_checks`, `moment_checks`);
  - `the_forge_s_command_is_run_in_the_checkout` and the JSON readers
    (`slopty-worker::repo::checks`, with `#!/bin/sh` stand-ins for `gh` and `glab`);
  - `a_pull_request_s_checks_are_read_where_its_work_is` and
    `open_to_dos_keep_work_from_merging` (`slopty-server`, through a worker's link);
  - `a_task_s_pipeline_says_each_stage_and_open_to_dos_hold_the_merge` (`slopty-ui::project`);
  - `a_card_draws_its_pipeline_once_its_work_is_on_its_way` (`slopty-ui::workspace`).

## The board talks to the orchestrator (2026-10-02)

**The board's foot is a line to the orchestrator.** ✅ 2026-10-02
- Before: the board took the orchestrator's place in its tile, so directing it meant turning
  the tile back to the terminal and losing the board. The person mostly directs and watches,
  so the two belong together.
- Now every board with an orchestrator ends in one line ("Tell the orchestrator what to do
  next"). `c`, or the palette's "Tell the orchestrator…", puts the keyboard on it, and Escape
  gives the board its keys back. Enter sends the words and clears the line.
- The words go as `TaskTell` with no task: the server logs `Moment::Told` on the project's
  timeline and hands them to the orchestrator through its hooks, as it hands it reports. Its
  inbox wakes it when it is idle. Nothing is typed into its terminal, so a prompt the person
  has half-written in the TUI is left alone, and the words are on the timeline for anyone
  following the project.
- A board whose orchestrator is not running is refused in the server's words, and the words
  go back on the line unless the person has started another.
- The person's words are now all kept on their way. Before, a second message to a task's
  agent replaced the first one still unread. That fitted a next-step button pressed twice,
  but not a line the person writes on.
- The board's bare keys (`m`, `r`, `1`, ...) hold only while the board has the keyboard. The
  line sits beside the board's key context, not inside it, so a letter typed on the line is a
  letter.
- `slopty project tell <project> <words>` does the same from a shell.
- Tests: `the_person_s_words_go_at_once_beside_the_server_s` (`slopty-server::deliver`, now
  with the orchestrator's node) and `the_board_talks_to_its_orchestrator`
  (`slopty-ui::workspace`).

## Where the work goes, by what it needs (2026-10-02)

**A project says what each kind of its work needs of a machine.** ✅ 2026-10-02
- Before: every task carried its own placement rules, so an orchestrator had to repeat "this
  needs a Mac" on each task it made. The role told it to add `os == "linux"` by hand, and the
  board could only show the raw rule as the reason a task went where it did.
- Now `Project.needs` holds `Need { name, paths, require, prefer }`, said with
  `Verb::ProjectNeeds` (MCP `project_needs`, `slopty project need`). A task owning one of a
  need's paths, a path within one or one that holds one, has that need; a need with no paths is
  every task's. Its rules join the task's own wherever the task is ranked: a spawn, a proposal,
  "Run on…" and `placement_suggest`. A rule the task holds already is not added twice.
- The rules stay CEL over the workers' open facts, so a need is as open as a rule: "Apple
  work" over the app's paths requires `os == "macos"`, "Linux first" over everything prefers
  `os == "linux"`, "GPU work" requires `has(probes.cuda)`. The name is free text the person or
  the orchestrator chooses; nothing in Slopty knows a list of them.
- Explainable: each `Reason` a need brought carries the need's name (`Reason.need`), and
  `Suggestion::why` says the name in the rule's place: the card's place reads "Apple work", a
  ranking "Linux first +20", and a worker kept out "fails Apple work (os == "macos")". The rule
  itself stays in the reason for anyone who asks.
- Only the person or the orchestrator says the needs; a task's agent is refused. A rule that
  does not compile is refused naming its need, and so are the needs together when they would
  hold more rules than one placement may (32 of each kind), since a task with every need has
  all of them. A task whose own rules and its needs' come to more is refused naming the needs.
  A change is a timeline entry (`Moment::Needs`), since it moves where work goes from then on.
- The machines lens lists the needs under "What the work needs", each with the paths it covers
  and what it asks. Each row of the tree, and each card, ends in a place chip: the worker and
  its system ("studio · macOS"), how it is there (runs, ran, pinned, would start), its worktree,
  branch and reason in the hint. A click on a task not started yet opens "Run on…"; on any
  other it shows the machines lens. The chips sit at the end of the line so they read as one
  column down the tree.
- Rejected: a fixed list of platforms or capabilities on a task. It could not say "a Mac on AC
  power" or "the box with the GPU", which open facts and CEL already do.
- Tests: `a_need_follows_the_paths_a_task_owns` and `a_ranking_says_what_decides_it`
  (`slopty-proto`), `work_goes_where_its_needs_say_and_the_board_says_which` (`slopty-server`),
  `a_need_and_the_reasons_it_brings_say_its_name` (`slopty-tools`),
  `a_need_says_what_it_covers_and_what_it_asks` and
  `a_node_says_where_it_runs_and_why_and_whether_it_can_move` (`slopty-ui::project`),
  `every_node_says_where_it_runs_and_a_waiting_one_moves_from_there` (`slopty-ui::workspace`),
  and `a_live_task_shows_its_checks_its_time_and_its_next_steps` (app e2e, where the project
  needs a Mac for its Apple work and the card says so).

**An agent goes only where it is installed, pinned or not.** ✅ 2026-10-02
- A start that runs an agent (Claude Code, Codex, or a command whose program is one of them)
  is ranked with a built-in check, `agent`: the worker's `agents` facts must list it. A pin
  does not get round it, unlike a rule: opening a program that is not there only fails later,
  in a terminal nobody is watching.
- The agents a worker registered with (`WorkerCaps.agents`) count as installed before its own
  facts arrive, so a worker that just started takes Claude Code tasks at once. A worker that
  has not reported its facts yet is refused for any other agent with "has not said yet whether
  codex is installed", which an orchestrator can retry; one that has reported says "codex is not
  installed".
- A held `agent` check is left out of `why`, like `online`, since every worker that fits shares
  it.
- Tests: `an_agent_goes_only_where_it_is_installed_even_pinned` (`slopty-server::placement`) and
  `a_codex_task_goes_only_where_codex_is_and_starts_with_its_role` (`slopty-server`).

**Codex runs a task, with Slopty's tools.** ✅ 2026-10-02
- `Runner::Codex { prompt, args }` (MCP `task_spawn` with `agent: "codex"`, `slopty task spawn
  --codex`) opens the person's own `codex`, unmodified. The server gives it its role through
  Codex's own `developer_instructions` config (`-c developer_instructions="…"`), the place Codex
  documents for instructions a tool adds, rather than inside the first prompt. The brief stays
  the first prompt the person sees, and the role is not lost when the conversation compacts.
  The cost is that it replaces any `developer_instructions` in the person's own config for that
  run.
- The worker wires Slopty's tools in as it opens the terminal, as it does for Claude Code: `-c
  mcp_servers.slopty.command="<relay>"`, `args=["mcp"]` and `env_vars` naming the server,
  project, task, session and token variables, since Codex hands an MCP server only the
  variables it is told to. Codex 0.156.1 reads these keys back as given (`codex mcp get slopty
  --json` with the overrides, under an empty `CODEX_HOME`). Arguments that name the `slopty`
  server themselves are left alone; a prompt that merely mentions it is not taken for one.
- A writing task beside a clone runs with Codex's own `--worktree`, so two Codex tasks never
  edit one checkout. Codex makes and names that worktree itself, so its role tells it to name
  the branch when it reports done.
- Its arguments are judged before it starts, as Claude Code's are: only what asks the person no
  less goes through without the person's `permission_flags`. That is the model, images, a
  worktree, a `read-only` or `workspace-write` sandbox, the `untrusted` or `on-request` approval
  policy, and `-c` keys of the model alone. A bypass, `--full-auto`, `--approve-for-me`, a
  looser sandbox or policy, a profile, any other config key (an MCP server, a hook, a policy),
  another root, a subcommand and `--` are refused, naming the word.
- Tests: `only_what_asks_the_person_no_less_goes_through` and
  `codex_starts_with_its_role_and_its_brief` (`slopty-server::hub::codex`),
  `a_codex_task_goes_only_where_codex_is_and_starts_with_its_role` (`slopty-server`),
  `codex_opened_on_a_worker_with_a_server_gets_slopty_s_tools` (`slopty-worker`, the stub
  standing in for `codex`), and the `task_spawn` and `project_needs` cases of
  `the_project_tools_default_to_the_caller_s_own_project_and_task` (`slopty-tools`).

**A project is a name and its members; orchestration is a part it may have.** ✅ 2026-10-03
- The person works on a few projects spread over many machines and wants Slopty organised by
  them, with the machine as one fact among others (`.research/organization-2026-10-04.md`).
  Most of the projects a client groups by are derived and never stored: a repository's clones,
  a folder (`docs/decisions/ui.md`, "The navigator groups by project; the machine is a facet").
  A declared project on the server is how the person names one by hand.
- `Project::members` holds what else is in it besides its repository's clones: a matcher each,
  an open map of fact key to the value a tile must have, or for a path the directory it must be
  in (`{repo: github.com/o/api}`, `{machine: studio, cwd: ~/notes}`). So one project may hold
  two repositories, or one folder name on two machines. Matchers are evaluated on the client
  over the facts it already assembles, so no expression language runs off the server (CEL stays
  the placement language).
- `ProjectCreate` takes the members and `ProjectSet` replaces them whole (absent leaves them).
  The server trims each value and refuses an empty matcher (it would match nothing), one named
  twice, more than `Project::MEMBERS_MAX` (32) or a matcher past `Project::member_fits` (8
  keys, a key with no space, a value up to 1024 bytes).
- A project with no orchestrator, no repository and no target is kept and listed: its
  repository, target, verifier and orchestrator are the part it gains when it orchestrates.
  `repo` and `target` stay strings, empty for a plain project, since every orchestrating path
  reads them as given and an empty one already means "none named".
- An item names its project itself where no fact could (a window, a display, a note, a page):
  `Item::facts` is an open map the worker keeps with the item, set by `ItemOp::SetFact` and
  trimmed and bounded (`fact_fits`: a key up to 64 characters with no space, a value up to
  1024 bytes, since a pin holds a group's key and that may hold a path; at most 32), so a pin
  is the same on every device. A thread's row says the
  repository its directory is in (`ThreadRow::{cwd, repo, repo_id}`, the origin read from the
  config file once per directory), so an agent with no terminal groups as a shell does.
- Tests: `slopty-proto` `a_fact_is_said_and_taken_back_within_its_bounds`,
  `a_member_names_facts_within_bounds` and the goldens `client_item_set_fact`,
  `worker_item_pinned`, `project_create`, `project_set`, `table_snapshot`; `slopty-worker`
  `an_item_fact_is_kept_and_broadcast`, `a_thread_row_names_its_repository_once_known`;
  `slopty-server` `a_project_without_an_orchestrator_is_kept_and_listed`,
  `members_name_clones_and_folders`.

## Phases

1. **Wiring and state.** Built 2026-09-30, except the tile.
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
   - Mirror presence as a placement fact (placement itself was built in Phase 1).
   - A worktree setup file (A3).
   - Linux worker hardening: systemd, x86_64 and a real network e2e.
3. **Verify and merge.** The verifier, the merge queue and the reviewer built 2026-10-01.
   - The verifier runs per task, and the merge queue on the server.
   - The fresh-context reviewer.
   - The timeline, and tokens per agent.
4. **Scale.** The shared build cache, channel push and moving agents between workers, each only
   after it is measured.
