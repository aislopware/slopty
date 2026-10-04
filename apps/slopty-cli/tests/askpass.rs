//! `slopty` as `ssh`'s askpass helper, and a password-only sign-in through it against a real
//! `sshd` of this user's: one password for the whole install over one shared connection, then
//! the person's key, so the machine stops asking.
//!
//! The `sshd` takes a key whose passphrase stands in for the password: no account's password
//! can be known to a test, and `ssh` asks for a passphrase through the same askpass door, once,
//! as it asks for a password. The `sshd` also offers password logins, so a refused key reads as
//! a machine that asks for one. Its sessions get `HOME` set to the test's own directory, which
//! is checked before any key is written: nothing of the person's is read or changed.

#[cfg(test)]
mod askpass_helper {
    use std::io::{Read as _, Write as _};
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::{Path, PathBuf};
    use std::process::Stdio;

    use slopty_deploy::{
        DeployError, Event, Job, Key, OnEvent, Plan, SecretString, Ssh, Target, askpass,
    };

    const SLOPTY: &str = env!("CARGO_BIN_EXE_slopty");

    /// Run `slopty` as `ssh` runs its askpass, asking `sock` with `kind`; its exit code and output.
    fn helper(sock: &Path, kind: Option<&str>) -> (Option<i32>, String) {
        let mut cmd = std::process::Command::new(SLOPTY);
        cmd.arg("me@mini's password: ")
            .env(askpass::SOCK, sock)
            .env("SSH_ASKPASS", SLOPTY)
            .env_remove(askpass::KIND);
        if let Some(kind) = kind {
            cmd.env(askpass::KIND, kind);
        }
        let out = cmd.stdin(Stdio::null()).output().unwrap();
        (out.status.code(), String::from_utf8(out.stdout).unwrap())
    }

    /// The helper prints what the socket answers, with the line's end `ssh` reads to; it prints
    /// nothing and exits 1 when the socket refuses, and when there is no socket at all.
    #[test]
    fn askpass_prints_the_answer_and_fails_without_one() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("a");
        let listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
        let serving = std::thread::spawn(move || {
            let mut asked = Vec::new();
            for answer in [&b"hunter2"[..], b""] {
                let (mut stream, _) = listener.accept().unwrap();
                let mut question = String::new();
                stream.read_to_string(&mut question).unwrap();
                asked.push(question);
                stream.write_all(answer).unwrap();
            }
            asked
        });
        assert_eq!(helper(&sock, None), (Some(0), "hunter2\n".to_owned()));
        assert_eq!(helper(&sock, Some("confirm")), (Some(1), String::new()));
        let asked = serving.join().unwrap();
        assert_eq!(asked, ["\nme@mini's password: ", "confirm\nme@mini's password: "]);
        assert_eq!(helper(&dir.path().join("gone"), None), (Some(1), String::new()));
    }

    /// A real `sshd` of this user's on a loopback port that takes one key, whose passphrase is the
    /// test's "password", and offers password logins besides.
    struct Sshd {
        dir: tempfile::TempDir,
        port: u16,
        _daemon: tokio::process::Child,
    }

    /// The passphrase the test types, unlike any other string it holds.
    const PASSWORD: &str = "slopty-test-passphrase-7f3a";

    impl Sshd {
        const PROGRAM: &str = "/usr/sbin/sshd";

        async fn start() -> Option<Self> {
            if cfg!(not(target_os = "macos")) && !Path::new(Self::PROGRAM).exists() {
                eprintln!("skipped: no {}", Self::PROGRAM);
                return None;
            }
            let dir = tempfile::tempdir().unwrap();
            let at = |name: &str| dir.path().join(name);
            keygen(&["-q", "-t", "ed25519", "-N", "", "-C", "host", "-f"], &at("host")).await;
            keygen(&["-q", "-t", "ed25519", "-N", PASSWORD, "-C", "client", "-f"], &at("client"))
                .await;
            keygen(&["-q", "-t", "ed25519", "-N", "", "-C", "person", "-f"], &at("person")).await;
            std::fs::create_dir_all(at("home")).unwrap();
            std::fs::copy(at("client.pub"), at("authorized_keys")).unwrap();
            let port =
                std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
            let config = format!(
                "Port {port}\nListenAddress 127.0.0.1\nHostKey {host}\nAuthorizedKeysFile {keys}\n\
                 PasswordAuthentication yes\nKbdInteractiveAuthentication no\nUsePAM no\n\
                 StrictModes no\nPidFile {pid}\nSetEnv HOME={home}\nLogLevel VERBOSE\n",
                host = at("host").display(),
                keys = at("authorized_keys").display(),
                pid = at("sshd.pid").display(),
                home = at("home").display(),
            );
            std::fs::write(at("sshd_config"), &config).unwrap();
            // OpenSSH 9.8 penalises a source after failed logins, which would refuse the wrong
            // password test's second try; older ones (Ubuntu 24.04's 9.6) reject the option.
            let penalties = tokio::process::Command::new(Self::PROGRAM)
                .args(["-t", "-o", "PerSourcePenalties=no", "-f"])
                .arg(at("sshd_config"))
                .output()
                .await
                .unwrap();
            if penalties.status.success() {
                std::fs::write(at("sshd_config"), format!("{config}PerSourcePenalties no\n"))
                    .unwrap();
            }
            let sshd = tokio::process::Command::new(Self::PROGRAM)
                .arg("-D")
                .arg("-f")
                .arg(at("sshd_config"))
                .arg("-E")
                .arg(at("sshd.log"))
                .stdin(Stdio::null())
                .kill_on_drop(true)
                .spawn()
                .unwrap();
            let deadline = tokio::time::Instant::now()
                .checked_add(std::time::Duration::from_secs(10))
                .unwrap();
            while tokio::net::TcpStream::connect(("127.0.0.1", port)).await.is_err() {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "sshd listens on {port}: {}",
                    std::fs::read_to_string(at("sshd.log")).unwrap_or_default()
                );
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            Some(Self { dir, port, _daemon: sshd })
        }

        fn path(&self, name: &str) -> PathBuf {
            self.dir.path().join(name)
        }

        /// The app's `ssh`, with the test's key, known hosts and askpass, and nothing of the
        /// person's: no config, no agent.
        fn ssh(&self) -> Ssh {
            let mut ssh = Ssh::unattended(&Target {
                host: "127.0.0.1".to_owned(),
                user: None,
                port: Some(self.port),
            });
            let host = std::fs::read_to_string(self.path("host.pub")).unwrap();
            let host = host.split_whitespace().take(2).collect::<Vec<_>>().join(" ");
            std::fs::write(self.path("known_hosts"), format!("[127.0.0.1]:{} {host}\n", self.port))
                .unwrap();
            let key = self.path("client").display().to_string();
            let known = format!("UserKnownHostsFile={}", self.path("known_hosts").display());
            let mine = ["-F", "/dev/null", "-i", &key, "-o", "IdentitiesOnly=yes"];
            let hosts =
                ["-o", "IdentityAgent=none", "-o", &known, "-o", "GlobalKnownHostsFile=/dev/null"];
            ssh.options.splice(0..0, mine.into_iter().chain(hosts).map(str::to_owned));
            ssh.askpass = Some(PathBuf::from(SLOPTY));
            ssh
        }

        fn log(&self) -> String {
            std::fs::read_to_string(self.path("sshd.log")).unwrap_or_default()
        }
    }

    async fn keygen(args: &[&str], file: &Path) {
        let out =
            tokio::process::Command::new("ssh-keygen").args(args).arg(file).output().await.unwrap();
        assert!(out.status.success(), "ssh-keygen: {}", String::from_utf8_lossy(&out.stderr));
    }

    /// `script` run through `runner`: its exit status and what it printed.
    async fn sh(runner: &dyn slopty_deploy::Runner, script: &str) -> (bool, String) {
        let on: &mut OnEvent<'_> = &mut |_: Event| {};
        let ran = runner.run(Job { script, input: None, watch: false }, on).await.unwrap();
        (ran.status.success(), format!("{}{}", ran.stdout.trim(), ran.stderr.trim()))
    }

    /// A plan with nothing to send, so a deploy stops after its first step, which only reads the
    /// machine: nothing is uploaded or installed in this user's real home.
    fn reading_only(password: Option<&str>) -> (Plan, tempfile::TempDir) {
        let empty = tempfile::tempdir().unwrap();
        let plan = Plan {
            sources: vec![empty.path().to_path_buf()],
            update: false,
            server: slopty_deploy::Server { host: "studio".to_owned(), port: 45_560 },
            end_sessions: false,
            password: password.map(SecretString::from),
            add_key: false,
        };
        (plan, empty)
    }

    /// The machine's refusal of the key asks for a password, as whose. The password given goes to
    /// `ssh` once through the helper, and the deploy's steps share the one sign-in it made: the
    /// `sshd` sees one login however many steps run. The person's key goes in once, private as
    /// `ssh-copy-id` leaves it. The connection ends with the runner.
    #[tokio::test]
    async fn a_password_only_host_installs_on_one_password_and_then_takes_the_key() {
        let Some(sshd) = Sshd::start().await else { return };
        let ssh = sshd.ssh();
        let on: &mut OnEvent<'_> = &mut |_: Event| {};

        let (plan, _empty) = reading_only(None);
        let asked = slopty_deploy::deploy(&ssh, &plan, on).await.unwrap_err();
        let failure = asked.failure();
        let me = std::env::var("USER").unwrap_or_default();
        let ask = failure.password.clone().expect("the machine asks for a password");
        assert_eq!((ask.host.as_str(), ask.refused), ("127.0.0.1", false), "{failure:?}");
        if !me.is_empty() {
            assert_eq!(ask.user, me);
            assert_eq!(failure.title, format!("127.0.0.1 asks for {me}'s password"));
        }

        let (plan, _empty) = reading_only(Some(PASSWORD));
        let reached = slopty_deploy::deploy(&ssh, &plan, on).await.unwrap_err();
        assert!(matches!(reached, DeployError::Mismatch { .. }), "signed in, then read: {reached}");

        let signed = ssh.signed_in(&SecretString::from(PASSWORD)).await.unwrap();
        let home = sshd.path("home");
        let (_, said_home) = sh(&signed, "echo \"$HOME\"").await;
        assert_eq!(Path::new(&said_home), home, "the test's home, never the person's");
        for _ in 0..3 {
            assert!(sh(&signed, "uname -s").await.0);
        }
        let person = std::fs::read_to_string(sshd.path("person.pub")).unwrap();
        assert_eq!(slopty_deploy::add_key(&signed, &person, on).await, Key::Added);
        assert_eq!(slopty_deploy::add_key(&signed, &person, on).await, Key::AlreadyThere);
        let keys = home.join(".ssh").join("authorized_keys");
        assert_eq!(std::fs::read_to_string(&keys).unwrap(), person);
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!((mode(&home.join(".ssh")), mode(&keys)), (0o700, 0o600));

        let logins = |log: &str| log.matches("Accepted publickey").count();
        let log = sshd.log();
        // One for the deploy above, one for this sign-in: never one per step.
        assert_eq!(logins(&log), 2, "{log}");
        assert!(!log.contains(PASSWORD), "{log}");
        signed.end().await;
        let deadline =
            tokio::time::Instant::now().checked_add(std::time::Duration::from_secs(10)).unwrap();
        while sshd.log().matches("Disconnected from user").count() < 2 {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the shared connection ends: {}",
                sshd.log()
            );
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }

    /// A wrong password is refused and asks again, and is asked for once: the helper answers no
    /// second question, so `ssh` does not try the password on the machine's own password login.
    /// The password is in no log `sshd` or `ssh` wrote and in no error.
    #[tokio::test]
    async fn a_wrong_password_says_so_and_asks_again() {
        let Some(sshd) = Sshd::start().await else { return };
        let ssh = sshd.ssh();
        let wrong = "slopty-test-wrong-2b9c";
        let refused = ssh.signed_in(&SecretString::from(wrong)).await.unwrap_err();
        let failure = refused.failure();
        assert_eq!(failure.title, "127.0.0.1 did not take that password");
        assert!(failure.password.as_ref().is_some_and(|ask| ask.refused));
        let log = sshd.log();
        assert!(!log.contains("Accepted publickey"), "{log}");
        assert!(!log.contains("Failed password"), "the password never went to the login: {log}");
        for said in [log, refused.to_string(), format!("{refused:?}"), format!("{failure:?}")] {
            assert!(!said.contains(wrong) && !said.contains(PASSWORD), "{said}");
        }
    }
}
