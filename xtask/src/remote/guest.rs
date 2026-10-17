//! A check's app e2e case in a macOS guest on this Mac, instead of on a runner Mac.
//!
//! On a runner an e2e case spent two thirds of its time compiling: about two minutes of setup,
//! then a p50 of 501 s building the daemons and the app on three cores, for a p50 of 68 s of
//! tests (`.research/devloop-2026-10-17.md`). Here the build is incremental, in the lane's own
//! warm directory, and the tests run in a long-lived guest ([`GUEST`]) that shows nothing on this
//! Mac (`--no-graphics`) and touches none of its windows, input or grants, as the live lane's
//! guests do (`docs/TESTING.md`, "Live lane in a VM"). The guest runs the runners' very macOS
//! build (26.6.2, 25G83) on Apple's paravirtual GPU, as a runner does.
//!
//! - The lane's tree is the check's commit (`gate::snapshot_commit`), built into
//!   `target/local/<lane>/e2e-target` with the e2e features, apart from the check's own target dir,
//!   whose test-profile builds of the same binaries would replace these in `debug/`.
//! - The test binary goes over as a nextest archive, the binaries the suite spawns beside it, and
//!   the e2e crate's directory (its goldens) at the very path the tests were compiled with.
//! - One run at a time per guest, under a lock; a second lane waits and says so.
//! - Renders and diffs come back to `target/e2e/artifacts/`; accepted goldens come back into the
//!   tree, only those the run wrote; the log goes where `-E failed` reads it.

use std::fs::File;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail, ensure};
use camino::{Utf8Path as Path, Utf8PathBuf as PathBuf};
use xshell::Shell;

use super::Goldens;

/// The guest that runs the e2e cases, a clone of the live lane's base (`cargo xtask vm create`).
pub const GUEST: &str = "slopty-26-e2e";

/// The base it is cloned from.
const BASE: &str = "slopty-26";

/// The guest's memory, in MB.
const MEMORY_MB: &str = "4096";

/// The account the guest logs in as.
const USER: &str = "admin";

/// The longest boot, to a command in the logged-in session: 40–50 s idle, 214 s with this Mac's
/// load at 50.
const BOOT: Duration = Duration::from_mins(6);

/// How many tests run at once in the guest: `SLOPTY_E2E_THREADS`, else three of its four
/// cores. The app's whole suite at HEAD 73366704 took 250 s one at a time and 97 s three at a
/// time, with the same four tests failing, and on a runner 193 s against 103 s with the same
/// nine (`.research/devloop-2026-10-17.md`). One (`SLOPTY_E2E_THREADS=1`) shows every line as
/// it comes.
fn threads() -> usize {
    std::env::var("SLOPTY_E2E_THREADS").ok().and_then(|v| v.parse().ok()).unwrap_or(3).max(1)
}

/// How long the guest stays up with no run, after which it is stopped: it holds 5 GB of this
/// Mac's memory while up, whose swap the lanes' builds already fill, and boots again in 23 s on an
/// idle Mac.
const IDLE_STOP_S: u64 = 20 * 60;

/// Mark the guest used now, and leave a detached watcher that stops it once it has been idle for
/// [`IDLE_STOP_S`]: unused since, and no run holding its lock.
fn stop_when_idle(home: &Path) {
    use std::os::unix::process::CommandExt as _;
    let stamp = home.join("locks").join(format!("{GUEST}.used"));
    let lock = home.join("locks").join(format!("{GUEST}.lock"));
    if std::fs::write(&stamp, b"").is_err() {
        return;
    }
    let tart_home = home.join("tart");
    let script = format!(
        "sleep {IDLE_STOP_S}; now=$(date +%s); used=$(stat -f %m '{stamp}'); \
         [ $((now - used)) -ge {IDLE_STOP_S} ] || exit 0; \
         /usr/bin/lockf -t 0 '{lock}' /usr/bin/env TART_HOME='{tart_home}' tart stop {GUEST} --timeout 30"
    );
    let _watcher = Command::new("/bin/sh")
        .args(["-c", &script])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn();
}

/// What one e2e run here asks.
pub struct Run<'a> {
    /// The check's name, for where its log goes.
    pub name: &'a str,
    /// The case's nextest filterset.
    pub filter: Option<&'a str>,
    /// What it does with the goldens.
    pub goldens: Goldens,
}

/// How a run here went.
pub struct Outcome {
    /// Whether every test passed (or the filter matched none in a filtered run).
    pub passed: bool,
    /// The build and the archive.
    pub build: Duration,
    /// The copy into the guest and the tests.
    pub tests: Duration,
    /// nextest's summary line, or why there is none.
    pub summary: String,
}

/// Where the guests live: `SLOPTY_VM_HOME`, else the external disk, as `xtask vm` has it.
fn home() -> PathBuf {
    std::env::var("SLOPTY_VM_HOME")
        .map_or_else(|_| PathBuf::from("/Volumes/Lacie/vms"), PathBuf::from)
}

/// Whether this Mac can run the e2e guest: tart, and the base to clone it from.
pub fn available() -> bool {
    cfg!(target_os = "macos")
        && tart(&home()).arg("list").output().is_ok_and(|out| {
            out.status.success()
                && String::from_utf8_lossy(&out.stdout)
                    .lines()
                    .any(|l| l.split_whitespace().nth(1) == Some(BASE))
        })
}

fn tart(home: &Path) -> Command {
    let mut tart = Command::new("tart");
    tart.env("TART_HOME", home.join("tart"));
    tart
}

fn tart_out(home: &Path, args: &[&str]) -> Result<String> {
    let out = tart(home).args(args).stdin(Stdio::null()).output().context("tart")?;
    ensure!(
        out.status.success(),
        "tart {}: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(String::from_utf8(out.stdout)?.trim().to_owned())
}

/// `ssh` and `rsync -e`'s options for the guest.
fn ssh_args(home: &Path) -> Vec<String> {
    let mut args = vec!["-i".to_owned(), home.join("ssh/id_ed25519").into_string()];
    for option in [
        "IdentitiesOnly=yes",
        "BatchMode=yes",
        "StrictHostKeyChecking=no",
        "UserKnownHostsFile=/dev/null",
        "LogLevel=ERROR",
        "ConnectTimeout=10",
        "ServerAliveInterval=10",
        "ControlMaster=auto",
        "ControlPersist=60",
    ] {
        args.extend(["-o".to_owned(), option.to_owned()]);
    }
    args.extend(["-o".to_owned(), format!("ControlPath={home}/ssh/cm-%C")]);
    args.extend(["-l".to_owned(), USER.to_owned()]);
    args
}

fn ssh(home: &Path, ip: &str, script: &str) -> Result<()> {
    let status = Command::new("ssh")
        .args(ssh_args(home))
        .arg(ip)
        .arg(script)
        .stdin(Stdio::null())
        .status()
        .context("ssh")?;
    ensure!(status.success(), "`{script}` on the guest: {status}");
    Ok(())
}

/// `rsync -a` with `args`, over the guest's ssh.
fn rsync(home: &Path, args: &[&str], cwd: &Path) -> Result<()> {
    let ssh = format!("ssh {}", ssh_args(home).join(" "));
    let status = Command::new("rsync")
        .args(["-a", "-e", &ssh])
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .status()
        .context("rsync")?;
    ensure!(status.success(), "rsync {}: {status}", args.join(" "));
    Ok(())
}

/// The guest booted, cloned from the base first if it is not there; its address.
#[expect(clippy::disallowed_methods, reason = "xtask is a script, not a library; polling a guest")]
fn up(home: &Path) -> Result<String> {
    let listed = tart_out(home, &["list"])?;
    let state = listed
        .lines()
        .find(|l| l.split_whitespace().nth(1) == Some(GUEST))
        .map(|l| l.split_whitespace().last().unwrap_or("").to_owned());
    match state.as_deref() {
        Some("running") => return tart_out(home, &["ip", GUEST, "--wait", "60"]),
        Some(_) => {}
        None => {
            println!("  making the e2e guest {GUEST} from {BASE}");
            tart_out(home, &["clone", BASE, GUEST])?;
            // Half the base's memory: the guest runs one app and its daemons at a time, and this
            // Mac's lanes build beside it with their swap full.
            tart_out(home, &["set", GUEST, "--memory", MEMORY_MB])?;
        }
    }
    let log = home.join("logs").join(format!("{GUEST}.log"));
    let file = File::create(&log).with_context(|| format!("create {log}"))?;
    let started = Instant::now();
    let mut vm = {
        use std::os::unix::process::CommandExt as _;
        tart(home)
            .args(["run", GUEST, "--no-graphics"])
            .stdin(Stdio::null())
            .stdout(file.try_clone()?)
            .stderr(file)
            .process_group(0)
            .spawn()
            .context("tart run")?
    };
    loop {
        if let Some(status) = vm.try_wait()? {
            bail!("tart run {GUEST} exited with {status}; see {log}");
        }
        let ready = tart(home)
            .args(["exec", GUEST, "/usr/bin/true"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if ready {
            println!("  booted {GUEST} in {:.0?}", started.elapsed());
            return tart_out(home, &["ip", GUEST, "--wait", "60"]);
        }
        ensure!(started.elapsed() < BOOT, "{GUEST} did not come up in {BOOT:?}; see {log}");
        std::thread::sleep(Duration::from_secs(2));
    }
}

/// The guest's lock, held for one run.
fn lock(home: &Path, lane: &str) -> Result<File> {
    let dir = home.join("locks");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{GUEST}.lock"));
    let file = File::options().create(true).truncate(false).write(true).open(&path)?;
    if file.try_lock().is_err() {
        println!("  waiting for the e2e guest: another lane's run holds it");
        file.lock().with_context(|| format!("lock {path}"))?;
    }
    std::fs::write(&path, lane)?;
    Ok(file)
}

/// nextest's config for the guest: the tree's, less its setup scripts, which run cargo (the
/// guest has none, and the binaries they would build are built).
fn guest_config(tree: &Path) -> Result<String> {
    let path = tree.join(".config/nextest.toml");
    let text = std::fs::read_to_string(&path).with_context(|| format!("read {path}"))?;
    let mut config: toml::Table = text.parse().with_context(|| format!("parse {path}"))?;
    let _scripts = config.remove("scripts");
    let _experimental = config.remove("experimental");
    if let Some(toml::Value::Table(profiles)) = config.get_mut("profile") {
        for (_name, profile) in profiles.iter_mut() {
            if let toml::Value::Table(profile) = profile {
                let _scripts = profile.remove("scripts");
            }
        }
    }
    Ok(toml::to_string(&config)?)
}

/// The executables a build left at the top of `debug`: what the suites spawn.
fn binaries(debug: &Path) -> Result<Vec<String>> {
    use std::os::unix::fs::PermissionsExt as _;
    let mut names = Vec::new();
    for entry in std::fs::read_dir(debug).with_context(|| format!("read {debug}"))? {
        let entry = entry?;
        let meta = entry.metadata()?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if meta.is_file() && meta.permissions().mode() & 0o111 != 0 && !name.contains('.') {
            names.push(name);
        }
    }
    names.sort();
    Ok(names)
}

/// Run the app's e2e case of commit `sha` in lane `lane`'s directory, in the guest.
pub fn e2e(root: &Path, lane: &str, sha: &str, run: &Run<'_>) -> Result<Outcome> {
    let started = Instant::now();
    crate::prune::before_build();
    let dir = crate::local::dir(root, lane);
    let tree = crate::gate::snapshot_commit(root, &dir, sha)?;
    crate::local::target_by_config(&dir)?;
    let target = dir.join("e2e-target");
    let sh = Shell::new()?;
    sh.change_dir(&tree);
    sh.set_var("LIBGHOSTTY_VT_SYS_PREBUILT_DIR", root.join("target").join(crate::prune::PREBUILT));
    let target_arg = format!("--target-dir={target}");
    crate::e2e::build_app(&sh, &[&target_arg])?;
    let stage = dir.join("guest");
    std::fs::create_dir_all(&stage)?;
    let archive = stage.join("app.tar.zst");
    let live = crate::e2e::LIVE;
    crate::tools::step(
        "archive slopty-e2e app",
        &xshell::cmd!(
            sh,
            "cargo nextest archive --cargo-profile dev {target_arg} -p slopty-e2e --test app --features {live} --archive-file {archive}"
        )
        .env(crate::gate::BINS_FRESH, "1"),
    )?;
    let config = stage.join("nextest.toml");
    std::fs::write(&config, guest_config(&tree)?)?;
    let built = started.elapsed();

    let home = home();
    let _held = lock(&home, lane)?;
    stop_when_idle(&home);
    let copying = Instant::now();
    let ip = up(&home)?;
    // Whatever a stopped run left: its processes and the stack roots its failures kept.
    ssh(
        &home,
        &ip,
        &format!(
            "pkill -9 -f '{target}/debug/[s]lopty' ; rm -rf /private/tmp/slopty-e2e-* ; sudo mkdir -p '{dir}' && \
             sudo chown {USER} '{dir}' && mkdir -p '{target}/debug' '{tree}' '{stage}/x' && \
             rm -rf '{tree}/target/e2e'"
        ),
    )?;
    let debug = target.join("debug");
    let bins = binaries(&debug)?;
    // Never `--inplace`: a file rewritten where the guest reads it was taken for a corrupt
    // archive, and a binary a killed run still maps keeps its old inode this way.
    let mut args: Vec<&str> = Vec::new();
    args.extend(bins.iter().map(String::as_str));
    let to = format!("{ip}:{debug}/");
    args.push(&to);
    rsync(&home, &args, &debug)?;
    let to = format!("{ip}:{tree}/");
    rsync(
        &home,
        &[
            "--relative",
            "--delete",
            "--exclude",
            "target/",
            "./crates",
            "./apps",
            "./.config",
            "./Cargo.toml",
            &to,
        ],
        &tree,
    )?;
    let nextest = which("cargo-nextest")?;
    let to = format!("{ip}:{stage}/");
    rsync(&home, &[archive.as_str(), nextest.as_str(), config.as_str(), &to], &stage)?;
    println!(
        "  copied {} binaries and the archive into {GUEST} ({:.1?})",
        bins.len(),
        copying.elapsed()
    );

    let data = tree.join("target/e2e");
    let accept = match run.goldens {
        Goldens::Compare => None,
        Goldens::Accept => Some("changed"),
        Goldens::AcceptAll => Some("all"),
        Goldens::Review => Some("review"),
    };
    let filter = crate::e2e::app_filterset(run.filter).unwrap_or_default();
    let output = match threads() {
        1 => "--no-capture".to_owned(),
        n => format!("--test-threads {n} --failure-output final"),
    };
    let mut command = tart(&home);
    command.args(["exec", GUEST, "/usr/bin/env"]);
    for (key, value) in crate::e2e::app_env(data.as_str(), debug.as_str(), accept) {
        command.arg(format!("{key}={value}"));
    }
    command.arg(format!("INSTA_WORKSPACE_ROOT={tree}"));
    command.args([
        "/bin/sh",
        "-c",
        &format!(
            "mkdir -p '{data}/run' '{data}/artifacts' && cd '{tree}' && exec '{stage}/cargo-nextest' \
             nextest run --config-file '{stage}/nextest.toml' --archive-file '{archive}' \
             --extract-to '{stage}/x' --extract-overwrite --workspace-remap '{tree}' \
             --run-ignored only {output} --no-fail-fast -E \"$0\""
        ),
        &filter,
    ]);
    let logs = root.join("target/remote").join(run.name).join("logs/logs-run");
    if logs.exists() {
        std::fs::remove_dir_all(&logs)?;
    }
    std::fs::create_dir_all(&logs)?;
    let log = logs.join("e2e.log");
    println!("▶ e2e app in {GUEST}{}", run.filter.map(|f| format!(" -E {f}")).unwrap_or_default());
    let status = teed(command, &log)?;
    let tests = copying.elapsed();
    stop_when_idle(&home);

    // Renders and diffs of changed frames, as a runner's come back.
    let renders = root.join("target/e2e/artifacts");
    if renders.exists() {
        std::fs::remove_dir_all(&renders)?;
    }
    std::fs::create_dir_all(&renders)?;
    let from = format!("{ip}:{data}/artifacts/");
    let _pulled = rsync(&home, &[&from, renders.as_str()], root);
    if accept.is_some_and(|a| a != "review") {
        let back = stage.join("golden-back");
        if back.exists() {
            std::fs::remove_dir_all(&back)?;
        }
        let from = format!("{ip}:{tree}/crates/slopty-e2e/golden/");
        rsync(&home, &[&from, back.as_str()], root)?;
        let applied = apply_goldens(
            &back,
            &tree.join("crates/slopty-e2e/golden"),
            &root.join("crates/slopty-e2e/golden"),
        )?;
        println!("  {applied} goldens the run wrote applied to the tree");
    }
    let text = std::fs::read_to_string(&log).unwrap_or_default();
    let summary = text
        .lines()
        .rev()
        .find(|l| l.contains("Summary ["))
        .map_or_else(|| "no summary: see the log".to_owned(), |l| l.trim().to_owned());
    let code = status.code();
    let passed =
        code == Some(0) || (run.filter.is_some() && code == Some(crate::e2e::NO_TESTS_RUN));
    if !passed {
        let failed = super::failed_tests(&text);
        for test in &failed {
            println!("        FAIL {test}");
        }
        println!("  every line: {log}; renders and diffs: {renders}");
    }
    Ok(Outcome { passed, build: built, tests, summary })
}

/// Copy into `tree_goldens` each file of `back` (the guest's goldens after the run) whose bytes
/// differ from `snapshot`'s (what the run started from): the goldens the run wrote, and none of
/// another lane's edits in the tree. How many.
fn apply_goldens(back: &Path, snapshot: &Path, tree_goldens: &Path) -> Result<usize> {
    let mut applied = 0_usize;
    for entry in walk(back)? {
        let below = entry.strip_prefix(back).context("a golden outside its dir")?;
        let was = std::fs::read(snapshot.join(below)).ok();
        let now = std::fs::read(&entry)?;
        if was.as_deref() != Some(now.as_slice()) {
            let to = tree_goldens.join(below);
            if let Some(parent) = to.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&to, &now)?;
            applied = applied.saturating_add(1);
        }
    }
    Ok(applied)
}

/// Every file under `dir`.
fn walk(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let mut dirs = vec![dir.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).with_context(|| format!("read {dir}"))? {
            let path = PathBuf::try_from(entry?.path())?;
            if path.is_dir() {
                dirs.push(path);
            } else {
                files.push(path);
            }
        }
    }
    Ok(files)
}

/// `name`'s path on `PATH`.
fn which(name: &str) -> Result<PathBuf> {
    let out = Command::new("which").arg(name).output().context("which")?;
    ensure!(out.status.success(), "{name} is not on PATH");
    Ok(PathBuf::from(String::from_utf8(out.stdout)?.trim()))
}

/// Run `command`, its output into `log` and, less the lines of what passed, onto this terminal;
/// how it ended.
fn teed(mut command: Command, log: &Path) -> Result<std::process::ExitStatus> {
    use std::io::{BufRead as _, Write as _};
    let mut file = File::create(log).with_context(|| format!("create {log}"))?;
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("tart exec")?;
    let stderr = child.stderr.take().context("stderr")?;
    let (tx, lines) = std::sync::mpsc::channel::<String>();
    let err_tx = tx.clone();
    let err = std::thread::spawn(move || {
        for line in std::io::BufReader::new(stderr).lines().map_while(Result::ok) {
            if err_tx.send(line).is_err() {
                break;
            }
        }
    });
    let stdout = child.stdout.take().context("stdout")?;
    let out = std::thread::spawn(move || {
        for line in std::io::BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    for line in lines {
        writeln!(file, "{line}")?;
        let trimmed = line.trim_start();
        let shown = ["FAIL [", "TIMEOUT [", "Summary [", "Starting ", "SIG", "error:"]
            .iter()
            .any(|p| trimmed.starts_with(p));
        if shown {
            println!("{line}");
        }
    }
    let joined = (out.join(), err.join());
    ensure!(joined.0.is_ok() && joined.1.is_ok(), "a reader thread panicked");
    Ok(child.wait()?)
}

#[cfg(test)]
mod tests {
    use camino::Utf8PathBuf;

    /// Only the goldens whose bytes the run changed, or wrote anew, reach the tree; one the
    /// run left as it found it keeps whatever the tree holds (another lane's edit).
    #[test]
    fn only_the_goldens_a_run_wrote_come_back() {
        let dir = std::env::temp_dir().join(format!("xtask-guest-goldens-{}", std::process::id()));
        let _gone = std::fs::remove_dir_all(&dir);
        let dir = Utf8PathBuf::from_path_buf(dir).expect("utf8");
        let (back, snapshot, tree) = (dir.join("back"), dir.join("snapshot"), dir.join("tree"));
        for d in [&back, &snapshot, &tree] {
            std::fs::create_dir_all(d.join("ipad")).expect("dirs");
        }
        std::fs::write(snapshot.join("same.png"), "a").expect("write");
        std::fs::write(back.join("same.png"), "a").expect("write");
        std::fs::write(tree.join("same.png"), "edited by another lane").expect("write");
        std::fs::write(snapshot.join("ipad/moved.txt"), "old").expect("write");
        std::fs::write(back.join("ipad/moved.txt"), "new").expect("write");
        std::fs::write(back.join("fresh.png"), "made").expect("write");
        assert_eq!(super::apply_goldens(&back, &snapshot, &tree).expect("applied"), 2);
        let read = |p: &str| std::fs::read_to_string(tree.join(p)).expect("read");
        assert_eq!(read("same.png"), "edited by another lane");
        assert_eq!(read("ipad/moved.txt"), "new");
        assert_eq!(read("fresh.png"), "made");
        let _gone = std::fs::remove_dir_all(&dir);
    }
}
