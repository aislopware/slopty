//! Deploys through a fake `ssh` that plays the remote host in a temporary home (uploads run
//! there for real under `sh`; the remote `uname`, install and doctor answer as scripted), and
//! through a runner the test drives, for the steps, the bytes and the failures.

use std::os::unix::process::ExitStatusExt as _;

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
    assert!(Platform::of_binary(b"#!/bin/sh\n").is_empty());
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
    Plan { sources: vec![source.path().to_path_buf()], update, server: None }
}

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
            "studio sh -c '{STAGE}/slopty worker install --bin-dir {STAGE} --fresh'"
        )),
        "{scripts:#?}"
    );
    assert_eq!(deployed.platform, Platform { os: Os::MacOs, arch: Arch::Arm64 });
    assert_eq!(deployed.health, health());
    assert_eq!(deployed.server, None, "no server named, none saved");
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
}

impl Scripted {
    fn new(answer: Answer) -> Self {
        Self { answer, ran: std::sync::Arc::default(), local: false }
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
        self.ran.lock().push(job.script.to_owned());
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
}

fn mini(script: &str) -> (i32, &'static str, &'static str) {
    if script.starts_with("uname -sm") {
        (0, "Darwin arm64\n100.64.0.2 51234 100.64.0.9 22\n", "")
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
        if script.contains("worker install") {
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
    ran.iter().find(|s| s.contains("worker install")).cloned().unwrap_or_default()
}

/// The install saves the server the worker registers with, as the far side reaches it: a
/// named server as it is, and one this machine runs at loopback at the address `ssh` came
/// from there. One the far side cannot be told of installs a worker on its own, and says so.
#[tokio::test]
async fn the_worker_registers_with_the_server_as_the_machine_reaches_it() {
    let source = binaries(mac_arm64);
    let with = |host: &str| Plan {
        server: Some(Server { host: host.to_owned(), port: 45560 }),
        ..plan(&source, true)
    };
    let runner = Scripted::new(healthy);
    let (done, _) = run(&runner, &with("studio.tail1234.ts.net")).await;
    assert_eq!(done.unwrap().server.as_deref(), Some("studio.tail1234.ts.net:45560"));
    assert_eq!(
        install_script(&runner.ran()),
        format!(
            "{STAGE}/slopty --server studio.tail1234.ts.net:45560 worker install --bin-dir \
             {STAGE} --update"
        )
    );

    let runner = Scripted::new(healthy);
    let (done, _) = run(&runner, &with("127.0.0.1")).await;
    assert_eq!(done.unwrap().server.as_deref(), Some("100.64.0.2:45560"), "where ssh came from");
    assert!(install_script(&runner.ran()).contains(" --server 100.64.0.2:45560 "));

    for unreachable in ["localhost", "studio;reboot"] {
        let runner = Scripted::new(no_client);
        let (done, _) = run(&runner, &with(unreachable)).await;
        assert_eq!(done.unwrap().server, None, "{unreachable}");
        assert!(!install_script(&runner.ran()).contains("--server"), "{unreachable}");
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
        server: Some(Server { host: "127.0.0.1".to_owned(), port: 45560 }),
        ..plan(&source, true)
    };
    let (done, events) = run(&runner, &plan).await;
    let deployed = done.unwrap();
    assert_eq!(deployed.server.as_deref(), None, "loopback stays this machine's own");
    let bin = format!("\"{}\"", source.path().display());
    assert_eq!(
        runner.ran(),
        [
            "uname -sm".to_owned(),
            format!("{bin}/slopty worker install --bin-dir {bin} --update"),
            format!("{bin}/slopty --json worker doctor"),
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
    assert_eq!(runner.ran(), ["uname -sm"], "nothing installed");
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
