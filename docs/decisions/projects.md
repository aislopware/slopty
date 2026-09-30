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
  online and live_agents. Facts travel beside `WorkerInfo`, not in it, so the directory's wire
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
  it, and the server marks a `Delivered` moment. A hook that dies in between leaves the batch
  for the next one. Keeping, handing over and dropping run one at a time per worker, so a
  batch kept meanwhile is never dropped in its place. A batch not acked is sent again when the worker registers again,
  folded into the next one, or put back when the terminal closes.
- Report text cannot close its `<slopty-reports>` block, so a report never reads as the
  server's own words.
- Why hooks, not typing: the TUI is the source of truth and nothing types into it behind the
  person's back. Hooks are Claude Code's own door for context, and they reach an agent the
  moment it next thinks, at no cost while it is idle.

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

Not built in Phase 1:
- worktrees and mirrors, so a task's `cwd` must already exist on the placed worker;
- mirror presence as a fact;
- the verifier run and the merge queue;
- the known gaps under "An agent never has more than the person gave it".

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
3. **Verify and merge.**
   - The verifier runs per task, and the merge queue on the server.
   - The fresh-context reviewer.
   - The timeline, and tokens per agent.
4. **Scale.** The shared build cache, channel push and moving agents between workers, each only
   after it is measured.
