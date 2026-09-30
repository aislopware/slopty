//! The server daemon reports its own panics: the reporter is installed first thing in `main`.

#[cfg(test)]
#[cfg(target_vendor = "apple")]
mod crash {
    use slopty_crash::Kind;
    use slopty_crash::probe::{self, Trigger};

    #[test]
    fn a_panic_leaves_a_report_with_resolved_frames() {
        let data = tempfile::tempdir().unwrap();
        let exe = std::path::Path::new(env!("CARGO_BIN_EXE_slopty-server"));
        let crashed = probe::run(exe, &[], Trigger::Panic, data.path()).unwrap();
        assert_eq!(crashed.status.code(), Some(101), "died of the panic: {}", crashed.stderr);
        let [report] = crashed.reports.as_slice() else {
            panic!("one report: {:#?}", crashed.reports);
        };
        assert_eq!(report.process, "slopty-server", "filed under its process");
        assert!(
            matches!(&report.kind, Kind::Panic { message, .. } if message.contains("asked this process to panic")),
            "the panic: {:?}",
            report.kind
        );
        let frame = |function: &str| {
            report
                .frames
                .iter()
                .find(|f| f.function.as_deref().is_some_and(|name| name.starts_with(function)))
                .unwrap_or_else(|| panic!("no frame in {function}: {:#?}", report.frames))
        };
        let install = frame("slopty_crash::install");
        assert!(
            install
                .file
                .as_deref()
                .is_some_and(|file| file.ends_with("crates/slopty-crash/src/lib.rs"))
                && install.line.is_some(),
            "the call site resolved to its source line: {install:?}"
        );
        let main = frame("slopty_server::main");
        assert!(
            main.file
                .as_deref()
                .is_none_or(|file| file.ends_with("apps/slopty-server/src/main.rs")),
            "main, by its whole path, in this binary's source: {main:?}"
        );
    }
}
