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

**One cap is a setting, under the person's bounds.** ✅ 2026-09-30, narrowed 2026-10-04 and
2026-10-10
- The person's `[server.projects]` bounds in `settings.toml` hold `live_agents`, the most live
  agents across the fleet (24). `permission_flags` went on 2026-10-10: how far a project's
  agents go is its autonomy ("A project's autonomy is how far its agents go", below). The
  server reads the file at start and as it changes (`Hub::set_bounds`). Agents read the bounds and the live counts in `project_status` and cannot
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
- No agent's start may name them, in any project (`hub::projects::allowance`, since
  2026-10-10; before, the projects in `[server.projects] permission_flags` could). How far a
  project's agents go is its autonomy, which the server pins itself. The person's own start
  names what it likes. No agent can start another with more than it has.
- An agent's environment for a new terminal cannot steer what runs there: `PATH`, `HOME`,
  `ZDOTDIR`, `SHELL`, `BASH_ENV`, `ENV`, `XDG_CONFIG_HOME` and anything starting `SLOPTY_`,
  `CLAUDE`, `ANTHROPIC_`, `NODE_`, `BUN_`, `DYLD_`, `LD_` or `GIT_CONFIG` are refused with
  `Limit`. The worker applies the request's environment first and its own last, so its hooks,
  mod and session id always win.
- Flags are one door. A settings file in the repository, which an agent may have written, is
  another, and keys typed into a TUI a third. So an agent the server starts also:
  - begins in `--permission-mode default` when its arguments name no mode;
  - has bypass mode locked off (`disableBypassPermissionsMode: "disable"` in the settings the
    worker adds, `slopty_agent::hooks::held_to_asking`).
- Two backstops watch what actually runs, on every terminal the server started or an agent
  opened or typed into:
  - The mode each hook reports: a mode looser than those closes the terminal, with a note on its
    task saying why, unless its project's autonomy allows that mode.
  - The worker judges the argv of the agent in each terminal's foreground with the same
    allowlist and reports what loosens it (`AgentReport::Loosened`, at most 16 items of 256
    bytes, sent when it changes). So `claude` started inside a shell by any means, which no
    start check sees, is closed the same way. The worker knows its own `slopty` and mod paths,
    so it can accept its own hooks and MCP config by value; the server cannot and refuses them.
- An agent may not type into another agent's TUI (`Forbidden`), in any project: what reaches
  an agent from another goes through reports and hooks. Nothing may type into an agent that waits on the person (a permission, a question) or
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
  settled for 2 minutes, a later report of the task replacing it. *Since 2026-10-11 a report may
  also ask, and an agent's own report goes at once; only an unreported turn settles ("A task's
  questions go to its orchestrator, at once").* A batch is at most 9000 bytes, and each node has
  one batch outstanding.
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
  - the worker's facts reaching `slopty workers --json`, and a task made and started in one
    `slopty task start` on the worker it names, its agent run with its project and task in its
    env;
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

The reading of checks below is superseded by "One pull request watcher" (2026-10-09): a task's
card reads its thread's pull request, and `Hub::watch_checks`, `Verb::PullChecks` and
`Checks` are gone. The pipeline row stands.

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
- A Codex start (`task_start` with `agent: "codex"`, `slopty task start --agent codex`) opens the person's own `codex`, unmodified. The server gives it its role through
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
- A writing task beside a clone opens its terminal in a worktree the worker makes from the
  project's target (`OpenTerminal.worktree`, named for the task, branch `worktree-<name>`), so
  two Codex tasks never edit one checkout and the work starts where it is to land. Codex's own
  `--worktree` was dropped for it (2026-10-06): Codex 0.156 makes its managed worktree from
  the clone's `HEAD` (`codex-rs/tui/src/worktree_startup.rs` asks for no base), which is
  whatever branch the person left the orchestrator's clone on. A `--worktree` the person
  passes themselves is still Codex's, and the worker makes none then. Tests:
  `a_task_with_no_directory_goes_beside_a_clone_in_a_worktree_of_its_own` (`slopty-server`)
  and `a_terminal_opens_in_a_worktree_made_from_its_base` (`slopty-workerd`, on a real
  repository whose clone is on another branch).
- Its arguments are judged before it starts, as Claude Code's are: only what asks the person no
  less goes through on an agent's start. That is the model, images, a
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

**A project is a name; orchestration is a part it may have.** ✅ 2026-10-03 (its members were
deleted on 2026-10-10: grouping by repository already does what they did)
- The person works on a few projects spread over many machines and wants Slopty organised by
  them, with the machine as one fact among others (`.research/organization-2026-10-04.md`).
  Most of the projects a client groups by are derived and never stored: a repository's clones,
  a folder (`docs/decisions/ui.md`, "The navigator groups by project; the machine is a facet").
  A declared project on the server is how the person names one by hand.
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
- Tests: `slopty-proto` `a_fact_is_said_and_taken_back_within_its_bounds` and the goldens
  `client_item_set_fact`, `worker_item_pinned`, `table_snapshot`; `slopty-worker`
  `an_item_fact_is_kept_and_broadcast`, `a_thread_row_names_its_repository_once_known`.

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
  the kept branch to the base. A worktree the worker made for a task is on its card from the
  start: `OpenTerminal` and `SpawnAgent` that named one answer `Outcome::OpenedIn` with it, as
  `StartThread` answers with its thread's, so a Codex terminal's worktree, which Codex never
  reports, is freed once merged too (2026-10-06). A status line still updates the card after.
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
- A start names any agent by the thread model's id. `task_start` and `slopty task start
  --agent <name>` take `claude` (the default) and `codex`, which run in a terminal, and `pi`,
  `acp:<name>` or an ACP agent's bare registry name, which run as threads.
- The start holds it to a worker that has the agent installed, as it does Claude Code and
  Codex: built-in agents under the `agents` facts, ACP agents under `acp` by the registry's
  name. A start names no arguments (2026-10-10), so there are no flags to judge.
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
- Superseded 2026-10-09 by "One pull request watcher": a forge that cannot answer leaves the
  thread's last reading standing, on the worker.
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

**A task's thread put to sleep holds no place, and a thread put to sleep is no news.** ❌
2026-10-04. *Deleted the same day with sleep itself: see "Sleep, waits on another thread, queue
reordering and edited allows are gone" in `agents.md`.*

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
  - The branch is pushed first, as a push pushes it, and the pull request is opened only once
    it went up (2026-10-10, readiness 10-10 rank 4). gh opens one only for a branch the forge
    has, and a fresh agent worktree's branch is on no remote yet, so "Open pull request" beside
    "Push" failed until the person pushed by hand. A worker without the forge's command line
    pushes nothing. The thread says "Pushing and opening the pull request" meanwhile. Tests:
    `repo::pull::tests::a_pull_request_is_opened_once_its_branch_went_up` (a stand-in gh, a
    bare forge on disk, nothing pushed without gh) and
    `a_merge_request_is_opened_and_merged_with_glab` (the same through glab).
- **The person's binaries, their words.** git is found as everywhere else (`changes::git`).
  gh is found on `PATH` or where Homebrew and the system put it. Each runs with the person's
  config, hooks and credential helpers as they are. `GIT_TERMINAL_PROMPT=0`,
  `GH_PROMPT_DISABLED=1` and `GIT_EDITOR=true` make whatever would prompt fail instead. Slopty
  reads no credential.
  - Each runs on the person's `PATH`: the daemon's own, then their login shell's, read once for
    the worker's life (`facts::person_path`, 2026-10-12, readiness 10-12 rank 2). A worker
    launchd started has `/usr/bin:/bin` and little else, so a husky, lefthook or pre-commit
    hook, git-lfs or a signing helper installed by Homebrew, mise or npm failed under
    "Commit" while "Ask Claude to commit" worked, Claude Code being started on the login
    `PATH` already. The op is scoped to that `PATH` (`Programs::scope`), so gh's and glab's
    reads, the pull-request watcher and the merge queue's landing run on it too. Test:
    `repo::commit::tests::a_hook_s_tool_is_found_on_the_person_s_path`.
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
- Superseded 2026-10-09 by "The repository's run and archive scripts": a project no longer
  keeps scripts. `Project.scripts`, `Script`, the verbs `ScriptSet`, `ScriptDelete`,
  `ScriptRun` and `RunScript`, and `slopty project script` are gone. The scripts are read from
  the files the repository's other tools already keep, so the person writes them once.
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

- ✅ **The board is a grouped list** (2026-10-06, `.research/elegance-icons-2026-10-06.md` §5.2
  and §5.7; supersedes "Lanes keep their places" and "A card reads as a task" above). Lanes in
  a grid of bordered cards read as a dashboard of boxes. The board is now one column, as
  Linear's grouped issues are.
  - **A lane is a head over its rows.** The head is the lane's glyph, its name at 12/500 in the
    secondary ink (`kit::label`) and its count, muted. Lanes are parted by space alone
    (`spacing.lg`), with no rule and no frame. `lanes_across`, `LANE_W` and the board's width
    are gone. The tile hands the board only its zoom (`ProjectView::set_zoom`).
  - **A task is a row, not a card.** A row is 32 pt tall (a finger's row on touch), with no
    edge and no fill at rest, the hover wash under the pointer and the selection where the
    keyboard stands. It holds the mark, `#n` muted, and the title at 13/500. At the trailing
    end are its facts, the muted metadata role: what moves it on, its check (Verifying or
    Verified, with Output), its quiet stages, the branch with the git glyph, why it was pinned,
    the machine it runs on (its form's glyph), and its actions. Each row is a
    `kit::priority_row`. The title keeps a floor of 8 ems. The least needed fact leaves first
    (to-dos, the meta words, the branch, then the place), so a row fits a 312 pt column with no
    wrapping. Only the first action is a button with an edge: the solid when it frees a held
    task, the secondary otherwise (Merge). The rest, and the controls of the task stood on, are
    ghost words.
  - **What needs reading stands under the row**, from the number's edge: what the task's
    agent asks (its own word, else the status the task was left with), the stages that hold
    it or failed, a failed verifier's head with its last lines in an inset (`kit::inset`), and
    the "Run on" picker. Nothing, most of the time.
  - **The orchestrator waiting on the person leads *Needs you*.** Its own raised band over the
    lanes is gone, so "Needs you" is said once. Its question sits on the line under it.
  - **Merged folds to its head**, "Merged 1" with a disclosure, until the person opens it.
    It stays open while a merged task has something to do (a push that failed). The keyboard
    skips the rows of a folded lane.
  - **The board's title is the panel title** (16/600). Its lead glyph is gone: the tile's
    header already says what the tile is.
  - The only boxes left are the failure's inset and the first action's button.
  - Tests: `the_board_is_one_grouped_list` (one column as wide as the body, the lanes in
    order, the orchestrator leading Needs you with its question, a row 32 pt inside the tile,
    Merged folding and opening), the board tests that click rows, stages and checks by their
    selectors (unchanged: `project-card-<n>`, `project-lane-<lane>`), and the goldens
    `project-lanes`, `project-lanes-dark` and `project-live-lanes`.

- ✅ **A finished task is reviewed from its row before it merges** (2026-10-06,
  `.research/readiness-2026-10-07.md` R8). Merge was offered on a change the person had not been
  shown. The only way to it went through the task's agent tile and its thread's review, which
  fails once the agent has ended or its tile is on another client. That is the usual state of a
  task finished hours ago, or of one seen from the phone.
  - **"Review" leads a Ready to merge row**, with Merge second (`TaskAction::Review`, `v` on
    the board, "Review the task" in the palette). It opens the task's worktree as a folder's
    changes on its machine (`ItemKind::Changes { path, against: Some(target) }`), so the commit
    sheet and who wrote each line are there too. It shows the whole branch since it left the
    project's target (`Against::Branch`), which is what the merge brings. The scope bar can
    still turn it to what is not committed.
  - It reads the worktree, not the thread, so it opens whether or not the agent runs or has a
    tile here. A second "Review" goes to the tile already open.
  - Test: `workspace::tests::projects::a_finished_task_is_reviewed_from_the_board`.

- ✅ **A new worktree runs the repository's setup** (2026-10-06,
  `.research/worktree-setup-2026-10-06.md`, readiness R4's second half). A fresh worktree has
  the tracked files and what `.worktreeinclude` copies, but no `node_modules`, no generated
  code and no local database, so an agent's first minutes went on setting up what the person
  had already written a setup for, in another tool's file.
  - **Read, never a file of our own.** No setup file is shared between tools. The worktree's
    own checkout is read for the first of `setup::SOURCES` whose setup is not empty:
    `.conductor/settings.toml` (`[scripts] setup`), `conductor.json` (`scripts.setup`),
    `.codex/environments/environment.toml` (`[setup] script`), `.cursor/worktrees.json`
    (`setup-worktree-unix`, else `setup-worktree`: commands, or a script under `.cursor/`),
    `.superset/config.json` (`setup`), `.superset/setup.sh`, then `t3.json` (the scripts marked
    `runOnWorktreeCreate`). The Codex app writes an empty script by default, so empty never
    counts. Sources are never merged. A file that does not parse is passed over. Orca's
    `orca.yaml`, the rarest, is not read: it would take a YAML parser for one tool.
  - **How it runs.** After `.worktreeinclude`'s copies, in the worktree, with stdin closed:
    the person's login shell, interactive (their `PATH` and toolchains, as run scripts and
    verifiers), then `bash -e` (the shell those files are written for), so the first command
    that fails stops it. A list of commands is joined with `&&`. It is told its places as
    `SLOPTY_ROOT_PATH`, `_WORKSPACE_PATH`, `_WORKSPACE_NAME` and `_DEFAULT_BRANCH`, the same under
    `CONDUCTOR_` (which Orca copies), and its own tool's names (`ROOT_WORKTREE_PATH`,
    `SUPERSET_*`, `T3CODE_*`). It has no timeout, as none of those tools has one; its process
    group goes when the start is dropped.
  - **Once, and only in a worktree Slopty made.** A new worktree's git directory holds
    `slopty-setup-pending` until its setup succeeds or is passed over, so a worktree made
    another way, or one reopened after its setup, starts at once. A start sent again while the
    setup runs (after a reconnect) waits on the worktree's lock rather than running it twice.
  - **It holds the start.** Nothing starts in the worktree until the setup succeeds: a
    thread, a terminal opened in a worktree, a spawned agent, a task's thread. A failure keeps
    the worktree. A client's start answers `Outcome::SetupFailed { setup, code }` with where
    the setup came from and its last `Setup::TAIL` lines. While it runs, the client is sent
    `WorkerMsg::SettingUp` with the same, a few times a second at most. Starting again runs it
    again; `NewWorktree::setup` false starts without it, for good. An orchestrated start fails
    with the same words and lines, for the board and the orchestrator to read.
  - Tests: `repo::setup` (each tool's form and the order, the environment, output as a
    terminal last drew it, `bash -e` in the worktree stopping at its first failure), and
    `repo::worktrees::a_new_worktree_runs_its_setup_once_it_succeeds` (a failure kept and run
    again, a success not rerun, a pass-over for good). The goldens `outcome_setup_failed` and
    `link_worker_setting_up`.
  - **The start tile says it.** While the setup runs, the tile says "Setting up from
    conductor.json" under the working mark, with its newest line under that, in the terminal's
    face, muted, on one line, cut with an ellipsis. Each word from the worker replaces the last.
    A failed setup gives the draft back, as a refusal does. Over it the tile says "Setup from
    conductor.json failed" with the exit code when there is one, and the last lines in an
    inset, as a failed verifier's are. Two ways on follow. "Try again" sends the same start
    under a new intent to the same worktree, which the worker reopens and sets up again.
    "Start without setup" does the same with the setup off. A start with no draft (another
    run of the same message) keeps its tile for the same. Test:
    `workspace::tests::thread_start::a_start_tile_says_its_worktrees_setup_and_takes_it_again`.

- ✅ **A worktree can check out a pull request** (2026-10-06, readiness rank 18's wire). To
  review a pull request by its number, an agent needs its head in a worktree of its own, where
  gh reads it as that pull request and its review compares against the right base.
  - `NewWorktree::pull` names `origin`'s pull request. A new worktree starts from its head,
    fetched as `refs/pull/<n>/head`, in place of `base`. Its branch `worktree-<name>` tracks
    where gh looks, as `gh pr checkout` sets it. If one branch of `origin` is at the head,
    the pull request comes from that branch, and the worktree tracks `refs/heads/<branch>`, so
    a pull updates it (a push names the branch, `git push origin HEAD:<branch>`). Otherwise it
    is a fork's, and the worktree tracks `refs/pull/<n>/head`. A pull request `origin` does not
    have is refused as "origin has no pull request #n". Reopening a worktree reads no pull
    request, as it reads no base. The setup runs as it does in any new worktree.
  - `PullSeen::base` carries the branch a thread's pull request merges into, so that pull
    request's review reads its whole change against that branch rather than `origin`'s
    default.
  - Test: `repo::worktrees::a_worktree_of_a_pull_request_checks_out_its_head_and_tracks_it`
    (a fork's head and a branch's, one not there, a reopen). The thread table's goldens
    changed.

- ✅ **One pull request watcher** (2026-10-09, `.research/readiness-2026-10-08.md` R7). A
  task's pull request was read twice over: the server polled each task's checks through its
  worker (`Hub::watch_checks`, `Verb::PullChecks`, `repo::checks`), while the worker already
  watched every thread's pull request (`thread::pulls`). The server learned a task had one only
  from Claude Code's status line (`AgentBranch::pr`), so a Codex, pi or ACP task never showed
  its pull request or offered Fix CI for it.
  - *The card reads the thread.* `Task::pull` and `TaskCard::pull` are the `PullSeen` the
    task's thread row carries, found by the seat its thread runs at (its terminal, or
    `SEAT_FACT`): the hub takes each table frame and hands the rows' pull requests to
    `Projects::pulls_seen`, which cuts them to a card's bounds (`PULL_MAX_BYTES`). Among
    several threads at one seat, the latest to change speaks.
  - *The timeline.* `Moment::Pull` is logged when a pull request is first seen and each time
    its number or where it stands moves; its words alone, or its going away, change only the
    card. A move to a failed check, changes asked for or a conflict is the project's notice
    ("its pull request #42: lint failed"), and the recap says "Checks failed on". The thread's
    own notice stays off for a task's agent, so the person hears it once.
  - *The board.* Fix CI is offered while the task's agent runs and its open pull request has a
    failed check, and the words name the first one with the forge's own command to see them
    (`gh pr checks N`, `glab mr view N`). Address the comments follows `ChangesRequested`. The
    pipeline says "PR #42" or "MR !42", the failure or how many still run.
  - *Deleted*: `AgentBranch::pr`, `agent::PullRequest` and `Review`, the status line's `pr`
    reading, `Checks`, `ChecksState` and their bounds, `Moment::Checks`, `Verb::PullChecks`,
    `Outcome::Checks`, `repo::checks` (its `find` moved to `repo::commit`) and
    `Hub::watch_checks`. `Moment::Branch` names the branch alone. A forge that cannot answer
    leaves the thread's last reading standing, as the worker's watcher keeps it.
  - Tests: `hub::thread_tests::a_codex_task_s_card_shows_its_thread_s_failing_pull_request`,
    `project::tests::a_task_s_card_follows_its_thread_s_pull_request`,
    `hub::ladder::tests::a_resting_thread_s_pull_request_lifts_it` (the project tells of a
    task's failed check), `held_up_work_is_said_by_what_held_it`, and in `slopty-ui::project`
    `a_task_s_pipeline_says_each_stage_and_its_open_to_dos` and
    `running_checks_hold_nothing_and_a_merged_pull_request_asks_for_no_fix`. The project goldens
    changed.

- ✅ **A task starts fresh, or goes to another agent** (readiness R22, 2026-10-09). A task whose
  agent went in circles could only be stopped, or cancelled and planned again, which lost its
  place on the board. `Verb::TaskRestart { project, task, agent }` begins the work again with a
  new agent.
  - **What the server does.** It closes the task's agent if one runs (its terminal, or its
    thread's seat) and ends that assignment at once rather than waiting for the worker to say
    so. Then it starts the task again through the usual start (`TaskSpawn`, with its bounds,
    placement and permission checks). The new agent is `agent`, else the one the task ran last,
    read from its worker's thread table at the old seat. It is pinned to the worker the task ran
    on and starts in the folder the earlier agent worked in (its thread's, else the task's
    worktree), so the work so far is where it was. A task that never ran must name its agent.
  - **What the new agent is told.** Its first prompt is the task's brief, then one line: an
    agent worked on this before, its changes are in the worktree, and its thread is read with
    `slopty agent read --thread <id>`. Nothing of the old session is replayed, so "fresh" means
    a new context with the files kept. Throwing the work away is Cancel, not this.
  - **Who may.** The person, and the project's orchestrator for its own tasks. A task's own
    agent may not restart itself (the same scope rule as `TaskSpawn`).
  - **The board.** On the task it stands on, after "Stop its agent": "Start fresh", then "Give
    to another agent…", which opens a short list of the agents the task's machine can start
    (`WorkerSeen::agents`). Both show only on a task an agent has worked on and that is not
    merged. The CLI and MCP tools do not expose it yet; the orchestrator restarts a task by
    stopping and starting it.
  - Tests: `slopty-server` `hub::thread_tests::a_task_goes_to_another_agent_and_starts_fresh`
    (pi to Codex, then Codex afresh: closed first, same worker and folder, the brief and the
    pointer, a never-run task refused, a task's own agent forbidden); `slopty-ui`
    `project::tests::a_task_starts_fresh_or_goes_to_another_agent`; goldens `task_restart` and
    `task_restart_codex`.

- ✅ **An orchestrator's plan is confirmed and started** (2026-10-09). When a project's
  orchestrator puts its plan to the person, allowing it sets the project's tasks going, so the
  plan's plain allow reads "Confirm & start" in the tray and on the plan's card. The workspace
  tells a thread view that its terminal is an orchestrator (`ThreadView::set_orchestrates`,
  from the projects mirror). Any other agent's plan, and an orchestrator's approval of a
  command, keep the agent's own words. Test: `slopty-ui`
  `conversation::thread::tests::face::an_orchestrator_s_plan_is_confirmed_and_started`.

- ✅ **Work lands through a pull request when the target is protected** (readiness R23,
  2026-10-09). With "push after each merge" on, the queue moved the target in the
  orchestrator's clone and then pushed it. A forge that protects the branch refused the push,
  which left the clone's target ahead of the forge's, a merge only on paper, and "Push again"
  failing the same way each time.
  - **Detected by the push itself.** `Verb::FastForward` with `push` now pushes first, and
    moves the clone's branch only after. A push the forge refuses for protection, in its own
    words (GitHub's `GH006` and its rulesets' `GH013`, GitLab's "not allowed to push code to
    protected branches", "protected branch" from a hook), moves nothing and is answered
    `ErrorCode::Protected`. The rules are the forge's, applied to the person's own
    credentials, so an admin whose push is allowed still merges directly. Asking the forge's
    API instead would need rights a developer may not have, and could disagree with what the
    push meets. A push refused for any other reason keeps the old behaviour: the branch moves
    and the step says why. What a remote says (`remote:` lines) now leads a failed git's
    message, so the reason is kept.
  - **Landing.** On `Protected` the lane sends `Verb::LandPull`. The worker pushes the
    rebased, verified commit as the task's branch (forced, since it is the task's own work
    rebased), then finds the open pull request of that branch into the target or opens one
    with the person's `gh` or `glab`. The task keeps state Done with `Merge::Pull { number,
    url, head, … }`. That takes it out of the queue, which moves on, and the orchestrator is
    told where the work waits.
  - **Merged there, merged here.** The task's thread already watches its branch's pull
    request (the one PR watcher). When it reads that pull request merged, the task becomes
    Merged, with `pushed`. Closed without a merge, the task waits for the person's Merge
    again. The board shows the pull request on the task's pipeline until the watcher has read
    it.
  - **The clone catches up** (2026-10-10). Once the watcher reads a task's pull request merged,
    the server sends the orchestrator's worker `Verb::CatchUp` (`Hub::catch_up`). The worker
    fetches `origin`'s target to its tracking ref and fast-forwards the clone's target to it, as
    `Verb::FastForward` moves a branch (`repo::verify::catch_up`): in the checkout that has it
    checked out, through `merge --ff-only`, so the person's checkout gets the merge and nothing
    of theirs is overwritten. A target already there, or ahead, stays. One with commits the
    forge's lacks is not moved: the worker answers `Conflict` and the server logs it. The next
    task is then rebased onto what the forge holds, not onto the clone's old target. It runs
    in the background, since a fetch is the network's to take, and a race with the lane's own
    fast-forward is settled by the compare-and-swap both use.
  - Tests: `slopty-worker`
    `repo::verify::tests::a_protected_target_refuses_the_push_and_nothing_moves` (a bare
    forge whose pre-receive hook speaks GitHub's words) and
    `repo::pull::tests::work_on_a_protected_target_goes_up_as_a_pull_request` (a stand-in gh;
    found, not opened twice) and
    `work_on_a_protected_gitlab_target_goes_up_as_a_merge_request` (a stand-in glab on a GitLab
    `origin` whose pushes go to a bare forge on disk; found by its branches, not opened twice,
    gh never asked; added 2026-10-10); `slopty-server`
    `hub::queue::tests::a_protected_target_takes_the_work_through_a_pull_request` (now through
    the catch-up); `repo::verify::tests::the_clone_s_target_catches_up_with_origin_s` (a bare
    forge another checkout pushes the merge to: caught up and checked out, stays when there or
    ahead, refused when diverged); golden `catch_up`; `slopty-ui`
    `project::tests::work_waiting_in_a_pull_request_says_where`; goldens
    `land_pull`, `pull_opened`, `project_reply_protected` and `task_in_pull_card`.

- ✅ **Auto mode is looser than asking; a held terminal takes it away** (readiness 10-09 rank
  5, 2026-10-09). *Revised 2026-10-10: a project the person lets go on its own runs its
  agents in auto, see "A project's autonomy is how far its agents go" below.* Claude Code
  2.1.283 and later start an interactive session in `auto` when nothing names a mode
  (<https://code.claude.com/docs/en/permission-modes>, "Which mode a session starts in"). So a
  `claude` an orchestrator typed into a terminal the server opened for it came up in auto, the
  server read that as looser than allowed, and closed the terminal.
  - **Ruled: `auto` stays off `SAFE_MODES`.** In auto mode a second model, the classifier,
    reviews actions instead of the person, and lets through "everything, with background
    safety checks" (the same page's table of modes). That is less asking than `default`, which
    runs only reads unasked. With auto mode available, plan mode also lets classifier-approved
    commands run.
  - **What holds a run to asking.** It is the `--settings` lock the worker already put on
    agents the server starts (`slopty_agent::hooks::held_to_asking`, renamed from
    `without_bypass`): `permissions.disableBypassPermissionsMode` and now
    `permissions.disableAutoMode`, both `"disable"`. The docs say any settings file setting
    `disableAutoMode` starts the session in `default` and takes `auto` out of the Shift+Tab
    cycle. So the run starts asking and stays asking: keys typed into its TUI cannot reach
    auto, a settings file the agent writes cannot either, and plan mode's classifier is gone
    with it. A settings lock was chosen over pinning `--permission-mode default` because the
    flag sets only the start, while the lock also holds a mode switch later.
  - **The typed `claude`.** A terminal the server opens for an agent caller, in a project that
    allows no looser permissions, carries `SLOPTY_ASKING=1` (`project::ASKING_ENV`). Any value
    a caller passes is dropped first, and an agent naming it is refused, as every `SLOPTY_`
    variable is. `slopty hook wire` (the shell's `claude` function) puts the same lock on a
    `claude` typed there. A person's own terminal carries none and keeps their mode.
  - **Still closed.** A looser mode reported from a held terminal still closes it, and the
    words now say what auto mode is: "went into auto mode, where Claude Code's classifier
    approves what the person never allowed". `--permission-mode auto` named on an agent's
    start is refused with the same reason. A `claude` an agent types into a terminal the
    person opened carries no lock (the variable cannot be added to a running shell), so that
    case is still closed when it reports auto.
  - Tests: `slopty-agent`
    `hooks::tests::a_held_run_locks_bypass_and_auto_mode_off_and_allows_slopty_s_tools`;
    `slopty-cli`
    `hook::tests::a_claude_typed_where_the_server_holds_to_asking_starts_in_default`;
    `slopty-server` `hub::project_tests::an_agent_looser_than_allowed_is_closed` (the
    variable on an agent's terminal, none on the person's, the closing and refusing words for
    auto).

- ✅ **A clone's worktrees are listed with how each stands, and the merged ones go at once**
  (readiness R13, 2026-10-09). Agents leave a worktree under `.claude/worktrees/` for every
  task, so a long project fills the disk. Freeing them one at a time ("Remove this worktree",
  `docs/decisions/workspace.md`) needed a look at each.
  - **The listing** (`GitOp::Worktrees`, answered `GitDone::Worktrees`). The worker lists every
    linked worktree of the clone the folder is in that is still there, wherever it lies (since
    2026-10-10, see "Every linked worktree is listed"). For each it gives:
    - the files not committed;
    - whether a live terminal works in it;
    - its commits not yet in `origin`'s default branch or the clone's checked-out branch,
      matched by patch (`git cherry`), as removal judges them;
    - whether it is merged.
    It is merged when no commit is ahead. A squash or rebase merge leaves no commit that
    matches, so for a branch whose commits leave the question open, the person's own `gh` or
    `glab` is asked whether its pull request merged at its tip. Those asks run at once, and
    only for those worktrees. The newest commit comes first, with at most `WORKTREES_MAX`
    entries. Nothing is fetched or moved.
  - **"Remove merged worktrees"** is in the palette wherever "Review changes" applies (a folder
    in a clone, or a shell standing in one). Every merged worktree with nothing uncommitted, no
    terminal and no live agent in it is asked to go through the same `GitOp::RemoveWorktree`
    as one alone. The worker still refuses whatever is not safe. The others are passed over and
    counted. Once every answer is in, one notice says how many went, why any stayed, and how
    many were passed over. The folder tiles in a worktree that went close.
  - Tests: `repo::worktrees::tests::a_clones_worktrees_are_listed_with_how_each_stands`
    (slopty-worker: real git, a stand-in `gh`); `workspace::tests::worktrees::remove_merged_takes_only_the_landed_worktrees_nothing_works_in`
    and `remove_merged_says_when_there_is_nothing_to_take` (slopty-ui); goldens
    `client_git_worktrees`, `worker_git_worktrees`, and the e2e `settings-keyboard` (the new
    palette line).

- ✅ **A client asks a worker for a clone directly** (readiness rank 12, the wire and worker
  half, 2026-10-09). A start's folder step can offer a repository another machine has and this
  one lacks. Until now only the server could ask for a clone (`Verb::CloneRepo`), and only into
  `~/slopty/clones/`. A machine reached with no server must be able to make one too.
  - **The wire.** `ClientMsg::CloneRepo { request, url, into }` asks for it. `url` is the other
    machine's `RepoId.url`, which has no credentials in it. `into` is where the person's
    client chose to put it. `WorkerMsg::RepoCloning` says how far it has come, a step lost when
    the client is behind. `WorkerMsg::RepoCloned` answers with `CloneOutcome`: `Cloned` with
    the path and the `RepoId` it read, `Refused` in Slopty's words, or `Failed` in git's.
  - **The worker** clones with the same `Cloner` the server's clones use (`clone_into`), so the
    two kinds share their turns and their one-at-a-time per place. A clone is made beside its
    place and moved in only once git finished. A clone of the same origin already there is
    answered as found. A place that holds anything else, a path that is not absolute, or an
    address that names no remote is refused before git runs.
  - **Trust.** Slopty keeps Claude Code's folder trust only for the clones it places itself
    under `~/slopty/clones/`. A clone the person placed is theirs to trust, as any folder of
    theirs is.
  - Tests: `repo::cloning::tests::a_clone_goes_where_the_person_asks` (slopty-worker: real
    git, the forge reached through the person's own `insteadOf`); the daemon's
    `a_clone_asked_by_a_client_is_found_or_refused` (`apps/slopty-worker/tests/e2e.rs`);
    goldens `client_clone_repo`, `worker_repo_cloning`, `worker_repo_cloned`,
    `worker_repo_clone_refused` and `worker_repo_clone_failed`.

- ✅ **The repository's run and archive scripts** (2026-10-09, readiness rank 15, R12). The
  scripts a project kept were Slopty's own, typed in once more per project, while the
  repository usually holds them already in another tool's file. Conductor, the Codex app,
  Superset and T3 Code each have a Run action for a dev server or a test watch, and Conductor
  and Superset an archive (teardown) script that stops what a worktree left going before it
  is removed.
  - **Read, never a file of our own,** as the setup is (`repo::run`), from the checkout the
    scripts would run in, in the order of `setup::SOURCES`. For each kind the first file that
    names one wins, and sources are never merged.
    - Run: `.conductor/settings.toml` (`[scripts] run`, a string, or named
      `[scripts.run.<id>]` tables with `command`, `args`, `options.cwd`, `default` and
      `hide`), `conductor.json` (`scripts.run`), `.codex/environments/environment.toml`
      (`[[actions]]`, each a `name` and a `command`), `.superset/config.json` (`run`, with its
      `cwd`) and `t3.json` (the scripts not marked `runOnWorktreeCreate`). A file's one script
      is named "Run". The default comes first. A folder that climbs out of the checkout runs
      in the checkout.
    - Archive: `.conductor/settings.toml` and `conductor.json` (`scripts.archive`) and
      `.superset/config.json` (`teardown`). Cursor and Codex keep none.
  - **The wire.** `GitOp::Scripts` on a folder answers `GitDone::Scripts(RunScripts { from,
    list })`. Each `RunScript` is ready to open: its name, its line, and the `command`, `cwd`
    and `env` a client passes to `OpenSession` as they are. The command runs the line through
    the person's login shell, which takes the terminal over when it ends (`repo::script`). The
    environment names the places as a setup from the same file gets them. So no new message
    opens one, and a script runs where its client is connected, with or without a server.
  - **The archive runs before a worktree goes** (`repo::worktrees::take_out`), so it runs for
    every removal: a thread's, "Remove merged worktrees" and a task's. It runs as the setup
    does, in the worktree with the same environment. When it fails the worktree stays and the
    removal fails with its exit code and last line (`Failed::Archive`). A discard (a forced
    removal) logs the failure and goes on, since its changes go anyway.
  - **Agents.** A script is the person's to start from the client. An agent that wants the
    command reads the repository's file itself.
  - The client's Run (a palette line, and a picker when there are several) belongs to the UI.
  - Tests: `repo::run` (slopty-worker): each tool's form and the order, then the real files
    of public repositories as their tools read them (`run_fixtures/`), and a script ready to
    open; `repo::worktrees::the_archive_script_runs_before_a_worktree_goes` (a failure keeps
    the worktree, a discard goes on); goldens `client_git_scripts` and `worker_git_scripts`.
    The project goldens changed, and `script_set`, `script_delete`, `script_run` and
    `run_script` are gone with their verbs.

- ✅ **An agent's permissions are told again to every new server link** (2026-10-10, readiness
  10-10 rank 8). The worker marked an agent's permission mode (`PermissionMode`) and what
  loosens it (`Loosened`) as reported once it put them on the daemon's report broadcast. With
  the server link down (a server restart, which is every update) or behind, they were lost, and
  registration did not send them again. So the guard never closed a terminal held to asking
  whose agent went looser while the server could not hear.
  - What was last reported of each live session stands in the agent table
    (`AgentTable::permission_reports`): its mode, and what loosens it when anything does. A
    session that ends takes them with it.
  - Every registration sends them after the agents' branches, and so does a link that fell
    behind the broadcast (`Lagged`). The server's guard judges a report it already acted on
    the same way, and a terminal it closed is no longer among the live sessions.
  - Tests: `slopty-agent` `tests::the_permissions_reported_stand_for_the_next_link`, and the
    daemon's `claude_in_a_shell_line_is_guarded_and_judged_as_a_spawned_one`
    (`apps/slopty-worker/tests/server_link.rs`), whose second link, after the first closes as
    a restarting server's does, hears the `Loosened` again with nothing changed.

- ✅ **Reports outlive a full link, a lost word and a server restart** (2026-10-10, readiness
  10-10 rank 9). A batch of reports the worker's link could not take when it fell due stayed
  outstanding with nothing to send it again: no new report, no registration. What waited and
  what was outstanding lived only in the server's memory, so a restart (every update) lost it.
  And the worker's word that a batch was read could be lost, so the server sent it again and the
  agent read it twice.
  - **A full link.** A batch the link refuses (`try_send`) falls due again after `RESEND`
    (one second) and goes as the next batch, with whatever came since. One for a worker with no
    link still goes when it registers.
  - **A restart.** The store keeps what waits for each node and what is outstanding, in
    `deliveries.json` beside the projects, written after each burst of changes settles and once
    more at shutdown. When each word came is kept as wall time. A server that comes back takes
    it up before any link is served, and a worker registering is sent its outstanding batches
    again under their numbers. One outstanding on a terminal that closed while the server was
    away waits for its node's next terminal.
  - **One number per batch.** Numbers go on past those kept and past the wall clock in
    milliseconds, so a server whose store was lost or set aside never reuses one a worker saw.
  - **A lost word.** The worker notes the last batch each session's agent read, beside the
    batches. That note outlives the worker's restart and goes when the session ends. A batch
    sent again under that number is acknowledged again and never kept to be read twice. A link
    that fell behind the report broadcast says every last read again. A repeated `Delivered` on
    the server matches no outstanding batch and does nothing.
  - **A fold after a lost word.** Suppose the word that batch N was read is lost while the
    link stays up, and a new report falls due before the worker says N again. The server then
    folds N's reports into N+1. So each report carries an id of its own in every batch it rides
    in. `Deliver` holds `Reports { open, blocks: [ReportBlock { id, text }], close }` where it
    held one text. Ids are never reused, across restarts too, and go on past the wall clock as
    batch numbers do.
    - The worker notes the ids its agent read with the last batch (`reports::Read`, the latest
      `READ_KEPT`). It keeps a batch without them (`reports::put`). A batch with nothing left
      to read is only acknowledged.
    - Tests: `deliver::tests::a_word_keeps_its_id_in_every_batch_it_rides_in`,
      `reports::tests::nothing_read_is_kept_again`. The daemon's
      `reports_reach_an_agent_through_its_next_prompt_s_hook` folds the read report into
      batch 8 beside a new one, and only the new one waits. Golden `deliver` changed.
  - Tests: `deliver::tests` `a_batch_the_link_could_not_take_goes_again`,
    `reports_on_their_way_outlive_a_restart` and
    `an_outstanding_batch_on_a_terminal_gone_is_found`; the hub's
    `a_batch_a_full_link_could_not_take_goes_again` and
    `a_batch_not_handed_over_outlives_a_restart` (through the store); `slopty-agent`
    `reports::tests::a_batch_read_already_is_not_kept_again`; and the daemon's
    `reports_reach_an_agent_through_its_next_prompt_s_hook`, which sends the batch again after it
    was read and hears `Delivered` again with nothing kept.

- ✅ **Every linked worktree is listed, marked by who made it** (2026-10-10, readiness 10-10
  rank 18; wire change). The listing showed only `.claude/worktrees/`. So a worktree made by
  hand, or by Codex 0.162's managed worktrees, never showed and was never cleaned up.
  - The worker lists every linked worktree that `git worktree list` names. Each carries
    `AgentWorktree::made_by`, the agent whose tool made it, read from where it lies and what it
    keeps:
    - Claude Code for one under the clone's `.claude/worktrees/`, where Slopty makes its own too;
    - Codex for one in a four-hex bucket under `$CODEX_HOME/worktrees` (`<root>/1f2e/<name>`),
      as Codex's own `has_managed_layout` reads it, or one whose git directory keeps Codex's
      owner file `codex-thread.json`, wherever its root was set;
    - none for one made by hand or by a tool not known here.
    The field is an open agent id, so a new tool's mark is one more rule, not a new type.
  - Removing and freeing take any linked worktree, under the same guards: never one a terminal
    works in, never one with anything not committed (`git worktree remove` without `--force`),
    and its branch only once every commit landed. A discard, which takes work not committed
    with it, still touches only an agent's worktree under `.claude/worktrees/`.
  - Not read: a Codex root moved by `[desktop] git-worktree-root` in its config. Such a
    worktree is marked by its owner file once a thread holds it, and listed unmarked before.
  - Tests: `repo::worktrees::tests::a_clones_worktrees_are_listed_with_how_each_stands` (a hand-made
    worktree listed unmarked and free to go, a Codex one marked by its owner file),
    `codex_worktrees_are_known_by_their_layout`, and
    `a_worktree_in_use_or_not_committed_is_kept` (the person's own is kept while it holds a
    change, then goes with its branch). Golden `worker_git_worktrees` changed.

- ✅ **A task's machine that goes away is said, and frees its place** (2026-10-11, readiness
  10-11 rank 13, the server's half). A task whose worker stopped answering stayed "Running",
  its orchestrator heard nothing, and its agent still counted against `live_agents`.
  - **Said.** When a worker's link drops (`Hub::lost`, the moment it is `Unreachable`), every
    task whose agent works there (an open assignment, its work not over) gets a timeline note
    ("Its machine mini stopped answering…"), and its clock stops (`Projects::machine_seen`).
    Its orchestrator is told at once (`Kind::Stuck`), with the way on: start the task again on
    another machine. When the worker registers again, those tasks, and only those, hear that
    it answers again, and their orchestrators too. The set of tasks told is kept in memory, so
    a server restart tells nobody that a machine is back when nobody heard it went.
  - **Freed.** A worker away holds no place: `projects::live` counts only linked workers'
    terminals and seats. So the fleet's and a project's bounds count none of its agents, and
    its task may start again elsewhere (its terminal is no longer live, so the start is not
    refused as "has a terminal already"). Should the worker come back with the old agent still
    there, both run, which the person chose by starting it again.
  - Test: `a_task_s_machine_going_away_is_said_and_frees_its_place` (slopty-server
    `hub/project_tests.rs`).

- ✅ **A merged task's worktree and the server's branches go, however its terminal ended**
  (2026-10-11, readiness 10-11 rank 14).
  - **Worktrees.** The settle loop freed a merged task's worktree only after closing the agent's
    terminal itself. One the person closed, or that ended while its worker was away, leaked.
    Now a terminal's close and a worker's registration both free the merged tasks on that worker
    whose terminal is no longer open (`Projects::unfreed`, `Hub::free_closed`). Each task is
    asked of its worker once (`Projects::freeing`). One the worker kept (something not committed,
    a terminal in it) is not asked again on its own, so the timeline says why once. "Remove
    merged" in the worktree list stays the person's way to try again.
  - **Branches.** Once a task's work brought home (`slopty/<project>/<task>`) is merged, the
    server drops that branch from the orchestrator's clone (`Verb::DropBranches`, which takes
    only `slopty/` names, passes over one absent and keeps one a worktree has checked out).
    `Task::base_ref` named a ref nothing ever wrote, and is deleted.
  - **A project let go** frees what is left: each task's worktree on its worker (kept as above
    when it holds work), and the server's branches in every clone they may be in: the home
    branches in the orchestrator's clone, `slopty/<project>/target` in the tasks' clones on
    other machines (`steps::server_branches`).
  - Tests: `a_merged_task_s_worktree_goes_though_its_terminal_closed_unsettled` (slopty-server
    `hub/outcome_tests.rs`), `the_server_s_branches_go_and_no_other` (slopty-worker
    `repo/worktrees.rs`), and the merge across two machines now checks the home branch is gone
    (`a_finished_task_is_verified_and_merged_into_the_orchestrator_s_clone`, slopty-cli
    `tests/projects.rs`).

- ✅ **A change the person makes is made once across a drop and a restart** (2026-10-11,
  readiness 10-11 rank 16).
  - **The client.** Every verb a client sends that changes something goes under a key of its
    own (`ServerCaller::call`). One whose answer a drop lost is carried to the next link and sent
    again under the same key, in the order it was first sent. It used to answer Interrupted at
    once. A read goes again too, with no key. Past `RESEND_WITHIN` (30 s, well inside the
    server's 10-minute `KEY_LIFETIME`) the carried verb answers Interrupted, so a long outage
    still says so.
  - **The server.** The keys project changes and starts were made under, and the merges waiting
    for a branch to come home, used to live only in memory. They now go to the projects log as
    `Keep::Key` and `Keep::Merge` lines beside the changes they belong to, and the snapshot holds
    them (`ProjectsFile::keys`, `merges`). A key's age goes by the wall clock and is read back
    as an `Instant` that far in the past; one past its lifetime is let go. The file holds at most
    `KEYS_KEPT`. A restored merge goes once the Home step the restart takes up again brings the
    branch home.
  - **Made once.** A push again and a project let go are now made once under their key as well
    (`Hub::once`). Before, they ignored it, and a repeated delete answered "unknown project".
  - **Format.** A `projects.json` from before has no `keys` or `merges` and is set aside as
    unreadable (pre-release; no default kept for old files).
  - Tests: `a_keyed_change_sent_again_after_a_restart_is_made_once` and
    `a_merge_waiting_for_its_branch_outlives_a_restart` (slopty-server `hub/project_tests.rs`),
    and the store log's replay covering keys and merges (`store.rs`). Also
    `a_verb_whose_answer_a_drop_lost_goes_again_under_its_key` (slopty-client
    `tests/server_link.rs`).

- ✅ **The person's pin reaches the orchestrator, and every way in can restart a task**
  (2026-10-11, readiness 10-11 rank 17, the server and tools half).
  - **A pin is told.** "Run on…" set a planned task's pin and told nobody, so an orchestrator
    could start the task elsewhere. When the person moves a task's pin (`TaskUpdate { run_on }`
    that changes it), the orchestrator now gets a notice through its hooks, paced like a done
    report. The notice names the machine, or says the task may now run anywhere. A change that
    leaves the pin as it was, and an agent's own pin, are not told.
  - **A pinned start with nothing to work on is refused.** A start with no directory goes beside
    a clone of the project's repository. Pinned to a worker with no clone, and with no address to
    clone from, it used to start in that worker's home. It is now refused (`Unplaced`), saying to
    clone it there or pin it to a worker that has a clone. The GUI's Start sends
    exactly such a start.
  - **Restart and push everywhere.** `TaskRestart` reached the server only from the GUI. Now the
    orchestrator has the `task_restart` tool, and the CLI has `slopty task restart [--agent]`.
    The CLI also has `slopty task push`; push stays the person's, so it is no agent tool.
  - Tests:
    - `the_person_s_pin_is_told_to_the_orchestrator` and the pinned refusal in
      `a_task_with_no_directory_goes_beside_a_clone_in_a_worktree_of_its_own` (slopty-server);
    - `task_restart_hands_a_task_to_a_new_agent` and the tool list (slopty-tools);
    - `restart_and_push_name_their_task` (slopty-cli).

- ✅ **A task on another machine starts from the merged target, and a dependent waits for the
  merge** (2026-10-12, readiness 10-12 rank 3).
  - **The start.** Pushing is off by default, so what the merge queue merged stays in the
    orchestrator's clone. A task placed in a clone on another machine branched its worktree
    from that machine's `origin/<target>`. It worked on stale code, and its conflicts showed up
    only at merge.
    - Now, before such a start, the server sends the target the way a give-back does
      (`Hub::send_target_to`, sharing the trip with `send_target`). It goes as
      `slopty/<project>/target`, and the worktree starts from that branch.
    - While it goes, the task's card shows a clone step, which settles to where the target
      came from.
    - When the forge holds everything (`NothingNew`), the worktree starts from the target as
      before. A task in the orchestrator's own clone sends nothing.
    - A target that cannot be sent refuses the start, saying why. Starting an agent on code
      the person has already moved past would only waste its work.
  - **Dependencies.** A dependent task could start once its dependency was Done, before that
    work was merged anywhere a new worktree starts from. A dependency now counts only once it
    is Merged (`project::delivered`). A read-only task is the exception: it has nothing to
    merge, so Done is enough. Only a restart, whose work is begun, starts it anyway.
  - Tests: `a_task_elsewhere_starts_from_the_target_the_orchestrator_s_clone_holds`
    (slopty-server `hub/project_tests.rs`), with the earlier start tests now answering the
    target's trip; `a_dependent_starts_once_its_dependency_is_merged` (`project/tests.rs`).

- ✅ **Letting a project go cleans up on every machine, and merged work frees its branch
  wherever it ran** (2026-10-12, readiness 10-12 rank 15).
  - **A remote task's branch.** The merge queue rebases in the orchestrator's clone, so the
    merge's head exists only there. A branch on another machine held none of it and was always
    judged "did not land", then kept. `Merge::Merged` and `Merge::Pull` now carry `from`, the
    task's own commit that the merge took, in hex, and `RemoveWorktree`'s `landed` names it
    first. The worker resolves the commit (`Outcome::Rebased::from`), because a merge with no
    verifier knows only the branch's name.
    - A merge held after a rebase goes on from what that rebase made, which only the
      orchestrator's clone holds. The lane remembers the task's own commit beside it
      (`Lanes::rebased`), in memory. After a server restart, the rebased commit stands in, and
      the branch elsewhere is kept: the safe side.
  - **A kept worktree.** One the worker kept, for work not committed or a terminal in it, stayed
    in the "being freed" set forever, so even letting the project go skipped it. It now moves to
    a kept set. That set is never asked again on its own, since the person frees it from the
    worktree list, but letting the project go asks once more.
    - A close that failed, or a worker unreachable mid-free, leaves the task free to be asked
      again: by the settle loop a whole wait later, or once the worker is back.
  - **Merged with its terminal closed already.** Freeing followed only a terminal's end or a
    worker's registration. Now any change that leaves a task Merged with a worktree runs the
    same pass (`Hub::projects_moved`), whether the queue, its pull request or the person
    merged it.
  - **An offline machine, and the verify checkout.** Delete was fire-and-forget. Each removal
    (worktrees, the server's branches, and now the project's verify checkout
    `~/slopty/verify/<project>`) is a `Cleanup` the store keeps (`ProjectsFile::cleanups`,
    `Keep::Cleanup`). The kept removal goes once its worker answers, however it answers. One
    whose worker is unreachable waits and is asked as that worker registers, across a server
    restart too. A machine back under a new id takes its own.
    - No verb was needed for the checkout. `RemoveWorktree` given a path in `VERIFY_PLACES`
      removes it by force (`repo::verify::drop_checkout`): it is Slopty's own, and a build
      there holds gigabytes. No archive script runs. One already gone answers as removed, and
      one whose clone is gone goes as a plain directory.
  - Tests: `a_held_merge_keeps_the_task_s_own_commit_and_frees_a_closed_agent_s_worktree`
    (`hub/queue/tests.rs`); `a_let_go_project_cleans_up_a_kept_worktree_and_its_checkout_once_the_worker_is_back`
    and the retried close in `a_merged_task_s_worktree_goes_once_its_agent_is_closed`
    (`hub/outcome_tests.rs`); the cleanup lines in the store's replay tests;
    `a_let_go_project_s_checkout_goes_by_force` and `Rebased::from` in slopty-worker's
    `repo/verify/tests.rs`.

- ✅ **A task's Codex is held to asking, as Claude Code is** (2026-10-12, readiness 10-12
  rank 24).
  - **The gap.** Only Claude Code tasks were pinned to a mode that asks (`--permission-mode
    default`, a locked watch). A Codex task's arguments were judged (`hub/codex.rs`), but the
    person's own `config.toml` still decided its approval policy and sandbox, so
    `approval_policy = "never"` there ran a task with no approvals at all.
  - **At the start.** At the project's autonomy ("A project's autonomy is how far its agents
    go", 2026-10-10; before, without `permission_flags`), a task's Codex in its terminal
    starts with `--ask-for-approval on-request --sandbox workspace-write`. Flags
    win over `config.toml`. Arguments that name their own policy or sandbox keep them, and the
    argument check lets those through only when they ask no less. (A task's Codex has run only
    in its terminal since 2026-10-10, when a start stopped naming its arguments.)
  - **As it runs.** A task's Codex is watched as locked, as Claude Code's terminal is. Its row
    says the effective policy (the thread's mode) and sandbox (its `sandbox` fact), read from
    the app-server whether the TUI or a thread runs it (`Board::codex_moves`). A policy past
    `on-request` (`never`, `granular`), or a sandbox past the workspace (`dangerFullAccess`,
    `externalSandbox`), closes it, and the timeline says why (`Hub::codex_settings`). That is
    how a thread whose configuration names no sandbox is held, and how a switch made in the
    TUI is caught. A terminal an agent opened or typed into is held the same way.
  - Rejected: reading the person's `config.toml` on the worker to predict the policy. Codex
    resolves profiles, project trust and managed configuration itself. Its row says the
    outcome, so the outcome is what is judged.
  - Tests: `a_task_s_codex_is_held_to_its_level` and
    `a_codex_thread_s_settings_are_judged_as_its_arguments_are` (`hub/codex.rs`); the pinned
    command and the close in `a_codex_task_goes_only_where_codex_is_and_starts_with_its_role`
    (`hub/project_tests.rs`).

- ✅ **What the merge queue merged can be pushed from the board, and the cards say what waits**
  (2026-10-12, readiness 10-12 rank 16, the server's half).
  - Pushing is off unless the person turns it on, so merged work stays in the orchestrator's
    clone. `TaskPush` acted only after a push had failed. Now it pushes the target for any
    merged task not pushed yet.
  - A push takes the whole branch, but only the task it was asked for had its card updated, so
    "not pushed" stayed on every earlier merge. Now a push that goes through, the merge's own
    or the person's, marks every task merged into that target by then as pushed
    (`Projects::pushed_with`).
  - So the board reads what waits off the cards: the merged tasks whose merge is not pushed.
    No count goes on the wire. A count of commits would need a git read of the orchestrator's
    clone on every status, and tasks are the unit the board already shows. A push made outside
    Slopty is not seen. Pushing from the board again answers at once, and the forge says
    nothing new.
  - Tests: `a_push_takes_every_merge_before_it` (`project/merge/tests.rs`);
    `a_merge_left_unpushed_is_pushed_on_the_person_s_word` (`hub/queue/tests.rs`).

- ✅ **A worktree's pull request goes back into the branch it came from, and the merge sheet
  offers the person's own method first** (2026-10-12, readiness 10-12 below the line 1, the
  worker's half). A pull request opened from an agent's worktree always targeted the
  repository's default branch, so a worktree made from `release-2` asked to merge into
  `main`. The merge sheet always started on Squash, whatever the repository allows or the
  person last chose.
  - **The base.** A new worktree from a branch records it as the branch's `gh-merge-base`
    (`git config branch.worktree-<name>.gh-merge-base <base>`). That is the key `gh pr create`
    reads when it is given no `--base` (gh 2.102), so gh needs nothing more. A start from a
    pull request's head, or from a detached `HEAD`, records none. `GitStatus.merge_base` carries
    the key to the client, and on a GitLab the worker passes it to glab as `--target-branch`
    when the person names no target.
  - **The method.** `PullStatus.methods` lists the ways the repository allows, in gh's words
    and in the order merge, squash, rebase, from `gh repo view --json
    viewerDefaultMergeMethod,…MergeAllowed`. `PullStatus.method` is the one to offer first, in
    this order of preference:
    - the method last merged by from Slopty, kept repo-local as `slopty.merge-method` (a
      clone's worktrees share it) and written on every merge that succeeds, while the
      repository still allows it;
    - else the person's last method on GitHub;
    - else the first one allowed.
    A GitLab's project settings are not read by glab, so all three are offered there, with the
    kept method else `merge` first.
  - **Cost.** What a repository allows is asked once and kept 10 minutes per checkout. The pull
    request watcher and the landed-worktree check read the pull request without it
    (`pull::status`); only what a person sees (`GitOp::PullStatus`, the read after a merge or
    a push) carries it (`pull::offered`).
  - Tests: slopty-worker:
    - `a_pull_request_offers_the_ways_its_repository_allows_and_the_last_merged_by`;
    - `the_method_offered_first_falls_back_in_order`;
    - `a_merge_request_goes_into_the_branch_s_merge_base`;
    - `a_merge_request_is_opened_and_merged_with_glab` (the kept method);
    - `a_status_names_the_branch_s_merge_base`;
    - the `gh-merge-base` cases of `a_new_worktree_starts_current_and_carries_the_ignored_files_it_names`
      and `a_worktree_of_a_merge_request_checks_out_its_head_and_tracks_it`.
    The `worker_git_status`, `worker_git_pull_status`, `worker_git_merge_request_status`,
    `worker_git_merged` and `worker_git_pushed` goldens moved.

- ✅ **A project carries its goal, the person's autonomy, and where the goal stands; its
  members are gone** (2026-10-10, the orchestrator-first study, items 3, 9, 10 and 12, the wire
  half).
  - **Goal.** `Project.goal` is what the person hands over, the orchestrator's first prompt.
    It is kept trimmed, within `BRIEF_MAX` as a brief is, and none when empty
    (`ProjectCreate.goal`).
  - **Autonomy.** `Project.autonomy` (`Ask`, `Edits`, `Own`; `Ask` by default) is how far the
    project's agents go before they ask. Only the person sets it, at `ProjectCreate` or through
    `ProjectSet.autonomy`. An agent naming anything but `Ask` is refused. How each level maps
    onto each agent's own permission modes is "A project's autonomy is how far its agents
    go", below.
  - **Progress.** `Verb::ProjectProgress { summary, next, done }` is the orchestrator saying
    where the goal stands. The server keeps it as `Project.progress`, with its time, and logs it
    as `Moment::Update`. A task's agent may not say it, an empty summary is refused, and each
    text is at most `Progress::TEXT_MAX` (2048) bytes. Telling the person once on `done`
    (`NoticeKind::GoalDone`) is item 12's.
  - **Members are deleted.** Grouping by repository on the client already does what they did,
    so `Project.members`, the matchers and their bounds are gone.
  - Tests: `a_project_carries_its_goal_and_the_person_s_autonomy` and
    `the_orchestrator_says_where_the_goal_stands` (slopty-server `hub/project_tests.rs`); the
    goldens `project_create`, `project_set`, `project_progress`, `project_snapshot` and
    `project_pushed`.

- ✅ **A task's start names only the worker and the agent** (2026-10-10, the orchestrator-first
  study, item 4's wire half).
  - **Why.** `task_start` took about 22 arguments: a folder, a command, a prompt, a model, the
    agent's arguments, environment, a size and `ignore_dependencies`. The orchestrator only
    ever needs to say what the work is, where it runs and which agent does it. Everything else
    either was the server's to decide or a way around the person's bounds.
  - **The wire.** `TaskLaunch` is `{ pin, agent }`. The hub builds its own `Launch` from it:
    the agent told the task's brief first, beside a clone of the project's repository in a
    worktree of the task's own when it writes. With no clone known, it starts in the
    repository's path when the project names one, else in the worker's home. A restart builds
    the same `Launch` with the folder the earlier agent worked in.
  - **Deleted with it.** Command tasks (`Runner::Command`) and `Runner` itself; judging a
    start's arguments and environment, since a start names none; Codex run as a task's thread.
    The CLI's `slopty task start` loses `--cwd`, `--prompt`, `--command`, `--model`, `--env`,
    the size, `--ignore-dependencies` and the agent's arguments.
  - **Kept.** A `codex` with loosening flags typed into a terminal an agent opens is now judged
    as a `claude` there was, since the command task that judged it went.
  - **One wait.** `project_status` reads at once. `task_wait` is the one wait, so
    `Verb::ProjectStatus` keeps its `timeout_ms` for it.
  - Tests: `task_start_makes_and_starts_a_task_in_the_caller_s_own_project` (slopty-tools);
    `flags_that_loosen_permissions_are_the_person_s_alone` and
    `a_pinned_task_is_spawned_on_its_worker_and_put_on_its_task` (slopty-server); the CLI's
    `apps/slopty-cli/tests/projects.rs`, its stand-in agent scripted through the worker's
    environment; the goldens `task_spawn` and `task_spawn_agent`.

- ✅ **The orchestrator dispatches, and its tools say only that** (2026-10-10, the
  orchestrator-first study, item 4's tool and role half).
  - **Role.** The orchestrator's role now says it dispatches and does not code. It puts the
    split to the person in plan mode before starting any task, answers its tasks' scope
    questions with `task_tell` (asking the person only what is theirs to decide), and ends the
    goal with its summary.
    - "Do sequential and small work yourself" is gone: a coding orchestrator is a single agent
      with extra steps.
    - The second "task_start runs Claude Code" line is gone; `task_start`'s own description
      says which agents it runs.
  - **`task_start`** takes `title`, `brief`, `depends_on`, `read_only`, `worker` and `agent`,
    plus `task` for one the project has. `task` stays because a stopped, given-back or refused
    task is started again by its number, and the server's own refusals tell the orchestrator
    so. Its `project` (always the caller's own) and its `idempotency_key` are gone.
  - **`task_update`** keeps `task`, `status` and `note`. A task's state moves by itself: its
    agent's status, `task_report` and the person's merge move it. `state`, `branch`, `base`,
    `depends_on`, `verifier`, `metadata`, `project` and `idempotency_key` are the CLI's
    (`slopty task update`) and no model's.
  - **`project_status`** reads at once, with no progress wrapper; `task_wait` is the one wait.
  - Tests: `the_tool_schemas_are_golden` (slopty-tools, the schema of every tool a model is
    handed); `task_start_makes_and_starts_a_task_in_the_caller_s_own_project` and
    `the_server_s_record_of_the_caller_s_terminal_names_its_task` (the trimmed arguments are
    unknown fields); `a_report_reaches_the_orchestrator_through_its_worker` (slopty-server, the
    role's words).

- ✅ **A project's autonomy is how far its agents go** (2026-10-10, the orchestrator-first study,
  item 10). It replaces `[server.projects] permission_flags` and revises "Auto mode is looser
  than asking" (2026-10-09) for projects the person lets go on their own.
  - **Why.** `permission_flags` was an all-or-nothing switch in a settings file, naming projects
    whose agents could be handed any flag. The person sets `Project.autonomy` from the board
    instead, and the server alone turns it into each agent's own permissions.
  - **Each level, per agent.** `Autonomy::claude_mode` and `Autonomy::codex_approval`
    (`slopty-proto`):

    | Level | Claude Code `--permission-mode` | Codex `--ask-for-approval` |
    |-------|---------------------------------|----------------------------|
    | Ask   | `default`                       | `on-request`               |
    | Edits | `acceptEdits`                   | `on-request`               |
    | Own   | `auto`                          | `never`                    |

    Codex always runs with `--sandbox workspace-write`. Its sandbox already lets it edit the
    workspace, so Edits needs no looser policy.
  - **The worker holds the level.** `Verb::SpawnAgent.autonomy` (in place of
    `permission_flags`) tells the worker to lock the run (`hooks::held_to`). Bypass mode is
    always locked off, auto mode unless the level is Own, and `mcp__slopty` joins the allow
    list so the agent's work with its project never waits on the person. A resumed agent keeps
    the lock it had (`Invocation::auto`).
  - **The server watches it.** `Watched.held` (in place of `locked`) is the level the agent was
    started at. A mode or Codex setting past that level closes its terminal and the timeline
    says why: `acceptEdits` passes at Edits, `auto` only at Own, bypass never. A start keeps
    its level. Raising a project's autonomy starts its next agents higher.
  - **Agents never loosen.** No agent's start may name a loosening flag or a steering variable
    in any project, and no agent types into another agent's TUI. An agent's own start, and a
    terminal it opens, is held to Ask whatever it names. The person's own starts are theirs,
    flags and all, and the worker holds them to nothing.
  - **Managed builds.** A company's managed settings may set `disableAutoMode`. Then Claude
    Code starts an Own agent in `default`, the strictest level, by itself. The board says so
    (the UI's half).
  - Tests: `a_task_s_agent_starts_at_its_project_s_autonomy`,
    `an_agent_looser_than_allowed_is_closed` (a project raised to Edits keeps `acceptEdits` and
    closes on `auto`), `flags_that_loosen_permissions_are_the_person_s_alone` and
    `no_agent_starts_anything_looser_than_asking` (`hub/project_tests.rs`);
    `a_task_s_codex_is_held_to_its_level` and
    `a_codex_thread_s_settings_are_judged_as_its_arguments_are` (`hub/codex.rs`);
    `a_held_run_locks_bypass_and_auto_mode_off_and_allows_slopty_s_tools` (`slopty-agent`
    hooks); `the_project_bounds_come_from_the_shared_settings` (`slopty-serverd`); the goldens
    `server_request_spawn` and `project_reply_status`.

- ✅ **A task's questions go to its orchestrator, at once** (2026-10-11, the orchestrator-first
  study, items 11 and 16's first half).
  - **Why.** A task's agent with a scope or design question either asked the person through
    its own question tool, which put an orchestrator's decision in the person's queue, or ended
    its turn, which the orchestrator heard only once `DONE_SETTLE` (2 minutes) had passed.
    A finished task's report waited the same two minutes.
  - **The role.** A task's agent is told that a question the brief does not settle goes to the
    orchestrator with `task_report`, after which it ends its turn, and that it asks the person
    nothing itself: only its own permission prompts reach the person. The orchestrator's role
    already answers with `task_tell` and asks the person, through its own question tool, only
    what is theirs (item 4). Nothing answers a prompt for the person.
  - **At once.** An agent's own report falls due as it comes (`deliver::Item::due`), a
    question as much as a finish. A later report still unsent replaces it as before.
    `DONE_SETTLE` now paces only what the server says of a turn nobody reported, and
    `WAIT_SETTLE` a wait on the person. A report carries no branch when it asks, so it starts
    no trip home and no verifier.
  - **Its words.** A report's block opens `task N reports:` rather than `task N: done`, since a
    report may ask. `task_report`'s description says both uses.
  - Tests: `an_agent_s_report_goes_at_once_and_an_unreported_turn_settles` (`deliver`); the
    role's words in `a_start_whose_answer_was_lost_is_put_on_its_task_when_its_terminal_shows`;
    the label in `a_report_reaches_the_orchestrator_through_its_worker` and the outcome tests.

- ✅ **The orchestrator says where the goal stands, and the person hears it met once**
  (2026-10-11, the orchestrator-first study, item 12, the server's half).
  - **The tool.** `project_update { summary, next, done }` is `Verb::ProjectProgress` for the
    orchestrator's own project (`ops::project_update`), the tenth tool. Its latest summary is
    the board's first line, which is the UI's half. The orchestrator's role tells it to say
    where the goal stands as tasks land, and to end with `done` and a summary of what was done,
    what was merged and what is left.
  - **Goal met, once.** When a progress says `done` and the last one did not, the server
    tells the person (`ladder::tell_goal_met`, `NoticeKind::GoalDone`), routed as every notice
    is: to the client they are at, or pushed to their phone when they are at none. The words
    are "Goal met:" and the summary's first line, about the project at the update's timeline
    entry, opening the orchestrator. Saying `done` again tells nothing.
  - Tests: `a_goal_met_is_told_once` (`hub/ladder/tests.rs`);
    `project_update_says_where_the_goal_stands_in_the_caller_s_project` and the tool list and
    schema goldens (slopty-tools); the role's words in
    `a_report_reaches_the_orchestrator_through_its_worker`.

- ✅ **Work ready to merge reaches the person, once per batch** (2026-10-11, the
  orchestrator-first study, item 13, the server's notice).
  - **Why.** Verified work waited on the board in silence: nothing told a person who was away
    that a merge was theirs to make.
  - **One note per project.** When a task turns ready (done, not read-only, in no merge yet),
    the server waits `READY_SETTLE` (20 s), so tasks verified close together make one note.
    Then it tells the person how many of the project's tasks wait on their merge
    (`ladder::tell_ready`, `NoticeKind::ReadyToMerge`): "3 ready to merge", or "#4 Store is
    ready to merge" for one alone. The note is about the project at the entry that made the
    latest ready, opens the orchestrator, and goes where every notice goes. On a phone its
    `PushBody.merges` names the task ready longest (earliest update, then lowest number), which
    the note's Merge action merges with no round trip. Work turning ready later makes a new
    note with the count as it stands.
  - Each note keeps its own collapse id (one per timeline entry), so an older count stays on
    the phone until read. A Merge from it on a task already merged is refused by the server.
  - Test: `work_turning_ready_is_pushed_once_with_the_oldest_to_merge` (`hub/ladder/tests.rs`:
    three ready while the person is away push once after the settle, the oldest is the Merge,
    and a fourth later pushes the new count).

- ✅ **A dependent may start from its dependency's verified work, before that work merges**
  (2026-10-11, the orchestrator-first study, item 13, the dependency opt-in).
  - **Why.** A dependent waited for its dependency's merge, and the merge waits on the person.
    A chain of tasks therefore ran one merge at a time, even when each link was done and its
    verifier had passed.
  - **The opt-in.** A new task may name one of its `depends_on` as `start_from`
    (`TaskSpec::start_from`, `task_start`'s `start_from`, the CLI's `--start-from`). A
    `start_from` outside `depends_on` is refused. `may_start` counts that dependency once it is
    Done on a branch with a verifier pass (`Record::checked`). Only a pass names the commit the
    work is checked at, so in a project with no verifier the task waits for the merge as any
    dependent does. The other dependencies are still merged first.
  - **Its worktree.** The base is that work's branch as the orchestrator's clone holds it
    (`Record::starts_on`). A worktree in that clone starts from the branch directly. A clone on
    another machine is first sent it as `slopty/<project>/<n>` (`Hub::send_start_to`, which
    replaced `send_target_to` and sends the target the same way). The agent's role says it
    builds on that work and leaves its commits alone. The task keeps the commit it started on,
    which is the dependency's verified head (`Task::started_on`), from its first start; a
    restart reuses the worktree.
  - **Its merge.** The task waits in the queue until that work is merged (`queue_of`). Its
    branch then still holds the dependency's original commits, while the target holds them
    rebased, or resolved by that agent after a give-back. The plain rebase would pick them
    again and conflict, so `Verb::Rebase` carries `after`, the commit it started on. When the
    head holds `after` and the target does not, the worker rebases with `--onto <target>
    <after>`, picking only the task's own commits. Anything else gets the plain rebase, which
    covers a restart that began on the target, or a retry already on top of it.
  - **The person's word.** Dropping the dependency from `depends_on` drops `start_from` with
    it. The task then merges with what its branch holds.
  - Tests:
    - `a_task_starts_from_its_dependency_s_checked_work_and_merges_after_it`
      (`project/tests.rs`);
    - `a_task_starts_on_its_dependency_s_checked_branch` (`hub/project_tests.rs`);
    - the `after` assertion in
      `a_task_done_is_verified_in_the_orchestrator_s_clone_and_merged_by_fast_forward`
      (`hub/queue/tests.rs`);
    - `work_started_on_another_s_is_rebased_from_where_it_started` (slopty-worker
      `repo/verify/tests.rs`: the plain rebase conflicts, the `--onto` one picks the one
      commit, and an unrelated head takes the plain rebase);
    - the `task_start` test, the tool schema, and the `rebase`, `task_create` and task goldens.

- ✅ **`task_start` answers once placed, and a task's worktree merges back into the target**
  (2026-10-11, the orchestrator-first study, item 16; its first part, a report going at once,
  landed with item 13's notice).
  - **The answer.** A start that had to clone the repository onto its worker, or send that
    clone the target or another task's work first, held the orchestrator's `task_start`, and
    the person's Start, for as long as the clone and the trip took. That could be minutes.
    The hub now reserves the start once the worker is chosen. That reservation is the place
    counted against the bounds, and a second start of the task is refused. It answers with the
    task, its clone step under way, and runs the rest in the background (`Hub::finish_start`):
    the clone, the send, the agent's start, and its terminal put on the task.
    - A start with nothing to send still answers once its agent has started, with its
      terminal, as before (`start_sends`, `Hub::sends_start`).
    - A start that fails after its answer says why on the card, through its step. It also
      tells the orchestrator at once (`start_failed`, `Kind::Stuck`): "task N could not start:
      …", to start it again once that is put right. Its reservation goes, so the next start
      is not held up.
  - **The merge base.** A task's worktree may start from another task's work
    (`start_from`), and its pull request must still merge into the project's target.
    `NewWorktree.merge_base` carries the target apart from `base`, and the worker sets
    `branch.<b>.gh-merge-base` to it (`worktrees::make`). A worktree that names none merges
    back into the branch it started from, as before.
  - Tests:
    - the failure half of `a_task_elsewhere_starts_from_the_target_the_orchestrator_s_clone_holds`
      (the answer at once with its step, the failed trip delivered to the orchestrator, and a
      second start not held up);
    - the clone failure in `a_task_on_a_worker_with_no_clone_gets_one_made_and_shown`;
    - the merge base in `a_task_starts_on_its_dependency_s_checked_branch`;
    - `a_new_worktree_starts_current_and_carries_the_ignored_files_it_names` (slopty-worker:
      `gh-merge-base` is the target while the worktree starts from another branch);
    - the CLI's `a_task_s_clone_is_made_where_it_runs_and_its_branch_comes_home`, which waits
      for the agent after the early answer;
    - the `start_thread` and `client_start_in_worktree` goldens.

- ✅ **Project state survives a server restart** (2026-10-11, the orchestrator-first study,
  item 18).
  - **Clones.** The server kept the clones it had made in memory (`Steps::made`), so a
    restarted server forgot them. Its next task on that worker then cloned again, or was
    refused when no address was known. That list is deleted.
    - Each worker reports the clones under `~/slopty/clones` in its own `repos` fact
      (`facts::clones`, `facts::REPOS`): each key of a clone's identity mapped to its path,
      the same shape the server uses.
    - A clone made for the server joins that fact at once (`facts::clone_made`, through
      `Orchestrator::set_facts`). A gathering that started before the clone keeps it while its
      folder is there.
    - On `Outcome::Cloned` the server writes it into its copy of the worker's facts
      (`projects::cloned`), so the start that asked can go on. `repos_of` merges the worker's
      `repos` with its shells' repositories.
  - **A Home trip waits for both ends.** A trip taken up after a restart when the
    orchestrator's worker registers ran at once and failed if the task's worker had not
    registered yet. It now waits for that worker (`away_from`, `Projects::resume_on`). In the
    other order it was already right: a trip is taken up only when the orchestrator's worker
    is back.
  - **A merge pressed while its branch is on its way home** was already kept
    (`ProjectsFile::merges`).
  - **Starts are stored.** A task start under way whose terminal is not yet on its task is
    kept (`ProjectsFile::starts`, `Keep::Starts`, `projects::keep_starts` at every change to
    `starting`). The next server takes it up as answered: it still counts, a second start of
    the task is refused, and its terminal goes on the task once its worker shows it. A
    start's grace runs only while its worker is linked, from the moment it registers, so a
    worker away for longer than the grace does not lose it.
  - **The send has its own step.** Sending a task's clone its start, or the target to rebase
    onto, was shown as a Clone or a Merge step. It is now `StepKind::Send`, added last so no
    golden moved. Its failure is a start's, so the card offers Start, not Retry
    (`project/model.rs`, which also gets its step line).
  - Tests:
    - `the_clones_made_for_the_server_are_a_fact` (slopty-worker);
    - the reported clones in `a_repository_is_a_fact_of_every_worker_with_a_clone`;
    - `a_branch_on_its_way_home_waits_for_both_ends_after_a_restart` (both orders);
    - `a_start_a_restart_cut_off_is_put_on_its_task_when_its_worker_returns`, on a paused
      clock across twice the grace, with the store's replica agreeing;
    - the Send steps in the hub and CLI tests.
