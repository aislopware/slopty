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
    - *Any ACP agent, driven,* through the `agent-client-protocol` crate.
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
    transcript's file name. A thread held from before starts over under a new epoch and is
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
