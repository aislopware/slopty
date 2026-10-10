//! A deploy as the app makes one, into a fresh macOS guest that has never had a worker:
//! `Ssh::unattended` over the system `ssh`, which never asks, against a machine whose host key
//! this Mac does not know yet. The key is offered by its fingerprint, which must be the one the
//! guest's own key file has (read by `cargo xtask` through the guest agent, not over SSH), then
//! trusted; the deploy then installs this build's worker, which answers from this Mac.
//!
//! Live (`#[ignore]`), run by `cargo xtask vm deploy --app`, which clones and boots the guest,
//! admits this Mac's address in its `[network] allow` and says where everything is:
//! `SLOPTY_VM_GUEST` (its address), `SLOPTY_VM_USER`, `SLOPTY_VM_KEY` (the key it trusts),
//! `SLOPTY_VM_BINS` (this tree's worker, built for this Mac and so for the guest) and
//! `SLOPTY_VM_HOST_KEY` (the fingerprint of its `ssh_host_ed25519_key`). `ssh` reads no config
//! and no agent, and a known-hosts file of the test's own: nothing of the person's is read,
//! written or asked.

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Instant;

    use slopty_deploy::{DeployError, Deployed, Event, Plan, Ssh, Step, Target};

    fn var(name: &str) -> String {
        std::env::var(name)
            .unwrap_or_else(|_| panic!("{name} unset: `cargo xtask vm deploy --app`"))
    }

    /// The app's `ssh` to the guest, with the guest's key and no config, agent or known host of
    /// this Mac's.
    fn app_ssh(known_hosts: &std::path::Path) -> Ssh {
        let target =
            Target { host: var("SLOPTY_VM_GUEST"), user: Some(var("SLOPTY_VM_USER")), port: None };
        let mut ssh = Ssh::unattended(&target);
        let isolated = [
            "-F".to_owned(),
            "/dev/null".to_owned(),
            "-i".to_owned(),
            var("SLOPTY_VM_KEY"),
            "-o".to_owned(),
            "IdentitiesOnly=yes".to_owned(),
            "-o".to_owned(),
            "IdentityAgent=none".to_owned(),
            "-o".to_owned(),
            format!("UserKnownHostsFile={}", known_hosts.display()),
            "-o".to_owned(),
            "GlobalKnownHostsFile=/dev/null".to_owned(),
        ];
        ssh.options.splice(0..0, isolated);
        ssh
    }

    async fn deploy(ssh: &Ssh, plan: &Plan) -> (Result<Deployed, DeployError>, Vec<Event>) {
        let mut events = Vec::new();
        let done = slopty_deploy::deploy(ssh, plan, &mut |event| events.push(event)).await;
        (done, events)
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "live: cargo xtask vm deploy --app"]
    async fn the_app_s_deploy_trusts_a_new_guest_as_shown_and_puts_this_build_there() {
        let dir = tempfile::tempdir().unwrap();
        let known_hosts = dir.path().join("known_hosts");
        let ssh = app_ssh(&known_hosts);
        let bins = PathBuf::from(var("SLOPTY_VM_BINS"));
        let plan = Plan {
            sources: vec![bins.clone()],
            update: true,
            // This Mac, as the guest reaches it: the worker registers with a server here.
            server: slopty_deploy::Server { host: "127.0.0.1".to_owned(), port: 45_560 },
            end_sessions: false,
            password: None,
            add_key: false,
        };

        // 1. A machine this Mac's ssh has never seen: the first step stops, nothing asked.
        let started = Instant::now();
        let (done, events) = deploy(&ssh, &plan).await;
        let stopped = started.elapsed();
        let failed = done.expect_err("a host key nobody trusted yet");
        assert!(failed.unknown_host_key(), "{failed}");
        assert_eq!(events, [Event::Step(Step::Reach)], "nothing went up");

        // 2. Its key, offered by the fingerprint the guest's own key file has.
        let started = Instant::now();
        let offer = ssh.explain(&failed).await;
        let read = started.elapsed();
        let key = *offer.trust.expect("the key, offered");
        let own = var("SLOPTY_VM_HOST_KEY");
        assert!(
            key.keys.iter().any(|k| k.sha256 == own),
            "{:?} against the guest's {own}",
            key.keys
        );
        assert_eq!(key.file, known_hosts);
        key.trust().await.unwrap();

        // 3. Trusted, the same deploy runs through: this build's worker answers there as itself.
        let started = Instant::now();
        let (done, events) = deploy(&ssh, &plan).await;
        let deployed = started.elapsed();
        let deployed_worker = done.unwrap();
        let sent = events.iter().rev().find_map(|e| match e {
            Event::Sent { total, .. } => Some(*total),
            _ => None,
        });
        assert_eq!(deployed_worker.health.caps.build, slopty_proto::wire::this_build());
        assert!(
            deployed_worker.health.exe.ends_with("/bin/slopty-worker"),
            "{:?}",
            deployed_worker.health
        );

        eprintln!(
            "guest deploy, the app's path: unknown key stopped in {stopped:.2?}, key read in \
             {read:.2?}, deploy {deployed:.2?} ({} bytes up); health {:?}",
            sent.unwrap_or_default(),
            deployed_worker.health
        );

        // 4. It answers over QUIC from this Mac, at the address ssh reached. macOS lets a process
        //    reach the local network only once the app it runs under has the Local Network grant;
        //    `ssh` is exempt as a system binary, so the deploy itself never needed it.
        let guest = var("SLOPTY_VM_GUEST");
        let probe = std::net::UdpSocket::bind("0.0.0.0:0").unwrap();
        if let Err(e) = probe.send_to(b"probe", (guest.as_str(), 9)) {
            panic!(
                "this Mac's local network is closed to this process ({e}): switch on the app that \
                 runs this terminal under System Settings, Privacy & Security, Local Network"
            );
        }
        let worker = format!("{guest}:45550");
        let started = Instant::now();
        let ping = std::process::Command::new(bins.join("slopty-probe"))
            .args(["ping", "--worker", &worker, "--count", "3"])
            .output()
            .unwrap();
        let pinged = started.elapsed();
        assert!(ping.status.success(), "{}", String::from_utf8_lossy(&ping.stderr));
        eprintln!("ping from here {pinged:.2?}: {}", String::from_utf8_lossy(&ping.stdout).trim());
    }
}
