//! End to end: start the daemon, spawn a shell, take the master, lose the connection, come back.

#[cfg(test)]
mod roundtrip {
    use std::path::PathBuf;
    use std::time::Duration;

    use slopty_core::SessionId;
    use slopty_proto::input::CellMetrics;
    use slopty_proto::terminal::TermSize;
    use slopty_pty::{PtyMaster, PtydClient, SpawnSpec};

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
        let child = std::process::Command::new(env!("CARGO_BIN_EXE_slopty-ptyd"))
            .arg("--socket")
            .arg(&socket)
            .arg("--backlog-bytes")
            .arg("64")
            .spawn()
            .unwrap();
        // The socket file appears between bind() and listen(); poll by connecting.
        for _ in 0..500 {
            if tokio::net::UnixStream::connect(&socket).await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        Daemon { child, socket, _dir: dir }
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
}
