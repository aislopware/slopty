# Spaces and agents

Slopty runs coding agents on many machines as if they were one. The model has three parts and
nothing else:

- **A space** is a folder on a machine. Threads live in spaces; the same repository cloned on
  two machines is two spaces of one repository.
- **An agent** is any thread: Claude Code, Codex, pi or an ACP agent, in a terminal or not.
- **An orchestrator** is any agent that started others. Nothing marks it beyond that: each
  agent knows its parent (the fact `slopty.parent` on its row), so every device draws the same
  tree, at any depth.

The person hands over a goal to whichever agent they like. That agent splits the work, starts
agents on the machines that suit it, follows them and brings their branches home, with eight
tools and its own judgement. Slopty adds reach and hearing, not a workflow.

**The task engine is gone; agents start agents.** ✅ 2026-10-13
- Until now a project was a goal the server drove: a task state machine, a placement step, a
  verifier and a merge queue with give-backs and a review limit, an orchestrator role and a
  task role written into each agent's prompt, and a timeline, recap and cost per project. The
  person found it heavy and the agents hemmed in; the frontier models it served plan, place,
  verify and merge well on their own when they can reach the machines.
- Prior art pointed the same way: Claude Code's own subagents and Codex's are plain
  "start another, hear when it is done", with no states of their own; Amp and T3 Code cut
  their orchestration machinery down to a few tools and the model's judgement.
- What replaced it:
  - **No state machine.** An agent's state is its thread's: working, at rest, waiting on the
    person, ended. The server keeps no task, no plan and no outcome of its own.
  - **No roles.** No agent is told how to work. A started agent's system text is one line that
    names the agent that started it.
  - **No verifier, merge queue or review limit.** An agent tests and merges with its own tools
    in its own terminal; the person reviews what lands.
  - **No placement.** The agent reads the machines' facts and names one.
- Deleted with it: the project store, its log and its snapshot; the project and task verbs and
  outcomes and their goldens; the board, task cards, review tile and commit sheet in the app;
  `slopty project` and `slopty task`; the project tools; the loosening judge and the held-to-
  asking start; project notices on the attention ladder.

**Eight tools, each saying what it does.** ✅ 2026-10-13
- An agent Slopty starts gets Slopty's tools over MCP (`slopty mcp` on its `--mcp-config`), and
  the same verbs are `slopty agent …`, `slopty agents` and `slopty branch-bring` in any shell:
  - `machines`: every machine with its facts (OS, CPUs, memory, agents installed, toolchains,
    clones), online or not.
  - `agent_start`: start an agent on a machine, in a folder or the machine's clone of a
    repository, optionally in a fresh worktree; answers its id once it runs.
  - `agent_tell`: words for an agent, which reach it through its hooks marked as the caller's.
  - `agent_read`: an agent's turns, what was said or also what was done, from a cursor.
  - `agent_wait`: wait until one (or each) of some agents comes to rest, needs the person or
    ends; answers each as it stands.
  - `agent_stop`: stop an agent and every agent under it, deepest first.
  - `agents`: every agent on the fleet, each with its parent.
  - `branch_bring`: carry a branch from one machine's clone to another's, as a git bundle
    through the server.
- A description says what the tool does and what it returns, never how to work. Arguments are
  few and open: a machine by name or id, an agent by id, an agent kind by its own name, a model
  and an effort by the agent's own words. A repeat under the same idempotency key answers as
  the first did.
- No tool answers a request in the person's place, and an agent sends no raw harness
  arguments: what a started agent may do is the person's one setting
  (`docs/decisions/agents.md`, "How freely a started agent acts is one setting").

**An agent reaches its own tree.** ✅ 2026-10-13
- An agent tells and stops only agents under the agent at the top of its own tree: itself, its
  parent's other children, what it started. Anything else is `Forbidden`; an unknown agent is
  `UnknownAgent`. The person reaches every agent.
- Reading (`agent_read`, `agent_wait`, `agents`) is open to every agent: the fleet is one
  workspace, and seeing it changes nothing.
- An agent started from a terminal the server cannot tie to a thread has no parent; so does
  one the person starts.

**A parent hears through its hooks.** ✅ 2026-10-13 (replaces "Reports go up through hooks")
- What a child comes to reaches its parent unasked, through the parent's own hooks, as Claude
  Code's subagents reach theirs (`slopty-server::deliver`):
  - a rest, once it has settled for 10 s (`REST_SETTLE`), with the child's last words, so a
    flurry of turns is one word;
  - a wait on the person, once it has lasted 30 s (`WAIT_SETTLE`): a permission the person
    answers at once wakes nobody;
  - an end, and words another agent tells, at once.
- A later word about a child replaces the one still waiting, the child going back to work takes
  it back, and a parent that already saw the child at rest (`agent_wait`, `agent_read`) is not
  told again.
- Words told are marked as whose they are: the person's as the person's, an agent's as "an
  agent, not the person", with its id. A block cannot close its `<slopty-reports>` tag, so no
  word reads as Slopty's own.
- The batch goes to the worker of the recipient's live seat (`FromServer::Deliver`), at most
  9000 bytes. The agent's `SessionStart`, `UserPromptSubmit` and `Stop` hooks run
  `slopty hook reports` and hand it in as `additionalContext`, or at `Stop` as
  `decision: block` so a finishing agent reads it first. An idle Claude Code is woken through
  its inbox (`CLAUDE_CODE_MESSAGING_SOCKET`), under the same rules as typing: never while it
  waits on the person or holds their draft. A thread with no terminal takes the batch as a
  message. Either way the worker answers `AgentReport::Delivered`.
- A batch stays outstanding until delivered: sent again when the worker registers again,
  folded into the next when more falls due, put back when its seat closes. The store keeps
  what waits (`deliveries.json`), so a server restart loses nothing; batch numbers and word ids
  are never reused, so a word read once is never read twice.
- Why hooks, not typing: the TUI stays the source of truth and nothing types into it behind
  the person's back. There is no report verb: an agent says what it did in its turn, and its
  parent reads that.

**The attention ladder rolls up by thread, tree, space and fleet.** ✅ 2026-10-13
- What needs the person rolls up from a thread to its tree (the agent at the top and every agent
  under it, across machines), to its space (the folder, or the repository's clone), to the
  fleet, and reaches the phone as before. The project tiers went with projects.
- An agent another started that finishes sends the person no notice: its parent hears it. A
  question or a failure still reaches the person, wherever the agent sits in a tree.

**Tests use a stub agent.** ✅ 2026-09-30, narrowed 2026-10-13
- End-to-end tests run `slopty-stub-claude` as `claude`, never the real one, so no plan is
  spent. It fires the hooks its `--settings` registers, keeps a transcript, takes a turn with
  its first prompt (`STUB_TURNS`) and calls Slopty's tools through the `slopty mcp` its
  `--mcp-config` names.
- `apps/slopty-cli/tests/agents.rs` proves the model against a real server, ptyd and worker:
  the person starts a lead; the lead starts a child on the machine, waits until it rests, reads
  its turn and tells it more; the child takes the words through its hooks, marked as the
  lead's; and once the child rests again the lead hears it through its own hooks, unasked.

**Workers report open facts.** ✅ 2026-09-30
- A fact is a flag, a number, a word, or a list or map of them (`Fact`). Each worker sends its
  facts on the server link (`ToServer::Facts`) when they change: installed agents and
  toolchains with their versions, Rust targets, GPUs, AC or battery, its clones. They are
  gathered lazily and cached (`slopty-worker::facts`), never on a hot path.
- The server adds what it knows itself (name, OS, CPUs, memory, load, online, live agents,
  repositories) and cuts each worker's facts to bounds (1024 items per list or map, 4 KiB per
  text, 4096 facts, 64 KiB in all). `machines` and `slopty workers --json` show them. Nothing
  on the server judges rules over them.

**An agent never has more than the person gave it.** ✅ 2026-09-30, narrowed 2026-10-13
- Every link says whom it speaks for. A client is the person and an MCP surface is an agent.
  The CLI inside a Slopty terminal is an agent when an agent runs there, when an agent opened
  it or typed into it, or when the server does not know it. So an agent cannot borrow the
  person's word through a shell, its own or one it drives.
- A program proves which terminal it runs in with the token its worker made for that terminal
  (`SLOPTY_SESSION_TOKEN`): a keyed blake3 of the session id under the worker's own key, which
  the worker sends the server when it registers. An unproven agent has no parent and reaches
  no tree.
- Only the person answers a request, sends a message as the composer would, restarts a worker
  or changes settings: each is `Forbidden` from an agent. Only the person's read of a thread
  holds its prompts. The server alone asks a worker for a clone, a bundle or a seated thread.
- An agent may not type into another agent's TUI: what reaches an agent from another goes
  through its hooks.
- Claude Code's folder trust is written only for a folder strictly inside the one Slopty makes
  its own in, never an ancestor or a worktree whose `.git` leads outside.
- An agent's environment for a new terminal cannot steer what runs there: `PATH`, `HOME`,
  `SHELL`, `SLOPTY_*`, `CLAUDE*`, `ANTHROPIC_*`, `DYLD_*`, `LD_*` and the like are refused.
- Known and left open: the terminals an agent opened or typed into are remembered in the
  server's memory only, so after a server restart the CLI in such a shell speaks for the person
  until an agent touches it again.

**Any agent's thread is read and answered alike.** ✅ 2026-10-04, narrowed 2026-10-13
- `ReadThread { of, view, after, hold }` reads through the thread model, whatever the agent.
  `of` names a terminal or a thread's id, a subagent's too; the server finds the worker that
  holds it. The answer is whole turns after a cursor, in a `messages` view or an `activity`
  view that adds each tool call and the end of its output. Reasoning is never given.
- A read is bounded so a model can take it in at once: a message cut to 4000 characters, an
  output to its last 1000, the read stopping at the first whole turn past 48 000 characters.
  `truncated`, `next` and `skipped` say what was left out.
- The open requests come with the read. The person answers one with `AnswerRequest`
  (`slopty agent answer`); there is no tool for it, and the server refuses it from an agent.


**An agent's worktree is made by its worker.** ✅ 2026-10-01, narrowed 2026-10-13
- `agent_start` with a worktree name makes `worktree-<name>` beside the folder's clone (Claude
  Code makes its own; for any other agent the worker does, under the clone's
  `.claude/worktrees/`). A worktree goes when the person removes it, after the repository's
  archive script.

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
  committed is committed as saved. Each git op starts once the saves sent before it are on
  disk, and after the earlier status, commit, review or worktree removal of its repository.
  - It runs beside the saves sent after it (2026-10-11, readiness-w 10-11 rank 8). A save
    waits only for a git op that holds its path: a commit holds the paths it commits, and a
    worktree's removal holds everything under it. A committed path is matched by its last
    components, so a file of the same name elsewhere waits as well, which costs a wait and
    never commits half a save. A folder op waits for every op that holds paths. Before, a
    status, commit, review or removal ran in the save loop itself, so a pre-commit hook that
    ran for minutes held every save the client made. Test:
    `a_long_commit_hook_holds_only_the_saves_of_what_it_commits` (`slopty-workerd` e2e, a
    hook that waits on a fifo the test opens).
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
    reads and the pull-request watcher run on it too. Test:
    `repo::commit::tests::a_hook_s_tool_is_found_on_the_person_s_path`.
  - A login shell that does not say its `PATH` within `LOGIN_WAIT` (5 s) is left for a minute
    (`LOGIN_RETRY`, 2026-10-11, readiness W item 6). Meanwhile git runs on the daemon's own
    `PATH`, and the worker warns once. Before, a failed read was never kept, so a slow or
    broken rc file added 5 s to every commit, push and forge read. Test:
    `facts::tests::a_login_shell_that_failed_is_left_a_minute`.
  - A refusal comes back as `GitOutcome::Failed { said }`: the end of git's or gh's own words,
    at most `SAID_MAX`, such as a rejected push or a hook that failed.
  - What Slopty itself refuses is `Refused { why }`: no files chosen, an empty message,
    a path outside the repository, a folder in no repository, no remote, or a detached HEAD.
  - A missing program is `Unavailable { program, why }`. With no gh, the answer says so and
    opens nothing.
  - No message is ever made up. An empty one is refused, since a commit takes the person's.
- **Not an agent's.** An agent commits, pushes and opens pull requests with its own git and
  gh in its own terminal. The sheet is the person's, in the app.
- Tests:
  - `repo::commit` (`slopty-worker`): git's records parsed; the chosen files committed with
    the message, other staged files left staged, a push that sets the upstream and then one
    that does not; and refusals in git's words, from a failing hook to a rejected push.
  - `a_file_saved_then_committed_is_committed_as_saved` (`slopty-workerd` e2e, the direct
    route).
  - The goldens in `golden_git`.


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
    thread, a terminal opened in a worktree, an agent another agent started. A failure keeps
    the worktree. A client's start answers `Outcome::SetupFailed { setup, code }` with where
    the setup came from and its last `Setup::TAIL` lines. While it runs, the client is sent
    `WorkerMsg::SettingUp` with the same, a few times a second at most. Starting again runs it
    again; `NewWorktree::setup` false starts without it, for good. An agent's start fails
    with the same words and lines, for the agent that asked to read.
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
    every removal: a thread's and "Remove merged worktrees". It runs as the setup
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


- ✅ **Landed worktrees are swept daily, and a kept one says what it keeps** (2026-10-16, goal
  re-audit finding 25, the worker side). Nothing cleaned up the worktrees agents left (Claude
  Code's subagent trees, the runs a terminal held when they were closed), and a worktree kept
  for its uncommitted files said so only in a trace.
  - **The sweep.** Ten minutes after the worker starts, then every 24 hours, it looks over each
    clone a thread it holds works in (`repo::worktrees::sweep`). Under `.claude/worktrees/` it
    removes each worktree whose work landed: every commit in `origin`'s default branch or the
    clone's checked-out branch by patch, or its pull request merged at its tip as `gh` or `glab`
    says. Its branch goes once landed, as `free` does. It never touches a worktree with work
    not landed, one a held thread or a live terminal works in, one made elsewhere by hand or by
    Codex, or the clone. A landed one with anything not committed is kept and counted. A
    thread's own worktree is never swept while the worker holds the thread, so a chat taken up
    again still finds its folder.
  - **The words.** A worktree kept for what is not committed is refused as
    "Kept: N uncommitted changes in <tree> (<first lines>)", Claude desktop's words, wherever it
    is freed: `GitOp::RemoveWorktree`, orchestration's verbs, and the sweep's log. Freeing a
    chat's worktree when the person archives it is the client's call of
    `GitOp::RemoveWorktree`, which shows these words; archiving stays the person's filing on
    the app.
  - Tests: `repo::worktrees::tests::{a_sweep_takes_landed_clean_worktrees_and_keeps_the_rest,
    a_worktree_in_use_or_not_committed_is_kept}` (`slopty-worker`).
- ✅ **A worktree with commits not pushed is kept** (2026-10-17, D12 of
  `.research/claude-desktop-2026-10-16.md`, whose branch removal keeps commits that exist nowhere
  else). Freeing or sweeping a worktree checked only for files not committed, so a worktree whose
  commits sat on a detached `HEAD`, or had landed only in the clone's own `main` before it was
  pushed, could go with work saved nowhere but this machine.
  - **The check.** A commit of the worktree's `HEAD` is not pushed when no remote branch holds
    it (`git rev-list HEAD --not --remotes`) and `origin`'s default branch has no commit of the
    same patch (`git cherry`), so work rebased or cherry-picked there counts as pushed. A pull
    request its forge says merged at its tip is pushed. A clone with no remote has nowhere to
    push: there a commit counts when no branch holds it.
  - **Kept and said.** `remove`, `free` (`GitOp::RemoveWorktree`, orchestration's verbs) and
    the daily sweep keep such a worktree whole, and say "Kept: N commits not pushed in <tree>
    (<the newest few, short hash and subject>)", as they say uncommitted files. Nothing is
    deleted, and the worktree goes once its commits are pushed. A run the person discards
    (`discard`) keeps its commits on its branch, as before.
  - Tests: `repo::worktrees::tests::a_worktree_with_commits_not_pushed_is_kept`
    (`slopty-worker`): a branch not pushed, a detached `HEAD`'s commit, and work landed only in
    the clone's `main`, each kept, then gone once pushed.
