# Decisions — Audio

See `docs/DECISIONS.md` for the legend. Newest entries go at the end.

- ✅ Opus via Apple's `AudioConverter` (`kAudioFormatOpus`), no libopus: the encoder exists on
  macOS 26.5 (verified 2026-09-05: `encodes_and_decodes_a_tone` round-trips a 440 Hz tone;
  the first packet comes back 120 frames short, Opus pre-skip) and the decoder on both
  platforms. One fixed configuration, 48 kHz stereo float interleaved, 960-frame packets at
  96 kb/s, so nothing about the format travels on the wire (packets are 480 frames since 2026-09-29,
  see "The Mac's device renders 128 frames and packets are 10 ms"). Playback is an `AudioQueue` with
  three 20 ms buffers refilled from a mutex-guarded ring (≤200 ms; underrun pads silence and
  counts, overrun drops the oldest; buffers and ring superseded 2026-09-24, see "Playback holds
  about 40 ms"). The input-proc pattern: the proc hands its one slice and
  then returns a private status (`'SLOP'`) with zero packets, which `FillComplexBuffer`
  surfaces and the caller treats as "done".

- ✅ Capture rides the video `SCStream` (`capturesAudio`, `sampleRate 48000`, `channelCount 2`,
  `excludesCurrentProcessAudio`) with a second stream output of type `Audio` on the same
  serial queue. `CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer` returns
  `kCMSampleBufferError_ArrayTooSmall` (-12737) unless the list is sized by a first call with
  a null list, so the code asks for the size, allocates 8-byte-aligned words, then fetches;
  ScreenCaptureKit delivers float32 non-interleaved, which `interleave` folds to L/R.

- ✅ **Mute is client-side, decode-but-don't-play** (2026-09-05). `ScreenHandle::set_muted`
  flips an `AtomicBool` the worker reads per packet; the Opus decoder keeps running so its
  state stays continuous and unmuting resumes on the next 20 ms packet. Nothing goes to the
  host: a host-side stop would need a protocol change and would also silence the other
  clients, and the muted stream costs ~12 kB/s, less than one video frame. The pill shows on
  the active window once audio has arrived, and on every muted window regardless, so a
  silenced item is never mistaken for one whose sound stopped.

- ✅ Transport: one `Kind::Audio` datagram per packet (~240 B), sequence in the frame field,
  no parity and no retransmit; the client counts gaps as `audio_lost` and the ring pads. Host
  gate: after 300 ms without a sample above -80 dBFS no packets go out, so the many silent
  windows on a canvas cost nothing; the first loud chunk reopens it (the 300 ms hold keeps
  natural pauses from chattering). Measured 2026-09-05 on loopback (display 6, `afplay`
  Glass.aiff ×3, 5 s): 249 packets sent, 246 received, 0 lost, bench player audible.

- ✅ **Clipboard sync is polled on the host and pushed ahead of ⌘V on the client**
  (2026-09-05, protocol 4; superseded 2026-09-25 by **The host announces its clipboard only
  while watched**). `NSPasteboard` has no change notification, only `changeCount`,
  so hostd reads it every 200 ms (one Mach call to the pasteboard server); a client-side
  watcher was rejected because GPUI's clipboard API has no count and the client would have to
  push on every poll. The client instead pushes exactly when it matters: the paste chord
  goes to the host on the same ordered stream right after `ScreenRequest::Clipboard`, so the
  host's paste already sees the text. Only clients with a window open receive the host's
  clipboard (the connection filters the broadcast) and the client never writes text it
  already holds, which is what stops the loopback echo when app and host share a Mac.
  Text only; files and images stay local. `cargo xtask bundle` builds `Slopty.app` with the
  daemons and CLI beside the app (`Contents/MacOS/slopty worker install` works unchanged since
  the installer copies from its own directory), ad-hoc signed unless `--sign` names an
  identity.

- ✅ **Short audio gaps are concealed by replaying the last packet with a fade** (2026-09-12,
  `slopty_codec::audio::Conceal`; the replay superseded 2026-09-30 by **A lost packet is the
  last pitch period repeated**). Apple's Opus decoder takes no empty packet for its own
  concealment, so the client keeps the last decoded packet and, on a sequence gap of up to
  `MAX_CONCEALED` = 3 packets (60 ms; 6 of 10 ms since 2026-09-29), pushes it again faded linearly to silence across the
  gap before the packet that arrived: a lost packet is a dip, not a click, and the ring keeps
  the gap's 20 ms per packet so the next real packet is not played early. Longer gaps stay
  silence (replaying 60 ms of anything sounds worse than a pause, and the ring underruns to
  silence by itself). Counted as `audio_concealed` in the client's `ScreenStats` and on the
  overlay's second line. Unit-tested on the fade shape and the cap; the loss-injection app
  self-test (`SLOPTY_E2E_DROP_PERMILLE`) exercises the path. The jitter estimator with an
  adaptive prefill that was still open here came on 2026-09-25 ("Playback is a jitter buffer"). No `Quality` field for
  audio on purpose: keeping it off the wire avoided a protocol bump, and the gate makes "on"
  free.

- ✅ **The player opens on a blocking thread; the worker never waits for it** (2026-09-13,
  `AudioSlot::Opening`). The first AudioToolbox client in a process initialises the HAL through
  coreaudiod, which took ~7 s on the mac-studio (`docs/MEASUREMENTS.md`, 2026-09-13). Created
  inline on the first audio datagram, that wait stopped the whole screen worker: no
  reassembly, no NACK, no report, no picture. Now `Worker::open_audio` starts `Audio::new` on
  `spawn_blocking` and polls a `oneshot` on each packet; packets that land before it is open
  count as `audio_lost` (they were not played, and a 7 s prefill would be worse than the
  silence). Decode and playback stay together in `Audio`, so the sequence baseline is the
  first packet the open player sees. The worker test proves a video frame goes through while
  the player is still opening.

- ✅ **A picture on the client's clipboard is pushed ahead of ⌘V, text-style** (2026-09-15,
  protocol 46; superseded 2026-09-25 by **The host announces its clipboard only while
  watched**). A screenshot taken on the client and pasted into a remote window is the
  coding case (a picture into a chat, an issue, a design tool); the text ruling's shape
  carries it: `ScreenRequest::ClipboardImage { media_type, bytes }` goes on the ordered
  control stream right before the paste chord, so the host's paste already finds it, and a
  `(length, hash)` fingerprint keeps a second ⌘V of the same screenshot from shipping it
  again. Only the kinds the pasteboard holds as they are go (PNG, TIFF, JPEG;
  `PictureKind`): WebP or GIF would need converting on one side, and the host is the wrong
  side. The cap is 16 MiB (`MAX_CLIPBOARD_IMAGE_BYTES`; a Retina screenshot as PNG is a few
  MB), checked on both ends. One way only: the host's pictures are not polled (a screenshot
  on the host would push megabytes to every client with a window open, on every ⌘⇧4), and a
  client that wants one has the terminal's drop and paste paths. Tests:
  `a_paste_chord_pushes_the_clipboards_picture_once`, `the_kinds_the_pasteboard_holds_as_they_are`,
  golden `client_clipboard_image`.

- ✅ **Playback holds about 40 ms, and a stall's burst is trimmed rather than kept** (2026-09-24;
  the fixed 40 ms and the hard trim superseded 2026-09-25 by "Playback is a jitter buffer", the
  three device buffers 2026-09-28 by "Playback renders straight from the ring").
  The ring kept up to 200 ms behind three 20 ms device buffers, and only an overrun past
  200 ms trimmed it. After a stall, the held-up packets land together and the listener stayed
  that far behind the picture for good (a simulated 250 ms stall: 260 ms behind, still 260 ms a
  second later). Now the device holds three 10 ms buffers and the ring trims to 40 ms whenever a
  push takes it past 50 ms. Two packets landing together still fit under that cap; three are a
  burst. The same simulation now reads 70 ms after the burst and 74 ms on average afterwards
  (`docs/MEASUREMENTS.md`, 2026-09-24). A trim is one audible skip, once per stall. Capture
  audio also leaves the video's serial queue: ScreenCaptureKit delivers it on its own queue at
  `QOS_CLASS_USER_INTERACTIVE`, so a sample never waits behind a frame's encode call. Tests:
  `a_stall_burst_does_not_leave_lasting_delay`, `jitter_below_the_cap_is_kept`.

- ✅ **The host announces its clipboard only while watched, and writes a client's on that
  client's paste chord** (2026-09-25, protocol 52; replaces the two push-ahead-of-⌘V entries
  above on the worker side; the poll period, eager reads and write-only-on-⌘V superseded
  2026-09-30 by **Clipboard v2**).
  - The worker reads `changeCount` every 200 ms only while some client has sent
    `ClipMsg::Watch(true)`; otherwise the poller sleeps on a watch channel and reads nothing.
    Watching starts from the pasteboard as it is: a change made while nobody watched was made
    on the worker, and announcing it on focus would overwrite the client's clipboard.
  - An empty pasteboard does not count as seen (2026-09-29). A copy's `clearContents` moves
    `changeCount` and empties the board, and `writeObjects:` then puts the contents on without
    moving it again (measured on macOS 26). A poll that fell between the two used to mark the
    count seen with nothing to offer, and the copy was never announced. That was about 1 run in
    15 of `the_clipboard_is_announced_fetched_and_pasted_both_ways`. The worker now reads the
    board again on the next poll, which costs one more pasteboard call per poll while it is
    empty. Test: `contents_put_on_after_a_poll_saw_the_board_cleared_are_announced`.
  - A change becomes an `Offer`, richest first. Text of at most 64 KiB rides inline. PNG,
    TIFF, RTF, HTML and file URLs are listed with size and BLAKE3 digest and kept for `Fetch`
    until the next change; an answer over 64 KiB goes on a bulk stream. The file URLs of every
    item travel as one `public.file-url` representation, one URL per line. Concealed and
    transient contents are skipped.
  - Echoes are broken three ways. The `changeCount` a write leaves is never announced. Contents
    whose `com.aislopware.slopty.origin` names this worker are not announced; its value is
    postcard `(Peer, u64)`, the origin and generation. Contents whose digest matches what was
    last announced or written are not announced again, which covers Universal Clipboard.
  - A client's offer is recorded, not written. ⌘V on one of its window streams writes it:
    inline text at once, otherwise the worker fetches the missing representations and holds that
    client's window input, in order, until they arrive. After 3 s the chord goes on
    unwritten, and the next ⌘V writes what arrived meanwhile. A client's file URLs are never
    written: they name files on the client, which travel as a transfer.
  - Tests use a named pasteboard (`--pasteboard`, `SLOPTY_PASTEBOARD`), never the general one:
    the `slopty_worker::clip` unit tests and `slopty-worker`'s
    `the_clipboard_is_announced_fetched_and_pasted_both_ways`.

- ✅ **Uploads are written as `.partial` beside their name, placed per top-level entry, and
  resume from what is durable** (2026-09-25, protocol 52).
  - Each top-level entry of a drop is placed when its first file arrives. It goes to the
    base directory: the session's OSC 7 directory, else its shell's own working directory
    from the process table. When that name is taken there it goes to
    `~/.slopty/drop/<xfer>/` (`--drop-dir`, `SLOPTY_DROP_DIR`). Staging always goes there.
  - A blocking thread writes the file, fed through a channel of 16 chunks; QUIC flow control
    is the backpressure. Then fsync, the sender's mode and mtime, rename, and an fsync of the
    directory. `Done` carries the BLAKE3 digest of the whole file; on a resume the kept prefix
    is hashed again.
  - A file's stream can overtake its transfer's `Begin`, which rides the control stream, so it
    waits up to 5 s for it. `Resume` answers the partial's length after an fsync. `Cancel`
    stops the streams and keeps the partials. A finished staging transfer puts its paths on
    the pasteboard as file URLs, stamped with this worker as origin, so they are not
    announced back.
  - `Fetch` walks the path (symbolic links skipped, 10 000 files at most) and sends `Begin`
    and then one bulk stream per file. A download does not resume yet.

- ✅ **Ports are scanned on a hint or while the shell is busy, never when idle** (2026-09-25,
  protocol 52).
  - The session actor checks each PTY read for a local server's address or an OSC 8 web link
    and hints the worker. After a hit it skips the check for a second. A keystroke's echo pays
    nothing measurable and a full 64 KiB read 7 to 11 µs (`docs/MEASUREMENTS.md`,
    2026-09-25).
  - The worker scans a hinted session 250 ms later, so a server that prints its address as it binds
    is found listening. Every 2 s it looks at each session and scans one whose tty's foreground
    process is not the one ptyd spawned. It also scans one that still has listeners (a
    background job) and one whose foreground program just ended. `Ports` goes to every client
    when a set changes, and the known sets go to a client when it connects.
  - Every bidirectional stream a client opens after the control stream is a tunnel. The worker
    dials `127.0.0.1` and then `::1`, because a dev server bound to `localhost` on macOS often
    listens on `::1` only. It splices both ways with a half-close each way, and a refused dial
    resets the stream.

- ✅ **Copied files paste as a transfer, and the window's paste waits on the client for them**
  (2026-09-25, no protocol change).
  - A paste of files goes where a drop goes. In a terminal, files copied here go up to the
    shell's directory and their quoted paths are typed. In a streamed window, they go to
    staging, and the worker's pasteboard then holds their URLs, as after a drop.
  - A window's paste of files holds that window's input on the client, the chord first, until
    the staging upload ends in any way. The worker's own hold was the other choice. It was
    rejected because the upload's `Begin` leaves from a task of its own and can reach the worker
    after the chord, and because the worker would then track which client stages what. The hold
    has no time limit, since the tile shows the upload and its cancel pill, and a cancel, a
    failure or the link going ends it.
  - The client's offer for copied files holds only `public.file-url`, one URL per line, the
    format the worker's offers already use. The worker never writes a client's file URLs, so the
    offer writes nothing. It still counts, because it replaces the client's earlier offer. A
    paste would otherwise write that offer's text over the staged files.
  - Files a worker copied reach the client as that offer. The client writes the offer's text,
    remembers whose files they are, and fetches the URLs when a paste wants them. Into a shell
    of the same worker, the paths are typed as they are, with no transfer. Into another worker's
    tile they come down to a scratch directory here, go up, and the directory is removed once
    the upload ends. Pasting them into Finder on the client is not covered, since that needs a
    file promise on the pasteboard.
  - Finder names a copied file by reference (`file:///.file/id=…`). Such a URL means nothing on
    another machine. Both pasteboards resolve it to the path URL when they read it
    (`MacPasteboard::file_urls`, `MacBoard::file_urls`).
  - Drags out get a seam for the self-test. The workspace hands a drag's file promises to a
    sink, which is a system drag in the app. The self-test app parks them instead, and its
    `KeepDragged` command keeps them as a drop into a directory would. No OS drag is ever
    synthesised.
  - Tests: clipboard `copied_files_are_offered_as_urls_and_named_for_a_paste`; worker
    `a_clients_copied_files_replace_its_offer_and_write_nothing`; screen
    `a_paste_of_files_holds_the_window_input_until_they_are_there`; workspace
    `files_copied_here_paste_into_a_shell_as_a_drop`,
    `files_a_worker_copied_paste_into_its_shell_or_travel_to_another`,
    `files_pasted_into_a_window_are_staged_before_the_chord_goes`; platform
    `a_named_pasteboard_names_the_files_copied_on_it`; e2e app
    `files_copied_here_and_pasted_into_a_shell_land_there`,
    `files_copied_on_the_worker_paste_into_its_shell_as_their_paths`,
    `a_path_dragged_out_of_a_shell_is_kept_by_a_download`.

- ✅ **A shell's paste of a picture puts it on the worker's pasteboard first, held as a
  window's paste is** (2026-09-28, no protocol version change; `TermRequest::PastePicture`,
  `Dest::Attachment`).
  - Claude Code reads a pasted picture off its own machine's pasteboard. It looks on ⌃V, and on
    an empty paste, which is what a ⌘V with no text sends. Before this, the client's clipboard reached
    the worker's pasteboard only on a window's ⌘V, so a screenshot taken on the Mac or iPad
    running the app never reached an agent in a shell.
  - ⌘V or ⌃V in a shell, with a picture copied here (PNG or TIFF) and no text beside it, sends
    this client's offer when the worker has not heard it, then `PastePicture(PasteChord)` on
    the same ordered control stream. ⌘V carries `Command`, which the worker applies as an empty
    paste (bracketed when the program asked for it). ⌃V carries the key as typed. Files
    copied here still paste as a drop, and text still pastes as it did, confirm strip and all.
  - The chord is a numbered terminal input rather than a `ClipMsg`, so it keeps its place
    among the session's keystrokes, and the worker holds it the way it holds a window's ⌘V:
    the same `Held` in `conn.rs`, now keyed by a window or a session. The session's input
    behind it waits in order until the pasteboard holds the offer, or 3 s, like a window's. A
    `ClipMsg` would have had to name the session and fence off the next keystroke's datagram
    copy by other means. The client sends no datagram copy of the request either. A copy could
    reach the worker before the offer it follows and paste whatever the pasteboard held.
  - The client never sends back up a picture that came from a worker's offer. That picture
    sits on a worker's pasteboard already.
  - The conversation face attaches a picture pasted into its composer, and any file dropped on
    the face, by its path, as Claude Code takes one dropped on a terminal. It goes up with
    `Dest::Attachment` to a fresh `~/.slopty/drop/<xfer>/` on the worker (the drop directory,
    `--drop-dir` in tests), with nothing put on the pasteboard. The session's working tree was
    the other choice. It was rejected because a screenshot there shows in `git status`, can be
    committed, and a second one called the same would land elsewhere. A chip in the composer
    shows the upload, and once it lands the path is typed at the composer's cursor. Nothing
    reaches the PTY until the message is sent.
  - Tests: goldens `client_term_paste_picture_command`, `client_term_paste_picture_control`,
    `client_xfer_begin_attachment`; client `a_picture_paste_follows_its_offer_and_has_no_copy`;
    worker `a_picture_paste_holds_its_shells_input_behind_it` and
    `a_shells_picture_paste_sets_the_pasteboard_before_the_chord_goes_on` (named pasteboard);
    terminal view `a_picture_goes_to_the_worker_ahead_of_the_paste_chord`; composer
    `an_attachment_is_a_chip_until_its_path_is_typed`; workspace
    `a_picture_pasted_into_the_composer_uploads_and_its_path_is_typed`; e2e app
    `a_picture_copied_here_is_on_the_workers_pasteboard_before_a_shells_paste`.

- ✅ **Playback is a jitter buffer that aims for the lateness it measures** (2026-09-25, no
  protocol change; `slopty_codec::audio::{Jitter, Ring}`).
  - A packet's delay is its arrival, as the connection's reader stamped it, minus its sequence
    times 20 ms. Its lateness is that delay minus the least one of the last 5 s. The ring aims
    to hold, when a packet arrives on time, the 95th percentile of lateness plus one 10 ms
    device buffer, kept between 20 and 120 ms. Until a second of arrivals is in, it aims for
    40 ms.
  - Lateness past 110 ms is a stall, which no depth up to the ceiling could cover. The stall's
    packets are left out of the percentile, and so is everything released within 10 ms of one.
    A stall's backlog carries every lateness from its length down to none, and none of it
    describes the link otherwise. A lateness that did starve the ring is kept as a floor for the
    window, since it has proved it is not noise.
  - The depth an arrival reads is the ring's level before the packet plus the packet's
    lateness. A late packet finds the ring lower by exactly its lateness, so a clump reads the
    same depth as a steady stream and costs nothing. When the mean of the last 25 readings is off
    the target by more than 7.5 ms, one 5 ms slice is dropped or played twice, joined with a
    2.5 ms crossfade. That absorbs a worker clock ±100 ppm off the device's. A reading more than
    40 ms over the target is a stall's backlog, and is cut back to the target as its packets
    land, crossfaded as well.
  - The ring plays only once the next on-time packet would find the target: at the start, after
    running dry, and after a mute. A ring that runs dry ramps from the last sample played down to
    silence over 2.5 ms, so a dry ring at a buffer's edge is not a click. Running dry after a
    packet with nothing above -80 dBFS is the host's silence gate, not an underrun, and the next
    sound starts a fresh estimate, since the gate stops the sequence clock too.
  - The alternative to the percentile was the window's maximum lateness. It would absorb
    recurring keyframe bursts that the percentile calls noise when they are two packets in a
    hundred, at the price of holding any one outlier's delay for 5 s. The percentile with the
    starved floor costs about one underrun per window on such a link, 3 in 20 s with a 40 ms
    burst every 2 s.
  - Counted in `ScreenStats` as `audio_underruns`, `audio_trimmed`, `audio_stretched` and
    `audio_target`. Measured on synthetic traces in `docs/MEASUREMENTS.md` (2026-09-25). Tests:
    `a_stall_burst_does_not_leave_lasting_delay`,
    `keyframe_bursts_starve_the_ring_at_most_once_a_window`, `clock_drift_is_absorbed_by_slices`,
    `the_silence_gate_is_not_an_underrun`.

- ⏸ **Worker files paste into Finder through a File Provider domain, not a pasteboard promise**
  (2026-09-25, ruling only; nothing built yet). Finder enables Paste only for a file URL that
  already exists on disk, so neither `NSFilePromiseProvider` nor the Carbon
  `promised-file-url` pair gets a Paste on the general pasteboard. No shipping client uses them:
  Microsoft's Windows App and Devolutions RDM hand out pre-made temporary files, RustDesk an
  empty decoy it swaps after the paste, and every placeholder-and-swap scheme races the reader
  (Windows App on macOS 26 pastes zero-filled files). A dataless file is the one mechanism where
  the kernel holds the reader until the bytes exist ("reads trigger downloads", WWDC21 10182;
  clonefile(2): a dataless source "must be materialized before being cloned").
  - Design: a read-only replicated File Provider extension in Rust (objc2 `define_class!`, a
    `no_main` bin whose `main` registers the classes and calls `NSExtensionMain`) in
    `Slopty.app/Contents/PlugIns`, sandboxed with a `TEAMID.dev.aislopware.slopty` app group and
    `network.client`. On a worker's file offer the app writes a manifest to the group container,
    signals the enumerator and puts the items' user-visible URLs on the pasteboard as
    `public.file-url`. `fetchContents` streams the bytes over a connection the extension opens
    itself, since the system launches it without the app.
  - Checked on this machine: a Rust appex signed with the Developer ID and no provisioning
    profile registers with PlugInKit and fileproviderd, and the Team-prefixed group works. Its
    domain comes up user-disabled until someone switches it on in System Settings, hidden or
    visible; the testing-mode entitlement that skips this needs a development profile. So it
    ships with a one-time switch-on (`showExtensionManagementInterface()`), and CI covers it only
    under a development-signed build.
  - Open: whether a hidden domain can be switched on at all, and whether other apps raise a
    privacy prompt when they read a placeholder. Both are one manual check.
  - Withdrawn (2026-09-25): an attempt at the pasteboard route, file promises on the general
    pasteboard, sat uncommitted in the tree. Finder pastes only files that are already on disk,
    so it could not work, and it was taken out of the tree. Its diff is kept outside the
    repository, at `.research/parked/`.

- ✅ **The jitter estimate runs outside the playback lock, and a mute starts a fresh one**
  (2026-09-25, no protocol change).
  - `Playout` is now two parts. `Jitter` holds the window, the percentile and the target, and
    only the thread that decodes packets takes it. `Ring` holds the samples and the depth
    steering, and the AudioQueue callback shares it. A packet locks the ring to read what it
    saw since the last packet (`Since`), times itself against the window with the ring
    unlocked, then locks the ring again to queue. The callback never waits on the window's
    sort. A stretch now works in place instead of through a temporary vector, so nothing under
    the ring's lock allocates beyond the ring's own growth. If the ring runs dry between the two
    locks, the next packet carries the mark instead of this one. The synthetic-trace numbers
    (stall, keyframe bursts, ±100 ppm drift) are unchanged.
  - Muting emptied the ring without flagging the gate. When the host's gate closed for 110 ms
    to 1 s during a mute, every packet after the unmute read that much late against the minimum
    from before the gap. Past the 110 ms a depth can cover, each one counted as a stall, and the
    ring was cut to its floor on every packet for up to the 5 s window. A mute now marks the
    next packet as gated, as running dry after silence does. A packet more than 20 ms late after
    a quiet one also starts a fresh estimate, which covers a gate shorter than the ring's depth.
    A mute also ramps down from the last sample played instead of stepping to zero.
  - On a synthetic trace (muted from 2 to 3 s, a 400 ms gate inside the mute, 1 ms scatter),
    the old code had 85 underruns and cut 652 ms. Now there are 0 underruns, 30 ms of drift
    slices, and 60 ms mean heard delay from 4 s on. Tests:
    `a_gate_while_muted_leaves_no_lasting_cuts`,
    `a_late_packet_after_silence_starts_a_new_estimate`.

- ✅ **A worker's clipboard promise fetches over the link the worker has at the paste, and a
  fetch on a dead link fails at once** (2026-09-25, no protocol change).
  - A promise the client put on its pasteboard captured the `Remote` of the connection its offer
    came in on. Once that link dropped and came back, a paste of a promised representation (a
    picture, or text past 64 KiB that the 1 MiB prefetch had not fetched) asked the dead link's
    `ClipCache`. The fetch was never sent, the slot stayed asked, and the paste held the pasting
    app's main thread for the whole 5 s `CLIP_WAIT` before it came back empty.
  - The offer belongs to the worker, not to one connection: the worker keeps its last offer
    across links. So a promise now holds the worker's link as it is now (`ClipSync::link`, a
    `watch` that `reset_remote` updates when a link comes up or goes). A paste while the link
    is down gets nothing at once; after the relink it is fetched over the new link. Bytes whose
    digest is not the one offered are not pasted, because a restarted worker counts
    generations from 1 again.
  - `ClipCache` fails fast. A fetch it cannot send (the writer is gone) returns at once. The
    link's end (the control reader's error, or `WorkerLink` dropped) closes the cache, which
    wakes every waiting paste and answers later ones from what had arrived.
  - Measured with the worker stopped until the app gave the silent link up, then resumed: a
    3 MB promised text read from the app's pasteboard after the relink took 5010 ms and pasted
    nothing before; 38 ms and whole after. Tests:
    `a_worker_copy_pastes_promptly_after_the_link_drops_and_returns` (app e2e),
    `a_promise_is_fetched_over_the_link_the_worker_has_now` (headless workspace),
    `a_paste_on_a_dead_link_answers_at_once` (`slopty_client::clip`).

- ✅ **A drop's landing is deleted once nothing in it is going anywhere, and a gone run's are
  swept** (2026-09-26). Files that Mail and Photos promise, and every iPad drop, are received
  under `$TMPDIR/slopty-drops/<pid>-<n>/`, and nothing ever deleted those copies. Now a landing
  whose files all failed goes at once, as does one no view took. Otherwise `Dropped::landing`
  names it for whoever uploads the files to `file_drop::discard` when the upload ends. Starting
  the app sweeps the landings of processes that no longer exist (signal 0). The iPad copied each
  file under a name it had checked was free. Its completions run at once on threads of their
  own, so two files of one name could overwrite each other. `file_drop::reserve` now creates
  the name with `create_new`, numbering on a clash, before the copy. Tests:
  `files_arriving_at_once_under_one_name_never_share_it`,
  `landings_are_discarded_and_a_dead_runs_are_swept`,
  `a_failure_never_removes_anything_outside_the_drop`.

- ✅ **The pasteboard is used by one thread at a time, and the app reads it on the main thread**
  (2026-09-27). An audit asked for the client's pasteboard reads to move off the main thread,
  since a large or promised representation blocks until its owner provides it (`dataForType:`
  is a synchronous round trip to the pasteboard server). What was found:
  - AppKit's headers put no main-thread rule on `NSPasteboard`, but it is not Sendable, Apple
    has told a developer through Feedback that it is not safe off the main thread (reported by
    Wade Tregaskis, FB14885505), and crash reports show its type cache and its
    `pasteboardWithName:` table racing when two threads use it at once. UIKit declares
    `UIPasteboard` Sendable since the iOS 16 SDK.
  - So the app keeps its reads on the main thread. They already happen only after the change
    count moved and only when a remote tile takes the keyboard or before a paste, so a large
    copy costs one read, not one per frame.
  - The worker had the race: its poller reads on a blocking thread while a client's paste
    writes from another. `slopty_input::pasteboard::MacBoard` now holds one process-wide lock
    across every `NSPasteboard` call. Test: `threads_take_turns_on_the_pasteboard` (four
    threads writing and reading a private pasteboard).

- ✅ **iOS moves files through other apps: an iPad drags a row out as an item provider, and the
  Files picker uploads and saves** (2026-09-28, `.research/gap-audit-2026-09-28.md`, "Behind
  these").
  - Drag out (iPad): a `UIDragInteraction` on the window's view (`file_drop::out::offer`). At the
    lift its delegate asks the workspace what is under the touch (`WorkspaceView::drag_offers`:
    the folder row there, found by the bounds the rows were drawn at). Each file becomes an
    `NSItemProvider` whose file representation is registered, not written (`public.data`, a
    folder as `public.folder`). A receiver's load brings the file down through the download the
    Mac's promise uses, on a thread of its own, into `$TMPDIR/slopty-out/<pid>-<n>/`. That goes
    once the receiver has the files (`sessionDidTransferItems`) or the drop is cancelled, and a
    gone run's are swept. The lift shows the kind's symbol under the finger, since by default it
    lifts a picture of the whole window. UIKit leaves drags off on an iPhone, where they stay in
    one app.
  - A shell's path lifts too (2026-09-28). The delegate asks the terminal what path it printed
    under the touch (`TerminalView::path_at`). That is the detector ⌘-click uses, not a second
    one, so a wrapped path and a `:line` suffix read the same way. The workspace makes the path
    absolute against the shell's directory, as it does for ⌘-drag, except one under `~`, which
    the worker expands to its own home (every caller used to turn `~/.zshrc` into
    `<cwd>/~/.zshrc`). A path printed with a trailing `/` is offered as a folder. Off a path, a held touch still goes to GPUI's long
    press, which selects a word. Test:
    `a_path_a_shell_printed_is_offered_under_a_held_touch`.
  - Drop in (iPad): a folder from Files was refused, since it conforms to `public.folder` and
    not `public.data`, and would then have failed in `fs::copy`, which takes no directory. The
    drop now takes both, loads the representation that conforms to either, and moves what the
    system hands over into the landing whole (`file_drop::arrive`), under a name reserved as a
    file or as a directory. A move, since the copy is the app's to take; across volumes, a copy.
  - Files picker (iOS): "Upload from Files…" (the palette, and the folder's path bar) shows
    `UIDocumentPickerViewController` in import mode. The copies it hands over are moved into a
    landing and go to the tile as a drop on it would (`WorkspaceView::files_picked`), so the
    upload deletes them when it ends. "Save to Files…" (the palette, and the selected row's
    button) brings the row down into the outbox off the main thread and shows the picker in
    export mode; the copy goes once it is done. Import uses the string-typed initialiser, which
    is deprecated, because the content-type one takes a `UTType` and nothing here binds
    UniformTypeIdentifiers yet (the Mac's drag icon makes the same trade). A `FilesSeam` global
    takes the picker's asks in tests, so none shows a real one.
  - Tests: `an_offer_is_named_by_its_path_and_typed_by_its_kind`,
    `a_fetch_lands_in_a_directory_of_its_own`,
    `a_dropped_file_or_folder_arrives_whole_under_a_name_of_its_own` (`slopty-platform`);
    `a_row_under_a_held_touch_is_offered_as_the_workers_file`,
    `the_files_picker_is_offered_on_ios_only`,
    `the_files_picker_is_asked_through_its_seam_and_its_files_go_up` (headless workspace).

- ✅ **Playback renders straight from the ring on the device's I/O thread** (2026-09-28, no
  protocol change; `slopty_codec::audio::Player`).
  - The `AudioQueue` kept three 10 ms buffers enqueued ahead of the ring and refilled one each
    time the device took one, so 20 to 30 ms always sat between the ring and the device. Offline,
    a queue renders exactly what was enqueued, from frame 0, and adds nothing of its own. So the
    queue cost exactly those 20 to 30 ms, on top of the ring and the device's 17.3 ms (the Mac
    Studio's speakers: a 512-frame I/O buffer, then 317 frames of safety offset and device and
    stream latency). Video is presented on arrival, so sound trailed the picture by that much
    more than the network made it (`docs/MEASUREMENTS.md`, 2026-09-28).
  - The player is now an output audio unit: the default output on macOS, which follows the
    device the user picks, and RemoteIO on iOS. Its render callback fills each I/O buffer from
    the ring, as many frames as the device asks for, so nothing waits between the ring and the
    device's own buffer.
  - Fewer or smaller queue buffers were the other choice. A queue's callback runs on a thread of
    its own after the device has taken a buffer, not in step with the device, and 480-frame
    buffers do not divide a 512-frame I/O cycle. Two buffers would leave one I/O cycle for that
    thread to refill before the device reads past what is enqueued, and would still hold 10 to
    20 ms. The render callback holds none and has no refill thread to wait on.
    `AVAudioSourceNode` does the same through an engine and its graph; the unit is the layer the
    engine sits on, with nothing between.
  - The render callback takes the ring's lock on the device's real-time thread. What runs under
    it is bounded already: no sort, since the jitter estimate runs outside it (2026-09-25). The
    ring now starts with room for 260 ms, past the deepest it holds (the 120 ms ceiling, a
    stall's backlog while it is cut, 60 ms of concealment), so it does not grow under the lock
    either. Superseded the same day by "The device's thread never waits on the decoder's":
    the callback takes no lock now.
  - The ring's depth policy is unchanged. Its one device buffer is still counted as 10 ms, and
    512-frame renders sit within that on the synthetic traces. iOS renders 1 024 frames at a
    time unless the audio session asks for fewer: on a 20 s trace with 1 ms of scatter that
    starves nothing either, with the ring 18 ms deep. Asking the session for a 10 ms I/O buffer
    (`setPreferredIOBufferDuration`, where `slopty_platform::playback_audio_session` sets the
    category) would take up to 11 ms more off on iOS, and is the next step there.
  - The worker's encoder now encodes each 20 ms packet from its pending samples in place rather
    than collecting them into a new vector per packet.
  - Not verified here: the unit running on a real device, since that plays through this
    machine's output. `player_starts_and_drains` starts it on silence and waits for the ring to
    drain. Tests: `the_output_unit_opens_without_starting` (found, given the ring's format and
    callback, initialised, not started),
    `the_render_callback_plays_the_ring_in_order_at_the_devices_size`,
    `a_device_rendering_1024_frames_is_not_starved`; the latency traces now render 512 frames at
    a time and count the ring alone.

- ✅ **The device's thread never waits on the decoder's** (2026-09-28, no protocol change;
  `slopty_codec::audio::{Ring, Steer, Feed}`).
  - Before, the render callback locked the ring, a `parking_lot` mutex the decoder's thread took
    twice per packet. Under it the device drained its buffer out of a `VecDeque`, marked a dry
    ring and kept its ramp. Under the same lock the decoder appended each packet and steered the
    depth. A dropped slice made the deque contiguous, crossfaded it and drained it. A repeated
    one resized, rotated and copied it. A start faded in the front, and a mute cleared it. If the
    decoder was preempted while it held the lock, the real-time thread parked behind it with no
    priority inheritance and could miss its deadline. With the decoder pushing on a thread of its
    own, a 512-frame render took up to 4.1 µs at p99, 31 µs at p99.9 and 813 µs at worst
    (`docs/MEASUREMENTS.md`, 2026-09-28).
  - The ring is now single-producer, single-consumer and allocated once: 2^15 samples, 341 ms.
    The device's side is wait-free. It does a few atomic loads and stores, one compare-and-swap
    it never retries, and the copy. The decoder's side (`Feed`: the jitter estimate and
    `Steer`) sits behind a mutex the callback never takes.
  - Steering moved into the arriving packet. The decoder drops a slice from the packet, or plays
    one twice in it, before publishing it. A 960-frame packet has room for the 240-frame slice
    and its 120-frame crossfade. The depth changes by the same amount wherever the slice comes
    out; the listener hears the correction one ring depth later, which a ±100 ppm drift never
    notices. A stall's backlog is cut from each packet as it arrives, all of it but the
    crossfade (17.5 ms a packet), instead of from the ring's front. Every synthetic trace gives
    the numbers it gave before.
  - A run word says whether the device plays: an epoch and a playing bit. The decoder starts a
    run once the ring holds the target, fading in the front first, which it may write because
    the device reads nothing while stopped. The device stops a run when it runs dry. The decoder
    stops one on a mute and moves the run's start past what is queued. The device frees that at
    its next render and ramps down from what it last played. Each start takes a new epoch, so a
    stale compare-and-swap cannot stop a newer run. The decoder sees a dry run in the word on its
    next packet and counts the underrun then, by the loudness of the last packet, as before.
  - A mute can land while a render copies. The render then reads `run` a second time, sees it
    changed and plays a ramp instead of what it copied. A `write` it saw past the mute would
    show in that second read, so no sample of the next run gets out early.
  - Samples are `f32` bits in `AtomicU32`s, loaded and stored relaxed. On Apple silicon those
    are the plain loads and stores a `memcpy` makes, and any race outside the protocol stays
    defined behaviour. The copy is not vectorised: a render's median went from 417 to 500 ns,
    0.005% of a 10.7 ms buffer. Under a concurrent decoder its p99.9 fell from 22–31 µs to
    1.6–2.4 µs.
  - `try_lock` with silence when the lock is taken was the other choice. That still hands the
    device's buffer to the decoder's scheduling: a render that loses the race plays silence,
    which is the glitch, only shorter. The lock was also held across whole-ring memmoves. Going
    wait-free cost nothing measurable.
  - A full ring drops what does not fit and counts it as trimmed. That only happens when the
    device has stopped rendering.
  - Tests: `the_render_callback_never_waits_on_the_decoder` (another thread holds the decoder's
    whole side while the callback plays the queue in order, runs dry, stops the run and ramps to
    silence), `a_mute_discards_the_queue_and_the_next_run_starts_after_it`, and the latency
    traces unchanged. Measurement: `render_cost`, ignored.

- ✅ **The Mac's device renders 128 frames and packets are 10 ms** (2026-09-29, no protocol
  version change; every binary is rebuilt together; `slopty_codec::audio`). What sits between a
  sample on the worker and the listener shrank in three places (`docs/MEASUREMENTS.md`,
  2026-09-29, "audio: a smaller device buffer and 10 ms packets").
  - The player asks the macOS output device for 128-frame renders (`IO_FRAMES`,
    `kAudioDevicePropertyBufferFrameSize` through the output unit, clamped to the device's
    `BufferFrameSizeRange`). A device that refuses keeps its own size. On the Mac Studio's
    speakers the I/O buffer went from 512 frames (10.7 ms) to 128 (2.7 ms), so the device's
    share before the DAC went from 17.3 to 9.3 ms. Over a minute of real-time feeding at each
    size the HAL reported no overload. The render callback is a copy that costs the same per
    frame at any size. The setting is the process's and applies to the device the default
    output had when the player opened; after the user switches devices, the new one runs at its
    own size until the next player.
  - Opus packets are 480 frames, 10 ms, where they were 960 (RFC 6716 allows 2.5 to 60 ms, and
    10 ms is the shortest that keeps CELT's full quality). Apple's converter takes them: 88–106
    bytes at 96 kb/s, the same 120-frame pre-skip. The worker sends a packet once 10 ms of audio
    is in, so its first sample leaves 10 ms sooner when the capture hands audio over in buffers
    that short. `MAX_CONCEALED` became 6 packets, still 60 ms. The wire carries 100 packets a
    second instead of 50, about 25 kbit/s more in headers.
  - The ring's device term is the device's real buffer. It was a constant 10 ms in the target
    (95th percentile of lateness plus one device buffer), in the depth a cut keeps, and in the
    deadband. It is now the largest render the device has asked for, and the deadband is half
    of it plus half a slice. At 128 frames that takes 7 ms off the target on a jittery link. On
    iOS, which renders 1 024 frames unless its audio session asks for fewer, the target now
    counts the 21.3 ms it really takes: 5 ms more depth than before, and no underrun either way
    on the traces.
  - The floor stays 20 ms. 15 ms was tried because packets now come twice as often. Fed in real
    time on this machine at 128-frame renders, it ran the ring dry one to three times a minute,
    each of which would be a gap in sound. 20 ms never did. A glitch is worse than 5 ms.
  - Together, on the synthetic traces, the listener hears 14 ms sooner on a steady link with a
    drifting clock (45 → 31 ms at the DAC) and 18 ms sooner under keyframe bursts (66 → 48). A
    stall's backlog takes longer to cut, 59 ms at most after the burst against 31, since a
    10 ms packet gives at most 7.5 ms to a cut.
  - Not measured: how much audio ScreenCaptureKit hands over at a time, which needs a
    Screen Recording-signed worker. If it is 1 024 frames, packets leave in twos and threes;
    the trace built that way (`capture_in_1024_frame_chunks_is_covered`) starves nothing and
    holds 16 ms against 27 before.
  - Tests: `device_io_buffer` (ignored, silence on the real output),
    `a_device_rendering_1024_frames_is_not_starved` (now 512 and 1 024, with the target covering
    the render), `capture_in_1024_frame_chunks_is_covered`, `a_packet_lasts_its_frames`, and the
    latency traces at 128-frame renders.

- ✅ **The device term is the largest render of the last few seconds, not of all time**
  (2026-09-29). The ring kept the largest render the device had ever asked for, so one large
  render (an iPhone's screen locking asks 4 096 frames, a Mac's output moving to another device
  asks one large buffer) held 85 ms in the target for the player's life. `RenderSize` now keeps
  the most of two windows of two seconds on the device's own clock, the one filling and the one
  before, as one atomic word the device thread alone writes, so a large render is forgotten
  after two to four seconds and a device that keeps asking that much keeps it. On the synthetic
  trace with one 4 096-frame render among 128-frame ones
  (`one_large_render_does_not_hold_the_depth_up_for_good`), the target goes 20 → 86 → 20 ms and the listener hears 21 ms after it against 94 ms with the
  old mark; one underrun, the large render itself. The existing traces render one size
  throughout, so their numbers are unchanged.

- ✅ **Clipboard v2: lazy on both ends, the focused client's copy mirrored, one worker's copy
  relayed to the next** (2026-09-30, `.research/design-dragdrop-clipboard.md` §3.5 and stage
  S1; supersedes the 200 ms poll and the write-only-on-⌘V rule of **The host announces its
  clipboard only while watched**).
  - **The bug.** Copy in a window of worker A, focus worker B, ⌘V: B pasted its own old
    clipboard. The client wrote A's offer to its pasteboard stamped with the origin marker,
    and `ClipSync::hold` skipped every contents carrying that marker, its own write included,
    so B was never offered anything. Now the client re-offers what it wrote to every other
    worker, origin kept (`Source::Offer { origin: Worker(A), .. }`). A fetch from B is
    answered by fetching from A (`Answer::From(A)`, `slopty_client::clip::relay`), off the main
    thread. Tests: `a_worker_offer_is_relayed_to_another_worker_and_fetched_through_the_first`
    (`slopty_client::clip::local`), `a_copy_on_one_worker_is_relayed_to_the_next_one_focused`
    (headless workspace).
  - **Every type, every item.** A representation is a `ClipType`: one of the six formats, or
    `Apple(String)`, a UTI, which both Apple pasteboards take as they are. An offer lists items,
    each with its representations, so twenty copied files or three pictures keep their shape.
    `transfer::carried` drops what means nothing elsewhere: `dyn.*`, the pre-UTI names, promise
    bookkeeping and our markers. A fetch names one representation by `RepRef` (source, item,
    type). Goldens below.
  - **Lazy reads.** A poll reads the change count, each item's types and the markers. Only text
    and file URLs are read, and only while together they fit 64 KiB inline; past that a read is
    capped (`Board::data_within`, `Pasteboard::item_data_within`): the length is checked before
    the bytes are copied, so a long text is listed with its size and nothing kept. Anything else
    is listed with `size: None` and read when fetched, capped the same way, and only while the
    change count says the same contents before and after the read: otherwise the answer is
    `Unavailable`. A fetch keeps what it read for the offer only up to 8 MiB. A 200 MB copy on
    the worker costs a types list until someone pastes it.
  - **The worker polls every 50 ms while watched** and reads the count alone every 250 ms while
    clients are linked but none watches, which stamps its own copies with the time they were
    seen. A look at an unchanged board takes 1.5–4.5 µs, about 0.01 % of a core at 50 ms
    (`a_look_at_an_unchanged_board_is_one_cheap_call`).
  - **The worker's pasteboard mirrors the focused client's clipboard, latest copy wins.** An
    offer carries `age_ms`, the time since the copy on the sender's clock. The worker places it
    at arrival − age on its own clock and compares that with when its own last copy was seen.
    What the worker's first look after it starts finds was copied at a time nobody knows, so it
    is older than any client's copy; a change found when a client comes back after none was
    linked is placed at when the last one left. (The first version started the count at 0 and
    stamped the board as copied at the first look, so after every restart the client's copy
    lost and ⌘V pasted the worker's old clipboard again.) Offer generations start from the wall
    clock on both ends, since the ids outlive the process and a restart must not reuse one.
    A newer client copy goes on the pasteboard at once as promises (`NSPasteboardItemDataProvider`,
    one per item); an older one waits for a paste. The window ⌘V hold stays as the fence for
    the offer overtaking the chord, and it re-writes the paster's own offer when two clients
    share a worker, unless the worker's own copy is newer. A promise's read on the worker sends
    an urgent `Fetch` to the client and waits at most 5 s (`PROVIDE_WAIT`).
  - **Prefetch under a budget.** After an offer the client fetches in the background, at bulk
    priority, every representation whose size the worker knows that fits
    `min(8 MiB, cwnd ÷ rtt × 250 ms)` (`prefetch_budget`, from quinn's path stats). One of
    unknown size is not fetched ahead: the first version probed those one at a time with
    `Fetch { max }`, which made the worker read each whole, so a 200 MB copy was read on every
    offer. `Fetch { max }` and `TooBig { size }` stay for a relay's cap and for the most a client
    takes (256 MiB); a `TooBig` answering a capped ask leaves alone a paste that has asked
    since for the whole. A fetch a paste waits on is `urgent`, and its bulk stream is raised to
    the tunnels' level (−1), ahead of background uploads.
  - **A wait lasts while bytes move.** A promise on the worker (`PROVIDE_WAIT`, 5 s) and a
    paste on the client (the UI's wait) give up only after that long with nothing arriving:
    each chunk of the representation's bulk stream moves the deadline, so a 60 MB picture over
    100 Mbit/s pastes. A stream longer or shorter than its header said is dropped on both ends,
    and the worker's held paste is told at once rather than after its 3 s.
  - **The worker's promises run on its main run loop.** AppKit asks a promise from a block the
    main run loop runs (`__CFRunLoopDoBlocks` under `park_main`, seen in the stack), and that
    loop also serves the virtual displays and the input sources. A promise waiting on a client
    runs it every 10 ms (`serve_main_run_loop`) instead of stalling them for the wait.
  - **No file names cross.** Neither end writes a representation that names a file:
    `public.file-url` by either spelling, Finder's node, a file promise's bookkeeping, or a
    `public.url` whose scheme is `file`, inline or found when a promise is kept
    (`slopty_platform::pasteboard::names_a_file`). Types that do not travel (`carried`) are
    dropped on the receiving end too, however they were spelled. The first version wrote a
    worker's file URLs on the client's pasteboard: with the same user name, ⌘V in Finder copied
    the client's own file at that path, and a worker could name `~/.ssh/id_ed25519` for a mail
    to attach. Files still paste by moving them (`ClipSync::files`).
  - **The worker's clipboard managers leave a mirror out.** Every write of a client's copy is
    marked `org.nspasteboard.AutoGeneratedType` ("the user had no intention to Copy this
    content", nspasteboard.org), so Maccy or Raycast on the worker do not record it, nor pull
    its promises across after each mirror.
  - **A tap of iOS's paste button loads what is clipboard-sized.** No video or audio (a Photos
    video is gigabytes), an item's pictures once in their first format (the rest are
    conversions made on demand), at most 64 MiB a representation and 128 MiB a tap, each
    checked on its length before it is copied.
  - **Secrets.** Concealed or transient contents are offered with `concealed: true` and types
    only: never inline, never prefetched, never mirrored. A paste chord fetches them. The worker
    writes them whole, marked concealed and transient, and clears them after 60 s
    (`CONCEALED_FOR`) or when that client's clipboard moves on.
  - **The macOS client asks `accessBehavior` before a focus-time read.** When reading the
    general pasteboard would raise the paste alert, focus sends no offer and the read waits for
    a paste (`Pasteboard::reads_ask` on macOS as on iOS). Relaying a worker's own offer needs no
    read, so it still goes on focus.
  - **Writes are for this Mac only.** Both ends write with
    `prepareForNewContentsWithOptions(CurrentHostOnly)`, so Universal Clipboard does not fetch
    every promise at once to hand to the person's other devices.
  - Goldens: `client_clip_offer`, `client_clip_offer_items`, `client_clip_fetch_max`,
    `worker_clip_fetch`, `worker_clip_data`, `worker_clip_too_big`, `worker_clip_unavailable`,
    `uni_bulk_rep`. Tests: the `slopty_worker::clip` unit tests on a fake board
    (`a_big_copy_reads_only_its_types_until_fetched`,
    `a_fetch_after_the_board_moved_is_unavailable`,
    `the_focused_clients_copy_is_mirrored_unless_the_workers_is_newer`,
    `a_concealed_copy_is_written_only_by_a_paste_and_cleared_after`,
    `every_item_and_apple_type_round_trips`, `a_workers_secret_is_offered_without_bytes`,
    `contents_found_at_start_lose_to_any_clients_copy`,
    `a_copy_made_while_no_client_was_linked_is_placed_when_the_last_left`,
    `a_restarted_workers_first_offer_is_a_new_generation`,
    `a_poll_inside_a_write_of_promises_announces_nothing`,
    `a_clients_file_names_never_go_on_the_pasteboard`,
    `a_copy_landing_during_a_fetch_is_not_served_under_the_old_offer`,
    `long_text_is_measured_at_a_poll_and_not_kept`,
    `a_promise_waits_on_while_the_bytes_keep_coming`);
    `slopty_client::clip`'s `prefetch_stays_in_its_budget`,
    `a_secret_is_fetched_only_by_a_paste`, `a_multi_item_copy_keeps_its_items`,
    `a_focus_read_waits_for_a_paste_when_reads_ask`, `a_paste_during_a_capped_fetch_gets_the_whole`,
    `a_paste_waits_on_while_its_bytes_keep_coming`, `a_workers_file_names_never_go_on_this_pasteboard`,
    `a_workers_fetch_reads_no_more_than_its_cap`, `a_relaunched_clients_first_offer_is_a_new_generation`;
    `slopty_platform`'s `a_file_named_any_way_is_not_contents`; `slopty_input`'s
    `a_promise_is_kept_when_read`, `a_capped_read_of_a_big_copy_copies_nothing`; and the worker e2e
    `pbpaste_on_the_worker_sees_the_focused_clients_copy` (a real cross-process promise read
    on a named pasteboard, the worker's board holding a copy from before it started and the
    client's copy half a minute old) and `a_copy_reaches_a_watching_client_within_a_poll`. The
    app e2e (`tests/app/clipboard.rs`, named pasteboards only) drives the app's own half:
    `text_copied_on_one_worker_is_ready_to_paste_in_another_workers_window` (a second worker
    behind a relay, its drawn window focused, and its pasteboard read through the promise that
    fetches via the app from the first worker),
    `a_multi_item_copy_arrives_as_its_items` and
    `the_focused_clients_copy_is_on_the_workers_pasteboard` (copied while a note had the focus,
    mirrored as the shell takes it back). Numbers in `docs/MEASUREMENTS.md`, "Clipboard v2".
  - Not in this stage: files as File Provider placeholders (S2 on), and the exception for a
    copy made in a streamed window just before its client switched tiles, which is still not
    announced to that client.

- ⏸ **Opus loss concealment stays fade-replay until a decoder conceals without allocating**
  (2026-09-30; MEASUREMENTS "Opus concealment: ropus against fade-replay, and repetition").
  Fade-replay itself superseded the same day by **A lost packet is the last pitch period
  repeated**, and the repetition below taken by **Audio datagrams carry earlier packets while
  the client reports loss**; ropus stays deferred on the reasons given here.
  - `ropus` 0.12.18 (a pure-Rust port of libopus, fixed-point) decodes Apple's packets to within
    71–75 dB of Apple's own decode. Its concealment is better by every measure that follows what
    is heard. It adds no click at either edge of a gap, where fade-replay steps 8–15× the
    signal's own step. It keeps the level, -1 dB where fade-replay loses 5, and its spectrum is
    closer. Waveform SNR prefers it on speech (+1.3 to +4 dB) and fade-replay on the synthetic
    music (-0.6 to -1.1 dB).
  - Not adopted, for two reasons:
    - its decoder allocates 16–35 times a packet, and its concealment 25 times;
    - a concealed packet costs 2.1–2.2× Apple's decode.
    Both fail the bar the library audit set: no new allocation a packet, and at most 2× Apple's
    decode. The client's stream task decodes every packet. Adopt it once a decode path that
    allocates nothing exists, upstream or in a fork, with the crate added to `Cargo.lock` then.
  - In-band FEC is not available at this operating point. LBRR is a SILK feature, and at 96
    kbit/s in 10 ms packets every packet is CELT-only, where libopus switches it off.
  - Sending each packet again in the next audio datagram recovers 97–98 % of the losses at
    20–50‰ random loss and 44–46 % in bursts. At 50‰ that is 9.4 → 22.7 dB on speech and 10.0 →
    27.4 on music. It costs about +89 % of the audio datagram (+88–95 kbit/s) and no latency:
    the recovered packet arrives when today's stand-in is queued. It is a wire change, so it
    waits on a ruling; sent only while the receiver reports loss, it would cost nothing on a
    clean link.
  - A cheaper fix to fade-replay itself, unmeasured: continue the waveform into the gap instead
    of restarting the old packet, and crossfade into the packet after it. That removes most of
    the jump at each edge.

- ✅ **A lost packet is the last pitch period repeated** (2026-09-30, `slopty_codec::audio::Conceal`;
  MEASUREMENTS "Opus loss concealment").
  - Replaying the last 10 ms whole lands out of phase with whatever was playing, so a gap was a
    dip with a seam at each end. A tone at 170 Hz, whose period is no whole number of frames,
    came back from the replay at -4.2 dB against the lost packet: worse than silence.
  - Now the client keeps the last 60 ms it played. On a gap it finds the pitch period of their
    end: the lag from 2.5 to 15 ms at which the last 10 ms best match what came before them
    (normalised cross-correlation on a mono downmix at 12 kHz, refined at 48 kHz). It repeats
    that period, several times over when it is under 5 ms, because a short cycle repeated alone
    buzzes. Each wrap of the cycle crossfades into what preceded its start (a quarter of the
    cycle, at most 2.5 ms). This follows ITU-T G.711 Appendix I.
  - The first 10 ms play at full level, and the rest fade to silence by `MAX_CONCEALED` (60 ms).
    A longer gap is a pause, as before, and the history before it is forgotten.
  - The packet after a gap fades in from the stand-in's continuation over 2.5 ms
    (`Conceal::take`). Apple's decoder resumes from a state that never saw the lost packet, so
    that seam is there even when the stand-in is right.
  - Scored on the harness that measured ropus (MEASUREMENTS "Opus concealment: pitch repeat"),
    against a clean decode by the same decoder. On speech the SNR went from 13.6 to 17.6 dB at
    20‰ random loss and from 9.4 to 12.7 at 50‰: level with ropus's own concealment at 20‰ (0.1
    dB short) and ahead of it at 50‰, 100‰ and in bursts. The lost segments went from -0.9 to +5.1 dB (segmental), and the
    level from -4.8 dB to 0.0. Leaving a gap now steps as the clean signal does, and entering one
    steps 3× the clean step where fade-replay stepped 11×. On the synthetic music, waveform SNR
    is 1.2 dB lower, as with ropus's: a continued waveform whose phase drifts scores worse than
    a quieter fade. Its spectrum and level are closer.
  - It costs 22.5 µs a concealed packet (Apple's decode is 27 µs) and allocates nothing after
    its first gap, which meets the bar the library audit set and ropus did not.
  - Not taken yet: Opus's own concealment and in-band FEC (LBRR). Apple's decoder does neither,
    and a pure-Rust decoder (ropus, library audit item 11) is a dependency to weigh on its own.
  - Tests: `a_periodic_sound_is_continued_in_phase`, `the_next_packet_fades_in_from_the_stand_in`,
    `a_long_stand_in_fades_to_silence_by_the_cap`, `nothing_is_concealed_without_history_or_past_the_cap`,
    `a_short_history_is_repeated_as_it_is`. Measurement: `concealment_against_the_clean_decode`
    (ignored).

- ✅ **Audio datagrams carry earlier packets while the client reports loss** (2026-09-30,
  `slopty_media::audio`; MEASUREMENTS "Opus concealment: pitch repeat"). A wire change: the
  audio datagram's payload and two fields of `ReceiverReport`.
  - An audio datagram carries up to two earlier Opus packets, nearest first, each behind its
    2-byte length, and the header's `data_count` is one more than the copies. The client decodes
    the copies that cover the end of a gap, and conceals only what is left. A recovered packet
    goes to the player as a stand-in does: it keeps the gap's time and says nothing of the
    depth, because it arrived with the packet after it, not late.
  - The client's report says how many audio packets arrived in the window and how many the
    sequence skipped (`audio_received`, `audio_lost`), counted where the datagrams are
    reassembled. A skipped packet counts as lost even when a copy brought it back, because
    that is the loss the copies are sized by. Counting only what stayed lost would turn them off
    as soon as they worked.
  - `AudioCopies` on the worker turns those counts into none, one or two copies. A lone packet
    lost now and then stays with concealment. About two in a second (a decaying count with a
    1 s half-life) turn one copy on, and a pair in one report or about five in a second turn on
    two. A copy goes 10 s after the loss that asked for it, one at a time, as parity does.
  - At 20–50‰ random loss one copy brings back 97–98 % of the lost packets, and speech rises
    from 12.7 to 26.5 dB with pitch concealment for the rest. A copy costs about 120 bytes a
    datagram, 95 kbit/s, and only while the client reports loss. In bursts one copy recovers
    44–46 %, which is what the second copy is for.
  - Tests: `the_copies_come_back_nearest_first`, `the_layout_is_fixed`,
    `copies_that_do_not_fit_are_left_off`, `a_malformed_audio_payload_is_refused`,
    `the_copies_follow_the_reported_loss`, `steady_loss_keeps_the_copies` (`slopty-media`),
    `audio_is_counted_for_the_report` (pipeline), the client's
    `audio_waits_for_the_player_without_holding_video_and_counts_gaps`, which ends on a gap the
    copies fill, and the `client_screen_report` golden. The fuzz targets `reassemble` and
    `feedback` carry copies and drive `AudioCopies`.

- ⏸ **Drag and drop lands at the point, both ways: a real drag session on the worker, fed by the
  client's own** (2026-09-30, ruling and plan; the spikes and the helper's roles are built,
  nothing is wired. Builds on `.research/design-dragdrop-clipboard.md` §2–§4 and §8, whose
  clipboard stage shipped as **Clipboard v2**; that study's claims are rechecked below against
  primary sources retrieved 2026-09-30). Each phase below turns ✅ in this entry as it lands,
  with its tests and numbers.
  - **The gap.** A file dropped on a window or display tile is uploaded to
    `~/.slopty/drop/<xfer>/` and put on the worker's pasteboard as file URLs
    (`tile.rs` `drop_files` → `Dest::Staging` → `clip.write_files`). The drop point is thrown
    away: nothing lands in the folder, mail or upload field under the pointer until someone
    presses ⌘V there. Only files, never text, pictures or URLs. Nothing drags out of a streamed
    app at all; drags out exist only for worker paths the client already names (terminal ⌘-drag,
    folder rows, a file tile's grip, `drag::Promise`).
  - **Prior art.**
    - Apple Screen Sharing drags files both ways into and out of the shared screen (support
      article mh14066). How it moves the bytes, and whether High Performance mode keeps it, is
      not documented.
    - Screens 4/5 drop files "over the Connection Window … where you'd like to transfer them"
      (the remote desktop, Finder), on Apple's API since macOS 10.10 (help.edovia.com
      file-transfers). From an iPad the host shows a Drop Target window near the cursor once a
      drag starts, and files land in a fixed Downloads folder, not at a point
      (file-transfers-ipad).
    - Jump Desktop has no file drag on Fluid or Mac to Mac (staff reply, 2024-11-28). Parsec
      documents text alone. Chrome Remote Desktop uploads with a button onto the Desktop
      (`remoting/host/file_transfer/directory_helpers.cc`, `DIR_USER_DESKTOP`).
    - RustDesk accepts drops only in its file manager page. Its macOS paste of remote files is a
      `public.file-url` data provider that makes an empty `/tmp` decoy, then fills Finder's copy
      of it over RPC, found by an FSEvents watch (`libs/clipboard/.../item_data_provider.rs`).
    - Barrier dropped at the point on a Mac: a borderless 3×3 window under the cursor, a HID
      `CGEventPost` mouse-down into it, and a drag begun from that view's `mouseDown:`
      (`OSXDragSimulator.mm`, `OSXDragView.mm`). It noticed drags leaving by reading
      `NSDragPboard` (`OSXPasteboardPeeker.mm`). Deskflow removed all of it as "broken on all
      platforms" (PR #8569, 2025-05). VMware's guest tools catch a drag leaving the guest with a
      window of their own that takes the drop (`DnDUIX11::OnDestCancel`).
    - So only Screens and Apple's own Screen Sharing reach the bar, and no source says how
      either starts the drag on the host. Everything below that rests on host-side behaviour is
      a spike first (P0).
  - **Platform facts that shape it.**
    - `beginDraggingSessionWithItems:event:source:` wants the mouse-down that starts the drag,
      and the drag "begins at the next turn of the run loop". macOS 27 adds
      `beginDraggingSession(items:gesture:source:)`, which needs a gesture recogniser. The
      floor is 26.5, so the event form is the one used.
    - `CGEventPostToPid` puts an event into one app's stream "immediately before any event taps
      instantiated for the specified process" (`CGEvent.h`). Nothing documents it moving the
      window server's drag or the pointer, and the drag manager picks the target under the real
      pointer. So an injected drag goes through the HID tap and moves the worker's real
      pointer, as a person's would. Posting needs the Post Event grant the worker already holds.
    - macOS 27 numbers clicks: a press, its drags and its release must share one
      `kCGMouseEventNumber` (`CGEventTypes.h`; Deskflow #9978 and #9991). The injector sets none
      today (`crates/slopty-input/src/injector.rs`), which may already break title-bar drags on 27.
    - A drop target sees promise items only if it registered
      `NSFilePromiseReceiver.readableDraggedTypes`, and many views take only file URLs.
      `receivePromisedFilesAtDestination:` terminates the app when called outside
      prepare, perform or conclude on macOS 27 (release notes). So files dropped in are real
      files on the worker before the target reads them. Promised files leaving a remote app
      can be taken only by a window of ours that the remote drag drops onto.
    - The drag pasteboard (`NSPasteboardNameDrag`) is one of the "other pasteboards" that
      "default to always allow access" (`NSPasteboard.AccessBehavior`). Reading it raises no
      prompt, and an event tap is not needed to see a drag begin.
    - A drag source cannot see the target's answer while dragging: `NSDraggingSource` hears
      moves and the end, and `NSDraggingSession` has no current operation and no cancel.
      A destination answers `draggingUpdated:` synchronously, and a UIKit drop answers
      `sessionDidUpdate:` synchronously, so the badge on the client is the worker's last report,
      one round trip behind the pointer at worst.
    - The worker is a LaunchAgent in the Aqua session (TN2083: a GUI agent "has access to all
      GUI services") running a bare `CFRunLoop` with no `NSApplication`. Its main run loop also
      serves the virtual displays and the pasteboard promises.
  - **Drop in (client → a point in a streamed window or display).**
    - *Client, macOS.* The remote tile's drop destination grows from `slopty_platform::file_drop`'s
      view into `RemoteDrop`. It registers for file URLs, promises, text, RTF, HTML, images and
      URLs, and replaces GPUI's filename-only handling over remote tiles. That takes a gpui-fast
      hook: GPUI's destination yields over a rect a platform view claims; check upstream's open
      pull requests first. `draggingEntered:` reads the drag pasteboard into `ClipEntry`s: each
      file with its size, mode and whether it is a folder; promised types; data under
      `INLINE_CLIP_BYTES` inline, anything larger listed with its size. It sends
      `DragInput::Enter` and starts the upload. `draggingUpdated:` sends `Move` (newest wins,
      and a move to the point already sent is not sent again) and answers the worker's last
      operation. While a drag rests, the worker nudges it two points out and back after each
      rest of the spring delay and 300 ms (`slopty_dnd::nudge`): an injected drag resting still
      does not spring a spring-loaded target (P0 (8b)), nor does one nudged more often (the
      roles, below).
      `draggingExited:` sends `Leave` and cancels the upload. `performDragOperation:` answers NO
      when the last report was none, which slides the image back as a local refusal does.
      Otherwise it calls promises in (the one moment macOS 27 allows), sends `Drop`, and shows a
      progress ring at the point until the worker says the drop ended. While a drag is over a
      tile the client stops drawing the worker's cursor there, since the system's drag cursor
      and badge are the pointer.
    - *Transfer first.* The files upload from `Enter` into the drag's own landing
      (`Dest::Drag(id)`, `~/.slopty/drop/<drag>/`) at bulk priority, with the existing
      `.partial`, BLAKE3, resume and cancel. A typical hover of 0.5–2 s finishes small files
      before the drop. Promised files join the upload once called in. `Leave` cancels and
      removes the partials. A folder keeps its tree.
    - *Worker.* A `DragIn` state machine per drag in `apps/slopty-worker/src/conn.rs`, beside the
      paste `Held`.
      1. On `Enter` it raises a window stream's window, since the HID route hits whatever is
         topmost. It asks the drag helper (below) for a source window at the mapped global point:
         a few points, borderless, just above normal windows, answering `acceptsFirstMouse:`
         with YES (on 26.6 its first press reaches the view either way, P0 (1b); YES keeps it
         so if that changes).
      2. The injector enters drag mode. For the drag's life it takes the HID route and gives the
         press and every drag and release one event number. It posts the press and a drag
         2 px away. The helper's view then gets a real `mouseDown:`/`mouseDragged:` and begins
         the session with that event. At once the source window stops taking the mouse,
         `animatesToStartingPositionsOnCancelOrFail` goes NO, and the source mask outside the
         app is Copy, so nothing on the client is ever moved away.
      3. The items are one `NSDraggingItem` per client item: each file's final landing path as
         `public.file-url`; data as an `NSPasteboardItem` whose provider fetches lazily
         (`ClipMsg::Fetch` with `Source::Drag`); a promised file as a `public.file-url` provider
         answered from a table filled before the drop. Each item's image is transparent, since
         the client's own drag image is the one the person follows, and a second one in the
         picture would trail it by a round trip.
      4. `Move` posts a drag at the mapped point, coalesced like moves today.
      5. For the operation, the worker classifies `NSCursor.currentSystemCursor`, which it
         already reads for the stream (`slopty-capture/src/cursor.rs`), against the copy, link
         and not-allowed cursors by image digest. A change goes out as `DragEvent::Operation`.
         With a Copy-only mask, the copy cursor is an accept and the arrow a refusal.
      6. `Drop` waits until every file is whole, then posts the release at the point. The
         helper's `endedAtPoint:operation:` gives the final answer, which goes out as
         `DragEvent::Ended { op, error }`. The injector returns to the pid route, and on a window
         stream the real pointer goes back where it was.
      7. `Leave` posts Escape then the release through the HID route. If P0 finds that ends the
         drag badly, the fallback is VMware's: the source window returns under the pointer,
         takes the release, and answers none.
    - *One drag per worker.* A worker has one pointer, so one drag at a time crosses it,
      whichever client it comes from. A second client's `Enter` meanwhile is answered
      `Operation { op: None }` and its tile says the worker is busy with another drag. A client
      that leaves mid-drag counts as its `Leave`.
    - *A target that refuses.* While hovering, the badge shows it and the local drop slides back
      with nothing sent. A target that accepts on hover and refuses at the drop ends with
      `op: None`: the client says the target refused the files, and the landing is deleted by
      the existing sweep. A drop held too long on a transfer that fails says which file failed,
      and the drop is cancelled on the worker.
  - **Drag out (a streamed app → the client).**
    - *Detect.* The worker knows when this client's left button is held on a stream and has
      moved more than 3 pt, since it injects both. It notes the drag pasteboard's change count at
      the press and reads it every 8 ms from the first move to the release. Apple does not
      document that a drag bumps it, but Barrier and shipping shelf apps rely on it (P0 proves it
      here). A change means an app began a drag. The worker reads the items' types, file URLs
      (stat for size, mode, folder), promised types and small text, and sends
      `DragEvent::OutBegan { drag, items }`.
    - *Follow.* A drag cannot begin under pid-posted events at all (P0 (4)): they reach the app's
      queue with no window, so no view sees the press, and carrying it on through the HID tap
      starts nothing. So a left press on a window stream goes through the HID tap from the start,
      with the window raised and unobscured at the point, and the worker watches the drag
      pasteboard from that press. Moves
      keep going to the worker while the pointer stays on the tile, so the remote app's own hover
      feedback shows in the picture. A single-window stream does not capture the worker's drag
      image (P0 (7): 0 of its pixels in the window's picture, 2113 in the display's), so while in
      the tile the client draws the items' icons at its own
      pointer, at its own frame rate.
    - *Hand over.* When the pointer leaves the tile with the button still held, the client begins
      a local drag from that mouse-dragged event, the path `drag::drag_out` already uses. Each
      file is an `NSFilePromiseProvider` whose write fetches from the worker on its own
      operation queue (`XferMsg::Fetch` by path, resumable). Each data item is an
      `NSPasteboardItem` whose provider fetches urgently. It sends `DragInput::Catch`. The worker
      moves the remote drag onto the helper's catcher: a transparent window at the pointer,
      registered for the item types and promises, answering Copy. It posts two drags over it (so
      the drag manager re-targets) and the release under the press's event number. The catcher's
      `performDragOperation:` takes file URLs as references (the files stay put), calls promises
      into `~/.slopty/drag/<drag>/` (the only moment macOS 27 allows), and keeps data up to
      `MAX_REP_BYTES`. It answers `OutCaught` with the final names and sizes. The source app
      sees an ordinary copy drop, so nothing slides back on the worker and nothing is moved
      there.
    - *Land.* Bytes move only when the local target keeps its promise, with progress
      (`NSProgress` published with the file URL) in Finder and in the tile's transfer pill.
      Coming back onto a tile of the same worker turns into a drop in whose items are the
      worker's own paths, and no bytes move. A local cancel after the catch leaves the caught
      promise files to the sweep.
  - **The drag helper.** The worker re-executes itself as `slopty-worker dnd`: an accessory
    `NSApplication` (never `prohibited`, which has hung on pasteboard access, FB17775671)
    spawned on the first drag, kept while any client streams, and restarted if it dies. It is
    the same signed executable, so it needs no second TCC grant, no `xtask sign` entry and no
    bundle. It posts no events itself: the worker's injector owns the route, the event numbers
    and the button state. The daemon keeps AppKit windows and drag sessions out of the process
    that serves streams and the virtual displays, so a hang in AppKit's drag code cannot stall
    video or promises. It talks to the worker over a Unix socket in the worker's runtime
    directory, with `slopty_proto::dnd` messages carrying insta goldens: `SourceAt`,
    `CatcherAt`, `Stop`, `Data` down; `SourceReady`, `SessionBegan`, `SessionEnded`, `NeedData`
    and `Caught` up. Its code lives in a new `crates/slopty-dnd`, so the worker crate stays
    AppKit-light.
  - **iPad and iPhone.**
    - Drop in: the existing `UIDropInteraction` answers `sessionDidUpdate:` with a
      `UIDropProposal` of the worker's last operation and sends the same `DragInput` moves.
      UIKit asks again only when the finger moves, so a late report shows on the next move.
      Data may be loaded only in `performDrop` (`UIDropSession.loadObjects`), so the upload
      starts at the drop and the worker holds its release until the files are whole. The ring
      shows meanwhile. Each file representation is copied inside its completion handler, since
      the system deletes it when that returns (`file_drop::arrive` already does this).
    - Drag out: UIKit starts a drag only from its own lift, and no public call starts one from
      code. When a worker drag reaches the tile's edge, the worker catches it as on the Mac, and
      the tile shows a chip at that edge naming what it holds. The chip is a
      `UIDragInteraction` source with lazily registered file representations
      (`file_drop::out`), lifted with a second touch. That is Screens' drop target, moved to the
      client where the person's hand is. Cross-app drags work on iPhone since iOS 15.
  - **Feel.**
    - The drag image the person follows is always the local system's, at the display's rate.
      Only the hover feedback (highlights, insertion carets, springing folders) comes from the
      worker, through the stream.
    - Moves ride the stream's numbered input path, newest wins, like pointer moves. Their cost is
      the input path's, not a new one.
    - Spring-loaded folders open because the remote pointer rests where the local one rests and
      the worker nudges a resting drag two points out and back every 800 ms (the spring delay and
      300 ms), which springs a target about 1.5 s after the pointer comes to rest, where resting
      still does not (P0 (8b)) and nudges 600 ms apart or less never do. Holding a drop over a
      folder while a big file finishes may spring it open, and the drop then lands in the folder
      that opened, the same place.
    - Targets, measured before anything is tuned: from `Enter`/`Move` to `Operation` at the
      client, p95 ≤ RTT + 25 ms; from a drop with its files whole to the target's
      `performDragOperation:`, p95 ≤ RTT/2 + 10 ms; video and input p95 unchanged during a 1 GB
      drop. They go to `docs/MEASUREMENTS.md` with their commands.
  - **Wire (`slopty-proto`, goldens for every variant; a wire change, nothing versioned).**
    - `drag.rs`: `DragId` (16 random bytes, like `XferId`), `DragOp { None, Copy, Link, Move }`
      and `DragOps` flags.
    - `DragInput` rides `ScreenInput::Drag`, so it is numbered with the stream's input.
      `Move` is out of order, like `ScreenInput::Move`; the rest are in order. The variants are
      `Enter { drag, x, y, allowed, items: Vec<ClipEntry> }`, `Move { drag, x, y }`,
      `Leave { drag }`, `Drop { drag, x, y }` and `Catch { drag }`.
    - `DragEvent` rides `ScreenEvent::Drag { stream, event }`, with the variants
      `Operation { drag, op }`, `Ended { drag, op, error }`, `OutBegan { drag, items }`,
      `OutCaught { drag, items }` and `OutFailed { drag, error }`.
    - In `transfer.rs`, `Source` gains `Drag(DragId)`, so `RepRef` fetches a drag's data the way
      it fetches an offer's, and `Dest` gains `Drag(DragId)`: the drag's landing, with nothing
      put on the pasteboard. `ClipEntry` gains `file: Option<FileMeta>` (name, size, folder,
      mode, mtime). Files out are fetched by path with the existing `XferMsg::Fetch`. The study's
      `FileRef` and `XferMsg::Pull` wait for the File Provider domains.
    - Goldens: `client_screen_drag_enter`, `_move`, `_leave`, `_drop`, `_catch`;
      `worker_screen_drag_operation`, `_ended`, `_out_began`, `_out_caught`, `_out_failed`;
      `client_xfer_begin_drag`; `dnd_*` for the helper socket.
  - **Tests, and what they may touch.** No test posts an event to any process but its own
    children, none reads a screen, and each passes or fails on digests, operations and timestamps
    its own processes report.
    - *In the gate, on this Mac.*
      - Goldens.
      - The worker's `DragIn` and drag-out state machines on the injector's `Recorder` backend
        and a fake helper: `enter_raises_then_presses_at_the_source_and_drags`,
        `a_drag_shares_one_event_number`, `moves_coalesce_and_a_still_hover_posts_nothing`,
        `the_release_waits_for_every_file`, `leave_cancels_the_upload_and_the_session`,
        `the_route_goes_back_to_the_pid_after_the_drag`, `cursor_shapes_map_to_operations`,
        `a_drag_pasteboard_change_while_held_is_a_drag_out`,
        `nothing_is_watched_without_a_held_button`, `catch_wiggles_over_the_catcher_and_lets_go`.
      - `slopty-dnd` on named pasteboards: its items read back through the client's reader as
        the same `ClipEntry`s (`every_drag_item_reads_back_as_it_was_sent`). The catcher's
        receive path, `catcher::take`, reads a named pasteboard as it reads the drag one
        (`the_catcher_takes_urls_as_references_and_data_whole`), and the drag watch reports a
        change once (`a_change_is_reported_once_with_what_the_items_hold`).
      - Headless GPUI (`slopty-ui`): `the_badge_is_the_workers_last_answer`,
        `a_refused_drop_slides_back_and_sends_nothing`, `a_held_drop_rings_and_cancels`,
        `leaving_the_tile_during_a_worker_drag_hands_it_over`,
        `a_drag_back_onto_the_same_worker_names_its_own_paths`.
      - App e2e against a real worker whose injector and helper record rather than post (a
        `--dnd-record` seam beside `--pasteboard`): `DropFiles` grows into a drag sequence
        (enter, moves, drop), and the test asserts that the worker's landing holds the files
        with matching digests before the recorded release, that a scripted refusal comes back
        as the badge, and that a cancel leaves no partials.
    - *Live, opt-in (`#[ignore = "live"]`, `cargo xtask e2e dnd`, never in `cargo gate`).* A real
      HID drag from the helper onto the test's own child: `slopty-drop-target`, an accessory
      AppKit app on the wire lane's `slopty-gesture-app` pattern. It is registered for file
      URLs, promises and text, answers a scripted operation, and prints what it received and
      when. For drag out, `slopty-drag-source` begins a drag of its own files and promises.
      Unlike the gesture app's window, these windows must be on a screen, since the drag manager
      hit-tests at the pointer. HID moves the one real pointer, so these run only in a macOS
      guest under tart (`cargo xtask vm live`, testing.md "Live tests that drive the desktop run
      in a macOS guest under tart"), and skip unless `SLOPTY_DND_E2E` and `SLOPTY_VM` are set,
      which only that lane does. They never run on this Mac nor on the other one.
    - One harness pattern: the drop target and drag source reuse the wire lane's child-app shape
      (ready line with pid and window, one stdout line per callback, gone when stdin closes).
      That lane's gesture harness is agreed through the coordinating session before either
      child is written.
  - **Phases, each turning ✅ here as it lands.**
    - ✅ *P0 — spikes* (2026-09-30, macOS 26.6.2 in a tart guest, 12 live tests in
      `crates/slopty-dnd/tests/spikes.rs`, `cargo xtask vm live -p slopty-dnd --test spikes`,
      62 s of tests). Every drag starts in the test's own `slopty-drag-source` and ends on its
      own `slopty-drop-target` (`tests/support/`, the wire lane's child-app shape); nothing else
      is posted to. **The design stands.**
      - (1) ✅ An HID press into an accessory app's pop-up-level window, the app never active,
        begins a session there, and the release over the drop target delivers the file's
        `file://` URL at the point (`perform … files=file:///…`, `file … exists=1`); the source
        ends with `op=1`, a copy. (1b) With `acceptsFirstMouse:` answering NO the press still
        reaches the view and begins the drag.
      - (2) ✅ The system cursor, read as the worker reads it (`slopty_capture::cursor_shape`)
        and classed against AppKit's cursors by differing pixels, is the copy cursor (`diff=0`)
        over a target answering copy and the arrow (`diff=0`), not the not-allowed cursor, over
        one answering none.
      - (3) ✅ Escape posted through the HID tap mid-drag ends the session before the release,
        with `op=0`, and the release drops nothing.
      - (4) ✅ as a finding against the pid route: pid-posted presses and drags reach the app's
        queue under one press number but carry no window (their `locationInWindow` is screen
        points), so no view sees them and no session begins; the real pointer does not move,
        and carrying the press on through the HID tap starts nothing. The same with the
        injector's window route (the owner activated), with raw posts naming the window in
        fields 91 and 92, and with a titled window that can become key (4a–4c). The children
        are accessory apps; whether a regular app's key window takes window-less events is
        left to the input lane, since it bears on every window-stream click.
      - (5) ✅ The drag pasteboard's change count stays put on a click (4 → 4) and moves when a
        drag begins (→ 5); its item then names the file (`public.file-url` and five legacy
        spellings). Read from another process with no prompt.
      - (6) ✅ On 26.6 a drag begins and drops under each numbering: the injector's per-press
        number (1 on every event of the press), 0 on every event, and the field left to
        CoreGraphics (read back as 0). So the numbering is not what 26 needs; whether 27 does
        (Deskflow #9991) waits for a 27 guest.
      - (7) ✅ A window capture of the target under the drag holds none of the drag image's
        magenta (0 pixels of 280×200), the display capture 2113 of 1024×768.
      - (8a) ✅ A destination that reads the drag pasteboard half a second after the drop finds
        the same file URL. (8b) ✅ With spring loading on (`com.apple.springing.enabled` 1, delay
        0.5 s), a drag resting still over a spring-loaded target for 2.5 s gets
        `springLoadingEntered:` and the highlight but never activates; moves a point either side
        every 100 ms after that rest activate it about 0.8 s later. (The same moves from the
        start of the rest never do; the helper's roles below measured why.) A SwiftUI
        destination cannot be built in Rust, so SwiftUI's read under injected input stays
        open; (8a) covers the late read an item provider makes.
      - Still to run on macOS 27 (no 27 guest: its base would put the disk under the prune
        floor): all of them, (1), (4) and (6) first, since 27 numbers clicks and changed
        promise reception.
    - ✅ *The helper's roles, the platform half of P2 and P3* (2026-09-30, macOS 26.6.2 in a
      tart guest, 5 live tests in `crates/slopty-dnd/tests/roles.rs`,
      `cargo xtask vm live -p slopty-dnd --test roles`, 57 s). They are built as the library the
      helper process will run, and not wired yet. The worker's `DragIn`, its watch loop and the
      helper socket come after P1. Numbers are in MEASUREMENTS, "the drag helper's roles".
      - `slopty_dnd::source`: a few points of window at a point. A press into it begins a real
        session with that press, carrying `items::Writers`. A whole file goes as its URL, and
        anything else (data, a file still arriving) goes as an `NSPasteboardItemDataProvider`
        promise, answered through a `Provide` callback when a target reads it. The image is
        clear, only Copy is allowed, a failed drag does not slide back, and once the session
        begins the window lets the mouse through. Live, a whole file, promised text and a file
        written 300 ms into the target's read all land at the point.
        - A target blocks in its read for as long as a provider takes: 5 s, then it takes the
          file whole and the drag ends as a copy.
        - So a drop is held for an upload only briefly. Past that, P5's placeholders are the
          way.
      - `slopty_dnd::catcher`: a 64-point window registered for files, URLs, text, pictures, PDF
        and file promises, answering Copy. `take` keeps file URLs as references. Each file
        promise is called in inside `performDragOperation:` on an `NSOperationQueue`. Other
        representations are kept whole up to a cap and listed past it, and the promise
        bookkeeping and legacy spellings are dropped. A drag out of a test app, let go over it,
        gives the file where it was and the promised file whole in the drag's folder, and the
        app sees a copy.
      - `slopty_dnd::watch`: the drag pasteboard's count moved by the first move after the
        press (3 points, 8 ms). Its items name the file and the promise's content type. A file
        URL's item also offers `com.apple.pasteboard.promised-file-url` and is not taken for a
        promise.
      - `slopty_dnd::nudge`: a one-point move never reaches the target. Nudges 600 ms apart or
        less never spring it, and 650–700 ms springs it only on some runs. Two points out and
        back after each rest of the spring delay (read from `com.apple.springing.delay`) and
        300 ms springs it every time, about 1.5 s after the pointer rests.
        `which_nudge_periods_spring_a_target` keeps the table.
    - *P1 — wire.* `slopty-proto` alone, landing before anything that uses it, after the wire
      lane's change to `ScreenInput`.
    - *P2 — drop in on macOS.* The worker's `DragIn`, the injector's drag mode, the helper's
      source role, `Dest::Drag`; the client's `RemoteDrop`, the gpui-fast hook, the badge, the
      ring, the landing.
    - *P3 — drag out on macOS.* Drag-pasteboard watch, HID follow, catcher,
      `OutBegan`/`OutCaught`, the client's hand-over and its in-tile icons.
    - *P4 — iPad and iPhone.* The drop proposal, a drop held for its upload, the edge chip.
    - *P5 — lazy files.* The ⏸ File Provider domains (**Worker files paste into Finder through
      a File Provider domain**) on both ends: a dropped file exists at once as a placeholder
      and the release no longer waits for its bytes; drag out and pasted worker files reach
      Finder without a promise.
  - **Rejected.** Drop by AppleScript, Accessibility or `open -a` (a few apps, no hover, per-app
    code). Promises alone as the worker's source (URL-only targets refuse them, and Finder takes
    the URL when both are offered). An empty decoy file filled after the drop (RustDesk; the
    reader races it, as the File Provider entry found). Escape to end a drag out (the source sees
    a cancel and never keeps its promises). A listen-only event tap to see drags start (it needs
    Input Monitoring and still does not say a drag began). AppKit windows in the worker process
    itself (a hung drag would stall the streams on the main run loop). A second signed helper
    binary (a second grant and signing entry for nothing).
  - **Risks.** Injected drags may differ from a person's, which P0 settles. The badge rests on
    the system cursor; without it, the client shows Copy while hovering and the end decides.
    During a drag the worker's real pointer moves and a window stream's window is raised, which
    someone at the worker sees. Files from `~/Desktop`, `~/Documents` or `~/Downloads` may
    raise the Files and Folders prompt for the worker, so the doctor reports that grant. How long
    Finder waits on a slow promise and what a slow data provider does to the target app are
    undocumented, and the live lane measures a 60 s promise and a 5 s provider before big data
    relies on either. A target that checks the file while hovering finds nothing at its path
    until the upload is whole, and may refuse; P5's placeholders end that.

- ⏸ **One sound per worker on a client, not one per stream** (2026-09-30, ruling only; nothing
  built yet. Measured in MEASUREMENTS, "streams from one worker: what they could share").
  - *What each stream owns today.* Every stream's capture sets `capturesAudio` (`audio: true` in
    `crates/slopty-worker/src/screen.rs`), so each holds its own ScreenCaptureKit audio tap, and
    each stream opens its own Opus encoder. The client opens an Opus decoder and a CoreAudio
    `Player` per stream, and mutes per stream. ScreenCaptureKit filters audio by application, not
    by window: a single-window filter captures all of the owning app's audio, even from windows
    not in the picture (Apple, WWDC22 session 10155, "Take ScreenCaptureKit to the next level"),
    and a display filter every app's. So two tiles of one app, or a desktop tile and any window
    tile, carry the same sound, and the client plays it once per tile through players whose
    buffers drift apart: the same sound doubled and out of phase.
  - *What it costs.* A stream's sound is about 0.016 of a core and 0.041 W of CPU across the
    worker's encode and the client's decode (118 µs and 37 µs a 10 ms packet), 0.126 cores and
    0.33 W at eight streams, before the player and the tap. That is more than the stream's video
    decode (0.007 of a core). Nothing else the streams own is worth sharing: the client's decoders
    cost 0.007 of a core each and keep 60 pictures a second up to eight 1080p (p95 2.6 ms) or eight
    4K streams (p95 32 ms, the decode engine full); the encoder sessions share the two engines
    already, through `placement_fps` and the focus give-way; and one display capture cropped per
    window cannot replace window captures, which must stream a window others cover.
  - *The ruling.* A client hears one sound from a worker. The worker captures it once for that
    client: an audio-only capture whose filter is the union of the applications behind the
    client's streams (every app when one of them is a display), changed with
    `updateContentFilter:` as streams open and close. The video captures stop capturing audio.
    One Opus encoder per client, one decoder and one player per worker on the client, and the mute
    is the worker's sound, not a tile's. Streams of different apps still sound together, as they
    do now; what goes is the duplicate.
  - *Wire.* The sound gets a lane of its own on the worker connection instead of riding a
    stream's datagrams: open and close with the first and last stream that has sound, its packets
    tagged by that lane. (Riding the oldest open stream as a carrier would need no wire change but
    a hand-over, with a gap and a new decoder, whenever that tile closes, and pre-release a format
    is replaced cleanly, not worked around.) Goldens for the lane's open, close and packet.
  - *Owners, in order.* `slopty-proto` (the lane, goldens); then `slopty-capture` (an audio-only
    capture with an application set it can change), `slopty-worker`'s `screen.rs` (video
    captures without audio, one audio capture and encoder per client, the set kept current), and
    `slopty-client` with `slopty-ui` (one player per worker, the mute on the worker's sound) side
    by side.
  - *Tests.* Worker units on a fake capture: two window streams of one app open one audio capture
    and one encoder; a display and a window stream, one capture over every app; windows of two
    apps, one capture over both, which drops an app when its last window closes. Client: one
    player for a worker's three tiles, and the mute silences all three. Live, in the macOS guest
    (`cargo xtask vm live`): two window streams of the test's own app playing a tone arrive as one
    sound, counted in packets per second at the client. Measurement: `concurrent_audio` at N
    streams against one lane.
