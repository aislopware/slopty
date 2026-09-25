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
  (Question|Elicitation)` or `Done` event that still has no detail makes the daemon read the
  transcript's last 256 KiB for the newest non-sidechain `assistant` record with a `text`
  block (`slopty_agent::transcript`, `spawn_blocking`, table lock released meanwhile). The
  transcript is JSONL of `{"type":"assistant","message":{"content":[{"type":"text",…}]}}`
  records; `isSidechain: true` rows are subagent chatter and skipped. No wire change: `detail`
  already existed. Found on the way: the `permission_prompt` Notification that follows a
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
