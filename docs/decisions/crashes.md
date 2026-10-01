# Decisions — Crash reports

See `docs/DECISIONS.md` for the legend. Newest entries go at the end.

- ✅ **Crash reports come from a panic hook, a signal handler and macOS's own `.ips` files. No
  minidumps** (2026-09-30). Every binary calls `slopty_crash::install` as the first line of
  `main`, and the iOS app calls it first thing in its `main` too. Reports land in
  `<data dir>/crashes` (0700, files 0600), and each process keeps its newest 20. Nothing leaves
  the machine.
  - **Panics.** The hook writes `<ms>-<pid>-<process>.json` with the message, the location, the
    thread and the frames, then runs the previous hook, so stderr reads as before. It resolves the
    frames on the spot with the `backtrace` crate, the symbolizer std uses. A panic that cannot
    unwind makes core panic a second time ("panic in a function that cannot unwind"). That second
    panic marks the first report `aborted` instead of filing one of its own.
  - **Fatal signals.** `SIGSEGV`, `SIGBUS`, `SIGILL`, `SIGFPE`, `SIGABRT` and `SIGTRAP` go to a
    handler on the alternate stack Rust gives each thread. The handler only makes
    async-signal-safe calls. It writes a text `.native` record from a stack buffer: the signal,
    `si_addr`, the thread's name, pc and lr, and the return addresses up the frame-pointer chain,
    which the Apple arm64 ABI keeps. Then it hands the signal on to the previous handler (Rust's
    stack-overflow check) or restores the default and raises it again. The process still dies of
    the signal, and ReportCrash still writes its `.ips`. The first thread to crash writes the
    record. Another thread crashing at the same moment waits for it (a bounded spin).
  - **The next run resolves the record.** The handler cannot symbolize, and the same binary
    later can do it for free. The record names the executable by its Mach-O UUID and load
    address. A later process of the same build (same UUID) moves each address to its own load
    address and resolves it with `backtrace::resolve`. Addresses in system libraries resolve
    through `dladdr` at the very same address, because the dyld shared cache stays where it is
    until the Mac restarts. `getpid`'s address, kept in the record, tells whether it still sits
    there. The resolving happens on a utility-QoS thread at `install` when a record of that
    process is waiting, and in `slopty_crash::reports`. A process first claims a record by
    renaming it to `<record>.<pid>`, so of two processes of one build only one resolves it, and
    it writes each report through a partial file of its own (`create_new`) before renaming it
    into place. A claim or a partial left by a dead process is handed back or deleted. Once
    records are resolved, and after each panic report, `backtrace::clear_symbol_cache` drops the
    debug info, so a long-lived app does not hold it.
  - **An abort folds into a panic only when the panic caused it.** When core panics a second
    time about a panic that could not unwind, the hook stores that thread's system id in a
    static. The abort follows as soon as the hook returns. The signal handler reads the static
    and writes `after_panic 1` when the aborting thread is that one. Only such a record marks the
    latest panic report of its pid `aborted` instead of becoming a report. A caught panic (a
    tokio task, `catch_unwind`) never sets it, so an abort hours later keeps its own report and
    frames.
  - **Whole function paths from the symbol table.** The workspace builds with
    `debug = "line-tables-only"`, whose debug info names a function without its module (`fire`,
    `{closure#0}`). For each address, the outermost frame takes its name from the executable's
    own `LC_SYMTAB`, read in place in `__LINKEDIT` and demangled (`slopty_crash::probe::fire`).
    Inlined frames keep the short names, with their file and line. A shipped build's report,
    resolved against its dSYM, has whole paths on inlined frames too (below, "Shipped builds
    have limited debug info").
  - **macOS's reports.** `reports` also parses `~/Library/Logs/DiagnosticReports/<process>-*.ips`
    of Slopty's processes (`bug_type` 309). Native crashes inside VideoToolbox, AppKit or the
    Objective-C runtime show up there with every thread. An `.ips` with the same pid and process
    as one of our reports, captured within a minute of it, becomes a link on it; a pid that came
    round again later stays a report of its own. A file over 4 MiB is skipped (a crash report
    with every thread of a busy app is a few hundred kilobytes). Frames come from
    `lastExceptionBacktrace` when an uncaught Objective-C exception ended the process, else from
    the faulting thread. ReportCrash leaves v0-mangled Rust names, so `rustc-demangle` reads them.
  - **Rejected: minidumps** (`crash-handler`, `minidumper` and `minidump-writer` from
    EmbarkStudios and rust-minidump, maintained, crash-handler 0.8.1 on 2026-09-25). They capture
    out of process, which needs a server process beside every daemon and the app. Reading them
    needs `dump_syms` symbol files and a stackwalker, which is a crash-server pipeline Slopty
    does not have and does not want. ReportCrash already captures every native crash out of
    process, for free, with all threads, registers and images. iOS gives an app no way to
    capture another task. The minidump route would add weight to every binary and duplicate what
    macOS writes anyway.
  - **Rejected: resolving records with `addr2line`'s `Loader` from any process.** It would add
    a second gimli and object (addr2line 0.27 wants gimli 0.34 and object 0.40, and `backtrace`
    pins 0.32 and 0.37). It still could not name system frames, because those libraries exist
    only in the shared cache and not on disk. The next run of the same build does both with
    what is already linked.
  - **Why a handler as well as `.ips`.** ReportCrash files its report seconds after the death.
    Our record is there before the process is gone, resolved to our file and line, and tests can
    rely on it. iOS keeps its crash logs where the app cannot read them, so there the record is
    the only report until MetricKit delivers one.
  - **The processes.** `Process` names each binary the way its executable is named
    (`slopty-app`, `slopty`, `slopty-worker`, `slopty-server`, `slopty-ptyd`, and `Slopty` on
    iOS), which is also how ReportCrash names its files. The data directory is
    `slopty_platform::dirs::data_dir()`, read before any argument parsing, so it is the same for
    every process on the machine whatever `--data-dir` says. `slopty-ptyd` does not link
    `slopty-platform` and installs only when `SLOPTY_DATA_DIR` is set. Its `LaunchAgent` always
    sets it, and the worker's tests that start a ptyd without it do not file reports in the
    user's directory.
  - **Crashing on purpose.** `SLOPTY_CRASH_TEST=panic|abort|segv` makes `install` crash its
    process, which reaches every binary through the one line that installs the reporter.
    `slopty_crash::probe::run` starts a binary that way with its own data directory and reads
    what it left.
  - **Cost.** `install` takes 48 µs at the median in release and 62 µs in dev (41 fresh
    processes on a Mac Studio M1 Max, empty crash directory; p90 58 µs in release). That covers
    `current_exe`, the `dladdr` and load-command walk, six `sigaction` calls and one directory
    read. Nothing runs on the input, terminal or frame path. Measured with a throwaway test that
    re-runs itself and times `install` in the child:
    `cargo test --release -p slopty-crash --test <timing test> -- --nocapture`.
  - **Known gap, `.llvm.` suffixes.** When rustc gives a function a `.llvm.<hash>` suffix to
    share it between codegen units, Apple's linker writes the debug-map stab without the suffix
    and the object file keeps it. `backtrace` (and so std's own backtraces) looks the object's
    symbol up by that name, misses, and finds no line table. `atos` misses it too. Those frames
    keep their symbol-table name but no file or line. In the tests these are
    `slopty_crash::probe::panic` and an async `main`'s body. The fix belongs in backtrace-rs
    `symbolize/gimli/macho.rs` `search_object_map`: when the exact name is absent, take the
    object symbol named `<name>.llvm.<digits>`. It is a candidate for a vendored patch and an
    upstream pull request. The panic report's `location` does not depend on it.
    A shipped build is resolved against its dSYM, not the debug map, and has no such gap:
    `probe::panic` resolves to its line there.
  - **`slopty crashes`** lists the latest reports (`--last`, `--all-frames`, `--json`), each with
    its first frames, its file and any `.ips` beside it. It resolves the CLI's own pending
    records first.
  - Tests: `slopty-crash` unit tests cover the record format and its offsets, the link-register
    rule for a leaf function's caller, the stack-bounded frame walk, the handler's number
    formatting, names and rotation (20 per process, a crash loop keeps other processes'
    reports), torn files, the panic-machinery trim, `.ips` parsing of a recorded worker `SIGSEGV`
    (`tests/fixtures/slopty-worker-segv.ips`, identifiers zeroed), UTC and `.ips` times, and the
    image's own UUID and symbol lookup. `tests/crash.rs` runs the test binary again as a child
    that installs the reporter and crashes. It covers a panic, a segfault, an abort (the first
    frame is `libsystem_kernel`'s, named through the shared cache), a panic in an `extern "C"`
    function (one report, `aborted`), an abort after a caught panic (two reports, the panic not
    `aborted`), and a segfault on a named thread. Store tests cover the one-minute `.ips` link,
    a claim only one process wins, a dead claimer's record handed back, a dead writer's partial
    deleted, and the `.ips` size cap. Each binary's
    `tests/crash.rs` panics that binary through `SLOPTY_CRASH_TEST` and checks the report: the
    process name, the message, `slopty_crash::install`'s call site resolved to its line, and the
    binary's `main` by its whole path. The children point `EXC_CRASH` and `EXC_CORPSE_NOTIFY` at
    `MACH_PORT_NULL`, so deliberate crashes leave no `.ips` in the user's `DiagnosticReports`.
  - **Pending.** The worker and the server should read new reports on start, list them in
    `slopty worker doctor` (`CtlRequest::Doctor`), and tell attached clients once that the
    worker restarted after a crash. The app should show them and subscribe to `MXMetricManager`
    for hang and CPU-exception diagnostics, which no in-process hook can see. `slopty-crash`
    should join `LINUX_CRATES`. The iOS build of the crate was not compiled in this change.

- ✅ **A hang of the app's main thread is a report too** (2026-09-30). The app runs GPUI's hang
  monitor (the gpui-fast `profiler` feature: a journal of every task poll, action, input, draw
  and present on the main thread, read by a thread of its own every 2 s). One piece of work past
  250 ms, or 250 ms of work before one frame, once the first frame is up, becomes a `.hang`
  report through `slopty_crash::record_hang`. It says how long the thread was held, from when
  to the frame that ended it, and by what: a task and where it was spawned, an action, an
  input, a draw or a present. A task's spawn site is its frame. The launch before the first
  frame is not a hang. `slopty crashes` lists hangs with the crashes. They are kept apart, 20
  per process besides the crashes, so a run of hangs never pushes a crash out.
  - A store fix came with it. Two reports in one millisecond used to take the next free
    millisecond, which could be the oldest one's that the rotation had just deleted, and the
    rotation then deleted the new report. Now a colliding report takes the millisecond after
    its process's newest.
  - Tests: `slopty-crash` checks a hang's headline, its frames and its own rotation. The app's
    `hangs` test turns journal events into a report. `slopty-e2e`'s
    `a_hang_of_the_main_thread_is_filed_like_a_crash` holds the real app's main thread for
    600 ms (`Command::HoldMain`) and reads the report back as `slopty crashes` does. What the
    journal costs a frame is in `docs/MEASUREMENTS.md`, "hang reports".
  - `MXMetricManager` still sees what this cannot: a hang the system measured from outside,
    and CPU exceptions.

- ✅ **Shipped builds have limited debug info** (2026-10-01). The `dist` profile builds with
  `debug = "limited"`, and cargo packs each binary's debug info into `<bin>.dSYM` and strips the
  binary. Where the dSYMs go is the next entry.
  - **Why `limited`.** With `line-tables-only` the debug info has no linkage names, so an
    inlined frame is named by its bare name (`install`, `{closure#0}`, `call_once<…>`), and under
    fat LTO a crashing address resolves to about six inlined frames besides its own. With
    `limited`, 100 % of the app's inlined frames and 95.5 % of the worker's have their whole
    path, against 22 % before. It leaves the shipped binary the same size (packed split debug
    info keeps all of it in the dSYM), costs a rebuild 3–7 % more CPU, and grows the dSYM
    by 41 % in the app (191 to 269 MiB, 41 to 61 MB compressed) and 48 % in the worker. The
    rule was that whole paths on inlined frames are worth a modest cost; this is one. MEASUREMENTS
    2026-10-01, "debug info for shipped builds". `dev`, `test` and `release` keep
    `line-tables-only`: they are built many times a day, and their reports are read on the
    machine that built them.
  - **Without its dSYM** a shipped binary still names every frame: the strip (`strip -S`) keeps
    the local symbol table, which the reporter reads for the outermost frame of each address.
    Such a report has no file, no line and no inlined frames.
  - **The VM live lane** runs test binaries without this checkout's object files, where their
    debug info lives (`split-debuginfo = "unpacked"`). `cargo xtask vm live` makes a dSYM of
    each archived test binary with `dsymutil` and puts it beside the binary in the guest, so a
    report there has files and lines as here.
  - Tests: `tests/crash.rs`
    `a_shipped_build_resolves_through_its_dsym_and_names_frames_without_it` strips a copy of
    the test binary as the `dist` profile does, panics it alone and then with a dSYM beside it,
    and checks the frames are named, with their offsets, image and build UUID, without it, and
    resolved to their lines with it.

- ✅ **The dSYMs ship apart from the app** (2026-10-01). The bundle and the worker tarball ship
  lean. A release publishes the dSYMs as an artifact of their own, `<UUID>/<bin>.dSYM`, one
  directory per binary's Mach-O UUID. `cargo xtask symbolicate <report>` resolves a report
  taken in the field against them. Earlier the same day each dSYM shipped beside its binary in
  `Contents/MacOS`, where `backtrace` finds it on the user's machine; this replaces that.
  - **Why apart.** In the bundle the dSYMs weighed nearly four times the app: zipped, the bundle
    is 177.7 MB with them and 36.6 MB without, so 80 % of every download was debug info, paid
    by every user for the few reports that come back. The field loses nothing that cannot be had
    back. A report keeps each frame's address, image and offset, the build's UUID, and the names
    from the symbol table the strip leaves. What it lacks (file, line, inlined frames) is a
    function of the build, so the dSYM of the same UUID gives it back exactly.
  - **How a report is resolved.** `cargo xtask symbolicate <report.json> [--dsyms <dir or
    .tar.gz>]…` reads the report's build UUID and searches the given directories or release
    archives, then `target/dist`, `target/release` and `target/bundle`, for a `*.dSYM` whose
    DWARF file has that UUID. For each group of frames at one address in the executable it takes
    the address's offset in the image plus the dSYM's `__TEXT` address (less one for a return
    address, so a call's line is named, not the next one's), and `atos -i` gives the inlined
    chain with each frame's whole path, file and line. Frames in other images stay as the
    report has them. `--json` prints the resolved report.
  - **Building the artifact.** `cargo xtask dsyms --from target/dist --out <dir> <bin>…` checks
    each dSYM's UUID against its binary's and copies it (following cargo's symlink) to
    `<UUID>/<bin>.dSYM`. `cargo xtask bundle` puts the app's under `target/bundle/dSYMs`, beside
    the bundle, and the release job publishes the worker's as
    `slopty-worker-<tag>-aarch64-apple-darwin-dSYMs.tar.gz`.
  - **What it settles.** A worker installed outside a bundle, or deployed to another Mac binary by
    binary (`slopty-platform` `copy_binaries`), used to leave its dSYMs behind. Nothing ships them
    now, so every copy of a binary is complete. Notarization no longer meets a dSYM in the
    bundle. A build on this Mac still resolves its own reports in place: `target/dist` keeps the
    dSYM beside the binary, where `backtrace` looks.
  - **Not covered.** macOS's own `.ips` reports of native crashes are read by `slopty crashes` as
    they are; `symbolicate` takes Slopty's JSON reports. MEASUREMENTS 2026-10-01, "dSYMs apart
    from the bundle".
  - Tests: `xtask` `symbolicate::tests::a_dsym_found_by_uuid_resolves_inlined_frames` makes a
    dSYM of its own test binary, finds it by UUID and resolves an address inside an
    always-inlined function to its chain (the inlined function, then its caller by whole path,
    each with its file). End to end: a `dist` worker from the lean bundle, crashed in the field
    with no dSYM, resolved by `cargo xtask symbolicate` against the collected dSYMs and their
    archive (MEASUREMENTS).
