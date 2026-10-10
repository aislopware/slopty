//! Golden bytes of every message between the worker and ptyd, and the custody fingerprint they
//! make. A changed snapshot is a custody change: a worker update then restarts ptyd, ending
//! the sessions it holds, so accept one deliberately (`cargo insta review`).

#[cfg(test)]
#[expect(
    clippy::redundant_pub_crate,
    reason = "the build script compiles the same file, and there `pub(crate)` is what it needs"
)]
#[path = "../src/custody.rs"]
mod custody;

#[cfg(test)]
mod golden {
    use std::path::PathBuf;

    use slopty_core::{SessionId, WallMs};
    use slopty_proto::codec;
    use slopty_proto::input::CellMetrics;
    use slopty_proto::ptyd::PtydError;
    use slopty_proto::terminal::TermSize;
    use slopty_pty::SpawnSpec;
    use slopty_pty::protocol::{
        Bequest, Exit, Heir, OutputFrame, PtydEvent, PtydRequest, SessionInfo, inherit_args,
    };
    use uuid::Uuid;

    fn id() -> SessionId {
        SessionId::from_uuid(Uuid::from_u128(0x0102_0304_0506_0708_090a_0b0c_0d0e_0f10))
    }

    fn size() -> TermSize {
        TermSize { cols: 120, rows: 40, metrics: CellMetrics { cell_width: 16, cell_height: 34 } }
    }

    fn hex(bytes: &[u8]) -> String {
        bytes
            .chunks(16)
            .map(|row| row.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" "))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The name of `request`'s golden. No wildcard: a new request fails to build here until
    /// it has a sample in [`requests`].
    const fn request_name(request: &PtydRequest) -> &'static str {
        match request {
            PtydRequest::Spawn { .. } => "request_spawn",
            PtydRequest::Attach { .. } => "request_attach",
            PtydRequest::Output { .. } => "request_output",
            PtydRequest::Checkpoint { .. } => "request_checkpoint",
            PtydRequest::Resize { .. } => "request_resize",
            PtydRequest::Close { .. } => "request_close",
            PtydRequest::List => "request_list",
            PtydRequest::Shutdown => "request_shutdown",
            PtydRequest::Reclaim { .. } => "request_reclaim",
            PtydRequest::Adopt { .. } => "request_adopt",
            // An older ptyd reads it from a newer build: it is the handover's.
            PtydRequest::Succeed { .. } => "succession_request",
        }
    }

    /// The name of `event`'s golden, with no wildcard either.
    const fn event_name(event: &PtydEvent) -> &'static str {
        match event {
            PtydEvent::Spawned { .. } => "event_spawned",
            PtydEvent::Attached { .. } => "event_attached",
            PtydEvent::Ok => "event_ok",
            PtydEvent::Sessions(_) => "event_sessions",
            PtydEvent::Exited { .. } => "event_exited",
            PtydEvent::Error { .. } => "event_error",
        }
    }

    /// The name of `error`'s golden, with no wildcard either.
    const fn error_name(error: &PtydError) -> &'static str {
        match error {
            PtydError::SessionExists => "error_session_exists",
            PtydError::NoSuchSession => "error_no_such_session",
            PtydError::AttachedElsewhere => "error_attached_elsewhere",
            PtydError::Os(_) => "error_os",
        }
    }

    fn requests() -> Vec<PtydRequest> {
        let spec = SpawnSpec {
            command: vec!["/bin/zsh".to_owned(), "-l".to_owned()],
            cwd: Some(PathBuf::from("/Users/me/src")),
            env: vec![("TERM_PROGRAM".to_owned(), "slopty".to_owned())],
            size: size(),
        };
        vec![
            PtydRequest::Spawn { id: id(), spec },
            PtydRequest::Attach { id: id() },
            PtydRequest::Output { id: id(), bytes: b"$ ls\r\n".to_vec() },
            PtydRequest::Checkpoint { id: id(), state: b"\x1b[2J\x1b[H$ ".to_vec() },
            PtydRequest::Resize { id: id(), size: size() },
            PtydRequest::Close { id: id() },
            PtydRequest::List,
            PtydRequest::Shutdown,
            PtydRequest::Reclaim { id: id() },
            PtydRequest::Adopt {
                id: id(),
                pid: 4242,
                size: size(),
                started_ms: WallMs::from_millis(1_759_000_000_000),
                term: "xterm-ghostty".to_owned(),
            },
            PtydRequest::Succeed { program: PathBuf::from("/Users/me/.slopty/bin/slopty-ptyd") },
        ]
    }

    fn errors() -> Vec<PtydError> {
        vec![
            PtydError::SessionExists,
            PtydError::NoSuchSession,
            PtydError::AttachedElsewhere,
            PtydError::Os("openpty: no space left on device".to_owned()),
        ]
    }

    fn events() -> Vec<PtydEvent> {
        let info = SessionInfo {
            id: id(),
            pid: 4242,
            tty: PathBuf::from("/dev/ttys003"),
            size: size(),
            attached: true,
            exited: Some(Exit::with(-9)),
            backlog: 1024,
            checkpoint: 2048,
        };
        vec![
            PtydEvent::Spawned { id: id(), pid: 4242 },
            PtydEvent::Attached {
                id: id(),
                checkpoint: b"\x1b[H".to_vec(),
                backlog: b"ok\r\n".to_vec(),
                dropped: 7,
                size: size(),
                started_ms: WallMs::from_millis(1_759_000_000_000),
                term: "xterm-ghostty".to_owned(),
            },
            PtydEvent::Ok,
            PtydEvent::Sessions(vec![info]),
            PtydEvent::Exited { id: id(), exit: Exit::with(1) },
            PtydEvent::Error { id: Some(id()), error: PtydError::NoSuchSession },
        ]
    }

    #[test]
    fn every_request() {
        for request in requests() {
            let bytes = codec::encode(&request).expect("encodes");
            insta::assert_snapshot!(request_name(&request), hex(&bytes));
        }
    }

    #[test]
    fn every_event() {
        for event in events() {
            let bytes = codec::encode(&event).expect("encodes");
            insta::assert_snapshot!(event_name(&event), hex(&bytes));
        }
        for error in errors() {
            let event = PtydEvent::Error { id: None, error: error.clone() };
            let bytes = codec::encode(&event).expect("encodes");
            insta::assert_snapshot!(error_name(&error), hex(&bytes));
        }
    }

    /// Everything one ptyd hands the build it runs next: the command line it runs that build
    /// with, the state file's first frame, and a session's record after it.
    #[test]
    fn every_handover_frame() {
        insta::assert_snapshot!("succession_argv", inherit_args(7).join(" "));
        let bequest = Bequest {
            socket: PathBuf::from("/Users/me/.slopty/run/ptyd.sock"),
            backlog_bytes: 1 << 20,
            shell_dir: PathBuf::from("/Users/me/.slopty/shell"),
            sessions: 1,
            fds: vec![9],
        };
        let bytes = codec::encode(&bequest).expect("encodes");
        insta::assert_snapshot!("succession_bequest", hex(&bytes));
        let heir = Heir {
            id: id(),
            master: 9,
            pid: 4242,
            tty: PathBuf::from("/dev/ttys003"),
            started_ms: WallMs::from_millis(1_759_000_000_000),
            term: "xterm-ghostty".to_owned(),
            size: size(),
            checkpoint: b"\x1b[H".to_vec(),
            backlog: b"ok\r\n".to_vec(),
            dropped: 7,
            exited: Some(Exit::with(-9)),
            attached: true,
            orphan: true,
            orphan_mark: Some(1_759_000_000_123_456),
        };
        insta::assert_snapshot!("succession_heir", hex(&codec::encode(&heir).expect("encodes")));
    }

    /// The tap's frame, built by hand to copy its bytes once, is the request's own encoding.
    #[test]
    fn the_taps_frame_is_the_output_request() {
        let bytes = b"$ ls\r\n";
        let frame = OutputFrame::new(id(), bytes).unwrap();
        let request = PtydRequest::Output { id: id(), bytes: bytes.to_vec() };
        assert_eq!(frame.as_bytes(), codec::encode(&request).unwrap().as_ref());
    }

    /// The fingerprint the binary says is the one these goldens and the shell scripts make,
    /// so a changed golden or script moves it; and it moves with either.
    #[test]
    fn the_custody_is_the_goldens_and_the_scripts() {
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let (goldens, shell) = super::custody::sources(&manifest);
        let goldens = super::custody::goldens(&goldens).unwrap();
        let scripts = super::custody::scripts(&shell).unwrap();
        assert!(
            goldens.iter().any(|(name, _)| name.contains("request_spawn")),
            "the goldens are read: {:?}",
            goldens.iter().map(|(n, _)| n).collect::<Vec<_>>()
        );
        assert!(scripts.iter().any(|(path, _)| path == "zsh/.zshenv"), "and the scripts");
        let derived = super::custody::of(goldens.clone(), scripts.clone());

        let said = std::process::Command::new(env!("CARGO_BIN_EXE_slopty-ptyd"))
            .arg("--custody")
            .output()
            .unwrap();
        assert!(said.status.success(), "{said:?}");
        let said = String::from_utf8(said.stdout).unwrap();
        let succession = super::custody::succession(&goldens);
        assert_eq!(said.trim(), format!("{derived} {succession}"), "custody, then succession");
        assert_eq!(derived.len(), 16, "{derived}");
        assert!(
            goldens.iter().any(|(name, _)| name.contains("golden__succession_heir")),
            "the handover's goldens are read"
        );
        assert_ne!(succession, super::custody::of(Vec::new(), Vec::new()), "of something");

        let mut moved = goldens.clone();
        if let Some((_, text)) = moved.iter_mut().find(|(name, _)| name.contains("request_spawn")) {
            text.push('0');
        }
        assert_ne!(
            super::custody::of(moved.clone(), scripts.clone()),
            derived,
            "a golden moves it"
        );
        assert_eq!(
            super::custody::succession(&moved),
            succession,
            "but not the succession, unless it is the handover's"
        );
        let mut handover = goldens.clone();
        if let Some((_, text)) =
            handover.iter_mut().find(|(name, _)| name.contains("succession_heir"))
        {
            text.push('0');
        }
        assert_ne!(super::custody::succession(&handover), succession, "the handover's moves it");
        assert_ne!(super::custody::of(handover, scripts.clone()), derived, "and the custody");
        let mut edited = scripts.clone();
        if let Some((_, bytes)) = edited.first_mut() {
            bytes.push(b'\n');
        }
        assert_ne!(super::custody::of(goldens.clone(), edited), derived, "a script moves it");
        let mut header = goldens;
        for (_, text) in &mut header {
            *text = text.replacen("expression:", "expression: moved", 1);
        }
        assert_eq!(super::custody::of(header, scripts), derived, "an insta header does not");
    }

    /// A running ptyd says its custody beside its socket, under its own pid, for an install to
    /// read, and takes the file away when it shuts down.
    #[tokio::test]
    async fn a_running_ptyd_says_its_custody_beside_its_socket() {
        use std::time::Duration;
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("ptyd.sock");
        let said = dir.path().join("ptyd.custody");
        let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_slopty-ptyd"));
        let mut child = slopty_testkit::env::scrub(&mut command, &dir.path().join("home"))
            .arg("--socket")
            .arg(&socket)
            .spawn()
            .unwrap();
        let custody = std::process::Command::new(env!("CARGO_BIN_EXE_slopty-ptyd"))
            .arg("--custody")
            .output()
            .unwrap();
        let custody = String::from_utf8(custody.stdout).unwrap().trim().to_owned();
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        while !said.exists() && std::time::Instant::now() < deadline {
            assert!(child.try_wait().unwrap().is_none(), "ptyd exited early");
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        let text = std::fs::read_to_string(&said);
        let (mut client, _exits) = slopty_pty::PtydClient::connect(&socket).await.unwrap();
        client.shutdown().await.unwrap();
        let status = child.wait().unwrap();
        assert_eq!(text.unwrap(), format!("{} {custody}\n", child.id()));
        assert!(status.success(), "{status}");
        assert!(!said.exists(), "taken away at shutdown");
        assert!(!dir.path().join("ptyd.custody.part").exists(), "nothing half-written");
    }
}
