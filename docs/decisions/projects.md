# Projects

A project is one goal worked on by many agents across the fleet. The person talks to one
orchestrator agent. It splits the goal into tasks, starts each task's agent on the worker it
names (a Linux worker for the server and the Linux crates, a Mac for anything Apple), and the
server verifies and merges what they finish. Every agent in the project can be seen and opened
while it works. Nothing runs where the person can't follow it.

The research behind these rulings, with sources, is in `.research/projects-research-2026-09-30.md`
(primitives, products, distribution, measured cost), `.research/projects-foundations-2026-09-30.md`
(what Slopty already has) and `.research/feature-prune-frontier-2026-10-04.md` (what a frontier
model no longer needs).

**The orchestrator is an agent with the project's tools, not a driver.** ✅ 2026-09-30
- The orchestrator is an ordinary interactive agent session (Claude Code in a PTY tile, or any
  agent Slopty runs). What Slopty adds is the project's tools over MCP (`slopty mcp`):
  `project_status`, `task_get`, `task_start`, `task_update`, `task_report`, `task_tell`,
  `task_wait` and `read_thread`. Everything else (the workers and their facts, terminals,
  files, the workspace) is the `slopty` command, which the agent runs in its shell like any
  other. The agent chooses to call them, and its TUI stays the source of truth, as for every
  agent.
- Rejected:
  - **Claude Code's own Projects as the backbone.** It is a public beta on Pro and Max, and its
    orchestrator conversation lives at claude.ai, off the tailnet.
  - **Agent teams.** They are experimental, off by default, allow no nested teams, and put no
    teammate in a worktree.
  - **Wrapping the CLI and parsing its screen.** Omnara abandoned exactly that as "unfeasible to
    maintain".
- Kept for later: bridging to Claude Code Projects through Remote Control, if the person wants
  to steer from claude.ai too.

**One level of tasks, and every task is a visible session.** ✅ 2026-09-30, narrowed 2026-10-04
- The project is the orchestrator and its tasks, side by side. A task's agent starts no task
  and tells none: it does its work and reports it. Inside one session an agent may still fan
  out with its own subagents or workflows, which show as its task's natives.
- The measurements favour shallow, centralised trees. Google's scaling study measured
  independent agents amplifying errors 17.2× and centralised ones 4.4×, and coordination
  collapsed throughput in Cursor's experiments.
- Every session Slopty starts is a tile with the brief as its first prompt. A native subagent
  inside a session appears under its task from its `SubagentStart`/`SubagentStop` hooks, or
  from its thread's row for other agents.

**A frontier model is steered by a sentence, not by machinery.** ✅ 2026-10-04
- The person's direction is frontier models only: a premium agentic app where remote work feels
  local. Under it, every piece of the projects mode that steered the orchestrator by rule where
  a sentence does was cut or merged. Claude Code's own Projects does the same jobs with plain
  instructions and one hard cap, and Amp's record of removals says a simple shell tool is often
  enough.
- What went, and what carries the job now:
  - **Placement rules and project needs** (CEL over the workers' facts, `placement_suggest`,
    and the forked `cel` crate). The orchestrator reads the facts (`slopty --json workers`) and
    names the worker in `task_start`; its role says that work needing no Apple platform belongs
    on Linux. The server still checks that the worker is online and has the agent installed.
  - **Budgets and per-project caps.** A plan already caps itself, and Slopty runs the person's
    own subscription binaries. One hard cap stays, the fleet's live agents, as Claude Code keeps
    one daily cap, and so does the review limit.
  - **The server's reviewer stage.** Review goes through the agent's own door: the review tile
    gains a way to ask the thread's agent for its own review (Claude Code's `/code-review`,
    Codex's `review/start`), with the findings as line comments (`docs/decisions/agents.md`,
    "Review with <agent>"). An orchestrator may still start a
    read-only task to review, in words.
  - **Path claims.** Worktrees isolate writers, and a conflict comes back as a give-back on
    rebase. Claude Code Projects and Cursor coordinate without claims.
  - **Report kinds other than done.** `task_report` means done. A need or a block reaches the
    orchestrator as the turn end it already hears, and the person as the agent's own question.
  - **Asking before each task starts** (`ask_to_start` and its proposals). A project's rules
    say it in a line when the person wants it.
  - **Dollar cost.** Slopty touches no credential, so it cannot know how a run is billed.
    The plan's windows stay, in the status bar ("The board is its lanes alone", below).
  - **Most MCP tools.** `slopty mcp` serves the eight project tools above, not 46. An agent
    Slopty starts outside a project gets no Slopty MCP at all, and reaches the rest through
    the `slopty` command. Every agent saves a tool block of about 24 KB of descriptions, and
    Codex retired its own MCP server for the same reason.
  - **`slopty git`.** A person or an agent at a shell already has git and gh. The person's
    commit sheet in the app stays.
- Rejected: keeping them as hidden options. An unused feature is deleted, not hidden.

**The board is its lanes alone.** ✅ 2026-10-04 (frontier prune step 4; overturns the tree,
timeline and machines lenses and the board's meters)
- Tasks are one level, so the board needs one view of them. The lanes say where each task
  stands, and the thread view already shows a task's subagents, so the other lenses repeated
  what the lanes said. What went:
  - **The tree, timeline and machines lenses**, with their keys (`1` to `4`) and palette lines.
    The timeline stays on the server, for the recap and the orchestrator.
  - **The estimate of the time left**, and the machines lens's `WorkerFacts` polling every 5 s.
  - **The context meters and plan windows on the board.** The status bar shows the plan's
    windows. The header kept the project's time at work until 2026-10-06 (see "Time at work
    per task" below).
  - **Opening a subagent from the board.** A task's card opens its agent, and the thread view
    leads into the subagent.
- **Run on…** stays. It is a control on the card stood on (key `o`), and the place chip moves
  a task only while it has not started. A chip that cannot move is a label, with no pointer.
- The "Needs you" band now holds only the orchestrator, since a task waiting on the person
  stands in its own lane. It sits on the lanes' left edge rather than past the column's edge.
- With the server always there, a board's action has no "No server to send it to" notice.
- Tests: `the_board_says_each_thing_once_and_fills_its_tile`,
  `a_task_not_started_is_pinned_from_its_card`,
  `every_card_says_where_it_runs_and_a_waiting_one_moves_from_there` and
  `the_board_says_what_its_agents_spent` (`slopty-ui::workspace`). The `project-lanes`,
  `project-lanes-dark` and `project-live-lanes` goldens were retaken. `project-tree`,
  `project-live-tree` and `project-timeline` were deleted.

**Project state lives on the server.** ✅ 2026-09-30
- The server holds, in a store beside `workers.json`:
  - the project: its name, repository, target branch, verifier command and orchestrator session;
  - its tasks: title, brief, kind, dependencies, the worker it is pinned to, state and status
    text, its assignment (worker and session), branch, the verifier's result and the merge,
    and free-form metadata;
  - an append-only event timeline.
- Workers report; clients mirror, as they mirror the item registry. The project outlives any
  client, and a phone sees the same project as the Mac.
- `AgentBranch` stops being dropped at the worker's server link, so the hub learns every PR and
  worktree.

**Code moves through each worker's own clone.** ✅ 2026-09-30, revised 2026-10-01
- Each worker clones the repository from the forge with its own git, and a task's agent works
  in a worktree of its own ("A task placed where there is no clone gets one"). A finished
  branch crosses machines as a bundle through the server ("A finished task's branch comes home
  as a bundle"). There is no bare repository on the server and no remote helper of Slopty's.
- `.git` is never file-synced (Mutagen documents why not). Worktrees on one host share objects;
  across hosts they fetch.

**A task is done when its verifier passes; the server merges one at a time.** ✅ 2026-09-30,
merging on the person's word since 2026-10-04
- The project names its verifier (`cargo gate` here). A finished task's branch runs it on a host
  that can, and the result is recorded on the task. A pass leaves the task ready to merge.
- The person's Merge puts it in the server's merge queue, which rebases each branch onto the
  target branch, runs the verifier again, and fast-forwards, one branch at a time. What the
  person can review sets the pace, so merging is theirs alone. A conflict or a failure goes back to
  the owning agent as a message with the details. There is no integrator agent: Cursor found
  that role became the bottleneck.

**The person's plan bounds concurrency, with one hard cap and the review limit.** ✅
2026-09-30, narrowed 2026-10-04
- All agents draw on the person's plans, and multi-agent runs cost 7-15× the tokens of one
  session.
- The person's `live_agents` bound in the server's settings caps live agents across the fleet.
  A project's review limit (3 by default, the person's alone) stops new starts while that many
  of its tasks wait on the person, so what the person can review sets the pace. The status
  bar shows the plan's windows as the agents report them.

**What the user sees.** ✅ 2026-09-30
- A project opens as a tile. It shows:
  - its tasks, each with its worker (and OS), branch, state (working, waiting, blocked,
    verifying, merged) and last line;
  - a timeline of the events that matter (started, blocked on you, verifier passed or failed,
    merged, conflict);
  - at the top, anything waiting on the person, such as a permission or a question.
- Clicking a task opens its agent's tile. The board's line talks to the orchestrator only.
  Minimal, in the Warp and Linear school, and every colour a token.

**Tests use a stub agent.** ✅ 2026-09-30
- End-to-end tests run a stub `claude` (a test binary that speaks the hook protocol and makes
  scripted commits), never the real one. This proves the project end to end without spending
  quota:
  1. The orchestrator starts.
  2. A task starts on the worker named, and its branch comes home as a bundle through the
     server.
  3. The verifier runs, the merge queue merges, and the board and timeline show each step.

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
  cursor), `TaskCreate`, `TaskUpdate`, `TaskSpawn`, `TaskReport`, `TaskGet`, `TaskTell`,
  `WorkingOn` and `WorkerFacts`, among others. The CLI serves them all (`slopty project …`,
  `slopty task …`, `slopty workers --json`). `slopty mcp` serves the eight that a project's
  agents need: `task_start` makes a task and starts it in one call (`TaskCreate` then
  `TaskSpawn`), as `slopty task start` does.
- A task's verifier run names the commits it judged (`VerifierRun { head, base }`, in hex), so
  a verdict never outlives the work it saw.

**The task model is open.** ✅ 2026-09-30
- A task has a free-form `kind` (up to 64 characters), a `status` text beside its fixed state
  (up to 512), and `metadata`, any JSON object up to 16 KiB. A project has metadata too.
- `depends_on` names other tasks of the project. The graph must stay acyclic, so a dependency
  that closes a cycle, or names an unknown task, is refused. A task whose dependencies are not
  done is refused its start unless the start says to ignore them, and kept to start later.
- Tasks do not nest: every task belongs to the project directly.
- A task runs a `Runner`: Claude Code with its prompt and arguments, Codex, any agent as a
  thread, or a command (a build, a script; the login shell when empty). A task may name its
  own verifier over the project's.
- A read-only task runs in the clone itself, not in a worktree of its own.
- Every move between states is checked (`TaskState::may_become`). Merged is final, and only a
  done or verifying task merges.

**Workers report open facts.** ✅ 2026-09-30
- A fact is a flag, a number, a word, or a list or map of them (`Fact`). Each worker sends its
  facts on the server link (`ToServer::Facts`) when they change. They are gathered lazily and
  cached (`slopty-worker::facts`), never on a hot path:
  - installed agent CLIs and toolchains with their versions, Rust targets, GPUs;
  - AC or battery.
  - The person's `[worker.labels]` and `[worker.probes]` were cut on 2026-10-05 ("Labels and
    probes are gone", below).
- The server adds what it knows itself, over anything a worker sent under the same name: name,
  worker, os, os_version, arch, cpus, memory_mb, encoders, displays, capture and input, load,
  online, live_agents and repos. Facts travel beside `WorkerInfo`, not in it, so the directory's
  wire shape is unchanged. `slopty workers --json` and `WorkerFacts` show them all.
- The facts are for the orchestrator to read. It names the worker a task runs on; nothing on
  the server judges rules over them ("A frontier model is steered by a sentence", above).
- A worker's facts are cut to bounds as they arrive (1024 items per list or map, 4 KiB per
  text, 4096 facts in all).

**Where a task starts.** ✅ 2026-09-30, narrowed 2026-10-04
- A task runs on the worker its start names (`task_start`'s `worker`, a name or an id), or on
  one the person picked with "Run on…". A start that names none goes to a worker with room:
  beside a clone of the project's repository when the task named no directory and a worker has
  one, then the fewest live agents, the least load per cpu, and the name.
- Wherever it goes, the worker is online and has the agent installed ("An agent goes only where
  it is installed, pinned or not", below). A start pinned to a worker that is offline or lacks
  the agent is refused and never moves elsewhere.

**One cap is a setting, under the person's bounds.** ✅ 2026-09-30, narrowed 2026-10-04
- The person's `[server.projects]` bounds in `settings.toml` hold two things: `live_agents`,
  the most live agents across the fleet (24), and `permission_flags`, the projects whose agents
  may be given flags that loosen Claude Code's permissions. The server reads the file at start
  (`Hub::set_policy`). Agents read the bounds and the live counts in `project_status` and cannot
  raise them.
- A project's own `Limits` hold the review limit alone, which only the person sets.
- The other sizes are constants in the server, not settings: projects kept (64), tasks per
  project (512), title, brief and timeline lengths.

**Counts follow live terminals, not task states.** ✅ 2026-09-30
- A task's run counts from the moment it is started until its terminal ends, whatever its state
  says. A start whose caller left, or an agent that marked its own task done, cannot escape the
  cap. A start counts for up to 30 s after its worker answers, until the worker announces the
  terminal.
- The hub chooses the id of every terminal it starts (`SpawnAgent` and `OpenTerminal` carry
  it), and a caller never can. So a start whose answer was lost still counts for its 30 s, and
  when its worker announces that id the terminal goes on its task as if the answer had come.
  An agent cannot name another's terminal as its own start.
- A command task counts like an agent, since it may be another agent's CLI.
- Since 2026-10-03 one exception: the agent of a task merged or given up counts against no
  limit while it rests, and counts again as soon as it works ("A finished task's agent stops
  counting", below).
- The fleet bound counts every live terminal with an agent in it, in a project or not, plus
  every start in flight. A plain `slopty agent spawn` is refused at the bound as a task's start
  is.
- A project's orchestrator counts only while its terminal is live.
- A terminal an agent opens counts against the fleet bound as a start does, and a terminal an
  agent typed into counts as an agent from then, so a plain shell cannot carry an agent past
  the bound.

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
  - A task's agent works in its own task alone: it moves and reports that task, and makes,
    starts and tells none, since tasks are one level.
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
- Only the person answers a request (`AnswerRequest` from an agent is `Forbidden`),
  merges a task, records its verifier, or names a verifier for a project or a task: a verifier
  is the person's word on what counts as done. Only the person's read of a thread holds its
  prompts, so an agent reading another's never hides a prompt from the TUI.
- Every way to start `claude` is judged by its arguments against an allowlist
  (`slopty_proto::project::SAFE_FLAGS`): `SpawnAgent`, `TaskSpawn`, and a terminal whose
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
  coming never keep an agent from resting.
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

**`TaskSpawn` is the linkage; `SpawnAgent` keeps its wire shape.** ✅ 2026-09-30
- The server chooses the task's worker and forwards an ordinary `SpawnAgent`, or `OpenTerminal`
  for a command. `SLOPTY_PROJECT` and `SLOPTY_TASK` go last in its env, so they win over the
  caller's. The new terminal is then assigned to the task.
- The start is detached from its caller, so a caller that goes away mid-start still leaves its
  terminal on the task. A terminal that its task can no longer take (merged meanwhile, say) is
  closed, not left running outside any count.
- `task_start` (and `slopty task start`) is the one way work starts: it makes the task and
  sends `TaskSpawn` in one call, or starts a task the project already has. A plain
  `slopty agent spawn` starts an agent in no task.
- The worker's own spawn adds `--mcp-config=<json>` naming `slopty mcp` on stdio. The flag is
  variadic in Claude Code, so the `=` form keeps the next argument from being taken as a second
  config.
- Every session a worker starts carries `SLOPTY_SERVER` when the worker has a server. So
  `slopty mcp` and `slopty` in any shell find it with no flags, and Claude Code passes it on to
  its MCP servers.
- A task's agent starts with a conversation id the server chose (`--session-id`, unless its
  arguments pick one), kept in its assignment (`Assignment.conversation`), so the task knows
  its transcript before the first hook.
- A task's agent is told its role (`--append-system-prompt`): its project and task, whom it
  reports to and how (`task_report`), and the project's `agent_rules` from its metadata when
  set. The orchestrator is told its own, with `orchestrator_rules`, as its first delivery
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
  `TaskSpawn` or `slopty open` never starts a second terminal; the key is the caller's own
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
- A task's agent reports its work done to the project's orchestrator (`task_report`, with a
  note, artifacts, a branch or a pull request). A report means done: a need or a block reaches
  the orchestrator as the turn end it already hears ("A task's outcome reaches the
  orchestrator without a report", below). The report lands on the task's timeline at once.
- Reports wait per node on the server (`slopty-server::deliver`). A finish goes once it has
  settled for 2 minutes, a later report of the task replacing it. A batch is at most 9000
  bytes, and each node has one batch outstanding.
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
  is in, mapped to the clone's path, so `repos["github.com/o/r"]` says where a clone is. An orchestrator's role names its repository by that
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
- A start with an empty `cwd` prefers a worker whose `repos` hold either key of that identity,
  and the task starts in the clone's root on the worker chosen.
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
  Linux worker.
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
  - the worker's facts reaching `slopty workers --json`, and a command task made and started in one `slopty task start` on the worker it names, run with
    its project and task in its env;
  - an agent started with a stale `SLOPTY_TASK`, then assigned to another task, updating the
    task the server says it is on.
- The model (`slopty-server::project`), the placement (`slopty-server::placement`) and the hub
  (`hub/project_tests.rs`) test each ruling above at their own layer. The hub tests cover
  concurrent starts at the cap, a caller that leaves, a lost start adopted, the fleet bound,
  the permission flags on every path (shell lines included), the mode and command-line
  backstops, a shell an agent drove speaking as an agent, an agent's environment, typing into an
  agent's TUI and naming a verifier refused, an agent's terminals under the fleet bound,
  keyed starts replayed and scoped to their caller, reports batched, paced and parked, and a
  restart's reconcile. They also cover an agent's scope: the
  terminals it may put to work, a project's allowance for its own agents only, the terminals
  it opened kept across a restart, and a task's agent refused every task but its own.
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
  the task goes to the worker named or the one with most room, and that worker clones the
  repository first.
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
- A clone under way when the server stops is made again once its worker is back (see "A step
  a restart left under way is taken up again").
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
    `slopty-worker::repo::verify`). Each rebased commit carries where it came from as
    trailers (`Slopty-Task`, and `Slopty-Thread` when the thread is known).
  - Fast-forward only, so the target never holds a merge commit the verifier did not see.
  - When the rebase leaves the tree that already passed, because the target had not moved,
    the verifier is not run again, though the trailers changed the commits. This is
    Mergify's direct merge.
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
- A pass leaves the task done, ready to merge. The person's Merge puts it in the queue
  (`Task.merge = Merge::Queued { since_ms }`). The queue is its tasks, done and queued, in the
  order they joined. Verifying comes before merging, since an agent waits on each verdict.

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
- The orchestrator hears too, at once:
  - "given back" with the reason;
  - "merged into <target> at <short>", since what depends on the task can start then.
- A job that stops for a reason that is not the task's holds the lane with that reason on the
  step, and the task keeps its place. Examples: the worker is away, or the person's checkout of
  the target has changes in the way. A worker registering again starts the lanes once more, and
  so does the next change that concerns the project.
- Only the person asks for a merge (`TaskMerge`, `slopty task merge`). An agent's call is
  `Forbidden`, telling it to report its work done. A task whose verifier has not passed is
  verified first, and one that passed or has none joins the queue at once. `Verify`, `Rebase` and `FastForward` are the server's own verbs and
  `Forbidden` to every caller.

**At most three automatic give-backs.** ✅ 2026-10-04
- A failed verifier or a rebase conflict goes back to the task's agent at most
  `GIVE_BACKS_MAX` (3) times, counted together (`Task::give_backs`). The next failure is held
  for the person: the agent is not told, and the task waits on the person until they say what
  next. Their next word on the task starts the count again.
- Why: an agent that cannot fix its work would otherwise loop on the plan, one verifier run
  per try, with nobody looking.
- Test: `a_task_given_back_three_times_waits_on_the_person` (`slopty-server::hub::queue`).

**The queue survives a restart.** ✅ 2026-10-01
- The lane keeps nothing the store does not. Each time, it reads its next job from the tasks
  (`Projects::next_job`): the longest verifying task, else the head of the queue.
- A step under way when the server stopped is taken up again once its worker is back (see "A
  step a restart left under way is taken up again"). The task keeps its state and its place,
  and registration starts the lane again.
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
    queue does not hold yet, waiting for the person's Merge, comes last.
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
  - `--mcp-config` naming `slopty mcp` when its session names a project (a task's thread);
    any other gets one paragraph on `--append-system-prompt` pointing it at `slopty --help`
    (`slopty_agent::hooks::POINTER`, joined to a system prompt the person appends), since
    Slopty's tools are for a project's agents (2026-10-04);
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
- Two system prompts are kept: the one appended to an agent started with Slopty's tools, which
  is the server's role, and one that carries the pointer to Slopty's CLI. A system prompt on
  any other agent is still never written down, since it may carry anything.
- A shell the person ran the agent in gets everything back but the role. The role spans lines,
  which a line typed at a prompt cannot carry.
- `--worktree` is not kept, because the resume runs in the directory the agent was in, which is
  the worktree. Kept, it would make another worktree.
- Tests: `slopty_s_own_wiring_is_noted_and_its_role_kept` (`slopty-agent`) and
  `a_project_s_agent_comes_back_with_its_tools_role_and_lock` (`slopty-worker::restore`).

## The board acts, and a project can be let go (2026-10-01)

**What moves a task on is a button on its row and its card.** ✅ 2026-10-01
- Before: the board only showed. Merging a task no check queued, or trying a failed step
  again, took `slopty task merge` in a shell.
- Now `Board::actions` names what the person can do to each task, and the row and the card
  draw it as small buttons. The palette and the keys `m` and `r` do the same to the task
  the board stands on:
  - **Merge** a finished task the merge queue does not hold yet, which is how work joins it;
  - **Retry** a failed home, verify or merge step. Both send `TaskMerge`, which runs the
    verifier again on the branch.
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
  record in the log. The agents' terminals stay, since they may hold work the person still
  wants.
- An agent asking for it is refused with `Forbidden`. The verb is server-only: no worker
  carries it, and no MCP tool offers it.
- `ProjectUpdate` has no removal. So the server sends every client a fresh projects
  snapshot, and that snapshot's first part replaces the client's mirror. Adding a removal to
  the update would have been one more wire shape for a rare event.
- The board asks twice: the first "Delete the project" says what a second does, and only a
  second within 5 s sends the verb. The CLI's `slopty project delete <name>` sends it at once,
  since a typed name is deliberate.
- Tests: `the_person_lets_a_project_go_and_every_client_hears_it` (`slopty-server`), the
  `project_delete` golden.

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
- "Passed" and "Failed" are words with a glyph, not colours. Red is kept for a run that
  failed.
- A row's or card's second line holds three facts at most, so two separators. They are what
  moves the task on, what its agent says, then where it runs. "Not placed" is gone, since it
  repeated on almost every row.
- The header's line is a sentence ("slopty → main, verified by cargo gate"). The live count
  and the progress are readouts at the right.
- The bar's hover says what each segment counts.

**Short lanes stack; the board never wraps a row of lanes under another.** ✅ 2026-10-03
(design review `.research/design-review-2026-10-03.md` #1, #4; superseded 2026-10-05 by **The
board reads in one direction**, below)
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
  second line, place, pipeline, check, its tail, its buttons),
  and the heading as two (`ProjectView::card_lines`). Only the balance rests on the count, and
  a card drawn a line taller or shorter than counted moves nothing else.
- A verdict names its verifier ("Verifier failed", "Verifier passed"), as the timeline does. A
  task the server sends back after a failed check, with no agent left to fix it, is Planned
  (`hub/queue.rs`), so it stands in Up next. There a bare "Failed" read as the Failed lane,
  which is a task given up.
- Tests: `short_lanes_stack_so_every_lane_stands_in_the_first_screenful` (`slopty-ui`), with
  `the_board_says_each_thing_once_and_fills_its_tile` unchanged. The `project-lanes` golden
  was retaken.

## Where tasks run (2026-10-02)

**The board has a machines lens, and the person can move a task before it starts.** ✅
2026-10-02, narrowed 2026-10-04; the lens went on 2026-10-04 ("The board is its lanes alone")
- Before: the board showed which worker a task was on, but never which machines had room.
- The machines lens (key `4`) asks for `WorkerFacts` every 5 s while it is shown, and stops
  when it is not. It groups the tasks by host: first the workers holding tasks, then the online
  ones, then the ones away, each by name. A host's heading gives its kind ("macOS, 12 cores,
  64 GB"), its load and its live agents. The tasks that have not started sit under "Not
  started".
- **Run on…** (key `o`) opens a picker on a task that has not started. It lists "Anywhere" and
  every worker by name with its system, the agents it runs and whether it is online. A pick
  sends `TaskUpdate { run_on }` (`RunOn::Worker` or `RunOn::Anywhere`), which sets or clears
  the task's pin. `TaskCard.pin` shows the pin to the board. The CLI's `slopty task update
  --run-on <worker|anywhere>` does the same.
- Tests: `a_task_runs_where_the_person_says_and_keeps_why_it_went_there` (`slopty-server`) and
  `a_task_not_started_is_pinned_from_its_card` (`slopty-ui::workspace`).

## What changed while you were away, and the time it took (2026-10-02)

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
  move the person: a verifier, checks or a step that failed, a conflict, an agent that ended
  before its work was done, and then merged, verified, started and created. Each kind is one line naming its tasks in the order they last moved
  ("Verifier failed on #5 and #6", "Merged #4 Write the decision"), and a line counts past
  three. What needs the person comes first, marked in the warning tone. Project-wide entries
  and notes are left to the timeline.
- The snapshot carries only the latest 64 entries, so a long absence falls before what the
  board holds. The recap then reads the gap back with `ProjectStatus { since }`, a page at a
  time and at most 8 pages. If the server no longer keeps the entries back to the cursor, the
  recap says so on its last line rather than passing for complete.
- The first look on a device has no recap: there is nothing to compare with, and the board
  already shows everything. A cursor past the end belongs to an earlier project of the same
  name, so it is dropped.
- The band sits over "Needs you" until the person closes it or the board hides. Nothing on
  the server changes: it is the client's own reading.
- Tests: `a_recap_tells_what_needs_you_first_and_names_its_tasks` (`slopty-ui::project`) and
  `a_board_opens_onto_what_changed_since_you_last_looked` (`slopty-ui::workspace`, over
  `ServerCaller::queued`).

**Time at work per task, the orchestrator's share apart.** ✅ 2026-10-02. *The board's
readout was deleted on 2026-10-06 (journeys audit, prune-critically): nothing acted on the
minutes once dollar cost had gone, and the header keeps what needs the person. The server
still keeps `Spent`, since its settling reads whether a task's agent is at work, and agents
still read it through `project_status` and `task_get`. Gone: `slopty_ui::project::spend`
(`ProjectSpend`, `Board::at_work`, `worked`), the header's readout and the board's tick that
moved it. The test `the_board_says_nothing_of_what_its_agents_spent` holds that the header
says no time, cost, context or plan figure.*
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
- `TaskCard.spent` carries it to the board. The header shows the project's total, with the
  tasks' and the orchestrator's shares on hover.
- **Context and quota are the agents' threads' word.** (The board's meters went on
  2026-10-04: "The board is its lanes alone".) `slopty_proto::thread::Meters`
  carries a session's context and the plan's rate windows from the agent's own status line, so
  no credential is read. The workspace takes them per session
  (`WorkspaceView::thread_meters`), and the board shows:
  - the context as a figure beside the time, hidden under 20 % and warning from 80 %;
  - each rate window in the header at the fullest any agent reports, warning from 80 %.
- Dollar cost was cut on 2026-10-04: Slopty touches no credential, so it cannot know how a
  run is billed.
- A node whose thread this client has not heard shows its time alone. Tokens beyond the
  context are not summed, since the meters carry the context's size, not a running count.
- Tests: `spent_counts_the_stretches_at_work` (`slopty-proto`),
  `time_at_work_is_counted_per_task_and_apart_for_the_orchestrator` (`slopty-server`),
  `time_adds_up_with_the_orchestrator_apart` (`slopty-ui::project`) and
  `the_board_says_what_its_agents_spent` (`slopty-ui::workspace`). The goldens
  `project_snapshot`, `project_reply_status`, `task_merged_card` and others carry the new
  fields.

**The board's next steps are the person's words to the task's agent.** ✅ 2026-10-02
- Before: when a task's verifier failed, its pull request's review asked for changes or
  its rebase conflicted, the server told its agent once. If the agent stopped short, the person had to
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
  never come. The CLI has `slopty task tell <words>`. (Narrowed on 2026-10-03: the
  orchestrator may tell its tasks too, "The orchestrator tells its tasks", below.)
- While a task's agent runs, `Board::actions` puts its next step first on its row and card,
  and the palette offers each one for the task the board stands on:
  - **Fix CI** while its verifier's failure still speaks;
  - **Address comments** while its pull request's review asks for changes;
  - **Resolve conflicts** after a rebase onto the target conflicted.
  `Board::told` writes the words from what the board knows: the verifier's command, commit
  and first line; the pull request; and git's word on the conflict. Each ends with what to do next. Retry for a failed
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
  - what the verifier said;
  - its place in the queue;
  - "PR #n", its checks in words ("1 of 6 checks fail", "2 of 6 checks running", "checks
    pass") and "Changes requested", each a chip of its own so a narrow lane wraps them rather
    than cutting one long chip;
  - the to-dos still open, shown so the person sees them before a merge; they hold nothing.

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
- Tests:
  - `pull_request_checks` (goldens `pull_checks`, `outcome_checks`, `moment_checks`);
  - `the_forge_s_command_is_run_in_the_checkout` and the JSON readers
    (`slopty-worker::repo::checks`, with `#!/bin/sh` stand-ins for `gh` and `glab`);
  - `a_pull_request_s_checks_are_read_where_its_work_is` (`slopty-server`, through a worker's
    link);
  - `a_task_s_pipeline_says_each_stage_and_its_open_to_dos` (`slopty-ui::project`);
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

## Agents beyond Claude Code, and projects by their members (2026-10-02)

**An agent goes only where it is installed, pinned or not.** ✅ 2026-10-02
- A start that runs an agent (Claude Code, Codex, or a command whose program is one of them)
  is checked against the worker: its `agents` facts must list it. A pin does not get round it:
  opening a program that is not there only fails later, in a terminal nobody is watching.
- The agents a worker registered with (`WorkerCaps.agents`) count as installed before its own
  facts arrive, so a worker that just started takes Claude Code tasks at once. A worker that
  has not reported its facts yet is refused for any other agent with "has not said yet whether
  codex is installed", which an orchestrator can retry; one that has reported says "codex is not
  installed".
- Tests: `an_agent_goes_only_where_it_is_installed_even_pinned` (`slopty-server::placement`) and
  `a_codex_task_goes_only_where_codex_is_and_starts_with_its_role` (`slopty-server`).

**Codex runs a task, with Slopty's tools.** ✅ 2026-10-02
- `Runner::Codex { prompt, args }` (`task_start` with `agent: "codex"`, `slopty task start
  --agent codex`) opens the person's own `codex`, unmodified. The server gives it its role through
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
- Only a project's Codex gets the tools: one whose terminal names a project
  (`SLOPTY_PROJECT`), as the server opens a task's. A `codex` the person opens in a tile is
  theirs and starts as typed (fixed 2026-10-05; it had the tools wherever the worker had a
  server, against the prune ruling that tools are for project agents only).
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
  `a_project_s_codex_gets_slopty_s_tools_and_a_tile_s_does_not` (`slopty-worker`, the stub
  standing in for `codex`), and the codex case of
  `task_start_makes_and_starts_a_task_in_the_caller_s_own_project` (`slopty-tools`).

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
  over the facts it already assembles, so no expression language runs anywhere.
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

## What a task's agent came to, finished tasks settle, and the orchestrator speaks to its tasks (2026-10-03)

From the T3 Code orchestrator study (`.research/t3code-orchestrator-2026-10-03.md`, R1 and
R2).

**A task's outcome reaches the orchestrator without a report.** ✅ 2026-10-03
- Before: the orchestrator heard of a task only through the agent's own `task_report` or the
  merge queue's notices. Agents often end a turn with the answer in their last words and no
  report, exit, or stop on a permission, and then the orchestrator idled with nothing
  delivered until it polled `project_status`.
- Prior art: T3 Code publishes a child's last assistant message, or its error, to the parent
  when the child's run ends. Claude Code's agent teams tell the lead when a teammate stops,
  with its final answer. T3's open #13343 adds telling the parent when a child waits for input.
- The server follows each task's agent through its turns (`project::turns`), from the same
  statuses the task's state follows, and the orchestrator hears, as a server notice:
  - **it ended its turn without a report.** A report during the turn is its word. The rest is
    delivered as a finish is, after `DONE_SETTLE`, with the agent's last line from its thread's
    row (`ThreadRow::last_line`, at most 2 KiB), read again until it goes, since the row with
    the final line can come after the status that ended the turn;
  - **it waits on the person**, naming the open request's title, or the permission or
    question when its thread names none. Heard once per wait, after it has lasted
    `WAIT_SETTLE` (30 s), so a permission the person answers at once wakes nobody. The words
    say only the person answers it: the orchestrator is told so it stops waiting blindly, and
    nothing ever answers for the person;
  - **it exited, or its terminal closed**, while its task still followed it. Heard once, at
    once, with its last words.
- When the agent goes back to work, what was waiting to be said of it is taken back. The
  agent's own report replaces it, and its next outcome does too. One that says again the same
  words of the same kind as the last sent for that task is dropped as it falls due, so an
  agent that flips between idle and working says nothing new. A task verifying, done, merged
  or given up is the merge queue's or the person's to speak of, and is not heard of here.
- It goes through the reports' own delivery: hooks and the inbox socket, and a `Delivered`
  moment on the timeline once handed over. Nothing is typed, and only published doors (hook
  statuses and the thread table the adapters build) are read. The full final message
  (`ThreadRow::last_said`) waits for a proto change that every adapter fills.
- Kept in memory, but a server that comes back takes up each turn its store says was under
  way, so a turn that ended while it was away is still heard, and a terminal gone meanwhile is
  heard as an exit.
- Tests: `the_orchestrator_hears_what_a_task_s_agent_came_to_when_it_said_nothing` and
  `a_turn_that_ended_while_the_server_was_away_is_still_heard` (`slopty-server::project`),
  `the_server_s_word_on_an_agent_gives_way_to_the_agent_s_own` (`slopty-server::deliver`),
  `a_task_s_outcome_reaches_the_orchestrator_without_a_report` and
  `a_turn_that_ended_while_the_server_was_away_reaches_the_orchestrator` (`slopty-server`,
  through a worker's link on a paused clock).

**A finished task's agent stops counting, and is closed once it rests.** ✅ 2026-10-03
- Before: merging closed only the verifier's terminal. A merged task's agent idled at its
  prompt, counted against the live caps, and a long project filled them with finished agents
  until every start was refused. T3 Code's settlement detaches idle sessions and closes terminals idle at a prompt,
  and its #15146 shows the cost of never freeing what finished work holds.
- A task merged or given up (`Failed`, whoever said it) whose agent is not at work counts
  against the fleet's live agents. Its
  terminal stays open and its assignment stays, so the board still links it and the person can
  look back or carry on. An agent that works again counts again, so an agent that gives up its
  own task and goes on working frees nothing.
- Once that agent has rested `SETTLE_AFTER` (10 min) at its prompt, with no request open on
  its threads, no background work or scheduled prompt, and its tile on no client's screen, the
  server closes its terminal through the worker's `Close` (`Hub::settle_finished`, looked at
  every 30 s). The wait starts again whenever any of that stops holding. The agent's session
  stays, so it can be taken up again, as "Stop its agent" relies on. The timeline says why in
  a note before the terminal's end says it is gone. Only a terminal the server started for the
  task is closed: one the person put on a task is theirs.
- Freeing the task's worktree came in the next change ("A merged task frees its worktree"
  below).
- Tests: `a_finished_task_s_agent_counts_only_while_it_works` (`slopty-server::project`) and
  `a_finished_task_s_agent_stops_counting_and_is_closed_once_it_rests` (`slopty-server`).

**The orchestrator tells its tasks.** ✅ 2026-10-03 (reverses part of 2026-10-02), narrowed
2026-10-04
- Before: `TaskTell` was the person's alone. An orchestrator could redirect a task only by
  starting it again, so it could not carry what it found to the same agent, or say "also
  cover X" while the task worked.
- Prior art: T3 Code's `t3_thread_send` (auto, queue, steer or restart, its provenance kept
  as the agent's and the MCP's), Codex's `followup` ("trigger a turn if it is idle … deliver
  at message boundaries"), Amp's Agent to Agent, Vibe Kanban's `run_session_prompt` and
  Nimbalyst's `send_prompt` all let an orchestrator speak to its children.
- Now the orchestrator may `TaskTell` any task of its own project (`agent_scope`). A task's
  agent tells none (that is `task_report`'s direction), nor does an agent of another project,
  and an agent's surface that proves no terminal tells nothing. The MCP tool is `task_tell`;
  the CLI's `slopty task tell` does the same inside the orchestrator's session.
- Why it is safe:
  - it goes through the reports' delivery, the agent's own hooks and inbox, so nothing is
    typed into a terminal, and the inbox post keeps to `may_type`, so it never lands on the
    person's draft or prompt;
  - the agent reads it under "Your orchestrator says (an agent, not the person; it answers
    nothing the person is asked):", never under the person's tag;
  - it never answers for the person: a task waiting on the person (a permission, a question)
    is refused until it moves on, and the words say they answer nothing the person was asked;
  - the person's words keep priority: they come first in a batch, and the orchestrator's tell
    never replaces them;
  - it is paced as a report: one waits per task, the latest replacing the one still unread,
    so a loop of tells costs the task one turn at a time.
- The timeline says who told: a note "The orchestrator told it: …", apart from the person's
  `Told`. A `Told { by }` would need a wire change, so the note stands until one lands with
  other proto work.
- The orchestrator's role says that `task_tell` says more to a task's agent.
- Tests: `the_orchestrator_tells_only_a_task_that_does_not_wait_on_the_person`
  (`slopty-server::project`), `the_orchestrator_speaks_after_the_person_and_never_in_their_place`
  (`slopty-server::deliver`), `only_the_orchestrator_tells_a_task_in_its_own_words`
  (`slopty-server`, through a worker's link) and `task_tell_names_its_task_and_carries_the_words`
  (`slopty-tools`).

**An orchestrator without hooks waits for its tasks' news.** ✅ 2026-10-03
- Before: a Claude Code orchestrator is woken by its hooks and inbox. A Codex orchestrator has
  no such door, nor will a pi or ACP one, so each polled `project_status` and worked out from
  the timeline what its tasks did.
- Prior art: T3 Code's `t3_thread_wait` (a timeout "does not cancel the child"), Codex's `wait`
  ("Returns empty status when timed out"), and Conductor's warning to wait for working before
  trusting idle.
- `task_wait { tasks, until: any | all, since, timeout_ms }` (MCP, and `slopty task wait`) is
  built in `slopty-tools` over the `ProjectStatus` long wait, so it costs a read per change of
  the project, never a poll, and needs no new verb. News of a task is a report, a move of its
  state, its terminal gone, its verifier or checks, or a step that ended. A delivery,
  a note or a tell is not.
- A turn that ended with no report is news because the server now writes it on the timeline,
  as the task's move from running to waiting, once per such turn (`Projects::rested`, from the
  outcomes above). A turn that reported stays quiet there, as before. A wait on the person is
  the block the timeline already kept.
- From now when no `since` is given: what came before is the past, of which only each task's
  latest report is kept, so a task started a moment ago and still idle is not mistaken for
  one that finished. A task merged or failed when the wait begins is ready at once, since no
  news may come of it.
- It answers each task's card with its `news` and `last_report`, which are `ready`,
  `timed_out`, and `next` to wait on from, so nothing between two waits is missed. Running out
  of time stops and cancels nothing. The default is 50 s, under the minute many MCP clients
  give a call, with progress every 10 s; it waits at most 30 min, a read at a time of up to
  the server's 240 s.
- The agent's last words are not in the answer yet: they need the thread's final message on
  the wire (`ThreadRow::last_said`). Until then `read_thread` on the task reads
  them, and a hooked orchestrator gets them in the outcome notice.
- Tests: `task_wait_waits_for_news_and_cancels_nothing` (`slopty-tools`, over a scripted
  timeline on a paused clock) and the rest's timeline mark in
  `a_task_s_outcome_reaches_the_orchestrator_without_a_report` (`slopty-server`).

**A merged task frees its worktree.** ✅ 2026-10-04
- Before: a writing task's agent works in a worktree of its own, under the clone's
  `.claude/worktrees/`, and nothing ever removed one. A long project filled the worker's disk
  with checkouts (and their `target/` directories) of work already merged.
- Prior art: T3 Code issue #15146 (worktrees never freed), Conductor's archive script, and Vibe
  Kanban's `worktree_deleted` per workspace.
- The worker's `RemoveWorktree { worktree, landed }` removes a worktree only when all of these
  hold:
  - it is a linked worktree directly under its clone's `.claude/worktrees/`, so neither the
    clone itself, the person's own worktrees nor any other path qualifies (`Invalid`);
  - no live terminal on the worker has its directory in it (`Conflict`);
  - `git status --porcelain` lists nothing, so nothing uncommitted or untracked is lost
    (`Conflict`, naming what git listed).
  It removes with `git worktree remove` and no `--force`, so git refuses whatever this missed.
- The branch the worktree had checked out goes too only when every commit on it is in one of
  `landed` by patch, as `git cherry` reads it. The merge queue rebases, so the commits on the
  target are not the branch's own and ancestry would never call them landed. `landed` is the
  merge's head, the target and the target on `origin`; those that name nothing on the worker
  are passed over. A branch with work that did not land is kept, and the timeline says so.
- The server asks once settling has closed a merged task's agent ("A finished task's agent
  stops counting" above), after the worker answers that close, so the agent's own terminal is
  gone. The card drops the worktree that went, and the timeline says what went and what was
  kept. A worktree the worker keeps stays on the card with the reason on the timeline.
- Only a merged task's worktree is freed. A task given up may be tried again, and Claude
  Code's `--worktree` remakes a missing worktree with `git worktree add -B`, which would reset
  the kept branch to the base. A Codex task's worktree is not freed yet, since Codex makes and
  names it itself and no report names where.
- `RemoveWorktree` is the server's alone: a tool that asks is `Forbidden`.
- Tests: `a_clean_worktree_goes_and_its_branch_only_once_landed` and
  `a_worktree_in_use_or_not_committed_is_kept` (`slopty-worker`, on real git repositories),
  `a_merged_task_s_worktree_goes_once_its_agent_is_closed` (`slopty-server`), and the goldens
  `remove_worktree` and `worktree_removed`.

**Any agent runs a task, as a thread.** ✅ 2026-10-04 (server, wire and the worker's door; each
adapter's tool wiring follows in the agents lane)
- Before: a task ran Claude Code or Codex in a terminal, or a command. pi and ACP agents run
  with no terminal, as threads of the worker's thread host, so they could start as threads
  but never run a task with Slopty's tools, a role and a count. The whole project model knew
  an agent only by the terminal it ran in.
- Prior art: T3 Code's `delegate_task`, which targets any provider instance and model.
- `Runner::Agent { agent, prompt, model, args }` names any agent by the thread model's id.
  `task_start` and `slopty task start --agent <name>` take `claude` (the default) and `codex`,
  which keep their own runners in a terminal, and `pi`, `acp:<name>` or an ACP agent's bare
  registry name, which become `Runner::Agent`. `model` goes as Claude Code's and Codex's
  `--model`, and as the thread's model for the rest.
- The start holds it to a worker that has the agent installed, as it does Claude Code and
  Codex: built-in agents under the `agents` facts, ACP agents under `acp` by the registry's
  name. The server cannot judge pi's or an ACP agent's flags, so without the person's
  `permission_flags` such an agent takes no arguments, and the first is named in the refusal.
- The server chooses a seat, a session id, and asks the worker for `StartThread { start,
  seat, env, role, worktree }`. The worker answers `ThreadStarted { thread, worktree }`. The
  assignment keeps the seat as its `term` and the thread as `thread`.
- **The seat makes a thread look like a terminal to everything else.** Its tools speak as the
  seat, with the worker's token for it, so `Speaker::Proven(seat)` finds the task as a
  terminal's tools do. Deliveries go to the seat. Settling closes it with `Close`, which the
  worker turns into ending the thread's agent. Its row in the worker's thread table carries
  the fact `slopty.seat`, so the board finds the row by the seat. Its last words, its
  requests, its spend and its rung on the ladder all work as they do for a terminal's thread.
- Its liveness and state come from that row. A row whose agent is there counts as a live
  agent. Its phase moves the task as a hook's status does (working runs it, a request open
  blocks it on the person, a rest waits), which also feeds the outcome notices. A row gone,
  or one whose process exited, ends the assignment. That happens at once for a row the table
  showed before, and only after the 30 s start grace for one it never showed, so a table
  behind the start ends nothing. A worker re-registering no longer ends a thread's
  assignment for want of a terminal.
- A writing task beside a clone gets a worktree the worker makes, named and placed as Claude
  Code's `--worktree` would make it (`.claude/worktrees/slopty-<project>-<task>`, branch
  `worktree-…`, from `origin`'s default branch, else `HEAD`). Claude Code's resets a branch of
  that name to the base; the worker's checks out a branch already there as it is, so a task
  tried again keeps its work. The worktree comes back on the answer, so the card names it from
  the start, and settling frees it as any other.
- What the agents lane adds behind the worker's `TaskThreads` door: starting the thread with
  the seat's fact and environment, opening a terminal agent's thread under the seat, the role
  through each agent's door (Codex's `developerInstructions`, pi's `--append-system-prompt`,
  ahead of the first prompt for ACP), Slopty's tools (Codex's MCP config, ACP's
  `session/new` `mcpServers`, a pi extension over `slopty mcp`), deliveries to a seat as a
  queued message, and `close` by the seat.
- Tests: `any_agent_runs_a_task_as_a_thread` (`slopty-server`), the goldens `task_spawn_agent`,
  `start_thread`, `thread_started` and `assignment_thread`, and
  `a_worktree_is_made_as_claude_code_would_and_a_branch_there_is_kept` (`slopty-worker`).

**A step a restart left under way is taken up again.** ✅ 2026-10-04
- Before: a step running when the server stopped was marked failed as the store loaded. A
  verifier still running on its worker was then run again from the start, and a branch on its
  way home or a clone stopped half done.
- On load the step stays running, its phase "Taken up again once its worker is back", with
  when it began and the commits it works on. The step as it stood is kept in memory
  (`Projects::restarted`). When its worker registers, the server takes it up
  (`Hub::resume_steps`), each step once. A step the task has moved on from in the meantime
  leaves nothing to take up.
  - **Clone:** made again. A worker asked again answers from the clone it already has, so
    this is safe to repeat. The task start that asked for it had already failed with the
    server, so the next start finds the clone.
  - **Bringing a branch home:** sent again. It only sets names the server alone uses.
  - **Verify:** a verifier whose terminal still runs is followed to its verdict, which is
    judged like any other. It is not run a second time, and the timeline has one start. One
    whose terminal is gone is run again by the lane, which closes the old terminal first.
  - **Merge and rebase:** the lane runs them again from the queue. They start from the
    target as it is now, as after any failure, so repeating them does no harm.
- `TaskStep.commits` (head and base) is what makes a verifier resumable. It is
  stored as soon as it arrives, although the progress between a step's start and end is not.
- Rejected: **failing every step on load.** That runs a long verifier twice.
- Tests: `a_step_is_shown_as_it_goes_and_is_taken_up_after_a_restart`,
  `the_queue_is_its_tasks_in_the_order_they_joined_and_outlives_a_restart` and
  `a_verifier_left_running_by_a_restart_is_followed_to_its_verdict` (`slopty-server`).

## Phases

1. **Wiring and state.** Built 2026-09-30, except the tile.
   - Spawned agents get `slopty mcp` through `--mcp-config` and `SLOPTY_SERVER`,
     `SLOPTY_PROJECT` and `SLOPTY_TASK` in their environment.
   - The project and task store on the server, with verbs in `slopty-proto::orchestration`
     served through `slopty-tools`.
   - `SubagentStart`/`SubagentStop`/`TaskCreated`/`TaskCompleted` are forwarded, and so is
     `AgentBranch`.
   - A first project tile shows the tasks and opens them.
2. **Code across machines.**
   - Clones from the forge on each worker and branches carried home as bundles (the plan's
     `git-remote-slopty` and server-side bare repositories were dropped on 2026-10-01).
   - A worktree setup file (A3).
   - Linux worker hardening: systemd, x86_64 and a real network e2e.
3. **Verify and merge.** The verifier and the merge queue built 2026-10-01.
   - The verifier runs per task, and the merge queue on the server.
   - The timeline, and tokens per agent.
4. **Scale.** The shared build cache, channel push and moving agents between workers, each only
   after it is measured.

**A failed push after a merge is said, not folded into the merge.** ✅ 2026-10-04 (readiness
N16)
- A merge whose push to `origin` failed left the target moved on its clone and read as a merge
  like any other ("Merged: …, not pushed").
- `Merge::Merged::push_failed` carries git's words. The orchestrator is told the push failed
  and that the person pushes again. The task's row says "push failed: <git's first line>", and
  its pipeline shows a Push stage that holds (the stronger ink, as every holding stage), which
  the row then leaves to it.
- "Push again" on the card and in the palette sends `Verb::TaskPush`, the person's alone. The
  server pushes the target as the orchestrator's clone has it now (`FastForward` from and to
  `refs/heads/<target>`, which moves nothing): what the queue merged since sits on top of the
  task's work, and a push of the older head alone would be refused as behind. The card's merge
  keeps its head and takes the new push's outcome, with a Merge step on the timeline. A merge
  already pushed is answered as it is, so a resent word asks nothing of the worker.
- Tests: `a_merge_whose_push_failed_says_so` (`slopty-ui::project`),
  `a_merge_whose_push_failed_is_pushed_again_on_the_person_s_word` (`slopty-server`),
  `the_boards_actions_reach_the_server` (`slopty-ui::workspace`), and the goldens
  `task_merged_unpushed_card` and `task_push`.

**Checks that cannot be read say why, and never hide a reading.** ✅ 2026-10-04 (readiness #16)
- A worker with no `gh` or `glab`, or one not signed in, used to leave a pull request whose
  checks never came, with nothing said.
- `ChecksState::Unknown` with `Checks::why` (the worker's words, at most `CHECKS_WHY_MAX`)
  is what the server keeps when the worker answers `PullChecks` with `Unsupported` or `Failed`.
  It is asked again at the failed pace. The row says "checks unknown: <why>", holding nothing
  and offering no Fix CI, and the timeline says it once.
- A reading the card already has stands over a later failure to read: the last known state is
  still the most that is known, and a passing card flickering to unknown and back says nothing.
- Tests: `checks_that_cannot_be_read_say_why_and_never_hide_a_reading` (`slopty-server`),
  `checks_that_could_not_be_read_say_why_and_ask_nothing` (`slopty-ui::project`), and the
  goldens `outcome_checks` and `moment_checks`.

**An orchestrator is an agent, named from the GUI; a project is made from a sheet; a task is
stopped or cancelled from its card.** ✅ 2026-10-04 (readiness #16, N17)
- Only the CLI could name an orchestrator, any shell could be made one, and nothing a board
  told a shell was ever heard. Cancel and stop were CLI-only.
- "Start a project here" opens a "New project" sheet over the workspace: the name, repository
  and target filled from the terminal, a verifier, and whether a merge is pushed, with the
  orchestrator (the terminal's agent and its machine) named but not asked. Create sends
  `ProjectCreate`; the board opens once the server's word has that terminal as orchestrator.
- "Make this agent X's orchestrator" is a palette line for each project the focused agent
  does not orchestrate. It sends `ProjectSet` with the terminal, and the board turns to that
  tile once the mirror says so (`opening` waits on the orchestrator, not just the project).
- Both refuse a terminal with no agent at work and say why. The refusal is the client's: the
  server keeps taking a live terminal, since the CLI, the tests and the merge lane name a
  shell by its repository, and an agent may be started in it later.
- The task the board stands on offers "Stop its agent" (`Close` on its open assignment's
  terminal, whose session the agent can take up again) and "Cancel task" (`TaskUpdate` to
  `Failed` with the person's note), quieter and after what the task waits for. Both are on
  the palette too. Elsewhere the board stays quiet.
- Tests: `a_project_starts_in_the_focused_terminal`, `an_agent_becomes_a_project_s_orchestrator`
  and `a_task_stood_on_can_be_stopped_or_cancelled` (`slopty-ui::workspace`).

- ✅ **What holds a task up is said as it lands** (2026-10-04, readiness N16). A failed merge
  or a red verifier showed only on the board, so it was seen only once someone looked.
  - Moments that hold a task up are news: failing checks with their names, a verifier that
    failed, and a step that stopped with why. A step done, a merge and a task's progress are
    not.
  - With the app in front, the news is a toast "Title: #3 fix: its verifier failed", unless
    that project's board is already in the focused tile. With the app away, it is a
    notification that opens on the orchestrator, and it is withdrawn with the rest.
  - Tests: `only_what_holds_a_task_up_is_news` (`project/tests.rs`),
    `a_project_s_failure_is_said_as_it_lands` (`tests/projects.rs`). (Moved to the server on
    2026-10-05: "A project's held-up work is the server's notice".)

**Any agent's thread is read and answered alike.** ✅ 2026-10-04 (R10; the worker's thread
door is wired in the agents lane)
- Before: `read_conversation` read Claude Code's transcripts through the conversation face, by
  terminal, and `answer_permission` answered its held prompts by number. It could not read a
  pi or ACP thread, a Codex thread without a TUI, or a task started as a thread (R7).
- Prior art: T3 Code's `t3_thread_read`, with a messages view and an activity view, bounded
  text with a truncation flag, and a position cursor.
- `ReadThread { of, view, after, hold }` reads through the thread model, whatever the agent.
  `of` names a task (its assignment's thread, or the one seated in its terminal), a terminal,
  or a thread's id, a subagent's too. The server finds the worker that holds it and sends
  `ThreadOf::On`.
- The answer is whole turns after `after`, a `TurnId`. The `messages` view gives the person's
  messages and the agent's answers. The `activity` view adds each tool call (its kind, title,
  state, the last 1000 characters of its output, and the child thread it started) and the
  agent's notices. Reasoning is never given.
- A read is bounded so a model can take it in at once. A message is cut to its first 4000
  characters and an output to its last 1000, each marked. The read stops at the first whole
  turn past 48 000 characters, though the first turn always goes. `truncated` says something
  was left out. `next` is the last whole turn given, so a turn under way is read again until
  it ends, and reading on from `next` misses nothing. `skipped` says turns after `after` are no
  longer held.
- The open requests come with the read, each with the choices it offers. The person answers
  one with `AnswerRequest { of, ask, choice, message }` (`slopty agent answer`), done as a
  client's intent would be. There is no MCP tool for it, and the server refuses it from an
  agent. Only the person's read holds a Claude Code TUI's prompts, as before (`hold`, set by
  the server). The worker's daemon gives orchestration its threads through
  `orchestrate::ThreadReads`, and answers as `conversation::ORCHESTRATION` by the nil client.
- The old path is gone: `ReadConversation`, `AnswerPermission`, `ConversationPage`,
  `ThreadInfo`, `orchestrate::conversation::read_page` and `Conversations::answer`.
  `ReadThread` and `AnswerRequest` took their places in `Verb`, and `Outcome::Thread` took
  `Outcome::Conversation`'s, so no other golden moved.
- The MCP endpoint's tool list and count are checked against the tool table itself, so a tool
  added or removed cannot leave them stale.
- Tests: `orchestrate::thread_read` (`slopty-worker`: the two views, the cursor over a turn
  under way, the bounds and what they say, a skipped start and the open requests);
  `a_thread_is_read_by_task_thread_or_term_and_answering_is_no_tool` and
  `a_read_names_its_turns_and_what_waits` (`slopty-tools`);
  `any_agent_runs_a_task_as_a_thread` (its thread read by task, seat and id, holding for the
  person alone), `an_agent_never_takes_the_person_s_word_through_any_surface` and
  `the_agent_screen_and_upload_verbs_go_to_their_worker` (`slopty-server`); the goldens
  `read_thread_task`, `read_thread_on`, `thread_read` and `answer_request`; and the server e2e,
  where a played Claude Code session's thread is read over the CLI and its prompt is answered
  there.

**A thread's subagents are its task's natives, whatever the agent.** ✅ 2026-10-04 (R7
follow-up)
- Before: the tree's natives came only from Claude Code's hooks. A Codex, pi or ACP task's
  subagents were rows in its worker's thread table (`ThreadRow::parent`) and never reached
  the board.
- The hub reads each table it takes in (`Board::native_moves`). A row whose parent chain
  reaches a thread seated at a task's seat or terminal is a subagent of that node. It is
  reported as a hook would report it (`AgentReport::SubagentStarted` and `SubagentStopped`),
  so the store, the tree, the counts and the bound on natives all treat it as they treat
  Claude Code's.
- A subagent runs while its phase is working, waiting or needs-you. It stops once it rests,
  ends or exits, with its row's last line as what it answered. A row gone from the table has
  stopped. Its id is its thread's id, and its kind is its row's title (the agent's id when
  that is empty).
- A family whose root is Claude Code adds none, because Claude Code's hooks report its
  subagents already, under their own ids.
- Test: `a_thread_s_subagents_are_its_task_s_natives` (`slopty-server`).

**A task's thread put to sleep holds no place, but stays its task's.** ✅ 2026-10-04
- An agent put to sleep has ended, with its session kept so the next message wakes it. Its
  task's thread therefore no longer counts against the live limits (`Board::live_seats`
  skips it), and a subagent put to sleep has stopped as a native.
- It is not gone. Its row stays in the table, so its assignment holds and the task keeps
  its agent. Waking takes the place back.
- Tests: `an_asleep_thread_counts_as_no_live_agent_and_stays_on_its_task` and
  `a_thread_s_subagents_are_its_task_s_natives` (`slopty-server`).

**A thread put to sleep is no news, and stands below idle.** ✅ 2026-10-04
- The person puts an agent to sleep at rest. Its turn finished before that, and the Finished
  notice went then, so the ladder sends no notice when a thread goes to `Rung::Sleeping`.
  That holds even for a thread that slept straight from work (`moved` rules the arm
  explicitly). Sleep still ends the stretch the thread was busy for.
- A root's rung starts from its own rung and rises with its family's. A default once stood in
  for an empty family and lifted a sleeping root to idle, so a thread put to sleep from work
  read as finished. That default is gone.
- Test: `a_thread_put_to_sleep_is_no_news` (`hub::ladder`).

**The person commits, pushes and opens a pull request from any thread.** ✅ 2026-10-04
- Before: only a project's task had a way to its branch's end (`TaskPush`, the merge queue).
  A thread outside any project left its changes in the working tree, so the person went to a
  terminal to commit them.
- Prior art: T3 Code's thread-level commit dialog (`.research/t3code-ui-2026-10-03.md`,
  "Larger, unranked"). The person picks files, writes the message, and chooses commit, commit
  and push, or open a pull request.
- `slopty_proto::git` carries it to the worker code (`slopty_worker::repo::commit`). The
  app's commit sheet asks the worker straight, with `ClientMsg::Git { request, repo, op }`
  answered by `WorkerMsg::GitDone`. It rides the client's save queue, so a file saved and then
  committed is committed as saved. A status or a commit runs in that order. A push or a pull
  request waits on the network, so it runs beside later saves, after what came before.
- There is no `slopty git` (cut on 2026-10-04): a person or an agent at a shell already has git
  and gh, and no top-tier tool ships a git wrapper of its own.
- `GitOp` is `Status`, `Commit { paths, message }`, `Push` and `PullRequest { title, body,
  base, draft }`.
  - A status is `git status --porcelain=v2 --branch -z`. Each file keeps git's own two letters
    (`GitFile::xy`), so no state is invented beyond git's. The branch, upstream, ahead and
    behind come with it, and at most `FILES_MAX` files, with the rest counted.
  - A commit takes exactly the chosen paths. It runs `git add --all -- <paths>`, then
    `git commit --file=- --only -- <paths>` with the message on stdin, so anything else staged
    stays staged. A path new to git is added, and a path gone is removed.
  - A push goes to the upstream. With none, it sets one on the repository's only remote, or on
    `origin` among several. With several remotes and no `origin`, it refuses and says which
    remotes there are.
  - A pull request is `gh pr create`. An empty title becomes `--fill`, so gh writes it from the
    commits, never Slopty. The URL is the one gh prints.
- **The person's binaries, their words.** git is found as everywhere else (`changes::git`).
  gh is found on `PATH` or where Homebrew and the system put it. Each runs with the person's
  config, hooks and credential helpers as they are. `GIT_TERMINAL_PROMPT=0`,
  `GH_PROMPT_DISABLED=1` and `GIT_EDITOR=true` make whatever would prompt fail instead. Slopty
  reads no credential.
  - A refusal comes back as `GitOutcome::Failed { said }`: the end of git's or gh's own words,
    at most `SAID_MAX`, such as a rejected push or a hook that failed.
  - What Slopty itself refuses is `Refused { why }`: no files chosen, an empty message,
    a path outside the repository, a folder in no repository, no remote, or a detached HEAD.
  - A missing program is `Unavailable { program, why }`. With no gh, the answer says so and
    opens nothing.
  - No message is ever made up. An empty one is refused, since a commit takes the person's.
- **Not an agent's.** An agent commits, pushes and opens pull requests with its own git and
  gh in its own terminal. The sheet is the person's, in the app.
- The sheet is drawn in the thread and review tiles (`conversation/thread/commit.rs`).
- Tests:
  - `repo::commit` (`slopty-worker`): git's records parsed; the chosen files committed with
    the message, other staged files left staged, a push that sets the upstream and then one
    that does not; and refusals in git's words, from a failing hook to a rejected push.
  - `a_file_saved_then_committed_is_committed_as_saved` (`slopty-workerd` e2e, the direct
    route).
  - The goldens in `golden_git`.

**A branch's pull request reads where it stands, and merges on the person's word.** ✅ 2026-10-04
- Before: Slopty opened a pull request for any thread and read a task's checks onto its
  card, but the person could not see whether a thread's pull request was reviewed, blocked or
  failing, and merged it in a browser.
- Prior art: T3 Code shows a thread's pull request with its checks and merges it from the
  thread, through gh.
- `GitOp::PullStatus` reads the branch checked out with `gh pr view --json …`, which asks for
  the state, draft, head and base, head commit, review decision, mergeability, merge state and
  check rollup. The answer is `GitDone::PullStatus(Option<PullStatus>)`, with none for a
  branch that has no pull request. That is gh's "no pull requests found", which is read as an
  answer, not a failure.
- **Open data.** Each fact keeps GitHub's own spelling as a string (`state`, `review`,
  `mergeable`, `merge_state`, and each `PullCheck::state`, the conclusion once a check ends,
  else its status). A state GitHub adds later is carried, not refused.
  - Only the ranking is closed: `PullCheck::bucket` (passed, skipped, running, failed) and
    `PullStatus::standing`.
  - Standing goes merged or closed first, then failing, conflicting, changes requested,
    draft, checks running, ready, and waiting.
  - A check state not known here reads as running, never as passed.
- A check is a CI job (`CheckRun`: name, workflow, conclusion or status, and its page) or a
  commit status another service set (`StatusContext`: context, state, target). Both arrive as
  a `PullCheck`, at most `CHECKS_MAX` (200), with the rest counted.
- **Refreshed on demand and after a push.** A push reads the pull request again
  (`GitDone::Pushed::pull`), so the sheet shows the new checks as started. If gh is missing or
  fails there, the push still stands and `pull` is none. A refresh is otherwise the person's
  ask; nothing polls.
- **The merge is the person's.** `GitOp::Merge { method, head, delete_branch }` runs
  `gh pr merge --<method>` with gh's own methods, merge, squash or rebase.
  - Any other method is refused before gh runs.
  - `head` passes `--match-head-commit`, so gh merges only the commit the person looked at.
    A push after they looked makes gh refuse, in its words.
  - The answer carries what gh said and the pull request read again.
  - The whole sheet is the person's, merge included: an agent that may merge does so with its
    own gh.
- gh runs as everywhere else, found on `PATH` or where Homebrew and the system put it
  (`repo::checks::find`), with prompts off. `repo::commit::Programs` names the git and gh an
  op runs, so tests hand it a stand-in gh and never reach a person's sign-in.
- Not GitLab yet: gh speaks for GitHub alone, and on a GitLab remote it refuses in its own
  words. A task's merge request checks stay as they were (`Verb::PullChecks`, `glab`).
- The thread and review tiles show the pull request where it stands (`conversation/thread/git.rs`,
  `review/view.rs`).
- Tests:
  - `repo::pull` (`slopty-worker`), with a stand-in gh: the forge's words read from jobs and
    commit statuses with their pages; no pull request reads as none, and no gh says so; a
    merge by the method named at the head seen, with branch deletion; a method gh lacks is
    refused before it runs; and gh's refusal comes back in its words.
  - `git::tests` (`slopty-proto`): buckets and standing.
  - The goldens `client_git_pull_status`, `client_git_merge`, `worker_git_pull_status`,
    `worker_git_no_pull` and `worker_git_merged`. `worker_git_pushed` changed: a push now
    carries the pull request.

**A project keeps the person's scripts.** ✅ 2026-10-04
- Before: the person typed the project's dev server, test run or build into a terminal they
  opened and moved into the right folder themselves, on each worker and each task's worktree.
- Prior art: T3 Code's project scripts, named commands per project run from the thread with
  one action.
- `Project.scripts` holds up to `SCRIPTS_MAX` (32) scripts. Each `Script { name, command, dir }`
  has:
  - a name of letters, digits, `-`, `_` and `.`, unique in its project;
  - a command line of at most 4 KiB, as the person would type it;
  - an optional folder under the project's, relative and never climbing out
    (`Script::refusal`).
  The verbs are `ScriptSet` (in place of one of the same name), `ScriptDelete` and
  `ScriptRun`. Setting and taking away a script are timeline notes. The scripts ride the
  project, so every client and `project_status` have them, and the store keeps them.
- **Where it runs.** `ScriptRun { project, name, worker, task }`:
  - with a task, in that task's worktree on its worker. A task with no worktree yet, or a
    worker named that is not the task's, is refused;
  - else in the project's clone on the worker named, or on the orchestrator's worker when
    none is named. The clone is the one a shell there or the server's own clone placed
    (`clone_on`), else the project's own path on the orchestrator's worker. A worker with no
    clone is refused, pointing at one that has one or at a task's worktree.
  - The script's `dir` goes under that folder.
- **An ordinary shell the person owns.** The server sends the worker `RunScript`, and the
  worker opens a terminal tile titled "name · project".
  - It runs the line through the person's login shell, interactive (`$SHELL -l -i -c`), as
    verifiers do, so their `PATH`, toolchains and aliases apply.
  - When the line ends, however it ends (Ctrl-C on a dev server included), that shell takes
    the terminal over in the same folder (`exec $SHELL -l`). The person goes on from there and
    closes it when done.
  - Nothing is typed into a shell. The command is the terminal's own program, so nothing can
    race a prompt or land in the wrong place.
  - A `RunScript` sent by a caller rather than the server is refused.
- **The person's alone; agents read them.** Agents see the scripts in `project_status` (JSON
  `scripts`, and a line each in the text). The server refuses setting, taking away and
  running a script to an agent, and there is no MCP tool.
  - An agent runs its own commands in its own terminal anyway, so a script gains it nothing.
    Its output would land in a tile the person owns, which the agent does not read.
  - A script opens a terminal on a machine of the person's choosing, which is a person's
    decision.
  - An agent that wants the command reads it from the status and runs it itself.
- CLI: `slopty project script set|rm|ls|run`. `run` takes `--worker` and `--task` and prints
  the TERM.
- The run action in the board, the palette and a project's tiles is still to come.
- Tests:
  - `hub::projects::scripts` (`slopty-server`): kept by name and sorted, refusals,
    the most kept, taken away with a timeline note, an agent reading them in the status but
    refused setting and running; run in the orchestrator's folder under its `dir`, in a
    task's worktree on its worker, and the refusals for no worker, an unknown script, a task
    with no worktree and another worker.
  - `repo::script` (`slopty-worker`): the login-shell command line and the shell taking over.
  - `a_script_set_from_the_cli_runs_in_the_project_s_clone` (CLI e2e, a real server and
    worker): set, listed, run in the clone's folder through the login shell, its terminal
    listed, then taken away.
  - The goldens `script_set`, `script_delete`, `script_run` and `run_script`, with a script
    on the golden project. The project snapshots changed.

- ✅ **A project's held-up work is the server's notice** (2026-10-05, readiness G8). Each
  client read the news from its own mirror of the boards (`news_line`) and posted its own note.
  Every device the person had said the same thing, and the moments the mirror never saw went
  unsaid: a push that failed after a merge, and a failure held for the person once the task's
  give-backs were spent.
  - **The wire.** A `Notice` is about a `Subject`: a thread, as before, or a project at one
    timeline entry (`Subject::Project { project, entry }`), with `NoticeKind::Project`. Its
    tile is a `TermRef`, since a project's orchestrator may run on another worker.
  - **The server says it** (`hub::ladder::tell_project`, from `projects_moved`, which every
    project change goes through). The moments are failing checks with their names, a failed
    verifier, a step that failed with its first line (a rebase that conflicts reads "its work
    conflicts with main"), and a merge whose push to origin failed. Once the task's
    give-backs are spent, the line adds that it waits on the person. A notice is routed like
    a thread's: nowhere while the orchestrator's tile is on screen where the person is, else
    to the desk they are at, else to the handheld they hold, else everywhere.
  - **The client shows it.** With the app in front it is a notice in the workspace ("Title:
    #3 fix: its verifier failed"). Away, it is a note of its own per timeline entry
    (`project-<project>-<entry>`), stacked under its project and opening the orchestrator,
    and it is taken back with the rest when the app comes back. The client-side path
    (`news_line`, `ProjectNote`, `WorkspaceEvent::ProjectNews`) is deleted.
  - Tests: server `a_project_s_held_up_work_is_a_notice_about_the_project` (a pass says
    nothing, a failure notices with the task's name, nothing while the orchestrator is on
    screen, and the give-back past the cap waits on the person) and
    `held_up_work_is_said_by_what_held_it`; ui
    `a_project_s_notice_leads_to_its_orchestrator_stacked_by_project`; golden
    `attention_notice_project`.

- ✅ **Labels and probes are gone** (2026-10-05, readiness deletions). `[worker.labels]` and
  `[worker.probes]` existed for the placement rules, which were cut (`fc98a475`). Since then the
  labels only showed in the orchestrator's overview, and the probes ran the person's shell
  commands every 10 minutes for nobody to read.
  - An orchestrator that needs to know something of a machine runs the command there itself,
    in a terminal, and reads the answer when it matters rather than up to 10 minutes stale.
  - A worker's facts are now what it finds: its agents, ACP agents, toolchains, Rust targets,
    GPUs and power. The `Fact` map stays open, so nothing on the wire changes.
  - A file that still has either table loads, with an unknown-key warning for each.
  - Tests: `slopty_settings` `worker_labels_and_probes_are_unknown`, `slopty-worker`
    `this_mac_reports_its_toolchains`, `server_link` `the_server_hears_the_workers_facts`, and
    `slopty-cli` `a_worker_s_own_facts_are_listed_and_a_command_task_runs_where_it_says`.

- ✅ **The board reads in one direction; its bar says only what merged; its message is a
  thread's** (2026-10-05, design critique `.research/design-critique-astra-2026-10-05.md` #02,
  #11, #12, #13; readiness 10-06 #17).
  - **Lanes keep their places.** Stacking short lanes down a column balanced the columns'
    heights, but a lane's place moved as its neighbours grew, and the reader had to find
    whether the next state was below or across. Now the lanes stand in their order, left to
    right and then down, as many across as fit at 280 pt (`LANE_W`), and a lane keeps its cell
    however tall the others grow. Under two lanes' width they are sections down one column.
    `stack_lanes` and the line count it balanced by (`card_lines`) are deleted.
  - **A card reads as a task.** It is inset 12 pt. Its title runs up to two lines at the task's
    size (14) and the medium weight, with the mark and the number on its first line. The facts
    stand 8 pt below in the secondary ink: the second line, where it runs, its way to the
    target, its check, its buttons. The way to the target is one line of plain words parted by
    the quiet dot, with the stage that holds the merge in the text ink. They are no longer
    bordered chips, because none of them is a control. A failed verifier's last lines are
    mono at the facts' size on a reading line, not at the caption's.
  - **The bar is the merged share.** Every task used to be a segment in its lane's tone, so a
    full bar could sit over "0 of 5 merged". Now the bar is the shared progress bar
    (`kit::progress::Bar`): the merged tasks' share of them all in the success fill, an empty
    track before any has merged, and "N of M merged" to a screen reader. How the rest stands
    is the lanes' counts.
  - **The message to the orchestrator is a thread's.** The board's flat field became the frame
    a thread's composer has (`kit::message::shell`): the resting elevation inside one
    hairline, the floating radius, the prose's size, and the edge in the accent while the
    keyboard is in it. It grows to six lines, sends on ↵ or its own control
    (`kit::message::send_control`, a control's side, a touch target's on a phone), and takes a
    new line on ⇧↵. It says "Message the orchestrator…", as the palette now does.
  - The header's "N live" says what is live: "3 agents running".
  - Tests: `the_bar_fills_only_with_what_merged`,
    `the_message_to_the_orchestrator_sends_from_its_control`, and
    `as_many_lanes_stand_across_as_fit_at_the_zoom` (280 pt). The board tests click through a
    `reveal` that scrolls a card into the body first, as a person would, because cards are
    taller now.
