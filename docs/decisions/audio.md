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
  `slopty_codec::audio::Conceal`). Apple's Opus decoder takes no empty packet for its own
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
  above on the worker side).
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
