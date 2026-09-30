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
    Inlined frames keep the short names, with their file and line.
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
