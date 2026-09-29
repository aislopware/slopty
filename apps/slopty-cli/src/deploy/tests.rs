//! A deploy through a fake `ssh` that plays the remote host in a temporary home, and the report
//! the person reads after it. The plan's steps and failures are `slopty_deploy`'s own tests.

use slopty_deploy::{Arch, Platform};
use slopty_platform::service::WORKER_BINARIES;
use slopty_proto::ctl::Health;
use slopty_proto::server::{Os as WorkerOs, WorkerCaps};

use super::*;

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

/// The CLI's options reach the plan: its `ssh`, its target and `--update`, and a whole deploy
/// through a fake `ssh` answers with the doctor's report.
#[tokio::test]
async fn the_options_reach_the_plan_and_the_report_says_what_is_next() {
    let dir = tempfile::tempdir().unwrap();
    let (ssh, log) = (dir.path().join("ssh"), dir.path().join("log"));
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let doctor = serde_json::to_string(&health()).unwrap();
    let script = format!(
        "#!/bin/sh\nprintf '%s %s\\n' \"$1\" \"$2\" >> '{log}'\ncd '{home}' || exit 1\n\
         case $2 in\n\
         *'uname -sm'*) echo 'Darwin arm64' ;;\n\
         *'worker install'*) ;;\n\
         *'worker doctor'*) printf '%s\\n' '{doctor}' ;;\n\
         *) eval \"$2\" ;;\n\
         esac\n",
        log = log.display(),
        home = home.display(),
    );
    std::fs::write(&ssh, script).unwrap();
    std::fs::set_permissions(&ssh, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let source = tempfile::tempdir().unwrap();
    let mac_arm64 = [0xcf, 0xfa, 0xed, 0xfe, 0x0c, 0x00, 0x00, 0x01];
    for name in WORKER_BINARIES {
        std::fs::write(source.path().join(name), mac_arm64).unwrap();
    }
    let opts = DeployOpts { target: "studio".to_owned(), update: true, bin_dir: None, ssh };
    let deployed = deploy(&opts, source.path()).await.unwrap();
    let scripts = std::fs::read_to_string(&log).unwrap();
    assert!(scripts.starts_with("studio sh -c 'uname -sm'\n"), "{scripts}");
    assert!(scripts.contains(" --update'\n"), "{scripts}");
    assert_eq!(deployed.platform, Platform { os: Os::MacOs, arch: Arch::Arm64 });

    let said = report("studio", &deployed);
    assert!(said.contains("is up on studio (macOS arm64)"), "{said}");
    assert!(said.contains("allow Screen & System Audio Recording for"), "{said}");
    assert!(!said.contains("Accessibility"), "granted already: {said}");
    assert!(said.contains("slopty add studio.tail1234.ts.net"), "{said}");
}
