//! A daemon's log file deleted under it, its folder and all (a disk cleaned while the daemon
//! runs), is made again where it is read: the lines after go there, not to the deleted file. One
//! test in its own binary, since it points this process's stderr at a file.

#[cfg(test)]
mod log_gone {
    /// The log's folder removed between two lines: the second is in a new file at the same path.
    #[test]
    fn a_log_deleted_under_the_daemon_is_made_again() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("Slopty");
        std::fs::create_dir_all(&folder).unwrap();
        let path = folder.join("slopty-worker.log");
        let file = std::fs::OpenOptions::new().create(true).append(true).open(&path).unwrap();
        rustix::stdio::dup2_stderr(&file).unwrap();
        drop(file);
        slopty_crash::log::init_looking("info", 1 << 20, std::time::Duration::ZERO);
        tracing::info!("before the clean");
        std::fs::remove_dir_all(&folder).unwrap();
        tracing::info!("after the clean");
        tracing::info!("and on");
        let now = std::fs::read_to_string(&path).expect("the log is there again");
        assert!(now.contains("and on"), "the lines after the clean are in it: {now}");
        assert!(!now.contains("before the clean"), "a new file: {now}");
    }
}
