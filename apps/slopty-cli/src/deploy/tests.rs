//! A deploy through a fake `ssh` that plays the remote host in a temporary home, and the report
//! the person reads after it. The plan's steps and failures are `slopty_deploy`'s own tests.

use slopty_deploy::{Arch, Platform};
use slopty_platform::service::WORKER_BINARIES;
use slopty_proto::ctl::Health;
use slopty_proto::server::{Os as WorkerOs, WorkerCaps};

use super::*;

fn health() -> Health {
    Health {
        worker: slopty_core::WorkerId::nil(),
        server: None,
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

/// The CLI's options reach the plan: its `ssh`, its target, `--update` and the server to
/// register with, and a whole deploy through a fake `ssh` answers with the doctor's report.
#[tokio::test]
async fn the_options_reach_the_plan_and_the_report_says_what_is_next() {
    let dir = tempfile::tempdir().unwrap();
    let (ssh, log) = (dir.path().join("ssh"), dir.path().join("log"));
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let doctor = serde_json::to_string(&health()).unwrap();
    let script = format!(
        "#!/bin/sh\nwhile [ \"$1\" = -o ]; do shift 2; done\nprintf '%s %s\\n' \"$1\" \"$2\" >> '{log}'\ncd '{home}' || exit 1\n\
         case $2 in\n\
         *'uname -sm'*) echo 'Darwin arm64' ;;\n\
         *'--plan'*) echo '{{\"ptyd\":\"restarts\",\"sessions\":2,\"build\":\"0.4.0\",\"running\":null}}' ;;\n\
         *'worker service'*) echo '{{\"ptyd\":\"absent\",\"worker\":\"absent\",\
         \"stops_at_logout\":\"it stops when you log out\"}}' ;;\n\
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
    let opts = DeployOpts {
        target: "studio".to_owned(),
        update: true,
        bin_dir: None,
        ssh,
        end_sessions: false,
    };
    let data = tempfile::tempdir().unwrap();
    let refused = deploy(&opts, Some("hub.tail1234.ts.net"), data.path(), source.path());
    let refused = format!("{:#}", refused.await.unwrap_err());
    assert!(refused.contains("ending 2 sessions; pass --end-sessions"), "{refused}");
    let opts = DeployOpts { end_sessions: true, ..opts };
    let deployed = deploy(&opts, Some("hub.tail1234.ts.net"), data.path(), source.path());
    let deployed = deployed.await.unwrap();
    let scripts = std::fs::read_to_string(&log).unwrap();
    assert!(scripts.starts_with("studio sh -c 'uname -sm"), "{scripts}");
    assert!(
        scripts.contains(" --server hub.tail1234.ts.net:45560 worker install --bin-dir"),
        "{scripts}"
    );
    assert!(scripts.contains(" --update --end-sessions'\n"), "{scripts}");
    assert_eq!(deployed.platform, Platform { os: Os::MacOs, arch: Arch::Arm64 });
    assert_eq!(deployed.server, "hub.tail1234.ts.net:45560");

    let said = report("studio", &deployed);
    assert!(said.contains("is up on studio (macOS arm64)"), "{said}");
    assert!(said.contains("allow Screen & System Audio Recording for"), "{said}");
    assert!(!said.contains("Accessibility"), "granted already: {said}");
    assert!(said.contains("registers with the server at hub.tail1234.ts.net:45560"), "{said}");
    assert!(said.contains("lists studio.tail1234.ts.net"), "{said}");
    assert!(said.contains("slopty-ptyd restarts, ending the 2 sessions"), "{said}");
    assert!(said.contains("it stops when you log out"), "{said}");

    assert!(!said.contains("nobody is logged in"), "the machine said nothing of it: {said}");

    let console = slopty_deploy::Console { logged_in: Some(false), filevault: Some(true) };
    let said = report("studio", &Deployed { console, ..deployed.clone() });
    assert!(said.contains("nobody is logged in at studio, and Slopty runs once someone is"));
    assert!(said.contains("unlock its disk with `ssh` first"), "{said}");
    let console = slopty_deploy::Console { filevault: Some(false), ..console };
    let said = report("studio", &Deployed { console, ..deployed });
    assert!(said.contains("turn on automatic login"), "{said}");
}

/// `slopty server deploy` sends the server and its CLI through the `ssh` given, runs `server
/// install` there, and points clients at the address `ssh` reached.
#[tokio::test]
async fn a_server_deploy_installs_the_server_and_names_where_it_is() {
    let dir = tempfile::tempdir().unwrap();
    let (ssh, log) = (dir.path().join("ssh"), dir.path().join("log"));
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let script = format!(
        "#!/bin/sh\nwhile [ \"$1\" = -o ]; do shift 2; done\nprintf '%s %s\\n' \"$1\" \"$2\" >> '{log}'\ncd '{home}' || exit 1\n\
         case $2 in\n\
         *'uname -sm'*) echo 'Darwin arm64'; echo '100.64.0.2 51234 100.64.0.9 22' ;;\n\
         *'server install'*) ;;\n\
         *) eval \"$2\" ;;\n\
         esac\n",
        log = log.display(),
        home = home.display(),
    );
    std::fs::write(&ssh, script).unwrap();
    std::fs::set_permissions(&ssh, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let source = tempfile::tempdir().unwrap();
    let mac_arm64 = [0xcf, 0xfa, 0xed, 0xfe, 0x0c, 0x00, 0x00, 0x01];
    for name in slopty_deploy::SERVER_BINARIES {
        std::fs::write(source.path().join(name), mac_arm64).unwrap();
    }
    let opts = ServerDeployOpts { target: "hub".to_owned(), bin_dir: None, ssh };
    let up = serve(&opts, source.path()).await.unwrap();
    assert_eq!(up.addresses, ["100.64.0.9", "hub"]);
    assert!(home.join(STAGE).join("slopty-server").is_file(), "the server went up");
    let scripts = std::fs::read_to_string(&log).unwrap();
    assert!(
        scripts.contains(&format!("hub sh -c '{STAGE}/slopty server install --bin-dir {STAGE}'")),
        "{scripts}"
    );
    let said = served("hub", &up);
    assert!(said.contains("up on hub (macOS arm64)") && said.contains("--server 100.64.0.9"));
}
