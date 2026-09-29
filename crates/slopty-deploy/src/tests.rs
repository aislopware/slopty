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
    Plan { source: source.path().to_path_buf(), update }
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
    assert_eq!(scripts.first().map(String::as_str), Some("studio sh -c 'uname -sm'"));
    assert!(
        scripts.contains(&format!(
            "studio sh -c '{STAGE}/slopty worker install --bin-dir {STAGE} --fresh'"
        )),
        "{scripts:#?}"
    );
    assert_eq!(deployed.platform, Platform { os: Os::MacOs, arch: Arch::Arm64 });
    assert_eq!(deployed.health, health());
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
}

impl Scripted {
    fn new(answer: Answer) -> Self {
        Self { answer, ran: std::sync::Arc::default() }
    }
}

impl Runner for Scripted {
    fn target(&self) -> &'static str {
        "mini"
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
    if script == "uname -sm" {
        (0, "Darwin arm64\n", "")
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
