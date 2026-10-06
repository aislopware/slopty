# Decisions — Claude Code

See `docs/DECISIONS.md` for the legend. Newest entries go at the end.

- ✅ Hooks are the authoritative signal (33 events as of 2026-09; we register the status-bearing
  subset), transcript JSONL is read-only context, screen heuristics corroborate only.

- ✅ Relay path (verified 2026-09-04 with Claude Code 2.1.260 running `-p` inside a Slopty
  shell): `slopty hook install` registers `{"command": "<abs slopty>", "args": ["hook"],
  "async": true, "timeout": 5}` for 12 events (`SessionStart/End`, `UserPromptSubmit`,
  `Pre/PostToolUse`, `PostToolUseFailure`, `PermissionRequest/Denied`, `Notification`,
  `Elicitation/Result`, `Stop`) in `~/.claude/settings.json` (or `--settings`). Exec form
  (no shell) and `async` so the agent never waits on us. The relay reads `SLOPTY_SESSION` and
  `SLOPTY_WORKER_SOCKET`, both injected by the host into every session's environment, posts
  `CtlRequest::Hook` and exits 0 whatever happens. `slopty-agent::Tracker` maps hooks to
  `AgentStatus`; `AgentEvent.attention` is true only on entering a blocked state or `Done`,
  so the permission `Notification` that follows a `PermissionRequest` does not alert twice.
  Joining clients get the current table after the canvas snapshot (no proto change:
  `WorkerMsg::Agent` already existed). Observed cycle: Idle → Working → Tool{Bash} → Working →
  Done → None.

- ✅ Answering a permission prompt from the badge (verified 2026-09-05 against Claude Code
  2.1.261 driven under a pty in `default` permission mode): the prompt is a numbered menu with
  the first entry highlighted — `❯ 1. Yes · 2. Yes, and always allow access to <dir> from this
  project · 3. Yes, and switch to auto mode · 4. No — Esc to cancel · Tab to amend`. Enter
  (`\r`) ran the command ("Ran 1 shell command", the file appeared); Esc (`\x1b`) ended the turn
  with "Interrupted · What should Claude do instead?" and nothing ran. So "allow" = Enter and
  "deny" = Esc, sent through `TerminalView::press` like the phone's key bar, with a sticky
  Control disarmed first. Neither key is sent twice: the canvas remembers the answer per session
  until the next `WorkerMsg::Agent` for it. Two things learned on the way: text and `\r` written
  in one burst are taken as a paste (the newline does not submit), and Esc while the model is
  still thinking interrupts the turn instead of answering — the badge only offers the buttons
  while the status is `Blocked(Permission)`, which the `PermissionRequest` hook raises after
  the menu is up. Question / elicitation badges cannot be answered with one key (the reply is
  text or a pick), so their button reveals and focuses the terminal.

- ✅ What the badge says when the agent waits or stops (verified 2026-09-05, Claude Code 2.1.261
  inside a Slopty session through `slopty open`, payloads captured with a `tee` hook beside
  the relay): every event carries `transcript_path`; `Stop` also carries
  `last_assistant_message` (here "Done. `/tmp/…/opened-enter` was created."), which the docs
  say to prefer because the transcript file "may lag the in-memory conversation". So the
  detail comes from the payload first — the `AskUserQuestion` input's first `question`, the
  `Elicitation` `message`, the `Stop` message's last non-empty line — and only a `Blocked
  (Question|Elicitation)` or `Done` event (`Done` dropped 2026-09-28, below) that still has no
  detail makes the daemon read the transcript's last 256 KiB for the newest non-sidechain
  `assistant` record with a `text` block (`slopty_agent::transcript`, `spawn_blocking`, table
  lock released meanwhile). The transcript is JSONL of
  `{"type":"assistant","message":{"content":[{"type":"text",…}]}}` records;
  `isSidechain: true` rows are subagent chatter and skipped. No wire change: `detail` already
  existed. Found on the way: the `permission_prompt` Notification that follows a
  `PermissionRequest` ~6 s later says only "Claude needs your permission" (no "to use X"), and
  used to replace `Permission{Bash}` + "$ touch …" with `Permission{""}` + nothing; the tracker
  now keeps the request's tool and detail when the notification names none. After Esc on the
  menu no `Stop` fires (the turn is interrupted; the next hook is whatever the user does), so
  the badge keeps saying "denied" until then — acceptable, since the outline is already off.

- ✅ "Terminal agent" (⌘⇧T, "New Agent" menu item) sends `OpenSession { command: ["claude"], title:
  "claude" }`; nothing else is special about the session, so the hook relay, badges and
  attention all apply as they do to a `claude` typed into a shell. The daemon's `PATH` is
  launchd's under a LaunchAgent, and `claude` is often a shell alias (`~/.claude/local`), so
  `slopty-pty::resolve_command` runs a bare name it cannot find on `PATH` through
  `$SHELL -lic '<quoted words>'` (interactive login shell: rc files, aliases, job control);
  a path or a name found on `PATH` still execs directly. Covered by
  `unknown_bare_program_goes_through_the_login_shell`.

- ✅ **Conversation view: richer entries, protocol 11** (2026-09-05). `TranscriptEntry` became
  `{ at, body }` with `TranscriptBody::{User, Assistant, Thinking, ToolUse, ToolResult}` and
  a `Clipped { text, more_lines }` for the long parts. The host clips thinking, tool input and
  tool output to 40 whole lines or 4 000 characters (`slopty_agent::transcript::clip`), whichever
  first: a `Read` of a 40-line file or a `cargo test` tail fits, a minified one-line blob is
  cut at 4 000 characters with an ellipsis, and the wire never carries a whole file while the
  reader still sees how much was dropped. Results are named after their `tool_use_id` by a
  bounded table in the `Tail` (512 ids, then it starts over), since Claude Code batches several
  calls before their results. Timestamps ride as Unix milliseconds and the client shows local
  "HH:MM" (`chrono`, already in the tree). Goldens `host_transcript` (every variant) and
  `client_hello` re-accepted; PROTOCOL_VERSION 10 → 11.

- ✅ **The agent's task list is kept as a checklist card** (2026-09-14). `TodoWrite` shows
  whole in the card, but it is the agent's list: it is rewritten on every call and gone when
  the session is. Rulings: (1) the list's header carries the "note" button an answer has
  ("Keep the task list as a card", `conversation-note-<entry>`), which puts the list beside
  the agent as a note (`conversation::todos_note`: a "Tasks" heading, `- [x]` for what is
  completed, `- [ ]` for pending and in progress — the note is the human's to tick from
  there, see canvas.md "A note's task line ticks on a click"); (2) it is the same
  `NoteBlock` event and the same `note_beside` placement as an answer's, so one path lands
  every kept thing; (3) a list with no items has no button. Test: headless
  `an_edit_shows_its_diff_and_a_todo_list_its_checklist` (the edit has no button, the list's
  press emits the checklist without folding the entry).

- ✅ **The answer that closes a turn says how long the turn took** (2026-09-14). Claude
  Code's own screen ends a turn with "worked for 42s"; a card showed only the clock stamps,
  so the length of a turn was a subtraction. Rulings: (1) the last answer before the next
  prompt (or the end of the transcript) carries "took 42 s" beside its stamp
  (`conversation-took-<ix>`, `conversation::turn_took` / `turn_label`: whole seconds under a
  minute, then `2 m 10 s`, then `1 h 02 m`), from the records' own stamps at both ends — the
  host's clock, so no client clock and no wire change; (2) an answer more answers follow says
  nothing (the turn is not over at it), nor does one whose prompt or self has no stamp, nor a
  pair whose clock went backwards. Tests: `a_turns_last_answer_says_how_long_the_turn_took`
  (pure), headless `a_turns_closing_answer_is_captioned_with_its_time`.

- ✅ **The composer types into the pty; no new wire message** (2026-09-05). ↩ sends the text
  as `TermRequest::Paste` (the host brackets it when the program asked, so Claude Code takes a
  multi-line message as one prompt) followed by the same `TermRequest::Key` Enter the keyboard
  sends, then clears. A `ClientMsg::Prompt` would have needed the host to know which program
  reads the pty and how it wants its input; typing keeps one input path, keeps slash commands
  and `@file` exactly as if typed, and works for any prompt the agent shows (an empty ↩ is a
  bare Enter that accepts it). Esc and Control keys keep their terminal meaning from inside the
  composer so ⌃C interrupts without leaving the chat; gpui-kit's input binds ⌃C to Copy only
  off macOS, so on iOS a hardware ⌃C in the composer copies and the key bar's ⌃ + C is the
  interrupt. Allow / Deny in the view raise `TerminalViewEvent::Answered` and the canvas types
  Enter / Esc through `allow_agent` / `deny_agent`, one answer per state, exactly as the badge.

- ✅ **↑ / ↓ in the composer recall the prompts sent, as a shell's history** (2026-09-13).
  A prompt is often the last one again with a word changed, and a shell reader's hand goes to
  ↑. Rulings: (1) ↑ on an **empty** composer shows the newest `User` entry, ↑ / ↓ from there
  the older / newer ones (`Conversation::recall`, `prompts()` — the empty ones and a repeat of
  the one before it dropped), the oldest stays put, and ↓ past the newest puts the draft back
  (`draft`, set aside on the first ↑); (2) with the human's own text under the caret — a draft,
  or a recalled prompt once typed into (`composer_changed` forgets the recall; gpui-kit's
  `set_value` emits no `Change`, so the recall itself does not) — ↑ / ↓ move the caret as they
  would, so a multi-line draft is still editable; (3) the caret lands at the end of a recalled
  prompt (`set_selected_range`); (4) the completion list takes ↑ / ↓ first while it is up; (5)
  sending forgets the recall; (6) the phone's bar ↑ / ↓ are the composer's too
  (`TerminalView::bar_key`: the completion list's while it is up, else a recall) — the program
  behind a conversation card is not on show for a raw arrow to mean anything there; the other
  bar keys, and every key in a shell, are pressed as before; (7) a click on a sent prompt's
  bubble (`conversation-reuse-<ix>`, "Edit and send again") puts it in the composer as if
  recalled to that point (`Conversation::reuse`), so ↑ / ↓ go on from it and the draft comes
  back past the newest — the prompt to send again is more often on screen than a count of
  ↑ away. Test: `the_composer_recalls_sent_prompts_on_up_and_down`.

- ✅ **A prompt sent mid-turn shows as queued** (2026-09-13). ↩ while the agent works still
  sends — Claude Code queues a prompt that arrives over stream-json mid-turn and takes it
  as its next turn (its interrupt reply even lists `still_queued`) — but the conversation
  showed nothing of it until that turn began, minutes later on a long one. Rulings: (1) a
  prompt sent while the status is `Working` or `Tool` is kept in `Conversation::queued` and
  drawn under the list and the partial as a faint bubble on the human's side captioned
  "queued" (`conversation-queued-<i>`, role Status, "Queued: first line"); (2) it leaves
  the queue when a `User` entry with its text arrives (a reset drops the lot), or when the
  status leaves the turn — `Done`, `Idle`, `None` — since a queued prompt the agent takes
  starts a turn of its own whose entry follows, and one it dropped will never come;
  `Blocked` mid-turn keeps the queue; (3) no "unsend": the agent holds the queue, and the
  client has no word for taking one back. Test:
  `a_prompt_sent_while_the_agent_works_waits_as_queued`.

- ✅ **A conversation copies out as Markdown** (2026-09-13). What an agent said is often
  pasted into a review, an issue or a note, and one answer more often than the lot. Rulings:
  (1) every answer ends in a "copy" button (`conversation-copy-<ix>`, "Copy answer") that
  puts its Markdown on the clipboard, as a fenced block's "copy" puts the code; ⌘⇧C keeps
  copying the newest answer; (2) the palette's "Copy conversation as Markdown"
  (`CopyConversation`, Terminal context, no key) writes `conversation::as_markdown`: "**You**"
  / "**Claude**" headed turns, a tool call as a quoted `> **Name** summary` line, a failed
  result's first line, a compaction as a rule with its token numbers, a notice quoted in
  italics — thinking and tool output stay out, as they are folded on screen; (3) no file
  export: the clipboard reaches every editor, and the transcript file itself is the agent's;
  (4) (2026-09-14) a "note" button next to "copy" (`conversation-note-<ix>`, "Keep answer as
  a card") keeps the answer on the canvas instead: the same `NoteBlock` event a command
  block's "Save as note" sends, so the card lands beside the agent's card with the answer's
  Markdown, its fences runnable while the canvas has a shell — an answer that is a plan or
  a runbook stays in view after the conversation has moved on.
  Tests: `the_conversation_exports_as_markdown_turns`,
  `the_conversation_replaces_the_grid_and_follows_the_transcript`.

- ✅ **Collapse defaults** (2026-09-05). Thinking and tool input start folded (they explain a
  step, they are not the step); a tool result shows its first 4 lines (`RESULT_PREVIEW_LINES`)
  because the head of a result is usually the verdict ("running 2 tests", "error[E0308]"), and
  opens to the whole clipped text with the dropped-line count. Folds live in the client (a
  `HashSet` of indices) and survive appends, a reset drops them with the entries. The list
  uses gpui's `FollowMode::Tail`: it stops following on a wheel-up, resumes when scrolled back
  to the bottom, and the "↓ latest" pill (`scroll_to_end` + `Tail`) is the shortcut.

- ✅ **Attribution without hooks: four signals, strongest wins, hooks never overruled**
  (2026-09-05). A `claude` the human started by hand — or one running before
  `slopty hook install` — now gets the same pill, badges, attention and conversation view as a
  hooked one. `AgentSource` (on the wire, `Process < Title < Transcript < Hook`) says which
  signal the status came from, and `Tracker::observe` / `observe_progress` refuse to let a
  weaker one overwrite a stronger one's state. `slopty-worker`'s `agents::watch` reads every session
  every 750 ms:
  - **Foreground process.** `tcgetpgrp` on the PTY master names the tty's foreground group;
    `slopty_pty::process` describes its leader (`proc_pidinfo PROC_PIDTBSDINFO` for the name
    and start time, the `KERN_PROCARGS2` sysctl for `argv`, `PROC_PIDVNODEPATHINFO` for the
    cwd) and `slopty_agent::detect` decides, as a pure function over name and `argv`, whether
    that is Claude Code. Present → `Idle`, gone → the agent is cleared.
  - **Title.** What Claude Code paints into OSC 0/2 separates a running turn from an idle
    prompt. Nothing finer is readable from a title, and that is all it is used for.
  - **Transcript.** `slopty_agent::discover` finds the JSONL from the session's own working
    directory and `transcript::progress` reads `Working` / `Tool` / `Done` out of the newest
    record. It can never report `Blocked`: a permission prompt is not written to the
    transcript until it has been answered. Blocking is what hooks are for, which is why the
    app still offers to install them.

- ✅ **The title's tables are `◐◑◒◓` for a running turn and `✳` for an idle one — not the
  in-pane spinner** (2026-09-05, from live `terminal_title` data on this machine: `◐ Claude
  Code` while working, `✳ GPUI and gpui-kit upstream sync track` when done). Claude Code
  paints a spinning half circle in front of the title while a turn runs and, between turns, a
  sparkle in front of the conversation's summary (the bare name before there is one). The
  `·✢✳∗✻✽` frames are the spinner it draws in its own output; they never reach the title, so
  a *title* that starts with one of them is some other program and is read as nothing. An
  earlier revision of `slopty_agent::title` had the two sets swapped, which read every idle
  agent as working and every working one as idle.

- ✅ **`detect::is_claude` reads both the executable name and `argv[0]`, and treats shells and
  JS runtimes as launchers** (2026-09-05, measured). The two disagree often: `/bin/sh` on
  macOS is `bash` by executable and `/bin/sh` by `argv[0]` (`slopty-pty`'s
  `a_ptys_foreground_process_is_the_program_it_runs` pins that), the kernel rewrites `argv`
  for a `#!` script so `~/.claude/local/claude` shows as `sh <script>`, the npm install shows
  as `node …/claude-code/cli.js`, and `slopty_pty::pty::resolve_command` itself starts an
  unfound `claude` as `zsh -lic claude`. So a name or `argv[0]` of `claude` counts outright,
  and a runtime counts only for the *first* argument that is not a flag — the script it runs —
  or, for a shell's `-c`, for the first word of the command it was handed. A bare login shell
  (`argv[0] = "-zsh"`) does not count, and neither does `sh -c 'echo claude'` or
  `node server.js --model claude`: the agent's name as somebody else's argument proves
  nothing.

- ✅ **The transcript is the newest `.jsonl` in the project directory, modified at or after
  the agent process started** (2026-09-05, verified against `~/.claude/projects` directory
  names only, never their contents). Claude Code escapes the working directory by replacing
  every character that is not an ASCII letter or digit with `-`
  (`/Users/x/.config` → `-Users-x--config`), which fixes the directory; time picks the file
  inside it, because the live session is the one still being written and a resumed
  conversation moves its old file's mtime forward. The start time comes from the process
  table, so a hostd restart does not make an old conversation look new. Only that one
  directory is ever read. Two `claude`s started in the same directory in the same window
  resolve to the same newest file, and the second one to write wins; the terminals are told
  apart by their sessions but their transcripts are not, which is a limit of the discovery
  and a reason the app offers the hooks.

- ✅ **The lookup is repeated, because `/clear` starts a new file** (2026-09-05). A tracker
  that already has a transcript keeps asking (`AgentTable::discoveries` carries the file
  being read, `Tracker::discovery` only stops for a hooked session whose hook named one), and
  hostd re-runs it every eighth tick (6 s); when the newest file in the project directory is
  not the one being tailed — `/clear`, `/resume <other>`, a compaction — the path moves and
  the `Tail` is dropped so the new conversation is read from its top. Without this the status
  froze on the old file's last record, so a cleared session sat on `Done` through its next
  turn.

- ✅ **A tracker follows a process, not a terminal** (2026-09-05). `Observation` carries the
  foreground process's pid and start time, and a tracker whose process changes is reset before
  it is attributed again: one `claude` exiting and another starting inside a 750 ms tick would
  otherwise inherit the first one's transcript, status and hooks in a terminal that looks
  unchanged. The reset also clears the transcript path, so the next lookup finds the new
  conversation and the tail starts over.

- ✅ **A poll event a hook has overtaken is dropped, not sent** (2026-09-05). hostd computes
  the tick's events under the lock and broadcasts them *before* it reads any file, and every
  event is checked against the table (`AgentTable::is_current`) immediately before it goes
  out. A hook arriving on the control socket between the poll and the send has already told
  every client something newer; without the check the poll's older state was put back and, for
  example, a permission badge vanished until the next hook.

- ✅ **Hooks decide the status; the process table may still end a session it watched**
  (2026-09-05). Once a hook has spoken, the weaker signals fill gaps only — the transcript
  path, so ⌘⇧L works whether it was named by a hook or discovered — and never change the
  status. Ending is nearly as strict: a hooked agent goes when `SessionEnd` says so, or when
  a `claude` that was actually seen in the tty's foreground has been absent for four probes
  (3 s). One probe is not enough because the relay (`slopty hook`) is itself briefly the
  foreground process of the terminal it reports on, and a session that only ever spoke
  through hooks (never seen as a process) is never ended this way at all — which is what
  keeps the played-hook self-tests honest, since those play hooks into a plain shell. The
  case this buys: a `claude` killed with `SIGKILL` sends no `SessionEnd` and would otherwise
  keep its pill until the terminal exited.

- ✅ **A first transcript read never raises attention** (2026-09-05). `Done` alerts only when
  the previous status was `Working` or `Tool`: the first read of a discovered file is usually
  a conversation that ended hours ago, and a Dock bounce for it would be a lie.

- ✅ **A driven agent is retuned in place and resumed from its transcript directory,
  protocol 16** (2026-09-12). Probed on CLI 2.1.269 over the same stdio protocol:
  `control_request` `set_model` and `set_permission_mode` are acknowledged without a restart
  (`{"subtype":"success"}`; the mode change is followed by a `system/status` record naming
  the mode), while `supported_models` and `supported_commands` answer "Unsupported control
  request subtype". Rulings: (1) the model list is a fixed table of Claude Code's own aliases
  (`fable`, `opus`, `sonnet`, `haiku`; `--help` documents the alias form) rather than a
  guessed set of full names, and the header chip shows the full name the agent's next
  assistant record carries, so a wrong alias shows as the agent's word, not ours; (2) the
  slash-command list is `init`'s `slash_commands`, sent once per session inside `AgentInfo`
  rather than re-asked; (3) the host answers a retune with the whole `AgentInfo` again, not
  a delta, since it is small and a client joining late needs the same message; (4) the cost
  rides as `cost_micro_usd: u64` so the wire type stays `Eq`; (5) "Resume" lists the host's
  `~/.claude/projects/<escaped cwd>` directory itself (the same escaping `discover` already
  verified) named by the first human prompt, with the rule that a transcript holding no
  prompt is not listed — Claude Code writes a file for a `/clear` or a hook run too, and
  resuming one of those shows an empty conversation; (6) the completion keys are caught in
  GPUI's capture phase as *actions* (`MoveUp`/`MoveDown`/`Escape` of gpui-kit's input) and
  not as key events: GPUI dispatches a matched key binding's action before the raw key
  event, so an `on_key_down` on the card never saw ↓ (the headless test caught it: Tab took
  the first match, not the second); (7) Tab completes as a `CompleteSlash` action bound in
  the "Terminal" context that propagates when there is nothing to complete, because
  gpui-kit's `Root` binds "tab" to its own focus-ring action and would have taken the key
  first (the app self-test caught it: Tab landed on "Send"); (8) the project directory is
  named by the *canonical* working directory (`discover::project_dir` resolves it when it
  exists): Claude Code names it by `process.cwd()`, which on macOS is `/private/var/…` for a
  `/var/…` the host was given, and the self-test's temp HOME is exactly such a path — the
  listing found nothing until then; (9) after ⌘W removes the active item, `reconcile` makes
  the canvas take the keyboard back on its next frame instead of leaving focus on the dead
  handle, which is why ⌘⌥R right after a close now lands (and why the arrange-by-repo golden
  moved: the remaining note draws as the active item, as it should); (10) a resumed card is
  seeded with the transcript's last entries before the agent speaks — Claude Code's
  `--resume` replays nothing over stream-json, and a card that opens empty on "Resume" is
  worse than no resume at all; (11) while the agent works the composer's Send is a Stop
  (`composer-stop`, `AgentInterrupt`), because on the phone a runaway turn had only the key
  bar's Esc, and the fake's `linger` turn is now ended by a click on it. The self-test's private
  `HOME` (also given to the driven launch now, the way the fake-claude launch already did)
  keeps the fake's transcripts out of the developer's `~/.claude`.

- ✅ **A tool call rides the wire as what it would do, not as its JSON, protocol 17**
  (2026-09-12). The card showed every tool call as a name, a one-line summary and its input
  as pretty JSON behind a fold — a `Bash` read fine, an `Edit` did not: the change was two
  JSON strings with escaped newlines, and the TUI's inline diff was the one thing the card
  had no answer to (the ruling above kept ⌘⇧T's TUI beside the card for exactly that).
  Rulings: (1) the host reads the input into a `ToolDetail` (`Command`, `Diff`, `Write`,
  `Read`, `Search`, `Todos`, `Agent`, `Json`) and the client draws the shape — the host
  already knows Claude Code's tools (it names their summaries), the diff is computed once for
  every client instead of on each, and a phone with no `similar` and no JSON parse in its
  paint path is the point; (2) the diff is `old_string` against `new_string` line by line
  (`similar` 3.2, `TextDiff::from_lines`), so a one-line change inside a ten-line
  `old_string` reads as context around one removed and one added, cut to the same 40 lines /
  4 000 characters as any long text with the count of what was dropped; (3) a known tool
  whose input lacks the field it is known by degrades to `Json`, never to an empty block, so
  a shape change on the agent's side shows what the card showed before; (4) a `Todo` with
  no text is dropped and the status is Claude Code's three (`pending`, `in_progress`,
  `completed`); (5) the `PermissionRequest` carries the same `ToolDetail` in place of its
  JSON, so the row and the entry never disagree; (6) on the card an edit's diff and a todo
  list show *unasked* — the diff is the point of the entry — the diff folded past 12 lines
  (`DIFF_PREVIEW_LINES`, three times a result's 4: a diff is read whole or not at all),
  everything else opens on the click it always did; (7) a screen reader hears the counts,
  not the lines. Wire: `TranscriptBody::ToolUse.input` → `detail`,
  `PermissionRequest.input` → `detail`, goldens `host_transcript` /
  `host_agent_permission` / `client_hello` re-accepted and `host_transcript_tools` (every
  shape) added, PROTOCOL_VERSION 16 → 17. Tests:
  `an_edit_is_a_diff_a_todo_list_a_checklist_and_a_stranger_its_json`,
  `a_long_diff_is_cut_like_any_long_text` (`slopty_agent::transcript`), headless
  `an_edit_shows_its_diff_and_a_todo_list_its_checklist` (tinted rows counted in the scene,
  the fold, the labels), and the app self-test's `edit` turn against the fake (an `Edit` and
  a `TodoWrite` the settings allow, golden `conversation-tools`).

- ✅ **A question is answered on the card, not allowed, protocol 18** (2026-09-12).
  Probed on CLI 2.1.269 over stdio: `AskUserQuestion` arrives as an ordinary `can_use_tool`
  control request (`tool_name: "AskUserQuestion"`, `requires_user_interaction: true`, the
  input `{questions: [{question, header, options: [{label, description}], multiSelect}]}`),
  and the answer is an *allow* whose `updatedInput` is the input plus
  `answers: {"<question text>": "<label>"}`; the agent then receives the tool result "Your
  questions have been answered: "…"="…". You can now continue with these answers in mind."
  and goes on. Before this the card offered Allow / Deny for it: Allow sent the input back
  with no answers, which is a question nobody answered. Rulings: (1) the fold reports the
  request as `Blocked(Question)`, the same reason the hook path uses for the TUI's question,
  so the badge, the notification ("Claude has a question", an "answer" pill that reveals the
  card, no Allow / Deny buttons) and the caret-in-composer rule already apply; the
  `PermissionRequest` still rides beside it, carrying the `ToolDetail::Question`; (2) the
  options are buttons, and a single-select question is answered by the one tap — the
  fewest gestures on a phone — while a multi-select one toggles and sends on Answer, which
  is disabled until every question has a pick; (3) typed text is the "Other" answer to the
  first question without one, since the TUI offers exactly that and a prompt while the
  agent waits would be lost anyway; (4) the answer is filed under the question's text,
  never its index, because that is the key Claude Code reads (the probe confirmed the
  wording of the result); labels of a multi-select are joined with ", " (the SDK's
  convention; unverified on the wire, marked in `Question::multi`'s doc); (5) the wire
  carries `answers: Vec<QuestionAnswer>` on `AgentAnswer` (empty for a permission) rather
  than a new message, so the host's one answer path and the "answered once" rule serve both.
  Wire: `ToolDetail::Question`, `Question`, `Choice`, `QuestionAnswer`, goldens
  `client_agent_answer` / `client_hello` / `host_transcript_tools` re-accepted,
  `client_agent_answer_question` and `host_agent_question` added, PROTOCOL_VERSION 17 → 18.
  Tests: `a_question_blocks_as_a_question_and_the_answers_are_filed_into_the_input`
  (`slopty_agent::stream`, on the probe's line), headless
  `a_driven_view_answers_a_question_in_place` (one tap; two questions with a multi-select
  and Answer; typed text), the app self-test's `ask me` turn against the fake (the options
  in the accessibility tree, no Allow, the tap on "Blue", the result's wording, golden
  `conversation-question`).

- ✅ **"Always" is the agent's own suggestion echoed back, protocol 19** (2026-09-12).
  Probed on CLI 2.1.269: a `can_use_tool` for `Write` carries
  `permission_suggestions: [{type: "setMode", mode: "acceptEdits", destination: "session"}]`;
  an allow whose response adds `updatedPermissions: <those suggestions>` made the next
  `Write` of the same turn run without asking, and a `system/status` record naming
  `acceptEdits` followed (the mode chip moves through the path protocol 16 built). The TUI
  offers exactly this as its "Yes, and don't ask again" line; a card that can only say yes
  once makes the human answer every edit of a long turn from the phone. Rulings: (1) the
  host never invents a rule — it echoes the agent's suggestions verbatim, so what "Always"
  does is what the TUI would have done and nothing wider; (2) the suggestions stay on the
  host beside the input (`stream::Pending`) and the wire carries only their meaning in the
  human's words (`PermissionRequest::always`, `always_label`: mode + destination, or the
  rules as `Tool(content)` + destination), so the client draws a sentence and the phone
  never parses a permission schema; (3) no suggestion, no button — `always: None` — and an
  `always: true` on such a request is a plain allow, never an error; (4) the button names
  its effect under the word ("Always" / "accept edits for this session") and a screen
  reader hears both, because a permission taken for the whole session is the one answer
  worth a second's reading; (5) `AgentAnswer` became a struct (`session, request, allowed,
  message, answers, always`), the way `AgentSet` already was, once it reached six fields.
  Wire: `PermissionRequest.always`, `AgentAnswer.always`, `ClientMsg::AgentAnswer(AgentAnswer)`,
  goldens `client_agent_answer` / `client_agent_answer_question` / `client_hello` /
  `host_agent_permission` / `host_agent_question` re-accepted, `client_agent_answer_always`
  added, PROTOCOL_VERSION 18 → 19. Tests:
  `an_always_allow_echoes_the_agents_suggestion_and_a_plain_one_does_not` (the probe's
  line, the bare line, the rule wording), headless `a_driven_view_speaks_to_the_agent`
  (Always between Allow and Deny in the tree, one tap answers for good), the app self-test
  (`write once more` taken with Always: the chip reads Accept edits; `write yet again`
  asks nothing).

- ✅ **A subagent is followed under its call, never shown as the agent, protocol 20**
  (2026-09-12). Probed on CLI 2.1.269 with an `Agent` (Explore) call: the subagent's own
  `assistant` and `user` records stream on the same stdout with `parent_tool_use_id` set to
  the spawning call, and around them come `system/task_started` (`tool_use_id`,
  `description`, `subagent_type`, `prompt`, `is_backgrounded`) and `system/task_progress`
  (`usage.tool_uses`, `usage.duration_ms`, `last_tool_name`, a "Running …" description);
  no `task_completed` was seen — the call's own `tool_result` ends it. Before this the card
  showed the subagent's thinking, tool calls and results inline as the agent's, moved the
  status chip to the subagent's tools, and the last assistant line could be the
  subagent's. Rulings: (1) `is_subagent` (sidechain in the file, `parent_tool_use_id` on
  the stream) gates entries, progress and the model in one place — the transcript reader —
  so the JSONL tail and the stream can never disagree; (2) the progress is one
  `AgentTask` per spawning call, upserted (a start after progress keeps the counts) and
  marked done by the result, sent whole each time like `AgentInfo`, and kept per session so
  a client joining mid-run sees the running subagents; (3) the card needs the call id to
  hang the task on, so `ToolUse` carries `call` — the same `tool_use_id` the results are
  named after — rather than the client guessing by order, which breaks with two subagents;
  (4) the line under the call says kind, state, tool uses, seconds and last tool and the
  label says the same, because on the phone the subagent's minutes are the only sign the
  turn is alive. Wire: `TranscriptBody::ToolUse.call`, `AgentTask`, `WorkerMsg::AgentTask`,
  goldens `host_transcript` / `host_transcript_tools` / `host_agent_sessions` /
  `client_hello` re-accepted, `host_agent_task` added, PROTOCOL_VERSION 19 → 20. Tests:
  `a_subagents_own_records_are_not_entries` (`transcript`),
  `a_subagent_is_followed_by_its_task_records_and_its_own_are_hidden` (`stream`, the
  probe's six lines), headless `a_subagent_progresses_under_its_call` (the label through
  running and done, the reset), the app self-test's `delegate the listing` turn (the
  subagent's Bash absent, the task done in the dump and in the tree).

- ✅ **The card shows the subscription's windows and what the agent is doing between tokens,
  protocol 21** (2026-09-12). Probed on CLI 2.1.269: right after `system/init` comes a
  `rate_limit_event` with `rate_limit_info.unifiedWindows.five_hour` / `seven_day`
  (`utilization` 0–1, `resetsAt` epoch seconds) and a `status` that reads `rejected` when the
  subscription is spent; an API-key run never sends one. And each turn starts with
  `system/status { status: "requesting" }` (and `compacting` around auto-compaction), before
  any content: until now the chip sat on "working" with an empty card and a stalled agent
  looked the same as one waiting on the model. Rulings: (1) the windows ride in `AgentInfo`
  as `usage: Option<Usage>` — whole percent per window, computed as the smallest integer not
  below the fraction (no float cast, so no lint carve-out) — because it is what the agent says
  about itself and a joining client needs it with the snapshot; (2) the chip says both
  windows tersely ("5h 23% · 7d 74%") and only when limited names the earliest reset
  ("limited until 14:00"), since on the phone the question is "can I keep going" not the
  raw numbers; (3) `requesting` / `compacting` become the Working detail and never touch a
  Blocked status, so a permission or a question is not overwritten by the next poll. Wire:
  `AgentInfo.usage`, `Usage`, `UsageWindow`; goldens `host_agent_info` / `client_hello`
  re-accepted, PROTOCOL_VERSION 20 → 21. Tests:
  `a_policy_denial_and_the_rest_are_named_or_ignored` (`stream`: the probe's rate-limit
  line, the bare rejected one, `requesting` and `compacting` as detail and not over a
  permission), headless `a_driven_view_shows_the_agent_and_retunes_it` (the usage chip in the
  tree), the app self-test (the fake sends the probe's event after init; the dump's `usage`
  reads "5h 23% · 7d 74%").

- ✅ **A picture pasted into the composer goes to the agent as an image block, protocol 23**
  (2026-09-12). Probed on CLI 2.1.269: a stream-json user message whose content is
  `[{type: image, source: {type: base64, media_type, data}}, {type: text}]` is accepted,
  replayed as sent, written to the transcript as sent, and the model read a 64×64 PNG ("What
  colour is this image?" → "Blue"). On the phone a screenshot is the fastest way to show the
  agent something; without this the card could only type. Rulings: (1) the client decides
  what is a picture — ⌘V captures the input's `Paste` action and takes the clipboard's image
  entries, letting a text clipboard fall through to the input — rather than the host sniffing
  bytes, because the clipboard's format is known where it is read; (2) only the types the
  model reads are attached (PNG, JPEG, GIF, WebP): a TIFF, what a copied macOS screenshot
  can be, is dropped with a log line rather than transcoded, until a measurement says the
  paste path needs it; (3) caps live in `slopty-proto` (`IMAGES_MAX` 4, `IMAGE_BYTES_MAX`
  4 MiB, under the model's 5 MB with room for the JSON) and the host refuses a prompt over
  them whole — a half-sent prompt would be worse than none; (4) the wire carries the bytes
  only client → host: the transcript entry says how many pictures went with the prompt, so
  a joining client and a resumed card show "1 picture" without shipping megabytes back, and
  the host's own user entry and the agent's replay agree (`is_replay` compares the count).
  Wire: `AgentSay.images: Vec<Image>`, `TranscriptBody::User.images`; goldens
  `client_agent_say` / `host_transcript` / `client_hello` re-accepted, PROTOCOL_VERSION
  22 → 23. Tests: `tool_results_are_named_after_their_call_and_injected_texts_are_not_entries`
  (`transcript`: a picture with text is one entry, two alone are an entry of their own),
  `a_policy_denial_and_the_rest_are_named_or_ignored` (`stream`: the blocks, no empty text
  block), headless `a_picture_pasted_into_the_composer_goes_with_the_prompt` (chip label,
  the tap, text still text, TIFF refused, sent with the text and alone), the app self-test
  (the socket's `attach`, the chip in the dump, the fake reading back "image/png 70 B", the
  bubble's "1 picture").

- ✅ **The phone pastes a screenshot through the key bar; the fork reads pictures off
  `UIPasteboard`** (2026-09-12). The fork's `gpui_ios` clipboard was text-only both ways, and
  the key bar's "paste" on a driven card sent a `TermRequest::Paste` to a session the agent
  does not have. Rulings: (1) the fork reads a picture as the encoded bytes under the
  format's uniform type (`public.png`, `public.jpeg`, `com.compuserve.gif`,
  `org.webmproject.webp`, then TIFF and BMP) when `hasImages`, before text — a screenshot is
  a PNG and the bytes are what the model wants, so no `UIImage` round trip that would
  re-encode; it writes an `Image` entry the same way (`setData:forPasteboardType:`), which
  is what lets a test put a picture on the simulator's own pasteboard through GPUI instead of
  `simctl pbcopy` from the Mac's (fork `f629234166` on `8ffbc6e145`, pin moved; first try
  crashed the app on the simulator: `-[NSData bytes]` received as `*const u8` fails objc2's
  debug-build encoding check, it must be `*const c_void` then cast); (2) on a
  driven card `paste_clipboard` is the composer's paste: pictures attach, text is inserted at
  the caret (`Conversation::insert_composer_text`), nothing goes to a session; (3) the Mac
  scenario keeps using the socket's `attach` because the Mac pasteboard is every app's.
  Tests: headless `a_picture_pasted_into_the_composer_goes_with_the_prompt` (the key bar
  path: a picture attached, text in the composer, no session paste), the simulator's
  `the_driven_agent_card_on_the_simulator` (the socket's `clipboard`, a finger on "Paste",
  the chip, ↩ and the fake reading back "image/png 70 B" — the fork's read and write on a
  real `UIPasteboard`).

- ✅ **A pasted picture is made fit off the UI thread, and a refused one says why**
  (2026-09-12). A phone screenshot is a 2–6 MB PNG, a photo a 3–12 MB JPEG; the first cut
  dropped anything over the 4 MiB cap with a log line nobody sees, and pasted a TIFF into
  silence. Rulings: (1) `terminal::attachment::fit` decodes the picture and, when it is over
  1568 px on the long side (what the model reads at full detail) or over the cap, shrinks it
  (Triangle: a few times faster than Lanczos, indistinguishable to the model) and re-encodes
  it — PNG when it has transparency, JPEG q85 otherwise, q70 as a second try — so the wire
  carries hundreds of KB, not megabytes; a picture already inside both bounds passes through
  untouched, because re-encoding what fits would only lose; (2) the fit runs in
  `cx.background_spawn`, with the count of pictures in flight drawn as a muted "preparing…"
  chip, because decoding a 12 MB JPEG on the UI thread is a visible hitch on a phone; (3)
  what is refused — a type the model does not read, undecodable bytes, a fifth picture —
  reaches the top bar as a notice through a new `TerminalViewEvent::Notice` /
  `CanvasEvent::Notice` pair, the route `HooksInstalled` already used; the fifth is refused
  before any decoding; (4) `slopty-ui` takes `image` with the four decoders, which GPUI
  already builds, so the binaries grow by nothing; the e2e scenarios paste a real 64×48 PNG
  (`snapshot::tiny_png`) since made-up bytes no longer decode. Tests: `attachment` (a small
  picture passes through byte for byte, 3200×1400 → 1568×686 JPEG, transparent 600×2000 →
  470×1568 PNG, a TIFF and garbage refused by name), headless
  `a_picture_pasted_into_the_composer_goes_with_the_prompt` (the big one lands shrunk as a
  JPEG, the TIFF's and the fifth's notices), the app and simulator scenarios on the real
  PNG.

- ✅ **An edit knows its line in the file, protocol 35** (2026-09-12). The card drew an
  edit's diff but could not say *where* in the file it was: Claude Code's `Edit` names a file
  and an `old_string`, never a line. Rulings: (1) the host looks it up — `transcript::locate`
  reads the file (up to 4 MiB, text only) and takes the 1-based line where `old_string`
  starts, else where `new_string` does, because a transcript read back from disk sees the
  file *after* the edit landed while a live `tool_use` sees it before; neither found leaves
  `line: None`; (2) a relative `file_path` resolves against the record's own `cwd`
  (`record_entries`), or the fold's `init.cwd` for stream-json records that carry none
  (`record_entries_in`), and stays unknown with neither; (3) the same lookup fills a
  `can_use_tool` permission's detail, so the Allow / Deny row's "open" and "view" land right
  too; (4) the client uses it twice — "open" types `${EDITOR:-vi} +N`, "view" lands the file
  card on the line (`TerminalViewEvent::ViewFile { path, line }`, `CanvasView::open_file`,
  `FileView::focus_line`: the row in the accent tint, scrolled to the centre once the text is
  there, `canvas.file_focus` holding the line for a view not made yet) — and ⌘-click on
  `src/main.rs:12` in a terminal while a command runs carries its own `:12` the same way.
  Wire: `ToolDetail::Diff.line: Option<u32>`; golden `host_transcript_tools`. Tests:
  `an_edits_line_is_found_in_the_file_before_and_after_it_lands` (old text, new text,
  neither, relative + cwd, a whole record), the file-card headless test (a `Diff` with line 2
  lands the card on index 1), `cmd_click_on_a_path_while_a_command_runs_views_it` (`:12`
  carried).

- ✅ **A picture dropped from the desktop onto a driven card is attached** (2026-09-13). A
  screenshot on the Mac's desktop went to the agent by opening it, copying it and ⌘V in the
  composer; Finder to card is the gesture every Mac app takes. Rulings: (1) the card's root
  takes GPUI's `ExternalPaths` drop (`drop_paths`): a driven card reads each picture file off
  the UI thread and hands it to `attach_image`, so the fit, the cap of `IMAGES_MAX` and the
  chip are the paste's; (2) the type is read from the extension (`picture_type`: png, jpg,
  jpeg, gif, webp, any case) and any other file is refused by name in the top bar — the
  model reads pictures, and a text file's place is `@path` on the host, not the client's
  copy; a file that cannot be read says so with the OS's word; (3) a shell card takes no
  files at all, not even a notice: the path is the client machine's and the shell runs on
  the host, so typing it there would name nothing. Tests: headless
  `a_picture_dropped_on_a_driven_card_is_attached` (the extension table; a PNG, a text file
  and a missing file dropped together on a shell card, then a driven one: one attachment
  with the file's bytes, two notices, the chip labelled).

- ✅ **A host window's picture goes to the agent without touching the client, protocol 33**
  (2026-09-12). "What is wrong with this dialog?" wants the window as the human sees it; the
  client only has the stream's decoded frame, at stream size, in a pixel buffer. Rulings: (1)
  `AgentSay::snapshots` names windows or displays (`CaptureTarget`), and the host takes each
  picture as it sends the prompt: `Target::snapshot` asks `SCScreenshotManager` for one image
  through the same content filter a stream would use, at native pixel size, cursor off,
  BGRA (`slopty_capture::Picture`); hostd's `snapshot::encode` turns it into RGBA, scales it
  to the model's 1568 px longest side, and writes PNG (JPEG at 85 only when the PNG would not
  fit `IMAGE_BYTES_MAX`), then the images join the prompt's own; a target that fails is
  logged and skipped, the prompt still goes. Nothing crosses the wire but the target id, and
  the phone gets the same feature for free; (2) the pictures are counted with the composer's
  own against `IMAGES_MAX`, one per target; (3) the "ask" pill on a window or display card
  (`ask-<uuid>`, "Ask the agent about this window"), and the palette line of that name for
  the active item, attach it to the agent card the human is on, else the topmost, else one the
  canvas opens (`CanvasView::ask_agent_about`, the same route as a command block's "Ask the
  agent", now an `Ask` of words or a picture); the composer shows a `🖥 <title> ×` chip
  (`composer-snapshot-<i>`, "Remove window <title>") until ↩ sends it. Tests:
  `a_window_is_asked_of_the_agent_as_a_snapshot` (canvas, headless),
  `a_wide_bgra_picture_is_scaled_and_recoloured` (hostd, the encoder), golden
  `client_agent_say`. Not tested: the screenshot call itself (needs Screen Recording; the
  live `cargo xtask e2e screen` suite is where a check would go).
- ✅ **The badge says what the agent is doing between records: thinking, or which tool it is
  composing** (2026-09-12). With `--include-partial-messages` Claude Code streams every
  content block's opening (`content_block_start` with `content_block.type` `thinking`,
  `text` or `tool_use` + `name`) before any delta, and a long think or a large tool input
  (a whole file for `Write`) can take many seconds during which only `text_delta` was read:
  the badge sat on "working" — and the protocol 21 "waiting for the model…" detail was never
  drawn either, since the badge text ignored a Working detail. Rulings: (1) `Block::{Thinking,
  Text, Tool(name)}` parse into `Event::BlockStart`, and the fold turns them into the Working
  detail ("thinking…", "calling Write…", none for text — the partial says it) under the
  protocol 21 rule: never over a Blocked or Done status; (2) the badge shows the Working
  detail when there is one ("working" otherwise), so `requesting`, `compacting`, a thought
  and a tool being composed all move the chip; (3) thinking deltas are not shown — Claude
  Code's own UI hides them — and no wire change: `AgentEvent.detail` already carries it.
  Tests: `text_deltas_stream_the_partial_and_the_record_clears_it` and
  `a_policy_denial_and_the_rest_are_named_or_ignored` (`stream`: the openings as detail, a
  tool opening not over a permission), the app self-test's `ponder` turn (the fake opens a
  thinking block and waits; the dump's new `agent_detail` reads "thinking…", Stop interrupts
  it). Every fake turn now opens its text block first, as the real CLI does.

- ✅ **The card shows how full the context window is, protocol 24** (2026-09-12). Claude
  Code's own status line answers "how much room is left" and Slopty's card did not: the
  only way to know a compaction was near was to be surprised by it. Probed on CLI 2.1.269:
  every assistant record's `message.usage` carries `input_tokens`,
  `cache_creation_input_tokens` and `cache_read_input_tokens`, whose sum is the context the
  request carried (the model's view of the conversation), and each `result` carries
  `modelUsage.<model>.contextWindow` (200 000 for haiku, 1 000 000 for the 1M models). There
  is also a `get_context_usage` control request with a per-category breakdown, but it is
  answered only where a callback is registered (the SDK host), not over stdio, and a
  round-trip per turn buys nothing the records do not say. Rulings: (1) `AgentInfo.context:
  Option<Context { tokens, window: Option<u64> }>` — the sum from the latest assistant
  record, the widest window the turn's `modelUsage` named (the main model's; a haiku subagent
  never widens it); it rides in `AgentInfo` because a joining client needs it with the
  snapshot; (2) the chip reads "ctx 16%" once the window is known and "ctx 31k" before the
  first result (never a guessed window), in the warn tone from 80% since auto-compaction is
  near; (3) the fold reports only changes, and a compaction needs no special case: the next
  assistant record's smaller sum lowers the chip by itself. Wire: `AgentInfo.context`,
  `Context`; goldens `host_agent_info` / `client_hello` re-accepted, PROTOCOL_VERSION 23 →
  24. Tests: `the_context_fill_follows_the_usage_and_the_result_names_the_window` (`stream`),
  headless `a_driven_view_shows_the_agent_and_retunes_it` (the chip in the tree, the labels),
  the app self-test (the fake's records carry 40 000 tokens against a 200 000 window: "ctx
  20%" in the dump's `context`).

- ✅ **A compaction is a divider in the card, not a prompt the human never typed, protocol
  25** (2026-09-12). When Claude Code compacts (auto, or `/compact`) it writes a
  `system/compact_boundary` record — on stdout with `compact_metadata { trigger, pre_tokens,
  post_tokens }`, in the transcript file with `compactMetadata { trigger, preTokens,
  postTokens }` — followed by a user record flagged `isCompactSummary: true` whose content
  is the summary ("This session is being continued…"). Until now the boundary was dropped
  (no `message`) and the summary drew as a You bubble of several screens. Rulings: (1)
  `TranscriptBody::Compacted { trigger, pre_tokens, post_tokens }` is an entry, drawn as a
  ruled divider "compacted (auto): 167k → 12k" (label "Compacted (auto): …"), so the card
  shows where the agent's memory of the conversation became a summary; (2) the flagged
  summary record yields no entry — it is the agent's reading, not the human's words — and
  nothing else about it is special-cased (no text-prefix sniffing); (3) the boundary's
  `post_tokens` lowers the context chip at once rather than waiting for the next assistant
  record. Both key spellings are read since the fold sees the stream live and the file on
  resume. Wire: `TranscriptBody::Compacted`; goldens `host_transcript` / `client_hello`
  re-accepted, PROTOCOL_VERSION 24 → 25. Tests:
  `tool_results_are_named_after_their_call_and_injected_texts_are_not_entries` (`transcript`:
  the file spelling, the summary hidden),
  `the_context_fill_follows_the_usage_and_the_result_names_the_window` (`stream`: the stream
  spelling, the chip lowered, the entry stamped), the app self-test's `/compact` turn (the
  divider last, "ctx 3%", no "continued from" entry, and the divider in the resumed past).

- ✅ **What the loop says, not the model, is a notice line in the card; the slash list and a
  backgrounded task follow their records, protocol 26** (2026-09-12). Read from the CLI
  2.1.269 bundle's own schemas (`strings` on the binary, the zod descriptions): a hook's
  feedback, a slash command's output and non-error status lines arrive as
  `system/informational { content, level: info|notice|suggestion|warning, tool_use_id?,
  prevent_continuation? }` ("Hosts render `content` as plaintext at the given level"; hook
  feedback is spelled "<Hook> says: …"); a turn moved to the fallback model as
  `system/model_fallback { trigger, original_model, fallback_model, content }`; a retry after
  a mode change allowed denied commands as `system/permission_retry { content, commands }`;
  a mid-session change of the slash list as `system/commands_changed { commands: [{name,
  description, argumentHint}] }` ("clients should REPLACE their cached command list"); a
  backgrounded task's end as `system/task_notification { tool_use_id?, status, summary,
  usage }`. All were dropped before, so a Stop hook's reason or "Allowed cargo test" never
  reached the card and a background subagent stayed "running" forever. Rulings: (1)
  `TranscriptBody::Notice { level: NoticeLevel::{Notice, Suggestion, Warning}, text }` is an
  entry drawn as one plain line in the muted, accent or warn tone; `info` lines are not
  entries (Claude Code shows them only in its transcript mode) nor are lines keyed to a
  `tool_use_id` (progress that would repeat); (2) a fallback reads "Switched to <model> for
  this turn: <why>" as a warning, a retry as a notice; (3) `commands_changed` replaces
  `AgentInfo.slash_commands` whole, names given their slash; (4) `task_notification` marks
  the task done and keeps the brief and kind already known — one without a `tool_use_id`
  cannot be joined and is ignored; the fold now ORs `done` so a late progress record never
  un-finishes a task. Wire: `TranscriptBody::Notice`, `NoticeLevel`; goldens
  `host_transcript` / `client_hello` re-accepted, PROTOCOL_VERSION 25 → 26. Tests:
  `tool_results_are_named_after_their_call_and_injected_texts_are_not_entries`
  (`transcript`: the hook line, the `info` and keyed lines skipped, the fallback, the retry),
  `a_subagent_is_followed_by_its_task_records_and_its_own_are_hidden` (`stream`: the
  notification ends the task, one without a call is nothing, the command list), the app
  self-test's `hooked` turn (the warning line between the prompt and the answer).
  Two more warning lines, no wire change: a `system/status` with `compact_result: failed`
  reads "Compaction failed: <compact_error>" (the `compacting` detail alone would have left
  the card looking as if it had worked), and a `system/stop_hook_summary` whose
  `hook_errors` is non-empty reads "Stop hook failed: …" — the summary is otherwise not an
  entry, since a hook that ran already spoke through `informational`.
  Read but not acted on: `control_request/request_user_dialog` (plan approval, MCP
  elicitation links, fallback-model retry dialogs) is sent only to a client that declared
  `supportedDialogKinds` in `initialize`, which Slopty does not, so it cannot leave the
  agent hanging on an unanswered dialog; `get_context_usage`, `rewind_files` and
  `rewind_conversation` need an SDK-host callback and are not answered over stdio.

- ✅ **A driven agent that dies on its own says why, protocol 27** (2026-09-12). When
  Claude Code exited without being asked — a lost login, an unknown `--resume` id, a crash —
  the pump broadcast the same `SessionClosed { reason: Exited }` as a ⌘W and the card simply
  vanished: a conversation gone with no word. Rulings: (1) `CloseReason::Failed { status,
  detail }` is a fourth reason, sent only by the driven pump and only when no `Close` was
  asked and the status is non-zero (a shell's own `exit 1` stays `Exited`; the reason is a
  *driven* failure); (2) `detail` is the agent's last non-empty stderr line, truncated the
  way every detail is, because that is where Claude Code says "Not logged in"; (3) the
  client shows it as the top-bar notice ("Claude Code exited with status 3: …") and closes
  the card as before — the transcript is on disk and ⌘⌥R resumes it — rather than keeping a
  dead card open. The `slopty` CLI's attach prints "failed". Wire: `CloseReason::Failed`
  (the enum loses `Copy`); golden `host_session_closed_failed` added, `client_hello`
  re-accepted, PROTOCOL_VERSION 26 → 27. Tests: the app self-test's `die` turn (the fake
  writes "fake: not logged in" to stderr and exits 3; the card is gone and the dump's
  `notice` reads the status and the line).

- ✅ **Slash completions say what a command does and takes, protocol 29** (2026-09-12).
  The list was bare names in chips; Claude Code has forty-odd commands and skills, and a name
  like `/compact` says nothing about its optional instructions. Read from the CLI (2.1.269):
  `system/init` carries `slash_commands` as names alone, and `system/commands_changed` (sent
  once skills are loaded and after any change) carries `{name, description, argumentHint,
  aliases?}` for the whole list. Rulings: (1) `AgentInfo.slash_commands` is
  `Vec<SlashCommand { name, description, hint }>` — the init fills names only and the next
  `commands_changed` replaces the list whole, so a card may show bare names for a moment and
  then the described ones; (2) the completion list is one line per command instead of
  wrapped chips: the name in the mono face, the hint and the description muted after it, the
  description ellipsised, and its a11y label is `slash_label` (`/compact [instructions] —
  Clear history but keep a summary`) so the dump and a screen reader read the same line;
  (3) `slash_matches` caps the list at `SLASH_MAX` = 8 — a prefix narrows it fast and a longer
  list would push the transcript off the top; (4) completing still puts `/name ` in the
  composer, the caret after the space, so a command with a hint is ready for its argument
  and one without is ready for ↩; (5) aliases are not carried — the CLI lists each alias as
  its own command already. Wire: `SlashCommand`; goldens `host_agent_info` / `client_hello`
  re-accepted, PROTOCOL_VERSION 28 → 29. Tests: `parse` of a described `commands_changed`
  (`agent`), the capped and case-insensitive `slash_matches` and the described
  `ListBoxOption`s in `a_driven_view_shows_the_agent_and_retunes_it` (headless), and the
  app self-test's driven scenario, where the fake describes its three commands after its
  init and the dump's a11y carries the described line.

- ✅ **The composer completes `@file` from the host's working directory, protocol 30**
  (2026-09-12). Claude Code expands `@path` in a prompt into the file's contents, which is
  how a human points the agent at a file; the composer took the text as typed, so the path
  had to be known and spelled out — on a phone, from memory. Rulings: (1) the word the text
  ends in, when it starts with `@`, is asked of the host as it is typed (`ListFiles {
  session, query }`, once per distinct query; a `mail@x` mid-word is not an `@` word and an
  empty query asks nothing); (2) the host answers from the driven session's working
  directory (`Driven::cwd`), off the runtime, with `slopty_agent::files::matching`: the
  `ignore` walker (hidden entries and `.gitignore` rules skipped, `require_git(false)` so a
  plain directory's ignore file counts too), depth 8 and 20 000 entries at most so a home
  directory ends, case-insensitive substring on the relative path, a path whose last
  component starts with the query first and shorter paths first among equals, at most 8
  (`FILES_LISTED`), a directory ending in `/` so the next keystrokes can descend; (3) the
  answer carries its query back and the view keeps it only while that is still the word
  the composer ends in, so a slow answer to an old query never lists under a new one; (4)
  the paths list in the same box as the slash commands (`Completion { insert, hint,
  description }` is the list's row; slash commands fill all three, paths the insert) and
  Tab replaces the `@` word alone, leaving the sentence around it, with a space after so
  the next word can follow — none after a directory, so the word goes on and the host is
  asked for what is inside it; (5) slash commands win when both would match — a `/…` text is
  never an `@` word. Wire: `ListFiles` / `Files`; goldens `client_list_files`,
  `host_files`, `client_hello` (re-accepted), PROTOCOL_VERSION 29 → 30. Tests:
  `a_name_that_starts_with_the_query_ranks_first_and_ignored_paths_are_skipped` (`agent`,
  a temp tree), the `@` half of `a_driven_view_shows_the_agent_and_retunes_it` (headless:
  the queries sent as the word grows, the stale answer dropped, the word replaced) and the
  app self-test's driven scenario (a file in the private home, `see @no` → `@notes.txt`,
  Tab → `see @notes.txt `).

- ✅ **A fenced block in an answer is its own element, with a copy button** (2026-09-12). An
  agent's answer is often "run this:" followed by a command, and the only way to get it into
  a shell was to select it by hand out of gpui-kit's markdown view — on a phone, not at all.
  Rulings: (1) an assistant turn is split at its ``` fences before rendering
  (`conversation::segments`: a line starting with ``` opens a block whose language is the
  rest of the line, the next such line closes it, an unclosed one runs to the end, blank
  prose between blocks is dropped); the prose segments still go through `TextView::markdown`,
  the code segments are drawn by Slopty — the same raised surface, mono face and `small()`
  size the markdown style gave a code block, so nothing moves — with the language and a
  "copy" button (a11y "Copy code") in a header row; (2) copy takes the lines inside the
  fences alone, no trailing newline, so it pastes as one command; (3) gpui-kit's markdown
  view was not forked for a per-block button — the split is thirty lines and leaves the fork
  in sync with upstream. "Run in the shell" — which shell to name — is settled by "A fenced
  block runs in the canvas's shell" below. Tests: `segments` cases and the button's click
  reading back from the headless clipboard
  (`the_conversation_view_lists_the_transcript_and_toggles_back`), and the app self-test's
  driven scenario (`snippet`: the fake answers with a fence, the dump's a11y carries "Copy
  code").

- ✅ **A conversation is resumed from any directory on the host, protocol 22** (2026-09-12).
  ⌘⌥R listed the active terminal's directory, and the daemon's default without one: on the
  phone, where there is no terminal to stand in, that meant one directory forever, and on the
  Mac a conversation in another project could not be reached without opening a shell there
  first. Rulings: (1) `ListAgentSessions { cwd: None }` now means every directory on the host,
  not the daemon's default — the phone's natural case, and the answer names its scope
  (`AgentSessions.cwd: Option<String>`) so the picker knows whether to offer more; (2) a
  directory's list keeps its focus and ends with one "Every directory" row that re-asks with
  `None`, rather than always listing the host, because the thirty newest across every project
  bury the one you were just working in; (3) the whole-host list is bounded by the same cap
  and opens only the newest candidates (mtime sort first, prompt read second), so a home with
  a thousand transcripts costs thirty reads; a transcript whose records never name a
  directory is listed under the escaped project directory name, which is what Claude Code
  itself knows. Wire: `WorkerMsg::AgentSessions.cwd` optional; goldens `host_agent_sessions` /
  `client_hello` re-accepted, PROTOCOL_VERSION 21 → 22. Tests:
  `the_conversations_on_disk_are_listed_newest_first_by_their_first_prompt` (`discover`: a
  second project joins in mtime order, the fallback name, the cap spans directories),
  headless `a_past_conversation_is_resumed_from_the_picker` (the row offered for one
  directory, the re-ask with `None`, none offered for the host), the app self-test (a shell
  that has reported its directory lists it and offers the host, one that has not lists the
  host outright; either way the host's list holds the fake's conversation and offers nothing
  wider).

- ✅ **A conversation can start in a fresh worktree, protocol 32** (2026-09-12). Two agents
  editing one checkout tread on each other; Claude Code's `--worktree` makes a git worktree
  under the repository's `.claude/worktrees/<name>` and runs there. Rulings: (1)
  `OpenAgent::worktree` (a flag: Claude Code names the worktree; a name prompt would cost a
  dialog for little) adds `--worktree` to the launch line; ⌘⌥⇧T (`NewWorktreeAgent`) and
  "New conversation in a fresh worktree" in the palette send it with the active shell's
  directory, like ⌘⌥T; (2) `init` reports the directory the agent really runs in, so
  `AgentInfo::cwd` carries it and the host's `Driven::cwd` (what `@file` completion and the
  resume list use) prefers it over the directory the agent was started in; (3) the card's
  header shows a `⎇ <name>` chip (`conversation-worktree`, a11y "Worktree: <name>") when
  that directory is under `.claude/worktrees`, from `conversation::worktree_name`, and nothing
  otherwise. The fake agent answers `--worktree` by reporting such a directory. Tests:
  `the_launch_line_quotes_only_what_a_shell_would_read`, the ⌘⌥⇧T step of
  `cmd_n_asks_the_host_for_a_shell_and_its_echo_places_and_focuses_it`, the chip in the header test,
  goldens `client_open_agent` and `host_agent_info`.
- ✅ **⌘↑ / ⌘↓ step between a conversation's prompts** (2026-09-13). The grid's ⌘↑ / ⌘↓
  put the previous / next prompt start at the top of the viewport; a conversation card is
  the same chord over its `User` entries (`Conversation::prompt_from_top`, `scroll_to_entry`).
  Rulings: (1) "above" is the prompt before the entry at the viewport's top, or that entry
  itself when the reader is part-way through it, and "below" the first prompt after it; (2)
  ⌘↓ past the last prompt follows the tail again (`pin`), as the grid goes back to following
  output, and ⌘↓ while following goes nowhere; (3) gpui's `ListState` keeps no top while it
  follows the tail, so the first ⌘↑ scrolls by minus the viewport's height from the very end
  — which is exactly where the reader is, the last entry ending at the content's end — and
  reads the logical top from there (`top_entry`); (4) with the caret in the composer the chord
  is gpui-kit's `MoveToStart` / `MoveToEnd` first, captured on the card like ⌘F's `Search`;
  (5) ⌘⇧C, the grid's "copy the newest block's output", copies the newest answer's Markdown
  (`Conversation::last_answer`). Test: `a_driven_view_steps_between_its_prompts`.
- ✅ **⌘F in a conversation card finds entries, no wire change** (2026-09-12). A driven
  card's transcript grows past a screen within minutes and the terminal's ⌘F already existed
  for the grid; a reader expects the same chord to work here. Rulings: (1) the search runs in
  the client over the entries it already holds (`conversation::entry_hits` over
  `entry_text`: the prompt, the answer, the thinking, a call's name and summary, a result, a
  notice — not the card's chrome) with the terminal's smart case, so a needle means the same
  in every card; no `TermRequest::Search` is sent and nothing is asked of the host; (2) the bar is the terminal's own (`Search`, `render_search`) so the ids
  (`terminal-search*`), the a11y labels, the `.*` regex toggle and the `TerminalSearch` Esc
  context are one thing everywhere, but in a conversation it is a row under the header
  (`floating = false`) rather than a corner overlay, so it covers no chip and no line; (3) a
  hit is an entry, washed in the warn tone with the current one stronger (as the grid's), the
  newest hit is the first on show (as the grid reveals its newest), ⌘G / ↩ / ⇧↩ step and wrap,
  and revealing pauses tail-following (`ListState::pause_following_tail`) so the agent's next
  line does not pull the reader off the hit — the list follows again once it is back at the
  bottom; (4) entries arriving under an open bar recount the hits and keep the reader on the
  entry they were on (`search_conversation(keep)`); (5) ⌘F with the caret in the composer is
  gpui-kit's own `input::Search` action first — captured on the card (`capture_action`) so
  the card's bar opens; Esc closes it and the caret goes back to the composer. Test:
  `a_driven_view_finds_in_its_conversation`.

- ✅ **Parity tracks loss asymmetrically, with a deadband** (2026-09-05). The ratio is
  `2 × smoothed loss + 5 %`, clamped to 5…50 %, and a report with `frames_lost > 0` raises it to
  1.5× the current ratio at once (parity was demonstrably not enough for a frame that then cost
  a refresh). Twice the loss is the bursty-channel rule of thumb: a frame survives only if
  *every* missing fragment is covered, so the ratio has to beat the mean by enough to absorb its
  variance. The smoothing is asymmetric — half weight on a rising sample, an eighth on a falling
  one (~0.4 s to decay at the 50 ms report cadence) — because the mesh traces show loss arriving
  in clumps between clean seconds, and a symmetric filter spends every clump under-protected and
  every gap over-protected. A change under 2 % does not move the ratio, since every change
  re-cuts the frame layout at the packetizer and a ratio that jitters by a fragment per frame
  buys nothing. Windows the receiver spent stalled are excluded from the estimate, the same rule
  the bitrate controller's `Stall` verdict follows: a link holding packets is not a link dropping
  them. Ceiling 50 % because past a half, a smaller picture beats a better-protected one. Six
  unit tests with a deterministic clumped-loss channel pin all of it; one of them found a real
  overflow (`lost × 1000` saturating `u32` made a worse window read as *less* loss).

- ✅ **An edit's diff is coloured by its file's grammar** (2026-09-12). The `+`/`−` rows of
  an Edit call read as plain mono while the file card beside them was coloured. Ruled: the
  changed rows take the same tokens as a file card (`conversation::diff_spans` over
  `highlight::spans`); context rows stay muted so the change is what stands out. Each side is
  parsed as its own text — the old side is the context and removed lines, the new side the
  context and added lines — so a removed line that opens a block comment does not bleed into
  the line that replaced it; context rows take the new side's spans. A diff is clipped to 40
  lines (≈ 6 ms to parse), which is too slow for every frame, so the spans are parsed on the
  first draw and kept by entry index (`Conversation::diffs`, entries only append; a reset
  clears it). No wire change. Test:
  `conversation::tests::a_diff_is_coloured_side_by_side_and_cached_by_entry`.

- ✅ **A block is a ledger of calls, and a session owns its terminal** (2026-09-15). Two
  slop-desk lessons (`docs/knowledge-from-slop-desk.md` §5) the tracker had not taken. (1)
  Claude Code fires tool calls in batches and runs the read-only ones concurrently, so a
  `PermissionRequest` for a Bash call could be followed by the `PostToolUse` of a Read beside
  it, which read as "Working" and took the badge down while the permission still waited.
  Ruled: `Tracker::blocks` is a set of `tool_use_id`s waiting on the human — a permission
  request or an `AskUserQuestion` opens one, and only that call's own `PreToolUse` (it was
  permitted and starts), `PostToolUse`, `PostToolUseFailure` or `PermissionDenied` closes it;
  while the set is non-empty a transition to `Working` or `Tool` is not applied. A turn
  boundary (`UserPromptSubmit`, `Stop`, `SessionStart`, `SessionEnd`) clears the set, since an
  Esc-interrupted prompt fires no hook at all. Events without an id (the `Notification`
  family) never touch the ledger. (2) A `claude -p` the agent runs from its own Bash call
  inherits `SLOPTY_SESSION` and posts its full hook set to the parent's session: its
  `SessionStart` cleared the parent's tool, its `Stop` minted a false "finished" with a sound.
  Ruled: a hook naming a different `session_id` is dropped while the tracker is busy
  (`Working`, `Tool`, `Blocked`); it takes over when the tracker is at rest (`None`, `Idle`,
  `Done` — a restart after a crash, since a nested run can only be spawned while the parent is
  busy) or when it is a `SessionStart` whose `source` is not `startup` (`/clear`, `/resume`,
  `/compact`, a fork: the human's own doing in that terminal, whatever the parent was up to).
  `Hook` gained `tool_use_id`. Not taken: the dissent watchdog (screen rules overriding
  stale hooks) — Slopty's transcript follower and the foreground-process probe already end a
  session whose hooks stopped. Tests: `a_block_stands_while_a_call_beside_it_finishes`,
  `a_nested_run_does_not_take_over_a_busy_agent`.

- ✅ **An Esc interrupt is read from the transcript** (2026-09-15). Esc ends a turn with no
  hook at all — no `Stop`, no `PostToolUseFailure` — so a hooked agent stayed "working" (or
  blocked on the permission the human had just dismissed) until its next prompt, the pinned
  spinner slop-desk recorded (`docs/knowledge-from-slop-desk.md` §5). Claude Code does write
  a user record `[Request interrupted by user]` (or `… for tool use`) to the transcript,
  and the daemon already tails that file. Ruled: `transcript::progress` reads that record as
  `Idle` with the detail "interrupted", and `Tracker::observe_progress` lets exactly that
  through the hook gate — a hooked agent that is working, in a tool or blocked goes idle,
  quietly (no attention: the human did it), and its block ledger is cleared; every other
  transcript verdict stays behind the hooks as before. Not done: a wall-clock watchdog; the
  transcript is a witness, a timer would be a guess. Tests: transcript `progress` (both
  markers), tracker `an_interrupted_turn_goes_idle_from_the_transcript`.

- ✅ **Any program can report its own status** (2026-09-15). slop-desk's `ctl report` verb
  (`docs/knowledge-from-slop-desk.md` §5) is the name-agnostic half of the design: a codex,
  gemini or opencode wrapper that says what it is doing gets first-class treatment with no
  per-agent code. `slopty hook report <working|blocked|done|idle|gone> [message…]`, run
  inside a session (`SLOPTY_SESSION` names it; outside one it fails loudly instead of being
  silently ignored as the Claude relay is), relays a `Report` hook payload over the same
  control socket; the tracker reads it as an ordinary hook — the pill moves, a block raises
  attention once and shows as a question, `done` lights the finished badge, `gone` ends the
  agent — and it clears the block ledger, since a wrapper has no `tool_use_id`s to settle. No
  session id on the payload, so a Claude session owning the terminal is not displaced. Tests:
  tracker `a_report_from_any_program_drives_the_pill`, CLI
  `a_report_is_a_hook_payload_the_tracker_reads`.

- ✅ **The agent is used through its TUI; Slopty only reads its status** (2026-09-15, protocol
  48). Claude Code has no public API and its stream-json shape, slash commands, transcript
  records and prompt menus move between releases; a GUI of Slopty's own over it was a second
  client to keep in step with every one of them, and the agents Slopty will meet next (codex,
  gemini, opencode) are TUIs too. Ruled: an agent is a program in a terminal, nothing more.
  Slopty keeps what reads its state — the hook relay, the process, title and transcript
  signals, the tracker, the pill, the attention outline, ⌘⇧A / "N need you", the Dock badge
  and the banner — and drops everything that spoke for the human or drew for the agent: the
  driven card and its stream-json pump (`hostd::driven`, `slopty_agent::stream`,
  `ClientMsg::OpenAgent/AgentSay/AgentAnswer/AgentInterrupt/AgentSet`, `WorkerMsg::AgentPartial/
  AgentPermission/AgentInfo/AgentTask/AgentSessions/Transcript/Files`, `SessionKind::Agent`,
  `CloseReason::Failed`), the conversation view (⌘⇧L, `terminal::conversation`), the composer
  with its attachments, window snapshots, `@` file and slash completions, the resume picker
  (⌘⌥R), "Ask the agent" on the block menu and the "ask" pills on window, file and note cards,
  and the badge's and the banner's allow / deny / answer buttons (a blocked badge now carries
  one "go", which reveals the terminal: the human answers Claude Code's own prompt there).
  The "+ agent" pill is ⌘⇧T again, no menu. Superseded by this: "Answering a permission
  prompt from the badge", "Conversation view from the transcript, not from hooks" and
  "Structured driving is Claude Code's own stream-json protocol" (video.md), the "+ agent" menu,
  the ask pills, the block menu's "Ask the agent", the palette's "New conversation in …"
  (canvas.md, now "New agent in …"). What stays sound in those entries is the evidence about
  Claude Code's prompts and files; the code they describe is gone (no compatibility layer,
  pre-release). Tests removed with it: the driven and conversation headless tests, the app
  self-test's `the_conversation_view_reads_and_answers_the_agent` and
  `a_driven_agent_talks_over_stream_json` with the `slopty-fake-claude` binary and the
  conversation goldens, the simulator's conversation and driven tests. Kept and retuned: the
  status pipeline's unit tests, `an_agent_seen_without_hooks_gets_the_pill_and_offers_the_hooks`,
  `an_agent_started_without_hooks_is_attributed_from_what_the_worker_can_see` (shell-script fake
  `claude`), the title-bar a11y order (`Heading < Status < Button "go" < Terminal`) and the
  find-everywhere and palette-directory tests without their agent lines.

- ✅ **A listed agent says which signal it was read from** (2026-09-25, protocol 57).
  `SessionSummary.agent` and `Outcome::Agent` carry a `SessionAgent { kind, status, source }`
  in place of the `(AgentKind, AgentStatus)` pair. `AgentEvent` already had the source, but a
  client that joins late seeds its badges from the summaries, and the seed claimed
  `AgentSource::Process`: every seeded agent offered "Install hooks" until its first event,
  including agents the hooks already report. Now the worker's agent table, the hub (which keeps
  each listed terminal's agent current from the events) and the listing agree on the source.
  The UI seeds it, so the offer shows only where no hook speaks. `slopty terminals` has an
  AGENT column ("working (hooks)", "idle (title)"), and `list_terminals` / `agent_status` give
  `source` as `hook | transcript | title | process`. Hub events keep no source: they record
  what changed, not how it was read. Tests: `the_summaries_seed_the_agents_before_any_event`
  (UI), `listed_terminals_carry_the_agent_as_last_reported` (hub),
  `a_summary_carries_the_agent_a_hook_reported` (worker e2e),
  `an_agent_reads_the_same_in_json_and_text` and the `terminals_as_*` snapshots (tools), the
  CLI's `terminals` and MCP `list_terminals` checks, the `worker_session_opened` golden.

- ✅ **The relay knows its own entry by the whole program path, and forwards only what the
  tracker reads** (2026-09-26). `is_relay` took the program as the text before the command's
  first space, so the standard install under `~/Library/Application Support/Slopty/bin/slopty`
  never matched: each `hook install` appended another group to all 12 events, `uninstall`
  removed nothing and `status` said not installed. Now the program is the whole `command` when
  `args` is `["hook"]`, and the command line before ` hook` (quotes allowed) in the shell form.
  `install` keeps the first relay entry of an event and drops the rest, with any group that
  leaves empty, so settings the old install filled collapse to one entry (both old-form paths
  removed 2026-09-28, below). The relay also cut
  stdin at 1 MiB, which turned a large `PostToolUse` (the tool's output rides along) into
  invalid JSON. It now reads the payload whole and forwards `slopty_agent::Hook` serialized:
  the fields the tracker reads, a few hundred bytes. The relay and `slopty worker` reach the
  daemon under the global `--data-dir`. Tests:
  `a_relay_under_a_path_with_spaces_is_recognised_installed_once_and_removed`,
  `install_collapses_duplicate_relays`,
  `a_large_tool_payload_is_forwarded_as_the_fields_the_daemon_reads`,
  `the_socket_is_the_data_dirs_when_its_daemon_is_installed`, `the_data_dir_is_global`.

- ✅ **An agent's status carries when its phase began** (2026-09-27, so a client can show how
  long a turn has run, as monocode's live list does). `AgentEvent.since_ms` and
  `SessionAgent.since_ms` are the worker's clock in ms since the epoch, 0 with no agent. The
  stamp moves only when the status enters a new phase (none, idle, busy, blocked, done): a tool
  call inside a turn keeps it. `slopty_agent::Tracker::enter` is the one place it is set, and the
  worker's replayed table and the server's resync both carry it, so a client that joins or
  reconnects reads the same start. The wire goldens moved (`worker_agent_hook`,
  `worker_agent_process`, `worker_session_opened`). Test:
  `the_status_is_stamped_when_its_phase_changes`.

- ✅ **An agent the server starts brings its hooks along** (2026-09-27, verified against Claude
  Code 2.1.283). `SpawnAgent` started `claude` with no way to report unless someone had run
  `slopty hook install` on that worker, and it then typed the first prompt after the fallback
  wait instead of on the ready hook. The worker now hands `claude` the relay beside it on
  `--settings`, for that run only. Two behaviours were checked with a hook that appends to a
  file: hooks from project settings and from `--settings` both run, and the same handler in both
  runs once, so a machine that also has the hooks installed hears each event once. The second
  check: of two `--settings`, only the last is kept, whole. So `slopty_agent::hooks::with_relay`
  reads the caller's last `--settings` (JSON, or a file from the agent's directory), adds the
  relay and passes it as the only one. A value it cannot read is passed on untouched for
  Claude Code to report.
  - No `InstallHooks` verb. `~/.claude/settings.json` belongs to the person; it changes when
    they ask, from the CLI or the app's offer (`ClientMsg::InstallHooks`). Agents that
    orchestration starts no longer need it.
  - Test: `a_spawned_agent_reports_through_the_relay_it_was_handed` (worker; a stand-in `claude`
    records its arguments, and the relay runs from the settings it was handed) and
    `a_run_gets_the_relay_on_the_one_settings_it_keeps`.

- ❌ **Claude Code gets a conversation face; the TUI stays the source of truth** (2026-09-27;
  superseded 2026-10-04 by ui.md, "One face for every agent";
  verified against Claude Code 2.1.283; research and plan in
  `.research/claude-gui-study-2026-09-27.md`). The person wants a view of an agent's work beside
  its TUI, toggled per tile: the edits as diffs, the tool calls with their results, subagents,
  background shells and the task list. This reverses the status-only half of "The agent is used
  through its TUI; Slopty only reads its status" (2026-09-15). That ruling stood on three
  things that have changed.
  - The SDK's stream-json wire is now published (`sdk.d.ts` documents the unions and tells a
    host to ignore unknown types).
  - Hooks grew to 33 events. `PostToolUse` carries the tool's response,
    `SubagentStop` the subagent's own transcript, and a `PermissionRequest` hook can answer the
    prompt.
  - A face that projects the live TUI keeps the TUI. The deleted driven card had none, so it
    could not toggle back.
  - What stays: nothing replaces the TUI, Slopty never types menu digits, and a driven
    (stream-json) session waits until the observed face's lag is measured and found wanting.
    *Superseded 2026-10-01* by "GUI-first: the agent's own session is the source of truth"
    (`agents.md`): the agent's own session is the record, and driven Claude is an opt-in.
- **Architecture: observe the TUI.** `claude` keeps running in its PTY with the relay on
  `--settings`. A tile toggles between the grid and the face with no restart. For a session a
  client follows, the worker tails the transcript, each subagent's `agent_transcript_path` and
  each background command's output file, and sends typed entries on a low-priority uni stream,
  never the control stream. Hooks give the live moments; the transcript fills in text and
  thinking as blocks complete. The composer types into the same PTY (bracketed paste, then
  Enter), and approvals go through the blocking `PermissionRequest` hook.
- **The drift risk has one owner.** `slopty_agent::conversation` is the only code that reads
  the JSONL. Clients get typed entries. It skips unknown record types, fields and lines that
  do not parse, and keeps an unknown tool as its name and clipped input.
  - Golden fixtures pin it: `crates/slopty-agent/tests/fixtures/conversation/<scenario>/`
    holds `transcript.jsonl`, `subagents/agent-<id>.jsonl` and `hooks.jsonl`, and insta
    snapshots hold the decoded threads. `cargo xtask fixtures claude [--only <name>]`
    recaptures them. It runs the installed `claude` on haiku in a scratch directory with no
    user or project settings, takes the records from the `transcript_mirror` frames of
    `--session-mirror` (never from files under `~/.claude`), registers itself as the hook to
    record payloads and answer permission requests, and scrubs paths, the user name, e-mail
    addresses and every id to numbered placeholders. Attachments keep only their type. Five
    scenarios: `edit` (read, edit, overwrite, ranged read), `tools` (tasks, glob, grep, bash,
    a failing bash, a background bash, a create, a subagent), `interrupt`, `compact`,
    `permission`. A snapshot that moves after a recapture is the format moving.
  - Entries: prompt (a slash command or `!` keeps its name), answer text, thinking, a tool call
    paired with its result, compaction with its summary, Esc, and Claude Code's notes (API
    errors, command output, informational lines). Tool details are typed for Edit/MultiEdit
    (hunks from `structuredPatch`, +/− totals), Write (create or overwrite), Read, Grep, Glob,
    Bash (exit code, stdout and stderr tails, background task and output file), WebFetch,
    WebSearch, Agent, TaskCreate/TaskUpdate/TodoWrite (which also keep the thread's task list),
    AskUserQuestion, ExitPlanMode and MCP tools.
  - Ids stay put across reads: a tool call is its `tool_use_id`, anything else its record's
    `uuid`, with `:<block>` for answer and thinking blocks. The decoder builds on `Tail`, and a
    record appended later (a result, a background task's `task-notification`) comes back as an
    upsert of the entry it belongs to. A record whose parent is not the newest one starts a
    branch (a rewind), and the entries past the fork are removed.
  - Subagents are threads keyed by agent id, from their own files or from `isSidechain`
    records in the main one. The `agent_metadata` line at the top of a subagent's file names
    the call that started it.
  - Clipping happens on the worker. Prose stops at 400 lines or 32 000 characters, tool output
    and inputs at 40 lines or 4 000 characters (Bash keeps the tail), a diff at 400 lines. A
    clipped text says how long the whole was and carries a `TextRef` (record uuid and part)
    that `full_text` resolves against the transcript later.
- **The relay forwards what the face needs.** `HOOK_EVENTS` grows from 12 to 19: `StopFailure`,
  `SubagentStart`, `SubagentStop`, `TaskCreated`, `TaskCompleted`, `PreCompact`,
  `PostCompact`. This reverses "forwards only what the tracker reads" (2026-09-26) for these
  fields: `tool_response` and `duration_ms`, the subagent's id, type, transcript and last
  message, the task, the compaction trigger and summary, the error, and the permission
  request's input and `permission_suggestions`. `Hook::trimmed` keeps it small. A tool's input
  and response keep 16 KiB of JSON each, an edit's `originalFile` goes, and free texts stop at
  32 000 characters. The tracker reads `StopFailure` as `Done` with the error as detail; before,
  a turn that died on an API error left the pill on "working".
- **`MessageDisplay` is not registered.** Claude Code holds each batch of the TUI's paint until
  that hook returns (10 s default timeout), so it goes on the input path. It is measured before
  anyone registers it.
- **Approvals: a blocking `PermissionRequest`.** Its entry is the one without `async`, with
  `timeout: 600` (Claude Code's own default). The relay posts the hook as before, then sends
  `{"cmd":"permission","session","payload","wait_ms"}` on the control socket
  (`slopty_agent::permission::RelayRequest`) and waits up to 595 s for
  `{"reply":"permission","decision":{"kind":"pass"|"allow"|"allow_always"|"deny",…}}`. It
  prints the output the hooks reference defines (`hookSpecificOutput.decision.behavior`, with
  `updatedPermissions` for always and `message`/`interrupt` for deny) and nothing for `pass`.
  Today's worker cannot read the request and closes the connection, so the relay reads no
  decision at once and the TUI shows its dialog as before. Phase 1b adds
  `CtlRequest::Permission(PermissionAsk)` and `CtlReply::Permission(PermissionAnswer)`, which
  tag the same way. It answers `pass` at once unless a client shows the face, and must not
  apply the payload to the tracker a second time.
  - Checked in the `permission` capture: 2.1.283 obeyed a deny (the file was not made), an
    allow, and an allow-always that handed back the suggestions. For `touch` those were
    `addDirectories` and `setMode acceptEdits`, so after it the next command needed no
    permission. The payload has no `tool_use_id`.
- **Meters: a status-line wrapper.** `with_relay` also sets `statusLine` to
  `'<slopty>' hook statusline`. It forwards the model, context used, cost and rate limits to
  the worker as a `Statusline` hook (the tracker ignores it), then runs the person's own
  status-line command through `sh -c` with the same input and prints its output byte for byte.
  The forward gives up after 500 ms so the line is never late.
  - Precedence: Claude Code takes `statusLine` from managed settings, then `--settings`, then
    the project's `settings.local.json`, its `settings.json`, then the user's (under
    `CLAUDE_CONFIG_DIR` or `~/.claude`). The wrapper on `--settings` beats every file. The
    person's line from the caller's `--settings`, which `with_relay` replaces, travels as
    `--command`; one from the files is looked up each time the wrapper runs. Their other fields
    (`padding`, `refreshInterval`) are kept. A managed `statusLine` beats the wrapper, and the
    face then has no meters.
- **Found in the captures.** Thinking reaches the transcript only as summaries, and only with
  `showThinkingSummaries`; without it the block is empty and the decoder skips it. Records in a
  subagent's file carry no `toolUseResult`, so its results show their text. Esc during a
  running command writes a rejected result ("User rejected tool use") and then
  "[Request interrupted by user for tool use]". In one run the command was moved to the
  background and a `killed` task notification followed as a user record. A background task
  that finishes writes a `queued_command` attachment whose `commandMode` is
  `task-notification`.
- **Phases.** 1a is this change: the decoder, the forwarding, the permission relay and the
  status-line wrapper, with no wire change. 1b, after UI wave 2: entry wire types with goldens
  in `slopty-proto`, follow and unfollow, the per-connection follow task and its uni stream, the
  worker's side of the permission request, and the meters. 2: the face itself in `slopty-ui`
  and `slopty-app`.
- Tests: the `conversation` snapshots (`edit`, `tools`, `interrupt`, `compact`, `permission`),
  `a_line_at_a_time_ends_where_a_whole_read_does` (half lines, files interleaved, a client
  applying only the changes), the decoder's unit tests
  (`a_result_pairs_with_its_call_whenever_it_arrives`,
  `a_background_command_finishes_on_its_notice`, `a_subagent_talks_in_its_own_thread`,
  `a_long_text_is_clipped_and_can_be_had_whole`, `what_it_does_not_know_is_passed_over`,
  `a_branch_abandons_what_followed_its_fork`, …), the hook fixtures (`tests/hooks.rs`,
  including `a_permission_decision_is_the_output_claude_code_acted_on`), the relay against a
  stand-in socket (`a_permission_request_prints_the_workers_decision`,
  `a_worker_that_does_not_decide_leaves_the_dialog_to_claude_code`,
  `the_wait_for_a_decision_is_bounded`), and the wrapper
  (`the_persons_line_passes_through_unchanged`, `the_meters_are_forwarded_as_a_hook`,
  `a_run_gets_the_status_line_wrapper_in_front_of_the_persons_own`).

- ❌ **A followed conversation streams from the worker; a permission prompt waits for its
  followers** (2026-09-27, phase 1b of "Claude Code gets a conversation face"; superseded
  2026-10-04 by ui.md, "One face for every agent").
  - **One definition of an entry.** The entry types (`Entry`, `Body`, `ToolDetail`, `Change`,
    `Clipped`, `TextRef`, …) and the meters moved from `slopty_agent` to
    `slopty_proto::conversation`, and the decoder builds them directly (`slopty_agent`
    re-exports them). A mirror in proto with a conversion at the worker would have been a second
    copy of forty types to keep in step, and `slopty-agent` already depends on `slopty-proto`.
    Every entry type has a golden (`golden__conversation__*`), so a changed decoder output that
    changes the wire shows there too.
  - **Follow.** `ClientMsg::Conversation(ConversationRequest::Follow { session })` starts it.
    The worker opens a conversation stream to that client (`UniHead::Conversation`, at
    `slopty_net::streams::CONVERSATION_PRIORITY`, level with tunnels: below the control stream,
    the terminals and video, above files) and runs one task per follow on the connection
    (`apps/slopty-worker/src/follow.rs`). It reads the transcript the agent table names and
    every subagent file, the ones in `<session>/subagents/` and the ones `SubagentStop` names
    (`slopty_agent::conversation::Transcripts`), on the blocking pool, every 250 ms and at once
    when a hook fires in the session (`slopty_worker::conversation::Board`). No lock is held
    across a read or a send. The stream opens with `Change::Reset` of every thread, then the
    conversation as it stands in frames of 64 changes, then `ConversationEvent::Current`; after
    that each change as it is read, and `Meters` when the status line moves them. A new
    transcript (`/clear`, `/resume`) or a rewritten one starts over the same way. Unfollow,
    the session closing or the connection ending finishes the stream; only followed sessions
    are read. `Expand` answers a clipped text whole (up to `EXPAND_CHARS`) on the same stream.
    Every follower of a session shares one read of it (amended 2026-09-28, workers.md "A
    followed session's transcripts are read once, whoever follows it"): the changes go out
    over a broadcast, and a follower that joins late is handed the conversation as it stands.
  - **Cost.** A session growing at twenty times a real turn's rate, followed on the typing
    connection, leaves the echo's median where it was and adds 1.2–1.6 ms at p90; an appended
    answer reaches the follower in p50 26 ms (MEASUREMENTS.md, "an echo beside a followed
    conversation").
  - **Held prompts.** The relay's `CtlRequest::Permission(PermissionAsk)` (which replaced the
    relay's own `RelayRequest`/`RelayReply` copies) goes to `follow::ask`. The state machine is
    `slopty_worker::conversation::Holds`: with nobody following the session the answer is
    `Decision::Pass` at once; else the prompt is held under a worker-wide id and sent to the
    followers on the control stream (`WorkerMsg::Permission(PermissionEvent::Asked)`, only to
    connections that follow the session, and again to one that follows later or missed the
    broadcast). The prompt carries the call as the conversation will show it
    (`slopty_agent::conversation::proposed`, an edit's change as a patch) and the suggested
    permission updates typed (`Grant::{Rules, Mode, Directories, Other}`). The first
    `ConversationRequest::Answer` from a follower takes it: allow, allow always (every
    suggestion handed back as `updatedPermissions`, as Claude Code's own "Yes, and always …"),
    or deny with a message. The last follower leaving, the wait running out a second before
    the relay's, or the relay's end closing (it now keeps the socket open while it waits, so its
    end closes when the relay is gone) hands it back undecided and the TUI's own dialog shows. The followers hear `Settled { Answered | Released | Withdrawn }`. An answer
    after that, a second answer, one for another session and one from a client that does not
    follow find nothing to take. Menu digits are never typed.
  - Tests: `Holds` (`with_no_follower_a_prompt_passes_at_once`, `the_first_answer_wins`,
    `a_late_or_stray_answer_is_dropped`, `the_last_follower_leaving_mid_hold_releases_it`,
    `a_disconnect_releases_where_it_was_the_last_follower`,
    `a_new_follower_is_shown_what_is_waiting`, `the_board_wakes_followers_on_each_hook`),
    `Transcripts` (`the_first_read_is_the_whole_session_then_only_what_grows`,
    `another_transcript_starts_over`, `a_rewritten_main_file_reads_the_subagents_again`,
    `a_clipped_text_is_found_in_its_threads_file`), the prompt
    (`a_prompt_words_the_call_and_what_always_would_grant`), the socket lines
    (`a_permission_request_and_its_decision_are_single_json_lines`), the relay
    (`a_permission_request_prints_the_workers_decision`, which checks the relay's end stays
    open while it waits), the goldens, and end to end through the real relay run as the test's
    own child (`a_prompt_is_held_while_its_thread_is_followed`: the held prompt shows on the
    followed thread as a request, an "always" answer is what the relay prints, a second answer
    finds nothing, a killed relay withdraws, the last follower leaving hands it back to the
    TUI, and with nobody following the relay is let go at once).

- ✅ **Slopty's Claude Code mod is the live channel, behind a strict version gate; the mod is
  TypeScript** (2026-09-27, phase 1c of "Claude Code gets a conversation face", verified against
  the official Claude Code 2.1.283; measured in `.research/claude-gui-study-2026-09-27.md`,
  "Mods and Channels, measured").
  - **What it adds.** The transcript has a block only once the model finished it. A Claude Code
    mod (a plugin's function hooks) sees the answer, thinking and tool input as they stream,
    with each subagent's `agentId`, plus the session's context and cost. The face shows those
    as live blocks: `ConversationEvent::Live(Vec<Live>)`, with `Live::{Start, Append, Clear}`
    keyed by `LiveId { turn, step, block }`. They are uncommitted text at the end of their
    thread, never entries.
  - **TypeScript, against "pure Rust everywhere".** Claude Code runs a mod in its own runtime
    and loads only TypeScript or JavaScript modules, so the mod cannot be Rust. It is kept to
    what that forces:
    - Three files embedded in `slopty-agent` (`assets/claude-mod`: `plugin.json`,
      `hooks/hooks.json`, `hooks/register.ts`).
    - A pipe with no logic: it forwards a whitelist of events, changes nothing it passes on,
      and swallows its own failures.
    - Written by the worker under its data dir in a directory named by the files' BLAKE3
      (`claude_mod::install`), so a running agent keeps the files it loaded.
    - No script of Slopty's is TypeScript. The recorder and the checks are Rust
      (`cargo xtask fixtures claude-mod`).
  - **Pinned, strictly.** The plugin API is early access; its typings grew from 13k to 15k lines
    between 2.1.277 and 2.1.283.
    - The mod's first event is a `hello` naming its protocol (`MOD_PROTOCOL`) and Claude Code's
      version. Nothing else is heard until a hello passes `slopty_agent::live::gate`: this
      protocol, and a version in `MOD_CLAUDE_VERSIONS`, which holds exactly the versions
      recorded (today `["2.1.290"]`, recorded 2026-10-06 with the mod unchanged; 2.1.289 and
      2.1.286 before it).
    - A newer release is heard provisionally (2026-10-05, ruled; see "A managed `claude`"
      below). A fleet moves to a new release before anyone records it, and refusing it left the
      face without its live stream on the very machines that most need it. So the gate has
      three answers (`live::Trust`):
      - **Verified**: a recorded version.
      - **Provisional**: an unrecorded release of a recorded `major.minor` line (2.1.291 while
        2.1.290 is recorded), in strict `x.y.z` digits. Its events are decoded strictly: an
        event of a known kind that does not decode (`ModEvent::Malformed`) drops the mod for
        the session (`Trust::Dropped`, logged once), its blocks in flight go, and no later
        `hello` revives it. The transcript, the hooks and the status line carry on alone. An
        unknown kind is skipped as before, which a newer mod's additions need.
      - **Refused**: another line (2.2, 3.0) or another protocol, logged once (warn).
      A session whose mod never says hello is followed from the fallback as before; nothing
      is surfaced as an error. `cargo xtask upstream check` names the newest Claude Code on
      npm beside the recorded one, and `cargo xtask fixtures claude-mod --version <it>`
      records it.
  - **What catches a break on update.** `cargo xtask fixtures claude-mod` downloads the official
    build (`@anthropic-ai/claude-code-darwin-arm64@<version>` from the npm registry, its tarball
    checked against the registry's SHA-512, into `target/claude/<version>`; `SLOPTY_CLAUDE`
    names another binary, which must still report the version). `fixtures claude` records with
    the same build, never with whatever `claude` is on `PATH` (here a patched managed launcher).
    The recorder:
    - runs `claude plugin validate --strict` on the mod and fails on any finding;
    - runs three scenarios headless in a scratch home against a canned Messages API on loopback
      (text and a Bash call, thinking, a subagent), so no account and no model are involved,
      with the mod posting to a socket of the recorder's own;
    - writes the scrubbed events, the transcripts of the same runs, the version recorded and a
      copy of the mod as recorded (`crates/slopty-agent/tests/fixtures/mod`).
    `slopty-agent`'s tests then hold the copy to the embedded files and the version to
    `MOD_CLAUDE_VERSIONS`. An edited mod or a new version fails the build until it is recorded,
    and a recording that no longer decodes, or no longer settles, fails too.
  - **The fallback stays first-class.** Hooks, the transcript and the status line carry the face
    everywhere the mod is not heard:
    - on a Claude Code not recorded;
    - in the person's daily `claude`, a managed launcher, until its egress lets the mod's
      requests reach the worker's socket ("A managed `claude`", below). The relay works there;
    - with `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC` set, which loads the mod but blocks its
      `fetch` (Unix socket included), so no hello ever comes.
    The mod only adds liveness. Every entry, prompt and meter still comes from the fallback
    path.
  - **Launch.** An agent the worker starts (`orchestrate::Launch`) gets:
    - `--plugin-dir=<dir>`, always in the `=` form: the spaced form is variadic in 2.1.283 and
      swallows the words after it, a prompt or a subcommand;
    - `CLAUDE_CODE_ENABLE_FUNCTION_HOOKS=1`, without which Claude Code 2.1.286 does not load
      the module. 2.1.289 no longer reads it: a rollout flag that defaults on decides, under the
      managed hooks switches. It is set all the same, for the builds that read it;
    - `SLOPTY_MOD_SOCKET`;
    - `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC` set empty. The worker cannot unset a variable
      in a session it spawns, and Claude Code reads empty as unset: the recorder runs with it
      so, which pins that.
    The caller's own variables go last and win. Every session has `SLOPTY_CLAUDE_MOD` and
    `SLOPTY_MOD_SOCKET`, and the shell integration's `claude` function gives a `claude` typed
    by hand the flag and the hooks switch, through `slopty hook wire` ("A `claude` typed in a
    Slopty shell is wired as one Slopty starts", below). It adds them once (not when the flag
    is already there, as in an agent the worker started through a login shell), never over a
    `claude` alias or function of the person's own, and not at all with
    `SLOPTY_NO_CLAUDE_MOD=1`. An inherited nonessential-traffic switch is left alone there: the
    person set it. The mod is then left out too (2026-10-05): Claude Code's plugin network gate
    refuses a sideloaded plugin's every request while the switch is set to anything but empty,
    so it could never say hello (`managed::ModOff`).
  - **The mod socket: hyper, not a parser of our own.** The mod can only `fetch`, so it posts
    `{session, events}` as HTTP/1.1 to `POST /v1/events` on a Unix socket beside the control
    socket (`worker.sock` → `worker.mod.sock`, in the same user-only directory). Each post is
    answered `204` once its events are on the board. The daemon serves it with hyper's http1
    server (`apps/slopty-worker/src/modsock.rs`): hyper and its utilities were already in the
    daemon through `slopty-tailnet`'s LocalAPI client, so it adds no crate. A hand-rolled
    reader would have been a second HTTP implementation to keep correct under keep-alive and
    partial reads. A batch names its terminal session (`SLOPTY_SESSION`); one for no live
    session here is refused `404`, and one over 8 MiB `413`.
  - **Board and overlay.** `slopty_worker::conversation::Board::reported` gates each session's
    events and keeps its blocks in flight (`slopty_agent::live::Board`) in the `Seen` its
    followers watch. A block is kept 30 s after its step stopped, or 120 s if it never does.
    A `measure` puts its context and cost on the meters, sooner than the status line. Each
    follower keeps an `Overlay` of what it was shown, sends only a block's start and what it
    grew by, and never shows a block that stopped before it first saw it (a late follower
    leaves those to the transcript). The transcript is still read on the tick and on hooks,
    not for every piece the mod reports.
  - **Settling.** A live block is cleared in the same pass that sent the transcript change
    settling it, after that change, so a client never shows a gap:
    - a text block by an answer upserted in its thread with the same text (a clipped answer by
      its head);
    - a thinking block by thinking upserted in its thread (the transcript may hold a summary);
    - a tool block by the call with its `tool_use_id`.
    A block nothing settles goes 5 s after its step stopped, or when the board lets it go (the
    session's `bye`). A new transcript (`/clear`) clears every block.
  - Tests:
    - the recording: `the_recorded_events_decode`,
      `the_recording_is_of_this_mod_on_a_trusted_version`, and
      `the_transcript_settles_every_recorded_block` (every block of the three runs is settled
      by the transcript the same run wrote);
    - `blocks_carry_their_thread_kind_and_text`, `a_follower_gets_only_what_is_new`,
      `blocks_settle_on_their_entry_or_after_the_grace`,
      `a_call_settles_by_its_id_and_a_long_answer_by_its_head`,
      `a_measure_updates_the_meters`, `the_gate_names_what_it_refuses`;
    - the mod on disk and on the command line: `the_mod_is_written_under_its_digest`,
      `an_agent_loads_the_mod_once`, `an_agents_environment_enables_the_mod`;
    - the board's gate: `the_mod_is_heard_after_its_hello_passes`;
    - the shells: `a_typed_claude_runs_as_the_cli_wires_it` and
      `a_typed_claude_is_wired_as_one_slopty_starts` (zsh, both bashes and fish);
    - the spawn: `a_spawned_agent_reports_through_the_relay_it_was_handed`, whose daemons
      inherit `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1` and whose stand-in `claude` finds
      it empty, the mod's flag and files, the hooks switch and the socket;
    - the goldens: `golden__conversation__conversation_live`;
    - end to end: `a_trusted_mod_streams_live_blocks_that_the_transcript_settles`. The recorded
      events are posted to the real daemon's mod socket. A piece before the hello, one after an
      unverified hello and one for another session show nothing. Then two steps stream, and
      each block is cleared only after its entry arrived. The measure reaches the meters.

- ✅ **The face reads as a finished product: turn figures, changed files, copy, find, and a
  calmer page** (2026-09-27; gap analysis against T3 Code and Amp in
  `.research/conversation-face-gaps-2026-09-27.md`).
  - **Turn figures come from the transcript, per turn.** `Change::Turn { thread, turn }` carries
    a `Turn` keyed by its prompt's entry id: when it started and ended, the models that
    answered, how many requests, their summed `Usage` (input, cache read and write, output,
    thinking), the context at the last request, the permission mode and the last stop reason.
    - The end is Claude Code's own: `system/stop_hook_summary` or `turn_duration`. Timing the
      fold from the entries' stamps counted a `/compact` sixteen seconds later as the turn's
      work (`a_compaction_stays_outside_the_fold` now reads 1 s, not 17 s).
    - Claude Code writes one assistant record per content block, each with the request's usage
      so far. A record with the same `message.id` and no user record since is the same request,
      so its usage replaces the earlier one rather than adding to it.
    - Checked on the `edit` recording: the turn's usage (input 50, cache read 125 375, cache
      write 2 344, output 1 942, thinking 1 359) equals the `cost-state` record's `modelUsage`
      for the run exactly.
    - No per-turn cost. `cost-state` is written only when the process exits, as session totals,
      so a turn's price would be a guess from a price table. The session's cost stays with the
      status line and shows in the context chip's hint.
  - **More the transcript says, now on the wire.** All additive, each with its golden:
    - `Body::Rewound { dropped }` where a branch set entries aside; the fold's turns go with
      them;
    - `Note::retry` (`attempt`, `max`, `in_ms`) on an API error that Claude Code retries;
    - `NoteKind::Hook` for a Stop hook that failed or stopped the turn;
    - `WebSearchDetail::links`, the first ten pages a search found.
  - **The page.** A centred 720-point reading column (between T3 Code's 768 and Amp's 672).
    - Prose: answers are set at the prose size (14) on the Markdown leading. Headings step
      18, 16, 14, paragraphs are 10 apart, and each code block names its language and has a
      copy.
    - Prompts: a raised bubble at the large radius, up to 85 % of the column. Its time and copy
      show on hover, and always under a finger.
    - Calls: a 24-point line. The verb is in the secondary tone and the subject in the text tone
      at the medium weight. A failure adds a small ✕ after the subject; nothing else turns red.
    - Diffs: in a frame at the medium radius headed by the file's directory, its name and its
      size. The call's own line then drops the size.
    - No raw JSON anywhere: an MCP or unknown tool's input reads as its keys and values.
    - Subagents: a subagent is a row with a body, not a bordered card. While it runs, it shows
      the call it is on and for how long.
    - Thinking is upright, not italic.
  - **What a turn changed.** A settled turn ends on its answer, whose hover shows its time and a
    copy, then "Changed N files" with each file's `+a −r`.
    - A file's click, or the header's `+N −M` chip, opens the session's changes in the same
      list: each file over every edit and write that changed it, across threads, diffs whole.
    - A created file shows its line count, because its content is not on the wire.
  - **The composer floats.** It sits on `kit::elevate` at the large radius, with no accent
    border and a 24-point fade where the list slides under it. That fade is the face's only
    gradient.
  - **The approval.** A held permission prompt takes the composer's shell rather than a card
    of its own. It is worded as a statement ("Claude wants to run a command").
    - A warn dot, not an amber border.
    - The command appears on the raised plate, capped at five lines with a "Show all".
    - What "always" grants opens on request, and the answers sit at the right with the primary
      last.
  - **Find.** ⌘F opens a floating bar over the list. It searches prompts, answers, call titles,
    notes, and thinking where the density shows it. Enter and Shift-Enter step through the
    matches, opening the fold that hides one, and Esc gives the composer the keyboard back.
  - Images and a background task's live output came next (the entry below). Model and mode
    pickers would have to type into the TUI's menus,
    which the face never does, so the mode shows as text in the composer's foot instead.
    (Amended for the model on 2026-09-28: `/model <alias>` takes its argument without a menu,
    so the model became a picker that types it; see "The composer answers questions, lists
    commands and mentions files". The mode stays text.)
  - Tests:
    - the decoder: `a_turn_adds_up_its_requests`, `an_api_error_says_when_it_retries`,
      `a_branch_abandons_what_followed_its_fork` (the rewind marker) and the five `conversation`
      snapshots;
    - the goldens: `golden__conversation__conversation_changes`, `_tools`;
    - `figures` (model names, turn meta, clock, files);
    - `find` (`a_query_finds_what_the_reader_can_read`,
      `a_folded_match_names_the_fold_that_opens_it`);
    - `a_tools_input_reads_as_keys_and_values`;
    - `a_prompt_is_a_statement`;
    - the face in a window: `a_changed_file_opens_the_sessions_changes`,
      `cmd_f_finds_words_in_a_folded_turn`, `an_answer_copies_its_words`;
    - end to end: the fold's label in `a_step_being_written_shows_live_until_the_transcript_settles_it`
      carries the turn's model and tokens, and the goldens `conversation`, `-dark`, `-phone`,
      `-subagent` and the new `conversation-settled` (a settled turn: its prompt, its fold with
      its figures, its answer and the file it changed);
    - the frame probe: `the_face_draws_a_streaming_answer_within_a_frame` (docs/MEASUREMENTS.md).
- ✅ **The face shows the work beyond words: background output, pictures, thinking, plans and
  tasks** (2026-09-27, wire additive but for `Prompt::images`). The face left out what a turn
  does besides write and call tools. T3 Code and Amp both show it, so the face now does too.
  - **Background output.** A `run_in_background` command writes to
    `<tmp>/claude-<uid>/<project>/<session>/tasks/<task>.output`, named in its result's text.
    The decoder keeps that path (`BashDetail::output_file`).
    - The follow task tails it in the same blocking read as the transcripts
      (`conversation::output::Outputs`), so no terminal path is touched. It reads the newest 32
      background calls, stats each and reads the last 64 KiB only when the length changed.
      After the call ends it reads once more, then stops.
    - A path is read only when it is absolute and names the call's own task file under a
      `tasks` directory (`is_task_output`), so a transcript cannot point the worker elsewhere.
    - The tail is clipped like any text (escapes out, a line's `\r` rewrites kept to the last)
      and goes out as `ConversationEvent::Output { thread, call, tail, bytes }`. The whole file,
      up to 1 MiB, comes through the usual `Expand` with `Part::Output`.
    - The end comes from Claude Code's `task-notification`: the `queue-operation` enqueue that
      is written the moment the work ends, a queued attachment, or user text. It sets the
      status, `BashDetail::finished_ms`, and for a background subagent its tokens, tool uses and
      duration from `<usage>`.
    - **No stop control.** Neither the transcript nor the hooks give a task's pid or a way to
      stop it, and the mod is a read-only pipe. The only way to stop one would be to type into
      the TUI, which the face never does, so the tray shows the state only.
  - **The tray.** Background work sits over the composer while it runs and until the next
    prompt: a row each with its name, state word and elapsed time, and the last line in mono.
    - A click opens the last twelve lines in the code frame; once the work has ended, it can
      expand to the whole file.
    - A subagent's row opens its thread.
    - Three rows show, and the rest sit under "N more in the background".
    - The clock ticks while anything runs.
    - The tray is the session's, so a subagent's thread leaves it out.
  - **Pictures.** A pasted image, a tool's screenshot and a `Read` of an image are base64 in the
    transcript. The wire carries only a description: `Image { digest (BLAKE3), media_type,
    bytes, width, height, at }`, on `Prompt::images` (which replaces the old count) and
    `ToolResult::images`.
    - The decoder reads the size from the header, for PNG, JPEG, GIF and WebP.
    - A row fetches its pictures when the list first lays it out. It sends `Expand` with
      `Part::Image`, and the worker answers `ConversationEvent::Image` with the bytes (capped at
      `IMAGE_BYTES`, 8 MiB) or none.
    - The client keeps the bytes by digest, so a picture shown twice is fetched once, and GPUI
      decodes it off the frame.
    - A thumbnail has its own shape inside a 200 × 120 box, so the row is laid out before the
      bytes come. It sits at the medium radius with a subtle hairline.
    - A click opens the picture fitted over the face on the scrim, with its size and type; a
      click anywhere or Esc closes it.
    - A picture over the cap, or one whose bytes are gone, says so in place.
  - **Thinking** is one line, "Thought for 9 s", timed from what it answered, with its first
    words muted. The chevron takes the icon's slot on hover. A click opens it. It shows from the
    Thinking density and opens by itself in Verbose.
  - **A plan** (`ExitPlanMode`) is a card on the panel, with "Plan", its heading as title,
    whether it was approved, and a copy button. Its Markdown is set at the prose size. Past 16
    lines it shows 12 and "Show the whole plan · N lines". It stays in view when its turn folds.
  - **Tasks** sit over the composer while one is open or the agent works: the task in progress,
    `done/total`, and a segment per task (up to ten). Opened, each task shows how long it took,
    from the calls that moved it.
  - **From the second design critique** (the conversation's part):
    - The approval sets its words at the composer's field inset (`kit::FIELD_INSET`) and uses
      the `warn_fill` dot.
    - Its fallback reads "Falls back to the terminal in 4 min", from five minutes left. It
      counts down on this client's clock from when the prompt came, and sits before the
      answers.
    - The composer's foot shows the permission mode and the model as read-only words. The
      density is in the palette only.
    - The header drops the model chip, and its readouts are 8 pt apart.
    - One `+N −M` everywhere (`kit::changes`, `changes_text`), with a zero side left out.
    - A tool row's facts follow its subject; only a duration or a ✕ sits at the right.
    - A row's chevron takes its icon's slot on hover or when open, and nothing trails.
    - The list fades 12 pt at its top once scrolled off its start (`kit::edge_fade`).
    - A missing last newline is a struck return mark on the line before it, not a row.
    - The subagent's bar is 28 pt on `content` with a hairline foot and the kind at meta
      size.
    - Motion: the composer and the approval cross-fade in one shell, on the sheet's pace
      asking and the settle's answered. A fold's rows settle in over 160 ms. The latest pill
      and the find bar slide in. Reduce Motion lands them at once.
    - The face toggle's placement is the tile's, so it was left to the tile.
  - Tests:
    - the decoder: `a_background_command_is_finished_by_its_queued_notice`,
      `a_background_subagent_reports_its_figures`, `a_pasted_picture_is_on_its_prompt`,
      `a_tools_picture_is_on_its_result`, and in `media` and `output`
      `a_pictures_header_gives_its_size`, `a_picture_is_described_and_found_again`,
      `a_read_picture_is_found_in_its_result`,
      `a_background_commands_file_is_tailed_while_it_runs`, `only_a_tasks_own_file_is_read`,
      `a_long_log_keeps_its_last_lines`, `escapes_and_overwrites_are_taken_out`,
      `outputs_and_pictures_are_found_for_a_follower`;
    - the goldens: `golden__conversation__conversation_output`, `_image`, `_image_gone`,
      `client_expand_image`;
    - the client: `a_picture_is_asked_for_once_and_kept_by_its_digest`,
      `a_background_commands_lines_follow_its_call`,
      `a_plan_stays_out_of_the_fold_and_pictures_stay_in_sight`,
      `the_work_session_reads_its_times`, `background_work_says_how_it_stands`,
      `a_plan_has_a_title_and_a_word`, `progress_counts_done_and_names_the_task_on_show`,
      `the_fallback_counts_down_from_five_minutes`, `a_thumbnail_keeps_its_pictures_shape`;
    - the face in a window: `a_picture_is_fetched_when_shown_and_opens_large`,
      `background_work_sits_over_the_composer`, `thinking_is_a_line_that_opens`;
    - end to end: `the_face_shows_the_work_beyond_words`, which writes the build's output file
      as it runs and appends its notice, with the goldens `conversation-work` and
      `conversation-work-verbose`.

- ✅ **The prompt rail is a cached view of its own** (2026-09-27, frame-path pass). The rail
  (a tick per prompt down the list's right edge) was built on every frame of the face: each
  prompt's text cloned and cut to its first line, a label formatted, and eighty positioned
  ticks laid out and painted, though a frame that scrolls or grows an answer changes none of
  it. The rail is now `conversation::view::rail::Rail`, placed with `Entity::cached`. The face
  hands it the prompts when its rows are rebuilt, and works out their words again only when a
  prompt's row or revision changed. The rail draws again only when that, the row count, the
  theme or the zoom changed. A headless panning frame over 80 turns fell from 1.19 to 0.83 ms
  at p50 and from 3.0–3.5 to 2.7 ms at p99 (`docs/MEASUREMENTS.md`, "the prompt rail off the
  face's frame").
  - Test: `the_prompt_rail_draws_only_when_its_prompts_change` (a pan and a streaming answer
    reuse it; a new prompt draws it with one more tick; a tick's click still moves the list).
  - What is left of a streaming frame is GPUI's layout of the rows in view and gpui-kit's
    `TextView`, which parses the live block's whole Markdown again on every word (about a
    quarter of a headless word frame). That parse belongs to the gpui-kit fork.
- ✅ **A streaming answer's Markdown parses from its last block** (2026-09-28, gpui-kit fork
  `dbd18ca4` and `577b935d`). The face keeps handing `TextView` the live block's whole text
  each frame, and the fork now treats Markdown that extends the last text as an append.
  Only the last block is parsed again; a list is parsed from its last item. The parse runs
  on the UI thread when that part is small, so the word lands in the frame that brought it
  at its exact height. Text that may hold a link or footnote definition (`]:`) or open
  with frontmatter is still parsed whole, because the new text can change earlier blocks.
  MDX is also parsed whole, since an append onto a failed parse would drop text. The face
  needed no change. This beat a `push_str` call from the face: the model's text already
  arrives whole, and every other `TextView` user that streams through `set_text` gains too.
  A headless word frame fell from 1.33–1.38 to 1.10–1.16 ms at p50 (`docs/MEASUREMENTS.md`,
  "a streaming answer's Markdown parsed from its last item").
  - Tests, in the fork: `markdown_appended_a_byte_at_a_time_parses_as_the_whole_text`
    checks each step of several samples against a whole parse, and
    `set_text_extending_markdown_parses_only_the_last_block_at_once` and
    `set_text_extending_markdown_with_a_definition_resolves_earlier_references` cover the
    rest.
  - A long block other than a list, such as a big table or code fence, is still parsed
    whole on every word.
- ✅ **Hook events are a type, and the old install and old Claude Code paths are gone**
  (2026-09-28). The event a hook names was a string, matched against literals in the tracker,
  listed apart in `HOOK_EVENTS`, and declared as `"PermissionRequest"` in two crates, with
  `"Report"` and `"Statusline"` invented in the CLI. It is now `slopty_agent::HookEvent`: one
  variant per event Claude Code sends, plus `Report` and `Statusline` for what `slopty hook`
  posts, each spelled on the wire as its variant, and `Other` for any event a newer Claude Code
  adds. `HOOK_EVENTS` is the list of variants the relay registers, and every match on an event
  is on the enum, so a new variant has to be placed in the tracker's transition.
  - Only the args form of the relay's entry (`command` the binary, `args: ["hook"]`) was ever
    written since the path-with-spaces fix, so `is_relay` no longer reads a shell command line
    `<path> hook`, and `install` no longer collapses duplicates. It repoints the first relay
    entry of an event or adds one. Pre-release, a settings file an old build wrote is
    reinstalled by hand.
  - Every `Stop` in the captures (`tests/fixtures/conversation/*/hooks.jsonl`, Claude Code
    2.1.283) and in the 2.1.261 probe above carries `last_assistant_message`. So a finished
    turn no longer rereads 256 KiB of transcript: `wants_transcript` asks only for a question
    or an elicitation raised by a notification that does not spell it out
    (`agent_needs_input`, `elicitation_dialog`).
  - Tests: `an_event_is_spelled_as_its_name_and_a_new_one_reads_as_other`,
    `blocked_and_done_say_what_they_want`, `recognises_our_command`,
    `install_is_idempotent_and_uninstall_restores`,
    `every_captured_event_is_one_the_relay_registers`.
- ✅ **A permission request goes to the worker once** (2026-09-28). The relay used to post a
  `PermissionRequest` as `CtlRequest::Hook`, wait for its reply, then open a second connection
  with `CtlRequest::Permission` carrying the same payload, and the worker read that JSON twice,
  once per request. Now `CtlRequest::Permission` is the only request for that hook. The control
  socket reads its payload once, takes it in as it takes any hook (the followers' board and the
  agent table), and hands the parsed hook to `follow::ask` to hold. A payload that does not
  read, or a session the worker does not run, is a `CtlReply::Error`, which the relay reads as
  no decision. This amends the note under **Approvals** that the worker must not apply the
  payload a second time: there is no first time any more.
  - The payload stays a JSON string inside the JSON line. Carrying it as raw JSON
    (`serde_json::value::RawValue`) would drop the escaping, and the layering allows it, but
    `CtlRequest::Hook` is built in callers this change did not own (the status-line relay, the
    worker's e2e, the e2e harness), so it is left for one change that moves them together.
  - Tests: `a_permission_request_prints_the_workers_decision` sees one request, the ask;
    `a_worker_that_does_not_decide_leaves_the_dialog_to_claude_code` and
    `the_wait_for_a_decision_is_bounded` answer that one connection.
- ✅ **The composer answers questions, lists commands and mentions files** (2026-09-28, read
  out of the Claude Code 2.1.283 bundle; wire change, goldens below). The face could show an
  `AskUserQuestion` but not answer it, and the person had to know every command's name and
  every path by heart. The composer now finishes those loops itself, still without typing
  into a TUI menu (`.research/ui-wave5-2026-09-28.md` §1.1, §1.4, §1.5, §1.9, §1.10, §1.11).
  - **A question is answered through the permission hook.** The 2.1.283 bundle takes a
    `PermissionRequest` allow that carries `updatedInput` for exactly two tools,
    `AskUserQuestion` and `ExitPlanMode` (the set its permission check exempts from asking
    again). It ignores an allow *without* an input for them and shows its own dialog. So:
    - `Verdict::Answer { answers }` is a new answer. The worker (`permission::decision`)
      turns it into `Decision::Allow { updated_input }`: the call's own input plus `answers`,
      keyed by each question's text. That is the map `AskUserQuestion` reads, and it takes a
      string or an array there. The face sends one string, a multi-select's labels joined with
      `", "` as the TUI joins them, and typed words for a question answered in the field.
    - `Decision::Allow` grew `updated_input`. An allow for either tool carries the call's input
      back, because a bare allow would leave the TUI's dialog up. Every other tool's allow
      carries none: an input there would send the call through the permission check again.
    - `Question::options` became `Choice { label, description }`, so the card can show what
      each option means.
    - This was checked by reading the bundle, not by a capture. `cargo xtask fixtures` runs a
      model and was not run for this change. A capture of an answered question is still owed.
  - **The question card.** It takes the composer's shell as the permission prompt does, one
    question at a time with "1 of 3". Options are two-line rows with a circle that fills when
    picked, and there is a field for words of one's own. A single choice goes on at once and
    is the answer on the last question. Digits 1–9 pick while the field is empty, ↑/↓ move and
    Enter goes on. Skip is a denial with words, which lets the turn go on. The settled line
    says "Answered: …". Digits are read from the field's own text, so the field needs no key
    bindings of its own.
  - **A plan is approved or kept.** `ExitPlanMode` keeps the permission card with "Approve
    plan" (an allow carrying the plan's input) and "Keep planning" (a denial with the field's
    words and no interrupt). There is no "Always allow": a plan is not a rule.
  - **Slash commands come from a table and the disk, not from the mod.** *Superseded
    2026-10-04: the mod sends Claude Code's own list and the table is gone. See "Claude Code's
    own command list and model aliases come from the mod" below.*
    `ConversationEvent::Commands` carries the whole list after `Current`, and again when it
    changes. Each follower looks every five seconds. The list is:
    - Claude Code's own commands, from a table read out of the version the fixtures pin
      (`slopty_agent::commands::BUILT_IN`, 2.1.283), with its bundled skills;
    - the project's `.claude/commands` and `.claude/skills`, in the agent's directory and each
      directory above it short of home, then the person's under `~/.claude`, then every
      enabled plugin's, named `<plugin>:<name>`.

    A command file's subdirectories namespace it (`git/commit.md` is `git:commit`), and a
    skill marked `user-invocable: false` stays out. The mod's `command.list` would be exact,
    but its shape is unverified and it exists only where the mod is trusted, so the table
    stands until a capture pins it.
  - **The command menu.** It is the shell's section nearest the field, opened by a leading `/`
    while the caret is in the first word of a one-line draft, never by a `/` mid-line. Names
    that start with the query rank first, then names that hold it, then descriptions; the
    project's and the person's rank ahead of Claude Code's own. ↑/↓ move, Enter or Tab picks,
    and Esc closes it until the caret leaves the word. A pick writes `/name ` into the draft
    and nothing more. Sending still types the draft raw.
  - **Mentions ride the conversation stream.** An `@` that starts a word sends
    `ConversationRequest::Search { session, query, limit }`, answered as
    `ConversationEvent::Found` on the session's conversation stream. The research named a
    folder request, but the worker knows the agent's directory (its foreground process's) and
    the client does not, and the answer lands on the stream the face already reads. The
    palette's `FindFiles` stays the palette's. The walk (`slopty_worker::file::mention`)
    honours `.gitignore` as quick open does, keeps at most 50 000 paths, and is kept per
    follower. It is walked again when a hook has fired since (the agent may have made or
    removed a file) or after 30 seconds. A folder pick writes `@dir/` and keeps asking one
    level down; a file writes `@path `. Claude Code reads `@path` itself, so the draft stays
    text.
  - **A sent prompt's mentions are chips.** Its `@path` words are set in the accent, and the
    paths are listed under the words as chips, glyph and name, with the path on hover. A chip
    opens the path in a tile, relative to the session's directory. The chips sit under the
    text rather than inside it because a wrapped line in GPUI cannot hold a box.
  - **The model picker types `/model <alias>`** (amending the 2026-09-27 note above). The
    model in the foot opens Opus, Sonnet, Haiku and Default over the field, and a pick types
    `/model <alias>` and Enter the way the composer types any command. `/model` takes its
    argument without opening its menu, so nothing is picked in the TUI. It only works while
    the agent is idle; mid-turn its hint says "After this turn". The permission mode stays
    text, because it only changes through ⇧Tab or a menu.
  - **Rewind hands over to the TUI.** A prompt's hover row has "Rewind…", which shows the TUI
    and types `/rewind` and Enter there. The person then picks the point in Claude Code's own
    menu, and the face shows the `Rewound` rule afterwards.
    - Ruling: a bare slash command typed because the person clicked is not driving the agent.
      It is exactly what they would have typed, and every choice after it is made in Claude
      Code's own UI. What stays forbidden is typing into a menu: its digits, its arrows, its
      picks.
    - It only works while the agent is idle, as Compact does.
  - **Recall.** ↑ in an empty composer brings back the prompt sent before it, and ↓ goes
    forward to the empty draft again. A recalled prompt that is edited becomes the person's
    own draft, and ↑ then moves the caret.
  - **An unreachable worker.** The face says "<worker> is unreachable · your draft is kept",
    dims the field, and neither Enter nor Send sends anything. The tile still covers the face
    with its state pill while the worker is away (`workspace/tile.rs`, `body_state`), so the
    line shows only once the tile keeps the face under that pill. That change is the tile
    owner's to make.
  - Tests:
    - wire: the goldens `conversation__client_answer_question`, `__client_mention_search`,
      `__conversation_commands`, `__conversation_found`, `ctl__ctl_reply_permission_answer`,
      and the changed `ctl__ctl_reply_permission_allow` and `conversation__conversation_tools`
      (a question's `Choice`);
    - the agent: `an_answer_and_a_plan_approval_carry_the_calls_input`,
      `custom_commands_are_found_where_claude_code_loads_them`,
      `the_built_in_table_names_each_command_once` (deleted with the table);
    - the worker: `a_mention_ranks_names_first_and_skips_the_ignored`;
    - the face in a window (`view/tests/composing.rs`): the command menu, a mention's search
      and pick, a question answered by digit, click and Enter, a plan kept then approved,
      mention chips, recall, the model picker, rewind and the unreachable worker;
    - the pure parts: `menu` (tokens, ranking, picks, mentions), `question` (one question at a
      time) and `approval` (the settled lines).

- ✅ **A shown face outlives a dropped link** (2026-09-29). A dropped link takes the session's
  terminal and agent state with it, and the face went with them, so an unsent draft was lost
  whenever the network blinked. The workspace now holds a face that showed when the link
  dropped (`Faces::held`). It stays on its tile with its draft, its composer says the worker is
  away, and the tile lays no away pill over it. It is let go once the worker has said again
  what runs in the session, or once its tile or worker is gone. Test:
  `a_face_stays_through_a_dropped_link_with_its_draft`.

- ✅ **An agent comes back after a reboot** (2026-09-29). Session restore brought every lost
  shell back but never what it ran, so a Claude Code conversation came back as a bare prompt.
  A conversation the person left running is now resumed with `claude --resume <id>` (Claude
  Code 2.1.283: `-r, --resume [value]`, by session id; it looks the id up under the directory
  it runs in).
  - **What is kept.** `AgentTable::resumable` says, per session, which conversation to bring
    back (`slopty_agent::resume`). The id is the hooks' `session_id`, else the file name of the
    transcript found for an unhooked agent. The directory is the agent process's own, else the
    hook's `cwd`. The flags are the ones of its command line (read from the process table)
    that shape the session: model, permission mode, effort, agent, name, added directories,
    allowed and denied tools, plugin directories other than Slopty's mod, and the
    skip-permissions pair. The prompt, `--settings`, `--mcp-config`, `--agents`, the system
    prompts and everything else are left out, because they may carry tokens. A `--settings`
    that carried Slopty's relay is noted instead, and the resumed agent gets the relay afresh.
    The permission mode the hooks last reported replaces the one the agent started with.
    `default` drops the flag. Leaving the skip-everything mode keeps only
    `--allow-dangerously-skip-permissions`.
  - **Which conversation.** The newest one. `/clear` and `/resume` end one conversation
    (`SessionEnd` with `clear` or `resume`) and start the next (`SessionStart`), and the old
    one stands until the new id is known. `SessionEnd` with `prompt_input_exit` or `logout`
    is the person's own exit and ends it for good. So does an agent process gone while the
    worker watched, by the tracker's usual rule. A clean exit (status 0) of the session's own
    program clears it too. `other`, which is also what a signal gives, keeps it (the reasons
    are the five in the 2.1.283 bundle's `SessionEnd` schema), because a reboot is exactly when
    the agent should come back. It lasts until four probes in a row see something other than
    the agent in the foreground. A `--print` run is never resumed.
  - **How it gets to disk.** The daemon's agent tick (750 ms) hands each live session's
    answer to `Worker::keep_agent`. The keeper writes the recipe only when the conversation
    changed, and an "unknown yet" (an agent seen, its id not) leaves what was kept.
  - **How it comes back.** A tile opened on `claude` runs `claude --resume <id> <flags>` as
    its command, with Slopty's mod, as orchestration starts agents. A shell the person typed
    `claude` into comes back as the shell, with the same line typed at its first prompt. That
    is the prompt after its first OSC 7 of its own (the replayed screen's does not count),
    while a line editor holds the tty (not canonical). The line goes through the person's own
    shell, so its `PATH`, aliases and the integration's `claude` function (the mod) apply. It
    lands in history, and the shell and its integration are still there when the agent exits.
    Running `$SHELL -lic 'claude …; exec $SHELL -l'` instead would not type anything, but the
    follow-up shell would lose the integration: zsh's bootstrap hands `ZDOTDIR` back before
    the `-c` runs. The typed line is dropped when a viewer types first, or when no prompt
    comes within 30 s (`FIRST_PROMPT_WAIT`). A shell without the integration never reports
    OSC 7, so its agent is not resumed.
  - **When it does not come back.** The transcript is gone (the hook's `transcript_path`,
    else `~/.claude/projects/<escaped dir>/<id>.jsonl`), or the directory is gone. Then the
    session is the shell alone, with one info line in the log and nothing on the terminal.
  - Tests: `resume::tests` (flags kept and dropped, the relay, the mode, the end reasons, the
    transcript path), `detect::tests::the_agents_own_arguments_follow_its_program_or_script`,
    `the_newest_conversation_is_the_one_to_resume`,
    `only_a_conversation_the_person_left_running_comes_back` and
    `an_unhooked_agent_is_resumed_from_its_transcript` (the table), `restore::tests`
    `a_kept_conversation_comes_back_resumed` and `each_session_keeps_its_own_conversation`,
    `session_actor` `a_line_is_typed_at_the_first_prompt_and_not_before` and
    `a_viewer_typing_first_drops_the_waiting_line`, and the e2e
    `a_claude_code_conversation_comes_back_resumed` (apps/slopty-worker). The e2e types a
    fake `claude` script into `/bin/bash`, posts a `SessionStart` hook to the control socket,
    kills ptyd, and checks that the next worker runs the script again with `--resume <id>
    --model … --permission-mode plan` in the same directory, and that the system prompt given
    at the start is in neither the recipe nor the new command line.
- ✅ **A yes or no is answered from the notification and the inbox** (2026-09-29; wire change,
  goldens below). A permission prompt was held only while a client followed the session, so a
  person away from the conversation face could only go to the terminal. Now any client may
  answer approvals, without typing into the TUI and still only through the `PermissionRequest`
  hook.
  - **Who it is held for.** `ConversationRequest::Approvals { on }` makes a connection an
    approver. `slopty_worker::conversation::Holds` holds a prompt for the session's followers
    as before (`Reach::Followers`), and when nobody follows, for the approvers
    (`Reach::Approvers`), but only a yes or no: `slopty_agent::permission::approvable`
    leaves out `AskUserQuestion` and `ExitPlanMode`, whose answer is a pick or a plan to read.
    An approver-only hold lasts `APPROVAL_HOLD` (120 s), or less when the relay's wait is
    shorter, and then goes back to the TUI undecided as it always did. Answers and news go to
    the clients a prompt was shown to (`Holds::tells`). The last follower unfollowing still
    releases the session's prompts, since the person went back to the TUI. The last approver
    leaving or stopping releases only what nobody else can answer. A follower that disconnects
    leaves a yes or no to the approvers.
  - **The TUI in front of the person.** A client that has the session's terminal focused
    while the app is frontmost, showing the TUI rather than the face, sends
    `ConversationRequest::Release { session, ask }` at once. Claude Code's own dialog then
    shows where the person looks, and nothing waits on a button they will not press. The
    release is taken like an answer: only from a client it was shown to, and only once.
  - **The workspace** (`workspace::inbox::approvals`) asks every worker it links to for
    approvals and keeps each session's approvable prompt. Its inbox row under *Needs you*
    ends in "Deny" and "Allow" (`inbox-deny-<session>`, `inbox-allow-<session>`), which answer
    it where it is. A deny carries no words, so the worker's own message goes to the model.
  - **The note.** `slopty_platform::notify` registers categories with the centre when
    `System` is made; that does not prompt. `APPROVAL` has "Allow"
    (`AuthenticationRequired`: a locked iPhone asks to be unlocked first), "Deny"
    (`Destructive`) and "Show" (`Foreground`). The delegate reads the response's
    `actionIdentifier` against the framework's `UNNotificationDefaultActionIdentifier` and
    `UNNotificationDismissActionIdentifier` (`notify::tap_of`) into `Tap::action`. The
    prompt reaches the client a moment after the agent's blocked status, so the note already
    up is replaced silently (`Note::silent`) with the buttons and the prompt's `ask` in its
    `userInfo`. It is replaced again, without them, when the prompt is no longer held.
    "Allow" and "Deny" answer through `WorkspaceView::answer_approval` and move nothing. A
    press on a prompt that has gone says so in a notice. "Show" is a tap.
  - Tests: `Holds` (`an_approver_is_held_for_without_following`), `hold_for`
    (`an_approvers_hold_is_bounded`), `approvable`
    (`only_a_tool_call_is_approvable_from_outside_the_conversation`), the note model and the
    delegate's routing with no centre
    (`the_approval_note_answers_in_place_and_shows_on_demand`,
    `a_response_becomes_a_tap_or_a_button_press`), the workspace's keeping
    (`a_yes_or_no_is_kept_until_it_settles_or_is_taken_once`), the note's buttons
    (`an_approval_note_carries_the_buttons_while_its_prompt_is_held`), headless
    `an_approval_is_answered_from_the_note_and_the_inbox_where_they_are` and
    `a_prompt_whose_terminal_is_in_front_goes_back_to_it`, and the worker end to end
    (`the_tables_holder_answers_without_following_and_the_tui_asks_otherwise`, in the worker's
    `tests/threads.rs`). That last test runs the real relay as its own child and sends the
    bounded hold as a control-socket request.
    No test posts a real notification. Goldens: new `conversation__client_approvals_on` and
    `conversation__client_release`.

- ✅ **A note's verdict waits for its prompt, and an answered prompt takes its note away**
  (2026-09-29). "Allow" on a note can arrive before its prompt: the tap launched the app, or
  the link is new and the worker has not yet sent what it holds. The verdict now waits for the
  prompt, answering it as it arrives, until the worker has had three seconds since it was asked
  for approvals, or fifteen for a worker not reached; then it says the prompt no longer waits,
  or that the worker was not reached, as a toast with the app in front and as a note of its
  own while it is away. "Allow" and "Deny" no longer bring the app forward. Once this client
  answered, the agent still reads as waiting until the worker says the prompt settled; its
  note is withdrawn then instead of posted again without the buttons. Tests:
  `a_notes_answer_waits_for_its_prompt`,
  `an_approval_note_carries_the_buttons_while_its_prompt_is_held`.

- ✅ **A resume is never typed with a control character, and a prompt is shown before it can
  be settled** (2026-09-29).
  - The flags a resume keeps include free text (`--name`, `--add-dir`, `--plugin-dir`), and a
    resume in a shell is typed at its first prompt. Quoting does not stop the line editor from
    acting on a `\r`, a `\x03` or an escape as it is typed. `resume::invocation` now leaves out a
    value holding one, and `Recipe::reopen` refuses to type a line that holds one (a kept recipe
    is a file) and reopens the shell alone. Tests: `a_value_with_a_control_character_is_not_kept`,
    the tampered recipes in `a_kept_conversation_comes_back_resumed`.
  - A client starting to answer approvals, or starting to follow, was sent the held prompts
    after the follows' lock was let go, so another client's answer could settle one in between
    and its `Settled` reach the client before the `Asked`, leaving a settled prompt on show.
    `follow::show_held` now queues them under the lock, and a new prompt goes on the broadcast
    under it too; a prompt is settled only under that lock, once out of the holds, so its
    `Asked` is always ahead. Test: `a_prompt_shown_to_a_new_approver_goes_out_ahead_of_its_settling`.

- ✅ **A turn that leaves work running is paused, not done** (2026-09-30; wire change). Since
  2.1.145 Claude Code's `Stop` lists what the turn left behind:
  - `background_tasks`, each `{id, type, status, description, command?}`;
  - `session_crons`, each `{id, schedule, recurring, prompt}`.

  A turn that started `cargo build` in the background and said "I'll check when it's done"
  used to read as `Done`, so the person was told it had finished. When the task ends, Claude
  Code wakes the session with a new `UserPromptSubmit` whose `prompt` is a
  `<task-notification>` block with a `<summary>`.
  - **The state.** A `Stop` with any task or cron is `AgentStatus::Waiting { tasks, crons }`.
    It raises no attention and sends no done notification. Its detail is the first task's
    description, else the first cron's prompt, clipped to 200 characters (`PENDING_TEXT_MAX`).
    The wake's prompt starts the next turn as `Working`, with the notification's summary as its
    detail. A `Stop` with both lists empty is `Done` and announced, and so is a `StopFailure`.
    A paused agent is not at rest, so a nested agent's hooks cannot take its conversation
    over (`owns`). A paused agent also ignores `idle_prompt`. In 2.1.281 the idle notifier fires
    after the threshold whenever the session is not loading, no dialog is open, and no loop
    wakeup or quota resume is armed; running background tasks do not stop it. So a paused
    turn sitting at its prompt would otherwise turn into `Blocked(IdlePrompt)` after a minute
    and lose both its state and its hold on the host. No fixture was recorded for this: the
    notifier lives in the interactive UI, and the recorder drives headless `-p`. A paused turn with tasks out keeps the host awake (workers.md, sleep policy,
    amended the same day).
  - **Evidence.** `cargo xtask fixtures claude --only background` records the pinned 2.1.283
    official build against the canned API (`claude_mod::FakeApi`). A scratch home is used, so
    no account is involved: the real one had hit its weekly limit. The scripted model starts
    `sleep 3; echo woke` with `run_in_background`, answers the wake, and stops. The recording
    (`tests/fixtures/conversation/background`) holds the paused `Stop` with the running task,
    the `<task-notification>` prompt, and the final `Stop` with empty lists.
  - **Claude Code's own subagents are not the person's.** A `SubagentStop` with an empty
    `agent_type` comes from Claude Code's own helpers (the compaction fixture has one).
    `Hook::is_internal_subagent` says so, and the conversation face does not open a subagent
    transcript for it.
  - Tests:
    - `slopty-agent`: `a_stop_with_work_out_waits_and_the_last_one_is_done` (including
      `idle_prompt` while paused),
      `a_paused_agent_keeps_its_conversation_against_a_nested_one`,
      `an_internal_subagent_is_told_by_its_empty_type` and
      `a_forwarded_stop_keeps_its_work_and_cuts_its_text`.
    - `slopty-agent`, `tests/hooks.rs`, on the recordings:
      `a_turn_with_a_command_out_pauses_until_the_command_wakes_it` and
      `claude_codes_own_helpers_stop_with_an_empty_type`.
    - `slopty-workerd`, `handoff`: `a_paused_turn_raises_no_done` sends the recorded `Stop`s
      to the control socket.
    - Golden: `worker_agent_waiting`.
    - `slopty-tools` reports the state as "paused".

- ✅ **The pull request and worktree a status line names reach every client** (2026-09-30;
  wire change). Claude Code gives its status line command `pr: {number, url, review_state?,
  kind?}` (`review_state` is `approved`, `pending`, `changes_requested` or `draft`; `kind` is
  `mr` for a GitLab merge request) and `worktree: {name, path, branch?, original_cwd,
  original_branch?}`. No hook carries either. Slopty's status line wrapper already forwards
  that input as a `Statusline` hook, and `statusline::hook` now reads both.
  - `AgentTable::branch` keeps them per session, and the worker broadcasts
    `WorkerMsg::AgentBranch { session, pr, worktree }` when either changes. It is also in the
    greeting and in a resync.
  - The branch lives with its agent. A status line in a session with no agent is passed over,
    and the entry goes wherever the agent's tracker goes. A client hides the chip whenever
    the agent's status is `None`, and no message clears it separately.
  - Tests:
    - `the_pull_request_and_worktree_are_read_from_the_status_line_input`.
    - `the_branch_is_told_when_it_changes_and_goes_with_the_agent`.
    - `slopty-workerd` `a_status_lines_pull_request_reaches_every_client`, where a later
      client gets it in its greeting.
    - Golden: `worker_agent_branch`.
  - The app wears it on the agent's header (ui.md, "What a shell hands over shows beside it,
    and a page not asked for waits for a yes").

- ✅ **Claude Code's phone pushes are held while a client is focused on the agent**
  (2026-09-30). Since 2.1.181, Claude Code skips its Remote Control push notifications while
  the file named by `CLAUDE_CLIENT_PRESENCE_FILE` exists. Without that, a person watching the
  turn in Slopty also had their phone buzz for it.
  - Every session gets `CLAUDE_CLIENT_PRESENCE_FILE=<data dir>/presence/<session>`, and the
    worker keeps that file only while some client is focused on the session's tile. Focus is
    the terminal focus report the clients already send (terminal.md, "A shell's browser and
    editor are the client's").
  - A client that disconnects takes its focus with it, and one that reconnects starts
    focused on nothing. A session that ends or exits takes its file with it.
  - Claude Code only `stat`s the file (2.1.281: `await stat(file)` in the presence pulse), so
    a file left behind would hold its pushes for good. Nothing else can be read from it: no
    time and no pid. So the worker removes the directory on SIGTERM or SIGINT, empties it when
    it starts, and a crash is covered by launchd restarting the worker.
  - A tmux pane drops the inherited variable (zsh, bash and fish scripts). A tmux server
    started in one session hands that session's variables to panes attached from any other,
    so the file could stand for a tile nobody is looking at.
  - The files are made and removed in order, off the runtime.
  - Tests: `presence_is_there_while_any_client_is_focused`,
    `an_old_connection_ending_does_not_drop_the_new_one`, `a_presence_file_comes_and_goes`,
    `a_gone_session_is_forgotten`, `slopty-pty` `a_tmux_pane_drops_the_inherited_presence_file`,
    and `slopty-workerd` `the_presence_file_follows_focus`. In that last test, a real shell
    prints the variable, and the file follows a real client's focus, disconnect and
    reconnect, then goes when the worker is sent SIGTERM.

- ✅ **After a worker restart, Claude Code's own registry puts the agents back** (2026-09-30;
  read from the registry since 2026-10-05). A restarted worker finds its agents again by
  process, but it loses what only hooks had said: a permission prompt, a question, the
  conversation id. Those came back only with the next hook. Each live Claude Code keeps
  `~/.claude/sessions/<pid>.json` (under `CLAUDE_CONFIG_DIR` when set), which `claude agents`
  lists, with `sessionId`, `cwd`, `kind`, `version` and `status`:
  - `busy`, `idle`, `shell` or `waiting`;
  - while waiting, `waitingFor`: a permission prompt, input needed, a sandbox request, a
    worker request, or an open dialog.

  How it is read:
  - The worker reads the files itself (`roster::registered`), once, on the second agents tick
    after starting. It used to run `claude agents --json`, which cost a process and, through a
    managed launcher, a full managed launch. Claude Code 2.1.286's own lister reads the same
    files (only names that are a pid in canonical decimal, the pid taken from the name), so
    nothing is lost. A file whose process is gone is passed over; the `.key` beside each file
    is never read.
  - Each tracker is matched by pid, or by a direct child of its pid: a managed launcher runs
    Claude Code as its child, so the terminal's foreground process is the launcher's. It gets
    its conversation id back.
  - It gets its status back only when hooks will keep that status current: the relay is
    registered in `~/.claude/settings.json`, or the agent's own command line carries it.
    Otherwise a restored `Blocked` would never be cleared.
  - Nothing is restored over a hook heard since the start, and a restored status raises no
    attention.
  - `roster::Listed::status` maps `busy` to `Working` and `idle` to `Idle`. A permission
    prompt or sandbox request maps to `Blocked(Permission)`, and any other wait to
    `Blocked(Question)`.
  - Tests: `statuses_read_as_the_hooks_would_say_them`, `the_registry_lists_the_live_sessions`,
    `claude_codes_own_list_restores_what_the_hooks_had_said`,
    `a_launchers_agent_is_found_by_its_child`, and `claude_codes_session_registry_reads_as_a_status`
    on the recorded `sessions/4321.json`. The xtask background capture copies the run's own
    registry file out of its scratch home, with the pid, the clocks, the socket and the name
    fixed.

- ✅ **The mode chip follows the mode the agent is in now** (2026-10-01). Shift-Tab in the TUI
  moves the permission mode, and fires no hook of its own. The chip used to show the mode the
  last prompt was sent in, so a switch showed only with the next prompt, and a switch while a
  turn ran not at all until then.
  - The agent's status carries the mode it last said (`AgentEvent::mode`, `SessionAgent::mode`:
    `HeardMode { name, heard_ms }`). A hooked agent says it in every hook's `permission_mode`;
    an unhooked one in the `permissionMode` of the newest prompt in its transcript
    (`transcript::prompt_mode`). The name passes through as Claude Code spells it, so a mode a
    newer Claude Code adds shows as named.
  - A hook that changes only the mode is an event of its own, quietly (`AgentTable::apply`), so
    a switch reaches every client with the next hook: the tool call, the answer or the turn's
    end that follows it.
  - The chip takes the freshest of three words, by time: the prompt's mode, a permission
    prompt's, and this one.
  - Nothing is typed into the TUI to learn or change it; the chip stays read-only.
  - Tests: `a_mode_change_alone_is_an_event_that_carries_it`,
    `a_transcripts_mode_moves_only_an_unhooked_agent`, `the_newest_prompt_names_the_mode` and
    `the_mode_chip_follows_the_live_mode`.

- ✅ **A block with no prompt held here is still a request on the thread** (2026-10-02). A
  `PermissionRequest` reaches a held prompt only while someone follows the thread, and an
  `AskUserQuestion` only then too, so a question asked while nobody watched set the tile's
  "Has a question" and filled the inbox, yet the thread showed no card.
  - The codec opens a request for any blocked status (a permission, a question, an
    elicitation) that no prompt is held for. Its title is the wait's words, it has no answers
    and no questions, and its id starts with `terminal-`. The UI then shows "Answer in the
    terminal" as its one button, and the worker takes that release as done, since the TUI
    holds the question already.
  - The worker's daemon sends a block's status just before the prompt it holds for it, so the
    request opens only once the block has gone 400 ms with no prompt (`ASK_GRACE`, looked at
    on the worker's 250 ms tick, `Observed::waited`). A held prompt never flashes a card it
    then replaces, and never leaves a withdrawn one in the thread.
  - A held prompt that comes later still withdraws the card and takes its place. A block a
    held prompt stood for asks nothing more once the prompt is settled: the block's start
    time marks it, so a status sent again after the answer opens no card.
  - The block's end settles it: answered in the terminal when the agent goes back to work,
    else withdrawn.
  - The "Always allow" button names a mode in words ("/work; accept edits mode"), split the
    way the client's sentence case splits it, so a mode Claude Code adds later still reads.
  - Tests: `a_block_with_no_prompt_held_asks_in_the_terminal`,
    `always_allow_says_a_mode_in_words`, `a_question_asked_in_the_terminal_is_shown_on_the_thread`
    and `a_hooks_folder_outranks_the_terminals_from_the_first_hook`.

- ✅ **A `claude` typed in a Slopty shell is wired as one Slopty starts** (2026-10-03, readiness
  audit A7). An agent opened with ⌘⇧T or the palette got the relay's hooks, Slopty's tools and
  a pinned conversation (`Worker::as_agent`), but one typed in a shell got only the mod: its
  permission prompts reached the app only once the hooks were installed in the person's
  settings, and it had no `project_*` or `task_*` tools.
  - **One rule.** `slopty_agent::hooks::wired` is what both doors run: a conversation pinned to
    a fresh id, the relay on the one `--settings` (the person's own merged in, the status-line
    wrapper in front of their line), and `--mcp-config` serving `slopty mcp` when the session
    has a server. A run wired already, or one that prints and exits, stays as it was given.
  - **The shell asks the CLI, not the worker.** The integration's `claude` function (zsh,
    bash, fish) runs `slopty hook wire -- <words>`, which answers from the session's own
    variables: `SLOPTY_SESSION` (the relay reports to it), `SLOPTY_SERVER` (tools only with a
    server, since `slopty mcp` finds it there) and the mod's directory and socket. Everything
    the wiring needs is in the session already, so a round trip to the worker would only add
    latency to every start. The CLI is the relay itself, so the hooks name this binary.
  - **How the words travel.** The answer is NUL-ended words: the variables to set, an empty
    word, then the arguments. A prompt with spaces, newlines or an empty word passes through
    whole. bash 3.2 (macOS's) drops NULs from a command substitution, so zsh and bash read the
    words with `read -d ''` from a process substitution, and fish with `string split0`. fish
    sets the variables through `env`, since a `set -lx` in a loop ends with the loop and
    `set -f` needs fish 3.4 (Ubuntu 22.04 has 3.3). No answer (no separator: an older CLI, an
    error) runs the words as typed.
  - **The person's own binary, unmodified.** The shell still runs `command claude` with the
    words: nothing wraps the process, the foreground program is Claude Code itself, and every
    flag Slopty adds is one Claude Code publishes (`--settings`, `--mcp-config`,
    `--session-id`, `--plugin-dir`). Options before a subcommand are taken by its root
    (`claude --session-id … --settings … mcp --help` prints the subcommand's help, checked with
    2.1.287), so `claude mcp list` and the rest still work wired.
  - **What it costs.** `slopty hook wire` takes about 8 ms over a process start
    (`docs/MEASUREMENTS.md`, "a typed claude's wiring"), against the hundreds Claude Code takes
    to start.
  - A typed `claude` that a reboot brings back (`claude --resume <id> …` typed at the shell's
    first prompt) goes through the same function, so it comes back wired too.
  - Tests: `a_persons_claude_is_wired_once` (slopty-agent),
    `a_typed_claude_is_wired_by_what_its_session_holds` (slopty-cli: outside a session, without a
    server, the person's own mod flag, no mod on disk), `a_typed_claude_runs_as_the_cli_wires_it`
    (slopty-pty: every shell runs the CLI's words exactly, an empty word and a space kept, falls
    back when the CLI refuses, the opt-out, the person's alias),
    `a_typed_claude_is_wired_as_one_slopty_starts` and `a_typed_print_run_gets_only_the_mod`
    (slopty-cli, end to end: the real CLI in zsh, both bashes and fish on a terminal as ptyd
    starts them, a stand-in `claude` writing down its arguments), and
    `claude_opened_in_a_tile_is_started_as_slopty_starts_its_agents` and
    `a_claude_code_conversation_comes_back_resumed` for the worker's side.

- ✅ **A conversation running in the background comes back attached, not resumed**
  (2026-10-04). `claude --resume <id>` refuses a conversation that still runs in the
  background (`claude --bg`), so a session lost to a reboot fell back to a bare shell. The
  worker now reads Claude Code's own registry (above) once it restores sessions, and only when
  one of them held a conversation. A conversation registered as a live session of any kind
  but `interactive` (`bg`, `daemon`, `daemon-worker`, as Claude Code itself tells background
  ones) opens with `claude attach <id>`: as the tile's command, or typed at the first
  prompt of the shell it ran in. Nothing Slopty gives a new agent is given again, since the
  session already runs with it. Any other conversation is resumed as before. Tests:
  `a_conversation_running_in_the_background_is_attached_not_resumed`,
  `the_background_conversations_are_claude_codes_own_list` (a registry the test writes) and
  `background_sessions_are_the_live_ones_run_with_bg`.

- ✅ **A prompt Claude Code takes back settles the agent and comes back to the list**
  (2026-10-04). An Esc just after Enter puts the prompt back in Claude Code's input. Nothing
  reaches the transcript and no `Stop` fires, so the agent read as working forever, and a
  message Slopty sent looked sent. No hook says this happened. Claude Code's own terminal
  title does show it, since the title goes back to its idle mark, and the title is already one
  of the signals the agent table reads.
  - After `UserPromptSubmit`, if the transcript stays silent and the title reads idle for 4
    probes in a row (about 3 s), the agent is idle, by its title. Any transcript record, any
    other hook or a spinner in between disarms or restarts the count, so a turn that began is
    never taken as gone back. The next hook speaks for the agent again.
  - The composer remembers the message it typed last until the transcript shows it. If the
    agent comes to rest by its title first, the message goes back to the head of the list,
    held as "Not taken: Claude Code put it back in its input". It is not typed again, since
    it is in the terminal already. It cannot be edited there, and it can be withdrawn.
  - Tests: `a_prompt_taken_back_leaves_the_agent_idle_by_its_title` (slopty-agent) and
    `a_message_claude_code_takes_back_is_held_as_taken_back` (the composer on a real
    terminal).

- ✅ **A session started in a worktree is resumed without `--worktree`** (2026-10-04, readiness
  A11). The question was whether a reboot's resume should keep the flag. It should not.
  - Claude Code 2.1.281 records the worktree a session runs in (`worktreeSession`) and, on
    `--resume`, enters that worktree again when it still exists. Given again, `--worktree`
    would make a second worktree beside it. `--tmux`, which needs `--worktree`, goes too.
  - The resume runs in the directory the hooks last named, which for such a session is the
    worktree.
  - Test: `a_worktree_is_entered_again_by_claude_code_not_made_again`.

- ✅ **Claude Code's own command list and model aliases come from the mod** (2026-10-04, prune
  FIX 2 and FIX 3). The slash menu came from a table of 2.1.283's commands compiled into
  Slopty, and the model menu from four aliases written in `observed::MODELS`. Both went stale
  with each Claude Code release.
  - **What the plugin API publishes** (2.1.286's own declarations, `.claude-plugin/types`):
    `$.command.list()` gives every command the person can run now, built-in, plugin, user and
    MCP alike, in the typeahead's order, each with its name, its typeahead line and its source.
    It gives no argument hint. `$.config.list()` gives the `/config` menu's rows; the `model` row
    is a choice whose options are the aliases `/model` takes (`default`, `sonnet`, `opus`,
    `haiku`, `fable`, `best`, the `[1m]` ones, `opusplan` on 2.1.286) and whose value is the one
    in use. `$.session.model()` gives the resolved model id.
  - **The mod sends a `catalog`** right after its `hello`, and again after a main-loop turn
    when it changed. The worker takes it only from a trusted mod (`Seen::catalog`). The thread's
    menu is then that list (`commands::listed`): a command whose file is on disk lends its
    argument hint and tells the person's from the project's, a long line is cut, and an MCP
    prompt ranks with the plugins. The thread's models are the aliases, labelled for people
    ("Sonnet 1M").
  - **Without the mod** (an untrusted Claude Code, or none loaded) the menu is the person's and
    the project's own commands from disk, and the models stay `observed::MODELS`. Nothing of
    Claude Code's own list is guessed.
  - The recording pins the shape: `cargo xtask fixtures claude-mod` records the catalog with the
    official build against the canned API, and `the_mods_catalog_is_the_threads_menu_and_models`
    reads it back. A Claude Code whose catalog moves fails that test before the gate trusts it.
  - Tests: `the_mods_catalog_is_the_threads_menu_and_models` and
    `claude_codes_own_list_is_the_menu_with_the_disks_hints` (`slopty-agent`);
    `the_mod_is_heard_after_its_hello_passes` (the worker holds a trusted catalog only);
    `a_session_works_where_its_hooks_say` (`slopty-workerd`, the project's own command with no
    mod).

- ✅ **Private local doors yes, Anthropic requests never** (2026-10-04, the owner's standing
  ruling for every Claude Code integration).
  - Allowed: whatever integrates best, Claude Code's private and undocumented local interfaces
    included: its internal plugin and Mods APIs, state reachable from inside its process, and
    its own files, sockets and protocols. Where the published plugin API lacks something the
    internals hold, the mod may read the internals. The shape is pinned by a recording and
    guarded by the version check, so an update that moves it fails a test loudly instead of
    going wrong quietly.
  - Never: a network request to Anthropic or claude.ai in Claude Code's name. No use of its
    OAuth token or `.credentials.json`, no private endpoints (usage, limits, models over the
    network), and no imitation of its client headers. Only the real Claude Code talks to
    Anthropic; a figure Slopty wants comes from what Claude Code already fetched and holds
    locally.
  - The command list and model aliases above needed no private door: the published plugin API
    has both.

- ✅ **A managed `claude`** (2026-10-05, ruled; research in
  `.research/managed-claude-2026-10-05.md`). Some organizations put a managed launcher in
  `claude`'s place: it checks in with its control plane on every run, checks out a credential,
  and starts the real client (`~/.local/share/claude-managed/artifacts/<version>/<platform>/claude`)
  as its child. Slopty treats it as generic Claude Code managed settings plus a launcher it
  never runs in the background. No passthrough is asked of the launcher, and nothing changes in
  the organization's repository.
  - **Managed settings are read before every wiring** (`slopty_agent::managed`). The system file
    (`/Library/Application Support/ClaudeCode/managed-settings.json` on macOS,
    `/etc/claude-code/managed-settings.json` on Linux) and the server-delivered cache
    `~/.claude/remote-settings.json`, every time, since a policy can change any day. Only these
    keys are kept: `disableSideloadFlags`, `disableAllHooks`, `allowManagedHooksOnly`,
    `strictPluginOnlyCustomization`, `allowManagedMcpServersOnly`, `allowedMcpServers`,
    `deniedMcpServers`, `disableAgentView`, `availableModels`, `enforceAvailableModels`,
    `permissions.defaultMode`, and of `env` only whether `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC`
    and `CLAUDE_CODE_DISABLE_TERMINAL_TITLE` are set. A switch either file turns on is on; a
    file missing or broken says nothing. What follows:
    - **No `--plugin-dir` or `--mcp-config`** under `disableSideloadFlags`: Claude Code refuses
      the run for them. `--settings` is no sideload flag, so the relay and its status line stay.
    - **No mod** where it could not load or be heard: hooks off or managed-only, or the plugin
      network blocked by `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC`, whether the managed
      settings set it or a typed `claude`'s own environment does (`ManagedSettings::mod_off`,
      which names the first reason). Slopty clears the switch only for the agents it starts,
      and only where no policy sets it. No hello is then expected, and the thread follows the
      fallback with no error; `slopty worker doctor` names the reason ("Claude Code's live
      stream is off: …").
    - **No `slopty` MCP server** where a deny names it, an allow list does not, managed-only MCP
      has no list, or MCP comes from plugins only; the project's agent gets the pointer to the
      CLI instead (`hooks::wired_under`).
    - `hooks::wired`, `hooks::with_mcp` and `claude_mod::Installed::args` read the settings
      themselves, so every caller (a tile, a restore, a project's agent, `slopty hook wire` in a
      typed `claude`) honours them; the `_under` forms take them for tests.
  - **The launcher is asked one thing.** `claude --managed-help` prints the launcher's usage
    before any I/O, and Claude Code refuses the flag at once (checked on the official 2.1.286 in
    a sandbox: `error: unknown option`, nothing written). The worker asks it once per `claude`
    found (by real path and modification time), under the version time-out
    (`facts::launcher`). On a launcher it never runs `--version` (a control-plane round trip,
    maybe a 230 MB download, and on a machine not enrolled a browser window) or `agents`:
    - the version is the newest client's directory name under
      `~/.local/share/claude-managed/artifacts`, or the client's own path when `claude`
      resolves to one (`managed::artifact_version`);
    - the live sessions come from the registry above, read, never asked;
    - an agent in a tile is matched to its registry entry through the launcher's direct child.
    A launcher that does not answer in time is taken for one that once, and asked again next
    time.
  - **The mod posts to `http://slopty.localhost/v1/events`** over its Unix socket: a reserved
    name every egress proxy exempts (`the_mod_posts_to_a_host_no_proxy_takes`). The owner is
    also narrowing the managed egress to Anthropic's own domains.
  - On the organization's side (report section 11): the launcher no longer proxies socket
    requests (0.1.70), and the hooks worker is realm-safe (its plugin scenario passes on 2.1.289
    on darwin-arm64, darwin-x64 and linux-x64), not yet published to devices. Until a device
    has them, the mod says no hello there and the face follows the hooks and the transcript,
    with nothing surfaced as an error.
  - Tests use stand-ins only, never an enrolled or signed-in `claude`, and no request to
    Anthropic or a control plane: `slopty-stub-managed-claude` (`slopty-testkit`) keeps the
    launcher's observable contract (answers `--managed-help`, strips the credential, TLS,
    loader and `SCC_*` names, sets `DISABLE_TELEMETRY` and `SCC_MANAGED_ARTIFACT`, drops
    `--bare`, runs the stand-in `claude` as its child, logs every run). Tests:
    `a_managed_claude_is_followed_through_its_launcher` (`slopty-workerd`: wired through the
    launcher, a hook heard, recovery through the child after a restart, and nothing asked but
    `--managed-help`), `a_managed_launcher_is_never_asked_its_version` (`slopty-worker`), the
    `managed` unit tests and `managed_settings_keep_the_tools_out_and_the_relay_in`,
    `managed_settings_decide_whether_the_mod_is_added`,
    `a_typed_claude_honours_the_managed_settings`, `a_typed_claude_with_the_quiet_switch_gets_no_mod`,
    `the_mod_is_off_for_its_first_reason`, `the_doctor_says_why_the_mod_is_off`,
    `a_provisional_mod_is_dropped_at_its_first_unreadable_event` and
    `a_provisional_mod_is_dropped_with_its_blocks`.
- ✅ **A Claude Code start can plan first** (2026-10-05, readiness 10-05 G13). The thread's
  mode chip already reads the permission mode every hook reports (`permission_mode`, to
  `Meters::mode` in the observed adapter), read-only, with the hint that the mode is changed in
  Claude Code's own terminal: Slopty never cycles its mode key. What was missing was a start in
  plan mode, though `--permission-mode` is a published flag. Under a Claude Code start's
  first-message field, a "Plan first" tick (and the palette's "Start in plan mode" while that
  field has the keyboard) starts it with `--permission-mode plan`. The worker takes from a
  client's start only `--resume <id>` and `--permission-mode <mode>`, each once, in any order,
  and the mode only among those Claude Code's help lists, short of `bypassPermissions`, which
  stays with the person's own `claude` flags. A start of an agent that has no such flag shows no
  tick. The same batch lets the palette's "Upload…" reach a thread tile, which takes files since
  "Every thread takes files".
  - Tests: `workspace::tests::thread_start::a_claude_code_start_can_plan_first`; worker
    `claude_start::a_start_in_plan_mode_opens_claude_planning` and
    `a_start_claude_code_cannot_take_is_refused_and_opens_nothing`.
- ✅ **No app offer to install the hooks** (2026-10-05, readiness 10-05 §3). The "Install hooks"
  pill on an agent the worker guessed at is deleted, with `ClientMsg::InstallHooks` and
  `WorkerMsg::HooksInstalled`. The pill edited the person's global `~/.claude/settings.json` to
  reach the one `claude` nothing else wires. Every `claude` typed into a Slopty shell is wired
  by the shell integration's `claude` function (`slopty hook wire`), and every agent Slopty
  starts gets the relay on its `--settings`. What is left is a `claude` started by path, through
  an alias of the person's own, or under a wrapper. For those, the transcript and the title
  still give a status, and `slopty hook install` stays the person's own act from the CLI: the
  app no longer offers to change a file of theirs that it does not need.
  `slopty_agent::hooks::install_at` stays because the CLI runs it. Wire: two variants leave the
  middle of their enums, so the golden of every variant after them moves (a wire change; nothing
  is versioned). Test removed: the app self-test
  `the_hooks_pill_installs_the_relay_in_the_harness_home`. The header test
  `a_header_holds_one_filled_chip_and_its_slot_does_not_repeat_it` now asserts that a guessed
  agent's header offers nothing.
- ✅ **One writer, wherever the other `claude` runs** (2026-10-05, readiness 10-06 N3). A resume
  of a past session was refused only while a Claude Code that this worker observes ran it. The
  person's own `claude` in another terminal app, or through the managed launcher, was never seen,
  so "Resume a past session…" could start a second writer beside it. Claude Code refuses that
  itself only for a background (`--bg`) session.
  - *Refused in words at the resume.* The worker reads Claude Code's own registry of its live
    sessions (`~/.claude/sessions/<pid>.json`, `slopty_agent::roster`) again at each resume, not
    once at start-up. A session whose live pid holds it is refused with "Claude Code runs this
    session in another terminal" (`start::HELD_ELSEWHERE`), and nothing opens. Only the
    `<pid>.json` files are read, as before. The registry's directory is a parameter of
    `claude::start::spawn`, so a test lists a session in a registry of its own.
  - *Marked before it is picked.* A listed Claude Code session that the registry says is held
    carries the open fact `running` (`wire::PAST_RUNNING`), valued with how it runs
    (`interactive` or a background kind). The session step marks it with the running mark. A
    fact was chosen over a new field, because `PastSession::facts` is the open place for what
    the agent's own records say. No golden moves.
  - Tests: `claude_start::a_resume_of_a_session_a_claude_elsewhere_holds_is_refused`,
    `workspace::agent_start::tests::a_session_a_live_agent_holds_is_marked_running`.
