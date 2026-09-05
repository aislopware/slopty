//! ghostty's terminfo entry: the source we render, and what `tic` makes of it.
//!
//! The snapshot is the contract with the vendored ghostty — bumping the submodule and
//! re-porting the entry shows up here as a diff, which is the point of keeping it.

#[cfg(test)]
mod terminfo_entry {
    use slopty_pty::terminfo;

    /// The rendered source, byte for byte. Accept a change only after reading what moved in
    /// `vendor/ghostty/src/terminfo/ghostty.zig`.
    #[test]
    fn the_source_is_what_ghostty_writes() {
        insta::assert_snapshot!("ghostty.terminfo", terminfo::source());
    }

    /// The three names, and the shape of the source `tic` parses: `name|name|name,` then one
    /// tab-indented capability per line.
    #[test]
    fn the_source_has_the_shape_tic_expects() {
        let source = terminfo::source();
        let mut lines = source.lines();
        assert_eq!(lines.next(), Some("xterm-ghostty|ghostty|Ghostty,"));
        let caps: Vec<&str> = lines.collect();
        assert_eq!(caps.len(), terminfo::CAPABILITIES.len());
        for line in &caps {
            assert!(line.starts_with('\t'), "capabilities are indented: {line:?}");
            assert!(line.ends_with(','), "capabilities are comma-terminated: {line:?}");
        }
        assert!(caps.contains(&"\tam,"), "a boolean is bare");
        assert!(caps.contains(&"\tcolors#256,"), "a number carries #");
        assert!(caps.contains(&"\tsc=\\E7,"), "a string carries = and one backslash");
        assert!(caps.iter().any(|l| l.ends_with("@,")), "a canceled capability carries @");
    }

    /// `tic` accepts the entry, files it where ncurses looks, and `default_term` then offers
    /// `xterm-ghostty` to the shells we spawn. Running it twice is a no-op.
    #[tokio::test]
    async fn tic_compiles_the_entry_and_the_term_name_follows() {
        let home = tempfile::tempdir().expect("a temp dir");
        let database = home.path().join(".terminfo");

        // SAFETY: nextest gives every test its own process, so nothing else reads the environment
        // while this runs. These are the variables `terminfo::dirs` searches.
        // SAFETY: as above.
        unsafe {
            std::env::set_var("HOME", home.path());
        }
        // SAFETY: as above.
        unsafe {
            std::env::set_var(terminfo::DIR_ENV, &database);
        }
        // SAFETY: as above.
        unsafe {
            std::env::remove_var("TERMINFO_DIRS");
        }
        assert_eq!(terminfo::user_database(), database, "the override wins over $HOME");
        assert!(!terminfo::installed(), "an empty home has no entry");
        assert_eq!(slopty_pty::pty::default_term(), "xterm-256color");

        let outcome = terminfo::install(&database).await.expect("tic compiles the entry");
        assert_eq!(outcome, terminfo::Installed::Compiled);
        assert!(
            database.join("78/xterm-ghostty").exists() || database.join("x/xterm-ghostty").exists(),
            "the compiled entry is where ncurses looks: {:?}",
            std::fs::read_dir(&database).map(|d| d.flatten().map(|e| e.path()).collect::<Vec<_>>()),
        );
        assert!(terminfo::installed());
        assert_eq!(slopty_pty::pty::default_term(), "xterm-ghostty");

        let again = terminfo::install(&database).await.expect("a second run is a no-op");
        assert_eq!(again, terminfo::Installed::Already);

        // `TERM` and the child's search path agree: we answered from a database ncurses does
        // not know about, so the child is pointed at it.
        assert_eq!(terminfo::child_database().as_deref(), Some(database.as_path()));
    }

    /// Without the override the child is told nothing: `default_term` answered from the places
    /// ncurses searches by itself, so an inherited `TERMINFO` is cleared rather than replaced.
    #[test]
    fn without_an_override_the_child_gets_no_terminfo() {
        // SAFETY: nextest gives every test its own process; nothing else reads the environment.
        unsafe {
            std::env::remove_var(terminfo::DIR_ENV);
        }
        assert_eq!(terminfo::child_database(), None);
        assert!(terminfo::dirs().len() > 1, "the ordinary places: {:?}", terminfo::dirs());
    }
}
