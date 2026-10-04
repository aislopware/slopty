//! A showcase of every surface filled the way a busy day fills it, rendered for a person to
//! review rather than held as goldens: three workers with shells that have run real-looking work
//! (a history graph, a build with warnings, a failed test run, a coloured listing, a chart in the
//! terminal's history), a file, a folder, a page, a note and a remote window; agents that need
//! the person, work, or are done; the navigator, the palette, the menus, the settings
//! and a toast; a Claude Code thread mid-turn with every kind of step, a request and a
//! questionnaire; and a project's board with tasks in every state and a merge queue.
//!
//! Everything shown is made by the test: a repository in a home of the run's own, stand-in
//! programs (`cargo`, `docker`) first on the shells' `PATH`, transcripts laid out as Claude Code
//! keeps them with their hooks handed to the worker's control socket or relayed by
//! `slopty hook`, and `slopty-stub-claude` for the project's agents. No agent runs, nothing is
//! typed into a remote window, and no golden is compared: the renders land in
//! `target/e2e/artifacts/showcase/`, light and dark, at the app's own window size. They run only
//! as their own case, `cargo xtask e2e showcase`; the app suite leaves them out.

use std::io::{BufRead as _, BufReader, Write as _};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use slopty_e2e::harness::{ProjectStack, SecondWorker, artifacts_dir, pinned_settings};
use slopty_e2e::{Command, Driver, Dump, Stack};

use crate::gallery::STEP;

/// The app's own window size (`apps/slopty`'s first window).
const WINDOW: (f32, f32) = (1280.0, 800.0);
/// Where the pointer rests before a render, over nothing that answers a hover.
const PARK: (f32, f32) = (1.0, 1.0);
/// How long the stand-in `cargo test` and `docker compose pull` take, so they end unwatched.
const SLOW: Duration = Duration::from_secs(7);

/// Where a render named `name` goes.
fn render_path(name: &str) -> PathBuf {
    let dir = artifacts_dir().join("showcase");
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(format!("{name}.png"))
}

/// How many stale frames this run has reported so far.
static STALE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// The app's state, also when its frame differs from the same state drawn from scratch: the
/// showcase counts every stale frame, says all the first few differ in and how the rest
/// begin, and goes on, since it is looked at, not judged.
async fn look(drv: &mut Driver) -> Dump {
    let dump = drv.dump_moving().await.unwrap();
    if let Some(stale) = &dump.stale {
        let seen = STALE.fetch_add(1, std::sync::atomic::Ordering::Relaxed).saturating_add(1);
        let lines = if seen <= 3 { usize::MAX } else { 2 };
        let first = stale.lines().take(lines).collect::<Vec<_>>().join("\n    ");
        println!("showcase: stale frame {seen}: {first}");
    }
    dump
}

/// Wait for `done`; past [`STEP`] say what did not happen and go on with the frame as it is.
async fn wait(drv: &mut Driver, what: &str, mut done: impl FnMut(&Dump) -> bool) -> Dump {
    let started = tokio::time::Instant::now();
    loop {
        let dump = look(drv).await;
        if done(&dump) {
            return dump;
        }
        if started.elapsed() > STEP {
            println!("showcase: timed out waiting for {what}");
            return dump;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Wait until every link has its round trip, every prompt its caret, and nothing moves.
async fn settled(drv: &mut Driver) {
    wait(drv, "the round trips and the carets", |d| d.rtt_sampled() && d.prompts_settled()).await;
    let started = tokio::time::Instant::now();
    let mut last = look(drv).await;
    loop {
        tokio::time::sleep(Duration::from_millis(120)).await;
        let next = look(drv).await;
        let still = |a: &[f32; 4], b: &[f32; 4]| a.iter().zip(b).all(|(x, y)| (x - y).abs() < 0.5);
        let same = next.items.len() == last.items.len()
            && next.items.iter().zip(&last.items).all(|(a, b)| still(&a.bounds, &b.bounds));
        if same || started.elapsed() > STEP {
            return;
        }
        last = next;
    }
}

/// Render the frame as it rests. A render whose frame is stale is tried again a few times, then
/// reported and skipped.
async fn shot(drv: &mut Driver, name: &str) {
    for _ in 0..4 {
        settled(drv).await;
        match drv.render(&render_path(name)).await {
            Ok(_) => {
                println!("showcase: rendered {name}");
                return;
            }
            Err(e) => {
                let first = e.to_string().lines().next().unwrap_or_default().to_owned();
                println!("showcase: {name} not rendered: {first}");
                tokio::time::sleep(Duration::from_millis(300)).await;
            }
        }
    }
}

/// Render `name` light, then dark, and come back to light.
async fn both(stack: &mut Stack, name: &str) {
    shot(&mut stack.driver, &format!("{name}-light")).await;
    stack.set_appearance("dark").unwrap();
    wait(&mut stack.driver, "the dark theme", |d| d.dark).await;
    shot(&mut stack.driver, &format!("{name}-dark")).await;
    stack.set_appearance("light").unwrap();
    wait(&mut stack.driver, "the light theme", |d| !d.dark).await;
}

/// The first shell, connected and prompted.
async fn first_shell(drv: &mut Driver) -> String {
    let dump = wait(drv, "the first shell with a prompt", |d| {
        d.status == "connected" && d.terminals.iter().any(slopty_e2e::TerminalInfo::reads_a_line)
    })
    .await;
    dump.terminals[0].session.clone()
}

/// Click the middle of the node with `role` and `label`, when there is one.
async fn click(drv: &mut Driver, role: &str, label: &str) -> bool {
    let dump = look(drv).await;
    let Some(node) = dump.a11y_node(role, Some(label)) else {
        println!("showcase: no {role} {label}");
        return false;
    };
    let [x, y, w, h] = node.bounds;
    drv.click(w.mul_add(0.5, x), h.mul_add(0.5, y)).await.unwrap();
    true
}

/// The labels of the nodes with `role`.
fn labels(d: &Dump, role: &str) -> Vec<String> {
    d.a11y.iter().filter(|n| n.role == role).filter_map(|n| n.label.clone()).collect()
}

/// The sessions of every terminal the app has a view of.
fn sessions(d: &Dump) -> Vec<String> {
    d.terminals.iter().map(|t| t.session.clone()).collect()
}

/// Open the login shell in a new column of the active workspace and give it the keyboard.
async fn new_shell(drv: &mut Driver) -> String {
    open_program(drv, &[]).await
}

/// Open `command` (the login shell when empty) in a new column; its session.
async fn open_program(drv: &mut Driver, command: &[&str]) -> String {
    let before = sessions(&look(drv).await);
    drv.open(command, 1).await.unwrap();
    let fresh = |t: &slopty_e2e::TerminalInfo| {
        !before.contains(&t.session) && (!command.is_empty() || t.reads_a_line())
    };
    let dump = wait(drv, "the new terminal", |d| d.terminals.iter().any(fresh)).await;
    let session = dump.terminals.iter().find(|t| fresh(t)).unwrap().session.clone();
    drv.reveal(&session).await.unwrap();
    session
}

/// Type `command` into `session`'s shell and wait until its output shows `until` and the shell
/// is back at its prompt.
async fn run(drv: &mut Driver, session: &str, command: &str, until: &str) {
    drv.reveal(session).await.unwrap();
    drv.type_text(command).await.unwrap();
    drv.keys("enter").await.unwrap();
    let ran = |t: &slopty_e2e::TerminalInfo| {
        t.rows.iter().any(|r| r.contains(until)) && t.at_prompt && t.reads_a_line()
    };
    let dump = wait(drv, command, |d| d.terminal(session).is_some_and(ran)).await;
    match dump.terminal(session) {
        Some(t) if !ran(t) => {
            let shown: Vec<&str> =
                t.rows.iter().map(|r| r.trim_end()).filter(|r| !r.is_empty()).collect();
            let tail = shown.iter().rev().take(6).rev().copied().collect::<Vec<_>>().join(" ⏎ ");
            println!("showcase: `{command}` shows: {tail} (at prompt: {})", t.at_prompt);
        }
        None => println!("showcase: `{command}`: its terminal has no view"),
        Some(_) => {}
    }
}

/// Type `command` into `session`'s shell and leave it running.
async fn start(drv: &mut Driver, session: &str, command: &str) {
    drv.reveal(session).await.unwrap();
    drv.type_text(command).await.unwrap();
    drv.keys("enter").await.unwrap();
}

/// A terminal that stands in for an agent's TUI: it names itself `title` as Claude Code names
/// a session by its task, then waits.
async fn agent_tile(drv: &mut Driver, title: &str) -> String {
    let named = format!("printf '\\033]0;{title}\\007'; exec cat");
    open_program(drv, &["sh", "-c", &named]).await
}

/// A canonical directory under `root`, made.
fn made(root: &Path, name: &str) -> PathBuf {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(&dir).unwrap()
}

/// Write `text` at `path`, its directories made.
fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// Write an executable script.
fn script(path: &Path, text: &str) {
    use std::os::unix::fs::PermissionsExt as _;
    write(path, text);
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// Seconds since the epoch, now.
fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

// ---------------------------------------------------------------------------------------------
// The stand-in programs and the repository.
// ---------------------------------------------------------------------------------------------

/// `cargo` as a busy workspace answers it: a build with two warnings, a test run that fails one
/// test after taking its time, a clean nextest run.
const CARGO: &str = r#"#!/bin/sh
g() { printf '\033[1;32m%12s\033[0m %s\n' "$1" "$2"; }
here=/Users/mira/code/atlas/crates
case "$1" in
build)
  for c in "tokio v1.48.0" "hyper v1.7.0" "sqlx-postgres v0.9.1" "tower-http v0.6.6" "axum v0.9.2"; do
    g Compiling "$c"
  done
  g Compiling "atlas-proto v0.4.2 ($here/proto)"
  g Compiling "atlas-core v0.4.2 ($here/core)"
  g Compiling "atlas-store v0.4.2 ($here/store)"
  g Compiling "atlas-api v0.4.2 ($here/api)"
  printf '\033[1;33mwarning\033[0m\033[1m: unused variable: `retry`\033[0m\n'
  printf '  \033[1;34m-->\033[0m crates/api/src/session.rs:88:13\n'
  printf '   \033[1;34m|\033[0m\n'
  printf '\033[1;34m88\033[0m \033[1;34m|\033[0m         let retry = Backoff::new(cfg.retry);\n'
  printf '   \033[1;34m|\033[0m             \033[1;33m^^^^^\033[0m \033[1;33mhelp: if this is intentional, prefix it with an underscore: `_retry`\033[0m\n'
  printf '   \033[1;34m|\033[0m\n'
  printf '   \033[1;34m=\033[0m \033[1mnote\033[0m: `#[warn(unused_variables)]` on by default\n\n'
  printf '\033[1;33mwarning\033[0m\033[1m: field `issued_at` is never read\033[0m\n'
  printf '  \033[1;34m-->\033[0m crates/store/src/lib.rs:41:5\n'
  printf '   \033[1;34m|\033[0m\n'
  printf '\033[1;34m38\033[0m \033[1;34m|\033[0m pub struct RefreshRow {\n'
  printf '   \033[1;34m|\033[0m            \033[1;34m----------\033[0m \033[1;34mfield in this struct\033[0m\n'
  printf '\033[1;34m...\033[0m\n'
  printf '\033[1;34m41\033[0m \033[1;34m|\033[0m     issued_at: OffsetDateTime,\n'
  printf '   \033[1;34m|\033[0m     \033[1;33m^^^^^^^^^\033[0m\n\n'
  printf '\033[1;33mwarning\033[0m: `atlas-api` (lib) generated 2 warnings (run `cargo fix --lib -p atlas-api` to apply 1 suggestion)\n'
  g Finished '`dev` profile [unoptimized + debuginfo] target(s) in 6.41s'
  ;;
test)
  g Compiling "atlas-api v0.4.2 ($here/api)"
  sleep 7
  g Finished '`test` profile [unoptimized + debuginfo] target(s) in 7.92s'
  printf '     \033[1;32mRunning\033[0m unittests src/lib.rs (target/debug/deps/atlas_api-3f9c2a1b7e5d4c60)\n\n'
  printf 'running 14 tests\n'
  for t in routes::tests::health_reports_the_version routes::tests::unknown_route_is_404 \
    session::tests::expired_token_is_401 session::tests::refresh_mints_a_new_token \
    session::tests::logout_revokes_the_family middleware::tests::trace_carries_the_session \
    middleware::tests::retry_keeps_the_idempotency_key store::tests::lookup_is_prepared_once \
    store::tests::rotate_is_atomic store::tests::revoke_family_spends_every_token \
    error::tests::store_errors_map_to_503 error::tests::auth_errors_map_to_401 \
    config::tests::defaults_read_from_env; do
    printf 'test %s ... \033[32mok\033[0m\n' "$t"
  done
  printf 'test session::tests::refresh_rotates_under_a_race ... \033[1;31mFAILED\033[0m\n\n'
  printf 'failures:\n\n---- session::tests::refresh_rotates_under_a_race stdout ----\n\n'
  printf "thread 'session::tests::refresh_rotates_under_a_race' panicked at crates/api/src/session.rs:212:9:\n"
  printf 'assertion `left != right` failed: the second refresh must not reuse the first token\n'
  printf '  left: "tok_9f2c41e0"\n right: "tok_9f2c41e0"\n'
  printf 'note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n\n\n'
  printf 'failures:\n    session::tests::refresh_rotates_under_a_race\n\n'
  printf 'test result: \033[1;31mFAILED\033[0m. 13 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.21s\n\n'
  printf '\033[1;31merror\033[0m: test failed, to rerun pass `-p atlas-api --lib`\n'
  exit 101
  ;;
nextest)
  g Finished '`test` profile [unoptimized + debuginfo] target(s) in 41.07s'
  printf '\033[1;32m    Starting\033[0m \033[1m212\033[0m tests across \033[1m9\033[0m binaries (3 skipped)\n'
  i=0
  for t in "atlas-core error::tests::display_is_stable" "atlas-core clock::tests::skew_is_bounded" \
    "atlas-store tests::rotate_is_atomic" "atlas-store tests::lookup_is_prepared_once" \
    "atlas-store tests::migrations_apply_in_order" "atlas-api session::tests::expired_token_is_401" \
    "atlas-api routes::tests::health_reports_the_version" "atlas-api middleware::tests::trace_carries_the_session" \
    "atlas-api::ws ws::tests::ping_keeps_the_socket" "atlas-api::ws ws::tests::close_frame_is_echoed"; do
    i=$((i + 3))
    printf '\033[1;32m        PASS\033[0m [   0.%03ds] \033[1;35m%s\033[0m\n' "$((i * 7))" "$t"
  done
  printf '\033[1;33m        SLOW\033[0m [> 60.000s] \033[1;35matlas-api::ws\033[0m ws::tests::reconnect_after_server_restart\n'
  printf '\033[1;32m        PASS\033[0m [  61.204s] \033[1;35matlas-api::ws\033[0m ws::tests::reconnect_after_server_restart\n'
  printf '\033[1;32m        PASS\033[0m [   0.044s] \033[1;35matlas-api::ws\033[0m ws::tests::backpressure_drops_oldest\n'
  printf '────────────\n'
  printf '     \033[1;32mSummary\033[0m [  64.982s] \033[1m212\033[0m tests run: \033[1m212\033[0m \033[1;32mpassed\033[0m (1 \033[1;33mslow\033[0m), \033[1m3\033[0m \033[1;33mskipped\033[0m\n'
  ;;
*)
  printf '\033[1;31merror\033[0m: no such command: `%s`\n' "$1"
  exit 101
  ;;
esac
"#;

/// `docker` on the dev box: the stack's containers, a service's log, a pull that takes its time.
const DOCKER: &str = r#"#!/bin/sh
case "$1 $2" in
"ps "*|"ps")
  printf 'CONTAINER ID   IMAGE                          COMMAND                  CREATED        STATUS                    PORTS                      NAMES\n'
  printf '3f9c2a1b7e5d   ghcr.io/atlas/api:0.4.2        "/usr/local/bin/atla…"   2 hours ago    Up 2 hours (healthy)      0.0.0.0:8080->8080/tcp     atlas-api\n'
  printf '8b1d7e44c093   ghcr.io/atlas/worker:0.4.2     "/usr/local/bin/atla…"   2 hours ago    Up 2 hours                                           atlas-worker\n'
  printf 'c27a90f1e6b2   postgres:18.1                  "docker-entrypoint.s…"   3 days ago     Up 3 days (healthy)       0.0.0.0:5432->5432/tcp     atlas-db\n'
  printf '51e0bb2d8f47   redis:8.2-alpine               "docker-entrypoint.s…"   3 days ago     Up 3 days                 0.0.0.0:6379->6379/tcp     atlas-cache\n'
  printf 'e9f3a6c1d5b0   grafana/otel-lgtm:0.11         "/otel-lgtm/run-all.…"   3 days ago     Up 3 days                 0.0.0.0:3000->3000/tcp     atlas-otel\n'
  ;;
"compose logs")
  c='\033[36matlas-api  |\033[0m'
  printf "$c 2026-10-02T09:41:07.118Z \033[32m INFO\033[0m request{method=POST path=/v1/sessions/refresh session=s_7Qm2}: atlas_api::session: refreshed family=f_19ac rotated=true\n"
  printf "$c 2026-10-02T09:41:07.121Z \033[32m INFO\033[0m request{method=POST path=/v1/sessions/refresh}: tower_http::trace: finished status=200 latency=3.4ms\n"
  printf "$c 2026-10-02T09:41:08.502Z \033[32m INFO\033[0m request{method=GET path=/v1/health}: tower_http::trace: finished status=200 latency=0.2ms\n"
  printf "$c 2026-10-02T09:41:09.733Z \033[33m WARN\033[0m request{method=POST path=/v1/sessions/refresh session=s_7Qm2}: atlas_api::session: token already spent family=f_19ac\n"
  printf "$c 2026-10-02T09:41:09.734Z \033[33m WARN\033[0m atlas_api::session: revoked family f_19ac (3 tokens) after a replay\n"
  printf "$c 2026-10-02T09:41:09.735Z \033[32m INFO\033[0m request{method=POST path=/v1/sessions/refresh}: tower_http::trace: finished status=401 latency=2.9ms\n"
  printf "$c 2026-10-02T09:41:12.040Z \033[31mERROR\033[0m atlas_store::pool: connection reset by peer, retrying in 250ms attempt=1\n"
  printf "$c 2026-10-02T09:41:12.297Z \033[32m INFO\033[0m atlas_store::pool: reconnected to postgres://atlas-db:5432/atlas\n"
  printf "$c 2026-10-02T09:41:15.880Z \033[32m INFO\033[0m request{method=GET path=/v1/sessions/s_88Kp}: tower_http::trace: finished status=200 latency=1.1ms\n"
  printf "$c 2026-10-02T09:41:16.204Z \033[34mDEBUG\033[0m atlas_api::middleware: retry kept idempotency-key=7c1e4b0a attempt=2\n"
  ;;
"compose pull")
  for s in api worker db cache otel; do printf ' \033[32m✔\033[0m %s Pulled\n' "$s"; done
  sleep 7
  printf '\033[32m✔\033[0m Images are up to date\n'
  ;;
*)
  printf 'docker: unknown command: %s\n' "$*"
  exit 1
  ;;
esac
"#;

/// The script that draws the latency chart into the terminal's history.
const PLOT: &str = r#"#!/bin/sh
printf '\033[1mRequest latency\033[0m · atlas-api · last 24 h · \033[32m■ p50\033[0m \033[33m■ p95\033[0m\n'
cat "$(dirname "$0")/latency.kitty"
printf '\n\033[2m00:00         06:00         12:00         18:00          now\033[0m\n'
"#;

/// The bars of the chart, in pixels: the median and the tail of each hour.
const P50: [u32; 24] = [
    22, 20, 19, 18, 18, 21, 30, 44, 52, 49, 47, 45, 43, 46, 50, 55, 61, 58, 49, 41, 35, 30, 27, 24,
];
const P95: [u32; 24] = [
    48, 44, 40, 39, 41, 47, 63, 88, 101, 96, 90, 86, 84, 89, 97, 104, 112, 108, 95, 82, 70, 62, 57,
    51,
];

/// The chart as kitty graphics: raw RGBA sent in chunks, placed over 48 columns and 9 rows.
fn latency_kitty() -> String {
    let (w, h) = (360_u32, 120_u32);
    let picture = image::RgbaImage::from_fn(w, h, |x, y| {
        let up = h.saturating_sub(1).saturating_sub(y);
        let bar = usize::try_from(x.checked_div(15).unwrap_or(0)).unwrap_or(usize::MAX);
        let inside = (2..12).contains(&x.checked_rem(15).unwrap_or(0));
        match (P50.get(bar), P95.get(bar)) {
            (Some(&p50), Some(_)) if inside && up < p50 => image::Rgba([52, 199, 89, 255]),
            (Some(_), Some(&p95)) if inside && (p95.saturating_sub(3)..p95).contains(&up) => {
                image::Rgba([255, 159, 10, 255])
            }
            _ if up.checked_rem(30) == Some(0) => image::Rgba([128, 128, 128, 70]),
            _ => image::Rgba([0, 0, 0, 0]),
        }
    });
    let data = data_encoding::BASE64.encode(&picture.into_raw());
    let chunks: Vec<&str> =
        data.as_bytes().chunks(4096).map(|c| std::str::from_utf8(c).unwrap()).collect();
    let last = chunks.len().saturating_sub(1);
    let mut out = String::new();
    for (n, chunk) in chunks.iter().enumerate() {
        let more = u8::from(n != last);
        let head = if n == 0 {
            format!("a=T,f=32,s={w},v={h},c=48,r=9,q=2,m={more}")
        } else {
            format!("m={more}")
        };
        out.push_str("\x1b_G");
        out.push_str(&head);
        out.push(';');
        out.push_str(chunk);
        out.push_str("\x1b\\");
    }
    out
}

/// The stand-in programs in `root/bin`, and a zsh configuration in `root/zsh` that puts them
/// first on the shells' `PATH`: after the login profile, which puts the system's own (where a
/// real `docker` may be) ahead of what the shell inherited. The `PATH` the daemons start with,
/// and the `ZDOTDIR` the shells take.
fn shells(root: &Path) -> (String, String) {
    let bin = made(root, "bin");
    script(&bin.join("cargo"), CARGO);
    script(&bin.join("docker"), DOCKER);
    let zsh = made(root, "zsh");
    let first = bin.to_string_lossy();
    write(
        &zsh.join(".zshrc"),
        &format!("PROMPT='%~ %# '\nexport PATH=\"{first}:$PATH\"\nexport CLICOLOR=1\n"),
    );
    let path = slopty_testkit::env::path_with(&bin).to_string_lossy().into_owned();
    (path, zsh.to_string_lossy().into_owned())
}

/// A person who commits.
type Who = (&'static str, &'static str);
const MIRA: Who = ("Mira Okafor", "mira@atlas.dev");
const JONAS: Who = ("Jonas Lindqvist", "jonas@atlas.dev");
const PRIYA: Who = ("Priya Raman", "priya@atlas.dev");
const BOT: Who = ("dependabot[bot]", "49699333+dependabot[bot]@users.noreply.github.com");

/// The repository the day's work is in.
struct Repo {
    dir: PathBuf,
    now: u64,
}

impl Repo {
    fn git(&self, who: Who, minutes_ago: u64, args: &[&str]) {
        let when = format!("{} +0200", self.now.saturating_sub(minutes_ago.saturating_mul(60)));
        let out = std::process::Command::new("git")
            .current_dir(&self.dir)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", who.0)
            .env("GIT_AUTHOR_EMAIL", who.1)
            .env("GIT_COMMITTER_NAME", who.0)
            .env("GIT_COMMITTER_EMAIL", who.1)
            .env("GIT_AUTHOR_DATE", &when)
            .env("GIT_COMMITTER_DATE", &when)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    fn put(&self, files: &[(&str, &str)]) {
        for (path, text) in files {
            write(&self.dir.join(path), text);
        }
    }

    fn commit(&self, who: Who, minutes_ago: u64, message: &str, files: &[(&str, &str)]) {
        self.put(files);
        self.git(who, minutes_ago, &["add", "-A"]);
        self.git(who, minutes_ago, &["commit", "-q", "-m", message]);
    }
}

const ROUTES_RS: &str = r#"//! The HTTP surface: every route the API answers, and the layers around them.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::TraceLayer;

use crate::error::ApiError;
use crate::middleware::{RetryLayer, SessionSpan};
use crate::session::{RefreshRequest, SessionService, Tokens};

/// How long a request may take before the client gets a 503.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// What every handler shares.
#[derive(Clone)]
pub struct AppState {
    pub sessions: Arc<SessionService>,
    pub version: &'static str,
}

/// The router, with tracing, retries and a timeout on every route.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/v1/health", get(health))
        .route("/v1/sessions", post(create_session))
        .route("/v1/sessions/refresh", post(refresh))
        .route("/v1/sessions/{id}", get(session).delete(logout))
        .layer(RetryLayer::idempotent(3))
        .layer(TimeoutLayer::new(REQUEST_TIMEOUT))
        .layer(TraceLayer::new_for_http().make_span_with(SessionSpan))
        .with_state(state)
}

async fn health(State(state): State<AppState>) -> Json<serde_json::Value> {
    Json(serde_json::json!({ "status": "ok", "version": state.version }))
}

async fn refresh(
    State(state): State<AppState>,
    Json(request): Json<RefreshRequest>,
) -> Result<Json<Tokens>, ApiError> {
    let session = state.sessions.refresh(&request.refresh_token).await?;
    Ok(Json(session.tokens()))
}

async fn session(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let found = state.sessions.find(&id).await?.ok_or(ApiError::NotFound)?;
    Ok(Json(found.public()))
}

async fn logout(State(state): State<AppState>, Path(id): Path<String>) -> StatusCode {
    match state.sessions.revoke(&id).await {
        Ok(()) => StatusCode::NO_CONTENT,
        Err(_) => StatusCode::NOT_FOUND,
    }
}

async fn create_session() -> StatusCode {
    StatusCode::NOT_IMPLEMENTED
}
"#;

const SESSION_RS: &str = r#"//! Sessions: minting, refreshing and revoking the tokens a client holds.

use atlas_store::{RefreshRow, SessionStore, StoreError};
use rand::rngs::StdRng;

use crate::clock::Clock;

/// Why a refresh was refused.
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("the refresh token has expired")]
    Expired,
    #[error(transparent)]
    Store(#[from] StoreError),
}

pub struct SessionService {
    store: SessionStore,
    clock: Clock,
    rng: StdRng,
}

impl SessionService {
    /// Trade a refresh token for a new pair.
    pub async fn refresh(&self, presented: &RefreshToken) -> Result<Session, AuthError> {
        let row = self.store.find_refresh(presented).await?;
        if row.expires_at <= self.clock.now() {
            return Err(AuthError::Expired);
        }
        let next = RefreshToken::mint(&mut self.rng);
        self.store.replace_refresh(row.id, &next).await?;
        Ok(Session::new(row.user, next))
    }
}
"#;

const MIDDLEWARE_RS: &str = r#"//! Layers every route shares: a span per request, and retries that keep their key.

use axum::http::{HeaderName, Request};

/// The header a retried request keeps, so the server can tell a retry from a new request.
pub const IDEMPOTENCY_KEY: HeaderName = HeaderName::from_static("idempotency-key");

pub struct RetryLayer {
    attempts: u32,
}

impl RetryLayer {
    pub const fn idempotent(attempts: u32) -> Self {
        Self { attempts }
    }

    fn retry<B: Clone>(&self, request: &Request<B>) -> Request<B> {
        let mut again = Request::new(request.body().clone());
        *again.uri_mut() = request.uri().clone();
        again
    }
}
"#;

const CI_YML: &str = "name: ci\non: [push, pull_request]\njobs:\n  test:\n    strategy:\n      matrix:\n        os: [ubuntu-24.04, macos-26]\n    runs-on: ${{ matrix.os }}\n    steps:\n      - uses: actions/checkout@v5\n      - run: cargo clippy --workspace --all-targets -- -D warnings\n      - run: cargo nextest run --workspace\n";

/// The repository: a Rust service with a week of history from three people and a bot, a merged
/// branch, a tag, an open branch, and a working tree mid-change.
fn make_repo(dir: &Path) -> Repo {
    std::fs::create_dir_all(dir).unwrap();
    let repo = Repo { dir: dir.to_path_buf(), now: now() };
    repo.git(MIRA, 9000, &["init", "-q", "-b", "main"]);
    let manifest = "[workspace]\nmembers = [\"crates/*\"]\nresolver = \"3\"\n\n[workspace.package]\nversion = \"0.4.2\"\nedition = \"2024\"\n";
    repo.commit(
        MIRA,
        8900,
        "chore: start the workspace with core, store and api",
        &[
            ("Cargo.toml", manifest),
            ("README.md", "# atlas\n\nSessions and tokens for the Atlas apps.\n"),
            (
                "crates/core/Cargo.toml",
                "[package]\nname = \"atlas-core\"\nversion.workspace = true\n",
            ),
            ("crates/core/src/lib.rs", "pub mod clock;\npub mod error;\n"),
            (
                "crates/store/Cargo.toml",
                "[package]\nname = \"atlas-store\"\nversion.workspace = true\n",
            ),
            (
                "crates/api/Cargo.toml",
                "[package]\nname = \"atlas-api\"\nversion.workspace = true\n",
            ),
            (
                "crates/proto/Cargo.toml",
                "[package]\nname = \"atlas-proto\"\nversion.workspace = true\n",
            ),
            ("crates/proto/src/lib.rs", "//! Wire types.\n"),
            ("rust-toolchain.toml", "[toolchain]\nchannel = \"1.98\"\n"),
        ],
    );
    repo.commit(
        JONAS,
        7300,
        "feat(store): a Postgres session store behind a trait",
        &[("crates/store/src/lib.rs", "pub struct SessionStore;\npub struct RefreshRow;\n")],
    );
    repo.commit(
        MIRA,
        7100,
        "feat(api): /v1/health and the session routes",
        &[
            ("crates/api/src/routes.rs", ROUTES_RS),
            ("crates/api/src/lib.rs", "pub mod routes;\npub mod session;\npub mod middleware;\n"),
        ],
    );
    repo.commit(
        PRIYA,
        5800,
        "ci: clippy and nextest on Linux and macOS",
        &[(".github/workflows/ci.yml", CI_YML)],
    );
    repo.commit(BOT, 5600, "build(deps): bump tokio from 1.47.1 to 1.48.0", &[(
        "Cargo.lock",
        "# This file is automatically @generated by Cargo.\nversion = 4\n\n[[package]]\nname = \"tokio\"\nversion = \"1.48.0\"\n",
    )]);
    repo.commit(
        JONAS,
        4300,
        "fix(api): answer an expired refresh with 401, not 500",
        &[("crates/api/src/session.rs", SESSION_RS)],
    );
    repo.commit(
        PRIYA,
        2900,
        "perf(store): prepare the session lookup once per pool",
        &[(
            "crates/store/src/lib.rs",
            "pub struct SessionStore;\npub struct RefreshRow;\n// prepared once per pool\n",
        )],
    );
    repo.git(PRIYA, 2900, &["tag", "v0.4.2"]);
    repo.git(MIRA, 1800, &["checkout", "-q", "-b", "feat/session-refresh"]);
    repo.commit(
        MIRA,
        1800,
        "feat(api): rotate the refresh token on every use",
        &[("crates/api/src/session.rs", &SESSION_RS.replace("replace_refresh", "rotate_refresh"))],
    );
    repo.commit(
        MIRA,
        1560,
        "test(api): a refresh that races another",
        &[(
            "crates/api/tests/refresh_race.rs",
            "#[tokio::test]\nasync fn two_refreshes_race() {}\n",
        )],
    );
    repo.git(MIRA, 1700, &["checkout", "-q", "main"]);
    repo.commit(PRIYA, 1680, "docs: how to run the stack on a laptop", &[(
        "README.md",
        "# atlas\n\nSessions and tokens for the Atlas apps.\n\n## Run it\n\n    docker compose up -d\n    cargo run -p atlas-api\n",
    )]);
    repo.git(
        MIRA,
        1200,
        &[
            "merge",
            "-q",
            "--no-ff",
            "feat/session-refresh",
            "-m",
            "Merge branch 'feat/session-refresh'",
        ],
    );
    repo.commit(
        JONAS,
        360,
        "feat(api): trace every request with its session id",
        &[("crates/api/src/middleware.rs", MIDDLEWARE_RS)],
    );
    repo.git(JONAS, 300, &["branch", "fix/clock-skew"]);
    repo.commit(
        MIRA,
        180,
        "refactor(core): one error type for the HTTP layer",
        &[("crates/core/src/error.rs", "#[derive(Debug, thiserror::Error)]\npub enum Error {}\n")],
    );
    repo.commit(
        PRIYA,
        45,
        "chore(scripts): plot request latency in the terminal",
        &[("scripts/plot-latency", PLOT), ("scripts/latency.kitty", &latency_kitty())],
    );
    script(&dir.join("scripts/plot-latency"), PLOT);
    repo.git(MIRA, 0, &[
        "config",
        "alias.lg",
        "log --graph --pretty=tformat:'%C(yellow)%h%C(reset)%C(auto)%d%C(reset) %s %C(dim)%ar · %an%C(reset)'",
    ]);
    repo.git(MIRA, 0, &["config", "core.pager", "cat"]);
    // The working tree mid-change: an edit, a staged manifest, a file not yet added.
    repo.put(&[
        ("crates/api/src/session.rs", &SESSION_RS.replace(
            "        let row = self.store.find_refresh(presented).await?;\n",
            "        let mut tx = self.store.begin().await?;\n        let row = self.store.find_refresh_for_update(&mut tx, presented).await?;\n",
        )),
        ("crates/api/src/retry.rs", "//! Backoff for the store's transient errors.\n"),
        ("Cargo.toml", &format!("{manifest}\n[workspace.dependencies]\nbackoff = \"0.4\"\n")),
    ]);
    repo.git(MIRA, 0, &["add", "Cargo.toml"]);
    repo
}

/// A page about the service, served here for the browser tile.
const PAGE: &str = r#"<!doctype html><html><head><meta charset="utf-8"><title>Atlas · Service health</title>
<style>
body{font:14px/1.5 -apple-system,system-ui,sans-serif;margin:0;background:#f6f7f9;color:#1d2330}
header{padding:18px 24px;background:#fff;border-bottom:1px solid #e3e6eb;display:flex;align-items:center;gap:12px}
h1{font-size:17px;margin:0}.pill{border-radius:999px;padding:2px 10px;font-size:12px;font-weight:600}
.ok{background:#e3f6e8;color:#1c7c3a}.warn{background:#fff3dc;color:#9a5b00}.bad{background:#fde4e2;color:#b42318}
main{padding:20px 24px;display:grid;grid-template-columns:repeat(3,1fr);gap:14px}
.card{background:#fff;border:1px solid #e3e6eb;border-radius:10px;padding:14px}
.card b{font-size:22px;display:block}.muted{color:#6b7280;font-size:12px}
table{grid-column:1/-1;width:100%;border-collapse:collapse;background:#fff;border:1px solid #e3e6eb;border-radius:10px}
td,th{padding:9px 14px;border-bottom:1px solid #eef0f3;text-align:left}th{font-size:12px;color:#6b7280;font-weight:600}
.bar{display:flex;gap:2px;align-items:flex-end;height:36px;margin-top:8px}.bar i{flex:1;background:#34c759;border-radius:2px}
</style></head><body>
<header><h1>Atlas · Service health</h1><span class="pill ok">All systems normal</span><span class="muted">eu-west · updated 09:41</span></header>
<main>
<div class="card"><span class="muted">Requests / min</span><b>18 420</b><div class="bar"><i style="height:40%"></i><i style="height:55%"></i><i style="height:48%"></i><i style="height:62%"></i><i style="height:70%"></i><i style="height:66%"></i><i style="height:81%"></i><i style="height:77%"></i><i style="height:90%"></i><i style="height:84%"></i></div></div>
<div class="card"><span class="muted">p95 latency</span><b>104 ms</b><span class="pill warn">+12% since 06:00</span></div>
<div class="card"><span class="muted">Refresh replays blocked today</span><b>37</b><span class="muted">families revoked: 11</span></div>
<table><tr><th>Service</th><th>Version</th><th>Status</th><th>Uptime</th><th>Errors (1h)</th></tr>
<tr><td>atlas-api</td><td>0.4.2</td><td><span class="pill ok">healthy</span></td><td>2 h 14 m</td><td>3</td></tr>
<tr><td>atlas-worker</td><td>0.4.2</td><td><span class="pill ok">healthy</span></td><td>2 h 14 m</td><td>0</td></tr>
<tr><td>postgres</td><td>18.1</td><td><span class="pill ok">healthy</span></td><td>3 d</td><td>1</td></tr>
<tr><td>redis</td><td>8.2</td><td><span class="pill warn">memory 82%</span></td><td>3 d</td><td>0</td></tr>
<tr><td>ws-gateway</td><td>0.3.9</td><td><span class="pill bad">degraded</span></td><td>41 m</td><td>58</td></tr>
</table></main></body></html>"#;

/// Serve [`PAGE`] on a port of loopback; the port.
fn serve_page() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut reader = BufReader::new(&stream);
            let mut line = String::new();
            while reader.read_line(&mut line).is_ok_and(|n| n > 2) {
                line.clear();
            }
            let answer = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{PAGE}",
                PAGE.len()
            );
            let _sent = (&stream).write_all(answer.as_bytes());
        }
    });
    port
}

// ---------------------------------------------------------------------------------------------
// Transcripts, as Claude Code keeps them.
// ---------------------------------------------------------------------------------------------

/// A transcript being written: each record chained to the last, stamped a few seconds apart.
struct Log {
    out: String,
    n: u64,
    parent: Option<String>,
    session: String,
    cwd: String,
    /// When the last record was written, in seconds since the epoch.
    at: u64,
    /// A subagent's id, whose records are a sidechain of the session.
    agent: Option<String>,
    /// The leading part of every uuid, so two logs of a session never share one.
    stem: &'static str,
}

impl Log {
    fn new(session: &str, cwd: &Path, at: u64) -> Self {
        Self {
            out: String::new(),
            n: 0,
            parent: None,
            session: session.to_owned(),
            cwd: cwd.to_string_lossy().into_owned(),
            at,
            agent: None,
            stem: "00000000-0000-4000-9000",
        }
    }

    fn push(&mut self, mut record: Value) {
        self.n = self.n.saturating_add(1);
        self.at = self.at.saturating_add(9);
        let uuid = format!("{}-{:012}", self.stem, self.n);
        record["uuid"] = json!(uuid);
        record["parentUuid"] = self.parent.clone().map_or(Value::Null, Value::String);
        record["timestamp"] = json!(stamp(self.at));
        record["sessionId"] = json!(self.session);
        record["cwd"] = json!(self.cwd);
        record["gitBranch"] = json!("main");
        record["version"] = json!("2.1.283");
        record["isSidechain"] = json!(self.agent.is_some());
        if let Some(agent) = &self.agent {
            record["agentId"] = json!(agent);
        }
        self.out.push_str(&record.to_string());
        self.out.push('\n');
        self.parent = Some(uuid);
    }

    fn prompt(&mut self, text: &str) {
        self.push(json!({
            "type": "user", "permissionMode": "acceptEdits", "userType": "external",
            "message": { "role": "user", "content": text },
        }));
    }

    fn assistant(&mut self, content: Value) {
        let n = self.n;
        let mut record = json!({ "type": "assistant", "message": {
            "id": format!("msg_01{n:010}"), "role": "assistant", "model": "claude-opus-5-5",
            "stop_reason": null,
            "usage": { "input_tokens": 6, "cache_read_input_tokens": 58_214,
                       "cache_creation_input_tokens": 1_204, "output_tokens": 612 },
        }});
        record["message"]["content"] = Value::Array(vec![content]);
        self.push(record);
    }

    fn think(&mut self, text: &str) {
        self.assistant(json!({ "type": "thinking", "thinking": text, "signature": "sig" }));
    }

    fn say(&mut self, text: &str) {
        self.assistant(json!({ "type": "text", "text": text }));
    }

    fn call(&mut self, id: &str, name: &str, input: Value) {
        let mut call = json!({ "type": "tool_use", "id": id, "name": name });
        call["input"] = input;
        self.assistant(call);
    }

    fn result(&mut self, id: &str, content: Value, structured: Value, error: bool) {
        let mut result = json!({ "type": "tool_result", "tool_use_id": id, "is_error": error });
        result["content"] = content;
        let mut record = json!({ "type": "user", "message": { "role": "user" } });
        record["message"]["content"] = Value::Array(vec![result]);
        record["toolUseResult"] = structured;
        self.push(record);
    }

    /// A call and what it gave back.
    fn tool(&mut self, id: &str, name: &str, input: Value, content: Value, structured: Value) {
        self.call(id, name, input);
        self.result(id, content, structured, false);
    }
}

/// `unix` (seconds since the epoch) as a transcript stamps a record, in UTC.
#[expect(clippy::arithmetic_side_effects, reason = "a calendar date from a count of days")]
fn stamp(unix: u64) -> String {
    let days = i64::try_from(unix / 86_400).unwrap();
    let secs = unix % 86_400;
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.000Z",
        secs / 3_600,
        (secs / 60) % 60,
        secs % 60
    )
}

/// `text` as `Read` gives it back: each line numbered from `first`.
fn numbered(text: &str, first: usize) -> String {
    text.lines()
        .enumerate()
        .map(|(n, line)| format!("{:>6}\t{line}", n.saturating_add(first)))
        .collect::<Vec<_>>()
        .join("\n")
}

/// A hunk of a structured patch, its counts taken from its lines.
fn hunk(old_start: usize, new_start: usize, lines: &[&str]) -> Value {
    let old = lines.iter().filter(|l| !l.starts_with('+')).count();
    let new = lines.iter().filter(|l| !l.starts_with('-')).count();
    json!({ "oldStart": old_start, "oldLines": old, "newStart": new_start, "newLines": new,
            "lines": lines })
}

/// An `Edit`'s record of what it did.
fn edited(path: &str, old: &str, new: &str, hunks: &[Value]) -> Value {
    json!({ "filePath": path, "oldString": old, "newString": new, "structuredPatch": hunks,
            "userModified": false, "replaceAll": false })
}

/// A `Bash` call's record.
fn ran(stdout: &str) -> Value {
    json!({ "stdout": stdout, "stderr": "", "interrupted": false, "isImage": false })
}

/// The id of the subagent the long session delegates to.
const SURVEY_AGENT: &str = "a3f9c2e17b5d40c8";

/// The long session: a settled turn that found and fixed a race with every kind of step (a
/// search, a read, a web page, a subagent, a multi-hunk edit, a new file, a passing and a failing
/// command, a plan of tasks) and an answer in Markdown with a table and code; then a second turn
/// under way whose last command waits on the person. Returns the main transcript and the
/// subagent's.
#[expect(clippy::too_many_lines, reason = "one session, told in order")]
fn long_session(session: &str, cwd: &Path) -> (String, String) {
    let file = |rel: &str| cwd.join(rel).to_string_lossy().into_owned();
    let session_rs = file("crates/api/src/session.rs");
    let mut log = Log::new(session, cwd, now().saturating_sub(35 * 60));
    log.prompt(
        "The refresh endpoint hands back the same token when two refreshes race. Find out why \
         and fix it without changing the API. Check what the OAuth security guidance says about \
         replayed refresh tokens too.",
    );
    log.think(
        "Two refreshes of one token that race could both read the row before either writes. \
         If `refresh` reads with a plain SELECT and writes afterwards, the second writer wins \
         and the first client is left holding a token the store no longer knows. I should find \
         every caller of the store first, then read `session.rs`.",
    );
    log.tool(
        "toolu_01",
        "Glob",
        json!({ "pattern": "crates/**/src/**/*.rs" }),
        json!("crates/api/src/routes.rs\ncrates/api/src/session.rs\ncrates/api/src/middleware.rs\ncrates/store/src/lib.rs\ncrates/core/src/error.rs\ncrates/core/src/clock.rs"),
        json!({ "filenames": ["crates/api/src/routes.rs", "crates/api/src/session.rs",
            "crates/api/src/middleware.rs", "crates/store/src/lib.rs", "crates/core/src/error.rs",
            "crates/core/src/clock.rs"], "durationMs": 9, "numFiles": 6, "truncated": false }),
    );
    let found = "crates/api/src/session.rs:26:        let row = self.store.find_refresh(presented).await?;\ncrates/api/src/session.rs:31:        self.store.replace_refresh(row.id, &next).await?;\ncrates/store/src/lib.rs:88:    pub async fn find_refresh(&self, token: &RefreshToken) -> Result<RefreshRow, StoreError> {\ncrates/store/src/lib.rs:117:    pub async fn replace_refresh(&self, id: RowId, next: &RefreshToken) -> Result<(), StoreError> {";
    log.tool(
        "toolu_02",
        "Grep",
        json!({ "pattern": "find_refresh|replace_refresh", "output_mode": "content", "-n": true }),
        json!(found),
        json!({ "mode": "content", "numFiles": 2, "filenames": [], "content": found,
                "numLines": 4 }),
    );
    log.tool(
        "toolu_03",
        "Read",
        json!({ "file_path": session_rs }),
        json!(numbered(SESSION_RS, 1)),
        json!({ "type": "text", "file": { "filePath": session_rs, "content": SESSION_RS,
            "numLines": SESSION_RS.lines().count(), "startLine": 1,
            "totalLines": SESSION_RS.lines().count() } }),
    );
    let survey = "Every caller of the session store, and how each one touches a refresh row:\n\n\
        - `SessionService::refresh` reads with `find_refresh` (a plain `SELECT`) and writes with \
        `replace_refresh` in a second statement, outside any transaction.\n\
        - `SessionService::revoke` deletes by session id and never looks at the refresh family.\n\
        - `atlas-worker`'s nightly sweep deletes expired rows in batches of 500.\n\n\
        Nothing locks the row, so two refreshes can both read it before either writes.";
    log.tool(
        "toolu_04",
        "Agent",
        json!({ "description": "Find every caller of the session store",
                "prompt": "List every caller of SessionStore in the workspace and say whether each reads and writes a refresh row in one transaction.",
                "subagent_type": "Explore" }),
        json!([{ "type": "text", "text": format!("{survey}\nagentId: {SURVEY_AGENT}\n<usage>subagent_tokens: 18342\ntool_uses: 6\nduration_ms: 41230</usage>") }]),
        json!({ "agentId": SURVEY_AGENT, "agentType": "Explore",
                "content": [{ "type": "text", "text": survey }],
                "prompt": "List every caller of SessionStore in the workspace and say whether each reads and writes a refresh row in one transaction.",
                "status": "completed", "totalDurationMs": 41_230, "totalTokens": 18_342,
                "totalToolUseCount": 6 }),
    );
    let rfc = "Section 4.14.2 of the OAuth 2.0 Security Best Current Practice says an \
        authorization server should rotate refresh tokens: each use issues a new token and \
        invalidates the old one. If an already-used token is presented again, the server cannot \
        tell the attacker from the legitimate client, so it should revoke the whole family of \
        tokens derived from the original grant.";
    log.tool(
        "toolu_05",
        "WebFetch",
        json!({ "url": "https://datatracker.ietf.org/doc/html/rfc9700#section-4.14.2",
                "prompt": "What does it say about rotating refresh tokens and about a replayed one?" }),
        json!(rfc),
        json!({ "bytes": 412_893, "code": 200, "codeText": "OK", "result": rfc,
                "durationMs": 1_830,
                "url": "https://datatracker.ietf.org/doc/html/rfc9700#section-4.14.2" }),
    );
    let todos = json!([
        { "content": "Lock the refresh row in one transaction", "activeForm": "Locking the refresh row", "status": "completed" },
        { "content": "Revoke the family on a replayed token", "activeForm": "Revoking the family on a replay", "status": "completed" },
        { "content": "Test two refreshes that race", "activeForm": "Testing two racing refreshes", "status": "completed" },
        { "content": "Keep clippy clean", "activeForm": "Keeping clippy clean", "status": "in_progress" },
        { "content": "Send the same Idempotency-Key on a retry", "activeForm": "Keeping the key on a retry", "status": "pending" },
    ]);
    log.tool(
        "toolu_06",
        "TodoWrite",
        json!({ "todos": todos }),
        json!("Todos have been modified successfully."),
        json!({ "oldTodos": [], "newTodos": todos }),
    );
    let old = "        let row = self.store.find_refresh(presented).await?;";
    let new = "        let mut tx = self.store.begin().await?;\n        let row = self.store.find_refresh_for_update(&mut tx, presented).await?;";
    let hunks = [
        hunk(
            9,
            9,
            &[
                " pub enum AuthError {",
                "     #[error(\"the refresh token has expired\")]",
                "     Expired,",
                "+    #[error(\"the refresh token was already used; its family is revoked\")]",
                "+    Replayed,",
                "     #[error(transparent)]",
                "     Store(#[from] StoreError),",
                " }",
            ],
        ),
        hunk(
            24,
            26,
            &[
                "     pub async fn refresh(&self, presented: &RefreshToken) -> Result<Session, AuthError> {",
                "-        let row = self.store.find_refresh(presented).await?;",
                "+        let mut tx = self.store.begin().await?;",
                "+        // Lock the row: a second refresh of the same token waits here, then finds it spent.",
                "+        let row = self.store.find_refresh_for_update(&mut tx, presented).await?;",
                "         if row.expires_at <= self.clock.now() {",
                "             return Err(AuthError::Expired);",
                "         }",
                "-        let next = RefreshToken::mint(&mut self.rng);",
                "-        self.store.replace_refresh(row.id, &next).await?;",
                "+        if row.spent {",
                "+            self.store.revoke_family(&mut tx, row.family).await?;",
                "+            return Err(AuthError::Replayed);",
                "+        }",
                "+        let next = RefreshToken::mint(&mut self.rng);",
                "+        self.store.rotate_refresh(&mut tx, row.id, &next).await?;",
                "+        tx.commit().await?;",
                "         Ok(Session::new(row.user, next))",
                "     }",
            ],
        ),
    ];
    log.tool(
        "toolu_07",
        "Edit",
        json!({ "file_path": session_rs, "old_string": old, "new_string": new }),
        json!(format!("The file {session_rs} has been updated successfully.")),
        edited(&session_rs, old, new, &hunks),
    );
    let test_rs = file("crates/api/tests/refresh_race.rs");
    let test_text = "use atlas_api::testing::{service, token};\n\n/// Two refreshes of one token that race: one wins, the other is told it was a replay.\n#[tokio::test]\nasync fn a_raced_refresh_is_a_replay() {\n    let (service, presented) = (service().await, token());\n    let (a, b) = tokio::join!(service.refresh(&presented), service.refresh(&presented));\n    assert!(a.is_ok() != b.is_ok(), \"exactly one refresh wins: {a:?} {b:?}\");\n}\n";
    log.tool(
        "toolu_08",
        "Write",
        json!({ "file_path": test_rs, "content": test_text }),
        json!(format!("File created successfully at: {test_rs}")),
        json!({ "type": "create", "filePath": test_rs, "content": test_text,
                "structuredPatch": [], "originalFile": null }),
    );
    let passed = "   Compiling atlas-api v0.4.2 (/Users/mira/code/atlas/crates/api)\n    Finished `test` profile [unoptimized + debuginfo] target(s) in 8.31s\n     Running unittests src/lib.rs (target/debug/deps/atlas_api-3f9c2a1b7e5d4c60)\n\nrunning 14 tests\ntest routes::tests::health_reports_the_version ... ok\ntest session::tests::expired_token_is_401 ... ok\ntest session::tests::refresh_mints_a_new_token ... ok\ntest session::tests::refresh_rotates_under_a_race ... ok\ntest session::tests::logout_revokes_the_family ... ok\ntest middleware::tests::trace_carries_the_session ... ok\n\ntest result: ok. 14 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.19s\n\n     Running tests/refresh_race.rs (target/debug/deps/refresh_race-91d0c2b7a4e3f815)\n\nrunning 1 test\ntest a_raced_refresh_is_a_replay ... ok\n\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.07s";
    log.tool(
        "toolu_09",
        "Bash",
        json!({ "command": "cargo test -p atlas-api", "description": "Run the API crate's tests" }),
        json!(passed),
        ran(passed),
    );
    let lint = "Exit code 101\n    Checking atlas-api v0.4.2 (/Users/mira/code/atlas/crates/api)\nerror: this `if` has the same condition as a previous `if`\n  --> crates/api/src/session.rs:34:12\n   |\n34 |         if row.spent {\n   |            ^^^^^^^^^\n   |\nnote: same as this\n  --> crates/api/src/session.rs:29:12\n   = help: for further information visit https://rust-lang.github.io/rust-clippy/master/index.html#ifs_same_cond\n\nerror: could not compile `atlas-api` (lib) due to 1 previous error";
    log.call(
        "toolu_10",
        "Bash",
        json!({ "command": "cargo clippy -p atlas-api --all-targets -- -D warnings",
                "description": "Lint the API crate" }),
    );
    log.result("toolu_10", json!(lint), json!(format!("Error: {lint}")), true);
    let fix_hunks = [hunk(
        27,
        27,
        &[
            "         if row.expires_at <= self.clock.now() {",
            "-            if row.spent {",
            "-                return Err(AuthError::Expired);",
            "-            }",
            "             return Err(AuthError::Expired);",
            "         }",
        ],
    )];
    log.tool(
        "toolu_11",
        "Edit",
        json!({ "file_path": session_rs, "old_string": "if row.spent {\n                return Err(AuthError::Expired);\n            }\n", "new_string": "" }),
        json!(format!("The file {session_rs} has been updated successfully.")),
        edited(&session_rs, "if row.spent {", "", &fix_hunks),
    );
    let clean = "    Checking atlas-api v0.4.2 (/Users/mira/code/atlas/crates/api)\n    Finished `dev` profile [unoptimized + debuginfo] target(s) in 2.04s";
    log.tool(
        "toolu_12",
        "Bash",
        json!({ "command": "cargo clippy -p atlas-api --all-targets -- -D warnings",
                "description": "Lint the API crate again" }),
        json!(clean),
        ran(clean),
    );
    log.say(
        "## What was wrong\n\n\
         Two refreshes that raced both read the refresh row before either wrote it, so each \
         minted a token and the second write won. The client holding the first token then failed \
         its next call with a 401.\n\n\
         ## The fix\n\n\
         - `refresh` runs in **one transaction** and locks the row with `SELECT … FOR UPDATE`.\n\
         - A spent token is a *replay*: the whole token family is revoked, as RFC 9700 §4.14.2 \
         advises.\n\
         - A new error, `AuthError::Replayed`, maps to `401` with `error=\"invalid_grant\"`.\n\n\
         | Case | Before | After |\n\
         |---|---|---|\n\
         | Two refreshes race | both succeed, one token dangles | one wins, the other gets 401 |\n\
         | A stolen token is replayed | succeeds | the family is revoked |\n\
         | An expired token | 401 | 401 |\n\n\
         ```rust\n\
         let row = self.store.find_refresh_for_update(&mut tx, presented).await?;\n\
         if row.spent {\n    \
             self.store.revoke_family(&mut tx, row.family).await?;\n    \
             return Err(AuthError::Replayed);\n\
         }\n\
         ```\n\n\
         All 15 tests in `atlas-api` pass, clippy is clean, and the new `refresh_race` test fails \
         on the old code.",
    );
    second_turn(&mut log, cwd);
    (log.out, survey_session(session, cwd))
}

/// The second turn, under way: a search, a read and an edit done, the workspace's tests asked
/// for and waiting on the person.
fn second_turn(log: &mut Log, cwd: &Path) {
    let middleware = cwd.join("crates/api/src/middleware.rs").to_string_lossy().into_owned();
    log.prompt(
        "Now make the retry middleware send the same Idempotency-Key when it retries a refresh, \
         and run the whole workspace's tests.",
    );
    log.think(
        "The retry builds a fresh request from the body and the URI only, so every header is \
         dropped, the idempotency key with them. Copy the key across, and only that.",
    );
    let hits = "crates/api/src/middleware.rs:6:pub const IDEMPOTENCY_KEY: HeaderName = HeaderName::from_static(\"idempotency-key\");";
    log.tool(
        "toolu_13",
        "Grep",
        json!({ "pattern": "IDEMPOTENCY_KEY", "output_mode": "content", "-n": true }),
        json!(hits),
        json!({ "mode": "content", "numFiles": 1, "filenames": [], "content": hits,
                "numLines": 1 }),
    );
    log.tool(
        "toolu_14",
        "Read",
        json!({ "file_path": middleware, "offset": 8, "limit": 16 }),
        json!(numbered(MIDDLEWARE_RS, 1)),
        json!({ "type": "text", "file": { "filePath": middleware, "content": MIDDLEWARE_RS,
            "numLines": MIDDLEWARE_RS.lines().count(), "startLine": 1,
            "totalLines": MIDDLEWARE_RS.lines().count() } }),
    );
    let old = "        *again.uri_mut() = request.uri().clone();";
    let new = "        *again.uri_mut() = request.uri().clone();\n        if let Some(key) = request.headers().get(&IDEMPOTENCY_KEY) {\n            again.headers_mut().insert(IDEMPOTENCY_KEY, key.clone());\n        }";
    let hunks = [hunk(
        17,
        17,
        &[
            "     fn retry<B: Clone>(&self, request: &Request<B>) -> Request<B> {",
            "         let mut again = Request::new(request.body().clone());",
            "         *again.uri_mut() = request.uri().clone();",
            "+        if let Some(key) = request.headers().get(&IDEMPOTENCY_KEY) {",
            "+            again.headers_mut().insert(IDEMPOTENCY_KEY, key.clone());",
            "+        }",
            "         again",
            "     }",
        ],
    )];
    log.tool(
        "toolu_15",
        "Edit",
        json!({ "file_path": middleware, "old_string": old, "new_string": new }),
        json!(format!("The file {middleware} has been updated successfully.")),
        edited(&middleware, old, new, &hunks),
    );
    log.say(
        "The retry now carries the `Idempotency-Key` across, and nothing else. Running the whole \
         workspace's tests to check nothing leaned on the old behaviour.",
    );
    log.call("toolu_16", "Bash", workspace_tests());
}

/// The command the second turn waits on the person for.
fn workspace_tests() -> Value {
    json!({ "command": "cargo test --workspace -- --test-threads=4",
            "description": "Run every test in the workspace" })
}

/// The subagent's own thread: the search it made and what it read, then its report.
fn survey_session(session: &str, cwd: &Path) -> String {
    let mut log = Log::new(session, cwd, now().saturating_sub(31 * 60));
    log.agent = Some(SURVEY_AGENT.to_owned());
    log.stem = "00000000-0000-4000-a000";
    let mut out = json!({ "type": "agent_metadata", "agentType": "Explore",
        "description": "Find every caller of the session store", "toolUseId": "toolu_04",
        "spawnDepth": 1 })
    .to_string();
    out.push('\n');
    log.prompt(
        "List every caller of SessionStore in the workspace and say whether each reads and \
         writes a refresh row in one transaction.",
    );
    let hits = "crates/api/src/session.rs\ncrates/worker/src/sweep.rs\ncrates/store/src/lib.rs";
    log.tool(
        "toolu_s1",
        "Grep",
        json!({ "pattern": "SessionStore", "output_mode": "files_with_matches" }),
        json!(hits),
        json!({ "mode": "files_with_matches", "numFiles": 3,
                "filenames": ["crates/api/src/session.rs", "crates/worker/src/sweep.rs",
                              "crates/store/src/lib.rs"] }),
    );
    let sweep = cwd.join("crates/worker/src/sweep.rs").to_string_lossy().into_owned();
    let text = "/// Delete expired refresh rows, 500 at a time.\npub async fn sweep(store: &SessionStore) -> Result<u64, StoreError> {\n    let mut gone = 0;\n    while let n @ 1.. = store.delete_expired(500).await? {\n        gone += n;\n    }\n    Ok(gone)\n}\n";
    log.tool(
        "toolu_s2",
        "Read",
        json!({ "file_path": sweep }),
        json!(numbered(text, 1)),
        json!({ "type": "text", "file": { "filePath": sweep, "content": text, "numLines": 8,
            "startLine": 1, "totalLines": 8 } }),
    );
    log.say(
        "Three callers. `refresh` reads and writes in two statements with no transaction; \
         `revoke` deletes by session id; the worker's sweep deletes expired rows in batches. \
         Nothing locks a refresh row.",
    );
    out.push_str(&log.out);
    out
}

/// A short session: a prompt, its steps, and an answer when the turn has ended.
fn short_session(
    session: &str,
    cwd: &Path,
    prompt: &str,
    steps: &[(&str, Value, &str)],
    answer: Option<&str>,
) -> String {
    let mut log = Log::new(session, cwd, now().saturating_sub(20 * 60));
    log.prompt(prompt);
    for (n, (name, input, output)) in steps.iter().enumerate() {
        let id = format!("toolu_s{n:02}");
        let structured = if *name == "Bash" { ran(output) } else { json!({}) };
        log.tool(&id, name, input.clone(), json!(output), structured);
    }
    if let Some(answer) = answer {
        log.say(answer);
    }
    log.out
}

/// A hook payload naming `session_id`, its transcript and its directory, with `more` members.
fn payload(event: &str, session_id: &str, transcript: &Path, cwd: &Path, more: &Value) -> Value {
    let mut payload = json!({
        "hook_event_name": event, "session_id": session_id, "transcript_path": transcript,
        "cwd": cwd,
    });
    if let (Some(all), Some(more)) = (payload.as_object_mut(), more.as_object()) {
        all.extend(more.clone());
    }
    payload
}

/// The control socket's request that hands the worker `payload` for terminal `session`, as
/// `slopty hook` relays it.
fn hook(session: &str, payload: &Value) -> Value {
    json!({ "cmd": "hook", "session": session, "payload": payload.to_string() })
}

/// The meters a status line reports, as the relay posts them.
fn meters(model: &str, context: f64, cost: f64) -> Value {
    json!({ "meters": {
        "model": model, "model_id": "claude-opus-5-5", "context_used_pct": context,
        "context_window": 200_000, "cost_usd": cost,
        "five_hour": { "used_pct": 42.0, "resets_at": now().saturating_add(7_200) },
        "seven_day": { "used_pct": 18.5, "resets_at": null },
    }})
}

/// An agent of the day: its terminal, its session id and where its transcript is.
struct Agent {
    terminal: String,
    id: String,
    transcript: PathBuf,
    cwd: PathBuf,
}

impl Agent {
    /// Lay out `transcript` for terminal `terminal` under `root` and say what to play.
    fn new(terminal: &str, root: &Path, cwd: &Path, transcript: &str) -> Self {
        let id = slopty_e2e::harness::agent_session(terminal);
        let path = root.join("projects").join(format!("{id}.jsonl"));
        write(&path, transcript);
        Self { terminal: terminal.to_owned(), id, transcript: path, cwd: cwd.to_path_buf() }
    }

    /// The control socket's request for `event` of this agent.
    fn hook(&self, event: &str, more: &Value) -> Value {
        hook(&self.terminal, &payload(event, &self.id, &self.transcript, &self.cwd, more))
    }
}

/// Hand `request` to a worker's control socket and check it took it.
fn took(reply: &Value) {
    assert_eq!(reply["reply"], "ok", "the worker took the hook: {reply}");
}

// ---------------------------------------------------------------------------------------------
// The workspace.
// ---------------------------------------------------------------------------------------------

/// What the studio's workspace holds when it is done.
struct Studio {
    history: String,
    build: String,
    chart: String,
}

/// The studio's columns: the history, the build and its failed tests, an agent that needs the
/// person, and an agent at work; with a page's `port`, a file, a listing and a chart, a folder,
/// the page and a note among them as well.
async fn studio(stack: &mut Stack, home: &Path, port: Option<u16>) -> Studio {
    let repo = home.join("code/atlas");
    let history = first_shell(&mut stack.driver).await;
    let drv = &mut stack.driver;
    run(drv, &history, "cd ~/code/atlas", "~/code/atlas").await;
    run(drv, &history, "git lg -22", "start the workspace").await;
    run(drv, &history, "git status -sb", "retry.rs").await;

    let build = new_shell(drv).await;
    run(drv, &build, "cd ~/code/atlas && cargo build", "Finished").await;
    // It takes its time and ends while the person is in another column.
    start(drv, &build, "cargo test").await;
    let tested = tokio::time::Instant::now();

    let needs = agent_tile(drv, "Harden the session refresh").await;
    let (main, survey) = long_session(&slopty_e2e::harness::agent_session(&needs), &repo);
    let agent = Agent::new(&needs, home, &repo, &main);
    let subagents = slopty_agent::conversation::subagents_dir(&agent.transcript);
    write(&subagents.join(format!("agent-{SURVEY_AGENT}.jsonl")), &survey);
    for (event, more) in [
        ("SessionStart", json!({ "source": "startup" })),
        ("Statusline", meters("Opus 5.5", 61.0, 3.84)),
        (
            "UserPromptSubmit",
            json!({ "prompt": "Now make the retry middleware send the same Idempotency-Key" }),
        ),
        ("PermissionRequest", json!({ "tool_name": "Bash", "tool_input": workspace_tests() })),
    ] {
        took(&stack.ctl(&agent.hook(event, &more)).await.unwrap());
    }
    let drv = &mut stack.driver;
    wait(drv, "the agent waiting on the person", |d| {
        d.terminal(&needs)
            .is_some_and(|t| t.agent.as_deref().is_some_and(|a| a.starts_with("blocked")))
    })
    .await;

    let chart = match port {
        Some(port) => more_tiles(drv, &repo, port).await,
        None => history.clone(),
    };
    working_agent(stack, home, &repo).await;

    if port.is_some() {
        let drv = &mut stack.driver;
        drv.keys("cmd-shift-n").await.unwrap();
        wait(drv, "the note", |d| d.item("note").is_some()).await;
        drv.type_text(
            "Release 0.4.3\n\n- [x] lock the refresh row\n- [x] revoke the family on a replay\n- [ ] keep the idempotency key on a retry\n- [ ] ws-gateway: why degraded since 09:00?\n- [ ] tag and push the images",
        )
        .await
        .unwrap();
    }

    tokio::time::sleep(SLOW.saturating_sub(tested.elapsed())).await;
    Studio { history, build, chart }
}

/// A file, a listing with a chart in the history, a folder and a page; the listing's shell.
async fn more_tiles(drv: &mut Driver, repo: &Path, port: u16) -> String {
    drv.open_file(&repo.join("crates/api/src/routes.rs").to_string_lossy(), Some(27))
        .await
        .unwrap();
    wait(drv, "routes.rs in its tile", |d| d.item("file").is_some()).await;

    let chart = new_shell(drv).await;
    run(drv, &chart, "cd ~/code/atlas && ls -lAh", "scripts").await;
    run(drv, &chart, "scripts/plot-latency", "now").await;
    wait(drv, "the chart in the history", |d| d.terminal(&chart).is_some_and(|t| t.images > 0))
        .await;

    drv.keys("cmd-shift-p").await.unwrap();
    wait(drv, "the palette", |d| d.a11y_node("Dialog", Some("Commands")).is_some()).await;
    drv.type_text("Open folder").await.unwrap();
    drv.keys("enter").await.unwrap();
    wait(drv, "the palette at the shell's directory", |d| {
        d.a11y.iter().any(|n| {
            n.role == "ListBoxOption"
                && n.label
                    .as_deref()
                    .is_some_and(|l| l.contains("Open folder") && l.contains("atlas"))
        })
    })
    .await;
    drv.keys("enter").await.unwrap();
    wait(drv, "the folder tile", |d| d.item("folder").is_some()).await;

    drv.ok(&Command::OpenUrl { url: format!("http://127.0.0.1:{port}/") }).await.unwrap();
    wait(drv, "the page, loaded", |d| {
        d.item("browser").and_then(|i| i.browser.as_ref()).is_some_and(|b| !b.loading && b.snapshot)
    })
    .await;

    chart
}

/// An agent at work on the studio: writing the audit log's migration, its edit under way.
async fn working_agent(stack: &mut Stack, home: &Path, repo: &Path) {
    let drv = &mut stack.driver;
    let terminal = agent_tile(drv, "Write the audit-log migration").await;
    let migration = repo.join("migrations/0007_audit_log.sql").to_string_lossy().into_owned();
    let sql = "CREATE TABLE audit_log (\n    id         BIGSERIAL PRIMARY KEY,\n    actor      UUID NOT NULL,\n    action     TEXT NOT NULL,\n    subject    TEXT NOT NULL,\n    at         TIMESTAMPTZ NOT NULL DEFAULT now()\n);\nCREATE INDEX audit_log_actor_at ON audit_log (actor, at DESC);\n";
    let transcript = short_session(
        &slopty_e2e::harness::agent_session(&terminal),
        repo,
        "Add an audit log table: who did what to which session, and when. Write the migration and \
         run it against the dev database.",
        &[
            (
                "Glob",
                json!({ "pattern": "migrations/*.sql" }),
                "migrations/0001_sessions.sql\nmigrations/0002_refresh.sql\nmigrations/0003_family.sql\nmigrations/0004_indexes.sql\nmigrations/0005_users.sql\nmigrations/0006_spent.sql",
            ),
            (
                "Write",
                json!({ "file_path": migration, "content": sql }),
                "File created successfully",
            ),
            (
                "Bash",
                json!({ "command": "sqlx migrate run", "description": "Apply the migration to the dev database" }),
                "Applied 7/migrate audit log (14.207ms)",
            ),
        ],
        None,
    );
    let agent = Agent::new(&terminal, home, repo, &transcript);
    for (event, more) in [
        ("SessionStart", json!({ "source": "startup" })),
        ("Statusline", meters("Opus 5.5", 23.0, 0.61)),
        ("UserPromptSubmit", json!({ "prompt": "Add an audit log table" })),
        (
            "PreToolUse",
            json!({ "tool_name": "Edit", "tool_input": { "file_path": repo.join("crates/store/src/audit.rs") } }),
        ),
    ] {
        took(&stack.ctl(&agent.hook(event, &more)).await.unwrap());
    }
    wait(&mut stack.driver, "the agent at work", |d| {
        d.terminal(&terminal).is_some_and(|t| t.agent.as_deref().is_some_and(|a| a != "idle"))
    })
    .await;
}

/// The dev box: the stack's containers and a service's log, a remote window, an agent that is
/// done, and a pull left running. Its first shell.
async fn devbox(stack: &mut Stack, second: &SecondWorker) -> String {
    std::fs::create_dir_all(second.path("home/srv/atlas")).unwrap();
    let address = second.address().to_owned();
    let drv = &mut stack.driver;
    drv.ok(&Command::AddWorker { address }).await.unwrap();
    let dump = wait(drv, "the dev box's first shell", |d| {
        d.items.iter().any(|i| i.worker == "devbox" && i.kind == "terminal")
    })
    .await;
    let shell = dump
        .items
        .iter()
        .find(|i| i.worker == "devbox" && i.kind == "terminal")
        .and_then(|i| i.session.clone())
        .unwrap();
    drv.reveal(&shell).await.unwrap();
    wait(drv, "the dev box's prompt", |d| {
        d.terminal(&shell).is_some_and(slopty_e2e::TerminalInfo::reads_a_line)
    })
    .await;
    run(drv, &shell, "cd ~/srv/atlas && docker ps", "atlas-otel").await;
    run(drv, &shell, "docker compose logs --tail 10 api", "idempotency-key").await;

    drv.ok(&Command::PickWindow { window: 7001, title: "Synthetic editor".to_owned() })
        .await
        .unwrap();
    wait(drv, "the window's tile", |d| d.items.iter().any(|i| i.kind == "window")).await;
    // Time for its first frames to come and be drawn.
    tokio::time::sleep(Duration::from_secs(3)).await;
    drv.ok(&Command::Move { x: PARK.0, y: PARK.1 }).await.unwrap();
    shot(drv, "workspace-devbox-window-light").await;

    let terminal = agent_tile(drv, "Bump tokio and fix the build").await;
    let home = second.path("home");
    let cwd = home.join("srv/atlas");
    let transcript = short_session(
        &slopty_e2e::harness::agent_session(&terminal),
        &cwd,
        "Bump tokio to 1.48 across the workspace and fix whatever breaks.",
        &[
            (
                "Bash",
                json!({ "command": "cargo update -p tokio", "description": "Bump tokio" }),
                "    Updating crates.io index\n     Locking 1 package to latest compatible version\n    Updating tokio v1.47.1 -> v1.48.0",
            ),
            (
                "Bash",
                json!({ "command": "cargo build --workspace", "description": "Build everything" }),
                "    Finished `dev` profile [unoptimized + debuginfo] target(s) in 38.12s",
            ),
        ],
        Some(
            "tokio is at **1.48.0** everywhere and the workspace builds with no change to the code. The only lock-file churn is `tokio` and `tokio-macros`.",
        ),
    );
    let agent = Agent::new(&terminal, &second.path("agents"), &cwd, &transcript);
    for (event, more) in [
        ("SessionStart", json!({ "source": "startup" })),
        ("Statusline", meters("Sonnet 5", 12.0, 0.18)),
        ("Stop", json!({ "background_tasks": [], "session_crons": [] })),
    ] {
        took(&second.ctl(&agent.hook(event, &more)).await.unwrap());
    }
    let drv = &mut stack.driver;
    wait(drv, "the agent done", |d| d.terminal(&terminal).is_some_and(|t| t.agent.is_some())).await;

    let pull = new_shell(drv).await;
    start(drv, &pull, "cd ~/srv/atlas && docker compose pull").await;
    drv.reveal(&shell).await.unwrap();
    shell
}

/// The build box: a clean test run, and an agent asking which way to go.
async fn build_box(stack: &mut Stack, third: &SecondWorker) {
    std::fs::create_dir_all(third.path("home/ci/atlas")).unwrap();
    let address = third.address().to_owned();
    let drv = &mut stack.driver;
    drv.ok(&Command::AddWorker { address }).await.unwrap();
    let dump = wait(drv, "the build box's first shell", |d| {
        d.items.iter().any(|i| i.worker == "build-01" && i.kind == "terminal")
    })
    .await;
    let shell = dump
        .items
        .iter()
        .find(|i| i.worker == "build-01" && i.kind == "terminal")
        .and_then(|i| i.session.clone())
        .unwrap();
    drv.reveal(&shell).await.unwrap();
    wait(drv, "the build box's prompt", |d| {
        d.terminal(&shell).is_some_and(slopty_e2e::TerminalInfo::reads_a_line)
    })
    .await;
    run(drv, &shell, "cd ~/ci/atlas && cargo nextest run --workspace", "Summary").await;

    let terminal = agent_tile(drv, "Triage the flaky websocket test").await;
    let cwd = third.path("home").join("ci/atlas");
    let transcript = short_session(
        &slopty_e2e::harness::agent_session(&terminal),
        &cwd,
        "ws::tests::reconnect_after_server_restart fails about one run in seven on CI. Find out why.",
        &[
            (
                "Bash",
                json!({ "command": "for i in $(seq 20); do cargo nextest run -E 'test(reconnect_after_server_restart)' --no-capture 2>&1 | tail -1; done | sort | uniq -c", "description": "Run the test 20 times" }),
                "     17 Summary [  61.2s] 1 test run: 1 passed\n      3 Summary [  60.0s] 1 test run: 0 passed, 1 timed out",
            ),
            (
                "Read",
                json!({ "file_path": cwd.join("crates/api/src/ws.rs") }),
                "   112\t        tokio::time::sleep(Duration::from_secs(60)).await;",
            ),
        ],
        Some(
            "The test waits a fixed **60 s** for the server to come back, and the reconnect's backoff can reach 64 s. Two ways to fix it; I need you to choose.",
        ),
    );
    let agent = Agent::new(&terminal, &third.path("agents"), &cwd, &transcript);
    let questions = json!([{
        "question": "How should the test wait for the reconnect?", "header": "Wait",
        "multiSelect": false,
        "options": [
            { "label": "Poll the socket", "description": "Check every 100 ms until it is open, up to 90 s" },
            { "label": "Cap the backoff", "description": "Make the backoff stop at 30 s in tests" },
            { "label": "Both", "description": "Poll, and cap the backoff so the test stays quick" }
        ]
    }, {
        "question": "Which runs should get the fix?", "header": "Scope", "multiSelect": true,
        "options": [
            { "label": "CI on Linux" }, { "label": "CI on macOS" }, { "label": "Local runs" }
        ]
    }]);
    for (event, more) in [
        ("SessionStart", json!({ "source": "startup" })),
        ("Statusline", meters("Opus 5.5", 34.0, 1.27)),
        ("UserPromptSubmit", json!({ "prompt": "ws test flaky" })),
        (
            "PermissionRequest",
            json!({ "tool_name": "AskUserQuestion", "tool_input": { "questions": questions } }),
        ),
    ] {
        took(&third.ctl(&agent.hook(event, &more)).await.unwrap());
    }
    wait(&mut stack.driver, "the agent asking", |d| {
        d.terminal(&terminal)
            .is_some_and(|t| t.agent.as_deref().is_some_and(|a| a.starts_with("blocked")))
    })
    .await;
}

/// The settings, page by page.
async fn settings(stack: &mut Stack) {
    let drv = &mut stack.driver;
    drv.keys("cmd-,").await.unwrap();
    wait(drv, "the settings", |d| d.a11y_node("Dialog", Some("Settings")).is_some()).await;
    drv.ok(&Command::Move { x: PARK.0, y: PARK.1 }).await.unwrap();
    // Light only: the form writes the file the theme is switched by.
    shot(&mut stack.driver, "settings-appearance-light").await;
    for page in ["Terminal", "Input", "Streams", "Network", "Keyboard", "About"] {
        let drv = &mut stack.driver;
        if click(drv, "Tab", page).await {
            drv.ok(&Command::Move { x: PARK.0, y: PARK.1 }).await.unwrap();
            shot(drv, &format!("settings-{}-light", page.to_lowercase())).await;
        }
    }
    let drv = &mut stack.driver;
    drv.keys("escape").await.unwrap();
    wait(drv, "the settings closed", |d| d.a11y_node("Dialog", Some("Settings")).is_none()).await;
}

/// Hover the row of `session`'s terminal that shows `text`, so its block says what it is.
async fn hover_row(drv: &mut Driver, session: &str, text: &str) {
    let dump = look(drv).await;
    let Some(term) = dump.terminal(session) else { return };
    let Some(row) = term.rows.iter().position(|r| r.contains(text)) else { return };
    let Some((x, y)) = term.cell_center(10, row) else { return };
    drv.ok(&Command::Move { x, y }).await.unwrap();
}

/// The day's ground: a home with the repository, the stand-in programs first on `PATH`, and
/// the studio's stack on it at the app's window size.
struct Day {
    stack: Stack,
    home: PathBuf,
    path: String,
    zdotdir: String,
    // Last, so the files go after the processes that use them.
    _scratch: tempfile::TempDir,
}

impl Day {
    async fn begin() -> Self {
        let scratch = tempfile::tempdir().unwrap();
        let home = made(scratch.path(), "home");
        let (path, zdotdir) = shells(scratch.path());
        make_repo(&home.join("code/atlas"));
        let home_str = home.to_string_lossy().into_owned();
        let env = [("HOME", home_str.as_str()), ("PATH", path.as_str()), ("ZDOTDIR", &zdotdir)];
        let mut stack = Stack::launch_with("studio", &env).await.unwrap();
        stack.driver.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
        Self { stack, home, path, zdotdir, _scratch: scratch }
    }
}

/// The studio's workspace, every kind of tile in it: the history, a build and its failed
/// tests, an agent that needs the person, a file, a listing with a chart, a folder, a page, an
/// agent at work and a note; the overview, and the toast a closed tile leaves.
#[tokio::test]
#[ignore = "showcase: cargo xtask e2e showcase"]
async fn showcase_the_studio_workspace() {
    let mut day = Day::begin().await;
    let port = serve_page();
    let studio = studio(&mut day.stack, &day.home, Some(port)).await;
    let stack = &mut day.stack;

    let drv = &mut stack.driver;
    drv.reveal(&studio.history).await.unwrap();
    drv.keys("cmd-1").await.unwrap();
    drv.ok(&Command::Move { x: PARK.0, y: PARK.1 }).await.unwrap();
    both(stack, "workspace-studio").await;

    let drv = &mut stack.driver;
    drv.reveal(&studio.build).await.unwrap();
    hover_row(drv, &studio.build, "test result: FAILED").await;
    wait(drv, "the failed block's facts", |d| {
        d.a11y_node("Button", Some("Block actions")).is_some()
    })
    .await;
    both(stack, "terminal-failed-block").await;

    let drv = &mut stack.driver;
    drv.ok(&Command::Move { x: PARK.0, y: PARK.1 }).await.unwrap();
    drv.keys("cmd-alt-right").await.unwrap();
    shot(drv, "workspace-agent-needs-you-light").await;
    drv.keys("cmd-alt-right").await.unwrap();
    shot(drv, "workspace-file-light").await;
    drv.reveal(&studio.chart).await.unwrap();
    both(stack, "workspace-chart").await;
    let drv = &mut stack.driver;
    drv.keys("cmd-alt-right").await.unwrap();
    shot(drv, "workspace-folder-light").await;
    drv.keys("cmd-alt-right").await.unwrap();
    both(stack, "workspace-browser").await;
    let drv = &mut stack.driver;
    drv.keys("cmd-alt-right").await.unwrap();
    shot(drv, "workspace-agent-at-work-light").await;
    drv.keys("cmd-9").await.unwrap();
    shot(drv, "workspace-note-light").await;

    drv.reveal(&studio.history).await.unwrap();
    drv.keys("cmd-alt-o").await.unwrap();
    wait(drv, "the overview", |d| d.overview).await;
    both(stack, "overview").await;
    let drv = &mut stack.driver;
    drv.keys("cmd-alt-o").await.unwrap();
    wait(drv, "the overview closed", |d| !d.overview).await;

    drv.keys("cmd-9").await.unwrap();
    wait(drv, "the note focused", |d| d.items.iter().any(|i| i.kind == "note" && i.active)).await;
    drv.keys("cmd-w").await.unwrap();
    wait(drv, "the take-back offer", |d| d.notice.is_some()).await;
    drv.ok(&Command::Move { x: PARK.0, y: PARK.1 }).await.unwrap();
    // No settling: the offer goes on a timer.
    drv.render(&render_path("toast-light")).await.unwrap();
    println!("showcase: rendered toast-light");

    day.stack.shutdown().await;
}

/// Three workers: the studio with its agents, the dev box with its containers, a remote window,
/// an agent done and a pull left running, the build box with a clean run and an agent asking;
/// the navigator over all of them.
#[tokio::test]
#[ignore = "showcase: cargo xtask e2e showcase"]
async fn showcase_three_workers() {
    let mut day = Day::begin().await;
    // A drawn screen on the dev box, a mesh's round trip to the build box.
    let plain = [("PATH", day.path.as_str()), ("ZDOTDIR", day.zdotdir.as_str())];
    let drawn = [plain[0], plain[1], ("SLOPTY_SYNTHETIC_SCREEN", "1")];
    let (devbox_worker, build_worker) = tokio::join!(
        SecondWorker::launch_env("devbox", slopty_shape::Link::CLEAR, &drawn),
        SecondWorker::launch_env("build-01", slopty_e2e::harness::TAILNET, &plain),
    );
    let (devbox_worker, build_worker) = (devbox_worker.unwrap(), build_worker.unwrap());
    let studio = studio(&mut day.stack, &day.home, None).await;
    let stack = &mut day.stack;
    let devbox_shell = devbox(stack, &devbox_worker).await;
    build_box(stack, &build_worker).await;
    // The pull ends while the person is on the build box.
    tokio::time::sleep(SLOW).await;

    let drv = &mut stack.driver;
    drv.ok(&Command::Move { x: PARK.0, y: PARK.1 }).await.unwrap();
    both(stack, "workspace-build-box").await;
    let drv = &mut stack.driver;
    drv.reveal(&devbox_shell).await.unwrap();
    drv.ok(&Command::Move { x: PARK.0, y: PARK.1 }).await.unwrap();
    both(stack, "workspace-devbox").await;
    let drv = &mut stack.driver;

    drv.reveal(&studio.history).await.unwrap();
    drv.ok(&Command::Move { x: PARK.0, y: PARK.1 }).await.unwrap();
    shot(drv, "navigator-three-workers-light").await;

    day.stack.shutdown().await;
    devbox_worker.shutdown().await;
    build_worker.shutdown().await;
}

/// The chrome over a working studio: the palette with results, the "…" menu, the
/// breadcrumb's menu, and every page of the settings.
#[tokio::test]
#[ignore = "showcase: cargo xtask e2e showcase"]
async fn showcase_the_palette_menus_and_settings() {
    let mut day = Day::begin().await;
    let studio = studio(&mut day.stack, &day.home, None).await;
    let stack = &mut day.stack;
    let drv = &mut stack.driver;
    drv.reveal(&studio.history).await.unwrap();

    drv.keys("cmd-shift-p").await.unwrap();
    wait(drv, "the palette", |d| d.a11y_node("Dialog", Some("Commands")).is_some()).await;
    drv.type_text("open").await.unwrap();
    both(stack, "palette").await;
    let drv = &mut stack.driver;
    drv.keys("escape").await.unwrap();
    wait(drv, "the palette closed", |d| d.a11y_node("Dialog", Some("Commands")).is_none()).await;

    if click(drv, "Button", "More").await {
        wait(drv, "the menu", |d| d.a11y_node("Menu", None).is_some()).await;
        drv.ok(&Command::Move { x: PARK.0, y: PARK.1 }).await.unwrap();
        shot(drv, "more-menu-light").await;
        click(drv, "Button", "More").await;
        wait(drv, "the menu closed", |d| d.a11y_node("Menu", None).is_none()).await;
    }

    let dump = look(drv).await;
    let crumb =
        dump.a11y_node("Group", Some("Where")).map(|g| g.bounds).and_then(|[gx, gy, gw, gh]| {
            dump.a11y
                .iter()
                .find(|n| {
                    let [x, y, _, _] = n.bounds;
                    n.role == "Button" && x >= gx && x <= gx + gw && y >= gy && y <= gy + gh
                })
                .map(|n| n.bounds)
        });
    if let Some([x, y, w, h]) = crumb {
        drv.click(x + w / 2.0, y + h / 2.0).await.unwrap();
        wait(drv, "the workspaces' menu", |d| d.a11y_node("Menu", None).is_some()).await;
        drv.ok(&Command::Move { x: PARK.0, y: PARK.1 }).await.unwrap();
        both(stack, "breadcrumb-menu").await;
        let drv = &mut stack.driver;
        drv.keys("escape").await.unwrap();
        wait(drv, "the menu closed", |d| d.a11y_node("Menu", None).is_none()).await;
    } else {
        println!("showcase: no breadcrumb button");
    }

    settings(stack).await;
    day.stack.shutdown().await;
}

// ---------------------------------------------------------------------------------------------
// A Claude Code thread.
// ---------------------------------------------------------------------------------------------

/// A screenshot of a login page with its error, made up for the attachment.
fn login_screenshot() -> Vec<u8> {
    let picture = image::RgbImage::from_fn(960, 600, |x, y| {
        let card = (330..630).contains(&x) && (140..460).contains(&y);
        let field = (360..600).contains(&x) && ((220..252).contains(&y) || (270..302).contains(&y));
        let error = (360..600).contains(&x) && (320..344).contains(&y);
        let button = (360..600).contains(&x) && (380..416).contains(&y);
        if y < 40 {
            image::Rgb([235, 236, 240])
        } else if button {
            image::Rgb([52, 120, 246])
        } else if error {
            image::Rgb([253, 228, 226])
        } else if field {
            image::Rgb([244, 245, 248])
        } else if card {
            image::Rgb([255, 255, 255])
        } else {
            image::Rgb([246, 247, 249])
        }
    });
    let mut png = std::io::Cursor::new(Vec::new());
    picture.write_to(&mut png, image::ImageFormat::Png).unwrap();
    png.into_inner()
}

/// The thread view's list.
fn thread_shows(d: &Dump) -> bool {
    d.a11y_node("Group", Some("Thread")).is_some()
}

/// Scroll the thread by `lines` (positive goes up the history).
async fn scroll_thread(drv: &mut Driver, lines: f32) {
    let dump = look(drv).await;
    let Some(list) = dump.a11y_node("Group", Some("Thread")) else { return };
    let [x, y, w, h] = list.bounds;
    drv.scroll(x + w / 2.0, y + h / 2.0, 0.0, lines).await.unwrap();
}

/// A Claude Code thread mid-turn, full width: the settled turn with every step open and the
/// subagent's own thread; the composer with a picture attached and a draft; the request the
/// agent waits on; and a questionnaire.
#[tokio::test]
#[ignore = "showcase: cargo xtask e2e showcase"]
#[expect(clippy::too_many_lines, reason = "one thread, shown state by state")]
async fn showcase_a_claude_code_thread_mid_turn() {
    let scratch = tempfile::tempdir().unwrap();
    let home = made(scratch.path(), "home");
    let (path, zdotdir) = shells(scratch.path());
    let repo = home.join("code/atlas");
    make_repo(&repo);
    let shot_file = home.join("Desktop/login-error.png");
    std::fs::create_dir_all(shot_file.parent().unwrap()).unwrap();
    std::fs::write(&shot_file, login_screenshot()).unwrap();
    let home_str = home.to_string_lossy().into_owned();
    let env = [("HOME", home_str.as_str()), ("PATH", path.as_str()), ("ZDOTDIR", &zdotdir)];
    let mut stack = Stack::launch_with("studio", &env).await.unwrap();
    stack.driver.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    let shell = first_shell(&mut stack.driver).await;
    run(&mut stack.driver, &shell, "cd ~/code/atlas", "~/code/atlas").await;
    let terminal = agent_tile(&mut stack.driver, "Harden the session refresh").await;

    let id = "7f3c2a10-5b1e-4c7d-9a42-1e8f0c6d2b91";
    let main = scratch.path().join("projects").join(format!("{id}.jsonl"));
    let (transcript, survey) = long_session(id, &repo);
    write(&main, &transcript);
    let subagents = slopty_agent::conversation::subagents_dir(&main);
    write(&subagents.join(format!("agent-{SURVEY_AGENT}.jsonl")), &survey);
    let start = payload("SessionStart", id, &main, &repo, &json!({ "source": "startup" }));
    let begun = stack.relay_hook(&terminal, &[], &start).unwrap();
    assert!(begun.wait_with_output().await.unwrap().status.success(), "the relay ran");
    let status = json!({
        "session_id": id, "transcript_path": main, "cwd": repo,
        "model": { "id": "claude-opus-5-5", "display_name": "Opus 5.5" },
        "workspace": { "current_dir": repo, "project_dir": repo },
        "context_window": { "context_window_size": 200_000, "used_percentage": 61 },
        "cost": { "total_cost_usd": 3.84, "total_duration_ms": 1_912_000 },
        "rate_limits": {
            "five_hour": { "used_percentage": 42.0, "resets_at": now().saturating_add(7_200) },
            "seven_day": { "used_percentage": 18.5 }
        },
        "pr": { "number": 318, "url": "https://github.com/atlas/atlas/pull/318", "review_state": "pending" },
    });
    let line = stack.relay_hook(&terminal, &["statusline", "--command", "true"], &status).unwrap();
    assert!(line.wait_with_output().await.unwrap().status.success(), "the status line ran");
    let prompt = json!({ "prompt": "Now make the retry middleware send the same Idempotency-Key" });
    let asked = stack
        .relay_hook(&terminal, &[], &payload("UserPromptSubmit", id, &main, &repo, &prompt))
        .unwrap();
    assert!(asked.wait_with_output().await.unwrap().status.success(), "the relay ran");

    let drv = &mut stack.driver;
    drv.reveal(&terminal).await.unwrap();
    wait(drv, "the thread", |d| {
        thread_shows(d)
            && d.a11y
                .iter()
                .any(|n| n.label.as_deref().is_some_and(|l| l.contains("Run every test")))
    })
    .await;
    drv.keys("cmd-shift-enter").await.unwrap();
    drv.ok(&Command::Move { x: PARK.0, y: PARK.1 }).await.unwrap();
    both(&mut stack, "claude-thread").await;

    // A picture attached to a draft.
    let drv = &mut stack.driver;
    let tile = look(drv).await.item_for_session(&terminal).map(|i| i.bounds);
    if let Some([x, y, w, h]) = tile {
        drv.drop_files(&[shot_file.as_path()], x + w / 2.0, y + h / 2.0).await.unwrap();
        wait(drv, "the attachment's chip", |d| {
            d.a11y.iter().any(|n| n.label.as_deref().is_some_and(|l| l.contains("login-error.png")))
        })
        .await;
    }
    let dump = look(drv).await;
    if let Some([x, y, w, h]) =
        dump.a11y.iter().find(|n| n.label.as_deref() == Some("Message")).map(|n| n.bounds)
    {
        drv.click(x + w / 2.0, y + h / 2.0).await.unwrap();
    }
    drv.type_text("Also add a test for a retry that loses the key — the screenshot is what the app shows today")
        .await
        .unwrap();
    drv.ok(&Command::Move { x: PARK.0, y: PARK.1 }).await.unwrap();
    both(&mut stack, "claude-composer").await;

    // The workspace's tests, asked for, wait on the person.
    let mut ask = payload(
        "PermissionRequest",
        id,
        &main,
        &repo,
        &json!({
            "permission_mode": "acceptEdits", "tool_name": "Bash", "tool_input": workspace_tests(),
            "permission_suggestions": [
                { "type": "addRules", "destination": "localSettings", "behavior": "allow",
                  "rules": [{ "toolName": "Bash", "ruleContent": "cargo test:*" }] }
            ],
        }),
    );
    let held = stack.relay_hook(&terminal, &[], &ask).unwrap();
    let drv = &mut stack.driver;
    wait(drv, "the request over the composer", |d| d.a11y.iter().any(|n| n.role == "Dialog")).await;
    drv.ok(&Command::Move { x: PARK.0, y: PARK.1 }).await.unwrap();
    both(&mut stack, "claude-permission").await;
    let drv = &mut stack.driver;
    if click(drv, "Button", "Allow").await {
        let _answered = tokio::time::timeout(STEP, held.wait_with_output()).await;
    } else {
        drop(held);
    }

    // What the thread changed, in a review tile beside it.
    let drv = &mut stack.driver;
    if click(drv, "Button", "Review").await {
        wait(drv, "the review tile", |d| d.items.iter().any(|i| i.kind == "review")).await;
        drv.ok(&Command::Move { x: PARK.0, y: PARK.1 }).await.unwrap();
        both(&mut stack, "claude-review").await;
        let drv = &mut stack.driver;
        drv.reveal(&terminal).await.unwrap();
    }

    // Then three questions.
    let questions = json!([{
        "question": "Where should the key live while a request is retried?", "header": "Key",
        "multiSelect": false,
        "options": [
            { "label": "Request extensions", "description": "Kept on the request, never on the wire twice" },
            { "label": "A header copy", "description": "Copied header by header, as the edit does now" },
            { "label": "The retry policy", "description": "The policy remembers it per attempt" }
        ]
    }, {
        "question": "Which retries should keep it?", "header": "Retries", "multiSelect": true,
        "options": [
            { "label": "Refresh", "description": "POST /v1/sessions/refresh" },
            { "label": "Logout", "description": "DELETE /v1/sessions/:id" },
            { "label": "Every POST" }
        ]
    }, {
        "question": "Ship it in 0.4.3 or wait for 0.5?", "header": "Release",
        "multiSelect": false,
        "options": [
            { "label": "0.4.3", "description": "This week, with the refresh fix" },
            { "label": "0.5.0", "description": "With the audit log" }
        ]
    }]);
    ask = payload(
        "PermissionRequest",
        id,
        &main,
        &repo,
        &json!({
            "tool_name": "AskUserQuestion", "tool_input": { "questions": questions },
        }),
    );
    let held = stack.relay_hook(&terminal, &[], &ask).unwrap();
    let drv = &mut stack.driver;
    wait(drv, "the questions", |d| {
        d.a11y_node("RadioButton", Some("Request extensions")).is_some()
    })
    .await;
    drv.ok(&Command::Move { x: PARK.0, y: PARK.1 }).await.unwrap();
    both(&mut stack, "claude-questions").await;

    // The questions withdrawn, as when the person answers in the terminal instead; then every
    // step open, from the tail up to the prompt. Last, since a scroll of the open steps has
    // taken the app down (`showcase-notes.md`).
    drop(held);
    let drv = &mut stack.driver;
    wait(drv, "the questions gone", |d| {
        d.a11y_node("RadioButton", Some("Request extensions")).is_none()
    })
    .await;
    drv.reveal(&terminal).await.unwrap();
    // Every step open, from the tail up to the prompt.
    let drv = &mut stack.driver;
    drv.keys("ctrl-o").await.unwrap();
    wait(drv, "every step", |d| labels(d, "Button").iter().any(|l| l.starts_with("Thought"))).await;
    shot(drv, "claude-steps-1-light").await;
    for n in 2..=5 {
        // A few lines at a time, each scroll drawn before the next.
        for _ in 0..3 {
            scroll_thread(drv, 5.0).await;
            settled(drv).await;
        }
        drv.ok(&Command::Move { x: PARK.0, y: PARK.1 }).await.unwrap();
        shot(drv, &format!("claude-steps-{n}-light")).await;
    }
    stack.set_appearance("dark").unwrap();
    wait(&mut stack.driver, "the dark theme", |d| d.dark).await;
    shot(&mut stack.driver, "claude-steps-5-dark").await;
    stack.set_appearance("light").unwrap();
    let drv = &mut stack.driver;
    wait(drv, "the light theme", |d| !d.dark).await;

    // The subagent's own thread, its call found further up in the first turn, opened from its
    // fold.
    let mut subagent = None;
    let mut unfolded = false;
    for _ in 0..40 {
        let dump = look(drv).await;
        let button = |starts: &str| {
            dump.a11y.iter().find_map(|n| {
                (n.role == "Button" && n.label.as_deref().is_some_and(|l| l.starts_with(starts)))
                    .then(|| n.label.clone().unwrap_or_default())
            })
        };
        // Clear of the tray over the list's foot, which would take the click.
        let clear = dump.a11y.iter().any(|n| {
            n.role == "Button"
                && n.label.as_deref().is_some_and(|l| l.starts_with("Subagent "))
                && n.bounds[1] + n.bounds[3] < 380.0
        });
        subagent = button("Subagent ").filter(|_| clear);
        if subagent.is_some() {
            break;
        }
        if let Some(fold) = button("Worked: Edited 2 files").filter(|_| !unfolded) {
            unfolded = click(drv, "Button", &fold).await;
            settled(drv).await;
            continue;
        }
        // Up to the first turn's fold, then down through what it opened.
        scroll_thread(drv, if unfolded { -5.0 } else { 5.0 }).await;
    }
    if let Some(label) = subagent {
        click(drv, "Button", &label).await;
        wait(drv, "the subagent's thread", |d| {
            labels(d, "Navigation").iter().any(|l| l.starts_with("Subagent "))
        })
        .await;
        drv.ok(&Command::Move { x: PARK.0, y: PARK.1 }).await.unwrap();
        both(&mut stack, "claude-subagent").await;
        let drv = &mut stack.driver;
        drv.keys("escape").await.unwrap();
    } else {
        println!("showcase: no subagent's call in view");
    }
    let drv = &mut stack.driver;
    drv.keys("ctrl-o").await.unwrap();
    for _ in 0..8 {
        scroll_thread(drv, -40.0).await;
    }

    stack.shutdown().await;
}

// ---------------------------------------------------------------------------------------------
// A project's board.
// ---------------------------------------------------------------------------------------------

/// Write the project stack's settings in `appearance`, still pointed at its server.
fn project_appearance(stack: &ProjectStack, appearance: &str) {
    let settings = format!(
        "{}\n[client]\nserver = \"{}\"\n",
        pinned_settings(appearance),
        stack.server.address()
    );
    std::fs::write(stack.path("app").join("settings.toml"), settings).unwrap();
}

/// Render the board's `name`, light and dark.
async fn board_both(stack: &mut ProjectStack, name: &str) {
    shot(&mut stack.driver, &format!("{name}-light")).await;
    project_appearance(stack, "dark");
    wait(&mut stack.driver, "the dark theme", |d| d.dark).await;
    shot(&mut stack.driver, &format!("{name}-dark")).await;
    project_appearance(stack, "light");
    wait(&mut stack.driver, "the light theme", |d| !d.dark).await;
}

/// The showcase project's name.
const PROJECT: &str = "atlas-043";

/// A project of eleven tasks in every state: running, blocked, planned, reviewed with changes
/// asked, a verifier that broke, two waiting in the merge queue, merged, failed. Its tree, its
/// lanes and its timeline.
#[tokio::test]
#[ignore = "showcase: cargo xtask e2e showcase"]
#[expect(clippy::too_many_lines, reason = "one project, made task by task")]
async fn showcase_a_project_board_in_every_state() {
    let mut stack = ProjectStack::launch("studio").await.unwrap();
    stack.driver.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    let repo = stack.path("repo").to_string_lossy().into_owned();
    let term = |v: &Value| v["term"].as_str().unwrap_or_default().to_owned();
    let session_of = |t: &str| t.rsplit_once('/').map_or(t, |(_, s)| s).to_owned();

    let orchestrator = term(&stack.slopty(&["agent", "spawn", "--cwd", &repo]).await.unwrap());
    stack
        .slopty(&[
            "project",
            "create",
            PROJECT,
            "--title",
            "Atlas 0.4.3: sessions that cannot be replayed",
            "--repo",
            "atlas",
            "--verifier",
            "cargo nextest run --workspace",
            "--review",
            "Every wire change comes with its migration and a test",
            "--orchestrator",
            &orchestrator,
        ])
        .await
        .unwrap();
    for args in [
        &[
            "--title",
            "Lock the refresh row in one transaction",
            "--owns",
            "crates/api/src/session.rs",
        ][..],
        &[
            "--title",
            "Revoke the family on a replayed token",
            "--owns",
            "crates/store/src/family.rs",
            "--parent",
            "1",
        ],
        &[
            "--title",
            "Keep the idempotency key on a retry",
            "--owns",
            "crates/api/src/middleware.rs",
        ],
        &["--title", "Audit log table and migration", "--owns", "migrations/0007_audit_log.sql"],
        &["--title", "Write the replay runbook", "--owns", "docs/runbooks/replay.md"],
        &["--title", "Load test the refresh path", "--read-only", "--depends-on", "1"],
        &["--title", "Bump tokio to 1.48", "--owns", "Cargo.lock"],
        &[
            "--title",
            "Trace every request with its session id",
            "--owns",
            "crates/api/src/trace.rs",
        ],
        &["--title", "ws-gateway: cap the reconnect backoff", "--owns", "crates/api/src/ws.rs"],
        &["--title", "Grafana board for refresh replays", "--owns", "ops/grafana/refresh.json"],
        &["--title", "Drop the v1 token format", "--owns", "crates/proto/src/token.rs"],
    ] {
        let mut create = vec!["task", "create", "--project", PROJECT];
        create.extend_from_slice(args);
        stack.slopty(&create).await.unwrap();
    }
    let started = tokio::time::Instant::now();
    loop {
        let workers = stack.slopty(&["workers"]).await.unwrap();
        let listed = workers.as_array().into_iter().flatten();
        if listed.into_iter().any(|w| w["facts"]["agents"]["claude"].is_string()) {
            break;
        }
        assert!(started.elapsed() < STEP, "Claude Code on the worker: {workers}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let mut agents = Vec::new();
    for task in ["1", "3", "9"] {
        let spawned = stack
            .slopty(&["task", "spawn", "--project", PROJECT, task, "--cwd", &repo])
            .await
            .unwrap();
        agents.push(session_of(&term(&spawned)));
    }
    let update = |task: &'static str, more: &'static [&'static str]| {
        let mut args = vec!["task", "update", "--project", PROJECT, "--task", task];
        args.extend_from_slice(more);
        args
    };
    for args in [
        update("1", &["--branch", "atlas/043/1", "--status", "Locking with SELECT … FOR UPDATE"]),
        update("3", &["--branch", "atlas/043/3", "--status", "Copying the key across retries"]),
        update("9", &["--branch", "atlas/043/9", "--status", "Reproducing the 64 s backoff"]),
        update(
            "2",
            &["--state", "blocked", "--status", "Revoke on the first replay, or the second?"],
        ),
        update("6", &["--passed", "--head", "71c9e0a", "--base", "c08d4c1"]),
        update(
            "7",
            &[
                "--state",
                "done",
                "--passed",
                "--summary",
                "212 tests passed",
                "--head",
                "4a7aa6d",
                "--base",
                "c08d4c1",
            ],
        ),
        update(
            "8",
            &[
                "--state",
                "done",
                "--passed",
                "--summary",
                "212 tests passed",
                "--head",
                "e5b0d17",
                "--base",
                "c08d4c1",
            ],
        ),
        update(
            "10",
            &[
                "--state",
                "done",
                "--passed",
                "--summary",
                "212 tests passed",
                "--head",
                "0b3f6c2",
                "--base",
                "c08d4c1",
            ],
        ),
        update(
            "4",
            &[
                "--failed",
                "--summary",
                "   Compiling atlas-store v0.4.2\nerror[E0063]: missing field `actor` in initializer of `AuditRow`\n  --> crates/store/src/audit.rs:41:9\nerror: could not compile `atlas-store`",
                "--head",
                "9c1e2f3",
                "--base",
                "c08d4c1",
            ],
        ),
        update("11", &["--state", "failed", "--note", "the iOS app still sends v1 tokens"]),
    ] {
        stack.slopty(&args).await.unwrap();
    }
    stack.slopty(&update("7", &["--state", "merged"])).await.unwrap();
    stack
        .slopty(&[
            "task", "review", "--project", PROJECT, "--task", "6", "--changes", "--summary",
            "Two blockers", "--finding",
            "crates/api/benches/refresh.rs:58: The load test reuses one token, so it never takes the lock.",
            "--finding",
            "docs/runbooks/replay.md: No step says how to tell a replay from a client bug.",
        ])
        .await
        .unwrap();

    let ready = agents.clone();
    wait(&mut stack.driver, "the project and its agents", |d| {
        d.projects.iter().any(|p| p.id == PROJECT && p.tasks.len() == 11)
            && ready
                .iter()
                .all(|a| d.items.iter().any(|i| i.session.as_deref() == Some(a.as_str())))
    })
    .await;
    let drv = &mut stack.driver;
    drv.reveal(&session_of(&orchestrator)).await.unwrap();
    drv.keys("cmd-shift-enter").await.unwrap();
    drv.keys("cmd-shift-j").await.unwrap();
    wait(drv, "the board", |d| d.projects.iter().any(|p| p.id == PROJECT && p.shown)).await;
    drv.ok(&Command::Move { x: PARK.0, y: PARK.1 }).await.unwrap();
    board_both(&mut stack, "project-tree").await;
    for (key, lens, name) in
        [("2", "Board", "project-lanes"), ("3", "Timeline", "project-timeline")]
    {
        let drv = &mut stack.driver;
        drv.keys(key).await.unwrap();
        wait(drv, lens, |d| {
            d.projects.iter().any(|p| p.id == PROJECT && p.lens.as_deref() == Some(lens))
        })
        .await;
        board_both(&mut stack, name).await;
    }
    let drv = &mut stack.driver;
    drv.keys("1").await.unwrap();
    drv.keys("down down enter").await.unwrap();
    wait(drv, "task 1's agent", |d| !d.focused.starts_with("project:")).await;
    shot(drv, "project-task-agent-light").await;

    stack.shutdown().await;
}

// ---------------------------------------------------------------------------------------------
// Codex, pi and opencode threads, started from the palette.
// ---------------------------------------------------------------------------------------------

/// A recording of an agent's session, as the adapters' own tests replay it.
fn fixture(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../slopty-agent/tests/fixtures")
        .join(path)
        .canonicalize()
        .unwrap()
}

/// What `codex --version` says: the worker lists Codex by its program, and its threads go to
/// the person's daemon, which the test plays.
const CODEX: &str = "#!/bin/sh\necho 'codex-cli 0.160.0'\n";

/// Codex's answer to the first turn, in place of the recording's "Hello.".
const CODEX_ANSWER: &str = "The refresh path changed in three places this week:\n\n\
1. **`session.rs`** locks the refresh row with `SELECT … FOR UPDATE`, so two refreshes of one \
token can no longer both win.\n\
2. **`middleware.rs`** keeps the `Idempotency-Key` across retries instead of minting a new one.\n\
3. **`routes.rs`** gives every request a 10 s timeout.\n\n\
| Change | Tests | Risk |\n|---|---|---|\n| Row lock | 4 new | Deadlock under load |\n\
| Retry key | 2 new | None seen |\n| Timeout | none | Slow refreshes now fail |\n\n\
The lock has no load test yet; I can run the refresh tests next.";

/// What the recording's command and reason become: the showcase's Codex asks to run the tests.
const CODEX_REWRITES: [(&str, &str); 6] = [
    ("touch made-by-codex", "cargo test -p atlas-api refresh"),
    (r#"["touch","made-by-codex"]"#, r#"["cargo","test","-p","atlas-api","refresh"]"#),
    (
        "It writes a file in the read-only workspace.",
        "It builds and runs the tests, which the read-only sandbox does not allow.",
    ),
    ("Make a file called made-by-codex.", "Run the refresh tests."),
    ("mock-model", "gpt-5.5-codex"),
    ("mock_provider", "openai"),
];

type Ws = tokio_tungstenite::WebSocketStream<tokio::net::UnixStream>;

/// What the pinned Codex and the client that started a thread said to each other
/// (`codex/approval.jsonl`, client `a`): whether the client sent it, and the frame.
fn codex_recording() -> Vec<(bool, Value)> {
    std::fs::read_to_string(fixture("codex/approval.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|line| line["client"] == "a")
        .map(|line| (line["dir"] == "sent", line["msg"].clone()))
        .collect()
}

/// Where the recording answered the request it sent at `at`.
fn answered(lines: &[(bool, Value)], at: usize) -> usize {
    let id = &lines[at].1["id"];
    let after = lines
        .iter()
        .skip(at)
        .position(|(sent, msg)| !sent && msg["id"] == *id && msg.get("result").is_some());
    at.saturating_add(after.unwrap())
}

/// `frame` as the showcase's Codex says it: in `cwd`, with its model and command.
fn codex_frame(frame: &Value, cwd: &str) -> Value {
    let mut text = frame.to_string().replace("/work", cwd);
    for (from, to) in CODEX_REWRITES {
        text = text.replace(from, to);
    }
    let mut frame: Value = serde_json::from_str(&text).unwrap();
    let item = &mut frame["params"]["item"];
    if item["type"] == "agentMessage" && item["text"] == "Hello." {
        item["text"] = json!(CODEX_ANSWER);
    }
    frame
}

async fn ws_next(ws: &mut Ws) -> Option<Value> {
    use futures_util::StreamExt as _;
    use tokio_tungstenite::tungstenite::Message;
    loop {
        match ws.next().await? {
            Ok(Message::Text(text)) => return serde_json::from_str(&text).ok(),
            Ok(Message::Close(_)) | Err(_) => return None,
            Ok(_) => {}
        }
    }
}

async fn ws_say(ws: &mut Ws, frame: &Value) {
    use futures_util::SinkExt as _;
    use tokio_tungstenite::tungstenite::Message;
    let _gone = ws.send(Message::text(frame.to_string())).await;
}

/// The person's Codex daemon, played from the recording on `listener` for every connection.
async fn codex_daemon(listener: tokio::net::UnixListener) {
    while let Ok((stream, _)) = listener.accept().await {
        tokio::spawn(async move {
            if let Ok(ws) = tokio_tungstenite::accept_async(stream).await {
                codex_serve(ws).await;
            }
        });
    }
}

/// One connection: the handshake as the recording answered it, no thread loaded, a start as
/// the recording's in the folder asked for, and each turn as the recording's turns went, under
/// the ids the worker chose and with the person's own words. The second turn stops at Codex's
/// request to run a command, which stays unanswered for the render. Any other request gets an
/// empty answer.
async fn codex_serve(mut ws: Ws) {
    let lines = codex_recording();
    let sent_at = |method: &str| {
        lines.iter().position(|(sent, msg)| *sent && msg["method"] == method).unwrap()
    };
    let turns: Vec<usize> = (0..lines.len())
        .filter(|at| lines[*at].0 && lines[*at].1["method"] == "turn/start")
        .collect();
    let mut cwd = String::new();
    let mut turn = 0_usize;
    while let Some(asked) = ws_next(&mut ws).await {
        let id = asked.get("id").cloned();
        let reply = |result: Value| json!({ "id": id.clone(), "result": result });
        match asked["method"].as_str().unwrap_or_default() {
            "initialize" => {
                let at = answered(&lines, sent_at("initialize"));
                ws_say(&mut ws, &reply(lines[at].1["result"].clone())).await;
            }
            "thread/loaded/list" => {
                ws_say(&mut ws, &reply(json!({ "data": [], "nextCursor": null }))).await;
            }
            "thread/start" => {
                asked["params"]["cwd"].as_str().unwrap_or("/").clone_into(&mut cwd);
                let at = answered(&lines, sent_at("thread/start"));
                let started = codex_frame(&reply(lines[at].1["result"].clone()), &cwd);
                ws_say(&mut ws, &started).await;
                let note = lines.iter().find(|(sent, m)| !sent && m["method"] == "thread/started");
                ws_say(&mut ws, &codex_frame(&note.unwrap().1, &cwd)).await;
            }
            "turn/start" if turn < turns.len() => {
                let (from, to) = (turns[turn], turns.get(turn.saturating_add(1)).copied());
                turn = turn.saturating_add(1);
                let reply_at = answered(&lines, from);
                let words = asked["params"]["input"][0]["text"].clone();
                let client = asked["params"]["clientUserMessageId"].clone();
                let end = to.unwrap_or(lines.len());
                for (at, (sent, frame)) in
                    lines.iter().enumerate().take(end).skip(from.saturating_add(1))
                {
                    let method = frame["method"].as_str().unwrap_or_default();
                    let skipped = ["thread/started", "warning", "remoteControl/status/changed"];
                    if *sent
                        || skipped.contains(&method)
                        || (frame.get("result").is_some() && at != reply_at)
                    {
                        continue;
                    }
                    let mut frame = codex_frame(frame, &cwd);
                    if at == reply_at {
                        frame["id"] = id.clone().unwrap_or(Value::Null);
                    }
                    if frame["params"]["item"]["type"] == "userMessage" {
                        frame["params"]["item"]["clientId"] = client.clone();
                        frame["params"]["item"]["content"][0]["text"] = words.clone();
                    }
                    ws_say(&mut ws, &frame).await;
                    tokio::time::sleep(Duration::from_millis(30)).await;
                    if method == "item/commandExecution/requestApproval" {
                        break;
                    }
                }
            }
            _ if id.is_some() => ws_say(&mut ws, &reply(json!({}))).await,
            _ => {}
        }
    }
}

/// A project stack whose worker has Codex (its daemon played by the test), pi
/// (`slopty-stub-pi`) and `opencode` (`slopty-stub-acp`) beside Claude Code, with a shell in a
/// repository of the day's work.
struct Threads {
    stack: ProjectStack,
    shell: String,
    _codex_home: tempfile::TempDir,
    _tools: tempfile::TempDir,
    daemon: tokio::task::JoinHandle<()>,
}

impl Threads {
    async fn begin() -> Self {
        // Short, as a Unix socket's path must be.
        let codex_home = tempfile::Builder::new().prefix("sc-").tempdir_in("/tmp").unwrap();
        let socket = codex_home.path().join("app-server-control/app-server-control.sock");
        std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
        let daemon = tokio::spawn(codex_daemon(tokio::net::UnixListener::bind(&socket).unwrap()));
        let tools = tempfile::tempdir().unwrap();
        let codex = tools.path().join("codex");
        script(&codex, CODEX);
        let claude = slopty_e2e::harness::stub_claude().await.unwrap();
        let pi = claude.with_file_name("slopty-stub-pi");
        let acp = claude.with_file_name("slopty-stub-acp");
        let codex_env = codex_home.path().to_string_lossy().into_owned();
        let programs: [(&str, &Path); 3] = [("codex", &codex), ("pi", &pi), ("opencode", &acp)];
        let mut stack =
            ProjectStack::launch_with("studio", &programs, &[("CODEX_HOME", &codex_env)])
                .await
                .unwrap();
        let bin = stack.path("programs");
        for (stub, recording) in [("pi", "pi/gate.jsonl"), ("acp", "acp/turns.jsonl")] {
            let config = json!({
                "fixture": fixture(recording),
                "record": stack.path(&format!("{stub}-record.json")),
            });
            write(&bin.join(format!("stub-{stub}.json")), &config.to_string());
        }
        let home = std::fs::canonicalize(stack.path("home")).unwrap();
        make_repo(&home.join("code/atlas"));
        stack.driver.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
        let shell = first_shell(&mut stack.driver).await;
        run(&mut stack.driver, &shell, "cd ~/code/atlas", "atlas").await;
        let started = tokio::time::Instant::now();
        loop {
            let workers = stack.slopty(&["workers"]).await.unwrap();
            let listed = workers.as_array().into_iter().flatten();
            let ready = listed.into_iter().any(|w| {
                let facts = &w["facts"];
                ["claude", "codex", "pi"].iter().all(|a| facts["agents"][a].is_string())
                    && !facts["acp"]["opencode"].is_null()
            });
            if ready {
                break;
            }
            assert!(started.elapsed() < STEP, "the agents on the worker: {workers}");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Self { stack, shell, _codex_home: codex_home, _tools: tools, daemon }
    }

    async fn end(self) {
        self.daemon.abort();
        self.stack.shutdown().await;
    }
}

fn palette_up(d: &Dump) -> bool {
    d.a11y_node("Dialog", Some("Commands")).is_some()
}

/// The palette's lines that start with `line`.
fn offering(d: &Dump, line: &str) -> bool {
    d.a11y.iter().any(|n| {
        n.role == "ListBoxOption" && n.label.as_deref().is_some_and(|l| l.starts_with(line))
    })
}

/// Open the palette with `typed` in it, offering `line`.
async fn palette_with(drv: &mut Driver, typed: &str, line: &str) {
    drv.keys("cmd-shift-p").await.unwrap();
    wait(drv, "the palette", palette_up).await;
    drv.type_text(typed).await.unwrap();
    wait(drv, line, |d| offering(d, line)).await;
}

/// The thread tiles' ids.
fn threads(d: &Dump) -> Vec<String> {
    d.items.iter().filter(|i| i.kind == "thread").map(|i| i.id.clone()).collect()
}

/// Start `agent`'s thread from the palette in the shell's folder; its tile's id.
async fn start_thread(t: &mut Threads, agent: &str) -> Option<String> {
    let drv = &mut t.stack.driver;
    drv.reveal(&t.shell).await.unwrap();
    let before = threads(&look(drv).await);
    let line = format!("New {agent} thread");
    palette_with(drv, &line.to_lowercase(), &line).await;
    drv.keys("enter").await.unwrap();
    let dump =
        wait(drv, "the thread's tile", |d| threads(d).iter().any(|i| !before.contains(i))).await;
    threads(&dump).into_iter().find(|i| !before.contains(i))
}

/// Type `text` into the composer of tile `item` and send it.
async fn send(drv: &mut Driver, item: &str, text: &str) {
    let dump = look(drv).await;
    let Some([left, top, width, height]) =
        dump.items.iter().find(|i| i.id == item).map(|i| i.bounds)
    else {
        println!("showcase: no tile {item}");
        return;
    };
    let inside =
        |b: &[f32; 4]| b[0] >= left && b[1] >= top && b[0] <= left + width && b[1] <= top + height;
    let composer = dump
        .a11y
        .iter()
        .find(|n| n.label.as_deref() == Some("Message") && inside(&n.bounds))
        .map(|n| n.bounds);
    let Some([x, y, w, h]) = composer else {
        println!("showcase: no composer in {item}");
        return;
    };
    drv.click(w.mul_add(0.5, x), h.mul_add(0.5, y)).await.unwrap();
    drv.type_text(text).await.unwrap();
    drv.keys("enter").await.unwrap();
}

/// Whether some node's label or value says `text`.
fn says(d: &Dump, text: &str) -> bool {
    d.a11y.iter().any(|n| {
        n.label.as_deref().is_some_and(|l| l.contains(text))
            || n.value.as_deref().is_some_and(|v| v.contains(text))
    })
}

/// Render the project stack's frame as `name`, light and dark.
async fn project_both(stack: &mut ProjectStack, name: &str) {
    stack.driver.ok(&Command::Move { x: PARK.0, y: PARK.1 }).await.unwrap();
    board_both(stack, name).await;
}

/// `agent`'s thread from the palette: a first turn answered, then a second that waits on the
/// person's approval. Its tile beside the shell, light, then maximised in both themes.
async fn agent_thread(t: &mut Threads, agent: &str, name: &str, turns: [(&str, &str); 2]) {
    let Some(item) = start_thread(t, agent).await else { return };
    let drv = &mut t.stack.driver;
    let [(first, answer), (second, asked)] = turns;
    send(drv, &item, first).await;
    // A message sent while a turn runs steers it, so the second waits for the first to end.
    let idle = |d: &Dump| d.a11y_node("Button", Some("Stop")).is_none();
    wait(drv, answer, |d| says(d, answer) && idle(d)).await;
    send(drv, &item, second).await;
    wait(drv, asked, |d| says(d, asked)).await;
    drv.ok(&Command::Move { x: PARK.0, y: PARK.1 }).await.unwrap();
    shot(drv, &format!("{name}-tile-light")).await;
    drv.keys("cmd-shift-enter").await.unwrap();
    project_both(&mut t.stack, name).await;
    t.stack.driver.keys("cmd-shift-enter").await.unwrap();
}

/// The palette's "New … thread" lines for every agent the worker has, then a Codex thread
/// started from it: a first turn answered with a table, a second waiting on a command.
#[tokio::test]
#[ignore = "showcase: cargo xtask e2e showcase"]
async fn showcase_the_palette_starting_a_codex_thread() {
    let mut t = Threads::begin().await;
    let drv = &mut t.stack.driver;
    palette_with(drv, "new", "New Codex thread").await;
    wait(drv, "every agent's line", |d| {
        ["Claude Code", "Codex", "pi", "opencode"]
            .iter()
            .all(|a| offering(d, &format!("New {a} thread")))
    })
    .await;
    project_both(&mut t.stack, "palette-new-thread").await;
    let drv = &mut t.stack.driver;
    drv.keys("escape").await.unwrap();
    wait(drv, "the palette closed", |d| !palette_up(d)).await;
    agent_thread(
        &mut t,
        "Codex",
        "codex-thread",
        [
            ("What changed in the session refresh this week?", "three places"),
            ("Run the refresh tests.", "cargo test -p atlas-api refresh"),
        ],
    )
    .await;
    t.end().await;
}

/// A pi thread started from the palette, driven over pi's RPC mode with Slopty's gate: a first
/// turn answered, a second waiting on the gate.
#[tokio::test]
#[ignore = "showcase: cargo xtask e2e showcase"]
async fn showcase_a_pi_thread() {
    let mut t = Threads::begin().await;
    let turns = [("Say hello.", "Hello"), ("Make a file called made-by-pi.", "made-by-pi")];
    agent_thread(&mut t, "pi", "pi-thread", turns).await;
    t.end().await;
}

/// An `opencode` thread started from the palette, driven over ACP: a first turn with its
/// thinking, a second waiting on its permission to write a file.
#[tokio::test]
#[ignore = "showcase: cargo xtask e2e showcase"]
async fn showcase_an_opencode_thread() {
    let mut t = Threads::begin().await;
    let turns = [("Say hello.", "Hello"), ("Make a file called made-by-acp.", "made-by-acp")];
    agent_thread(&mut t, "opencode", "opencode-thread", turns).await;
    t.end().await;
}
