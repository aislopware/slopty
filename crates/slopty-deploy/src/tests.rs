//! Deploys through a fake `ssh` that plays the remote host in a temporary home (uploads run
//! there for real under `sh`; the remote `uname`, install and doctor answer as scripted), and
//! through a runner the test drives, for the steps, the bytes and the failures.

use std::os::unix::process::ExitStatusExt as _;
use std::process::Stdio;

use slopty_proto::ctl::Tailscale;
use slopty_proto::server::{Os as WorkerOs, WorkerCaps};

use super::platform::{ELF_AARCH64, ELF_X86_64, MACHO_ARM64, MACHO_X86_64, platforms_of};
use super::*;

/// A Mach-O header for `arm64`, then `tag`.
fn mac_arm64(tag: &str) -> Vec<u8> {
    let mut bytes = vec![0xcf, 0xfa, 0xed, 0xfe];
    bytes.extend(MACHO_ARM64.to_le_bytes());
    bytes.extend(tag.as_bytes());
    bytes
}

/// A 64-bit little-endian ELF header for `machine`.
fn elf(machine: u16) -> Vec<u8> {
    let mut bytes = vec![0x7f, b'E', b'L', b'F', 2, 1, 1, 0];
    bytes.resize(18, 0);
    bytes.extend(machine.to_le_bytes());
    bytes
}

#[test]
fn a_binary_says_what_it_runs_on() {
    let arm = Platform { os: Os::MacOs, arch: Arch::Arm64 };
    assert_eq!(Platform::of_binary(&mac_arm64("x")), [arm]);
    let mut fat = vec![0xca, 0xfe, 0xba, 0xbe, 0, 0, 0, 2];
    for cpu in [MACHO_X86_64, MACHO_ARM64] {
        fat.extend(cpu.to_be_bytes());
        fat.extend([0; 16]);
    }
    let intel = Platform { os: Os::MacOs, arch: Arch::X86_64 };
    assert_eq!(Platform::of_binary(&fat), [intel, arm], "each slice of a universal binary");
    assert_eq!(
        Platform::of_binary(&elf(ELF_X86_64)),
        [Platform { os: Os::Linux, arch: Arch::X86_64 }]
    );
    assert_eq!(
        Platform::of_binary(&elf(ELF_AARCH64)),
        [Platform { os: Os::Linux, arch: Arch::Arm64 }]
    );
    assert_eq!(Platform::of_binary(b"#!/bin/sh\n"), []);
    assert!(Platform::of_binary(&[0xcf, 0xfa]).is_empty(), "a short file");

    let this = platforms_of(&std::env::current_exe().unwrap()).unwrap();
    let here = if cfg!(target_os = "macos") { Os::MacOs } else { Os::Linux };
    assert!(this.iter().any(|p| p.os == here), "this very test binary: {this:?}");
}

#[test]
fn uname_names_the_machine() {
    let mac = Platform::from_uname("Darwin arm64\n").unwrap();
    assert_eq!(mac.to_string(), "macOS arm64");
    assert_eq!(
        Platform::from_uname("Linux aarch64").unwrap(),
        Platform { os: Os::Linux, arch: Arch::Arm64 }
    );
    assert_eq!(Platform::from_uname("Linux x86_64").unwrap().arch, Arch::X86_64);
    assert_eq!(
        Platform::from_uname("FreeBSD amd64").unwrap_err().to_string(),
        "no worker runs on FreeBSD"
    );
    assert_eq!(
        Platform::from_uname("Linux riscv64").unwrap_err().to_string(),
        "no worker is built for a riscv64 CPU"
    );
}

/// The fake remote: its home, its log of scripts, and the `ssh` that reaches it.
struct Host {
    dir: tempfile::TempDir,
    ssh: PathBuf,
}

impl Host {
    /// A host that is `uname` and whose install prints two lines and exits `install_exit`, its
    /// doctor answering `health`.
    fn new(uname: &str, install_exit: u8, health: &Health) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let (ssh, log) = (dir.path().join("ssh"), dir.path().join("log"));
        let doctor = serde_json::to_string(health).unwrap();
        let script = format!(
            "#!/bin/sh\nprintf '%s %s\\n' \"$1\" \"$2\" >> '{log}'\ncd '{home}' || exit 1\n\
             case $2 in\n\
             *'uname -sm'*) echo '{uname}' ;;\n\
             *'--plan'*) echo '{KEPT}' ;;\n\
             *'worker service'*) echo '{REPORT}' ;;\n\
             *'worker install'*) echo 'installed (fake)'; echo 'worker up' >&2; \
             exit {install_exit} ;;\n\
             *'worker doctor'*) printf '%s\\n' '{doctor}' ;;\n\
             *) eval \"$2\" ;;\n\
             esac\n",
            log = log.display(),
            home = home.display(),
        );
        std::fs::write(&ssh, script).unwrap();
        std::fs::set_permissions(&ssh, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        Self { dir, ssh }
    }

    fn ssh(&self, echo: Echo) -> Ssh {
        Ssh { echo, ..Ssh::new(self.ssh.clone(), "studio".to_owned()) }
    }

    fn scripts(&self) -> Vec<String> {
        let log = std::fs::read_to_string(self.dir.path().join("log")).unwrap_or_default();
        log.lines().map(str::to_owned).collect()
    }

    fn staged(&self, name: &str) -> PathBuf {
        self.dir.path().join("home").join(STAGE).join(name)
    }
}

fn health() -> Health {
    Health {
        worker: slopty_core::WorkerId::nil(),
        server: None,
        version: env!("CARGO_PKG_VERSION").to_owned(),
        exe: "/Users/me/Library/Application Support/Slopty/bin/slopty-worker".to_owned(),
        caps: WorkerCaps {
            can_capture: false,
            can_inject: true,
            ..WorkerCaps::bare(WorkerOs::MacOs)
        },
        listen: "[::]:45550".to_owned(),
        allow: Vec::new(),
        tailscale: Tailscale::Up { node: "studio.tail1234.ts.net".to_owned(), ip: None },
        pasteboard: slopty_proto::ctl::PasteboardAccess::Allowed,
        clients: 0,
        sessions: 0,
        uptime_secs: 1,
    }
}

/// The three binaries built for `head`, each with its name after the header.
fn binaries(head: impl Fn(&str) -> Vec<u8>) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for name in WORKER_BINARIES {
        std::fs::write(dir.path().join(name), head(name)).unwrap();
    }
    dir
}

fn plan(source: &tempfile::TempDir, update: bool) -> Plan {
    Plan {
        sources: vec![source.path().to_path_buf()],
        update,
        server: Server { host: "studio.tail1234.ts.net".to_owned(), port: 45560 },
        end_sessions: false,
        password: None,
        add_key: false,
    }
}

/// What the machine says an update does to its ptyd: keeps it.
const KEPT: &str = r#"{"ptyd":"kept"}"#;
/// What it says its services are: both running, outliving the person's logout.
const REPORT: &str = r#"{"ptyd":{"running":700},"worker":{"running":701},"stops_at_logout":null}"#;

/// Runs `deploy`, keeping what it said.
async fn run(runner: &dyn Runner, plan: &Plan) -> (Result<Deployed, DeployError>, Vec<Event>) {
    let mut events = Vec::new();
    let done = deploy(runner, plan, &mut |e| events.push(e)).await;
    (done, events)
}

#[tokio::test]
async fn a_deploy_uploads_the_matching_binaries_installs_and_reads_the_doctor() {
    let host = Host::new("Darwin arm64", 0, &health());
    let source = binaries(mac_arm64);
    let (deployed, events) = run(&host.ssh(Echo::Terminal), &plan(&source, false)).await;
    let deployed = deployed.unwrap();

    for name in WORKER_BINARIES {
        let staged = host.staged(name);
        assert_eq!(std::fs::read(&staged).unwrap(), mac_arm64(name), "{name} arrived whole");
        let mode =
            std::os::unix::fs::PermissionsExt::mode(&staged.metadata().unwrap().permissions());
        assert_eq!(mode & 0o777, 0o755, "{name} is executable");
        assert!(!host.staged(&format!("{name}.part")).exists());
    }
    let scripts = host.scripts();
    assert_eq!(
        scripts.first().map(String::as_str),
        Some(format!("studio sh -c '{REACH}'").as_str())
    );
    assert!(
        scripts.contains(&format!(
            "studio sh -c '{STAGE}/slopty --server studio.tail1234.ts.net:45560 worker install \
             --bin-dir {STAGE} --fresh'"
        )),
        "{scripts:#?}"
    );
    assert_eq!(deployed.platform, Platform { os: Os::MacOs, arch: Arch::Arm64 });
    assert_eq!(deployed.health, health());
    assert_eq!(deployed.server, "studio.tail1234.ts.net:45560", "the server named, saved");
    let total: u64 = WORKER_BINARIES.iter().map(|n| mac_arm64(n).len() as u64).sum();
    assert_eq!(events.last(), Some(&Event::Step(Step::Check)));
    assert!(events.contains(&Event::Sent { sent: total, total }), "every byte: {events:?}");
    assert!(!events.iter().any(|e| matches!(e, Event::Line(_))), "the terminal showed them");
}

/// In the app the install's lines come back as events, both of its streams.
#[tokio::test]
async fn a_watched_install_comes_back_as_lines() {
    let host = Host::new("Darwin arm64", 0, &health());
    let source = binaries(mac_arm64);
    let (deployed, events) = run(&host.ssh(Echo::Lines), &plan(&source, false)).await;
    deployed.unwrap();
    let mut lines: Vec<&str> = events
        .iter()
        .filter_map(|e| if let Event::Line(l) = e { Some(l.as_str()) } else { None })
        .collect();
    lines.sort_unstable();
    assert_eq!(lines, ["installed (fake)", "worker up"]);
}

#[tokio::test]
async fn an_update_asks_the_remote_install_to_update_and_its_failure_fails_the_deploy() {
    let host = Host::new("Darwin arm64", 1, &health());
    let source = binaries(mac_arm64);
    let (failed, _) = run(&host.ssh(Echo::Terminal), &plan(&source, true)).await;
    let failed = failed.unwrap_err();
    assert!(
        failed.to_string().ends_with("on studio failed (exit status: 1); see above"),
        "{failed}"
    );
    let scripts = host.scripts();
    assert!(scripts.iter().any(|s| s.ends_with("--update'")), "{scripts:#?}");
    assert!(!scripts.iter().any(|s| s.contains("worker doctor")), "no doctor after a failure");

    let (failed, _) = run(&host.ssh(Echo::Lines), &plan(&source, true)).await;
    let failure = failed.unwrap_err().failure();
    assert_eq!(failure.title, "The install on studio failed");
    assert_eq!(failure.lines.len(), 2, "its lines: {failure:?}");
}

#[tokio::test]
async fn binaries_built_for_another_machine_are_refused_before_anything_moves() {
    let host = Host::new("Linux x86_64", 0, &health());
    let source = binaries(mac_arm64);
    let (refused, _) = run(&host.ssh(Echo::Terminal), &plan(&source, false)).await;
    let refused = refused.unwrap_err();
    let said = refused.to_string();
    assert!(said.contains("is built for macOS arm64, and studio is Linux x86_64"), "{said}");
    assert!(said.contains("--bin-dir"), "{said}");
    assert_eq!(refused.failure().title, "This build has no worker for Linux x86_64");
    assert_eq!(host.scripts().len(), 1, "only uname ran: {:?}", host.scripts());
    assert!(!host.staged("slopty").exists());

    let linux = binaries(|_name| elf(ELF_X86_64));
    let host = Host::new("Linux x86_64", 0, &health());
    run(&host.ssh(Echo::Terminal), &plan(&linux, false)).await.0.unwrap();
    assert!(host.staged("slopty-worker").exists(), "a build for the machine goes up");

    let unknown = Host::new("SunOS sparc", 0, &health());
    let (refused, _) = run(&unknown.ssh(Echo::Terminal), &plan(&source, false)).await;
    let refused = refused.unwrap_err();
    assert_eq!(refused.to_string(), "on studio");
    assert_eq!(refused.failure().title, "No worker runs on studio");
    let dashed = Ssh::new(host.ssh.clone(), "-oProxyCommand=x".to_owned());
    let (dashed, events) = run(&dashed, &plan(&source, false)).await;
    assert!(matches!(dashed, Err(DeployError::NotATarget(_))), "not an option in disguise");
    assert!(events.is_empty(), "nothing ran");
}

/// A machine the test plays: each script answers from `answer`, and what ran is kept.
/// How the test's machine answers a script: exit code, output, error output.
type Answer = fn(&str) -> (i32, &'static str, &'static str);

#[derive(Debug)]
struct Scripted {
    answer: Answer,
    ran: std::sync::Arc<parking_lot::Mutex<Vec<String>>>,
    local: bool,
    /// The runner a sign-in gave: what it runs is kept as `signed: <script>`.
    signed: bool,
}

impl Scripted {
    fn new(answer: Answer) -> Self {
        Self { answer, ran: std::sync::Arc::default(), local: false, signed: false }
    }

    fn ran(&self) -> Vec<String> {
        self.ran.lock().clone()
    }
}

impl Runner for Scripted {
    fn target(&self) -> &'static str {
        "mini"
    }

    fn is_local(&self) -> bool {
        self.local
    }

    fn program(&self) -> String {
        "ssh".to_owned()
    }

    fn run<'a>(
        &'a self,
        job: Job<'a>,
        on: &'a mut OnEvent<'_>,
    ) -> Pending<'a, std::io::Result<Ran>> {
        let said =
            if self.signed { format!("signed: {}", job.script) } else { job.script.to_owned() };
        self.ran.lock().push(said);
        let (code, stdout, stderr) = (self.answer)(job.script);
        if let Some((_file, len)) = &job.input {
            on(Event::Sent { sent: len / 2, total: *len });
            on(Event::Sent { sent: *len, total: *len });
        }
        if job.watch {
            for line in stdout.lines() {
                on(Event::Line(line.to_owned()));
            }
        }
        let status = ExitStatus::from_raw(code << 8);
        Box::pin(std::future::ready(Ok(Ran {
            status,
            stdout: stdout.to_owned(),
            stderr: stderr.to_owned(),
        })))
    }

    fn sign_in<'a>(
        &'a self,
        password: &'a SecretString,
    ) -> Pending<'a, Result<Option<Box<dyn Runner>>, DeployError>> {
        self.ran.lock().push(format!("sign in ({} characters)", password.expose_secret().len()));
        let signed = Self {
            answer: self.answer,
            ran: std::sync::Arc::clone(&self.ran),
            local: self.local,
            signed: true,
        };
        let signed: Box<dyn Runner> = Box::new(signed);
        Box::pin(std::future::ready(Ok(Some(signed))))
    }
}

fn mini(script: &str) -> (i32, &'static str, &'static str) {
    if script.starts_with("uname -sm") {
        (0, "Darwin arm64\n100.64.0.2 51234 100.64.0.9 22\n", "")
    } else if script.contains("--plan") {
        (0, KEPT, "")
    } else if script.contains("worker service") {
        (0, REPORT, "")
    } else if script.contains("worker install") {
        (0, "installing\nup\n", "")
    } else if script.contains("worker doctor") {
        (0, "{not json", "")
    } else {
        (0, "", "")
    }
}

/// The steps come in order, each upload's bytes counted among all of them, and a doctor that
/// is not a report fails the check with what it said.
#[tokio::test]
async fn the_steps_come_in_order_and_the_bytes_add_up() {
    let runner = Scripted::new(mini);
    let source = binaries(mac_arm64);
    let (done, events) = run(&runner, &plan(&source, true)).await;
    let lens: Vec<u64> = WORKER_BINARIES.iter().map(|n| mac_arm64(n).len() as u64).collect();
    let total: u64 = lens.iter().sum();
    let first = lens.first().copied().unwrap();
    let expected = [
        Event::Step(Step::Reach),
        Event::Machine(Platform { os: Os::MacOs, arch: Arch::Arm64 }),
        Event::Step(Step::Upload { name: "slopty-ptyd" }),
        Event::Sent { sent: first / 2, total },
        Event::Sent { sent: first, total },
    ];
    assert_eq!(events.get(..5), Some(&expected[..]));
    assert!(events.contains(&Event::Sent { sent: total, total }));
    let tail: Vec<&Event> = events.iter().rev().take(4).collect();
    assert_eq!(
        tail,
        [
            &Event::Step(Step::Check),
            &Event::Line("up".to_owned()),
            &Event::Line("installing".to_owned()),
            &Event::Step(Step::Install),
        ]
    );
    let ran = runner.ran.lock().clone();
    assert!(ran.iter().any(|s| s.ends_with("--update")), "{ran:?}");
    let failed = done.unwrap_err();
    assert!(failed.to_string().starts_with("the worker's doctor said \"{not json\""), "{failed}");
    let failure = failed.failure();
    assert_eq!(failure.title, "The new worker did not report back");
    assert_eq!(failure.lines, ["{not json"]);
}

/// `ssh`'s own failure (255) is named by what it printed, with what to do about it; the
/// command's own failure is not taken for ssh's.
#[tokio::test]
async fn ssh_failures_say_what_went_wrong_and_what_to_do() {
    fn upload_fails(script: &str) -> (i32, &'static str, &'static str) {
        if script.contains("cat >") {
            (1, "", "mkdir: .slopty: Permission denied\n")
        } else {
            mini(script)
        }
    }
    let cases: [(Answer, &str, bool); 6] = [
        (
            |_| (255, "", "Host key verification failed.\r\n"),
            "mini's host key is not trusted yet",
            true,
        ),
        (
            |_| (255, "", "me@mini: Permission denied (publickey).\n"),
            "mini did not accept your SSH key",
            true,
        ),
        (
            |_| (255, "", "ssh: Could not resolve hostname mini: nodename nor servname\n"),
            "No machine is named mini",
            true,
        ),
        (
            |_| (255, "", "ssh: connect to host mini port 22: Connection refused\n"),
            "mini does not accept SSH",
            true,
        ),
        (
            |_| (255, "", "ssh: connect to host mini port 22: Operation timed out\n"),
            "mini did not answer",
            true,
        ),
        (|_| (1, "", "sh: uname: not found\n"), "A step failed on mini", false),
    ];
    let source = binaries(mac_arm64);
    for (answer, title, hinted) in cases {
        let (failed, _) = run(&Scripted::new(answer), &plan(&source, false)).await;
        let failure = failed.unwrap_err().failure();
        assert_eq!(failure.title, title);
        assert_eq!(failure.hint.is_some(), hinted, "{failure:?}");
        assert_eq!(failure.lines.len(), 1, "the line it printed: {failure:?}");
    }

    let (failed, _) = run(&Scripted::new(upload_fails), &plan(&source, false)).await;
    let failed = failed.unwrap_err();
    assert_eq!(
        failed.to_string(),
        "uploading slopty-ptyd to mini failed (exit status: 1): mkdir: .slopty: Permission denied"
    );
    assert_eq!(failed.failure().title, "Could not copy slopty-ptyd to mini");
}

/// A failed install keeps its last lines, no more than [`TAIL`] of them, and says the last.
#[tokio::test]
async fn a_failed_install_keeps_its_last_lines() {
    fn chatty(script: &str) -> (i32, &'static str, &'static str) {
        if script.contains("worker install") && !script.contains("--plan") {
            (
                1,
                "1\n2\n3\n4\n5\n6\n7\n8\n9\nthe new worker did not come up; the previous one is back\n",
                "",
            )
        } else {
            mini(script)
        }
    }
    let source = binaries(mac_arm64);
    let (failed, _) = run(&Scripted::new(chatty), &plan(&source, true)).await;
    let failed = failed.unwrap_err();
    assert!(
        failed.to_string().ends_with(": the new worker did not come up; the previous one is back")
    );
    let failure = failed.failure();
    assert_eq!(failure.lines.len(), TAIL);
    assert_eq!(failure.lines.first().map(String::as_str), Some("3"));
}

/// The doctor's report as JSON, for a scripted machine that answers it.
static HEALTH: std::sync::LazyLock<String> =
    std::sync::LazyLock::new(|| serde_json::to_string(&health()).unwrap());

/// A machine that reaches this one from 100.64.0.2 and installs and answers as it should.
fn healthy(script: &str) -> (i32, &'static str, &'static str) {
    if script.contains("worker doctor") { (0, HEALTH.as_str(), "") } else { mini(script) }
}

/// A healthy machine whose login is not `sshd`'s, so it says no address it was reached from.
fn no_client(script: &str) -> (i32, &'static str, &'static str) {
    if script == REACH { (0, "Darwin arm64\n\n", "") } else { healthy(script) }
}

/// The install script a run ran.
fn install_script(ran: &[String]) -> String {
    let install = |s: &&String| s.contains("worker install") && !s.contains("--plan");
    ran.iter().find(install).cloned().unwrap_or_default()
}

/// The install saves the server the worker registers with, as the far side reaches it: a
/// named server as it is, and one this machine runs at loopback at the address `ssh` came
/// from there. One the far side cannot be told of stops the deploy before anything is sent.
#[tokio::test]
async fn the_worker_registers_with_the_server_as_the_machine_reaches_it() {
    let source = binaries(mac_arm64);
    let with = |host: &str| Plan {
        server: Server { host: host.to_owned(), port: 45560 },
        ..plan(&source, true)
    };
    let runner = Scripted::new(healthy);
    let (done, _) = run(&runner, &with("studio.tail1234.ts.net")).await;
    assert_eq!(done.unwrap().server, "studio.tail1234.ts.net:45560");
    assert_eq!(
        install_script(&runner.ran()),
        format!(
            "{STAGE}/slopty --server studio.tail1234.ts.net:45560 worker install --bin-dir \
             {STAGE} --update"
        )
    );

    let runner = Scripted::new(healthy);
    let (done, _) = run(&runner, &with("127.0.0.1")).await;
    assert_eq!(done.unwrap().server, "100.64.0.2:45560", "where ssh came from");
    assert!(install_script(&runner.ran()).contains(" --server 100.64.0.2:45560 "));

    for unreachable in ["localhost", "studio;reboot"] {
        let runner = Scripted::new(no_client);
        let (done, _) = run(&runner, &with(unreachable)).await;
        let refused = done.unwrap_err();
        assert!(
            matches!(&refused, DeployError::NoServerAddress { server, .. } if server.starts_with(unreachable)),
            "{unreachable}: {refused}"
        );
        assert_eq!(install_script(&runner.ran()), "", "nothing installed: {unreachable}");
        assert!(!runner.ran().iter().any(|s| s.contains("cat >")), "nothing sent: {unreachable}");
    }
}

#[test]
fn a_server_is_named_as_the_far_side_dials_it() {
    let at = |host: &str| Server { host: host.to_owned(), port: 45560 };
    assert_eq!(at("studio").seen_from(Some("10.0.0.2")).as_deref(), Some("studio:45560"));
    assert_eq!(
        at("::1").seen_from(Some("fd7a:115c:a1e0::2")).as_deref(),
        Some("[fd7a:115c:a1e0::2]:45560")
    );
    assert_eq!(at("[::1]").seen_from(None), None, "loopback, and nothing said where from");
    assert_eq!(
        at("127.0.0.1").seen_from(Some("127.0.0.1")).as_deref(),
        None,
        "the far side is this machine"
    );
    assert_eq!(at("$(id)").seen_from(None), None, "nothing a shell would run");
    assert_eq!(at("::1").address().as_deref(), Some("[::1]:45560"), "this machine's own");
    assert_eq!(at("127.0.0.1").address().as_deref(), Some("127.0.0.1:45560"));
    assert_eq!(at("$(id)").address(), None);
}

/// A target names this machine when it is loopback or one of its own addresses, with the
/// config's user and port, and the fields read as `ssh` takes them.
#[tokio::test]
async fn a_target_reads_as_ssh_takes_it() {
    for here in ["127.0.0.1", "localhost", "::1", "[::1]"] {
        assert!(Target::host(here).is_this_machine().await, "{here}");
    }
    // The address this machine would send from: one of its own, when it has a route at all.
    let own = std::net::UdpSocket::bind("0.0.0.0:0").unwrap();
    if own.connect("192.0.2.1:9").is_ok() {
        let ip = own.local_addr().unwrap().ip();
        assert!(Target::host(&ip.to_string()).is_this_machine().await, "this machine's {ip}");
    }
    assert!(!Target::host("192.0.2.1").is_this_machine().await, "TEST-NET-1 is nobody's own");
    assert!(!Target::host("no-such-host.invalid").is_this_machine().await);
    let with_port = Target { port: Some(2222), ..Target::host("127.0.0.1") };
    assert!(!with_port.is_this_machine().await, "a port of its own is an ssh to make");
    assert_eq!(
        Target::read(" me@mini ", "", ""),
        Ok(Target { host: "mini".to_owned(), user: Some("me".to_owned()), port: None })
    );
    Target::read("-oProxyCommand=x", "", "").unwrap_err();
    Target::read("mini", "", "0").unwrap_err();
}

/// Each worker's target is kept under the data dir, read back by a later open, and a file
/// that is not one reads as none kept.
#[test]
fn targets_are_remembered_per_worker() {
    let dir = tempfile::tempdir().unwrap();
    let mut kept = Remembered::open_in(dir.path());
    assert_eq!(kept.get("w1"), None);
    let mini = Target { user: Some("me".to_owned()), port: Some(2222), ..Target::host("mini") };
    kept.remember("w1", mini.clone()).unwrap();
    kept.remember("w2", Target::host("studio")).unwrap();
    let again = Remembered::open_in(dir.path());
    assert_eq!(again.get("w1"), Some(&mini));
    assert_eq!(again.get("w2"), Some(&Target::host("studio")));
    std::fs::write(dir.path().join(REMEMBERED), b"{not json").unwrap();
    assert_eq!(Remembered::open_in(dir.path()).get("w1"), None);
}

/// On this machine nothing is uploaded: the install and the doctor run the binaries where they
/// are, quoted, and a directory a script cannot name is refused before anything runs.
#[tokio::test]
async fn a_local_deploy_installs_in_place() {
    let source = binaries(mac_arm64);
    let runner = Scripted { local: true, ..Scripted::new(healthy) };
    let plan = Plan {
        server: Server { host: "127.0.0.1".to_owned(), port: 45560 },
        ..plan(&source, true)
    };
    let (done, events) = run(&runner, &plan).await;
    let deployed = done.unwrap();
    assert_eq!(deployed.server, "127.0.0.1:45560", "loopback stays this machine's own");
    let bin = format!("\"{}\"", source.path().display());
    assert_eq!(
        runner.ran(),
        [
            REACH.to_owned(),
            format!("{bin}/slopty --json worker install --bin-dir {bin} --update --plan"),
            format!(
                "{bin}/slopty --server 127.0.0.1:45560 worker install --bin-dir {bin} --update"
            ),
            format!("{bin}/slopty --json worker doctor"),
            format!("{bin}/slopty --json worker service"),
        ]
    );
    assert!(!events.iter().any(|e| matches!(e, Event::Step(Step::Upload { .. }))), "{events:?}");

    let odd = tempfile::Builder::new().prefix("a$b").tempdir().unwrap();
    for name in WORKER_BINARIES {
        std::fs::write(odd.path().join(name), mac_arm64(name)).unwrap();
    }
    let runner = Scripted { local: true, ..Scripted::new(healthy) };
    let (done, _) = run(&runner, &Plan { sources: vec![odd.path().to_path_buf()], ..plan }).await;
    assert!(matches!(done, Err(DeployError::Path { .. })), "{done:?}");
    assert_eq!(runner.ran(), [REACH], "nothing installed");
}

/// [`Local`] runs each script under `sh` in its home and hands a watched one's lines back.
#[tokio::test]
async fn the_local_runner_runs_in_its_home() {
    let home = tempfile::tempdir().unwrap();
    let local = Local { home: home.path().to_path_buf() };
    let mut events = Vec::new();
    let mut on = |e| events.push(e);
    let job = Job { script: "pwd -P", input: None, watch: false };
    let ran = local.run(job, &mut on).await.unwrap();
    assert!(ran.status.success());
    let resolved = home.path().canonicalize().unwrap();
    assert_eq!(ran.stdout.trim(), resolved.to_str().unwrap());
    let job = Job { script: "echo one; echo two >&2; exit 3", input: None, watch: true };
    let ran = local.run(job, &mut on).await.unwrap();
    assert_eq!(ran.status.code(), Some(3));
    let mut lines: Vec<String> = events
        .into_iter()
        .filter_map(|e| if let Event::Line(l) = e { Some(l) } else { None })
        .collect();
    lines.sort();
    assert_eq!(lines, ["one", "two"]);
    assert!(local.is_local());
}

/// With a build per platform, the one that runs on the machine goes up: a Linux box gets the
/// Linux build an app carries beside its own, and a machine none fits is refused naming the
/// first.
#[tokio::test]
async fn the_build_for_the_machine_is_the_one_that_goes() {
    let mac = binaries(mac_arm64);
    let linux = binaries(|_name| elf(ELF_X86_64));
    let host = Host::new("Linux x86_64", 0, &health());
    let both = Plan {
        sources: vec![mac.path().to_path_buf(), linux.path().to_path_buf()],
        ..plan(&mac, false)
    };
    run(&host.ssh(Echo::Terminal), &both).await.0.unwrap();
    assert_eq!(std::fs::read(host.staged("slopty-worker")).unwrap(), elf(ELF_X86_64));

    let arm = Host::new("Linux aarch64", 0, &health());
    let refused = run(&arm.ssh(Echo::Terminal), &both).await.0.unwrap_err();
    assert_eq!(refused.failure().title, "This build has no worker for Linux arm64");
    assert!(refused.to_string().contains("is built for macOS arm64"), "the first named: {refused}");
}

/// An app bundle's own directory comes first, then each Linux build in its `Resources`; a dev
/// tree's Linux cross-builds of the same profile stand in for those. A directory missing a
/// binary is not offered.
#[test]
fn an_app_lists_the_builds_it_carries() {
    let root = tempfile::tempdir().unwrap();
    let fill = |dir: &Path| {
        std::fs::create_dir_all(dir).unwrap();
        for bin in WORKER_BINARIES {
            std::fs::write(dir.join(bin), b"").unwrap();
        }
    };
    let macos = root.path().join("Slopty.app/Contents/MacOS");
    let workers = root.path().join("Slopty.app/Contents/Resources").join(BUNDLED_DIR);
    fill(&macos);
    fill(&workers.join("linux-x86_64"));
    std::fs::create_dir_all(workers.join("linux-arm64")).unwrap();
    assert_eq!(bundled(&macos), [macos.clone(), workers.join("linux-x86_64")]);

    let debug = root.path().join("target/debug");
    let cross = root.path().join("target/linux/aarch64-unknown-linux-gnu/debug");
    fill(&debug);
    fill(&cross);
    assert_eq!(bundled(&debug), [debug.clone(), cross]);
    let elsewhere = root.path().join("bin");
    assert_eq!(bundled(&elsewhere), [elsewhere], "nothing beside an installed copy");
}

/// The server goes up as a worker does, then `slopty server install` runs there; it is dialled
/// at the address `ssh` reached, then as named. On this machine it installs in place and is
/// dialled at loopback.
#[tokio::test]
async fn a_server_is_put_on_a_machine_and_named_where_it_is_reached() {
    fn served(script: &str) -> (i32, &'static str, &'static str) {
        if script.contains("server install") { (0, "up\n", "") } else { mini(script) }
    }
    let source = binaries(mac_arm64);
    std::fs::write(source.path().join("slopty-server"), mac_arm64("slopty-server")).unwrap();
    let runner = Scripted::new(served);
    let mut events = Vec::new();
    let sources = [source.path().to_path_buf()];
    let done = serve(&runner, &sources, &mut |e| events.push(e)).await.unwrap();
    assert_eq!(done.addresses, ["100.64.0.9", "mini"]);
    let ran = runner.ran();
    assert!(ran.iter().any(|s| s.ends_with(&format!("{STAGE}/slopty-server"))), "{ran:?}");
    assert_eq!(ran.last().unwrap(), &format!("{STAGE}/slopty server install --bin-dir {STAGE}"));
    let uploads: Vec<&str> = events
        .iter()
        .filter_map(|e| if let Event::Step(Step::Upload { name }) = e { Some(*name) } else { None })
        .collect();
    assert_eq!(uploads, SERVER_BINARIES, "the server and its CLI, no worker");

    let local = Scripted { local: true, ..Scripted::new(served) };
    let done = serve(&local, &sources, &mut |_| {}).await.unwrap();
    assert_eq!(done.addresses, ["127.0.0.1"]);
    let bin = format!("\"{}\"", source.path().display());
    assert_eq!(
        local.ran().last().unwrap(),
        &format!("{bin}/slopty server install --bin-dir {bin}")
    );

    let workers_only = binaries(mac_arm64);
    let refused = serve(&runner, &[workers_only.path().to_path_buf()], &mut |_| {}).await;
    assert!(matches!(refused, Err(DeployError::Mismatch { built: None, .. })), "{refused:?}");
}

/// `ssh-keygen -l`'s lines are read as each key's type and fingerprint, a key named by both
/// its name and its address once.
#[test]
fn fingerprints_read_as_ssh_keygen_prints_them() {
    let said = "256 SHA256:xaI/Xz1JNrch [mini]:2222 (ED25519)\n\
                256 SHA256:xaI/Xz1JNrch 100.64.0.7 (ED25519)\n\
                3072 SHA256:Rsa0Key |1|salt=|hash= (RSA)\n\
                not a key line\n";
    let keys = Fingerprint::read(said);
    let named: Vec<(&str, &str)> =
        keys.iter().map(|k| (k.kind.as_str(), k.sha256.as_str())).collect();
    assert_eq!(named, [("ED25519", "SHA256:xaI/Xz1JNrch"), ("RSA", "SHA256:Rsa0Key")]);
    assert_eq!(keys[0].public_file(), "/etc/ssh/ssh_host_ed25519_key.pub");
}

/// A real `sshd` of this user's on a loopback port, with a host key and a client key of its
/// own; the `ssh` that reaches it reads a known-hosts file of the test's, never the person's,
/// and no config or agent of theirs.
struct Sshd {
    dir: tempfile::TempDir,
    port: u16,
    _daemon: tokio::process::Child,
}

impl Sshd {
    const PROGRAM: &str = "/usr/sbin/sshd";

    /// Started and answering, or `None` where there is no `sshd` (a Linux box without it).
    async fn start() -> Option<Self> {
        // macOS ships it, so there a missing one fails the start below.
        if cfg!(not(target_os = "macos")) && !Path::new(Self::PROGRAM).exists() {
            eprintln!("skipped: no {}", Self::PROGRAM);
            return None;
        }
        let dir = tempfile::tempdir().unwrap();
        let at = |name: &str| dir.path().join(name);
        for key in ["host", "client", "other"] {
            keygen(&["-q", "-t", "ed25519", "-N", "", "-C", key, "-f"], &at(key)).await;
        }
        std::fs::copy(at("client.pub"), at("authorized_keys")).unwrap();
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let config = format!(
            "Port {port}\nListenAddress 127.0.0.1\nHostKey {host}\nAuthorizedKeysFile {keys}\n\
             PasswordAuthentication no\nKbdInteractiveAuthentication no\nUsePAM no\n\
             StrictModes no\nPerSourcePenalties no\nPidFile {pid}\n",
            host = at("host").display(),
            keys = at("authorized_keys").display(),
            pid = at("sshd.pid").display(),
        );
        std::fs::write(at("sshd_config"), config).unwrap();
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
        let deadline =
            tokio::time::Instant::now().checked_add(std::time::Duration::from_secs(10)).unwrap();
        while tokio::net::TcpStream::connect(("127.0.0.1", port)).await.is_err() {
            assert!(tokio::time::Instant::now() < deadline, "sshd listens on {port}");
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        Some(Self { dir, port, _daemon: sshd })
    }

    fn known_hosts(&self) -> PathBuf {
        self.dir.path().join("known_hosts")
    }

    /// The app's `ssh`, with the test's own key, config and known hosts.
    fn ssh(&self) -> Ssh {
        let mut ssh = Ssh::unattended(&Target {
            host: "127.0.0.1".to_owned(),
            user: None,
            port: Some(self.port),
        });
        let key = self.dir.path().join("client");
        let known = format!("UserKnownHostsFile={}", self.known_hosts().display());
        // No config, no agent: nothing of the person's is read, and nothing can ask them.
        let key = key.display().to_string();
        let mine = ["-F", "/dev/null", "-i", &key, "-o", "IdentitiesOnly=yes"];
        let hosts =
            ["-o", "IdentityAgent=none", "-o", &known, "-o", "GlobalKnownHostsFile=/dev/null"];
        ssh.options.splice(0..0, mine.into_iter().chain(hosts).map(str::to_owned));
        ssh
    }
}

/// `ssh-keygen <args> <file>`; what it printed.
async fn keygen(args: &[&str], file: &Path) -> String {
    let out =
        tokio::process::Command::new("ssh-keygen").args(args).arg(file).output().await.unwrap();
    assert!(out.status.success(), "ssh-keygen: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap()
}

/// Against a real `sshd`: a host key `ssh` does not know stops the app's deploy at its first
/// step, which offers the key by the fingerprint the machine's own key file has, with nothing
/// trusted yet. Trusting it adds the line `ssh` wrote to the known-hosts file `ssh -G` names,
/// after a last line with no end, and the machine is reached. A key that changed is never
/// offered, and says so.
#[tokio::test]
async fn an_unknown_host_key_is_offered_by_its_fingerprint_and_trusted_as_shown() {
    let Some(sshd) = Sshd::start().await else { return };
    let ssh = sshd.ssh();
    let other = std::fs::read_to_string(sshd.dir.path().join("other.pub")).unwrap();
    let other = other.split_whitespace().take(2).collect::<Vec<_>>().join(" ");
    let elsewhere = format!("elsewhere.example {other}");
    std::fs::write(sshd.known_hosts(), &elsewhere).unwrap();
    let source = binaries(mac_arm64);

    let (done, events) = run(&ssh, &plan(&source, false)).await;
    let failed = done.unwrap_err();
    assert!(failed.unknown_host_key(), "{failed}");
    assert_eq!(events, [Event::Step(Step::Reach)], "nothing went up");
    let offer = ssh.explain(&failed).await;
    let own = Fingerprint::read(&keygen(&["-l", "-f"], &sshd.dir.path().join("host.pub")).await);
    let key = *offer.trust.clone().expect("the key, to trust");
    assert_eq!(key.keys, own, "the machine's own key");
    assert_eq!(offer.title, "127.0.0.1 is new to this Mac");
    assert_eq!(offer.lines, [format!("ED25519 {}", own[0].sha256)]);
    assert!(
        offer.hint.is_some_and(|h| h.contains("ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub"))
    );
    assert_eq!(key.file, sshd.known_hosts(), "where ssh reads known hosts for it");
    assert_eq!(std::fs::read_to_string(sshd.known_hosts()).unwrap(), elsewhere, "nothing yet");

    key.trust().await.unwrap();
    let known = std::fs::read_to_string(sshd.known_hosts()).unwrap();
    assert_eq!(known, format!("{elsewhere}\n{}\n", key.lines.trim_end()));
    let job = Job { script: "uname -sm", input: None, watch: false };
    let ran = ssh.run(job, &mut |_| {}).await.unwrap();
    assert!(ran.status.success(), "reached once trusted: {}", ran.stderr);
    assert!(Platform::from_uname(&ran.stdout).is_ok(), "{}", ran.stdout);

    let changed = format!("[127.0.0.1]:{} {other}\n", sshd.port);
    std::fs::write(sshd.known_hosts(), &changed).unwrap();
    let (done, _) = run(&ssh, &plan(&source, false)).await;
    let failed = done.unwrap_err();
    assert!(!failed.unknown_host_key(), "a changed key is not a new one");
    let said = ssh.explain(&failed).await;
    assert_eq!((said.title.as_str(), said.trust), ("127.0.0.1's host key has changed", None));
    assert_eq!(std::fs::read_to_string(sshd.known_hosts()).unwrap(), changed, "left as it was");
}

/// A machine whose new build restarts ptyd, which holds two sessions, and on which the worker
/// stops at logout.
fn restarts(script: &str) -> (i32, &'static str, &'static str) {
    if script.contains("--plan") {
        (0, r#"{"ptyd":"restarts","sessions":2}"#, "")
    } else if script.contains("worker service") {
        let report = r#"{"ptyd":{"running":700},"worker":{"running":701},
            "stops_at_logout":"it stops when you log out: run `sudo loginctl enable-linger $USER` there so it keeps running"}"#;
        (0, report, "")
    } else {
        healthy(script)
    }
}

/// An update asks the machine what it does to ptyd before installing. Kept, the install runs
/// as before and the deploy says so. Restarted while it holds sessions, the deploy stops with
/// how many, having run no install, until the plan says to end them; then the install is told
/// to, and the linger note the machine gives comes back with the worker.
#[tokio::test]
async fn an_update_that_ends_sessions_stops_until_the_person_says_so() {
    let source = binaries(mac_arm64);
    let runner = Scripted::new(healthy);
    let (done, events) = run(&runner, &plan(&source, true)).await;
    let deployed = done.unwrap();
    assert_eq!(deployed.ptyd, Some(Ptyd::Kept));
    assert_eq!(deployed.stops_at_logout, None);
    assert!(events.contains(&Event::Ptyd(Ptyd::Kept)), "{events:?}");
    let install = install_script(&runner.ran());
    assert!(install.ends_with("--update"), "no --end-sessions when nothing ends: {install}");

    let runner = Scripted::new(restarts);
    let (done, events) = run(&runner, &plan(&source, true)).await;
    let stopped = done.unwrap_err();
    assert_eq!(stopped.ends_sessions(), Some(Ptyd::Restarts { sessions: Some(2) }));
    assert_eq!(install_script(&runner.ran()), "", "no install ran: {:?}", runner.ran());
    assert!(!events.contains(&Event::Step(Step::Install)), "{events:?}");
    let failure = stopped.failure();
    assert_eq!(failure.title, "Updating ends 2 sessions on mini");
    assert!(failure.hint.as_deref().is_some_and(|h| h.contains("Try again")), "{failure:?}");
    assert!(stopped.to_string().contains("pass --end-sessions"), "{stopped}");

    let told = Plan { end_sessions: true, ..plan(&source, true) };
    let (done, _) = run(&runner, &told).await;
    let deployed = done.unwrap();
    assert!(install_script(&runner.ran()).ends_with("--update --end-sessions"));
    assert_eq!(deployed.ptyd, Some(Ptyd::Restarts { sessions: Some(2) }));
    let note = deployed.stops_at_logout.unwrap();
    assert!(note.contains("enable-linger"), "{note}");

    let fresh = Scripted::new(restarts);
    let (done, _) = run(&fresh, &plan(&source, false)).await;
    assert_eq!(done.unwrap().ptyd, None, "a fresh install asks nothing of ptyd");
    assert!(!fresh.ran().iter().any(|s| s.contains("--plan")), "{:?}", fresh.ran());
}

/// A plan the machine cannot say, and an unknown count, read as such.
#[tokio::test]
async fn a_plan_that_is_not_one_fails_and_an_uncounted_restart_ends_every_session() {
    let source = binaries(mac_arm64);
    let garbled = |script: &str| {
        if script.contains("--plan") {
            (0, "error: unexpected --plan", "")
        } else {
            healthy(script)
        }
    };
    let (done, _) = run(&Scripted::new(garbled), &plan(&source, true)).await;
    let failed = done.unwrap_err();
    assert!(matches!(failed, DeployError::Plan { .. }), "{failed}");
    assert_eq!(failed.failure().title, "The new worker could not say what it changes");
    let uncounted = |script: &str| {
        if script.contains("--plan") {
            (0, r#"{"ptyd":"restarts","sessions":null}"#, "")
        } else {
            healthy(script)
        }
    };
    let (done, _) = run(&Scripted::new(uncounted), &plan(&source, true)).await;
    let failed = done.unwrap_err();
    assert_eq!(failed.ends_sessions(), Some(Ptyd::Restarts { sessions: None }));
    assert_eq!(failed.failure().title, "Updating ends every session on mini");
}

/// A Mac at its login window with `FileVault` on says so after `uname`, and a Linux machine says
/// nothing of either. An install there that finds no login session to start Slopty in is named
/// for that, with the way to a logged-in session: automatic login, or the disk unlocked and a
/// login through Screen Sharing.
#[tokio::test]
async fn a_target_with_nobody_logged_in_says_so_and_how_to_fix_it() {
    let source = binaries(mac_arm64);
    let at_login_window = |script: &str| {
        if script.starts_with("uname -sm") {
            (0, "Darwin arm64\n100.64.0.2 51234 100.64.0.9 22\nconsole=root\nfilevault=true\n", "")
        } else {
            healthy(script)
        }
    };
    let (done, _) = run(&Scripted::new(at_login_window), &plan(&source, true)).await;
    let console = done.unwrap().console;
    assert_eq!(console, Console { logged_in: Some(false), filevault: Some(true) });
    let at_desk = |script: &str| {
        if script.starts_with("uname -sm") {
            (0, "Darwin arm64\n\nconsole=me\nfilevault=false\n", "")
        } else {
            healthy(script)
        }
    };
    let (done, _) = run(&Scripted::new(at_desk), &plan(&source, true)).await;
    assert_eq!(done.unwrap().console, Console { logged_in: Some(true), filevault: Some(false) });
    let (done, _) = run(&Scripted::new(healthy), &plan(&source, true)).await;
    assert_eq!(done.unwrap().console, Console::default(), "a machine that said nothing");

    let refused = |script: &str| {
        if script.contains("worker install") && !script.contains("--plan") {
            let said = "Error: nobody is logged in at this Mac: launchd has no login session of \
                        uid 501 to start Slopty in (Could not find domain for user gui: 501)\n";
            (1, said, "")
        } else {
            healthy(script)
        }
    };
    let (done, _) = run(&Scripted::new(refused), &plan(&source, true)).await;
    let failure = done.unwrap_err().failure();
    assert_eq!(failure.title, "Nobody is logged in at mini");
    let hint = failure.hint.unwrap();
    for way in ["automatic login", "FileVault", "unlock its disk with ssh", "Screen Sharing"] {
        assert!(hint.contains(way), "{way}: {hint}");
    }
    assert_eq!(failure.lines.len(), 1, "what the machine said is kept");
}

/// `launchctl`'s own words for a GUI domain that is not there, from an install or a step, are
/// named as nobody logged in rather than quoted as the title.
#[tokio::test]
async fn a_missing_gui_domain_is_named_not_quoted() {
    let source = binaries(mac_arm64);
    let bootstrap = |script: &str| {
        if script.contains("worker install") && !script.contains("--plan") {
            (5, "Bootstrap failed: 125: Domain does not support specified action\n", "")
        } else {
            healthy(script)
        }
    };
    let (done, _) = run(&Scripted::new(bootstrap), &plan(&source, true)).await;
    let failure = done.unwrap_err().failure();
    assert_eq!(failure.title, "Nobody is logged in at mini");
    assert_eq!(failure.lines, ["Bootstrap failed: 125: Domain does not support specified action"]);
    let plan_step = |script: &str| {
        if script.contains("--plan") {
            (1, "", "Could not find domain for user gui: 501\n")
        } else {
            healthy(script)
        }
    };
    let (done, _) = run(&Scripted::new(plan_step), &plan(&source, true)).await;
    assert_eq!(done.unwrap_err().failure().title, "Nobody is logged in at mini");
    let other = |script: &str| {
        if script.contains("worker install") && !script.contains("--plan") {
            (1, "Error: start slopty-worker: Input/output error\n", "")
        } else {
            healthy(script)
        }
    };
    let (done, _) = run(&Scripted::new(other), &plan(&source, true)).await;
    assert_eq!(done.unwrap_err().failure().title, "The install on mini failed", "only that");
}

/// A host key `ssh` could not show here points at the sheet's own trust, not at a terminal.
#[tokio::test]
async fn a_host_key_hint_points_at_the_sheet() {
    let source = binaries(mac_arm64);
    let unknown = |_: &str| (255, "", "Host key verification failed.\r\n");
    let (done, _) = run(&Scripted::new(unknown), &plan(&source, false)).await;
    let hint = done.unwrap_err().failure().hint.unwrap();
    assert!(hint.contains("Trust and install"), "{hint}");
    assert!(!hint.contains("terminal"), "{hint}");
}

/// A machine that names a password (or a keyboard-interactive login) among the ways in asks
/// for one, as whose; a key-only refusal does not. A password the sign-in gave and the machine
/// refused asks again; a sign-in that never ended says the machine did not answer.
#[tokio::test]
async fn a_host_that_takes_a_password_is_asked_for_one() {
    let source = binaries(mac_arm64);
    let password = |_: &str| (255, "", "me@mini: Permission denied (publickey,password).\r\n");
    let (done, _) = run(&Scripted::new(password), &plan(&source, false)).await;
    let failure = done.unwrap_err().failure();
    assert_eq!(failure.title, "mini asks for me's password");
    let ask = PasswordAsk { user: "me".to_owned(), host: "mini".to_owned(), refused: false };
    assert_eq!(failure.password, Some(Box::new(ask)));
    assert!(failure.hint.is_some_and(|h| h.contains("keeps nothing")));
    let typed = |_: &str| (255, "", "root@mini: Permission denied (keyboard-interactive).\n");
    let (done, _) = run(&Scripted::new(typed), &plan(&source, false)).await;
    let failure = done.unwrap_err().failure();
    assert_eq!(failure.password.map(|a| a.user), Some("root".to_owned()));
    let key_only = |_: &str| (255, "", "me@mini: Permission denied (publickey).\n");
    let (done, _) = run(&Scripted::new(key_only), &plan(&source, false)).await;
    let failure = done.unwrap_err().failure();
    assert_eq!(
        (failure.title.as_str(), failure.password),
        ("mini did not accept your SSH key", None)
    );

    let refused = DeployError::SignIn {
        target: "mini".to_owned(),
        status: Some(ExitStatus::from_raw(255 << 8)),
        stderr: "me@mini: Permission denied (publickey,password).".to_owned(),
    };
    let failure = refused.failure();
    assert_eq!(failure.title, "mini did not take that password");
    assert!(failure.password.is_some_and(|a| a.refused && a.user == "me"));
    let silent =
        DeployError::SignIn { target: "mini".to_owned(), status: None, stderr: String::new() };
    assert_eq!(silent.failure().title, "mini did not answer");
    assert_eq!(silent.failure().password, None);
}

/// With a password the deploy signs in before its first step and every step rides that
/// sign-in; with a key to add, the last step puts the person's key there, or says there is
/// none. An update stopped at its sessions says so through the failure alone.
#[tokio::test]
async fn a_password_only_host_installs_on_one_password_and_then_takes_the_key() {
    fn keyed(script: &str) -> (i32, &'static str, &'static str) {
        if script.contains("authorized_keys") { (0, "added", "") } else { healthy(script) }
    }
    let source = binaries(mac_arm64);
    let runner = Scripted::new(keyed);
    let told = Plan {
        password: Some(SecretString::from("correct horse")),
        add_key: true,
        ..plan(&source, true)
    };
    let (done, _) = run(&runner, &told).await;
    let deployed = done.unwrap();
    let ran = runner.ran();
    assert_eq!(ran.first().map(String::as_str), Some("sign in (13 characters)"), "{ran:?}");
    assert!(ran.iter().skip(1).all(|s| s.starts_with("signed: ")), "{ran:?}");
    match public_key().await {
        Some(_) => {
            assert_eq!(deployed.key, Some(Key::Added));
            assert!(ran.last().is_some_and(|s| s.contains("authorized_keys")), "{ran:?}");
        }
        None => assert_eq!(deployed.key, Some(Key::NoPublicKey)),
    }
    let no_key = Scripted::new(keyed);
    let (done, _) = run(&no_key, &Plan { add_key: false, ..told.clone() }).await;
    assert_eq!(done.unwrap().key, None, "not asked");
    assert!(!no_key.ran().iter().any(|s| s.contains("authorized_keys")));

    let (done, _) = run(&Scripted::new(restarts), &told).await;
    let failure = done.unwrap_err().failure();
    assert_eq!(failure.ends_sessions, Some(Ptyd::Restarts { sessions: Some(2) }));
}

/// The password is in no argument or environment variable of the `ssh` that signs in (only the
/// socket's path is), and no plan or error prints it; that `ssh` takes its options ahead of the
/// runner's own, which hold `BatchMode=yes`.
#[test]
fn the_password_never_reaches_argv_env_or_the_log() {
    let secret = "correct horse battery staple";
    let ssh = Ssh::unattended(&Target {
        host: "mini".to_owned(),
        user: Some("me".to_owned()),
        port: None,
    });
    let dir = Path::new("/tmp/slopty-ssh-test");
    let master = ssh.master(
        &dir.join("c"),
        Path::new("/Applications/Slopty.app/Contents/MacOS/slopty"),
        &dir.join("a"),
    );
    let master = master.as_std();
    let args: Vec<String> = master.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
    let envs: Vec<(String, String)> = master
        .get_envs()
        .map(|(k, v)| {
            (
                k.to_string_lossy().into_owned(),
                v.map(|v| v.to_string_lossy().into_owned()).unwrap_or_default(),
            )
        })
        .collect();
    assert!(!args.iter().chain(envs.iter().map(|(_, v)| v)).any(|a| a.contains(secret)));
    let named = |name: &str| envs.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str());
    assert_eq!(named(askpass::SOCK), Some("/tmp/slopty-ssh-test/a"));
    assert_eq!(named("SSH_ASKPASS_REQUIRE"), Some("force"));
    assert_eq!(named("SSH_ASKPASS"), Some("/Applications/Slopty.app/Contents/MacOS/slopty"));
    let at = |option: &str| args.iter().position(|a| a == option).unwrap();
    assert!(at("BatchMode=no") < at("BatchMode=yes"), "ssh takes the first: {args:?}");
    assert!(at("ControlMaster=yes") < at("mini"), "{args:?}");
    assert!(args.contains(&"NumberOfPasswordPrompts=1".to_owned()), "asked once: {args:?}");

    let told = Plan {
        password: Some(SecretString::from(secret)),
        add_key: true,
        ..plan(&binaries(mac_arm64), true)
    };
    assert!(!format!("{told:?}").contains(secret), "{told:?}");
}

/// A shell playing the machine in a home of the test's: `$HOME` and the working directory are
/// both that home, never the person's.
#[derive(Debug)]
struct Shell {
    home: tempfile::TempDir,
}

impl Runner for Shell {
    fn target(&self) -> &'static str {
        "mini"
    }

    fn program(&self) -> String {
        "sh".to_owned()
    }

    fn run<'a>(
        &'a self,
        job: Job<'a>,
        _on: &'a mut OnEvent<'_>,
    ) -> Pending<'a, std::io::Result<Ran>> {
        let mut sh = tokio::process::Command::new("sh");
        sh.arg("-c").arg(job.script).current_dir(self.home.path()).env("HOME", self.home.path());
        Box::pin(async move {
            let out = sh.output().await?;
            Ok(Ran {
                status: out.status,
                stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            })
        })
    }
}

/// The key goes into `~/.ssh/authorized_keys` once, as `ssh-copy-id` puts it: the directory
/// and the file made private when new, a last line with no end ended first, a key already
/// there (under another comment) left as one, and a comment a shell would read as more than
/// words dropped.
#[tokio::test]
async fn a_key_goes_in_once_as_ssh_copy_id_puts_it() {
    use std::os::unix::fs::PermissionsExt as _;
    let shell = Shell { home: tempfile::tempdir().unwrap() };
    let key = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIBlah me@studio";
    assert_eq!(add_key(&shell, key, &mut |_| {}).await, Key::Added);
    let dir = shell.home.path().join(".ssh");
    let file = dir.join("authorized_keys");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), format!("{key}\n"));
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!((mode(&dir), mode(&file)), (0o700, 0o600));
    let renamed = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIBlah other@laptop";
    assert_eq!(add_key(&shell, renamed, &mut |_| {}).await, Key::AlreadyThere);

    std::fs::write(&file, "ssh-rsa AAAAB3Nza old").unwrap();
    let odd = "ecdsa-sha2-nistp256 AAAAE2VjZHNh $(touch pwned) \"x\"";
    assert_eq!(add_key(&shell, odd, &mut |_| {}).await, Key::Added);
    let written = std::fs::read_to_string(&file).unwrap();
    assert_eq!(written, "ssh-rsa AAAAB3Nza old\necdsa-sha2-nistp256 AAAAE2VjZHNh\n");
    assert!(!shell.home.path().join("pwned").exists());
    assert_eq!(
        add_key(&shell, "-----BEGIN OPENSSH PRIVATE KEY-----", &mut |_| {}).await,
        Key::NoPublicKey
    );
}

/// The agent's first key is the one, past a line that is not a key; with none there, the newest
/// `id*.pub` beats any other `.pub`, and a private key file is never opened.
#[test]
fn the_key_is_the_agent_s_first_else_the_newest_id_pub() {
    let dir = tempfile::tempdir().unwrap();
    let listed = "The agent has no identities.\nssh-ed25519 AAAAagent card\nssh-rsa AAAAsecond\n";
    assert_eq!(choose_key(listed, dir.path()).as_deref(), Some("ssh-ed25519 AAAAagent card"));
    assert_eq!(choose_key("", dir.path()), None, "nothing to add");
    let write = |name: &str, text: &str| std::fs::write(dir.path().join(name), text).unwrap();
    write("work.pub", "ssh-ed25519 AAAAwork work\n");
    write("id_ed25519", "ssh-ed25519 AAAAprivate-looking\n");
    assert_eq!(choose_key("", dir.path()).as_deref(), Some("ssh-ed25519 AAAAwork work"));
    write("id_rsa.pub", "ssh-rsa AAAAold old\n");
    write("id_ed25519.pub", "ssh-ed25519 AAAAnew new\n");
    let aged = |name: &str, secs: u64| {
        let at = std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs);
        std::fs::File::options()
            .write(true)
            .open(dir.path().join(name))
            .unwrap()
            .set_modified(at)
            .unwrap();
    };
    aged("id_rsa.pub", 1_000);
    aged("id_ed25519.pub", 2_000);
    aged("work.pub", 3_000);
    assert_eq!(choose_key("", dir.path()).as_deref(), Some("ssh-ed25519 AAAAnew new"));
}

/// What the purge there says it removed.
const REMOVED: &str = r#"{"services":["slopty-worker","slopty-ptyd"],"paths":["/Users/me/Library/Application Support/Slopty/worker-id"],"hooks":true}"#;

/// A machine whose purge goes as it should.
fn purges(script: &str) -> (i32, &'static str, &'static str) {
    if script.contains("worker uninstall --purge") { (0, REMOVED, "") } else { mini(script) }
}

/// Runs `remove`, keeping what it said.
async fn removing(
    runner: &dyn Runner,
    source: &tempfile::TempDir,
    password: Option<&SecretString>,
) -> (Result<Removed, DeployError>, Vec<Event>) {
    let mut events = Vec::new();
    let sources = [source.path().to_path_buf()];
    let done = remove(runner, &sources, password, &mut |e| events.push(e)).await;
    (done, events)
}

/// A removal over `ssh` runs exactly three things there: `uname`, the upload of this build's CLI
/// alone, and that CLI's purge, whose word on what it removed comes back. Nothing else is run:
/// no `rm` of its own, nothing outside the stage.
#[tokio::test]
async fn a_removal_uploads_the_cli_and_runs_its_purge() {
    let source = binaries(mac_arm64);
    let runner = Scripted::new(purges);
    let (done, events) = removing(&runner, &source, None).await;
    let removed = done.unwrap();
    assert_eq!(removed.services, ["slopty-worker", "slopty-ptyd"]);
    assert!(removed.hooks);
    let part = format!("{STAGE}/slopty.part");
    assert_eq!(
        runner.ran(),
        [
            REACH.to_owned(),
            format!(
                "mkdir -p {STAGE} && cat > {part} && chmod 755 {part} && mv -f {part} \
                 {STAGE}/slopty"
            ),
            format!("{STAGE}/slopty --json worker uninstall --purge"),
        ]
    );
    let steps: Vec<&Step> =
        events.iter().filter_map(|e| if let Event::Step(s) = e { Some(s) } else { None }).collect();
    assert_eq!(steps, [&Step::Reach, &Step::Upload { name: "slopty" }, &Step::Remove]);
}

/// On this Mac the CLI runs where it is, with no upload; behind a password every step rides
/// the one sign-in.
#[tokio::test]
async fn a_local_removal_runs_in_place_and_a_password_signs_in_once() {
    let source = binaries(mac_arm64);
    let runner = Scripted { local: true, ..Scripted::new(purges) };
    let (done, events) = removing(&runner, &source, None).await;
    done.unwrap();
    let bin = format!("\"{}\"", source.path().display());
    assert_eq!(
        runner.ran(),
        [REACH.to_owned(), format!("{bin}/slopty --json worker uninstall --purge")]
    );
    assert!(!events.iter().any(|e| matches!(e, Event::Step(Step::Upload { .. }))), "{events:?}");

    let runner = Scripted::new(purges);
    let password = SecretString::from("hunter2");
    let (done, _) = removing(&runner, &source, Some(&password)).await;
    done.unwrap();
    let ran = runner.ran();
    assert_eq!(ran.first().map(String::as_str), Some("sign in (7 characters)"));
    assert!(ran.iter().skip(1).all(|s| s.starts_with("signed: ")), "{ran:#?}");
}

/// A machine no worker runs on is refused at `uname`, before anything goes up; a purge that
/// fails says so with its error output; one that says something other than a removal fails
/// with what it said.
#[tokio::test]
async fn a_removal_that_cannot_go_says_why() {
    fn bsd(script: &str) -> (i32, &'static str, &'static str) {
        if script == REACH { (0, "FreeBSD amd64\n", "") } else { purges(script) }
    }
    fn refused(script: &str) -> (i32, &'static str, &'static str) {
        if script.contains("--purge") {
            (1, "", "remove ~/.claude/settings.json: denied")
        } else {
            mini(script)
        }
    }
    fn garbled(script: &str) -> (i32, &'static str, &'static str) {
        if script.contains("--purge") { (0, "removed slopty-worker", "") } else { mini(script) }
    }
    let source = binaries(mac_arm64);

    let runner = Scripted::new(bsd);
    let (done, _) = removing(&runner, &source, None).await;
    assert!(matches!(done, Err(DeployError::Machine { .. })), "{done:?}");
    assert_eq!(runner.ran(), [REACH], "nothing went up, nothing ran");

    let (done, _) = removing(&Scripted::new(refused), &source, None).await;
    let failed = done.unwrap_err();
    assert!(matches!(failed, DeployError::Failed { .. }), "{failed:?}");
    assert!(failed.to_string().ends_with("remove ~/.claude/settings.json: denied"), "{failed}");

    let (done, _) = removing(&Scripted::new(garbled), &source, None).await;
    let failed = done.unwrap_err();
    assert!(matches!(failed, DeployError::NotRemoved { .. }), "{failed:?}");
    assert_eq!(failed.failure().title, "The uninstall did not say what it removed");
}
