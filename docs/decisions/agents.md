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
      `Start`, `Intent`. Whether a client answers requests is said once, by
      `ConversationRequest::Approvals`, for every agent; the thread wire's own copy of it was
      removed on 2026-10-04;
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
    `/model` takes, as the mod lists them (`observed::MODELS` where it is not heard), so no
    client keeps a list of its own. `/model` waits
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
  - **Every adapter advertises `Cap::SNAPSHOTS`** (corrected 2026-10-04). The worker takes the
    snapshots, not the agent, so keep and revert are the worker's door for every thread whose
    folder is in git: observed Claude Code, Codex, pi and every ACP agent. Only observed Claude
    Code had the cap before, so keep and revert were refused on the others for no reason. Test:
    `a_pi_thread_puts_a_change_back_through_the_workers_snapshots`.
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

- ✅ **"Review with <agent>" runs the agent's own review over the change on show, and its
  findings become the tile's comments** (2026-10-04, frontier must-have M1; replaces the
  projects' reviewer stage). The research behind it is in
  `.research/feature-prune-frontier-2026-10-04.md` §6, with each agent's door checked against
  its own docs and source.
  - **The doors.**
    - Claude Code: its built-in `/code-review` skill. `/review` is now its alias, and the
      `code-review` plugin reviews GitHub pull requests only. It takes a ref range, so the
      worker writes the change the tile shows as two private commits under
      `refs/slopty/threads/<t>/review-{base,head}`: the old tree, then the new tree on it.
      `base...head` is then exactly that change. The command goes as the person's turn,
      through the composer like any command of theirs, only on their press.
    - Codex: `review/start` with target `commit` (the head commit) and delivery `inline`,
      Codex's own reviewer. Detached delivery is deprecated. Codex's `exitedReviewMode` text
      is now the agent's answer in the thread, where it used to be dropped.
    - pi publishes no review command and ACP no review method, so neither shows the action.
  - **The door is a capability.** `Cap::REVIEW` and `Intent::Review { from, to }` (two
    snapshot trees, as data). Codex always has it. Claude Code has it exactly while its own
    command list names `code-review` (`observed::REVIEW_COMMAND`). The button and the palette
    line show only where it is there.
  - **The private refs stay home.** Each thread keeps one pair, overwritten by each review,
    and an aside closed for good takes every ref of its thread with it (`Repo::forget`). A push
    names the person's branches, never `refs/slopty`, and no review commit is an ancestor of
    a branch. A test pushes `main` to a bare remote and finds neither the refs nor the commit.
  - **The findings.** While the agent reviews, the tile holds the change it asked about,
    though the thread moves on. Once the agent rests, with nothing pending and no background
    work, it reads every answer written after the request (`review::findings`).
    - The reading is tolerant: Codex's `- title — path:start-end` lines, and Claude Code's
      prose naming `path:12`, `path:12-18`, `path#L12-L18` or "`path` line 12".
    - A finding on a line in the diff becomes a comment under it, with the agent's mark and
      its title.
    - A finding with no line on show, or no place at all, is a note above the diff, never
      dropped.
    - The person lets any of them go, and sends the rest with their own comments as one
      message, or adds them to the draft.
    - A review that raised nothing says what the agent said of it. A refused one says why, in
      the error's tone.
  - Tests:
    - `codexs_findings_are_read_with_their_places_and_bodies` and the other cases in
      `review/findings.rs`.
    - `the_agents_findings_become_comments_and_notes_sent_as_one` and
      `findings_are_let_go_and_a_review_says_how_it_came_out` (`review/tests.rs`).
    - `an_agents_review_names_the_change_as_one_pair_that_never_leaves`
      (`slopty-worker/tests/review.rs`).
    - `a_review_by_claude_code_is_its_own_code_review_over_the_range`
      (`slopty-workerd/tests/threads.rs`).
    - `a_thread_reviews_while_claude_code_lists_code_review` (`slopty-agent`).
    - `a_review_asks_codexs_reviewer_and_its_findings_are_its_answer`
      (`slopty-agent/tests/codex.rs`).
    - `a_review_goes_to_codexs_reviewer_over_the_head_commit`
      (`slopty-worker/tests/codex.rs`).
    - The e2e `the_agents_own_review_puts_its_findings_on_the_diff`, with goldens
      `review-agent` and `review-agent-dark`.

- ✅ **The screen an agent drives opens beside its thread, watched until the person takes
  control** (2026-10-04, frontier must-have M2). The research behind it is in
  `.research/feature-prune-frontier-2026-10-04.md` §6.
  - **The link is the agent's own calls.** Every adapter already maps its agent's tool calls
    into the thread model, an MCP tool as its server and tool with its input, a command as its
    text. The host tells each call that runs in a thread's last turn (`Host::tools`), and
    `thread::screens` reads what it says of a screen. One worker module serves every agent,
    so no adapter grows a screen of its own. Nothing reads a screen to learn this, and nothing
    acts on one.
    - Computer use names its window outright. Claude Code's built-in `computer-use` server
      passes `window_id` to its `app_*` tools and says "Captured window_id" when it defaults
      one; `cua-driver` passes `window_id` and `pid`. A call that names an application is that
      application's largest window, and a call on the whole screen is the display.
    - A simulator's tools (a server for simulators or devices, `simctl` or a simulator
      destination in a command) mean the booted simulator named by id or by name, or the
      only one booted, as `xcrun simctl list devices booted -j` says. That is its window in
      Simulator, titled with the device's name. Another platform's device is no simulator.
    - A browser's tools (Claude in Chrome, Playwright, Chrome DevTools, `agent-browser`) mean
      the browser window titled with the page the tool reported. With no title, it is the one
      window of a browser only automation runs (Chrome for Testing, Chromium), never the
      person's own browser by guess.
  - **One fact on the thread.** `Action::ScreensSet` names up to three screens, the latest
    first, each with its capture target, an open kind (browser, simulator, desktop, app) and
    its label. A screen goes when its window closes or fifteen minutes after the agent last
    drove it. It is an action of its own because adapters re-send their whole meta, which
    would wipe a worker-set field.
  - **Watched, then taken over on the person's word.** The composer's toolbar offers the
    latest screen by its window's title (its mark alone in a narrow tile, the name in a hint),
    and the palette has "Watch the agent's screen". It opens as a window or display tile
    beside the thread, the item carrying the thread, so every client knows whose screen it
    is. While the agent drives, a pill at the foot says so. No move, click, key or scroll
    leaves the device, and a press does not raise the window. "Take control" stops the
    agent's turn under way through its own door (`Intent::Interrupt`) and gives the person
    the stream. "Hand back" watches again and sends nothing.
  - Tests:
    - `computer_use_names_a_window_an_application_or_the_display`,
      `a_browser_is_the_window_showing_the_page_the_tool_reported`,
      `a_simulator_is_the_booted_one_the_call_names` and the rest of
      `slopty-worker/src/thread/screens/tests.rs`.
    - `the_agents_calls_name_the_windows_it_drives` (`slopty-worker/tests/screens.rs`).
    - `an_agents_screen_is_watched_until_the_person_takes_control` (`slopty-ui/src/screen.rs`)
      and `the_screen_the_agent_drives_is_offered_beside_it` (`thread/tests/doors.rs`).
    - `the_screen_an_agent_drives_opens_beside_its_thread` (app e2e, over the drawn screen;
      goldens `agent-screen`, `agent-screen-dark`).

- ✅ **A line names the thread and turn that wrote it** (2026-10-04, frontier must-have M3).
  From a line of a file tile or a review, the person reaches the agent's thread at the turn
  that brought the line in.
  - **Blame over the turns' own snapshots.** The worker keeps the tree as each turn begins and
    ends (`thread::review`). Those snapshots, for every thread in one working tree and in the
    order the turns ended, make a history (`thread::authors`). Each turn becomes a commit of
    its after-tree. Where the tree changed between turns (the person's edits, another agent's),
    a commit of the next turn's before-tree comes first. `git blame --contents` of the file as
    it is now, against that history, gives each line the turn that brought it in.
    - The commits are made with fixed authors at the turn's own time (`Repo::commit_at`), so
      they are the same objects on every ask. No ref names them and the person's history never
      sees them. The history is kept per tree and grows a turn at a time. Asks are made one
      at a time per tree, since a review asks for many files at once.
    - A line from before the first snapshot, or from between turns, is blamed against the
      person's own `HEAD`. When that commit carries a `Slopty-Thread` trailer (a project merged
      it), the line is that thread's, with the commit and no turn. That holds on every machine
      with the commit, after the snapshots are gone. The thread may be another worker's; it is
      named when a worker here knows it.
    - A line changed since the last snapshot, and every line of a file larger than 4 MiB, is
      no one's.
  - **One answer per file, kept by the client.** `ThreadRequest::Authors` names a file,
    absolute or in a thread's repository. The answer (`Authors`) carries runs of lines, each
    with its thread, turn or commit, and when it was written. It also carries the file's
    modification time and blob, as the worker read it. The client keeps the last 64 answers
    (`slopty_client::threads::Authorships`) and draws from them on every frame. A file tile
    asks once per read (`FileView::stamp`, its modification time). A review asks for the file
    as its diff ends (its blob): the first 16 files listed as the review comes, the rest when
    the pointer first crosses one of their lines. No hover asks anything.
  - **Quiet until looked at.** A file tile names the caret line's author in a small pill in
    the corner of the text: the thread's title (its agent while untitled), the turn, and how
    long ago. The pill hides while the edit differs from the file as read, since its lines no
    longer match the answer. A review names a line's author at the line's end while the
    pointer is on it, as "Turn N" within the thread's own review. A press on either opens the
    thread where its agent's tile shows it, else in its own tile, and scrolls to the turn's
    first message (`ThreadView::go_to_turn`). A turn older than those held is reached by
    paging back.
  - Tests:
    - `every_line_is_the_turn_that_wrote_it` and `a_file_with_no_history_says_why`
      (`slopty-worker/tests/authors.rs`, on a real repository with two threads, the person's
      edits and a trailer's commit).
    - `blame_gives_each_line_its_commit` and `trailers_name_the_thread_that_made_a_commit`
      (`repo/snapshot.rs`).
    - `a_file_is_asked_once_per_change` (`slopty-client`).
    - `the_carets_line_names_the_turn_that_wrote_it` (file tile),
      `a_line_names_the_turn_that_wrote_it_under_the_pointer` (review) and
      `a_turn_gone_to_shows_from_its_message_paging_back_to_it` (thread view), all in
      `slopty-ui`.
    - The app e2e `a_line_names_the_turn_that_wrote_it_and_opens_it`, with goldens
      `file-author` and `file-author-dark`.
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

- ✅ **A client starts Claude Code and Codex threads too, each through the agent's own door**
  (2026-10-02, `crates/slopty-worker/src/thread/claude/start.rs` and `thread/codex.rs`,
  `slopty_agent::resume::started` and `codex::shared::start`; tests
  `crates/slopty-worker/tests/claude_start.rs`, `crates/slopty-worker/tests/codex.rs`, and
  `a_claude_code_start_opens_claude_in_a_terminal_and_names_its_thread` in
  `apps/slopty-worker/tests/threads.rs`). `ThreadRequest::Start` had refused both agents.
  - **Claude Code runs in a terminal and is observed.** The worker finds the person's own
    `claude` as their terminal finds it and opens it in one of its terminals, in the thread's
    folder. It goes through the worker's usual opening of any `claude`, so it gets the hook relay,
    the mod and Slopty's tools as a tile opened on `claude` does. The first message is Claude
    Code's own initial prompt, after `--` on its command line, and the model is its `--model`.
    Nothing is typed into the TUI, nothing is signed in, and a Claude Code that is not signed in
    says so in its own terminal. Arguments from a client are refused.
  - **Named before it speaks.** The worker chooses the session id and passes it with
    `--session-id`, so the thread's id (`observed::thread_of`) is known when the terminal opens.
    The observer begins the thread at once, with the terminal named, and the start is answered
    with it. The hooks and the transcript then fill it in as for any observed session, and the
    first message carries the start's intent once the transcript shows it. The provisional thread
    of a terminal whose session id is not yet known is not needed here: the id is never unknown.
  - **Codex is asked over its daemon.** The worker sends `thread/start` to the person's Codex
    app-server, as Codex's TUI starts a thread. Only the folder and the model are set, so the
    approval policy and the sandbox stay the person's own configuration's. The connection that
    starts a thread is subscribed to it, so the thread is followed from Codex's answer, and the
    prompt goes as its first turn (`turn/start`), marked with the start's intent. The TUI joins
    it as it joins any of the daemon's threads. A start asked before the handshake is held until
    the handshake is done. (A start while the daemon is not running was refused here; that is
    superseded by "A Codex start brings the person's daemon up" below.)
  - **Once.** Both are acted on once per intent id (`Host::record_start` for a thread its
    adapter begins on its own): a repeat gets the first outcome back and opens or asks for
    nothing.
  - **The stand-ins.** `slopty-stub-claude` is the person's `claude` on a `PATH` of the test's
    own. Its hooks now name the session it was started on, as Claude Code's do, so the relay's
    first hook is heard on the started thread. The Codex daemon is the recorded client that
    started a thread (`approval.jsonl`), replayed, with the person's message marked by the id the
    turn named, as Codex marks it.

- ✅ **Any agent a worker offers starts from the palette, and its thread opens as a tile of its
  own** (2026-10-02, `ItemKind::Thread` in `slopty-proto`; the client in
  `crates/slopty-ui/src/workspace/{faces,overlays,projects}.rs`; tests
  `the_palette_starts_each_agent_the_worker_offers` in `workspace/tests/thread_start.rs`,
  `a_claude_code_start_with_no_prompt_opens_in_the_home_it_names` in
  `apps/slopty-worker/tests/threads.rs`, the `worker_item_thread` golden, and the app e2e
  `crates/slopty-e2e/tests/app/threads_start.rs`).
  - **What a worker offers comes from its facts.** The client asks the server for every
    worker's facts (`Verb::WorkerFacts`) when a worker links and each time the palette opens,
    and maps the server's worker ids to its own keys. Claude Code, Codex and pi are offered when
    `agents` lists `claude`, `codex` and `pi`. Each ACP agent `acp` names is offered too. A
    program with no thread to start (`aider`) is not. With no server, nothing is offered, since
    only the server holds the facts. The `acp:<name>` naming moved into `AgentId` so the client
    builds it without linking the adapters.
  - **One line per agent, for the focused tile's worker and folder.** "New <agent> thread"
    starts it on that worker, in the folder a search from there would use: the focused shell's
    repository or directory, a file's or folder's own, else the worker's home as `~`. The
    worker expands `~` against its own home, since a client does not always know that home.
  - **No prompt.** A start from the palette carries no first message. Claude Code opens
    waiting for the person, and Codex, pi and ACP agents start a thread with no turn yet. What
    to ask is typed into the thread's own composer.
  - **The thread tile is the default and the terminal is one action away.** The started
    thread opens as an `ItemKind::Thread` tile drawing the thread view as an agent's tile does,
    and it takes the keyboard. A Claude Code thread's terminal does not open a second tile
    beside it. The view's "terminal" action reveals that terminal's tile, or adds one for the
    session when it has none. This follows the GUI-first direction: the person directs and
    reviews in the GUI, and the TUI stays able to take the session over.
  - **Refusals are said, never opened.** A worker that refuses a start (a missing binary, a
    Codex daemon not running) answers in words, shown as a notice, and no tile is added.

- ✅ **One answer answers all of a request's questions, and each adapter reads it as its agent
  takes it** (2026-10-02, `detail::Answer::{choice, read, parts}` and the doc of
  `Intent::Answer` in `slopty-proto`; tests `one_answer_answers_every_question`,
  `a_questionnaires_answer_is_the_answers_claude_code_takes` and
  `codex_questions_are_carried_and_answered_by_their_ids`).
  - **The encoding.** The questionnaire card sends one `Intent::Answer` for the whole form. For
    a lone question that offers nothing it is the words typed. Otherwise it is a JSON list of
    `detail::Answer`, one per question keyed by its text, with several picks of one question
    joined by `", "` and one's own words last, which is how Claude Code's own dialog joins them.
  - **The card is gpui-kit's questionnaire** (`conversation/thread/questions.rs`,
    `thread/view/asking.rs`; tests `thread::tests::questions::*`, e2e
    `an_agents_questions_are_answered_in_the_thread`). One question at a time under its header,
    each answer with its description, single or multiple choice, an "Other" field beside the
    choices and a field alone when there are none; digits, arrows, ↵ and ⌘↵ walk it. A new
    questionnaire takes the keyboard only from an empty composer, so a message being typed is
    never read as answers, and gives it back when it goes. "Answer in the terminal" shows only
    for a thread with a terminal.
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

- ✅ **The thread model carries what each agent's door publishes** (2026-10-02, audit in
  `.research/thread-completeness-2026-10-02.md`). The person wants the agent GUI as complete
  as T3 Code and Amp, for every agent. The audit lists each fact against each agent as carried,
  published but not carried, or not published; this pass carried the published rows that matter
  most to someone watching.
  - **A retry is said** (`Notice.retry`: attempt, max, wait). Claude Code, Codex
    (`willRetry`) and pi (`auto_retry_start`) say when they retry an API error. Without it an
    overloaded agent looked hung. Tests: `an_api_error_it_retries_says_the_attempt`,
    `an_error_codex_retries_counts_its_attempts`, `a_retry_says_its_attempt_and_its_wait`.
  - **Background work is listed** (`BackgroundTask` gained `kind`, `started_ms`, `ended_ms`).
    Claude Code's background shells and agents become `TasksSet`, keyed by the agent's own
    task id, closed by their end and dropped when the transcript drops them. Test:
    `background_commands_are_the_threads_background_work`.
  - **Per-turn cost is a usage key, not a field.** *Cut the same day: see "Dollar cost, the
    carried account and drafts on the worker are gone" below.* `Usage::COST_MICRO_USD` sat in
    the turn's open usage map, and `Usage::tokens()` left it out of the token sum. A new `Turn`
    field would have changed every literal of it for one number only some agents give.
  - **Effort is a meter** (`Meters.effort`): Codex's reasoning effort, pi's thinking level, ACP's
    thought-level option, each as the agent names it.
  - **Codex.** The turn's diff (`turn/diff/updated`) is the turn's changed lines. A reroute adds
    the model to the turn with a notice. Approval policy and sandbox are the thread's mode and
    a fact. A collab agent call is a subagent call that opens its child thread, and the child
    names its parent. Account rate limits carry no thread id, so the worker gives them to every
    Codex thread it follows and to each one it follows later. A thread's cost is Codex's own
    estimate (`account/usage/read` for the thread, its `estimatedUsageUsdMicros`), asked when
    the worker takes the thread up and after each turn ends, since Codex announces no cost. A
    plan billed only in credits gives no dollar figure, and the thread keeps none rather than
    a converted guess. Tests in `crates/slopty-agent/tests/codex.rs` and the worker's
    `tests/codex.rs` stand-in.
  - **ACP** turn tokens come from `PromptResponse.usage`, behind the schema's
    `unstable_end_turn_token_usage` feature, which this crate enables. It is an unstable part
    of the protocol, taken because it is the only door to a turn's tokens.
  - **A hook's folder outranks the terminal's.** A hook-only Claude Code thread had no folder,
    so its review said "not in a git repository" and its paths were absolute. The worker now
    takes `cwd` from `SessionStart` and every later hook, ahead of the terminal's process
    folder, and offers the folder's slash commands (`CommandsSet`). Test:
    `a_session_works_where_its_hooks_say`.
  - **A hunk is headed as git heads it** (`Hunk.heading` in both patch types). The rule is
    git's default for a file with no diff driver: the nearest line above the hunk that starts
    with an ASCII letter, `_` or `$`, cut at 80 bytes and then trimmed. It comes from the old
    file for the worker's snapshots and Claude Code's edits (`originalFile`), and from the
    `@@ … @@` line Codex writes. Test: `hunks_are_headed_as_git_heads_them` compares with
    `git diff --no-index`.
  - **An `AskUserQuestion` is a question, whichever hook brings it.** Claude Code answers it
    through the permission hook, so its `PermissionRequest`, and the permission prompt that
    follows, said "Wants to use AskUserQuestion". The tracker now reads both as
    `Blocked(Question)` with the question as detail. Test:
    `a_question_asked_through_the_permission_hook_stays_a_question`.
  - **Left:** Codex background terminals, hooks, MCP startup and rewind (not in the generated
    protocol yet), Claude Code `StopFailure` as failed and its per-turn cost, MCP servers as
    model state, pi commands and edit diffs, and ACP subagents and compaction.

- ✅ **A Codex thread reads like every other agent's** (2026-10-02, from the showcase).
  - A thread Codex has not named takes its first prompt as its title, as pi and ACP threads do,
    until `thread/name/updated` names it.
  - Codex runs every command through the person's shell (`/bin/zsh -lc '…'`). The call's title,
    its command row and the request's title show the command the shell runs, its quoting undone
    as a POSIX shell reads one word. A command that is not exactly one shell word after `-c` is
    shown as Codex sent it.
  - Codex begins a call before it asks about it, so the call is told again as waiting on the
    person while a request is open about it, and as running once the request is settled.
  - A request open here puts the thread in "needs you" whatever flags Codex sent with its
    status, and whether or not it said yet that the thread is active.
  - An ACP write of a new file is numbered from line 1, as git numbers a new file. The same
    holds for Claude Code's proposed `Write`.
  - Tests: `the_starter_sees_the_thread_and_the_approval_settled_elsewhere`,
    `an_open_request_waits_on_the_person_whatever_the_flags`,
    `a_commands_shell_wrapper_is_taken_off` and `the_session_maps_onto_the_thread`.

- ✅ **A Codex start brings the person's daemon up** (2026-10-04, rulings §3). A Codex start
  while no app-server daemon answers runs `codex app-server daemon start`. That is Codex's own
  lifecycle command, published for remote clients reaching a machine over SSH, and the one
  Codex's TUI itself runs at launch since 0.157.0. It runs the person's `codex`, found as their
  login shell finds it. The start waits while the command runs (up to 20 s, `DAEMON_WAIT`) and
  goes on once the daemon answers. The JSON object the command writes names the socket. A second
  start meanwhile is held with the first and starts nothing more.
  - **Refused in words, never retried.** No `codex` on the machine gives "Codex isn't installed
    on this machine". A start that fails is quoted as Codex wrote it, without its `Error:`
    label: "Codex's app-server didn't start: …". Nothing tries again until the next start.
  - **Only on a start.** A worker that only follows Codex never launches it, however long no
    daemon runs. A request handed to Codex's TUI while no daemon runs is still refused.
  - **No private server.** A `codex app-server` child of the worker would hold the thread
    outside the daemon, where the person's TUI cannot join it: a second writer.
  - **Resume is a start of the same thread.** A start whose arguments are `resume <thread>`
    (Codex's own words, `codex resume`) takes the thread up again with `thread/resume`. It is
    followed under the same thread id and answered `Started` with it, at once when it is followed
    already, and refused in Codex's words when Codex cannot load it. An exited Codex thread's
    Resume sends that start, so the daemon comes up and the thread comes back through one door.
  - **Left:** `InstalledAgent` names its agent by the closed `AgentKind`, which has only Claude
    Code, so detecting `codex` and `pi` in `caps.rs` waits on a wire change (an open `AgentId`
    there). Proposed to the owner of `slopty-proto`.
  - Code: `crates/slopty-worker/src/thread/codex.rs` (`Launch`, `Bringing`, `Begin`),
    `crates/slopty-agent/src/codex/daemon.rs`. Tests (`crates/slopty-worker/tests/codex.rs`):
    `a_codex_start_with_no_daemon_starts_the_persons_daemon_once`,
    `a_failed_daemon_start_is_refused_in_codexs_words`, `a_worker_alone_never_starts_codex` and
    `a_start_that_names_a_thread_takes_it_up_again`. A stand-in `codex` records how it was run.
    Also `daemon::tests::a_start_reads_as_its_socket_or_codexs_words`.

- ✅ **What an agent turns down is said, and Codex answers through its own doors** (2026-10-04,
  readiness #2, #8, #9, #14).
  - **Refusals stay in words.** The client's outbox settles a failed intent at once, so a
    refused stop, model switch, mode, compact, answer, release, Keep or Revert used to vanish
    without a word. The thread hub now keeps each one, other than a message or an edit to one
    (those keep their own line), as "Couldn't stop: …" or "Couldn't keep lib.rs: …". It stays
    until the person dismisses it or tries again, and holds up to 32. An intent the agent has no
    door for reads "the agent can't do that through Slopty".
  - **Attachments are refused in words where they cannot go.** A file reaches an agent only
    through a terminal its prompt runs in. On a thread with none (Codex, pi, ACP), attaching
    says "Files can't be attached to {agent} threads yet" and adds no chip. ↵ while a file is
    still uploading waits for it ("Sends once the attachments are up") and sends by itself when
    it lands. A failed upload sends nothing and says so, keeping the words.
  - **Codex queues.** A message queued while a Codex turn runs is held by the thread's mapping
    (`Shared::send` → `Send::Held`), shown as pending, changeable and withdrawable. It goes as the
    next `turn/start` once Codex says the turn ended. Codex has no queue of its own, so the
    thread now offers `Cap::QUEUE`.
  - **Codex's forms.** An MCP server's form (`mcpServer/elicitation/request`, mode `form`) is a
    request whose questions are its fields, in the order of their names. A boolean is Yes or
    No, an enum is its titles (several for a list), and text and numbers are words. The answers
    go back as `{"action": "accept", "content": {…}}`, each value of its field's type, and an
    answer that does not fit its field sends nothing. Decline and Cancel are the denies. A page
    to open or a device check stays with Codex's TUI.
  - **"Answer in Codex".** A Codex request with nothing to answer in the GUI (a secret, a
    grant) is released to Codex's own TUI. The worker runs `codex resume <thread>` in one of its
    terminals, which joins the running daemon as one more client, and names that terminal on the
    thread while it runs. The client brings it into view once it is named.
  - **Deny with a reason.** "Deny…" turns the answers into a field. The words go with the plain
    deny as the answer's message. Claude Code and pi take a denial's reason. Codex and an ACP
    agent hear it as the person's next message (a steer).
  - **Resume through each agent's door.** Claude Code: a start of `claude --resume <session>`
    in a new terminal, on the same thread. It is refused when that session already runs or the
    id is not a session. Codex: a start of `resume <thread>`, as above. pi and an ACP agent that
    loads sessions: the next message, which the composer says. An agent that cannot load its
    session says it cannot.
  - **Compact and modes.** `/compact` is listed where the thread has `Cap::COMPACT` and the
    agent lists no such command, and sends `Intent::Compact`. No wire field lists an agent's
    modes, so a thread with its own TUI but no `SET_MODE` says the mode is changed there.
    Proposed wire change: `ThreadMeta.modes`.
  - **"Machine", not "worker",** in what these surfaces say (rulings §8).
  - **Left:** "Edit…" on an edit approval (rulings §5a) needs `Verdict::AllowEdited { input }` in
    `slopty-proto`. Driving Claude Code for API-key users (§5c) is not built. Uploads to threads
    with no terminal are still to come.
  - Tests: `conversation::thread::tests::doors::*` and the attachment, queue and `/compact` tests
    in `tests::composing`; `review::tests::a_refused_keep_says_why_on_its_hunk`;
    `codex::form::tests::*`; `a_queued_message_waits_for_the_turn_and_goes_as_the_next` and
    `an_mcp_form_is_answered_as_its_content` (`crates/slopty-agent/tests/codex.rs`);
    `an_exited_thread_is_resumed_on_its_own_session_in_a_new_terminal`
    (`crates/slopty-worker/tests/claude_start.rs`).

- ✅ **A machine's own link says which agents it can start, with no server** (2026-10-04,
  readiness N11). This replaces "What a worker offers comes from its facts" above: with no
  server linked, nothing but Claude Code could be started, and the worker's capabilities found
  only `claude`.
  - *Wire.* `InstalledAgent` names its agent by the open `AgentId` its threads carry
    (`claude-code`, `codex`, `pi`, `acp:<name>`), not by a closed kind. Golden
    `machine_hello_ack` (`tests/golden_machine.rs`).
  - *The worker.* `caps::installed_agents` reads the same `agents` and `acp` facts the server is
    told (`facts::agents`): Claude Code, Codex and pi where their program answers `--version`,
    then each ACP agent of the registry or of `[worker.acp]` whose program is there. A program
    with no adapter (`aider`) is a fact but not startable.
  - *The client.* The palette's "New <agent> agent" lines and the start picker read the
    machine's capabilities as its link last said them, and an open palette takes a change at
    once. The server's `WorkerFacts` question is gone from this path.
  - *A start shows at once.* "Starting Codex on studio in ~/x…" is said as the start is sent,
    and an answer that is neither a thread nor a refusal (`Unsupported`) is said in words rather
    than logged.
  - Tests: `the_agents_found_are_what_a_thread_can_be_started_of` (`slopty-worker::caps`),
    `a_started_thread_opens_as_a_tile_and_a_refusal_is_said` and
    `an_open_palette_takes_the_agents_as_they_arrive` (`workspace/tests/thread_start.rs`).

- ❌ **An edit can be allowed as the person changed it, through the hook's `updatedInput`**
  (2026-10-04, rulings §5a). *Deleted the same day: see "Sleep, waits on another thread, queue
  reordering and edited allows are gone" below.* The
  hooks reference documents `updatedInput` on a `PermissionRequest` allow as the call's changed
  input, which Claude Code checks against its rules again before it runs it.
  - `PermissionPrompt::editable` and `Request::editable` carry the parts of the call the person
    may change: an `Edit`'s `new_string` and a `Write`'s `content`, each up to
    `Editable::TEXT_MAX` (256 KiB). A longer one is allowed or denied whole.
  - The answer is `Verdict::AllowEdited { input }` on the conversation path, and on the thread
    path an `Intent::Answer` whose choice is `Editable::choice`, a JSON object keyed
    `allow-edited`, which no option id, answer list or typed words can be.
  - The relay lays the edited fields over the call's own input, taking only a field offered
    for editing that the call holds as text. An edit can neither add a field, point the call at
    another path, nor change one that is not text, and words that are not a JSON object of
    texts leave the call as asked.
  - *The tray.* "Edit…" stands after the plain allow where the request offers a part. It puts
    each part in a field in the code face, in the answers' place, the first taking the
    keyboard. "Allow edited" sends them as they are now, and Cancel brings the answers back.
    It is a field rather than Slopty's diff editor: the change is one call's text, and the
    proposed diff stays drawn above it.
  - Tests: `edit_then_allow_sends_the_persons_text` (`conversation::thread::tests::doors`),
    `only_an_approval_with_parts_and_a_plain_allow_is_editable`,
    `an_edited_allow_carries_the_edited_input` (`slopty-agent::permission`),
    `an_edited_allow_reads_back_and_nothing_else_does` (`slopty-proto::thread`), goldens
    `machine_permission_editable`, `machine_answer_edited` and `intent_answer_edited`.

- ✅ **A thread lists the modes its agent publishes** (2026-10-04). `ThreadMeta::modes` (id,
  label, the agent's own description) answers the proposed change above. An ACP agent's session
  modes, or its mode config option, fill it. Claude Code, Codex and pi publish none on their
  doors, so theirs stay empty and the TUI changes their mode. Where the thread can switch
  (`Cap::SET_MODE`) and lists modes, the mode chip names the mode by the agent's own label and
  opens them as a menu, each with the agent's description; a pick sends `Intent::SetMode`, and
  the chip reads as the agent then says. Golden `frame_snapshot`; test
  `the_mode_chip_switches_the_agent_s_mode`.

- ✅ **A start opens its tile at once, and its first message goes as the start's prompt**
  (2026-10-04, readiness N11). A start drew nothing until the machine answered, and a start
  from the palette carried no prompt, so the agent booted idle and the task was typed after.
  - The last step of "New agent…" opens the thread's tile at once, focused, with a field
    asking "What should Codex do?" and where it will run. Nothing goes to the machine until ↵.
    What was typed goes as `Start::prompt`, so the first turn begins as the agent boots, and ↵
    on nothing starts it bare. This is the empty workspace's question, in the tile itself.
  - Once sent, the tile says "Starting Codex" and where. The thread's item lands under the
    tile's own id, so the thread takes its place with nothing moving, and its composer takes
    the keyboard. A refusal or an answer with no thread takes the tile away and says why.
  - The tile is the layout's alone until then (`workspace/starting.rs`): a worker's snapshot
    keeps it, ⌘W closes it, and a link that drops after the start was sent takes it away and
    says the answer may never come. A start not sent yet keeps its field.
  - Tests: `a_started_thread_opens_as_a_tile_and_a_refusal_is_said`,
    `a_start_on_its_way_closes_and_goes_with_its_link` and
    `new_agent_opens_the_picker_with_the_last_choices` (the prompt) in
    `workspace/tests/thread_start.rs`.

- ✅ **Files go with a message, landed outside the work** (2026-10-04, readiness item 2). A file
  dropped or picked in the composer is uploaded first, over the ordinary transfer, to
  `~/.slopty/drop/<transfer>/`. That needs no terminal and puts nothing in the agent's working
  tree, where it would show in its diff, be committed by accident, or be lost to a `git clean`.
  `Intent::Send` then carries the worker paths, and the worker reads each file only as the
  message goes.
  - A file counts as a picture by its first bytes (PNG, JPEG, GIF, WebP) and only up to 20 MiB.
    Anything else, and a picture that cannot be read, goes by its path, so the message is never
    lost for a file. At most 32 files go with one message, each an absolute path to a file on
    the worker, checked when the intent is decided.
  - Each agent gets the files the way it takes them. Codex gets the words, then each file's
    path, then a `localImage` per picture, which Codex reads itself. pi gets the words with the
    paths after them and each picture in `images`. An ACP agent gets each picture as an image
    block with its bytes and where it is, when it says it takes pictures (`promptCapabilities.image`).
    Every other file goes as a resource link. Claude Code's TUI is typed the words with the
    paths after them, shell-quoted, since its prompt reads a pasted path as a file.
  - The words in the thread stay as the person wrote them. The paths ride on the pending
    message, so a queued message keeps its files.
  - Tests: `pictures_are_read_whole_and_the_rest_go_by_path`, `only_files_here_are_taken`
    (`slopty-worker::thread::attach`), `a_picture_goes_to_pi_with_the_message`,
    `a_picture_goes_as_an_image_and_a_file_as_a_link`,
    `files_go_by_path_and_a_fork_and_a_listing_go_to_codex`, golden `intent_send_attachments`.

- ✅ **Past sessions per folder, from each agent's own record** (2026-10-04, readiness item 3).
  `ThreadRequest::Sessions` asks one agent's sessions in one folder and is answered with
  `WorkerMsg::Sessions`. Each session carries the words a start takes it up again with, and the
  thread already kept of it, if there is one. When nothing could be had, the answer says why in
  words instead of showing an empty list.
  - Codex is asked over its app-server (`thread/list`, by folder, last updated first).
  - An ACP agent is asked by a short run of its own (`session/list`, paged), when it lists
    sessions. A session is offered to take up again only when the agent loads sessions.
  - pi's session directory for the folder is listed. A session is called by the last name
    given it, else its first message, read only from the first and last 64 KiB of its file.
  - Claude Code's sessions are its project directory's transcript names and times alone. No
    transcript is opened: what the person said to Claude Code is theirs, and what Slopty shows
    of a session is what its own thread of it holds. The tests write their own project
    directory under a temporary home.
  - The words that take a session up again are each agent's own: `--resume <id>` for Claude
    Code, `--session <id>` for pi, and `resume <id>` for Codex and ACP. A start with them finds
    the thread kept of that session before making a new one.
  - Tests: `past_sessions_are_named_by_their_files_alone` (`slopty-agent::discover`),
    `a_past_session_is_listed_and_taken_up_again` (pi),
    `a_fork_and_a_listing_go_to_the_agent` (ACP),
    `past_threads_are_listed_with_the_words_that_resume_them` (Codex), goldens
    `client_sessions`, `link_worker_sessions` and `link_worker_sessions_absent`.

- ✅ **Fork, as each agent forks** (2026-10-04, readiness item 3). `Intent::Fork` branches a new
  thread off one, through a turn or the whole of it, and is answered once per intent like a
  start. It needs `Cap::FORK`, which a thread has when its agent can fork.
  - Codex forks over its app-server (`thread/fork`), through any turn that has ended.
  - An ACP agent forks in a fresh run of itself (`session/fork`, behind the schema's unstable
    feature, offered when the agent says it forks). The fork is then loaded, so its history is
    replayed into the new thread.
  - pi starts on a new session named by the intent, copying the old one (`--fork` with
    `--session-id`). `--fork` is passed on that first run only and never kept with the thread's
    flags, so every later run opens the copy.
  - Claude Code opens in a new terminal with `--resume <id> --fork-session --session-id <new>`.
    It needs a first message, since Claude Code writes the conversation only then.
  - pi, ACP and Claude Code copy whole sessions, so a fork from an earlier turn is refused in
    words. A fork of the whole thread records the last turn the two share.
  - Where a thread came from (`ThreadMeta::forked_from`, origin `fork`) is kept by the host as
    well as by the adapter. An agent's own account of the thread that does not say it, such as
    a reload or a worker restart, has the host's record put back.
  - Tests: `a_fork_copies_the_whole_session_into_a_new_thread` (pi),
    `a_fork_and_a_listing_go_to_the_agent` (ACP),
    `files_go_by_path_and_a_fork_and_a_listing_go_to_codex`,
    `a_fork_names_codexs_turn_and_a_forked_thread_says_where_it_came_from`,
    `a_fork_resumes_into_a_new_conversation` (Claude Code's words), goldens `intent_fork` and
    `intent_fork_whole`.

- ✅ **Codex threads at rest are let go** (2026-10-04). The worker kept every Codex thread it
  followed subscribed, so Codex kept each one loaded with its MCP servers until the worker
  restarted: about 29 MiB per thread for Slopty's relay alone (`docs/MEASUREMENTS.md`,
  2026-10-04).
  - A thread is let go (`thread/unsubscribe`) when all of these hold:
    - it has rested 10 minutes, with no turn under way;
    - nothing is asked of the person and no message is held;
    - Codex does not say it works on it;
    - no client follows it and no TUI of Slopty's runs on it.
  - Codex then unloads it after its own delay. The thread stays in the table as it was.
  - A follow, anything asked of the thread, or Codex saying it works on it again takes it up
    again with `thread/resume`. What was asked meanwhile goes once it is followed. Codex
    saying it unloaded the thread takes nothing up again.
  - Tests: `a_rested_thread_nobody_follows_is_let_go_and_taken_up_again` (stand-in daemon)
    and `what_rested_codex_threads_keep_followed_and_let_go` (the real `codex app-server`,
    run by hand).

- ✅ **A Codex thread's commands claim no Slopty terminal** (2026-10-04). Codex's shared
  daemon runs every thread's commands with its own environment, which is that of whatever
  started it. A Codex TUI typed in a Slopty shell can start it, and then every thread's tools
  carried that shell's `SLOPTY_SESSION`, its token and its project task, so Slopty's CLI in
  any Codex command spoke as that one terminal.
  - Every thread Slopty starts, takes up again or forks is now loaded with
    `shell_environment_policy.set` for those four variables, each set empty through the
    app-server's published per-thread `config`. Each is its own key, so the person's own
    policy is added to, never replaced. Set empty, they name no terminal, and the CLI speaks
    as an agent, never for the person.
  - A daemon the worker starts gets none of them.
  - Codex ignores `config` for a thread already loaded, and a thread the person's TUI starts
    gets none. So a `codex` typed in a Slopty shell runs without the four variables too: the
    shell integration wraps it in `env -u` in zsh, bash and fish, as it wraps `claude`, and the
    daemon it starts has no terminal to lend. A `codex` of the person's own is left alone.
    Test: `a_typed_codex_runs_as_no_terminal` (a stand-in `codex`).
  - Tests: the start, resume and fork parameters in
    `a_start_asks_the_persons_codex_for_a_thread_and_sends_its_first_turn`,
    `a_rested_thread_nobody_follows_is_let_go_and_taken_up_again` and
    `files_go_by_path_and_a_fork_and_a_listing_go_to_codex`. The measurement above checks
    that the real `codex app-server` takes the `config`.

- ✅ **A turn a usage limit stopped fails until the limit resets** (2026-10-04). A limit used
  to end Claude Code's turn as done, and Codex's limit error was a plain API error.
  - The turn's state `Failed` carries `until_ms`: when the agent can go on, where it is known.
    Its notice is of kind `limit`.
  - Claude Code records the API error it gives up on with its kind (`error: rate_limit`) and,
    for a usage limit, the quota it hit (`quotaLimits.resetsAt`). The decoder keeps both on
    the note (`Stop`). A turn whose last word is that error fails with its text. A turn the
    model went on in after an error completes. A limit with no quota recorded resets when the
    status line's full window does.
  - The main thread's done after a failed turn is a failure (`Phase::Failed`), whichever of
    the hook and the transcript is heard first.
  - The terminal's agent status says it too: `StopFailure` is `AgentStatus::Failed { error,
    until_ms }`, at rest like done and with the same attention. For a `rate_limit`, `until_ms`
    is when the full windows the status line last showed reset. Every reader that waits for an
    agent at rest takes it as one. Tests: `a_stop_failure_fails_the_turn_until_a_limit_resets`,
    `the_status_maps_to_a_phase`, and the golden `worker_agent_failed`.
  - Codex's `usageLimitExceeded` and `rateLimitExceeded`, once Codex gives up, fail the turn
    until its full window resets (`account/rateLimits/updated`).
  - Tests: `a_turn_a_usage_limit_stopped_fails_until_it_resets` (Claude Code and Codex) and
    `a_stopped_turn_fails_by_what_stopped_it`.

- ✅ **A queued message can be moved in the list or sent now** (2026-10-04, from the T3 Code
  study's queue controls). Two intents on the thread wire, for the UI to offer later:
  - `Reorder { pending, before }` moves a message that has not gone to just before another,
    or to the end (`Cap::QUEUE`). Queued messages go in the list's order. One rule serves every
    adapter (`Pending::reorder`): a message or mark no longer in the list is refused and moves
    nothing.
  - `Promote { pending }` sends a queued message now, as a steer would go (`Cap::STEER`). For
    Claude Code it becomes a steer in the list and is typed at the next step the guard allows.
    For Codex it leaves the queue and goes by `turn/steer` into the turn under way, or as a
    turn of its own when none runs. ACP takes no steer, so it is unsupported there. A message
    being typed, or already in Claude Code's terminal, is refused.
  - Tests: `a_queued_message_moves_in_the_list_or_goes_now` (the composer on a real
    terminal), `a_held_message_moves_in_the_queue_or_steers_the_turn` (Codex),
    `an_acp_thread_runs_turns_and_asks_before_it_acts` (ACP), and
    `a_message_moves_before_another_or_to_the_end` with the property `a_reorder_is_a_move`.

- ✅ **The person's past prompts are searched on each machine, and a hit takes its session up
  again** (2026-10-04, from the Ghostex study's first idea). `ThreadRequest::Sessions` grew
  rather than a second request: an agent and a folder are each optional, and it carries the
  words to find. With no words and both named, the agent lists its own sessions as before.
  Otherwise the worker answers from the person's prompts as each agent records them, best match
  first, and each session comes with the prompts that matched (`PromptHit`: the prompt cut to
  600 bytes round its first match, the matches as byte spans) and the words that take it up
  again. With no words, the sessions come prompted last first, each with its last prompt. A
  client asks every machine and merges the answers by time.
  - **What is read.** Claude Code's own prompt history (`~/.claude/history.jsonl`), with a long
    paste put back from the line or from `paste-cache/<hash>.txt`. This widens the earlier rule
    that no transcript is opened: the history is the record Claude Code keeps of what the person
    typed for its own ↑ recall, and transcripts are still never read. A session's transcript is
    looked at, never opened, to tell whether it can be taken up. Codex's prompt history
    (`$CODEX_HOME/history.jsonl`) holds what its TUI was sent; the folder comes from the first
    line of the thread's rollout. A thread another client of Codex's app-server ran is in no
    history, so its prompts are read from its rollout: the `user_message` events where it has
    them, else the `user` input items without the blocks Codex adds before a first turn. `exec`
    runs and subagents are left out, as nobody's prompts. pi's prompts are the person's
    messages in its session files. ACP agents keep no prompt record Slopty can read, so a search
    of one says so in words.
  - **What is no prompt.** A slash command on one line (`/model opus`) is left out; a path that
    begins a prompt is not one.
  - **The match.** Each word must be in the prompt as written, case ignored unless a word has a
    capital, accents ignored (`nucleo-matcher` substring atoms). Prompts are prose, so fuzzy
    letters in order would find nearly every long prompt for a short query. A word at a word's
    start scores above one inside a word; equal scores go to the newer prompt. Each prompt keeps
    its text folded as the matcher reads it, and one that lacks a word is passed over before it
    is scored: a warm search for a rare word went from 45 ms to 3–5 ms on this machine.
  - **Bounded and cached.** The worker keeps every record it read in memory with how far it read
    it. The records are only appended to, so a later search reads only the new whole lines, and
    a file replaced at its path is read again. A file is read from at most its last 64 MiB, a
    search reads at most 512 MiB and for 3 s, and a prompt is kept to 32 KiB. What a search
    skipped is said in `PastSessions::cut`, and the next one reads on. The numbers on this
    machine are in `docs/MEASUREMENTS.md`, "Prompt search".
  - **Resume.** A hit carries the words its agent's own resume takes (`--resume <id>`,
    `resume <id>`, `--session <id>`) and the thread kept of it, if one is, so opening a hit goes
    through the start path that already takes a session up again. A Claude Code session whose
    transcript is gone, or a Codex thread with no rollout, has no words and is shown only.
  - **Not yet.** The palette's search across machines, `slopty prompts` and an MCP tool are the
    client's half and come next. Claude Code driven over stream-json may not write its prompt
    history; that is checked when the driven drive is used.
  - Tests: `claude_codes_history_gives_prompts_with_their_pastes`,
    `codex_gives_prompts_from_its_history_and_rollouts`, `pi_gives_prompts_from_its_session_files`,
    `a_slash_command_is_no_prompt` (`slopty-agent::history`);
    `a_record_is_read_on_from_where_it_stopped_and_again_when_replaced`,
    `sessions_rank_by_their_best_prompt`, `a_bounded_search_says_what_it_skipped_and_reads_on`,
    `an_agent_without_a_record_says_why` (`slopty-worker/tests/history.rs`);
    `past_prompts_are_found_across_agents_with_the_words_that_resume_them` (the worker daemon);
    goldens `client_sessions_search` and `link_worker_sessions_found`.

- ✅ **A server task's thread starts at its seat, through each agent's own door** (2026-10-04,
  for the orchestrator's `StartThread`). The server picks a seat (a session id) for the task and
  hands it with the task's variables and a role (`TaskThread`). The worker starts the thread as
  `Seated`: the intent is derived from the seat, so a start repeated after a dropped link
  answers with the thread the first one started. The thread's row carries the seat as the fact
  `slopty.seat`, which the host re-applies over whatever the adapter later says of the meta, so
  it survives a restart and a fresh read from the agent.
  - **The variables.** The seat's variables are the server's (project, task) and the worker's
    for that seat (the server, the seat as `SLOPTY_SESSION`, its token), built by
    `Worker::seat_env` exactly as a terminal opened at the seat gets them.
  - **Each agent's door.**
    - Claude Code runs in a terminal opened under the seat id, so its hooks and tools are the
      seat's as for any terminal. The role goes as `--append-system-prompt`.
    - Codex gets the variables in the commands it runs (`shell_environment_policy.set`) and
      Slopty's tools as an MCP server in the `thread/start` config. The values go in the
      server's own `env`, since the daemon's environment belongs to no seat. The role goes as
      `developerInstructions`, and both are given again on `thread/resume`.
    - ACP gets `slopty mcp` with the variables in the `mcpServers` of `session/new`, `load` and
      `fork`; every agent must take stdio. ACP has no system prompt, so the role goes ahead of
      the first message.
    - pi runs with the variables. The gate registers Slopty's tools when the worker hands them
      over in `SLOPTY_PI_TOOLS` (`pi.registerMcpServer`). The role goes as
      `--append-system-prompt`, kept among the thread's own flags so a pi started again has it.
  - **Ending.** `TaskThreads::close(seat)` ends the seated thread through its adapter, or closes
    its terminal, and says false when no thread sits there.
  - **Reports.** A seat with no terminal of its own takes the server's delivered reports as a
    message (`Intent::Send`), queued where the agent queues and steered where it does not, once
    per batch. It is acknowledged only when the message went.
  - **Kept.** The host keeps the seat in the thread's own directory (`seat.json`): the seat, the
    server's variables, the role and the CLI. A thread taken up again after the worker restarts
    is given the same, and a woken Claude Code reopens under the seat. The worker's own variables
    for the seat, the token among them, are never written: the host adds them where the agent
    runs (`Host::env_of`, through the daemon's `Worker::seat_env`), and clients never see the
    file. `Host::reset` keeps the seat fact too, so a thread read again from its agent still
    names it.
  - Tests: `a_seated_start_opens_claude_under_the_seat_with_its_role`,
    `a_seated_start_gives_codex_the_seat_its_tools_and_its_role`,
    `a_seated_acp_thread_gets_the_seat_its_tools_and_its_role`,
    `a_seated_pi_thread_gets_the_seat_and_its_role` (`slopty-worker/tests`);
    `a_seated_pi_runs_with_the_seat_and_its_tools` (`slopty-worker::thread::pi`);
    `a_seats_thread_is_loaded_with_its_variables_and_slopty_tools` and
    `the_tools_are_an_mcp_server_entry_the_gate_registers` (`slopty-agent`).

- ❌ **The person puts an agent to sleep at rest, and wakes it on its own session** (2026-10-04,
  from the Ghostex study's second idea). *Deleted the same day: see "Sleep, waits on another
  thread, queue reordering and edited allows are gone" below.* The item registry's `sleeping` flag only ever released
  a client's view, and nothing set it, so it is gone. In its place, `Intent::Sleep` and
  `Intent::Wake` act on a thread (`Cap::SLEEP`), and a thread put to sleep reads as
  `Liveness::Asleep`.
  - **Sleep is the person's word.** Nothing sleeps an agent on its own: no idle sweep, no lease.
  - **The gates.** The host decides under its lock, so no message slips in between
    (`slopty_worker::thread::sleep::refusal`). It refuses, in words:
    - an agent that is not running, or that waits on its own wakeup;
    - a thread never asked anything;
    - a turn under way, or the agent waiting on its own work;
    - an open request;
    - a message waiting to go;
    - background work still running;
    - a message scheduled for the thread (it would miss it).
  - **How each agent ends.** Each ends the way it ends on its own, and the thread is kept:
    - pi's and an ACP agent's stdin are closed;
    - Codex lets the thread go at once (`thread/unsubscribe`);
    - Claude Code's terminal is closed. Its session is written as it goes, as when the person
      closes the window.

    A thread whose Codex TUI runs in a Slopty terminal is refused. The adapter then tells the
    end as it tells any exit, and the host tells it as the sleep. The sleep is kept through a
    restart and through the adapter telling it gone again, until the agent runs again.
  - **Wake.** A wake takes the session up through each agent's own resume:
    - pi runs on its `--session-id` and reads the session again;
    - an ACP agent loads the session (`session/load`), so it has the cap only when it loads
      sessions;
    - Codex resumes the thread (`thread/resume`);
    - Claude Code runs `--resume` in a new terminal, under the seat for a task's thread.

    The next message wakes a driven agent too, and following a Codex thread already takes it
    up again.
  - **On the ladder.** An asleep thread stands on `Rung::Sleeping`, below Idle, and is counted
    apart (`Counts::sleeping`). It is said quietly ("Asleep") and never as needing the person.
    What it left that asks for a look still ranks it: changes to review, or its error.
  - **Not yet.** The thread view's door for an asleep thread (Wake beside Resume) is the
    client's half.
    The daemon's Claude Code sleep (its terminal closed) has no daemon-level test yet.
  - Tests: `only_an_agent_at_rest_with_nothing_under_way_sleeps` (`thread::sleep`);
    `an_agent_put_to_sleep_ends_asleep_until_it_runs_again` and
    `a_seat_outlives_a_read_again_and_a_restart` (`slopty-worker/tests/threads.rs`);
    `a_pi_put_to_sleep_is_woken_on_its_session`,
    `an_acp_agent_put_to_sleep_is_woken_by_loading_its_session`,
    `a_thread_put_to_sleep_is_let_go_and_woken_by_resuming_it`;
    `an_agent_put_to_sleep_stands_below_one_at_rest` (`slopty-proto`); goldens `intent_sleep`,
    `intent_wake`, `frame_actions` and `attention_ladder`.

- ✅ **The person schedules a message for a time, or for when another thread rests** (2026-10-04,
  from the Ghostex study's seventh idea). *`Delivery::After` was deleted the same day; `At` stays
  for "Continue at" a limit's reset. See "Sleep, waits on another thread, queue reordering and
  edited allows are gone" below.* Two deliveries join the two a message already has:
  `Delivery::At { at_ms }` and `Delivery::After { thread, settle_ms }`. They need
  `Cap::SCHEDULE`, which every adapter has, since the worker holds the message and not the agent.
  - **Kept on the worker.** A scheduled message is a pending one, so the thread view already
    lists it, edits it and takes it back. The host holds the scheduled ones apart from what any
    adapter knows, and puts them back in every pending list an adapter tells. They are in the
    thread's log, so they outlive a restart. Withdraw, Edit and Promote of a scheduled message
    are the worker's to act on and never reach the agent; Promote means send it now. Reorder is
    refused, since each goes at its own moment.
  - **Its moment.** `At` goes at its time. `After` goes once the thread it waits on has been at
    rest for the settle, measured from when it came to rest. At rest means its turn ended,
    nothing asks the person, and it has no wakeup or work of its own under way. Ghostex settles
    for 10 s; the client picks. A thread already at rest counts from when it came to rest, so a
    message after a thread that will not work again still goes. A message cannot wait on its own
    thread, and one whose thread is gone is held, saying so.
  - **How it goes.** A task on the worker (`schedule::spawn`) wakes when a schedule or the
    thread table changes, and at the next due time. It sends each due message the way a
    client's goes: through the daemon's intent path, as the person's own message, queued where
    the agent queues, else as a steer. It is sent as an intent derived from the one that
    scheduled it, so it goes once. One the agent takes leaves the list. One it turns down stays,
    held with the agent's reason.
  - **Never twice.** A message is marked as going, in the log, before it is sent. One the worker
    stopped in the middle of sending comes back held ("It was being sent when the worker
    stopped, so it may have gone") for the person to take back or send again, never sent on its
    own. A message scheduled for a thread keeps its agent from being put to sleep.
  - **Not yet.** The composer's way to pick a time or a thread is the client's half. So is
    automation: a server project task on a schedule.
  - Tests: `a_message_goes_at_its_time_or_once_its_thread_has_settled` (`thread::schedule`);
    `a_scheduled_message_waits_on_the_worker_and_outlives_a_restart` and
    `the_worker_sends_a_scheduled_message_at_its_moment` (`slopty-worker/tests/threads.rs`);
    `a_message_scheduled_after_another_thread_goes_to_pi_once_it_rests`; goldens
    `intent_send_at` and `intent_send_after`.

- ✅ **A thread goes on in a new one, on another agent or on its own afresh** (2026-10-04, R8 of
  the T3 Code orchestrator study, after T3's budgeted context handoff and Amp's Handoff). *The
  account and the draft went the same day; the new thread's composer opens on a pointer back.
  See "Dollar cost, the carried account and drafts on the worker are gone" below.*
  `Intent::Continue { agent }` needs `Cap::CONTINUE`, which every adapter has, since the worker
  makes the account from the thread it holds and not from the agent. Fork stays the agent's own
  branch of its session; Continue is the portable way, for any pair of agents.
  - **A visible first message, never a hidden prompt.** The new thread starts with nothing
    sent. Its first message waits on the worker as a draft (`Delivery::Draft`), which the
    person reads, changes (Edit), sends (Promote) or drops (Withdraw). A draft is kept like a
    scheduled message, in the thread's log through a restart, but it never goes on its own and
    does not keep the agent from sleep. A client may keep a draft of its own the same way.
    Slopty's rule that the agent's session is the record holds: switching agents is a new
    thread with its own session, never words slipped into an old one.
  - **The account** (`slopty_agent::handoff::render`) is made by rule, not by a model, so the
    same thread always gives the same words. It opens by saying whose conversation it was and
    where, and that what follows is context rather than a request. One line says how much it
    holds. Then come the plan, the files changed and the commands run with how they ended, each
    newest first within an eighth of the budget, and together within half of what is left. The
    person's messages with their final answers fill the rest, newest first: the newest is cut
    short if it alone is too long, and older ones go whole or not at all. The budget is 32 KiB.
    A property test holds it to the budget for any thread.
  - **Where it came from.** The new thread's `forked_from` names the old one through its last
    turn, so the lineage reads as a fork's does. The old thread goes on as it was.
  - **Once.** The start is once per intent, as a client's start is. A repeat starts nothing and
    finishes what the first may not have, the draft and where it came from, both kept once.
  - **Not yet.** The thread view's door ("Continue in…" with the agents this worker has) is the
    client's half. So is a project task's "Restart fresh" or "Give to another agent", which
    starts the task's runner again with the account, its branch and its brief. A seated
    thread's seat does not go with it.
  - Tests: `a_thread_is_told_whole_when_it_fits`,
    `the_newest_message_stays_when_the_budget_is_tight` and the property test
    `an_account_keeps_within_its_budget` (`slopty_agent::handoff`);
    `a_claude_code_thread_goes_on_in_pi_from_a_draft_the_person_sends`
    (`slopty-worker/tests/pi.rs`); `only_an_agent_at_rest_with_nothing_under_way_sleeps` for
    the draft; goldens `intent_continue` and `intent_send_draft`.

- ✅ **A message sent now to an agent with no steer of its own stops its turn** (2026-10-04, R13
  of the T3 Code orchestrator study, after T3's steering restart and ACP agents steered by
  cancel and resend). `Delivery::Interrupt` needs `Cap::INTERRUPT` and `Cap::QUEUE`. It is for
  an agent without `Cap::STEER`, today any ACP agent, whose message otherwise waits for the turn
  to end.
  - **Through the agent's own doors, on the person's word.** The ACP adapter queues the message
    first, under the person's intent, so the pending list and the turn it starts name it as
    theirs, and stops the turn under way, when there is one, in the same step
    (`ThreadAsk::Interrupting`; the worker's earlier three-step `thread::steer` went with
    `Intent::Reorder`, see "Sleep, waits on another thread, queue reordering and edited allows
    are gone"). The stopped turn ends as the agent ends it, and the queue sends the message
    next. Nothing is typed into a screen and nothing is answered for the person: the open request
    is withdrawn by the agent's own cancel.
  - **No race to lose.** The stop is the agent's to make. A turn that ends before the stop lands
    leaves the message to go as a queued one does, so it goes once either way. Orchestration's
    intents take the same path.
  - **Not yet.** The composer's "Interrupt and send" for such an agent is the client's half.
  - Tests: `a_message_sent_by_interrupt_stops_the_turn_and_goes_next`
    (`slopty-worker/tests/acp.rs`); golden `intent_send_interrupt`.

- ✅ **Edit from a turn goes back in a new thread, through the agent's own door** (2026-10-04,
  §11 of the T3 Code UI study). `Intent::Rewind { turn, files }` needs `Cap::REWIND`.
  - **A branch, never a rewrite.** The conversation goes back only the way the agent offers.
    Codex branches a session cut before a turn (`thread/fork` with `beforeTurnId`), so its
    adapter has the cap, and the new thread's `forked_from` names the turn before. The thread
    edited from keeps every turn: the agent's session stays the record, and no session file is
    written. Codex's `thread/revert` rewrites a thread's own history in place. It is left
    unused, because the branch loses nothing and gives the same next turn.
  - **The other agents, checked and refused.**
    - pi's RPC `fork { entryId }` moves the running pi onto a new session cut before a message.
      Taking it means the adapter follows a session that changes under a thread, which it does
      not do yet. Until then pi has no cap.
    - ACP has no rollback, and its `session/fork` branches a whole session.
    - Claude Code goes back through its TUI's own rewind, which Slopty never types into. Its
      `--resume-session-at` is a hidden flag for print mode only, so it is not a door.
  - **The prompt returns to the composer.** The client that asked puts the turn's message in
    the new thread's composer, for the person to change and send. (It first waited on the
    worker as a `Delivery::Draft`; see "Dollar cost, the carried account and drafts on the
    worker are gone" below.)
  - **The files, on the person's word.** With `files`, the folder goes back to the turn's
    before-snapshot (`refs/slopty/threads/<thread>/<turn>-before`) through the thread's own
    index (`git restore --source --worktree`). Every file a snapshot holds goes back to its blob
    and mode, and a file the snapshot lacks is removed. Ignored files, the person's index, `HEAD`
    and stash are untouched. What the folder held first is kept as
    `refs/slopty/threads/<thread>/<turn>-rewound`, so nothing is lost. A turn with no
    before-snapshot is refused before anything happens.
  - **Order and once.** It is refused while a turn is under way. The branch comes first, since it
    changes nothing on disk, then the files, then the draft. Each step is kept under an intent of
    its own, so a repeat finishes what the first left and repeats nothing.
  - **Not yet.** The menu item on a person's message, "Edit from here" with or without the
    files, is the client's half.
  - Tests: `an_edit_from_a_turn_branches_before_it_and_puts_its_files_back`
    (`slopty-worker/tests/codex.rs`, a stand-in Codex daemon and a real git folder);
    `a_fork_names_codexs_turn_and_a_forked_thread_says_where_it_came_from`
    (`slopty-agent/tests/codex.rs`); golden `intent_rewind`.

- ✅ **Model, effort and mode are switched through each agent's own settings door** (2026-10-04).
  `Intent::SetEffort { effort }` joins `SetModel` and `SetMode`, with `Cap::SET_EFFORT`. A thread
  says what it can be switched to: `ThreadMeta::models`, `ThreadMeta::efforts` (new, each an
  `Effort { id, label, description }`) and `ThreadMeta::modes`. Each list is the agent's own, so
  no client keeps one, and a value it does not offer is refused before anything goes.
  - **Codex, through `thread/settings/update`.** The setting holds for the thread's next turns,
    the TUI's included, so a switch here and one in the TUI are the same switch. Codex then tells
    every client what it holds (`thread/settings/updated`, read now instead of passed over), so a
    switch made in the TUI shows here too.
    - The models are Codex's `model/list`, asked once as the worker joins the daemon, every
      page, hidden models left out. A model is named by its slug (`model`), which is what the
      thread runs, and shown by its display name.
    - The efforts are the running model's `supportedReasoningEfforts`. A model that lacks the
      thread's effort goes with its own `defaultReasoningEffort`, as Codex's own picker sets it.
    - The modes are the three approval policies, `untrusted`, `on-request` and `never`.
      `granular` is shown when Codex says it and is never offered. The sandbox is not a mode:
      it stays as the person's Codex configuration sets it.
    - A per-turn override on `turn/start` was the other door. It was not taken, because it
      reaches only Slopty's own next turn and leaves the TUI and the meters on the old values
      until then.
    - Codex's collaboration mode (plan or default) is a second axis beside the approval policy.
      It is experimental in the pinned build and stays unswitched for now.
    - A switch Codex refuses leaves the meters as they were and says why in the thread
      ("Codex didn't switch: …"), since the intent was done once it went.
  - **pi, through its RPC.** The efforts are `get_available_thinking_levels`, asked with the
    state and the models, and again after a model switch, since pi clamps the level to the new
    model. A model that does not reason has only `off`, which offers nothing to choose. An effort
    goes as `set_thinking_level`, and the meter follows pi's `thinking_level_changed`.
  - **ACP, through the session's config options.** The efforts are the values of the option
    whose category is `thought_level`, with their descriptions, and an effort goes as
    `session/set_config_option`. The agent answers with the options as they now stand. The cap
    is there only while the agent offers the option, as it is for models and modes.
  - **Claude Code, observed: the model only.** `/model <id>` stays: it is the TUI's own typed
    command, sent on the person's word through the composer's guard. The pinned 2.1.283 also
    lists `/effort` and `/plan`, but neither is a whole switch. `/effort` takes no argument and
    opens a menu, and `/plan` only enters plan mode, while leaving it or reaching any other mode
    is Shift-Tab's cycle. Driving either means keys into a menu or a mode cycle, which Slopty
    never types, so an observed thread has no `SET_MODE` and no `SET_EFFORT`. Its mode and
    effort stay read-only meters.
  - **Not yet.** The effort picker beside the model and mode chips is the client's half
    (`target/lanes/ui-queue.md`).
  - Tests: `a_thread_switches_among_what_codex_offers` and
    `the_threads_settings_are_its_mode_and_effort` (`slopty-agent/tests/codex.rs`);
    `a_switch_goes_to_codex_as_the_threads_settings` (`slopty-worker/tests/codex.rs`, a
    stand-in daemon that takes and refuses switches);
    `a_pi_threads_effort_is_set_among_the_levels_pi_offers` (`slopty-worker/tests/pi.rs`);
    `an_acp_threads_effort_is_set_through_its_thought_level_option`
    (`slopty-worker/tests/acp.rs`); golden `intent_set_effort`.

- ✅ **What was said in the threads is searched on the worker that holds them** (2026-10-04,
  `crates/slopty-worker/src/thread/search.rs`). `ThreadRequest::Search { query, limit }` is
  answered with `WorkerMsg::ThreadHits` on the control stream.
  - **What is searched.** Each thread's state as the worker's log keeps it: the person's
    messages, the agent's answers and reasoning, its calls' titles and its notices, in every
    thread held, a subagent's and an exited one's among them. A text the log keeps clipped is
    searched as far as it is kept. The rest is the agent's own session's, and reading every
    agent's session files for a palette keystroke would cost more than it finds. Past prompts in
    sessions the worker does not hold stay `ThreadRequest::Sessions`'s.
  - **The match is the prompt search's.** Every word in one item, case ignored unless a word
    has a capital, accents ignored, through the same matcher (`nucleo-matcher` substring atoms).
    A whole word ranks above one inside another word. The excerpt is cut round the first match
    the same way too: `history::excerpt` serves both. A thread ranks by its best item, then by
    its newest match.
  - **Lean answers.** A hit names its thread, item and turn, so a client scrolls to it, and
    says what kind of words matched (open: `person`, `agent`, `reasoning`, `tool`, `notice`).
    Title, agent and folder are the thread's row in the table the client already holds, so they
    are not sent again. At most `SEARCH_THREADS` threads, `HITS_PER_THREAD` items each and
    `ITEM_HIT_BYTES` of each, with counts of what was left out.
  - **No adapter waits on a search.** It runs on the blocking pool, and the host's lock is
    taken one thread at a time (`Host::visit`).
  - **Not yet.** The palette's "Threads" section is the client's half
    (`target/lanes/ui-queue.md`), and a CLI verb waits on a control-socket request of its own.
  - Tests: `every_word_is_found_in_what_was_said_the_best_and_newest_first` and
    `a_hit_shows_its_match_and_the_limit_counts_what_it_left_out` (`thread::search`); goldens
    `client_thread_search` and `link_worker_thread_hits`.

- ✅ **Sleep, waits on another thread, queue reordering and edited allows are gone** (2026-10-04,
  the session's cut list, from the feature audit and the orchestrator study). The user's
  standing rule is that nothing is hidden: a feature nobody reaches for, or that isn't worth its
  place, is deleted, and pre-release nothing is kept for compatibility.
  - **Sleep and wake.** Nothing ever sent `Intent::Sleep`, and settling a finished task's agent
    (`hub/settle.rs`) already frees a resting one. `Intent::Sleep`, `Intent::Wake`,
    `Cap::SLEEP`, `Liveness::Asleep`, `Rung::Sleeping` with `Counts::sleeping`, the worker's
    `thread/sleep.rs` and the host's sleep marking, each adapter's sleep and wake, and the
    thread view's Wake door are deleted. Resume stays: an exited agent goes on through its own
    resume, by the next message or the Resume strip. Codex's own let-go of a resting thread
    (`thread/unsubscribe`, taken up again on a follow or an ask) is not sleep, and stays.
  - **`Delivery::After`.** A message that waits for another thread to rest went with the
    general "Send later…" menu. `Delivery::At` stays, for "Continue at" a usage limit's reset,
    with drafts. The scheduler now only watches times (`schedule::when`).
  - **`Intent::Reorder` and `Pending::reorder`.** Nobody reordered a queue, and both studies
    agreed. The one inner use, a message sent by interrupt going first, now belongs to the one
    adapter that needs it. An agent that steers takes "now" as a steer (the daemon turns
    `Delivery::Interrupt` into `Delivery::Steer` where the thread has `Cap::STEER`). An ACP
    agent, which has no steer, puts the message first in its own queue and cancels the turn
    under way in one step (`ThreadAsk::Interrupting`). `thread/steer.rs`, which reordered
    through the wire intent, is deleted.
  - **The "Edit…" allow.** `Verdict::AllowEdited`, `Editable`, `Request::editable`,
    `PermissionPrompt::editable` and the relay's edited `updatedInput` are deleted. Deny with a
    reason stays, and so do `updatedInput` answers to `AskUserQuestion` and `ExitPlanMode`,
    which are the person's answers rather than an edit.
  - Tests kept or reworked: `what_a_resting_thread_left_ranks_it_above_rest` (`slopty-proto`),
    `a_message_goes_at_its_time` (`thread::schedule`),
    `a_scheduled_message_waits_on_the_worker_and_outlives_a_restart` and
    `the_worker_sends_a_scheduled_message_at_its_moment` (`slopty-worker/tests/threads.rs`),
    `a_message_scheduled_for_a_time_goes_to_pi_at_its_time` (`slopty-worker/tests/pi.rs`),
    `a_message_sent_by_interrupt_stops_the_turn_and_goes_next` (`slopty-worker/tests/acp.rs`),
    `a_queued_message_promoted_goes_now` (`slopty-worker/tests/compose.rs`) and
    `a_held_message_promoted_steers_the_turn` (`slopty-agent/tests/codex.rs`).

- ✅ **The person's stop holds what is queued until they speak again** (2026-10-04, A1 of the
  T3 Code delta study, after T3's "Queue paused" once Stop is pressed). Before this, a queued
  message went as soon as the stopped turn ended, so Stop with a message queued started it.
  - **Held, in words.** `Intent::Interrupt` holds every queued message not yet on its way as
    `PendingState::Held { reason: Pending::STOPPED }` ("Held since you stopped the turn",
    `Pending::hold_for_stop`). The person's next send releases them in order, behind it or
    after the new message as each agent queues. `Promote` sends one now and lets the rest go
    behind it. `Withdraw` and `Edit` work as before. A turn the agent ends by itself holds
    nothing.
  - **Each adapter's own queue.**
    - Claude Code (`thread/compose.rs`): the composer holds the worker's queue and skips a
      stopped message. One already in the terminal (typed, or taken back) is not the
      worker's to hold.
    - Codex (`Shared::stop`): the interrupt and the hold are one step, and `next_queued` waits
      while the front is held.
    - ACP (`Session::hold_queue`, `release_queue`): the same, in the adapter's own queue.
    - pi has no `Cap::QUEUE`, so there is nothing to hold.
  - **`Delivery::Interrupt` is exempt.** Its message is meant to go next. It releases what a
    stop held and goes first. Where the thread has `Cap::STEER`, the daemon now sends it as a
    steer.
  - No wire change: `PendingState::Held` already existed, and the view already draws a held
    message.
  - Tests: `a_stop_holds_the_queue_until_the_person_sends_again` in
    `slopty-worker/tests/compose.rs` (Claude Code), `slopty-worker/tests/acp.rs` (a stub ACP
    agent) and `slopty-agent/tests/codex.rs` (Codex).

- ✅ **The files go back only while no other thread works in the same folder** (2026-10-04,
  A10 of the T3 Code delta study, after T3 replaced its path-scoped revert with a refusal,
  #12306).
  - `Intent::Rewind { files: true }` restores the whole work tree. So another thread whose
    folder has the same git root, with a turn under way or waiting on the person, would have
    its edits go back under it mid-turn.
  - That rewind is refused before anything happens, in words: "“{title}” is working in the same
    folder, and its edits would go back too. Go back without the files, or once it rests"
    (`thread::rewind`).
  - Going back without the files, which is only the branch, still goes.
  - T3's blanket refusal is not taken: Slopty keeps what the folder held under `<turn>-rewound`,
    so the person's own edits are never lost.
  - Test: `an_edit_from_a_turn_branches_before_it_and_puts_its_files_back`
    (`slopty-worker/tests/codex.rs`) is first refused while a second stand-in Codex thread works
    in the same repository, and the files go back once that thread rests.

- ✅ **A Codex goal shows as Codex holds it, read-only** (2026-10-04, A11 of the T3 Code delta
  study; T3's #6777 and #15133, and #7935 for why goal controls stay out).
  - **The model.** `ThreadState::goal` is an open `Goal`, set by `Action::GoalSet`. It holds the
    objective, the state in open words (`active`, `paused`, `blocked`, `usage-limited`,
    `budget-limited`, `complete`: Codex's names in kebab case), the tokens used against the budget, the
    time used and when it changed.
  - **From Codex.** The adapter reads `thread/goal/updated` and `thread/goal/cleared`. On a
    resume it asks `thread/goal/get`, so a goal set while Slopty was away still shows.
  - **Setting, pausing or clearing a goal stays in Codex's TUI.** An active goal means Codex may
    start turns by itself after a stop. The thread view says so at its Stop door rather than
    fighting it. The turns Codex starts are seen through `turn/started` as any turn is, so
    T3's #15133 (a goal thread shown as done while it ran) does not arise.
  - Generated subset: `thread/goal/get`, `thread/goal/updated` and `thread/goal/cleared`
    (`cargo xtask codex schema`, Codex 0.160.0).
  - Tests: `a_codex_goal_shows_as_codex_holds_it` (`slopty-agent/tests/codex.rs`); `GoalSet` in
    the `golden_thread` actions.

- ✅ **An archived Codex thread is unarchived and taken up again, once** (2026-10-04, A8 of the
  T3 Code delta study, after T3's `44bd4c9c` (#15389), which closes #10481).
  - A Codex thread archived outside Slopty, by Codex's own TUI or `codex archive`, makes
    `thread/resume` fail.
  - On that refusal only, the worker calls `thread/unarchive` for the same native id and then
    repeats the identical resume once (`Waiting::Unarchive`).
  - Codex says the thread is archived only in words: "session <id> is archived", or its hint to
    run `codex unarchive`. They are read as T3 reads them (`thread::codex::archived`).
  - Any other refusal stays a refusal. A second one after unarchiving is told as it comes. No
    fresh session is ever started in its place, which is the fallback T3 kept.
  - Not measured against a signed-in Codex, which no test runs. The words are T3's, and they
    are taken from Codex's own message.
  - Tests: `an_archived_thread_is_unarchived_and_taken_up_again` (`slopty-worker/tests/codex.rs`,
    a stand-in daemon that refuses until unarchived); `only_an_archived_thread_is_unarchived`
    (`thread::codex`).

- ✅ **Ask aside: a side question on a fork that goes away** (2026-10-04, A9 of the T3 Code
  delta study; T3's most-upvoted idea, #7311, never merged there).
  - **The wire.** All three intents need `Cap::FORK`.
    - `Intent::Aside` forks the whole thread as `Intent::Fork` does, so Claude Code's
      `--fork-session` keeps the prompt cache. It answers `Started { thread }`.
    - The new thread carries the open fact `slopty.aside` (`ThreadMeta::ASIDE_FACT`, read by
      `aside_of`), naming the thread it was asked beside. The client sends the question to it
      as its first message.
    - `Intent::KeepAside` drops the fact, so the thread becomes an ordinary thread of its own.
    - `Intent::Discard` ends the aside's agent as a settled task's is ended, and the worker
      forgets the thread and its log. Discard is refused for a thread that is not an aside. It
      is answered once with the worker's starts, since the thread is gone after.
  - **The agent's session stays** wherever the agent keeps it, as every session does. Nothing
    is written into the main thread's session.
  - **Not sleep.** The study's sketch put a closed aside to sleep. Sleep was deleted the same
    day, so a closed aside ends.
  - **Kept by the worker.** The fact is the worker's own (`Own::aside`) and is put back on every
    meta, so an agent's rename or resume does not drop it. It outlives a worker restart.
  - **The client's half.** The sheet over the thread, and passing over `slopty.aside` threads
    in the navigator, the inbox and the attention ladder, belong to the UI and server lanes.
  - Tests: `an_aside_keeps_its_mark_until_it_is_kept_or_forgotten`
    (`slopty-worker/tests/threads.rs`); goldens `intent_aside`, `intent_discard`,
    `intent_keep_aside`.

- ✅ **Claude Code's prompts are held for thread follows, and the conversation stream is gone**
  (2026-10-04, item 7 of the readiness audit). Every agent reaches a client as a thread, so the
  old Claude Code follow stream had no reader left.
  - **Who a prompt is held for.**
    - A client that follows the session's thread (`ThreadRequest::Follow` of a thread with a
      terminal) holds every prompt there.
    - A client that keeps the thread table (`ThreadRequest::Table`) holds a yes or no, for
      `APPROVAL_HOLD` at most, since every thread's requests show in its rows. That is a
      notification's Allow, or the inbox. It took `ConversationRequest::Approvals`, which every
      app client sent anyway.
    - Orchestration follows as before.
    - The machine (`slopty_worker::conversation::Holds`) is unchanged but smaller: nothing is
      shown per link any more, so `shown_to`, `tells` and `stop_approving` are deleted.
  - **How it is shown.** The hold (`apps/slopty-worker/src/threads/hold.rs`) tells the
    session's thread observer directly (`thread::claude::Driver::permission`), in the order it
    decides, and the observer opens or settles the request on the thread.
    - That replaces the daemon broadcast, where a lagging receiver could drop a prompt.
    - A prompt for a terminal nothing observes yet starts observing it, as it did.
  - **Answers.** Answers go only through `Intent::Answer`, and the hand-back to the TUI only
    through `Intent::Release`, both on the thread's request.
    - The last follower letting the thread go, its connection ending, the wait running out or
      the relay going away still hand a prompt back undecided.
  - **Deleted.**
    - From the wire: `ConversationRequest`, `ConversationEvent`, `ClientMsg::Conversation`,
      `WorkerMsg::Permission`, `UniHead::Conversation` and its goldens, and `Blob` and
      `EXPAND_CHARS` with them.
    - From the transport and the client: `slopty_net::streams::{open_conversation,
      CONVERSATION_PRIORITY}`, `Uni::Conversation` and `LinkEvent::Conversation`.
    - From the worker: its follow stream (`follow.rs`), its shared transcript reader
      (`conversation::Reader`), and its `@` walk (`file::mention`). The thread composer uses
      `FindFiles`.
    - `PermissionEvent` and `PermissionPrompt` stay, as the worker's own vocabulary between the
      hold and the observer.
  - Tests:
    - In `slopty-workerd/tests/threads.rs`: `a_prompt_is_held_while_its_thread_is_followed` (it
      replaces the follow test of the e2e),
      `the_tables_holder_answers_without_following_and_the_tui_asks_otherwise` (the approver
      test), `a_trusted_mod_streams_live_blocks_that_the_transcript_settles` (live blocks
      through the thread).
    - `an_approver_is_held_for_without_following` and `a_new_follower_answers_what_is_waiting`
      (`slopty_worker::conversation`).
    - The measurement `echo_beside_a_followed_thread` (e2e, ignored).

- ✅ **The person's stop holds deliveries to that agent until they speak again** (2026-10-04,
  A2 of the T3 Code delta study). A report that fell due a minute after the person stopped an
  orchestrator was posted to its idle session and started it working again.
  - **The signal.** The interrupt record in Claude Code's transcript ("[Request interrupted by
    user]") already ends the turn as `Idle` with the detail "interrupted"
    (`transcript::INTERRUPTED`, `Progress::is_interrupt`). The daemon's `Tracker` now keeps that
    as a flag, and only the person's next prompt (`UserPromptSubmit`) or a new session lifts it.
    A later idle hook (`Notification` idle_prompt) does not, nor does the transcript alone for a
    hooked agent.
  - **The guard.** `orchestrate::may_deliver` is `may_type` plus a refusal while the flag
    stands (`AgentTable::interrupted`, `DaemonAgents::interrupted`). The worker's post to the
    server's orchestrator (`server.rs` `may_post`) asks it. A batch held there is not dropped:
    it rides with the person's next prompt.
  - Tests: `reports_wait_for_the_person_after_they_stop_the_agent`
    (`slopty-workerd/tests/server_link.rs`; it fails with `may_type` in its place),
    `an_interrupted_turn_goes_idle_from_the_transcript` and
    `the_table_keeps_the_persons_stop_until_their_next_prompt` (`slopty-agent`).

- ✅ **An agent left waiting only on its commands says so** (2026-10-04, A6 of the T3 Code
  delta study, the adapter half). A dev server the agent left running held its row at
  `Waiting` with the kind `task`, which the server cannot tell from a subagent or a monitor.
  - The observed adapter names the wait `command` (`Wait::COMMAND`) when every task it counts is
    a running `SHELL` task of the main thread and no scheduled prompt is set. Its text is the
    commands' titles. Anything else stays `task` (`Wait::TASK`).
  - The wait is worded again when the transcript starts or ends a task while the agent waits.
    The hooks often count a command before the transcript names it, so the wait turns from
    `task` to `command` once it does. A count the transcript does not show (a monitor) keeps
    it at `task`.
  - The kind is open, so the wire did not change. The server's settle reads `command` as at rest
    for a finished task (`ladder::COMMANDS_WAIT`).
  - Test: `a_wait_on_commands_left_running_names_them` (`slopty-agent` observed tests).

- ✅ **A Codex thread another client rewrote is read again, its held messages kept**
  (2026-10-04). Codex's `thread/reverted` says a client rewrote the thread's history in place.
  The worker takes the thread up again (`thread/resume`) and reads it whole, and the messages
  the person queued are put back ahead of anything queued since.
  - **A revert that undid the running turn.** A queued message waits for the turn under way to
    end. When the re-read shows no turn under way, nothing is left to end, so the message goes
    as the next turn at once. Before, it stayed held until some later notice about the thread.
  - Test: `a_reverted_thread_is_read_again_and_keeps_its_held_message`
    (`slopty-worker/tests/codex.rs`; without the send after the re-read it fails).

- ✅ **Dollar cost, the carried account and drafts on the worker are gone** (2026-10-04, prune
  step 3 of `.research/feature-prune-frontier-2026-10-04.md`, §5.1 #8 and #9).
  - **Tokens, never dollars.** `Usage::COST_MICRO_USD`, `Meters.cost_micro_usd`, the status
    line's `cost_usd`, pi's `cost` fields, ACP's `UsageUpdate.cost` and Codex's
    `account/usage/read` are no longer read. The figure was an estimate for API billing that
    most people on a subscription never pay, it read differently per agent (Codex gives none
    on a credits plan), and a turn's tokens already say how much work it took. The turn footer,
    the tray and the project's spend show tokens and time.
  - **A pointer, not an account.** Going on in another agent no longer writes a 32 KiB account
    of the old thread by rule (`slopty_agent::handoff` is deleted). The new thread starts with
    nothing sent, and the client that asked opens its composer on one short paragraph: the old
    thread's id, folder and branch, and `slopty agent read --thread <id>` to read it. The new
    agent reads what it needs with its own tools, and the person sends, changes or clears the
    words first. An account chosen by rule only guessed at what mattered, and it cost the next
    agent its context whether it needed the history or not.
  - **The composer, not the worker, holds a draft.** `Delivery::Draft` is deleted, with the
    tray's draft card. Edit from a turn and a fork from before a message put the message in
    the new thread's composer the same way. The hub keeps the words by the intent until the
    worker says which thread it started, then gives them once to that thread's first view
    (`ThreadHub::intent_seeded`, `take_seed`). Words the person had not sent are not worth a
    worker round trip, a log entry and a restart path of their own.
  - Goldens: `intent_send_draft` deleted; `intent_send_interrupt` and every golden carrying
    `Meters` changed.
  - Tests: `branching_to_another_agent_carries_the_thread_over` and
    `branching_from_a_message_keeps_or_puts_back_the_files` (`slopty-ui`, the composer seeds);
    `a_claude_code_thread_goes_on_in_pi_with_the_persons_first_message`
    (`slopty-worker/tests/pi.rs`); the Codex start test
    (`a_start_asks_the_persons_codex_for_a_thread_and_sends_its_first_turn`) asks no spend.

- ✅ **Only a project's agents get Slopty's tools; the others get a pointer to the CLI**
  (2026-10-04, prune §5.1 #3). A Claude Code Slopty wires (a tile opened on `claude`, ⌘⇧T, a
  `claude` typed in a Slopty shell) gets `--mcp-config` serving `slopty mcp` only when its
  session names a project (`SLOPTY_PROJECT`). Any other gets one paragraph on
  `--append-system-prompt` (`slopty_agent::hooks::POINTER`): Slopty reaches the person's other
  machines, and `slopty --help` says how. A system prompt the person appends keeps its place
  and gets the paragraph after it. The resume keeps it as it keeps a project agent's role.
  - Why: the tool block cost every agent its descriptions and schemas in context whether it
    reached another machine or not, and the CLI already does everything the tools did. The
    paragraph is the agent's lazy door: it reads the help when the work calls for it.
  - Driven Codex, pi and ACP threads already got Slopty's tools only when seated by a project.
    They get no pointer yet. Codex's `developerInstructions` would stand in for the person's
    own `developer_instructions` rather than add to them, pi's `--append-system-prompt` is not
    yet checked against its `APPEND_SYSTEM.md`, and ACP has no door for it.
  - Still open: a `codex` opened in a tile on a worker with a server gets Slopty's tools
    (`Worker::as_agent`, `codex_with_mcp`), and every `SpawnAgent` gets them
    (`orchestrate.rs`), project or not.
  - Tests: `a_persons_claude_is_wired_once` and
    `the_pointer_joins_the_persons_own_appended_prompt` (`slopty-agent`);
    `claude_opened_in_a_tile_is_started_as_slopty_starts_its_agents`
    (`slopty-worker/tests/agent_open.rs`); `a_typed_claude_is_wired_as_one_slopty_starts`
    (`slopty-cli/tests/claude_wire.rs`).

- ✅ **A live block waits for its prompt** (2026-10-04). The mod streams a block as the model
  writes it, often before the transcript has the prompt that started the turn: the hook and
  the mod run ahead of the file, read on a hook or every 250 ms. The observed adapter put such
  a block in the thread's last turn, so a live tool call stood ahead of the prompt it answered
  until the transcript settled it. Now a block that begins while its thread has no turn open
  (none began, or the last one ended) is held with what it says meanwhile, and begins after
  the prompt once the transcript opens its turn. A block the transcript settles first, or a
  thread read anew, lets it go. A turn with no prompt of its own in the transcript shows its
  blocks only as the transcript has them.
  - Still open: a live text the transcript settles is replaced at the end of the list, so its
    entry lands after a live tool call that began after it, until the thread is read again.
    Putting it in its place takes an insert on the thread wire.
  - Test: `live_blocks_stream_after_their_prompt_then_the_transcript_settles_them`
    (`slopty-agent` observed tests; it fails without the hold, the blocks shown before the
    prompt).
