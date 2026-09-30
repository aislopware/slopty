//! A worker in a macOS guest, reached from this Mac over the VM network the way the app reaches
//! any worker: the client core's link over QUIC and a `TermState` fed its frames. The worker is
//! this tree's build, put there by `slopty worker deploy`, running from launchd in the guest's
//! logged-in session with the Accessibility and Screen Recording grants the guest's base holds.
//!
//! Live (`#[ignore]`), run by `cargo xtask vm e2e`, which clones a fresh guest, deploys the worker
//! and says where it is (`SLOPTY_VM_WORKER`, `ip:port`) and which macOS it runs
//! (`SLOPTY_VM_MACOS`, the major version). Nothing is typed into a shell the test did not open.

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::time::{Duration, Instant};

    use anyhow::{Context as _, Result, bail};
    use slopty_client::{Effect, LinkEvent, TermState, WorkerLink};
    use slopty_core::ClientId;
    use slopty_net::client::{bind_client, connect_addr};
    use slopty_proto::handshake::Hello;
    use slopty_proto::input::{KeyAction, KeyCode, KeyEvent, Mods};
    use slopty_proto::server::Os;
    use slopty_proto::terminal::{OpenSession, TermEvent, TermRequest, TermSize};
    use slopty_proto::{ClientMsg, WorkerMsg};

    /// How long a connection, a shell start or a command may take.
    const STEP: Duration = Duration::from_secs(30);

    fn var(name: &str) -> String {
        std::env::var(name).unwrap_or_else(|_| panic!("{name} unset: run `cargo xtask vm e2e`"))
    }

    fn after(wait: Duration) -> Instant {
        Instant::now().checked_add(wait).expect("a deadline")
    }

    /// The visible rows of `state`, trailing blanks trimmed.
    fn rows(state: &TermState) -> Vec<String> {
        state
            .view()
            .iter()
            .map(|row| row.line.map(|l| l.text().trim_end().to_owned()).unwrap_or_default())
            .collect()
    }

    /// Apply the link's events to `state` until a row reads `row` exactly.
    async fn until_row(
        link: &WorkerLink,
        events: &mut tokio::sync::mpsc::Receiver<LinkEvent>,
        state: &mut TermState,
        row: &str,
    ) -> Result<()> {
        let deadline = after(STEP);
        while !rows(state).iter().any(|r| r == row) {
            let event = tokio::time::timeout_at(deadline.into(), events.recv())
                .await
                .with_context(|| format!("{row:?} on screen: {:#?}", rows(state)))?
                .context("the link closed")?;
            match event {
                LinkEvent::Term { session, event } => {
                    for effect in state.apply(event) {
                        if let Effect::Request(req) = effect {
                            link.send(ClientMsg::Term { session, req }).await?;
                        }
                    }
                }
                LinkEvent::Disconnected(why) => bail!("disconnected: {why}"),
                _other => {}
            }
        }
        Ok(())
    }

    #[tokio::test]
    #[ignore = "live: cargo xtask vm e2e"]
    async fn a_guest_worker_greets_as_macos_with_its_grants_and_runs_a_shell() {
        let addr: SocketAddr =
            var("SLOPTY_VM_WORKER").parse().expect("SLOPTY_VM_WORKER is ip:port");
        let major = var("SLOPTY_VM_MACOS");

        let endpoint = bind_client().unwrap();
        let hello = Hello { client: ClientId::new(), name: "vm e2e".to_owned() };
        let conn = tokio::time::timeout(STEP, connect_addr(&endpoint, addr, hello))
            .await
            .expect("connected in time")
            .expect("the guest's worker admits this Mac");

        // 1. The greeting says macOS at the guest's version, on the guest's display, and the grants
        //    the worker holds there from launchd: it can capture and post events.
        let ack = conn.ack.clone();
        let caps = &ack.caps;
        assert_eq!(caps.os, Os::MacOs, "{caps:?}");
        assert!(caps.os_version.starts_with(&major), "macOS {major}: {caps:?}");
        assert_eq!(caps.arch, "aarch64", "{caps:?}");
        assert!(!caps.displays.is_empty(), "the guest's display: {caps:?}");
        assert!(caps.can_inject, "Accessibility for the worker: {caps:?}");
        assert!(caps.can_capture, "Screen Recording for the worker: {caps:?}");
        assert_eq!(ack.home, "/Users/admin");

        // 2. A login shell there echoes what is typed and runs it. Only the output matches: the
        //    typed line holds `$((`.
        let mut link = WorkerLink::start(conn);
        let mut events = link.events().expect("the link's events");
        let size = TermSize { cols: 100, rows: 30, ..TermSize::default() };
        link.send(ClientMsg::OpenSession {
            request: 1,
            spec: OpenSession {
                size,
                cwd: Some("~".to_owned()),
                command: Vec::new(),
                env: Vec::new(),
                title: Some("vm e2e".to_owned()),
                attach: true,
            },
        })
        .await
        .unwrap();
        let session = tokio::time::timeout(STEP, async {
            loop {
                match events.recv().await {
                    Some(LinkEvent::Control(WorkerMsg::SessionOpened { summary, .. })) => {
                        break summary.id;
                    }
                    Some(LinkEvent::Control(WorkerMsg::Term {
                        event: TermEvent::Error(e),
                        ..
                    })) => panic!("open: {e}"),
                    Some(LinkEvent::Disconnected(why)) => panic!("disconnected: {why}"),
                    Some(_other) => {}
                    None => panic!("the link closed"),
                }
            }
        })
        .await
        .expect("SessionOpened");
        let mut state = TermState::new(size);
        let line = "echo vm-$((40+2)) $(uname -s) $(sw_vers -productVersion | cut -d. -f1)";
        link.send(ClientMsg::Term { session, req: TermRequest::Raw(line.as_bytes().to_vec()) })
            .await
            .unwrap();
        let enter = KeyEvent {
            seq: 0,
            action: KeyAction::Press,
            code: KeyCode::Enter,
            mods: Mods::empty(),
            consumed_mods: Mods::empty(),
            text: None,
            unshifted: None,
            composing: false,
            option_as_alt: false,
        };
        link.send(ClientMsg::Term { session, req: TermRequest::Key(enter) }).await.unwrap();
        until_row(&link, &mut events, &mut state, &format!("vm-42 Darwin {major}")).await.unwrap();

        let _closed = link.send(ClientMsg::Term { session, req: TermRequest::Close }).await;
        link.close();
    }
}
