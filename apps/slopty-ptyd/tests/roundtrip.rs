//! End to end: start the daemon, spawn a shell, take the master, lose the connection, come back.

#[cfg(test)]
mod roundtrip {
    use std::path::PathBuf;
    use std::time::Duration;

    use slopty_core::SessionId;
    use slopty_proto::input::CellMetrics;
    use slopty_proto::terminal::TermSize;
    use slopty_pty::protocol::{MAX_CHECKPOINT_BYTES, OutputFrame, PtydRequest};
    use slopty_pty::{PtyMaster, PtydClient, SpawnSpec};
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    struct Daemon {
        child: std::process::Child,
        socket: PathBuf,
        _dir: tempfile::TempDir,
    }

    impl Drop for Daemon {
        fn drop(&mut self) {
            let _kill = self.child.kill();
            let _wait = self.child.wait();
        }
    }

    async fn start() -> Daemon {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("ptyd.sock");
        let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_slopty-ptyd"))
            .arg("--socket")
            .arg(&socket)
            .arg("--backlog-bytes")
            .arg("64")
            .spawn()
            .unwrap();
        // The socket file appears between bind() and listen(); poll by connecting.
        let deadline =
            tokio::time::Instant::now().checked_add(Duration::from_secs(60)).expect("deadline");
        let mut ready = false;
        loop {
            if let Some(status) = child.try_wait().expect("poll ptyd") {
                panic!("ptyd exited early: {status}");
            }
            if tokio::net::UnixStream::connect(&socket).await.is_ok() {
                ready = true;
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        if !ready {
            let _kill = child.kill();
            let _wait = child.wait();
            panic!("ptyd socket not ready after 60 s");
        }
        Daemon { child, socket, _dir: dir }
    }

    fn tap(id: SessionId, bytes: &[u8]) -> OutputFrame {
        OutputFrame::new(id, bytes).unwrap()
    }

    /// A session running `sleep 60`, spawned on a connection of its own.
    async fn sleeper(daemon: &Daemon) -> SessionId {
        let (mut client, _exits) = PtydClient::connect(&daemon.socket).await.unwrap();
        let id = SessionId::new();
        let spec = SpawnSpec {
            command: vec!["/bin/sh".into(), "-c".into(), "sleep 60".into()],
            cwd: None,
            env: Vec::new(),
            size: size(),
        };
        client.spawn(id, spec).await.unwrap();
        id
    }

    /// Attach `id` on a fresh connection, retrying while another connection is still letting go
    /// of it.
    async fn attach_when_free(daemon: &Daemon, id: SessionId) -> slopty_pty::client::Attached {
        let (mut next, _exits) = PtydClient::connect(&daemon.socket).await.unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                match next.attach(id).await {
                    Ok(attached) => break attached,
                    Err(e) => {
                        assert!(e.to_string().contains("another connection"), "{e}");
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
                }
            }
        })
        .await
        .expect("the session is handed back")
    }

    /// A raw connection that has said `Hello` and asked for `id`, its requests written as
    /// `extra` follows them.
    async fn raw_attach(daemon: &Daemon, id: SessionId, extra: &[u8]) -> tokio::net::UnixStream {
        let mut raw = tokio::net::UnixStream::connect(&daemon.socket).await.unwrap();
        let mut out = slopty_proto::codec::encode(&PtydRequest::Hello).unwrap().to_vec();
        out.extend_from_slice(&slopty_proto::codec::encode(&PtydRequest::Attach { id }).unwrap());
        out.extend_from_slice(extra);
        raw.write_all(&out).await.unwrap();
        raw
    }

    fn size() -> TermSize {
        TermSize { cols: 30, rows: 5, metrics: CellMetrics { cell_width: 8, cell_height: 16 } }
    }

    async fn read_until(master: &PtyMaster, needle: &[u8], mut acc: Vec<u8>) -> Vec<u8> {
        let mut buf = [0_u8; 4096];
        let deadline = tokio::time::Instant::now().checked_add(Duration::from_secs(10)).unwrap();
        while !acc.windows(needle.len()).any(|w| w == needle) {
            let n = tokio::time::timeout_at(deadline, master.read(&mut buf))
                .await
                .expect("timeout")
                .unwrap();
            assert_ne!(n, 0, "EOF before {needle:?}: {:?}", String::from_utf8_lossy(&acc));
            acc.extend_from_slice(&buf[..n]);
        }
        acc
    }

    #[tokio::test]
    async fn spawn_attach_detach_reattach_close() {
        let daemon = start().await;
        let (mut client, mut exits) = PtydClient::connect(&daemon.socket).await.unwrap();
        let id = SessionId::new();
        let pid = client
            .spawn(
                id,
                SpawnSpec {
                    command: vec![
                        "/bin/sh".into(),
                        "-c".into(),
                        "echo first; read x; echo got:$x; echo last".into(),
                    ],
                    cwd: None,
                    env: Vec::new(),
                    size: size(),
                },
            )
            .await
            .unwrap();
        assert!(pid > 0);

        // Give the shell time to print while we're detached, so it lands in the ring.
        tokio::time::sleep(Duration::from_millis(300)).await;
        let attached = client.attach(id).await.unwrap();
        assert_eq!(attached.size, size());
        assert!(
            String::from_utf8_lossy(&attached.backlog).contains("first"),
            "backlog: {:?}",
            String::from_utf8_lossy(&attached.backlog)
        );
        let master = PtyMaster::new(attached.master).unwrap();

        // Drop the connection: ptyd must resume draining so the child never blocks.
        drop(client);
        master.write_all(b"hello\r").await.unwrap();
        drop(master);
        tokio::time::sleep(Duration::from_millis(300)).await;

        let (mut client2, _) = PtydClient::connect(&daemon.socket).await.unwrap();
        let list = client2.list().await.unwrap();
        assert_eq!(list.len(), 1);
        assert!(!list[0].attached);
        let reattached = client2.attach(id).await.unwrap();
        let text = String::from_utf8_lossy(&reattached.backlog).into_owned();
        assert!(
            text.contains("got:hello") && text.contains("last"),
            "backlog after reattach: {text:?}"
        );

        // The child has finished by now; ptyd reports it.
        let exited = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(status) = client2.list().await.unwrap()[0].exited {
                    break status;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(exited, 0);
        client2.close(id).await.unwrap();
        assert!(client2.list().await.unwrap().is_empty());
        client2.shutdown().await.unwrap();
        // The first connection is gone, so its exit channel is closed or empty; either is fine.
        let _first_conn_exit = exits.try_recv();
    }

    #[tokio::test]
    async fn live_output_flows_through_the_handed_over_master() {
        let daemon = start().await;
        let (mut client, _exits) = PtydClient::connect(&daemon.socket).await.unwrap();
        let id = SessionId::new();
        client
            .spawn(
                id,
                SpawnSpec {
                    command: vec!["/bin/sh".into(), "-c".into(), "read x; echo echoed:$x".into()],
                    cwd: None,
                    env: Vec::new(),
                    size: size(),
                },
            )
            .await
            .unwrap();
        let attached = client.attach(id).await.unwrap();
        let master = PtyMaster::new(attached.master).unwrap();
        master.write_all(b"ping\r").await.unwrap();
        let out = read_until(&master, b"echoed:ping", attached.backlog).await;
        assert!(!out.is_empty());
        client
            .resize(id, TermSize { cols: 90, rows: 40, metrics: CellMetrics::default() })
            .await
            .unwrap();
        assert_eq!(slopty_pty::pty::get_size(master.as_fd()).unwrap(), (90, 40));
        client.shutdown().await.unwrap();
    }

    /// A child's exit status reaches the worker as ptyd sends it, with no request of the worker's
    /// to carry it: the worker tells its viewers the real status, not a guess made at EOF.
    #[tokio::test]
    async fn an_exit_arrives_without_a_request_to_carry_it() {
        let daemon = start().await;
        let (mut client, mut exits) = PtydClient::connect(&daemon.socket).await.unwrap();
        let id = SessionId::new();
        client
            .spawn(
                id,
                SpawnSpec {
                    command: vec!["/bin/sh".into(), "-c".into(), "read x; exit 7".into()],
                    cwd: None,
                    env: Vec::new(),
                    size: size(),
                },
            )
            .await
            .unwrap();
        let attached = client.attach(id).await.unwrap();
        let master = PtyMaster::new(attached.master).unwrap();
        master.write_all(b"go\r").await.unwrap();
        let _echo = read_until(&master, b"go", attached.backlog).await;
        let exit = tokio::time::timeout(Duration::from_secs(10), exits.recv())
            .await
            .expect("the exit, unprompted")
            .expect("exit channel open");
        assert_eq!(exit, (id, 7));
        client.shutdown().await.unwrap();
    }

    /// The attached worker copies output into ptyd's ring on the connection that holds the
    /// master and now and then replaces the ring with the terminal state; the next worker to
    /// attach gets that state and the output after it. Taps from any other connection are
    /// ignored.
    #[tokio::test]
    async fn a_checkpoint_and_the_tap_after_it_come_back_on_reattach() {
        let daemon = start().await;
        let (mut client, _exits) = PtydClient::connect(&daemon.socket).await.unwrap();
        let id = SessionId::new();
        client
            .spawn(
                id,
                SpawnSpec {
                    command: vec!["/bin/sh".into(), "-c".into(), "read x; echo bye:$x".into()],
                    cwd: None,
                    env: Vec::new(),
                    size: size(),
                },
            )
            .await
            .unwrap();
        let attached = client.attach(id).await.unwrap();
        assert!(attached.checkpoint.is_empty(), "a fresh session has no checkpoint");
        let master = PtyMaster::new(attached.master).unwrap();

        client.output(&tap(id, b"before-the-checkpoint")).await.unwrap();
        client.checkpoint(id, b"STATE".to_vec()).await.unwrap();
        client.output(&tap(id, b"after-the-checkpoint")).await.unwrap();
        // Taps carry no reply; a request that does orders them before its reply.
        let info = client.list().await.unwrap();
        assert_eq!(info.len(), 1);
        assert_eq!(info[0].checkpoint, b"STATE".len());

        // Another connection cannot tap a session it does not hold.
        let (mut stranger, _) = PtydClient::connect(&daemon.socket).await.unwrap();
        stranger.output(&tap(id, b"ignored")).await.unwrap();
        stranger.checkpoint(id, b"IGNORED".to_vec()).await.unwrap();
        assert_eq!(stranger.list().await.unwrap()[0].checkpoint, b"STATE".len());

        // The worker dies: the connection drops and ptyd resumes reading the master itself.
        drop(client);
        drop(master);
        let (mut next, _) = PtydClient::connect(&daemon.socket).await.unwrap();
        let reattached = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Ok(a) = next.attach(id).await {
                    break a;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(reattached.checkpoint, b"STATE");
        assert_eq!(reattached.dropped, 0);
        let backlog = String::from_utf8_lossy(&reattached.backlog).into_owned();
        assert!(backlog.starts_with("after-the-checkpoint"), "backlog: {backlog:?}");
        assert!(
            !backlog.contains("before") && !backlog.contains("ignored"),
            "backlog: {backlog:?}"
        );
        next.shutdown().await.unwrap();
    }

    /// A worker that dies while ptyd is still writing its `Attached` reply (a 12 MB checkpoint
    /// is many socket buffers) leaves the session to the next worker: ptyd's write fails, and the
    /// claim and the paused reader go with the connection.
    #[tokio::test]
    async fn a_worker_that_dies_mid_attach_leaves_the_session_to_the_next() {
        let daemon = start().await;
        let id = sleeper(&daemon).await;
        let (mut first, _exits) = PtydClient::connect(&daemon.socket).await.unwrap();
        let _held = first.attach(id).await.unwrap();
        let state = vec![b'.'; MAX_CHECKPOINT_BYTES];
        first.checkpoint(id, state).await.unwrap();
        first.checkpoint(id, vec![b'!'; MAX_CHECKPOINT_BYTES + 1]).await.unwrap();
        let info = first.list().await.unwrap();
        assert_eq!(
            info[0].checkpoint, MAX_CHECKPOINT_BYTES,
            "one too large for an attach is ignored"
        );
        drop(first);

        let mut dying = attach_when_free_raw(&daemon, id).await;
        // Some of the reply, then gone while ptyd still has megabytes to write.
        let mut some = vec![0_u8; 64 << 10];
        dying.read_exact(&mut some).await.unwrap();
        drop(dying);

        let attached = attach_when_free(&daemon, id).await;
        assert_eq!(
            attached.checkpoint.len(),
            MAX_CHECKPOINT_BYTES,
            "the whole state, in one frame"
        );
        let (mut last, _exits) = PtydClient::connect(&daemon.socket).await.unwrap();
        last.shutdown().await.unwrap();
    }

    /// A raw connection that holds `id` once the connection before it let go.
    async fn attach_when_free_raw(daemon: &Daemon, id: SessionId) -> tokio::net::UnixStream {
        let (mut probe, _exits) = PtydClient::connect(&daemon.socket).await.unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            while probe.list().await.unwrap().iter().any(|s| s.id == id && s.attached) {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the first connection lets go");
        raw_attach(daemon, id, &[]).await
    }

    /// A frame that does not decode ends the connection that sent it, and the session it held
    /// goes back to ptyd even while that socket is still open on the worker's side.
    #[tokio::test]
    async fn a_frame_that_does_not_decode_hands_the_session_back() {
        let daemon = start().await;
        let id = sleeper(&daemon).await;
        // A whole frame whose body names no request.
        let garbage = [1, 0, 0, 0, 0x7f];
        let _still_open = raw_attach(&daemon, id, &garbage).await;
        let attached = attach_when_free(&daemon, id).await;
        assert!(attached.checkpoint.is_empty());
        let (mut last, _exits) = PtydClient::connect(&daemon.socket).await.unwrap();
        last.shutdown().await.unwrap();
    }

    /// The worker learns that ptyd is gone: its channel of exits ends.
    #[tokio::test]
    async fn the_exit_channel_ends_when_ptyd_goes() {
        let daemon = start().await;
        let (_client, mut exits) = PtydClient::connect(&daemon.socket).await.unwrap();
        let (mut other, _exits) = PtydClient::connect(&daemon.socket).await.unwrap();
        other.shutdown().await.unwrap();
        let ended = tokio::time::timeout(Duration::from_secs(10), exits.recv()).await;
        assert_eq!(ended.expect("the channel ends"), None);
    }

    /// What handing ptyd a checkpoint costs: a 4 MiB state sent 20 times, each followed by a
    /// `List`, whose reply comes after ptyd took the whole state. Run with
    /// `cargo nextest run -p slopty-ptyd --release --run-ignored only checkpoint_transfer_cost
    /// --no-capture`.
    #[tokio::test]
    #[ignore = "measurement, run by hand"]
    async fn checkpoint_transfer_cost() {
        let daemon = start().await;
        let (mut client, _exits) = PtydClient::connect(&daemon.socket).await.unwrap();
        let id = SessionId::new();
        let spec = SpawnSpec {
            command: vec!["/bin/sh".into(), "-c".into(), "sleep 60".into()],
            cwd: None,
            env: Vec::new(),
            size: size(),
        };
        client.spawn(id, spec).await.unwrap();
        let _attached = client.attach(id).await.unwrap();
        let state: Vec<u8> =
            (0..4_u32 << 20).map(|i| b"line of a checkpoint\r\n"[i as usize % 22]).collect();
        let mut took = Vec::new();
        for _ in 0..20 {
            let t = std::time::Instant::now();
            client.checkpoint(id, state.clone()).await.unwrap();
            assert_eq!(client.list().await.unwrap()[0].checkpoint, state.len());
            took.push(t.elapsed().as_micros());
        }
        took.sort_unstable();
        eprintln!(
            "checkpoint_transfer_cost: 4 MiB p50 {} us, max {} us",
            took[took.len() / 2],
            took[took.len() - 1]
        );
        client.shutdown().await.unwrap();
    }
}
