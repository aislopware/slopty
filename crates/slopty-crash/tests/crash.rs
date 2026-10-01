//! Real crashes of a real process: this test binary runs itself again as a child that installs
//! the reporter and crashes, then reads what the child left.
//!
//! A signal record is resolved by the next run of the same build, and the parent is that run:
//! the same executable, so the same image UUID.

#[cfg(test)]
#[cfg(target_vendor = "apple")]
mod crash {
    use std::os::unix::process::ExitStatusExt as _;
    use std::path::Path;
    use std::process::Command;

    use slopty_crash::probe::{self, Crashed, Trigger};
    use slopty_crash::{Frame, Kind, Process, Report};

    /// Which crash the child makes when it is not one [`probe`] knows.
    const CHILD_ENV: &str = "SLOPTY_CRASH_CHILD";

    /// The child: installs the reporter, which crashes as `SLOPTY_CRASH_TEST` asks; or, under
    /// [`CHILD_ENV`], crashes the way it names. Does nothing in the parent's own run.
    ///
    /// Never inlined, so its frame is named by its whole path from the symbol table: a sanitized
    /// build inlines it into the harness's `FnOnce::call_once` shim, which a stripped build
    /// would then name in its place.
    #[test]
    #[inline(never)]
    fn child() {
        let (Some(data), ask) = (std::env::var_os("SLOPTY_DATA_DIR"), std::env::var(CHILD_ENV))
        else {
            return;
        };
        if std::env::var_os(slopty_crash::TRIGGER_ENV).is_none() && ask.is_err() {
            return;
        }
        no_os_report();
        slopty_crash::install(Process::Worker, Path::new(&data));
        match ask.as_deref() {
            Ok("nounwind") => panics_where_it_cannot_unwind(),
            Ok("thread") => segfaults_on_a_named_thread(),
            Ok("caught") => aborts_long_after_a_caught_panic(),
            _ => {}
        }
    }

    /// Keeps macOS from filing a report of the child's deliberate crash in the user's
    /// `DiagnosticReports`: the crash exception goes nowhere instead of to `ReportCrash`.
    fn no_os_report() {
        /// `EXC_MASK_CRASH | EXC_MASK_CORPSE_NOTIFY`, `<mach/exception_types.h>`.
        const MASK: u32 = (1 << 10) | (1 << 13);
        /// `EXCEPTION_DEFAULT`, `<mach/exception_types.h>`.
        const EXCEPTION_DEFAULT: i32 = 1;
        /// `THREAD_STATE_NONE` for arm64, `<mach/arm/thread_status.h>`.
        const THREAD_STATE_NONE: i32 = 5;
        unsafe extern "C" {
            /// This task's port, which `mach_task_self()` (`<mach/mach_init.h>`) reads.
            static mach_task_self_: u32;
            fn task_set_exception_ports(
                task: u32,
                mask: u32,
                port: u32,
                behavior: i32,
                flavor: i32,
            ) -> i32;
        }
        // SAFETY: libsystem sets `mach_task_self_` before any Rust code runs and never changes it.
        let task = unsafe { mach_task_self_ };
        // SAFETY: `task_set_exception_ports` (mach/task.h) only rebinds this task's crash
        // exceptions, to `MACH_PORT_NULL`.
        let set = unsafe {
            task_set_exception_ports(task, MASK, 0, EXCEPTION_DEFAULT, THREAD_STATE_NONE)
        };
        assert_eq!(set, 0, "task_set_exception_ports");
    }

    #[inline(never)]
    extern "C" fn panics_where_it_cannot_unwind() {
        panic!("a panic in an extern \"C\" function");
    }

    #[inline(never)]
    fn aborts_long_after_a_caught_panic() {
        let caught = std::panic::catch_unwind(|| panic!("caught and handled"));
        assert!(caught.is_err(), "the panic was caught");
        std::process::abort();
    }

    fn segfaults_on_a_named_thread() {
        let worker =
            std::thread::Builder::new().name("slopty-encoder".to_owned()).spawn(faults_at_24);
        let _never = worker.unwrap().join();
    }

    /// The named thread's body, never inlined so the report names it by its path.
    #[inline(never)]
    fn faults_at_24() -> u8 {
        // SAFETY: none; the child crashes here on purpose. The load faults and the process dies
        // of SIGSEGV before anything reads the value.
        unsafe { std::ptr::with_exposed_provenance::<u8>(24).read_volatile() }
    }

    fn crash(trigger: Trigger) -> (tempfile::TempDir, Crashed) {
        let data = tempfile::tempdir().unwrap();
        let exe = std::env::current_exe().unwrap();
        let crashed =
            probe::run(&exe, &["--exact", "crash::child", "--nocapture"], trigger, data.path())
                .unwrap();
        (data, crashed)
    }

    fn crash_as(child: &str) -> (tempfile::TempDir, Crashed) {
        let data = tempfile::tempdir().unwrap();
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "crash::child", "--nocapture"])
            .env(CHILD_ENV, child)
            .env("SLOPTY_DATA_DIR", data.path())
            .output()
            .unwrap();
        let reports = slopty_crash::reports_with(&slopty_crash::crash_dir(data.path()), None);
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        (data, Crashed { status: output.status, stderr, reports })
    }

    fn only(crashed: &Crashed) -> &Report {
        let all: Vec<String> = crashed.reports.iter().map(describe).collect();
        assert_eq!(crashed.reports.len(), 1, "one report:\n{}\n{}", all.join("\n"), crashed.stderr);
        eprintln!("{}", describe(&crashed.reports[0]));
        &crashed.reports[0]
    }

    /// The first frame in `function`, a path: the function itself or a closure or generic
    /// instance of it. A trait shim that only mentions it in its type
    /// (`<crash::crash::child::{closure#0} as FnOnce<()>>::call_once`) is not in it.
    fn find<'a>(report: &'a Report, function: &str) -> &'a Frame {
        let within = |name: &str| {
            name.strip_prefix(function).is_some_and(|rest| {
                rest.is_empty() || rest.starts_with("::") || rest.starts_with('<')
            })
        };
        report
            .frames
            .iter()
            .find(|f| f.function.as_deref().is_some_and(within))
            .unwrap_or_else(|| panic!("no frame in {function}:\n{}", describe(report)))
    }

    /// Where the standard library's sources sit, in the prebuilt library's paths
    /// (`/rustc/<commit>/library/…`) and in one built with the program (`-Zbuild-std`).
    const STD_SOURCES: [&str; 3] =
        ["/library/core/src/", "/library/std/src/", "/library/alloc/src/"];

    /// The first frame of code outside Rust's standard library: where the faulting
    /// instruction was, past what was inlined into it from `core` and `std` (a sanitized build
    /// carries their debug info, so those inlined frames come first).
    fn first_own(report: &Report) -> Option<&Frame> {
        report.frames.iter().find(|f| {
            f.file.as_deref().is_none_or(|file| !STD_SOURCES.iter().any(|s| file.contains(s)))
        })
    }

    fn describe(report: &Report) -> String {
        let frames: Vec<String> = report.frames.iter().map(Frame::describe).collect();
        format!("{}\n  {}", report.headline(), frames.join("\n  "))
    }

    fn assert_resolved(frame: &Frame, file: &str) {
        assert!(frame.file.as_deref().is_some_and(|f| f.ends_with(file)), "{frame:?} is in {file}");
        assert!(frame.line.is_some_and(|line| line > 0), "{frame:?} has a line");
    }

    #[test]
    fn a_panic_leaves_its_message_place_and_frames() {
        let (_data, crashed) = crash(Trigger::Panic);
        assert_eq!(crashed.status.code(), Some(101), "the test failed by panicking");
        let report = only(&crashed);
        let Kind::Panic { message, location, aborted } = &report.kind else {
            panic!("a panic: {report:#?}");
        };
        assert_eq!(message, "SLOPTY_CRASH_TEST asked this process to panic", "message");
        assert!(location.as_deref().is_some_and(|l| l.contains("probe.rs:")), "at {location:?}");
        assert!(!aborted, "it unwound");
        assert_eq!(report.process, "slopty-worker", "filed under the process it installed as");
        let first = report.frames.first().unwrap();
        assert!(
            first.function.as_deref().is_some_and(|f| f.starts_with("slopty_crash::probe::panic")),
            "the report starts where the panic was raised: {:#?}",
            report.frames
        );
        assert_resolved(find(report, "crash::crash::child"), "tests/crash.rs");
        assert!(report.build.uuid.is_some() && report.build.exe.is_some(), "the build: {report:?}");
        assert!(crashed.stderr.contains("asked this process to panic"), "the old hook still ran");
    }

    #[test]
    fn a_segfault_is_recorded_in_the_handler_and_resolved_by_the_next_run() {
        let (data, crashed) = crash(Trigger::Segv);
        assert_eq!(crashed.status.signal(), Some(libc::SIGSEGV), "the child still dies of it");
        let report = only(&crashed);
        assert_eq!(
            report.kind,
            Kind::Signal { signal: libc::SIGSEGV, name: "SIGSEGV".to_owned(), address: 16 },
            "the fault"
        );
        let first = report.frames.first().unwrap();
        assert!(
            first.function.as_deref().is_some_and(|f| f.starts_with("slopty_crash::probe::segv")),
            "the faulting instruction: {:#?}",
            report.frames
        );
        assert!(first.offset.is_some(), "an offset into the executable too");
        find(report, "slopty_crash::install");
        assert_resolved(find(report, "crash::crash::child"), "tests/crash.rs");
        assert!(report.path.extension().is_some_and(|e| e == "json"), "stored as a report");
        let left: Vec<_> = std::fs::read_dir(slopty_crash::crash_dir(data.path()))
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(left.len(), 1, "the record became the report: {left:?}");
    }

    #[test]
    fn an_abort_names_the_system_frames_too() {
        let (_data, crashed) = crash(Trigger::Abort);
        assert_eq!(crashed.status.signal(), Some(libc::SIGABRT), "died of SIGABRT");
        let report = only(&crashed);
        assert!(matches!(report.kind, Kind::Signal { signal: libc::SIGABRT, .. }), "{report:?}");
        let first = report.frames.first().unwrap();
        assert_eq!(
            first.image.as_deref(),
            Some("libsystem_kernel.dylib"),
            "abort ends in the kernel's pthread_kill: {:#?}",
            report.frames
        );
        assert!(first.function.is_some(), "named from the shared cache: {first:?}");
        find(report, "slopty_crash::probe::fire");
        assert_resolved(find(report, "crash::crash::child"), "tests/crash.rs");
    }

    #[test]
    fn a_panic_that_cannot_unwind_is_one_report_marked_aborted() {
        let (_data, crashed) = crash_as("nounwind");
        assert_eq!(crashed.status.signal(), Some(libc::SIGABRT), "a panic in extern \"C\" aborts");
        let report = only(&crashed);
        let Kind::Panic { message, aborted, .. } = &report.kind else {
            panic!("the panic, not the abort: {report:#?}");
        };
        assert_eq!(message, "a panic in an extern \"C\" function", "message");
        assert!(aborted, "the abort that followed is folded into it");
        assert_resolved(
            find(report, "crash::crash::panics_where_it_cannot_unwind"),
            "tests/crash.rs",
        );
    }

    #[test]
    fn an_abort_after_a_caught_panic_is_a_report_of_its_own() {
        let (_data, crashed) = crash_as("caught");
        assert_eq!(crashed.status.signal(), Some(libc::SIGABRT), "died of SIGABRT");
        let all: Vec<String> = crashed.reports.iter().map(describe).collect();
        assert_eq!(crashed.reports.len(), 2, "the panic and the abort:\n{}", all.join("\n"));
        let panic = crashed
            .reports
            .iter()
            .find_map(|r| match &r.kind {
                Kind::Panic { message, aborted, .. } => Some((message.as_str(), *aborted)),
                _ => None,
            })
            .expect("the caught panic's report");
        assert_eq!(panic, ("caught and handled", false), "the caught panic did not abort");
        let abort = crashed
            .reports
            .iter()
            .find(|r| matches!(r.kind, Kind::Signal { signal: libc::SIGABRT, .. }))
            .expect("the abort keeps its own report");
        find(abort, "crash::crash::aborts_long_after_a_caught_panic");
    }

    #[test]
    fn a_crash_off_the_main_thread_names_its_thread() {
        let (_data, crashed) = crash_as("thread");
        assert_eq!(crashed.status.signal(), Some(libc::SIGSEGV), "died of SIGSEGV");
        let report = only(&crashed);
        assert_eq!(report.thread.as_deref(), Some("slopty-encoder"), "the thread");
        assert!(matches!(report.kind, Kind::Signal { address: 24, .. }), "{report:?}");
        let first = first_own(report).and_then(|f| f.function.as_deref());
        assert_eq!(first, Some("crash::crash::faults_at_24"), "the thread's body faulted");
    }

    /// A shipped build: this test binary with its debug info stripped (`strip -S`, as the dist
    /// profile's `strip = "debuginfo"` does), alone in a directory as a user runs it, and then
    /// with its dSYM beside it as a `dist` build in `target/dist` has one. Alone, the report
    /// still names every frame from the symbol table and keeps each frame's offset and the
    /// build's UUID, which `cargo xtask symbolicate` resolves; with the dSYM, the frames have
    /// their files and lines on the spot.
    #[test]
    fn a_shipped_build_resolves_through_its_dsym_and_names_frames_without_it() {
        let exe = std::env::current_exe().unwrap();
        let shipped = tempfile::tempdir().unwrap();
        let copy = shipped.path().join("crash");
        std::fs::copy(&exe, &copy).unwrap();
        assert!(Command::new("strip").arg("-S").arg(&copy).status().unwrap().success(), "strip");
        let run = |label: &str| {
            let data = tempfile::tempdir().unwrap();
            let args = ["--exact", "crash::child", "--nocapture"];
            let crashed = probe::run(&copy, &args, Trigger::Panic, data.path()).unwrap();
            eprintln!("{label}:");
            let report = only(&crashed).clone();
            let first = report.frames.first().unwrap();
            assert!(
                first
                    .function
                    .as_deref()
                    .is_some_and(|f| f.starts_with("slopty_crash::probe::panic")),
                "{label}: named where the panic was raised: {:#?}",
                report.frames
            );
            report
        };

        let bare = run("without its debug info");
        let child = find(&bare, "crash::crash::child");
        assert_eq!((&child.file, child.line), (&None, None), "no line table left: {child:?}");
        assert!(child.offset.is_some() && child.image.is_some(), "where to look it up: {child:?}");
        assert!(bare.build.uuid.is_some(), "and in which build: {:?}", bare.build);
        find(&bare, "slopty_crash::install");

        let dsym = shipped.path().join("crash.dSYM");
        let made = Command::new("dsymutil").arg(&exe).arg("-o").arg(&dsym).output().unwrap();
        assert!(made.status.success(), "dsymutil: {}", String::from_utf8_lossy(&made.stderr));
        let resolved = run("with its dSYM");
        assert_resolved(find(&resolved, "crash::crash::child"), "tests/crash.rs");
        assert_resolved(find(&resolved, "slopty_crash::install"), "src/lib.rs");
    }
}
