//! Deploys through a fake `ssh` that plays the remote host in a temporary home: uploads run
//! there for real under `sh`, the remote `uname`, install and doctor answer as scripted.

use slopty_proto::server::{Os as WorkerOs, WorkerCaps};

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
    Platform::from_uname("FreeBSD amd64").unwrap_err();
    Platform::from_uname("Linux riscv64").unwrap_err();
}

/// The fake remote: its home, its log of scripts, and the `ssh` that reaches it.
struct Host {
    dir: tempfile::TempDir,
    ssh: PathBuf,
}

impl Host {
    /// A host that is `uname` and whose install exits `install_exit`, its doctor answering
    /// `health`.
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
             *'worker install'*) echo 'installed (fake)'; exit {install_exit} ;;\n\
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

    fn opts(&self, update: bool) -> DeployOpts {
        DeployOpts { target: "studio".to_owned(), update, bin_dir: None, ssh: self.ssh.clone() }
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

#[tokio::test]
async fn a_deploy_uploads_the_matching_binaries_installs_and_reads_the_doctor() {
    let host = Host::new("Darwin arm64", 0, &health());
    let source = binaries(mac_arm64);
    let deployed = deploy(&host.opts(false), source.path()).await.unwrap();

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

    let said = report("studio", &deployed);
    assert!(said.contains("is up on studio (macOS arm64)"), "{said}");
    assert!(said.contains("allow Screen & System Audio Recording for"), "{said}");
    assert!(!said.contains("Accessibility"), "granted already: {said}");
    assert!(said.contains("slopty add studio.tail1234.ts.net"), "{said}");
}

#[tokio::test]
async fn an_update_asks_the_remote_install_to_update_and_its_failure_fails_the_deploy() {
    let host = Host::new("Darwin arm64", 1, &health());
    let source = binaries(mac_arm64);
    let failed = deploy(&host.opts(true), source.path()).await.unwrap_err();
    assert!(failed.to_string().contains("on studio failed"), "{failed:#}");
    let scripts = host.scripts();
    assert!(scripts.iter().any(|s| s.ends_with("--update'")), "{scripts:#?}");
    assert!(!scripts.iter().any(|s| s.contains("worker doctor")), "no doctor after a failure");
}

#[tokio::test]
async fn binaries_built_for_another_machine_are_refused_before_anything_moves() {
    let host = Host::new("Linux x86_64", 0, &health());
    let source = binaries(mac_arm64);
    let refused = deploy(&host.opts(false), source.path()).await.unwrap_err();
    let said = format!("{refused:#}");
    assert!(said.contains("is built for macOS arm64, and studio is Linux x86_64"), "{said}");
    assert!(said.contains("--bin-dir"), "{said}");
    assert_eq!(host.scripts().len(), 1, "only uname ran: {:?}", host.scripts());
    assert!(!host.staged("slopty").exists());

    let linux = binaries(|_name| elf(ELF_X86_64));
    let host = Host::new("Linux x86_64", 0, &health());
    deploy(&host.opts(false), linux.path()).await.unwrap();
    assert!(host.staged("slopty-worker").exists(), "a build for the machine goes up");

    let unknown = Host::new("SunOS sparc", 0, &health());
    deploy(&unknown.opts(false), source.path()).await.unwrap_err();
    let dashed = DeployOpts { target: "-oProxyCommand=x".to_owned(), ..host.opts(false) };
    assert!(deploy(&dashed, source.path()).await.is_err(), "not an option in disguise");
}
