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
  (2026-09-05, protocol 4). `NSPasteboard` has no change notification, only `changeCount`,
  so hostd reads it every 200 ms (one Mach call to the pasteboard server); a client-side
  watcher was rejected because GPUI's clipboard API has no count and the client would have to
  push on every poll. The client instead pushes exactly when it matters: the paste chord
  goes to the host on the same ordered stream right after `ScreenRequest::Clipboard`, so the
  host's paste already sees the text. Only clients with a window open receive the host's
  clipboard (the connection filters the broadcast) and the client never writes text it
  already holds, which is what stops the loopback echo when app and host share a Mac.
  Text only; files and images stay local. `cargo xtask bundle` builds `Slopty.app` with the
  daemons and CLI beside the app (`Contents/MacOS/slopty host install` works unchanged since
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
  self-test (`SLOPTY_E2E_DROP_PERMILLE`) exercises the path. Still ⏸: a jitter estimator
  with an adaptive prefill (the ring starts playing at once and holds at most 200 ms; no
  measurement has shown late packets as a problem on the links tried). No `Quality` field for
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
  protocol 46). A screenshot taken on the client and pasted into a remote window is the
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

- ✅ **Playback holds about 40 ms, and a stall's burst is trimmed rather than kept** (2026-09-24).
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
