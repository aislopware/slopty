# Agents

How Slopty runs, shows and steers coding agents of every kind. The research, with sources, is in
`.research/gui-first-2026-10-01/`. `plan.md` there synthesises it, and `protocols.md`,
`desktops.md`, `orchestrators.md`, `workflows.md` and `design.md` hold the evidence.

- ✅ **GUI-first: the agent's own session is the source of truth** (2026-10-01, verified against
  Claude Code 2.1.286, Codex app-server at `ecc78e4`, pi 0.99.2 and ACP at `9e03215`). After a
  long time working in terminals, the person found that a terminal's productivity cannot match
  a GUI that is well designed, beautiful and complete. In agentic work the person mostly
  directs, watches and reviews agents rather than typing commands. So Slopty becomes GUI-first
  and agent-native. It keeps the best terminal there is, one action away. The GUI extends beyond
  Claude Code to Codex, pi and any ACP agent. The core goal stays: directing and working on
  many remote machines must feel like one local machine.
  - **What this supersedes.** The rule that the TUI is the only source of truth (CLAUDE.md
    until 2026-10-01, and the "What stays" half of "Claude Code gets a conversation face; the
    TUI stays the source of truth" in `claude-code.md`, 2026-09-27). Its principle is kept and
    widened: the agent's *own* session is the record, not the TUI's screen.
  - **The rules.**
    1. The native session is the record: Claude's session id and transcript, Codex's thread on
       its daemon, pi's session file. The worker's thread log is a cache rebuilt from it, never
       a second history.
    2. One writer per session, enforced by the worker.
    3. Slopty acts only through the agent's published doors: its protocol (Codex app-server,
       pi RPC, Claude's stream-json control protocol when driven, ACP), its hooks, or
       keystrokes into its TUI sent on the person's word behind the draft guard. It never reads
       the screen to control an agent, never types menu digits or cycles mode keys, and never
       answers for the person.
    4. The TUI can always take a session over: live beside the face for Codex, by a handoff at
       idle for a driven agent. A TUI-born agent gets the whole GUI.
    5. Slopty launches the user's own unmodified binary, never offers a login and never reads a
       credential. API keys and third-party providers are first-class.
  - **Each agent's drive.** An adapter declares a drive and an open set of capabilities, and the
    UI shows a control only where the capability is present.
    - *Claude Code, observed by default.* Its TUI runs in the PTY, and the face reads hooks, the
      transcript and the mod's live blocks. The worker owns the composer and the queue, and
      types on the person's word. Approvals go through the blocking `PermissionRequest` hook.
    - *Claude Code, driven, as an opt-in per profile* (`claude -p` stream-json with the control
      protocol). Recommended for API keys and third-party providers. Handoff to the TUI at idle.
    - *Codex, shared.* Slopty is a second live client of the user's app-server daemon socket,
      and `codex` in a PTY attaches to the same thread beside the face.
    - *pi, driven,* over `pi --mode rpc`, with Slopty's permission-gate extension, which fails
      closed, because pi has no permission system of its own. Handoff to its TUI at idle.
    - *Any ACP agent, driven,* over its stdio with the protocol's own types (see "Any ACP agent
      is driven by one adapter" below).
    - *Any other program:* status only, through `slopty hook report`.
  - **Why Claude stays observed by default.** Driving gives a far richer face (token deltas,
    mode and model in place, `rewind_files`, `stop_task`). But Zed reports that Agent SDK use
    on subscription plans moved to a separate, limited credit pool, about 15–30× the cost for
    heavy use. A driven `claude -p` session is very likely billed the same way. Anthropic's
    legal page permits the unmodified binary with the user's own subscription, while its SDK
    overview forbids third-party claude.ai login, and no page settles driven `-p` use. The
    observed path is plainly allowed, already built and keeps the TUI live. Reopen this when
    Anthropic settles the question.
  - **Remote feels local.** Adapters, approval holds and turn snapshots live on the worker, so
    a dropped link pauses nothing. Each followed thread streams a snapshot, then actions
    numbered by epoch and sequence. A client resumes from where it left off with only the
    actions it missed. Sends and approvals carry intent ids, render at once, survive a
    reconnect and are deduplicated by the worker. The client caches each thread on disk, so it
    opens in the first frame. The model follows Codex's thread, turn and item primitives with
    the synchronisation semantics of Microsoft's Agent Host Protocol. Slopty takes AHP's
    model, not its wire: AHP is pre-1.0 and breaks with each minor release, its types are
    JSON-only and cannot travel on postcard, and its Rust crate is client-only.
  - **Turn snapshots.** At each turn edge the worker writes the whole working tree to a private
    git ref. Review, undo and rewind then cover every agent, including edits made through Bash,
    which Claude's own checkpoints miss.
  - **Order.** M1: one thread model with Claude GUI-first on it (the attention ladder, the
    thread view that survives drops, the worker composer and queue, turn snapshots and the
    review tile). M2: Codex, pi and the fleet overview. M3: projects the person directs from
    the board. M4: takeover and reach (git panel, moving an agent between hosts, best-of-N,
    generic ACP). The ranked list is `plan.md` §3.

- ✅ **The thread model: Codex's primitives with AHP's synchronisation, in Slopty's own types**
  (2026-10-01, `crates/slopty-proto/src/thread.rs`; the research is `plan.md` §2.2 and
  `protocols.md` §6.5, §7.2).
  - **Shape.** A thread (`ThreadId`, a UUIDv7 the worker mints) holds turns (`TurnId`, counted
    from 1, with `TurnId::BEFORE` for what precedes the first) and items (`ItemId`, the agent's
    own id where it is stable). Beside them it holds its open requests, the pending messages,
    the plan, background tasks, meters and the composer's commands. A subagent is a thread of
    its own, linked both ways: `ThreadMeta::parent` names the call, and `ToolCall::child` names
    the thread.
  - **Open where agents differ, closed where they share structure.** The agent (`AgentId`), its
    capabilities (`Cap`), its drive (`Drive`), a tool call's kind, a request's kind, a notice's
    kind, an origin, a wait's kind and the token kinds in `Usage` are all open strings, with the
    known values as constants. A new agent therefore brings its own values with no wire change.
    Three things are closed enums, because every adapter maps onto them and the UI ranks or
    places by them: `Phase` (the attention ladder's input), `ToolState` and `PartKey`.
    `ToolDetail` is keyed by kind (edit, write, read, search, exec, fetch, web search, agent,
    question, plan, tasks, mcp), never by one agent's tool names. An unknown kind has no detail,
    and an unknown native record is an `ItemBody::Extra`, kept and never dropped.
  - **One mutation, one reducer.** `Action` is the only way a thread changes.
    `ThreadState::apply` is pure and is the same code on the worker and on every client:
    - an action about a turn or item the state does not hold (one paged out) changes nothing;
    - `ItemCompleted` is authoritative;
    - `ItemUpdated` never moves a tool call back from a final state, and never back to
      streaming (`ToolState::may_become`);
    - `Append` counts lines and characters as if the text had come whole, so any chunking
      gives the same state. This is a property test, beside one that carries a state through
      the codec mid-stream and goes on.
  - **Requests carry the agent's own options.** Each `Choice` has the agent's id and label,
    an `Effect` (allow, deny or answer, so a client can tell yes from no without knowing the
    agent), the agent's scope words and whether it also stops the turn. Slopty never invents
    one. A settled request records who answered it (`Answerer`). The state keeps the open
    requests plus at most `RESOLVED_KEPT` (32) settled ones, so a client that comes back
    still sees who answered.
  - **Integers on the wire.** Cost is in millionths of a dollar and a rate window in
    hundredths of a percent, so the whole state is `Eq` and a golden or a property test
    compares it exactly.
  - **The wire.** `thread::wire` holds:
    - `ThreadRequest`: `Table`, `Follow { have, turns, max_latency_ms }`, `Page`, `Expand`,
      `Start`, `Intent`, `Approvals`;
    - `Intent`, each naming the capability it needs (`Intent::needs`);
    - `IntentDone` with an `Outcome`;
    - `ThreadFrame`: `Snapshot`, then `Actions { epoch, first }`, plus `Page` and `Expanded`;
    - `TableFrame` with `ThreadRow`, which carries the open requests as cards.

    The goldens are `tests/golden_thread.rs`. `ThreadState::window` and `page` cut a snapshot
    and its pages, and a property test shows that a window plus its pages rebuild the thread.
  - **Landed beside the old path, not over it.** `conversation.rs` and `agent.rs` stay until
    the UI lanes switch over. Until then the new types ride no `ClientMsg`, `WorkerMsg` or
    `UniHead` variant. The switch-over adds `ClientMsg::Thread`, `WorkerMsg::ThreadTable` and
    `WorkerMsg::IntentDone`, and `UniHead::Thread`, and deletes the old types in the same
    change.

- ✅ **The thread host keeps a log per thread, and a follower resumes from its cursor**
  (2026-10-01, `crates/slopty-worker/src/thread/`; tests in `crates/slopty-worker/tests/threads.rs`).
  - **The log.** `<data dir>/threads/<id>/` holds two files:
    - `snapshot`: the state and its cursor;
    - `tail`: a head naming the cursor it continues from, then each action framed as the
      wire frames it.

    The last 4096 actions (at most 4 MiB) are also kept in memory, which is what a returning
    follower is sent. A turn's end writes a new snapshot and starts the tail file over, once the
    tail file holds 1024 actions. The in-memory tail survives that, so compaction costs no
    follower a snapshot.
  - **A cache, so nothing is synced.** The agent's own session is the record, so the log is
    never fsynced. A torn last action is cut off on reading. A tail whose head does not match
    the snapshot is dropped: it is either from another epoch or the tail a compaction was about
    to replace. `Log::reset` rebuilds the log from the native session under the next epoch.
    A new log's first epoch is random, so a log lost and made again never meets an old cursor
    by chance.
  - **Follow.** A cursor of the same epoch inside the kept tail gets only the actions after it.
    Anything else gets a snapshot of the last turns, and older turns come by page. The host
    takes a follower's catch-up and subscribes it to the thread's feed under one lock, so
    nothing is missed or sent twice.
  - **Coalescing.** A follower holds batches for its `max_latency_ms`, and sends early once
    4 KiB of text or 512 actions have gathered. Runs of appends to one part are merged.
    `ThreadFrame::Actions` therefore carries `first` and `next`, and may hold fewer actions
    than `next - first`: the reducer gives the same state either way, which W0's chunking
    property proves. With no budget, each batch goes alone. A follower that lags the feed,
    or meets a batch that does not follow on, catches up from its cursor, by the tail or by a
    snapshot.
  - **The table** is in memory and rebuilt from the logs at start, under a new random epoch,
    so a client's cursor from an earlier run gets every row. It sends a delta while it still
    remembers every removal since the cursor (the last 1024 of them). A row that differs only
    in `updated_ms` changes nothing.
  - **Intents.** Each thread keeps the outcome of its last 512 intent ids. They are appended to
    `intents` beside the log and rewritten when that file reaches twice as many. A repeated id
    therefore gets the first outcome, across a worker restart too. Intents that start a
    thread are kept in `threads/starts`. A check, its action and its record happen under one
    lock.

- ✅ **Claude Code observed is an adapter: a codec in `slopty-agent`, its IO on the worker**
  (2026-10-02, `crates/slopty-agent/src/observed.rs`, `crates/slopty-worker/src/thread/claude.rs`;
  tests in `observed/tests.rs` over the recorded fixtures, and `crates/slopty-worker/tests/claude_threads.rs`).
  - **Beside today's path, sharing its inputs.** The driver hears what every client hears: the
    tracker's merged status (`WorkerMsg::Agent`), the held prompts (`WorkerMsg::Permission`),
    and the session summaries for the working directory. It also gets the board each followed
    session already has, with its hooks heard, meters, subagent files and the mod's blocks.
    It reads the transcripts itself on the blocking pool, after each hook and every 250 ms,
    with the same decoder the face uses. Nothing of the old path changes. The daemon starts it
    (`apps/slopty-worker/src/threads.rs`), with the host under `<data dir>/threads`.
  - **A thread per Claude Code session, named by the session's own id.** The id is derived
    from the session id (and for a subagent, from that and its agent id). So the same session
    is the same thread across a worker restart, and across a `claude --resume` in another
    terminal. The thread begins once the session id is known, from a hook or from the
    transcript's file name (before that, a provisional thread stands for it; see the entry on
    what a row carries). A thread held from before starts over under a new epoch and is
    read again, as the ruling has it: the log is a cache of the native session. When a
    terminal moves to another session (`/clear`, `/resume`), the old thread is left exited and
    resumable.
  - **The mapping.**
    - A prompt opens the next turn of its thread, and later entries belong to that turn. A
      branch (a rewind or an edited prompt) truncates the thread back to before the prompt it
      dropped.
    - The transcript's turn record fills in the models, tokens and end. A turn with an
      interrupt in it ends `Interrupted`.
    - Edits and writes add up to the turn's +N −M.
    - Tool kinds come from the decoder's typed detail, and titles are worded here.
    - A subagent's thread links to the call that started it, and the call names the thread.
  - **The mod's blocks are items.** The adapter keeps an `Overlay` as one more follower, so it
    shares the settling rules the face has (`live:<turn>:<step>:<block>`). A text or thinking
    block is a provisional item, appended to as it grows and removed once the transcript
    settles it. A tool block takes the call's own id, so the transcript's entry replaces it.
  - **Prompts are requests with Claude Code's own answers:** allow, always allow (worded with
    what the suggestions grant), deny, and deny and stop. A question carries its questions, and
    a plan is approve or deny. `observed::verdict` maps a choice back to the `Verdict` the
    relay prints. The status, meters and open requests are told again whenever the main thread
    begins anew, since the transcript has none of them.
  - **Not carried yet:** the agent's version (the mod's hello has it), and a session's working
    directory until its shell next reports one after the worker starts.

- ✅ **Threads ride the link beside the conversation path, which keeps working unchanged**
  (2026-10-02, `apps/slopty-worker/src/threads.rs`, `crates/slopty-net/src/streams.rs`;
  goldens in `crates/slopty-proto/tests/golden_thread.rs`, proved end to end by
  `apps/slopty-worker/tests/threads.rs`).
  - **Additive on the wire.** `ClientMsg::Thread` carries every `ThreadRequest`.
    `WorkerMsg::Threads` carries the table and `WorkerMsg::IntentDone` an intent's outcome,
    both on the control stream. A followed thread gets a unidirectional stream of its own
    (`UniHead::Thread`, at the conversation streams' priority) carrying `ThreadFrame`s. Each
    variant is appended last, so no existing golden changed.
  - **One stream per followed thread, as the conversation has.** Its task sends what the
    `Follower` makes from the client's cursor, and answers `Page` and `Expand` between frames
    on the same stream. A follow of a thread already followed is ignored. Unfollowing ends the
    task, and the stream finishes. The table is one task per connection, replaced by the next
    `Table` ask: what the client lacks from its cursor, then a frame at each change.
  - **Following a thread holds its terminal's prompts, as following its conversation does.**
    The connection joins the session's followers, so a `PermissionRequest` waits for it. It
    leaves when neither path follows that session any more.
  - **Intents act under the host's lock, once per id.** `Answer` and `Release` go through the
    held prompts the conversation path answers. A repeat, after a reconnect or from another
    client, gets the first outcome back and touches nothing. An intent the thread's caps lack
    is `Unsupported`, and so is any no adapter acts on yet (mode, compact, stopping a task).
    Messages, interrupts and models go to the composer (next entry). `Start` is refused and
    not recorded.
  - **Expansion asks the adapter.** The Claude driver answers an `Expand` from the session's
    own transcripts, on the blocking pool: a text cut at `EXPANDED_CHARS`, a picture's bytes,
    or `Gone`.

- ✅ **The worker types what is sent to an observed agent, under the draft guard**
  (2026-10-02, `crates/slopty-worker/src/thread/compose.rs`; tests in
  `crates/slopty-worker/tests/compose.rs` over a real terminal whose program records every
  byte, and `apps/slopty-worker/tests/threads.rs` over the link).
  - **The same keystrokes the face typed from the client.** A message is a paste (bracketed
    where the TUI asked), 200 ms, then Enter. A model is `/model <id>` typed, the pause, then
    Enter. An interrupt is Esc. All of it goes through orchestration's guard (`may_type`) at
    the moment it is written. Nothing is typed while the agent asks a person, before its hooks
    have spoken, once it has exited, or over a person's unsent line. Nothing it types clears
    that line.
  - **A message waits in the thread's pending list until it goes**, where a client shows it,
    edits it or withdraws it. Its entry leaves once the Enter is written. The item the agent
    makes of it carries the intent (`UserMessage::intent`): the host marks the first person's
    item whose words are the typed ones, so a client confirms its own bubble by id.
    - A steer goes as soon as the guard lets it, into the turn under way.
    - A queued message waits until the agent is at rest by its hooks, and then goes, one per
      turn. The next waits until the agent has taken the last one: it was seen working since,
      or the turn it started has ended. A queued message is never folded into the turn under
      way.
    - One the guard holds is `Held` with why, "Your draft in the terminal is in the way" for a
      person's line, and goes once the way is clear (tried again every 250 ms, since a draft
      says nothing when it is sent).
    - When a person starts a line between the paste and the Enter, the Enter is not sent and
      the message stays, saying it was typed and not sent. It is never typed again, and it
      cannot be edited, since its text is already in the terminal.
  - **An interrupt and a model are checked before they are taken.** Esc is refused over a
    draft, while a person is asked, and when the agent is not working. A model must be in the
    thread's catalogue (`ThreadMeta::models`). For Claude Code the catalogue is the aliases
    `/model` takes (`observed::MODELS`), so no client keeps a list of its own. `/model` waits
    for the agent to be at rest. Mode stays read-only.
  - **Each thread with something to send has one task**, which sends one thing at a time, so
    two messages never interleave in the terminal. It wakes on the thread's batches, on a new
    intent, and every 250 ms while something waits on the guard or the agent.

- ✅ **Every turn is snapshotted into private refs, and a review is a diff between two
  snapshots** (2026-10-02, `crates/slopty-worker/src/repo/snapshot.rs`,
  `crates/slopty-worker/src/thread/review.rs`; tests in `crates/slopty-worker/tests/review.rs`
  on a real repository, cost in `docs/MEASUREMENTS.md`, "a turn snapshot of this
  repository").
  - **A snapshot is the working tree as `git add -A` sees it**, written as a tree through an
    index of the thread's own, so the person's index, `HEAD` and stash are never touched.
    Untracked files are in and ignored ones out, so a Bash edit is caught as well as a tool's.
    The index is kept between snapshots, so each hashes only what changed since the last, and
    only the first starts from `HEAD`. A commit for each tree keeps it alive under
    `refs/slopty/threads/<thread>/<turn>-{before,after}`. The refs of a turn 256 turns back go.
  - **Taken at the edges, off the edge's path.** The host tells of a turn beginning or ending
    as the thread's last turn: a log read again from the start tells of old turns, and the tree
    is long past them. A turn's start is snapshotted at the earliest word of it, the agent
    starting to work, since the observed Claude path sees the turn itself only once the
    transcript has it. The snapshot follows as `Action::Snapshot`, one thread at a time and in
    order. A thread outside git takes none, and its review says why.
  - **A review compares two snapshots, "now" being one taken for it**: a turn (start to end,
    or to now while it runs), since a turn's start, or what is left after what the person kept.
    It is asked with `ThreadRequest::Review` and answered on the thread's stream as
    `ThreadFrame::Review`, from a task of its own so the thread's frames go on meanwhile. Each
    file carries its blob ids on both sides. Hunks are cut on the worker with three lines of
    context, the same way every time, so a hunk named by its place is the one the review
    showed. A review carries at most 20 000 diff lines, and files past that come without hunks.
  - **Keep and revert act once per id, checked against what the review showed.** A revert
    writes the file's old side back, whole or by hunk, only while the file is still the blob
    the review showed. Keeping moves the file, whole or by hunk, into a kept tree
    (`refs/slopty/threads/<thread>/kept`), only while what is kept of it is still the review's
    old side. "What is left to review" is the kept tree against now, starting from the
    thread's first snapshot. Both run git, so they are answered from a task of their own, and a
    repeat that comes while one is under way gets nothing until it is done.
  - **Every variant is appended last** (`ThreadRequest::Review`, `ThreadFrame::Review`,
    `Intent::Keep`, `Intent::Revert`), so no existing golden changed for them. The model
    catalogue (`ThreadMeta::models`, the composer's entry) did change the snapshot goldens.

- ✅ **What the worker adds to a thread outlives the thread being read again** (2026-10-02,
  `crates/slopty-worker/src/thread/host.rs`; `what_the_worker_owns_outlives_a_read_again` in
  `crates/slopty-worker/tests/threads.rs`). An observed thread is read again from its
  transcript after a worker restart, under a new epoch, and the transcript has none of what
  the worker added. The host keeps it beside the log and puts it back as the adapter tells
  the thread again: the intent each person's item came from, each turn's snapshots, and the
  pending messages. A message that was being typed when the worker went down may be in the
  terminal already, so it comes back held as typed and not sent, and is never typed again.
  The composer goes on with every thread's waiting messages when the daemon starts.

- ✅ **An observed thread is named by the session until its first prompt names it**
  (2026-10-02, `crates/slopty-agent/src/observed.rs`, `observed::Observed::title`). Claude
  Code paints the session's own name in the terminal title (`✳ Fix the flaky test`), so a
  thread started from a shell is named at once, and renamed as that name changes. The bare
  `Claude Code` names nothing. The first prompt that is not a command then names it, and
  keeps it.

- ✅ **A row carries what the client read from the old agent status** (2026-10-02,
  `ThreadState::row`, `observed::Observed`,
  `crates/slopty-worker/src/thread/claude.rs`; tests `a_row_says_what_runs_now`,
  `approvals_come_with_the_hooks`, and
  `a_claude_code_before_its_id_has_its_terminals_thread_until_the_id_comes` in
  `claude_threads.rs`). The client's reads of `AgentEvent`, `SessionAgent` and the permission
  prompts were listed, so that each could go to the thread table before the old path goes.
  Three had nothing to read from, and each is closed by one field or one adapter behaviour:
  - **What it is doing now.** `ThreadRow.doing` is the title of the newest tool call that runs
    or waits on a request ("Edit src/main.rs"), worked out by the reducer. Every adapter gets
    it with no work of its own, since the title is already its wording.
  - **Whether the hooks have spoken.** No new field. Claude Code's prompts are held only
    through the `PermissionRequest` hook, so the observed thread declares `approvals` once a
    hook is heard in its terminal (a status from a hook, a held prompt, or a hook the session's
    board counted). A Claude Code row without `approvals` is the one that offers to install
    the hooks. That follows the rule that a client shows a control only where the capability
    is.
  - **A Claude Code before its session id.** No new field. One started by hand and idle at its
    prompt has neither hook nor transcript yet, so the tracker sees it but nothing names a
    session. Its terminal names a provisional thread (`observed::terminal_thread`, an empty
    `native`). When the id comes, the session's own thread takes over and the provisional one
    is removed, not left exited, since it never was a session. It is removed too when the
    agent goes or the terminal closes first. Every agent tile so has a row.
  - **A first hook that asks.** A Claude Code whose first word is a `PermissionRequest` (no
    session start before it, its transcript still empty) had no thread to carry the prompt, so
    the person saw an empty tile and no card. The held prompt now opens the thread itself: the
    session's own when the hook names the id, else the terminal's provisional one. Every
    prompt held and not yet settled is told again to whichever thread begins after it, so the
    card survives the provisional thread giving way to the session's (test
    `a_first_hook_that_asks_opens_a_thread_with_its_request` in
    `apps/slopty-worker/tests/threads.rs`).
  - **One session id in two terminals.** A `--resume` of a session still running elsewhere
    puts two live Claude Codes on one id. The first terminal to claim it keeps the session's
    thread until its agent goes. The other observes a thread of its own
    (`observed::thread_in`), so no thread moves between terminals and each prompt lands on the
    terminal that asked it (test `two_terminals_on_one_session_id_keep_a_thread_each`).

- ✅ **The server ranks every thread on one ladder, and a notice goes where the person is**
  (2026-10-02, `crates/slopty-proto/src/thread/attention.rs`,
  `crates/slopty-server/src/hub/ladder.rs`; the tests in `hub/ladder/tests.rs`,
  `crates/slopty-server/tests/ladder.rs`, and `the_server_hears_the_workers_threads` in
  `apps/slopty-worker/tests/server_link.rs`).
  - **One rank from the row alone.** `Rung::of(&ThreadRow)` gives Needs you (a request open,
    or the phase says so), Failed, To review, Working, Waiting or Idle. The server and every
    client rank alike, and the words are the same on every surface.
  - **To review is the tree, not the agent's word.** After each turn's end snapshot, and after
    each keep or revert, the worker compares the tree with what the person kept, or with the
    thread's first snapshot when they kept nothing (`Action::ToReview`). That holds for every
    agent and every edit, a Bash edit included. Keeping every file clears it, and putting back
    something kept raises it again.
  - **The server ranks, because it holds every worker.** Each worker publishes its table
    (`ToServer::Threads`, the whole table on every registration, then each change). A subagent
    folds into the thread it hangs from, and never stands or notifies on its own. A subagent
    that needs the person lifts its parent at once. One that failed reads as working while
    anyone in its family still works or waits, since the parent may carry on without it, and
    lifts the parent to failed only once the whole family is at rest. Otherwise the parent
    stands as high as its highest subagent. The rest roll up per tile, worker,
    project node (a subtask into its task), project and fleet. Each roll-up counts every rung
    and names the thread to go to first, on the highest rung and there longest. The server
    ranks again on every row, terminal or project change, and sends the ladder only when it
    moved, whole each time, since rungs move far more rarely than rows.
  - **A workspace is the client's own**, laid out on the device alone, so the client folds the
    tiles it holds with `Ladder::over`. The fold is the same one the server uses.
  - **Presence routes the notices.** A person's client (`Role::Client`, never an agent's link)
    says where they are: `Seat::Desk` or `Seat::Handheld`, whether they are at it now, the
    workspace in front and the tiles on screen. A thread that hangs from no other and climbs
    to needing the person, fails, or comes to rest from working is a notice. Notices go:
    - to no client while its tile is on screen where the person is;
    - to the desks they are at, and never to a handheld beside them;
    - to the handhelds they hold when they are at no desk;
    - to every client when they are at none.

    A thread first seen at a rung (a worker that comes back) is no news. A finished notice
    carries how long the thread worked, from its first busy rung to its rest, by the worker's
    clock. The client shows it only when that is over its person's slow-command time. A
    notice a subagent raised names it (`Notice::via`). Every client hears who is where
    (`FromServer::Present`), and finds itself there by the number its `Welcome` gave its link.
    A client says where the person is through `ServerCaller::presence`, which sends nothing
    for the same again and tells every new link at once.
  - **The server's notice is the only trigger for an OS notification or an alert.** Once
    notices flow, a client shows only what it is handed and never decides on its own from the
    ladder or a status, so two devices never both alert for one thing.

- ✅ **A client mirrors each open thread, holds its intents in an outbox, and keeps both on
  disk** (2026-10-02, `crates/slopty-client/src/threads.rs`; the unit tests beside it and
  `a_thread_and_an_answer_outlive_a_dropped_link_and_a_relaunch` in
  `crates/slopty-client/tests/threads.rs`, against the real worker).
  - **The mirror runs the worker's reducer.** A snapshot replaces it; actions apply only when
    they start at its cursor. A frame that does not, or a stream that went with a link, is
    followed again from the cursor, with an unfollow first, since the worker ignores a follow
    of a thread it already streams. Each item keeps a revision, so a view measures again only
    the rows whose items moved.
  - **An intent shows until the thread's own record does, not until the worker answers.** A
    send is drawn as a bubble until the worker holds it as pending or the agent's message
    carries its id; an answer flips its card until the request is no longer open; a stop reads
    "Stopping" until no turn is active. Settling on the worker's answer alone would show the
    old state for the moment between the answer and the record. A send the worker turned down
    stays, with its words and why, until the person dismisses it.
  - **Unanswered intents go again under their first id** after a reconnect and after a
    relaunch, and the worker acts on an id once. Intents for a thread not open here settle by
    its table row.
  - **The cache is one directory per worker, the user's alone**: each thread's state with its
    cursor and the outbox, in postcard, replaced whole, at most the 64 threads last written. A
    cached thread draws in the view's first frame, read on the UI thread (0.5 ms for a 1.8 MB
    snapshot of 20 turns, `docs/MEASUREMENTS.md`), and catches up from its cursor. Every
    write runs off the UI thread, one batch after another, a thread two seconds after it
    rests.
  - **Expanded content is held by recency under 64 MiB**, and asked for once while it is on
    its way.

- ✅ **The thread view draws any agent's thread from the mirror, and an agent tile opens on
  it** (2026-10-02, `crates/slopty-ui/src/conversation/thread/`; the tests in
  `conversation/thread/tests.rs` and `an_agent_with_a_thread_opens_on_its_thread_view` in
  `workspace/tests/faces.rs`).
  - One reading column of 680 pt, prose at 15/1.6. A settled turn folds to its message, one
    line ("Worked 55 s: ran a command, read a file, 2 more") and its answer; the live turn
    never folds. The header says the title, the agent, the state word, the worker and, past
    20 %, the context ring.
  - The activity bar over the composer stacks the requests ("2 of 5", only a press answers
    one), the plan, the files the last turn edited with the way to their review, the messages
    waiting (taken back from here where the agent can queue) and the background work (stopped
    from here where the agent can). ↵ sends into the turn, ⌘↵ queues.
  - It stands beside the conversation face until that path goes: a tile whose terminal the
    worker's thread table names opens on its face on every device and draws the thread view;
    any other agent tile keeps the conversation face.

- ✅ **The review tile reads the worker's review frames, and every keep, revert and comment is
  an intent** (2026-10-02, `crates/slopty-ui/src/review/`; `review/tests.rs`). The scopes are
  the ones the worker answers today: the last turn (the default), since reviewed, and every
  turn; a branch, the unstaged changes and a task's span wait on the worker. Files go by
  weight, with tests, fixtures, snapshots, locks and generated code listed after the rest.
  "Mark reviewed" keeps every file shown, so "Since reviewed" is what changed after. Line
  comments are anchored by their line's text, dropped when it changes, and go as one message
  of `path L<n>: body` lines. A keep or revert the worker acted on asks for the review again.

- ✅ **The thread composer's menus never blank, and a waiting message is changed in place**
  (2026-10-02, `crates/slopty-ui/src/conversation/thread/view/composing.rs`;
  `thread/tests/composing.rs`). `/` lists the commands the thread says its agent takes, ranked
  as the conversation face ranks them; a source the client does not know ranks after plugins
  and is named as the agent named it. `@` asks the worker's file index under the thread's
  directory (`FindFiles`), the same one the palette asks. While the answer for the newest key
  is on its way, the last answer narrowed here stands in, so the list never empties between
  keys. Attachments go up as a drop on the tile does and the message carries their paths after
  its text; nothing goes while one is still uploading. A waiting message's pencil puts its
  words in the composer and the draft aside: ↵ sends `Intent::Edit` and brings the draft back,
  and Esc brings it back with nothing changed. The line shows the new words from the frame of
  the edit. A refused edit puts the old words back with the reason, and the person's words stay
  on the pencil until they dismiss it. If the message goes while it is being changed, its words
  stay in the composer as a new draft, with the draft that was put aside after them.

- ✅ **Codex: Slopty is one more client of the user's app-server, and the first answer to an
  approval settles it** (2026-10-02, verified against Codex 0.160.0;
  `crates/slopty-agent/src/codex/`, `xtask/src/codex/`; `crates/slopty-agent/tests/codex.rs`
  over `tests/fixtures/codex/approval.jsonl`).
  - **Types from the pinned build.** `cargo xtask codex schema` runs the official 0.160.0
    build's `codex app-server generate-json-schema --experimental` and turns the definitions the
    methods Slopty speaks reach into serde types: 12 client requests, the 5 approval and
    question requests, 27 notifications. The generator is xtask's own, about a thousand lines,
    because the bundle's shapes are few (objects, string enums, unions tagged by a property or
    by their one key, untagged unions, options, and an object that is also a union, read
    flattened). typify was the plan's choice, but it brings a dependency tree and its own
    naming for shapes that need neither. The file is checked in, so a Codex bump shows as a
    diff of its wire.
  - **The socket.** The daemon's control socket,
    `$CODEX_HOME/app-server-control/app-server-control.sock`, speaks WebSocket, one JSON-RPC
    message per text frame, without the `"jsonrpc"` field;
    `codex app-server proxy` only copies bytes, so a client has to speak WebSocket itself
    (tokio-tungstenite, the handshake only). The path in `CODEX_HOME` is a symlink to a socket
    in a short shared directory (`/tmp/codex-daemon-<uid>/<hash>`), because a socket's path
    must fit in 104 bytes on macOS. A client connects to the symlink's target for the same
    reason.
  - **A thread can be resumed only once its first turn has written it.** `thread/resume` of a
    thread with no turns yet fails with "no rollout found", so a second client joins after
    the first turn.
  - **Approvals with two clients, as recorded.** With Slopty and a Codex TUI both following one
    thread, the app-server sends `item/commandExecution/requestApproval` to both, under one
    request id, and both show `waitingOnApproval` in the thread's status. The first answer
    settles it. Both clients hear `serverRequest/resolved` for that id, and the turn goes on.
    A second answer, sent after that, gets no reply: the app-server logs "could not find
    callback" and drops it. So the face can offer the approval beside the TUI. Whoever answers
    first wins, and the other side's card is closed by `serverRequest/resolved` rather than by
    an error to its own answer.
  - **The recording.** `cargo xtask codex fixtures` runs the official build's app-server on its
    control socket in a scratch `CODEX_HOME` outside the repository, so no `AGENTS.md` or git
    state is read. The model is a canned Responses API on loopback, set up as a custom provider
    with retries off. Plugins and apps are off, so nothing is fetched. The policy is
    `on-request` in a read-only sandbox, and the model asks to run a command with escalated
    permissions. The paths, the host name, every UUID, the user agent and every time are
    scrubbed. The fixture tests read every recorded frame through the generated types. What
    either side sent reads back to the same JSON, and the routing above is asserted as
    recorded.
  - **The codec and the worker** (`codex/shared.rs`, `crates/slopty-worker/src/thread/codex.rs`;
    `crates/slopty-agent/tests/codex.rs` maps the recording, `apps/slopty-worker/tests/codex.rs`
    replays it to the real daemon from a stand-in app-server).
    - The worker joins the daemon's control socket under `$CODEX_HOME`, else `~/.codex`, and
      tries again every 2 s while there is none. It initializes as `slopty` with the
      experimental API, lists the loaded threads and resumes each. It resumes a thread the daemon
      starts later at `thread/started`, or at the thread's next status change while it is not
      yet resumable: the daemon tells every client of those, followed or not.
    - A Codex thread is one thread with drive `shared`, its id derived from Codex's. The turns
      a resume brings back are read first. Each of Codex's turns is the next of Slopty's, and
      an item keeps Codex's id. The status maps idle to done, failed or stopped by how the
      last turn ended, and `waitingOnApproval` to needs you, worded with the request once it
      opens.
    - A command or file approval offers Codex's `availableDecisions` in Codex's order: allow,
      allow for this session, always allow with the rule as its scope, deny, and deny and stop
      (`cancel`). The choice id is the decision's own name, so the answer sent is the TUI's own
      JSON-RPC answer, byte for byte as the recording has it. A permissions grant, a question
      or an elicitation opens as a request with no answers, for the TUI to answer.
    - **Two sides, one answer.** A request is settled only by `serverRequest/resolved`. It is
      named as answered from Slopty when this worker sent the first answer, else as answered by
      `Codex`. An answer to a request that is already settled, from a second client or after
      the TUI, is taken (`Outcome::Done`) and sent nowhere. The card shows who settled it. It
      is never an error, because nothing went wrong. In the narrow race where the TUI and
      Slopty answer within one round trip, Codex keeps the first it hears. Slopty may name
      itself then, since `serverRequest/resolved` does not say who answered.
    - A message goes as `turn/start`, or as `turn/steer` into the turn under way, carrying its
      intent as `clientUserMessageId`, which comes back as the user message's intent. An
      interrupt is `turn/interrupt`. Queueing is not offered yet (no `queue` capability).
    - Not carried yet: starting a Codex thread or its TUI from Slopty (`codex --remote`), the
      models list, images in a user message, and expanding a clipped output.

- ✅ **pi is driven over its RPC mode, with a gate that fails closed by pi's own rule**
  (2026-10-02, verified against pi 1.0.0, the latest on npm; `crates/slopty-agent/src/pi.rs`,
  `pi/rpc.rs`, `assets/pi-gate/gate.ts`, `xtask/src/pi/`; `crates/slopty-agent/tests/pi.rs`
  over `tests/fixtures/pi/gate.jsonl`).
  - **The gate.** pi has no permission system, so every pi Slopty drives loads one extension,
    embedded and written under the data directory by its digest as the Claude mod is. Its
    `tool_call` handler asks about each call through the RPC extension UI: a `select` whose
    title is the call as JSON (`slopty-gate/1`, the call's id, its tool, its arguments and the
    tool's own hints), with `allow` and `deny` as the options. Only `allow` lets the call run.
    A deny may carry the person's reason on the lines after it, and pi gives that reason to
    the model as the call's result. Anything else blocks the call: a dismissal, an abort, a pi
    run outside RPC mode. pi itself blocks a call whose `tool_call` handler throws, so a
    failure of the gate fails closed too. The extension only asks. What to ask the person and
    what to let through is the worker's to decide.
  - **As recorded.** pi says a call began (`tool_execution_start`) before the gate asks about
    it, so a call shows as running, then as waiting on the person. A steer sent while the gate
    waits is queued (`queue_update`) and goes after the call, as the next user message of the
    same run. An abort while the gate waits ends the call as "Operation aborted" and settles
    the run (`agent_settled`) before the abort's own response. pi writes `message_start` from
    the message it goes on filling, and each `message_update`'s usage from the usage it goes on
    adding to, so those hold some of what came after them, by timing. A message is built from
    its deltas and its `message_end`, its usage from the end alone.
  - **The recording.** `cargo xtask pi fixtures` installs `@earendil-works/pi-coding-agent` at
    the pinned version with bun under `target/pi/<version>`, its tarball checked against the
    registry's SHA-512, and runs it under node as its `bin` does. It runs with an environment
    of nothing but a scratch home, a scratch `PI_CODING_AGENT_DIR`, `PI_OFFLINE`,
    `PI_SKIP_VERSION_CHECK` and `PI_TELEMETRY=0`. Nothing is signed in, and no credential of
    this machine is in reach. The model is a canned Messages API on loopback, a provider of its
    own in the scratch `models.json`. The script is a first prompt with thinking, an allowed
    command with a steer queued at its gate, a denied one with a reason, and one interrupted at
    its gate, then the session's entries and stats. The paths, the canned model's address, the
    host name, every UUID, the session's entry ids and every time are scrubbed, and pi's
    system prompt is not kept. The starts and updates written by the race above are kept as
    pi begins them. Three recordings in a row are byte-identical. The tests read every record
    into the typed records, with none of an unknown kind. What went to pi reads back to the
    same JSON, and the gate's asks and answers are asserted as recorded.

- ✅ **pi on the worker: a thread started here runs the person's own pi, one writer per
  session, and nothing passes the gate but an answer it offers** (2026-10-02, verified against
  pi 1.0.0; `crates/slopty-agent/src/pi/driven.rs`, `crates/slopty-worker/src/thread/pi.rs`,
  `crates/slopty-testkit/src/bin/slopty-stub-pi.rs`; `crates/slopty-agent/tests/pi.rs` maps the
  recording, `crates/slopty-worker/tests/pi.rs` drives the stand-in end to end).
  - **The thread.** A pi session is one thread, its id derived from the session's id, with
    drive `driven`. A message from the person opens the next turn unless a run still works,
    when it steers that run and joins its turn. Each message streams from its deltas and is
    replaced whole at its end. The person's message `n` is item `msg-n` and the model's blocks
    `msg-n.i`; a tool call keeps pi's id. The same rule rebuilds the thread from the session's
    entries, so a thread read again is the one that was streamed, item for item.
  - **The session is named by the start.** `--session-id` with the start's intent id opens that
    session in pi's own directory or makes it, so starting a thread and taking it up again are
    the same words, and the person's own `pi --session <id>` finds it with their others. The
    plan had `--session <path>`, which puts the file outside pi's directory.
  - **Starting.** `ThreadRequest::Start` for agent `pi` starts it; every other agent is still
    refused there. pi is found on the daemon's `PATH`, else on the login shell's, and runs with
    that `PATH`, since pi is a Node program and its `node` is the person's too. Its version is
    what `pi --version` says. A start's own flags pass only from a list that loads no code,
    names no session and reaches no key (`slopty_agent::pi::SAFE_FLAGS`), and a value that
    reads as a flag is refused, since pi's parser may take it as one. A refused start is refused
    once per intent, as a started one is started once. The worker's facts list pi's version
    beside the other agents', which is what offers it.
  - **One writer.** The worker runs at most one pi per thread, and a thread's next pi starts only
    once the last is reaped. What was sent to a pi that was ending is handed back and goes to
    the next. A thread whose pi is gone is exited and resumable. The next message starts pi again
    on the same session, reads the thread again from `get_entries` before the message goes, and
    keeps each message's intent by its item. A session pi does not give back ends that pi with
    a notice, rather than numbering new items over old ones.
  - **Fail closed.** Only an answer the dialog offers reaches pi: the gate's allow, deny (with
    the person's reason) and deny and stop, which also aborts. When the worker goes, pi's stdin
    closes and pi ends with no answer sent, so a call at the gate never runs, and the thread
    shows the ask withdrawn, the call cancelled and the turn stopped. A worker that starts
    again cuts short from the thread whatever a pi of the last one was doing.
  - **Other dialogs.** An extension the person's pi loads may ask too. A choice or a yes or no is
    a question with the answers it offers; a text is a question answered in the person's own
    words. They stay open past the turn, since pi waits on them, until answered or until pi gives
    up at the dialog's timeout. A notice is a notice. Release is refused: while Slopty drives
    pi, there is no prompt of pi's own to hand a request to.
  - **The stand-in.** `slopty-stub-pi` replays a recording against what it is sent: a command
    that is the next step's gets that step's records under its own id, a query gets the
    recorded answer, and anything else is noted as unexpected and failed. It reads its fixture
    from a file beside the path it was started as, because the worker that starts it may be the
    test itself, whose environment is shared. The worker tests run the gate's recording through
    it: allow, deny with a reason, an interrupt at the gate, a resume from the session, and the
    worker going while the gate asks, each asserting what reached pi.
  - **Kept flags.** The start's safe flags, without the model ones, are kept as the thread's
    `pi-args` fact and re-checked against the safe list each time pi starts again on the session,
    so a resumed or taken-back pi keeps its tool list. The model is left to the session, which
    keeps its own.
  - **Not carried yet:** queueing a message (pi's follow-up queue has no way to take one message
    back), and a picture in a message.

- ✅ **pi's TUI takes a resting session on the person's word, and gives it back the same way;
  one writer holds it throughout** (2026-10-02; `Intent::Handoff`, `Intent::TakeBack`,
  `crates/slopty-worker/src/thread/pi/tui.rs`; `a_session_goes_to_pis_tui_and_comes_back` and
  `a_tui_the_person_ends_gives_the_session_back` in `crates/slopty-worker/tests/pi.rs`).
  - **Who holds it is in the meta.** Slopty holding the session is drive `driven` with no
    terminal; the TUI holding it is drive `observed` with the terminal it runs in and only
    `handoff` among the caps, so a client offers nothing else while the TUI holds it, and the
    worker refuses anything else with a reason.
  - **Handoff waits for rest.** The driven pi is told to end once no run works and no turn is
    open (`agent_settled` seen), never mid-turn. Only once it is reaped does the worker open a
    terminal running `pi --session-id <id>` plus the kept flags. The next task starts from the
    last one's end message, so the two never overlap. The stand-in takes a writer lock on the
    session file and the tests assert it never clashed.
  - **Following the TUI.** The TUI has no protocol, so the worker follows the session file pi
    writes: a stat every 250 ms and a read of only what it grew by, each new entry mapped by the
    same rule as `get_entries`. A file that shrank, or an entry off the last one (the person
    moved to another branch), reads the thread again whole. A worker that starts again with a
    thread held by a TUI follows it again, since the terminal outlives the worker's restart.
  - **Taking it back.** Take back waits until the TUI's session rests, then closes its terminal,
    waits for it to exit, and drives pi again, which reads the session from `get_entries` first.
    A TUI the person ends on their own leaves the session with Slopty, its pi exited and
    resumable, and the next message starts the driven pi on it.
  - **Rejected:** reading the TUI's screen to know when it rests (the session file says so, and
    Slopty never reads a screen to steer an agent), and sending `/quit` into it (keys go into an
    agent's TUI only on the person's word).

- ✅ **Any ACP agent is driven by one adapter, as the person's own program, asking before it
  acts** (2026-10-02, ACP v1 through `agent-client-protocol-schema` 1.10.2;
  `crates/slopty-agent/src/acp/`, `crates/slopty-worker/src/thread/acp.rs`; the tests in
  `crates/slopty-agent/tests/acp.rs` and `crates/slopty-worker/tests/acp.rs`).
  - **The types, not the SDK.** The `agent-client-protocol` crate brings an async runtime of its
    own beside tokio and pins an older schema. Slopty takes the schema crate alone, with no
    default features (no `schemars`, no unstable methods), and writes the framing itself: one
    JSON-RPC message a line, read as JSON first and classed by its members, so a method it does
    not know is answered or passed over, never a parse error that stalls the agent. The codec is
    sans-IO, like pi's, and the worker carries the lines.
  - **Which agents.** An open registry (`acp::registry`): the public ACP registry's agents, each
    with the command line that serves ACP on stdio, run as the person's installed program,
    never fetched. The person adds their own or replaces a known one by name and command line,
    and an empty command line takes one away. Claude Code, Codex and pi are left out, since each
    has an adapter of its own over a richer protocol. A thread's agent is `acp:<name>`, and
    `ThreadRequest::Start` for it starts the program found as the person's terminal finds it,
    in the thread's folder, with the daemon's environment. A start's own arguments are refused:
    the command line comes from the registry or the settings, never from a client.
  - **Slopty is a bare client.** `initialize` offers no file system and no terminal, so the agent
    works with its own tools. Its `session/request_permission` is a request on the call, with
    exactly the answers it offers (allow and reject, once or always, with the scope shown), and
    only one of those goes back. A request of any other method is refused with
    `method_not_found`. A cancel answers every open request as cancelled, as the protocol asks.
    An agent that answers that it needs signing in is left as it is, with a notice saying to
    sign in with the agent's own command in a terminal: Slopty never signs an agent in.
  - **Turns and items.** A prompt is a turn, opened by the person's message and ended by the
    prompt's stop reason (cancelled is stopped; refusal fails; the token and request limits end
    it with a notice). Streams are cut into items where the kind or the message id changes, and
    a tool call keeps the agent's id. A diff in a call's content is an edit or a write, with its
    patch. Plans, commands, modes, config options, usage and the session's title map to the
    thread's own fields. Anything else is kept as an `Extra` item rather than dropped.
  - **One message at a time.** ACP takes no message while a prompt runs, so the adapter declares
    `queue` and not `steer`: a message sent during a turn waits on the worker, can be withdrawn
    or edited there, and goes once the turn ends. A steer is refused as unsupported.
  - **Resumed by loading.** Whether the agent can load a session (`loadSession`) is kept as the
    thread's `acp-load-session` fact. A thread whose agent is gone is exited, resumable when that
    fact holds. The next message starts the agent again and sends `session/load`, and the
    agent's replay rebuilds the thread from nothing before the message goes. The replay is held
    until the load is answered, so a load that fails leaves the thread as it was, with a notice.
    A thread whose agent cannot load sessions refuses a message once it is gone.
  - **The stand-in.** `slopty-stub-acp` replays a recording against what it is sent, as
    `slopty-stub-pi` does: a request is the next step's by its method (a prompt by its words),
    an answer to one of the agent's own requests by that request's id and what it answers, and
    the agent's answers go out under the ids the client chose. It saves what it heard before it
    answers, so a test that sees the answer finds it said. The worker tests start
    `acp:opencode` with the stand-in on the `PATH` as `opencode`: a turn, an allow, a message
    queued behind an ask, a reject, an interrupt while the agent asks, a resume by loading, an
    agent that asks to be signed in, and the starts that are refused, each asserting what
    reached the agent.
  - **A failed agent is not started again on its own.** Messages held for an agent that failed
    (it could not open its session, or ended with an error) are dropped with the thread saying
    why, rather than handed to a fresh start that would fail the same way, over and over. The
    person's next message tries again.
  - **The fixtures are recorded.** `cargo xtask acp fixtures` downloads OpenCode 1.18.34 from the
    npm registry, checked against its SHA-512, and runs `opencode acp` in a scratch home with
    nothing signed in, its model list not fetched and its own providers off. Its model is a
    canned Messages API on loopback, set in the scratch config as the only provider, and every
    tool asks first. The recorder sends what the adapter sends and refuses what it refuses: a
    greeting with thinking, a write allowed once, a command rejected, a command cancelled while
    asked about, then the session loaded in a second process and prompted again. The lines are
    scrubbed (paths, OpenCode's time-ordered ids, times) into `turns.jsonl` and `load.jsonl`.
    Only `auth.jsonl`, an agent that wants a sign-in, is written to the schema by hand, since
    nothing signed out reaches it. `tests/acp.rs` holds every line to the schema's types and
    every message Slopty sends to the very JSON the agent was sent.
  - **What the recording showed of OpenCode.** It asks the client to write the file
    (`fs/write_text_file`) even though `initialize` offered no file system. The worker's refusal
    leaves the write to OpenCode, which makes the file itself. A write's diff appears only in the
    permission request, and the call's end replaces it with the tool's output, so the adapter
    keeps a diff once told. A new file's diff has an empty old text where the protocol has none,
    so an empty old text reads as a write. Its loaded session ends a rejected or cancelled call
    as failed, since nothing in the replay says why.
  - **The person's own agents.** `[worker.acp]` in the settings names an agent and its command
    line, or replaces a known one by name, and an empty list hides one. The worker reads it at
    each start, off the runtime, so an edit needs no restart.
  - **What a client can offer.** The worker's facts list the ACP agents installed under `acp`,
    by the registry's names (`acp.cursor`, `acp.amp-acp`, the person's own), each with the
    version its program says or `true` when it says none. Those names are the ones a thread's
    agent carries (`acp:<name>`), so what a client offers to start is what the worker will run.
    The `agents` fact had listed some of these by their program (`cursor-agent`, `amp`), which
    named neither the agent nor, for Amp, the program that speaks ACP; it now holds only the
    agents with adapters of their own (and `aider`).
  - **Not carried yet:** the client's offer itself (queued for the UI), pictures and files in a
    prompt, and the unstable methods (forking, subagents, session notices).

- ✅ **One answer answers all of a request's questions, and each adapter reads it as its agent
  takes it** (2026-10-02, `detail::Answer::{choice, read, parts}` and the doc of
  `Intent::Answer` in `slopty-proto`; tests `one_answer_answers_every_question`,
  `a_questionnaires_answer_is_the_answers_claude_code_takes` and
  `codex_questions_are_carried_and_answered_by_their_ids`).
  - **The encoding.** The questionnaire card sends one `Intent::Answer` for the whole form. For
    a lone question that offers nothing it is the words typed. Otherwise it is a JSON list of
    `detail::Answer`, one per question keyed by its text, with several picks of one question
    joined by `", "` and one's own words last, which is how Claude Code's own dialog joins them.
  - **Claude Code** takes the list as `AskUserQuestion`'s answers as they are.
  - **Codex** asks with `item/tool/requestUserInput`. Its request now carries the questions,
    each with the answers Codex offers and their descriptions, and the answer goes back under
    each question's id, the picks and one's own words split again by the labels offered
    (`Answer::parts`). An answer that leaves a question out, or names one not asked, is refused
    by the worker before it reaches Codex.
  - **A secret stays in the terminal.** A Codex question for a secret (`isSecret`) is not
    carried: an answer given in the GUI is kept in the thread's log. The card says to answer
    it in Codex's own terminal, which runs beside the face.
  - **The fixture is recorded.** `cargo xtask codex fixtures` starts a turn in Plan mode, where
    Codex offers the model its `request_user_input` tool. The canned model asks two questions,
    the recorder answers them by their ids as the adapter does, and Codex hands the answers back
    to the model as the tool's output (`tests/fixtures/codex/question.jsonl`). Its frames are
    held to the generated types, and a secret question is the recorded request with `isSecret`
    set.

- ✅ **The thread view carries what the conversation face showed, and its e2e moved with it**
  (2026-10-02, `crates/slopty-ui/src/conversation/thread/view/`;
  `crates/slopty-e2e/tests/app/conversation.rs`, `thread/tests/steps.rs`). A subagent's call
  opens its thread in the same view, under a bar that names it and leads back (Esc too), with
  the thread above kept followed and kept as the reader left it. ⌃O opens every step and folds
  them again. Commands run in the background sit over the composer with their last line, the
  running ones and those that ended in the last turn. Pictures sent with a message or returned
  by a call are drawn at the size their header gives, so a row keeps its height while the
  bytes come. A long message shows its start until "Show more". The working line shows only
  while the agent says it works: a turn whose end was never written is not under way once the
  agent is idle. Every behaviour the old face's e2e covered is now a thread-view e2e of the
  same behaviour, with the goldens renamed `thread-*`. The old face's code waits for S2.
