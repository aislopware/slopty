//! `cargo xtask vm ci-image`: the image the office Macs' guests run Slopty's jobs in (`cargo
//! xtask gitlab`, `xtask/src/gitlab/ci.yml`), set up as GitHub's `xcode-27` runner is for them.
//!
//! It is made from the e2e guests' base ([`FROM`], `remote::guest`): Cirrus Labs' macOS 27.0 with
//! this Mac's Xcode, the runners' iOS simulator runtime and both simulators settled, Spotlight
//! off and nothing updating. On it go what every job would otherwise fetch and that changes
//! only with a pin here or in `xtask setup`: Rust and its targets, nightly rustfmt, zig,
//! sccache, cargo-binstall and every tool `xtask setup` installs, at the paths of GitHub's
//! runners (`/Users/runner/.cargo`, `/Users/runner/.rustup`, Xcode as `Xcode_27.1.app`) so
//! sccache's units are shared with them; the screen capture the executor's SSH sessions do
//! approved without a prompt; and xtask itself as the jobs' guard (`xtask gitlab guard`), which
//! must watch the Mac's queue before a job has fetched anything. What changes with each commit
//! stays out: the sources, the registry, the target directory and the compile cache, which each
//! job fetches (sccache from R2), since a guest is a fresh clone every job and a baked copy would
//! be stale by the next commit (`docs/decisions/tooling.md`, "The office Macs run Slopty's jobs
//! while idle").
//!
//! `--push` pushes it to Zot on office-1, the office LAN's registry, as `<date>` and `latest` and
//! pins its digest in `gitlab/ci.yml`, which `cargo xtask gitlab publish` then gives the GitLab
//! project. The runners are on the office LAN, and Harbor is not: it sits in IDC behind the
//! office's tunnel, at 2.4 MB/s a connection up and 5.75 down from this Mac, where Zot takes 46 up
//! on one connection and 115 down. `--lan` pushes through a local nginx to the registry's LAN
//! address, for a Mac whose DNS answers the name with office-1's tailnet address, which the edge
//! refuses. Zot ends any request after 60 s (its default `http.readTimeout` and `writeTimeout`),
//! so a push sends each layer in requests of [`PUSH_CHUNK_MB`], and a pull, which tart resumes from
//! the byte it had five times a layer, keeps few enough layers at once that each gets a fifth of
//! its bytes in a minute (`TART_EXECUTOR_PULL_CONCURRENCY` in `gitlab/ci.yml`).

use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, ensure};
use camino::Utf8Path;
use clap::Args;

use super::{boot, exists, ip, remove, ssh_out, tart_out, tart_shown};
use crate::gitlab::GUARD;

/// The base the image is made from.
const FROM: &str = "slopty-27-xcode";

/// The image's guest here.
const NAME: &str = "slopty-ci-27";

/// Where it is pushed: the registry on office-1, whose `slopty/**` the account `slopty-publisher`
/// pushes and the runners' `slopty-runners` reads.
const REGISTRY: &str = "zot.jmango360.dev";
const REPOSITORY: &str = "zot.jmango360.dev/slopty/macos-27-ci";
const PATH_IN_REGISTRY: &str = "slopty/macos-27-ci";

/// Where `--lan`'s nginx listens.
const LAN_PROXY: &str = "127.0.0.1:5555";

/// How many layers a push sends at once: Zot takes a gigabit link's worth from four.
const PUSH_CONCURRENCY: &str = "8";

/// The size of a push's requests, in MB: a layer is 512 MiB before lz4, and one sent whole on a
/// shared link outlasts the minute Zot gives a request.
const PUSH_CHUNK_MB: &str = "100";

/// Its disk, in GB: macOS and Xcode 25, the runtime 8, the toolchains 5, and a cold build of
/// every test shard with the e2e (about 40) beside them. The image holds only what is written.
const DISK_GB: &str = "150";

/// Its cores and memory when the runner's executor sets none (both runners give a job the
/// host's: 14 cores and 48 GB, 16 cores and 44 GB).
const CPUS: &str = "8";
const MEMORY_MB: &str = "16384";

/// The Rust the gate pins (`RUSTUP_TOOLCHAIN` in ci.yml), and the targets its lanes build.
const RUST: &str = "1.99.1";
const TARGETS: &str = "aarch64-apple-ios aarch64-apple-ios-sim x86_64-unknown-linux-musl \
                       x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu aarch64-unknown-linux-musl";

/// The sccache GitHub's jobs run (`.github/actions/sccache`).
const SCCACHE: &str = "0.19.0";

/// How many times a push is tried before it fails: a push again skips the layers sent.
const PUSHES: u32 = 5;

/// The responsible process of every command the executor runs over SSH, whose TCC grants a
/// command's screen capture and events count against.
const SSH_RESPONSIBLE: &str = "/usr/libexec/sshd-keygen-wrapper";

/// `cargo xtask vm ci-image`.
#[derive(Args, Debug)]
pub struct CiImageOpts {
    /// Push it to Zot as well (`TART_REGISTRY_USERNAME` and `TART_REGISTRY_PASSWORD` set, the
    /// account `slopty-publisher`), and pin its digest in `xtask/src/gitlab/ci.yml`.
    #[arg(long)]
    push: bool,
    /// Push the image made before instead of making it again.
    #[arg(long, requires = "push")]
    only_push: bool,
    /// The registry's address on the office LAN (192.168.100.254), reached through a local nginx
    /// with the registry's name and certificate: for a Mac whose DNS answers otherwise.
    #[arg(long, requires = "push")]
    lan: Option<String>,
}

/// `cargo xtask vm ci-image`.
pub fn run(home: &Utf8Path, opts: &CiImageOpts) -> Result<()> {
    if !opts.only_push {
        make(home)?;
    }
    if opts.push {
        push(home, opts.lan.as_deref())?;
    }
    Ok(())
}

/// Make [`NAME`] from [`FROM`], under another name until it is whole.
fn make(home: &Utf8Path) -> Result<()> {
    ensure!(exists(home, FROM)?, "no {FROM}: the e2e lane's first run makes it (`remote::guest`)");
    let making = format!("{NAME}-making");
    if exists(home, &making)? {
        remove(home, &making)?;
    }
    tart_out(home, &["clone", FROM, &making])?;
    tart_out(
        home,
        &["set", &making, "--disk-size", DISK_GB, "--cpu", CPUS, "--memory", MEMORY_MB],
    )?;
    let booted = boot(home, &making)?;
    let ip = ip(home, &making)?;
    println!("▶ {making} up at {ip} in {booted:.0?}");
    let root = crate::tools::repo_root()?;
    let sha = Command::new("git")
        .args(["-C", root.as_str(), "rev-parse", "HEAD"])
        .output()
        .context("git rev-parse")?;
    let sha = String::from_utf8(sha.stdout)?.trim().to_owned();
    for (what, script) in steps(&sha) {
        println!("  {what}");
        ssh_out(home, &ip, &format!("export PATH={PATH}; set -e; {script}"))
            .with_context(|| format!("{what} in {making}"))?;
    }
    println!("{}", ssh_out(home, &ip, "cat /etc/slopty-ci-image")?);
    tart_out(home, &["stop", &making, "--timeout", "60"])?;
    if exists(home, NAME)? {
        remove(home, NAME)?;
    }
    tart_out(home, &["rename", &making, NAME])?;
    println!("✓ made {NAME}");
    Ok(())
}

/// The guest's `PATH` for the steps: the toolchains, then Homebrew's, then the system's.
const PATH: &str = "/Users/runner/.cargo/bin:/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin";

/// What makes the image, in order, each run by `sh` on the guest as `admin`; the tools at commit
/// `sha` of this checkout (on GitHub).
fn steps(sha: &str) -> Vec<(&'static str, String)> {
    vec![
        (
            "the disk grown into its new size",
            "yes | sudo diskutil repairDisk disk0 >/dev/null 2>&1 || true; \
             sudo diskutil apfs resizeContainer disk0s2 0 >/dev/null 2>&1 || true; \
             df -g / | awk 'NR == 2 { exit ($2 < 130) }'"
                .to_owned(),
        ),
        (
            "GitHub's runner paths: /Users/runner, Xcode as Xcode_27.1.app",
            "sudo mkdir -p /Users/runner/work && sudo chown -R admin:staff /Users/runner && \
             sudo ln -sfn /Applications/Xcode.app /Applications/Xcode_27.1.app && \
             ln -sfn /Users/runner/.cargo ~/.cargo && ln -sfn /Users/runner/.rustup ~/.rustup"
                .to_owned(),
        ),
        (
            "Rust, its targets and nightly rustfmt",
            format!(
                "export CARGO_HOME=/Users/runner/.cargo RUSTUP_HOME=/Users/runner/.rustup; \
                 curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs -o /tmp/rustup-init.sh; \
                 sh /tmp/rustup-init.sh -y -q --profile minimal --default-toolchain {RUST} --no-modify-path; \
                 rm /tmp/rustup-init.sh; \
                 rustup component add --toolchain {RUST} rustfmt clippy rust-src llvm-tools-preview rust-analyzer; \
                 rustup target add --toolchain {RUST} {TARGETS}; \
                 rustup toolchain install nightly --profile minimal --component rustfmt; \
                 rustup default {RUST}"
            ),
        ),
        (
            "zig, cargo-binstall and sccache",
            format!(
                "brew install --quiet zig >/dev/null && brew pin zig; \
                 curl -sSfL https://github.com/cargo-bins/cargo-binstall/releases/latest/download/cargo-binstall-aarch64-apple-darwin.zip -o /tmp/cb.zip; \
                 unzip -oq /tmp/cb.zip -d /Users/runner/.cargo/bin; rm /tmp/cb.zip; \
                 cargo binstall --no-confirm --locked sccache@{SCCACHE}"
            ),
        ),
        (
            "the tools `xtask setup` installs, and its xtask as the jobs' guard",
            format!(
                "rm -rf /tmp/slopty-setup; mkdir /tmp/slopty-setup; cd /tmp/slopty-setup; git init -q; \
                 git fetch -q --depth 1 https://github.com/aislopware/slopty {sha}; git checkout -q FETCH_HEAD; \
                 cargo xtask setup; \
                 sudo install -m 755 target/xtask/debug/xtask {GUARD}; {GUARD} gitlab guard --help >/dev/null"
            ),
        ),
        ("screen capture over SSH, approved once and for all", screen_capture()),
        (
            "quiet: reduced motion and transparency, muted",
            "defaults write com.apple.universalaccess reduceMotion -bool true; \
             defaults write com.apple.universalaccess reduceTransparency -bool true; \
             osascript -e 'set volume output muted true'"
                .to_owned(),
        ),
        (
            "caches dropped, the image stamped",
            format!(
                "rm -rf /tmp/slopty-setup /Users/runner/.cargo/registry /Users/runner/.cargo/git \
                 /Users/runner/.rustup/downloads/* /Users/runner/.rustup/tmp/* \
                 ~/Library/Caches/Mozilla.sccache ~/Library/Caches/Homebrew/*; \
                 {{ echo \"made: $(date -u +%Y-%m-%dT%H:%MZ) from {FROM}\"; \
                 echo \"macos: $(sw_vers -productVersion) $(sw_vers -buildVersion)\"; \
                 echo \"xcode: $(xcodebuild -version | paste -sd' ' -)\"; \
                 echo \"rust: $(rustc -V), nightly rustfmt\"; echo \"sccache: $(sccache --version)\"; \
                 echo \"zig: $(zig version)\"; echo \"nextest: $(cargo nextest --version | head -1)\"; \
                 echo \"tools: xtask setup at {sha}\"; }} | sudo tee /etc/slopty-ci-image >/dev/null"
            ),
        ),
    ]
}

/// Screen Recording, Accessibility and event posting granted to [`SSH_RESPONSIBLE`] in both TCC
/// databases (SIP is off in the base), and the private window picker's monthly reminder put off
/// for good, as GitHub's images do for their runner.
fn screen_capture() -> String {
    let rows = [
        "kTCCServiceScreenCapture",
        "kTCCServiceAccessibility",
        "kTCCServicePostEvent",
        "kTCCServiceListenEvent",
    ]
    .map(|service| format!("('{service}', 1, '{SSH_RESPONSIBLE}', 2, 0, 1, NULL, 'UNUSED')"))
    .join(", ");
    format!(
        "user_db=$(sudo lsof -a -u $(id -u) -c tccd -Fn | sed -n 's|^n\\(/private/var/containers/Data/ProtectedSystem/.*/com.apple.TCC/TCC.db\\)$|\\1|p' | head -n 1); \
         for db in '/Library/Application Support/com.apple.TCC/TCC.db' \"$user_db\"; do \
         sudo sqlite3 \"$db\" \"INSERT OR REPLACE INTO access (service, client_type, client, auth_value, \
         auth_reason, auth_version, indirect_object_identifier_type, indirect_object_identifier) VALUES {rows};\"; done; \
         plist=\"$HOME/Library/Group Containers/group.com.apple.replayd/ScreenCaptureApprovals.plist\"; \
         mkdir -p \"$(dirname \"$plist\")\"; key={SSH_RESPONSIBLE}; \
         /usr/libexec/PlistBuddy -c \"Delete :$key\" \"$plist\" >/dev/null 2>&1 || true; \
         /usr/libexec/PlistBuddy -c \"Add :$key dict\" -c \"Add :$key:kScreenCapturePrivacyHintPolicy integer 2592000\" \"$plist\"; \
         plutil -replace \"$key.kScreenCapturePrivacyHintDate\" -date 3024-01-01T00:00:00Z \"$plist\"; \
         plutil -replace \"$key.kScreenCaptureApprovalLastAlerted\" -date 3024-01-01T00:00:00Z \"$plist\"; \
         killall cfprefsd 2>/dev/null || true"
    )
}

/// Push the image as `<date>` and `latest`, again while a layer fails, and pin its digest.
fn push(home: &Utf8Path, lan: Option<&str>) -> Result<()> {
    for var in ["TART_REGISTRY_USERNAME", "TART_REGISTRY_PASSWORD"] {
        ensure!(std::env::var_os(var).is_some(), "{var} is not set: Zot's push account");
    }
    let proxy = lan.map(LanProxy::start).transpose()?;
    let (repository, base) = match &proxy {
        Some(_) => (format!("{LAN_PROXY}/{PATH_IN_REGISTRY}"), format!("http://{LAN_PROXY}")),
        None => (REPOSITORY.to_owned(), format!("https://{REGISTRY}")),
    };
    let date = Command::new("date").args(["-u", "+%Y-%m-%d"]).output().context("date")?;
    let date = String::from_utf8(date.stdout)?.trim().to_owned();
    let dated = format!("{repository}:{date}");
    let latest = format!("{repository}:latest");
    let mut args = vec![
        "push",
        NAME,
        &dated,
        &latest,
        "--concurrency",
        PUSH_CONCURRENCY,
        "--chunk-size",
        PUSH_CHUNK_MB,
        "--label",
        "org.aislopware.slopty.image=ci",
    ];
    if proxy.is_some() {
        args.push("--insecure");
    }
    let started = Instant::now();
    let mut attempt = 1_u32;
    while let Err(e) = tart_shown(home, &args) {
        ensure!(attempt < PUSHES, "{e:#}, {PUSHES} times");
        println!("  {e:#}: pushing again, from the layers Zot lacks");
        attempt = attempt.saturating_add(1);
    }
    let took = started.elapsed();
    let (digest, size) = digest(&base, &date)?;
    drop(proxy);
    let pinned = format!("{REPOSITORY}@{digest}");
    let ci = crate::tools::repo_root()?.join("xtask/src/gitlab/ci.yml");
    std::fs::write(&ci, pin(&std::fs::read_to_string(&ci)?, &pinned)?)?;
    println!(
        "✓ pushed {REPOSITORY}:{date} ({:.1} GB compressed) in {took:.0?}: {pinned}, pinned in \
         {ci} (`cargo xtask gitlab publish` next)",
        gigabytes(size)
    );
    Ok(())
}

/// nginx on [`LAN_PROXY`], passing plain HTTP on to the registry at a LAN address under its own
/// name and certificate; stopped when dropped.
struct LanProxy {
    nginx: Child,
}

impl LanProxy {
    #[expect(
        clippy::disallowed_methods,
        reason = "xtask is a script, not a library; nginx starting"
    )]
    fn start(address: &str) -> Result<Self> {
        let dir = Utf8Path::new("/tmp/slopty-ci-image-lan");
        std::fs::create_dir_all(dir)?;
        let conf = dir.join("nginx.conf");
        std::fs::write(&conf, lan_proxy_config(dir, address))?;
        let nginx = Command::new("nginx")
            .args(["-p", dir.as_str(), "-c", conf.as_str()])
            .stdin(Stdio::null())
            .spawn()
            .context("nginx (`brew install nginx`)")?;
        let proxy = Self { nginx };
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(10) {
            let up = Command::new("curl")
                .args(["--silent", "--fail", "--output", "/dev/null"])
                .arg(format!("http://{LAN_PROXY}/livez"))
                .status()
                .is_ok_and(|status| status.success());
            if up {
                return Ok(proxy);
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        anyhow::bail!("{REGISTRY} at {address} did not answer through nginx: {dir}/error.log")
    }
}

impl Drop for LanProxy {
    fn drop(&mut self) {
        let _killed: std::io::Result<()> = self.nginx.kill();
        let _status: std::io::Result<_> = self.nginx.wait();
    }
}

/// nginx's configuration for [`LanProxy`], its files under `dir`: streamed both ways, the
/// certificate checked against [`REGISTRY`], and a `Location` naming the registry turned back
/// into the proxy's own.
fn lan_proxy_config(dir: &Utf8Path, address: &str) -> String {
    format!(
        r#"daemon off;
pid {dir}/nginx.pid;
error_log {dir}/error.log warn;
worker_processes 4;
events {{ worker_connections 256; }}
http {{
  client_max_body_size 0;
  client_body_temp_path {dir}/body;
  proxy_temp_path {dir}/proxy;
  proxy_request_buffering off;
  proxy_buffering off;
  proxy_http_version 1.1;
  access_log off;
  server {{
    listen {LAN_PROXY};
    location / {{
      proxy_pass https://{address};
      proxy_ssl_server_name on;
      proxy_ssl_name {REGISTRY};
      proxy_ssl_verify on;
      proxy_ssl_verify_depth 4;
      proxy_ssl_trusted_certificate /opt/homebrew/etc/ca-certificates/cert.pem;
      proxy_set_header Host {REGISTRY};
      proxy_set_header Connection "";
      proxy_redirect https://{REGISTRY}/ /;
      proxy_read_timeout 900s;
      proxy_send_timeout 900s;
    }}
  }}
}}
"#
    )
}

/// `ci` (`gitlab/ci.yml`) with its `SLOPTY_IMAGE` line naming `pinned`.
fn pin(ci: &str, pinned: &str) -> Result<String> {
    let line = ci
        .lines()
        .find(|line| line.trim_start().starts_with("SLOPTY_IMAGE: "))
        .context("gitlab/ci.yml pins no SLOPTY_IMAGE")?;
    let indent = line.len().saturating_sub(line.trim_start().len());
    Ok(ci.replacen(line, &format!("{}SLOPTY_IMAGE: {pinned}", " ".repeat(indent)), 1))
}

/// `bytes` in GB, for a log line.
#[expect(clippy::cast_precision_loss, reason = "a size in a log line")]
fn gigabytes(bytes: u64) -> f64 {
    bytes as f64 / 1e9
}

/// The digest of the image tagged `tag` and its size in bytes (its manifest, config and layers),
/// as the registry at `base` serves its manifest.
fn digest(base: &str, tag: &str) -> Result<(String, u64)> {
    #[derive(serde::Deserialize)]
    struct Blob {
        size: u64,
    }
    #[derive(serde::Deserialize)]
    struct Manifest {
        config: Blob,
        layers: Vec<Blob>,
    }
    let user = std::env::var("TART_REGISTRY_USERNAME")?;
    let password = std::env::var("TART_REGISTRY_PASSWORD")?;
    let mut curl = Command::new("curl")
        .args(["--silent", "--show-error", "--fail", "--dump-header", "-", "--config", "-"])
        .args(["--header", "Accept: application/vnd.oci.image.manifest.v1+json"])
        .arg(format!("{base}/v2/{PATH_IN_REGISTRY}/manifests/{tag}"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .context("curl")?;
    let quote = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
    let login = format!("user = \"{}:{}\"", quote(&user), quote(&password));
    std::io::Write::write_all(&mut curl.stdin.take().context("curl's stdin")?, login.as_bytes())?;
    let out = curl.wait_with_output()?;
    ensure!(out.status.success(), "the manifest of {tag}: {}", out.status);
    let answer = String::from_utf8(out.stdout)?;
    let (digest, body) = manifest_answer(&answer)?;
    let manifest: Manifest = serde_json::from_str(body).context("the manifest")?;
    let size = manifest.layers.iter().map(|layer| layer.size).sum::<u64>();
    let size = size.saturating_add(manifest.config.size).saturating_add(body.len().try_into()?);
    Ok((digest, size))
}

/// A manifest's digest (`Docker-Content-Digest`) and body, from curl's headers and body.
fn manifest_answer(answer: &str) -> Result<(String, &str)> {
    let (headers, body) = answer.split_once("\r\n\r\n").context("no end to the headers")?;
    let digest = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("docker-content-digest").then(|| value.trim().to_owned())
        })
        .context("no Docker-Content-Digest")?;
    ensure!(digest.starts_with("sha256:"), "a digest: {digest}");
    Ok((digest, body))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every step is a script `sh` reads: a quote lost in Rust's escapes fails here, not an hour
    /// into making the image.
    #[test]
    fn every_step_parses() {
        for (what, script) in steps("0123abcd") {
            let script = format!("export PATH={PATH}; set -e; {script}");
            let status = Command::new("/bin/sh").args(["-n", "-c", &script]).status().unwrap();
            assert!(status.success(), "{what}: {script}");
        }
    }

    #[test]
    fn a_manifest_answer_reads_its_digest() {
        let answer =
            "HTTP/1.1 200 OK\r\nContent-Type: x\r\ndocker-content-digest: sha256:ab\r\n\r\n{}";
        assert_eq!(manifest_answer(answer).unwrap(), ("sha256:ab".to_owned(), "{}"), "any case");
        assert!(manifest_answer("HTTP/1.1 200 OK\r\n\r\n{}").is_err(), "a digest or nothing");
    }

    #[test]
    fn the_lan_proxy_streams_to_the_registry_by_name() {
        let conf = lan_proxy_config(Utf8Path::new("/tmp/x"), "192.168.100.254");
        for line in [
            "proxy_pass https://192.168.100.254;",
            "proxy_ssl_name zot.jmango360.dev;",
            "proxy_set_header Host zot.jmango360.dev;",
            "proxy_redirect https://zot.jmango360.dev/ /;",
            "listen 127.0.0.1:5555;",
            "proxy_request_buffering off;",
        ] {
            assert!(conf.contains(line), "{line} in {conf}");
        }
        assert_eq!(conf.matches('{').count(), conf.matches('}').count(), "balanced: {conf}");
    }

    #[test]
    fn a_push_pins_its_digest() {
        let ci = include_str!("../gitlab/ci.yml");
        let pinned = format!("{REPOSITORY}@sha256:{}", "a".repeat(64));
        let new = pin(ci, &pinned).unwrap();
        assert!(new.contains(&format!("\n  SLOPTY_IMAGE: {pinned}\n")), "pinned in place");
        assert_eq!(new.lines().count(), ci.lines().count(), "nothing else moved");
        assert!(
            ci.contains(&format!("SLOPTY_IMAGE: {REPOSITORY}@sha256:")),
            "the pin names {REPOSITORY}"
        );
    }
}
