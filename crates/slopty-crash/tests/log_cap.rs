//! A daemon's log file stays under its cap: the process's stderr is a file, as launchd makes it,
//! and the log written past the cap moves to `<file>.1` while a new file takes its name. One
//! test in its own binary, since it points this process's stderr at a file.

#[cfg(test)]
mod log_cap {
    /// 64 KiB of lines through a 4 KiB cap leave the file and its one old generation each under
    /// the cap and a line, with the newest line in the file and the oldest gone; stdout, the same
    /// file as stderr at start, follows the file.
    #[test]
    fn a_log_past_its_cap_moves_aside_and_starts_again() {
        use std::io::Write as _;
        const CAP: u64 = 4096;
        // The cap and one line, the most either file holds.
        const MOST: usize = 4096 + 160;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("slopty-worker.log");
        let file = std::fs::OpenOptions::new().create(true).append(true).open(&path).unwrap();
        rustix::stdio::dup2_stderr(&file).unwrap();
        rustix::stdio::dup2_stdout(&file).unwrap();
        drop(file);
        slopty_crash::log::init_capped("info", CAP);
        for line in 0..1000_u32 {
            tracing::info!(line, "a line of the log, long enough to count");
        }
        let old = dir.path().join("slopty-worker.log.1");
        let (now, before) = (std::fs::read_to_string(&path).unwrap(), std::fs::read(&old).unwrap());
        assert!(now.len() <= MOST, "the file: {} bytes", now.len());
        assert!(before.len() <= MOST, "the old one: {} bytes", before.len());
        assert!(now.trim_end().ends_with(" line=999"), "the newest line is in the file: {now}");
        let first = |text: &str| text.lines().any(|l| l.ends_with(" line=0"));
        assert!(!first(&now) && !first(&String::from_utf8_lossy(&before)), "the oldest is gone");
        let entries = std::fs::read_dir(dir.path()).unwrap().count();
        assert_eq!(entries, 2, "one old generation, no more");
        // Stdout writes to the new file too, not to the one moved aside.
        writeln!(std::io::stdout(), "on stdout").unwrap();
        std::io::stdout().flush().unwrap();
        let now = std::fs::read_to_string(&path).unwrap();
        assert!(now.ends_with("on stdout\n"), "stdout follows: {now}");
        assert!(!now.contains('\u{1b}'), "no colour in a file");
    }
}
