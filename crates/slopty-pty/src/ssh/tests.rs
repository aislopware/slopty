//! The wrapper against a fake `ssh` that records what it was asked: no host is reached.

use std::path::Path;

use super::*;

fn strings(split: &[OsString]) -> Vec<String> {
    split.iter().map(|a| a.to_string_lossy().into_owned()).collect()
}

#[test]
fn ssh_is_cut_into_options_destination_and_command_as_ssh_reads_it() {
    let cut = split(&os_args(&["-p", "2222", "-iKEY", "box", "-A", "ls", "-la"]));
    assert_eq!(strings(&cut.connect), ["-p", "2222", "-iKEY", "box", "-A"]);
    assert_eq!(strings(&cut.command), ["ls", "-la"]);
    assert_eq!(cut.flags, "piA");
    assert!(!cut.interactive(), "a remote command has no terminal");

    let login = split(&os_args(&["-vo", "User=me", "box"]));
    assert_eq!(strings(&login.connect), ["-vo", "User=me", "box"]);
    assert!(login.interactive());

    assert!(split(&os_args(&["-t", "box", "htop"])).interactive(), "-t asks for a terminal");
    assert!(split(&os_args(&["box", "-t", "htop"])).interactive(), "options after the host");
    for no_session in [&["-N", "box"][..], &["-V"], &["-G", "box"], &["-O", "check", "box"]] {
        assert!(!split(&os_args(no_session)).interactive(), "{no_session:?}");
    }
    assert!(!split(&os_args(&["-T", "box"])).interactive(), "-T refuses the terminal");

    let ended = split(&os_args(&["--", "box", "-p", "1"]));
    assert_eq!(strings(&ended.command), ["-p", "1"], "after `--` nothing is an option");
    assert!(!split(&os_args::<&str>(&[])).interactive(), "no destination");
}

#[test]
fn the_destination_is_user_at_hostname_from_ssh_g() {
    let config = "hostname box.tail1234.ts.net\nport 22\nuser alice\nidentityfile ~/.ssh/k\n";
    assert_eq!(destination(config).as_deref(), Some("alice@box.tail1234.ts.net"));
    assert_eq!(destination("user alice\nport 22\n"), None);
    assert_eq!(destination("hostname box\n"), None);
}

#[test]
fn the_cache_holds_one_version_per_destination() {
    let dir = tempfile::tempdir().unwrap();
    let cache = dir.path().join("data").join("ssh-terminfo");
    assert!(!cached(&cache, "a@b", "v1"), "no file yet");
    remember(&cache, "a@b", "v1").unwrap();
    remember(&cache, "c@d", "v1").unwrap();
    assert!(cached(&cache, "a@b", "v1") && cached(&cache, "c@d", "v1"));
    assert!(!cached(&cache, "a@b", "v2"), "a changed entry is installed again");
    remember(&cache, "a@b", "v2").unwrap();
    assert_eq!(std::fs::read_to_string(&cache).unwrap(), "c@d v1\na@b v2\n");
}

/// A fake `ssh` in `dir`: `-G` resolves every host to `alice@box.example` (or fails), the
/// install call keeps its standard input in `got` and exits `tic_exit`, and the session
/// prints its `TERM` and arguments. Every call is a line in `log`.
struct FakeSsh {
    path: PathBuf,
    log: PathBuf,
    got: PathBuf,
}

impl FakeSsh {
    fn new(dir: &Path, resolves: bool, tic_exit: u8) -> Self {
        let (path, log, got) = (dir.join("ssh"), dir.join("log"), dir.join("got"));
        let resolve = if resolves {
            "printf 'user alice\\nhostname box.example\\nport 22\\n'; exit 0"
        } else {
            "exit 255"
        };
        let script = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{log}'\n\
             if [ \"$1\" = -G ]; then {resolve}; fi\n\
             for last; do :; done\n\
             case $last in *'tic -x -'*) cat > '{got}'; exit {tic_exit};; esac\n\
             printf 'session TERM=%s args=%s\\n' \"$TERM\" \"$*\"\n",
            log = log.display(),
            got = got.display(),
        );
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        Self { path, log, got }
    }

    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(&self.log).unwrap_or_default().lines().map(str::to_owned).collect()
    }

    fn options(&self, dir: &Path) -> Options {
        Options {
            ssh: self.path.clone(),
            cache: dir.join("ssh-terminfo"),
            install: true,
            term: Some("xterm-ghostty".to_owned()),
        }
    }
}

/// What the prepared `ssh` printed as its session.
async fn session(ssh: std::process::Command) -> String {
    let out = tokio::process::Command::from(ssh).output().await.unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

async fn prepared(opts: &Options, args: &[&str]) -> (std::process::Command, Decision, Vec<String>) {
    let mut notes = Vec::new();
    let (ssh, decision) =
        prepare(opts, &os_args(args), &mut |n: &str| notes.push(n.to_owned())).await;
    (ssh, decision, notes)
}

const SEND: &str = "-o SendEnv=COLORTERM -o SendEnv=TERM_PROGRAM -o SendEnv=TERM_PROGRAM_VERSION";

#[tokio::test]
async fn the_first_login_installs_the_entry_and_later_ones_trust_the_cache() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeSsh::new(dir.path(), true, 0);
    let opts = fake.options(dir.path());

    let (ssh, decision, notes) = prepared(&opts, &["-p", "2222", "box"]).await;
    assert_eq!(decision, Decision::Installed("alice@box.example".to_owned()));
    assert_eq!(notes, ["setting up the xterm-ghostty terminfo on alice@box.example"]);
    assert_eq!(std::fs::read_to_string(&fake.got).unwrap(), terminfo::source(), "the entry");
    assert_eq!(fake.calls(), ["-G -p 2222 box".to_owned(), format!("-p 2222 box {REMOTE_TIC}")]);
    assert_eq!(session(ssh).await, format!("session TERM=xterm-ghostty args={SEND} -p 2222 box"));

    let (ssh, decision, notes) = prepared(&opts, &["box"]).await;
    assert_eq!(decision, Decision::Cached("alice@box.example".to_owned()));
    assert!(notes.is_empty(), "{notes:?}");
    assert_eq!(fake.calls().len(), 4, "one -G, no install: {:?}", fake.calls());
    assert_eq!(session(ssh).await, format!("session TERM=xterm-ghostty args={SEND} box"));
}

#[tokio::test]
async fn a_host_that_cannot_take_the_entry_gets_xterm_256color_and_is_asked_again_next_time() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeSsh::new(dir.path(), true, 1);
    let opts = fake.options(dir.path());
    let (ssh, decision, notes) = prepared(&opts, &["box"]).await;
    assert!(matches!(decision, Decision::Fallback(_)), "{decision:?}");
    assert!(
        notes.last().is_some_and(|n| n.ends_with("this session uses xterm-256color")),
        "{notes:?}"
    );
    assert_eq!(session(ssh).await, format!("session TERM=xterm-256color args={SEND} box"));
    assert!(!opts.cache.exists(), "a failure is not cached");
    let (_ssh, decision, _notes) = prepared(&opts, &["box"]).await;
    assert!(matches!(decision, Decision::Fallback(_)));
    assert_eq!(fake.calls().iter().filter(|c| c.contains("tic -x -")).count(), 2);

    let unresolved = FakeSsh::new(dir.path(), false, 0);
    let (ssh, decision, _notes) = prepared(&unresolved.options(dir.path()), &["box"]).await;
    assert!(matches!(decision, Decision::Fallback(_)), "{decision:?}");
    assert_eq!(session(ssh).await, format!("session TERM=xterm-256color args={SEND} box"));
}

#[tokio::test]
async fn what_is_not_a_login_with_our_term_passes_through_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeSsh::new(dir.path(), true, 0);
    let opts = fake.options(dir.path());
    for args in
        [&["box", "cat", "/etc/hosts"][..], &["-N", "-L", "8080:localhost:80", "box"], &["-V"]]
    {
        let (ssh, decision, _notes) = prepared(&opts, args).await;
        assert_eq!(decision, Decision::Untouched, "{args:?}");
        let printed = session(ssh).await;
        assert!(printed.ends_with(&format!("args={}", args.join(" "))), "{printed}");
    }
    let tmux = Options { term: Some("tmux-256color".to_owned()), ..opts.clone() };
    let (ssh, decision, _notes) = prepared(&tmux, &["box"]).await;
    assert_eq!(decision, Decision::Untouched);
    assert!(!session(ssh).await.contains("SendEnv"), "a TERM not ours is not ours to change");
    assert!(
        fake.calls().iter().all(|c| !c.starts_with("-G")),
        "nothing resolved: {:?}",
        fake.calls()
    );

    let untouched_hosts = Options { install: false, ..opts };
    let (ssh, decision, _notes) = prepared(&untouched_hosts, &["box"]).await;
    assert!(matches!(decision, Decision::Fallback(_)));
    assert_eq!(session(ssh).await, format!("session TERM=xterm-256color args={SEND} box"));
    assert!(fake.calls().iter().all(|c| !c.contains("tic")), "{:?}", fake.calls());
}
