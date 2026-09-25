# Decisions — Audio

See `docs/DECISIONS.md` for the legend. Newest entries go at the end.

- ✅ Opus via Apple's `AudioConverter` (`kAudioFormatOpus`), no libopus: the encoder exists on
  macOS 26.5 (verified 2026-09-05: `encodes_and_decodes_a_tone` round-trips a 440 Hz tone;
  the first packet comes back 120 frames short, Opus pre-skip) and the decoder on both
  platforms. One fixed configuration, 48 kHz stereo float interleaved, 960-frame packets at
  96 kb/s, so nothing about the format travels on the wire. Playback is an `AudioQueue` with
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
  `MAX_CONCEALED` = 3 packets (60 ms), pushes it again faded linearly to silence across the
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
  the fixed 40 ms and the hard trim superseded 2026-09-25 by "Playback is a jitter buffer").
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
