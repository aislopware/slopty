//! `xtask prune` on fixture target directories: units, caches and objects laid out as cargo 1.98
//! and cargo 1.101 nightly (the 1.100 layout) write them, with the access and modification times
//! of the use each test stages. No test runs cargo.

#![expect(clippy::arithmetic_side_effects, reason = "fixture times and sizes of a few MB")]

use std::fs::{File, FileTimes};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use camino::{Utf8Path, Utf8PathBuf};

use super::{GB, Limits, Options, Report, check_room, gigabytes, prune, size, tally};

const HOUR: u64 = 3600;
const MIB: usize = 1 << 20;

struct Fixture {
    root: Utf8PathBuf,
    now: SystemTime,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("xtask-prune-{name}-{}", std::process::id()));
        let _gone = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let root = Utf8PathBuf::from_path_buf(dir).unwrap();
        Self { root, now: SystemTime::now() }
    }

    fn ago(&self, secs: u64) -> SystemTime {
        self.now - Duration::from_secs(secs)
    }

    /// A profile directory: cargo's locks and nothing built yet.
    fn profile(&self, rel: &str) -> Utf8PathBuf {
        let dir = self.root.join(rel);
        std::fs::create_dir_all(&dir).unwrap();
        for lock in [".cargo-lock", ".cargo-build-lock", ".cargo-artifact-lock"] {
            File::create(dir.join(lock)).unwrap();
        }
        dir
    }

    /// A ledger that has seen passes since a month ago, the last `pass` seconds ago.
    fn ledger(&self, profile: &Utf8Path, pass: u64) {
        let since = secs(self.ago(30 * 24 * HOUR));
        let pass = secs(self.ago(pass));
        std::fs::write(profile.join(".xtask-prune"), format!("since {since}\npass {pass}\n"))
            .unwrap();
    }

    /// A unit in cargo's layout to 1.99: its fingerprint files last read at `used` and written
    /// at `built`, and an rlib of `bytes`.
    fn old_unit(&self, profile: &Utf8Path, name: &str, hash: &str, used: u64, built: u64) {
        let unit = profile.join(".fingerprint").join(format!("{name}-{hash}"));
        std::fs::create_dir_all(&unit).unwrap();
        for file in [format!("lib-{name}"), format!("lib-{name}.json"), format!("dep-lib-{name}")] {
            write(&unit.join(file), 64, self.ago(used), self.ago(built));
        }
        let deps = profile.join("deps");
        std::fs::create_dir_all(&deps).unwrap();
        write(&deps.join(format!("lib{name}-{hash}.rlib")), MIB, self.ago(built), self.ago(built));
        write(&deps.join(format!("{name}-{hash}.d")), 64, self.ago(built), self.ago(built));
        stamp(&unit, self.ago(built));
    }

    /// A unit in cargo's layout from 1.100: `build/<package>/<hash>/` with its fingerprint and
    /// output inside.
    fn new_unit(&self, profile: &Utf8Path, package: &str, hash: &str, used: u64, built: u64) {
        let unit = profile.join("build").join(package).join(hash);
        let fingerprint = unit.join("fingerprint");
        std::fs::create_dir_all(&fingerprint).unwrap();
        std::fs::create_dir_all(unit.join("out")).unwrap();
        for file in [format!("lib-{package}"), format!("lib-{package}.json")] {
            write(&fingerprint.join(file), 64, self.ago(used), self.ago(built));
        }
        let rlib = unit.join("out").join(format!("lib{package}-{hash}.rlib"));
        write(&rlib, MIB, self.ago(built), self.ago(built));
        stamp(&fingerprint, self.ago(built));
        stamp(&unit.join("out"), self.ago(built));
        stamp(&unit, self.ago(built));
    }

    /// An incremental cache whose last session was compiled `compiled` seconds ago, named and
    /// locked as rustc leaves one. Returns its lock file.
    fn cache(&self, profile: &Utf8Path, name: &str, compiled: u64) -> Utf8PathBuf {
        self.session(profile, name, SESSION, Some(SESSION_LOCK), compiled).unwrap()
    }

    /// A session directory of a MiB in a crate's cache, with its lock file if `lock` is named.
    fn session(
        &self,
        profile: &Utf8Path,
        name: &str,
        session: &str,
        lock: Option<&str>,
        compiled: u64,
    ) -> Option<Utf8PathBuf> {
        let cache = profile.join("incremental").join(name);
        let dir = cache.join(session);
        std::fs::create_dir_all(&dir).unwrap();
        write(&dir.join("dep-graph.bin"), MIB, self.ago(compiled), self.ago(compiled));
        stamp(&dir, self.ago(compiled));
        let lock = lock.map(|l| cache.join(l));
        if let Some(lock) = &lock {
            write(lock, 0, self.ago(compiled), self.ago(compiled));
        }
        stamp(&cache, self.ago(compiled));
        lock
    }

    fn opts() -> Options {
        Options {
            idle: Duration::from_secs(24 * HOUR),
            limits: Limits { budget: u64::MAX, floor: 0 },
            wait: false,
            dry_run: false,
        }
    }

    fn prune(&self, opts: Options) -> Report {
        prune(&self.root, opts, self.now, &|_| Ok(u64::MAX / 2)).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _gone = std::fs::remove_dir_all(&self.root);
    }
}

fn secs(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH).unwrap().as_secs()
}

fn write(path: &Utf8Path, bytes: usize, accessed: SystemTime, modified: SystemTime) {
    std::fs::write(path, vec![7_u8; bytes]).unwrap();
    let times = FileTimes::new().set_accessed(accessed).set_modified(modified);
    File::options().write(true).open(path).unwrap().set_times(times).unwrap();
}

/// A directory's times, set after its contents are written (writing them moves its mtime).
fn stamp(dir: &Utf8Path, modified: SystemTime) {
    let times = FileTimes::new().set_accessed(modified).set_modified(modified);
    File::open(dir).unwrap().set_times(times).unwrap();
}

fn accessed(path: &Utf8Path) -> SystemTime {
    std::fs::metadata(path).unwrap().accessed().unwrap()
}

const A: &str = "0123456789abcdef";
const B: &str = "fedcba9876543210";
const C: &str = "00112233445566ff";
const D: &str = "aabbccddeeff0011";

/// A finished session of an incremental cache and its lock file, named as rustc names them.
const SESSION: &str = "s-hmr8xe0qn1-0nvpiz2-b2u4rz5npeowc56h09c99rqrg";
const SESSION_LOCK: &str = "s-hmr8xe0qn1-0nvpiz2.lock";

/// Everything the old layout keeps: a unit last read three days ago goes with its artifacts,
/// one read an hour ago stays and its access times go back to the epoch so the next read stamps
/// them, an artifact whose unit is gone goes, and so does a cache no compile touched for a day.
#[test]
fn an_idle_unit_its_artifacts_an_orphan_and_an_old_cache_go() {
    let fx = Fixture::new("idle");
    let debug = fx.profile("debug");
    fx.ledger(&debug, HOUR);
    fx.old_unit(&debug, "idle", A, 72 * HOUR, 80 * HOUR);
    fx.old_unit(&debug, "used", B, HOUR, 120 * HOUR);
    write(&debug.join("deps").join(format!("libgone-{C}.rlib")), 64, fx.ago(HOUR), fx.ago(HOUR));
    fx.cache(&debug, "idle-0abc", 30 * HOUR);
    fx.cache(&debug, "used-0def", 2 * HOUR);

    let report = fx.prune(Fixture::opts());

    assert!(!debug.join(format!(".fingerprint/idle-{A}")).exists());
    assert!(!debug.join(format!("deps/libidle-{A}.rlib")).exists());
    assert!(!debug.join(format!("deps/idle-{A}.d")).exists());
    assert!(!debug.join(format!("deps/libgone-{C}.rlib")).exists());
    assert!(!debug.join("incremental/idle-0abc").exists());
    assert!(debug.join(format!("deps/libused-{B}.rlib")).exists());
    assert!(debug.join("incremental/used-0def").exists());
    let evidence = debug.join(format!(".fingerprint/used-{B}/lib-used"));
    assert_eq!(accessed(&evidence), UNIX_EPOCH, "the next read stamps it again");
    let ledger = std::fs::read_to_string(debug.join(".xtask-prune")).unwrap();
    let line = format!("{} .fingerprint/used-{B}", secs(fx.ago(HOUR)));
    assert!(ledger.lines().any(|l| l == line), "{ledger}");
    assert_eq!((report.idle.units, report.idle.caches, report.idle.leftovers), (1, 1, 1));
    assert_eq!(report.busy, []);

    // The next pass reads the use from the ledger, not the reset access time.
    let again = fx.prune(Fixture::opts());
    assert!(again.idle.is_empty(), "{:?}", again.idle);
    assert!(debug.join(format!("deps/libused-{B}.rlib")).exists());
}

/// Without a ledger the access times were never reset, so an old one proves nothing: the first
/// pass deletes no unit, starts the ledger and resets the times.
#[test]
fn the_first_pass_keeps_every_unit_and_starts_the_ledger() {
    let fx = Fixture::new("first");
    let debug = fx.profile("debug");
    std::fs::write(debug.join(".xtask-prune"), "1790700797").unwrap();
    fx.old_unit(&debug, "idle", A, 72 * HOUR, 80 * HOUR);

    let report = fx.prune(Fixture::opts());

    assert_eq!(report.idle.units, 0);
    assert!(debug.join(format!("deps/libidle-{A}.rlib")).exists());
    assert_eq!(accessed(&debug.join(format!(".fingerprint/idle-{A}/lib-idle"))), UNIX_EPOCH);
    let ledger = std::fs::read_to_string(debug.join(".xtask-prune")).unwrap();
    assert!(ledger.starts_with("since "), "{ledger}");
    let line = format!("{} .fingerprint/idle-{A}", secs(fx.now));
    assert!(ledger.lines().any(|l| l == line), "its clock starts now: {ledger}");

    // A day later with no read in between, it goes.
    let day_later = fx.now + Duration::from_secs(25 * HOUR);
    let report = prune(&fx.root, Fixture::opts(), day_later, &|_| Ok(u64::MAX / 2)).unwrap();
    assert_eq!(report.idle.units, 1);
    assert!(!debug.join(format!("deps/libidle-{A}.rlib")).exists());
}

/// A binary's objects are named by the rustc invocation that wrote them: the invocation with the
/// newest file is the one it was linked from, and the earlier ones go. A tie keeps both, and a
/// unit no compile touched since the last pass is not looked at again.
#[test]
fn objects_of_an_earlier_compile_go() {
    let fx = Fixture::new("objects");
    let tests = fx.profile("gate/tests/debug");
    fx.ledger(&tests, 2 * HOUR);
    fx.old_unit(&tests, "codec", A, HOUR, HOUR);
    fx.old_unit(&tests, "tie", B, HOUR, HOUR);
    fx.old_unit(&tests, "quiet", C, HOUR, 5 * HOUR);
    let deps = tests.join("deps");
    let object = |name: &str, age: u64| {
        write(&deps.join(name), 64, fx.ago(age), fx.ago(age));
        deps.join(name)
    };
    let old = [
        object(&format!("codec-{A}.cgu1.inv1.rcgu.o"), 5 * HOUR),
        object(&format!("codec-{A}.cgu2.inv1.rcgu.o"), 5 * HOUR),
    ];
    // Reused from the incremental cache: an old time under the new invocation's name.
    let kept = [
        object(&format!("codec-{A}.cgu1.inv2.rcgu.o"), 5 * HOUR),
        object(&format!("codec-{A}.cgu2.inv2.rcgu.o"), HOUR),
    ];
    let tie = [
        object(&format!("tie-{B}.cgu1.x.rcgu.o"), HOUR),
        object(&format!("tie-{B}.cgu1.y.rcgu.o"), HOUR),
    ];
    let quiet = [
        object(&format!("quiet-{C}.cgu1.old.rcgu.o"), 9 * HOUR),
        object(&format!("quiet-{C}.cgu1.new.rcgu.o"), 5 * HOUR),
    ];

    let report = fx.prune(Fixture::opts());

    assert!(old.iter().all(|p| !p.exists()), "the earlier compile's objects go");
    assert!(kept.iter().chain(&tie).chain(&quiet).all(|p| p.exists()));
    assert_eq!(report.idle.leftovers, 2);
    assert_eq!(report.idle.units, 0);
}

/// Cargo 1.100's layout: a unit is `build/<package>/<hash>/`, which goes whole, and a package
/// whose last unit went goes too; the earlier compile's objects inside a kept unit go.
#[test]
fn the_new_layout_prunes_units_and_objects() {
    let fx = Fixture::new("layout");
    let debug = fx.profile("debug");
    fx.ledger(&debug, 2 * HOUR);
    fx.new_unit(&debug, "a", A, 72 * HOUR, 80 * HOUR);
    fx.new_unit(&debug, "a", B, HOUR, HOUR);
    fx.new_unit(&debug, "b", C, 72 * HOUR, 72 * HOUR);
    let out = debug.join(format!("build/a/{B}/out"));
    write(&out.join("a.cgu1.old.rcgu.o"), 64, fx.ago(3 * HOUR), fx.ago(3 * HOUR));
    write(&out.join("a.cgu1.new.rcgu.o"), 64, fx.ago(HOUR), fx.ago(HOUR));
    stamp(&debug.join(format!("build/a/{B}")), fx.ago(HOUR));

    let report = fx.prune(Fixture::opts());

    assert!(!debug.join(format!("build/a/{A}")).exists());
    assert!(debug.join(format!("build/a/{B}")).exists());
    assert!(!debug.join("build/b").exists(), "a package with no unit left goes");
    assert!(!out.join("a.cgu1.old.rcgu.o").exists());
    assert!(out.join("a.cgu1.new.rcgu.o").exists());
    assert_eq!((report.idle.units, report.idle.leftovers), (2, 1));
}

/// A profile directory in neither layout stops the pass instead of pruning nothing in silence.
#[test]
fn a_layout_it_does_not_know_is_an_error() {
    let fx = Fixture::new("unknown");
    let debug = fx.profile("debug");
    std::fs::create_dir_all(debug.join("deps")).unwrap();
    std::fs::write(debug.join(format!("deps/libx-{A}.rlib")), "x").unwrap();
    let error = prune(&fx.root, Fixture::opts(), fx.now, &|_| Ok(u64::MAX / 2)).unwrap_err();
    assert!(format!("{error:#}").contains("layout"), "{error:#}");

    let fx = Fixture::new("unknown-new");
    let debug = fx.profile("debug");
    std::fs::create_dir_all(debug.join("build/pkg/not-a-unit")).unwrap();
    let error = prune(&fx.root, Fixture::opts(), fx.now, &|_| Ok(u64::MAX / 2)).unwrap_err();
    assert!(format!("{error:#}").contains("layout"), "{error:#}");
}

/// Four units and a cache, all inside the idle window, used 20 h, 15 h, 12 h (the cache), 10 h
/// and 10 min ago.
fn lru_fixture(name: &str) -> (Fixture, Utf8PathBuf) {
    let fx = Fixture::new(name);
    let debug = fx.profile("debug");
    fx.ledger(&debug, HOUR);
    fx.old_unit(&debug, "u1", A, 20 * HOUR, 30 * HOUR);
    fx.old_unit(&debug, "u2", B, 15 * HOUR, 30 * HOUR);
    fx.cache(&debug, "cache-0abc", 12 * HOUR);
    fx.old_unit(&debug, "u3", C, 10 * HOUR, 30 * HOUR);
    fx.old_unit(&debug, "u4", D, 600, 30 * HOUR);
    (fx, debug)
}

fn unit_bytes(debug: &Utf8Path, name: &str, hash: &str) -> u64 {
    [
        debug.join(format!(".fingerprint/{name}-{hash}")),
        debug.join(format!("deps/lib{name}-{hash}.rlib")),
        debug.join(format!("deps/{name}-{hash}.d")),
    ]
    .iter()
    .map(|p| size(p))
    .sum()
}

fn alive(debug: &Utf8Path, name: &str, hash: &str) -> bool {
    debug.join(format!("deps/lib{name}-{hash}.rlib")).exists()
}

/// Over the budget, the least recently used go first, to a tenth under it; what was used in the
/// last hour never goes, and what could not be freed is reported short.
#[test]
fn over_the_budget_the_least_recently_used_go_first() {
    let (fx, debug) = lru_fixture("budget");
    let unit = unit_bytes(&debug, "u1", A);
    let total = tally(&fx.root).bytes;
    // To 90 % of the budget is 1.5 units less than there is: u1 and u2 go.
    let budget = (total - unit * 3 / 2) * 10 / 9;
    assert!(budget < total);
    let mut opts = Fixture::opts();
    opts.limits.budget = budget;

    let report = fx.prune(opts);

    assert!(!alive(&debug, "u1", A) && !alive(&debug, "u2", B));
    assert!(debug.join("incremental/cache-0abc").exists());
    assert!(alive(&debug, "u3", C) && alive(&debug, "u4", D));
    assert_eq!((report.budget.units, report.budget.caches), (2, 0));
    assert_eq!(report.short, 0);

    opts.limits.budget = 1;
    let report = fx.prune(opts);
    assert!(!debug.join("incremental/cache-0abc").exists());
    assert!(!alive(&debug, "u3", C));
    assert!(alive(&debug, "u4", D), "used ten minutes ago");
    assert_eq!((report.budget.units, report.budget.caches), (1, 1));
    assert!(report.short > 0);
}

/// Under the free-space floor the same order frees space until the floor holds with a tenth to
/// spare; when nothing old enough is left, the gate's check refuses and names the largest.
#[test]
fn under_the_floor_the_least_recently_used_go_then_the_gate_refuses() {
    let (fx, debug) = lru_fixture("floor");
    let unit = unit_bytes(&debug, "u1", A);
    let before = tally(&fx.root).bytes;
    let floor = unit * 10;
    let limits = Limits { budget: u64::MAX, floor };
    let opts = Options { limits, ..Fixture::opts() };
    // Just under the floor, gaining what the pass deletes.
    let root = fx.root.clone();
    let free = move |_: &Utf8Path| {
        Ok::<_, anyhow::Error>(floor - 1 + before.saturating_sub(tally(&root).bytes))
    };

    let report = prune(&fx.root, opts, fx.now, &free).unwrap();

    assert!(!alive(&debug, "u1", A) && !alive(&debug, "u2", B));
    assert!(debug.join("incremental/cache-0abc").exists());
    assert!(report.free_after >= floor);
    check_room(&fx.root, &report, limits).unwrap();

    let report = prune(&fx.root, opts, fx.now, &|_| Ok(floor / 2)).unwrap();
    assert!(alive(&debug, "u4", D));
    let error = check_room(&fx.root, &report, limits).unwrap_err().to_string();
    assert!(error.contains("under the") && error.contains("floor"), "{error}");
    assert!(error.contains(&format!("{}/debug", fx.root)), "names the largest: {error}");
}

/// A directory a build holds keeps its units, whichever of cargo's two locks it holds, even when
/// the budget reaches them, and says what it held back.
#[test]
fn a_directory_a_build_holds_keeps_its_units() {
    for lock in [".cargo-lock", ".cargo-build-lock"] {
        let fx = Fixture::new("busy");
        let debug = fx.profile("debug");
        fx.ledger(&debug, HOUR);
        fx.old_unit(&debug, "idle", A, 72 * HOUR, 80 * HOUR);
        let ledger = std::fs::read_to_string(debug.join(".xtask-prune")).unwrap();
        let held = File::options().write(true).open(debug.join(lock)).unwrap();
        held.lock_shared().unwrap();

        let report =
            fx.prune(Options { limits: Limits { budget: 1, floor: 0 }, ..Fixture::opts() });

        let dirs: Vec<&str> = report.busy.iter().map(|b| b.dir.as_str()).collect();
        assert_eq!(dirs, ["debug"], "{lock}");
        assert!(alive(&debug, "idle", A), "{lock}");
        assert_eq!(report.idle.units + report.budget.units, 0, "{lock}");
        assert!(report.busy[0].held_back >= unit_bytes(&debug, "idle", A), "{lock}");
        assert_eq!(std::fs::read_to_string(debug.join(".xtask-prune")).unwrap(), ledger, "{lock}");
        assert_ne!(accessed(&debug.join(format!(".fingerprint/idle-{A}/lib-idle"))), UNIX_EPOCH);
        drop(held);
        let report = fx.prune(Fixture::opts());
        assert_eq!(report.idle.units, 1, "{lock}");
        assert!(report.busy.is_empty(), "{lock}");
    }
}

/// A child process holding a session's lock as rustc 1.98 does on macOS: `fcntl(F_SETLK)`, a
/// write lock. Another process, because a process drops its `fcntl` locks on a file when it
/// closes any descriptor of it, which the pass does in this one. Released when dropped.
struct FcntlHolder(std::process::Child);

impl FcntlHolder {
    fn hold(lock: &Utf8Path) -> Self {
        use std::io::BufRead as _;
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "prune::tests::hold_an_fcntl_lock", "--ignored", "--nocapture"])
            .env(HOLD, lock)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = std::io::BufReader::new(child.stdout.take().unwrap());
        let held = stdout.lines().map_while(Result::ok).any(|line| line == "held");
        assert!(held, "the child took the lock");
        Self(child)
    }
}

impl Drop for FcntlHolder {
    fn drop(&mut self) {
        drop(self.0.stdin.take());
        let _status = self.0.wait();
    }
}

const HOLD: &str = "XTASK_PRUNE_TEST_HOLD";

/// Not a test: the child [`FcntlHolder`] runs, which holds the lock until its stdin closes.
#[test]
#[ignore = "the child process of FcntlHolder"]
fn hold_an_fcntl_lock() {
    use std::io::{Read as _, Write as _};
    let Ok(lock) = std::env::var(HOLD) else { return };
    let file = File::options().read(true).write(true).open(lock).unwrap();
    rustix::fs::fcntl_lock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive).unwrap();
    let mut stdout = std::io::stdout();
    writeln!(stdout, "\nheld").unwrap();
    stdout.flush().unwrap();
    let _eof = std::io::stdin().read_to_end(&mut Vec::new());
    drop(file);
}

/// Holds a session's lock as a later rustc reading a finished session does: `flock`, shared.
fn flock_shared(lock: &Utf8Path) -> File {
    let file = File::open(lock).unwrap();
    file.try_lock_shared().unwrap();
    file
}

/// In a directory a build holds, an idle cache still goes, a session at a time under rustc's
/// own lock: a finished session and a `-working` one with their lock files, a lock file whose
/// session is gone and a session whose lock file is. A session a compile holds stays, locked
/// either way rustc locks, and so does a cache used two hours ago and every unit.
#[test]
fn a_busy_directory_sweeps_its_idle_caches_session_by_session() {
    let fx = Fixture::new("busy-caches");
    let debug = fx.profile("debug");
    fx.ledger(&debug, HOUR);
    fx.old_unit(&debug, "idle", A, 72 * HOUR, 80 * HOUR);
    let idle = debug.join("incremental/idle-0abc");
    let finished = fx.cache(&debug, "idle-0abc", 30 * HOUR);
    let working = fx
        .session(
            &debug,
            "idle-0abc",
            "s-hmr8y232qz-0h00y2h-working",
            Some("s-hmr8y232qz-0h00y2h.lock"),
            30 * HOUR,
        )
        .unwrap();
    fx.session(&debug, "idle-0abc", "s-hmr8a0000a-1aaaaaa-svh", None, 30 * HOUR);
    write(&idle.join("s-hmr8b0000b-0bbbbbb.lock"), 0, fx.ago(30 * HOUR), fx.ago(30 * HOUR));
    stamp(&idle, fx.ago(30 * HOUR));
    let read = fx.cache(&debug, "read-0def", 30 * HOUR);
    let wrote = fx.cache(&debug, "wrote-0aaa", 30 * HOUR);
    fx.cache(&debug, "used-0bbb", 2 * HOUR);
    let build = File::options().write(true).open(debug.join(".cargo-lock")).unwrap();
    build.lock_shared().unwrap();
    let reader = flock_shared(&read);
    let writer = FcntlHolder::hold(&wrote);

    let dry = fx.prune(Options { dry_run: true, ..Fixture::opts() });
    assert_eq!((dry.busy[0].swept.caches, dry.busy[0].sessions_held), (1, 2));
    assert!(finished.exists() && working.exists(), "a dry run deletes nothing");

    let report = fx.prune(Fixture::opts());

    let left: Vec<String> =
        idle.read_dir_utf8().unwrap().map(|e| e.unwrap().file_name().to_owned()).collect();
    assert!(left.is_empty(), "every session and lock file of the idle cache went: {left:?}");
    for kept in ["read-0def", "wrote-0aaa"] {
        let cache = debug.join("incremental").join(kept);
        assert!(cache.join(SESSION).exists(), "{kept}");
        assert!(cache.join(SESSION_LOCK).exists(), "{kept}");
    }
    assert!(debug.join("incremental/used-0bbb").exists());
    assert!(alive(&debug, "idle", A), "a unit stays under cargo's lock");
    let [busy] = report.busy.as_slice() else { panic!("{:?}", report.busy) };
    assert_eq!(busy.dir, "debug");
    assert_eq!((busy.swept.caches, busy.sessions_held), (1, 2));
    assert!(busy.swept.bytes >= 3 * MIB as u64, "{busy:?}");
    assert!(busy.held_back >= 2 * MIB as u64, "{busy:?}");
    assert_eq!((report.idle.caches, report.idle.units), (1, 0));
    assert_eq!(report.idle.bytes, busy.swept.bytes);

    drop((reader, writer, build));
    let report = fx.prune(Fixture::opts());
    assert_eq!(report.busy, []);
    // The idle cache emptied above was written to just now, so it is not idle yet.
    assert_eq!((report.idle.caches, report.idle.units), (2, 1), "{:?}", report.idle);
    assert!(!debug.join("incremental/read-0def").exists());
}

/// Under the floor, a busy directory's caches go for the budget session by session, its units
/// stay, and the gate's refusal names it as what a build held.
#[test]
fn under_the_floor_a_busy_directory_gives_its_caches_and_is_named() {
    let (fx, debug) = lru_fixture("busy-floor");
    let limits = Limits { budget: u64::MAX, floor: u64::MAX / 4 };
    let opts = Options { limits, ..Fixture::opts() };
    let build = File::options().write(true).open(debug.join(".cargo-build-lock")).unwrap();
    build.lock_shared().unwrap();

    let report = prune(&fx.root, opts, fx.now, &|_| Ok(0)).unwrap();

    assert!(debug.join("incremental/cache-0abc").exists(), "its directory stays");
    assert!(!debug.join("incremental/cache-0abc").join(SESSION_LOCK).exists());
    assert_eq!((report.budget.caches, report.budget.units), (1, 0));
    assert!(["u1", "u2", "u3", "u4"].iter().zip([A, B, C, D]).all(|(n, h)| alive(&debug, n, h)));
    let error = check_room(&fx.root, &report, limits).unwrap_err().to_string();
    assert!(error.contains("held by a build") && error.contains("debug ("), "{error}");
}

/// What rustc does not write in `incremental/` (a file, a crate cache it may not read, a session
/// name without three dashes, a lock file it may not open) stays and the pass goes on, busy or
/// not.
#[test]
fn odd_entries_in_incremental_leave_the_pass_going() {
    use std::os::unix::fs::PermissionsExt as _;
    let fx = Fixture::new("odd");
    let debug = fx.profile("debug");
    fx.ledger(&debug, HOUR);
    let incremental = debug.join("incremental");
    fx.cache(&debug, "plain-0aaa", 30 * HOUR);
    fx.cache(&debug, "sealed-0bbb", 30 * HOUR);
    let sealed = incremental.join("sealed-0bbb");
    let unopenable = fx.cache(&debug, "unopenable-0ccc", 30 * HOUR);
    let odd = fx.session(&debug, "odd-0ddd", "s-odd", None, 30 * HOUR);
    assert!(odd.is_none());
    write(&incremental.join("stray"), 64, fx.ago(30 * HOUR), fx.ago(30 * HOUR));
    let mode = |path: &Utf8Path, mode: u32| {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    };
    mode(&unopenable, 0o000);
    mode(&sealed, 0o000);
    let build = File::options().write(true).open(debug.join(".cargo-lock")).unwrap();
    build.lock_shared().unwrap();

    let report = fx.prune(Fixture::opts());

    assert!(!incremental.join("plain-0aaa").join(SESSION_LOCK).exists());
    assert!(incremental.join("odd-0ddd/s-odd").exists());
    assert!(incremental.join("stray").exists() && sealed.exists());
    assert!(unopenable.parent().unwrap().join(SESSION).exists());
    assert_eq!((report.busy[0].swept.caches, report.busy[0].sessions_held), (1, 1));

    drop(build);
    let report = fx.prune(Fixture::opts());
    assert_eq!(report.busy, []);
    assert!(incremental.join("stray").exists(), "not a cache");
    assert!(!incremental.join("odd-0ddd").exists(), "under cargo's lock a cache goes whole");
    assert!(!unopenable.exists(), "under cargo's lock no session lock is taken");
    mode(&sealed, 0o755);
}

/// A dry run reports and changes nothing: no file, no access time, no ledger.
#[test]
fn a_dry_run_changes_nothing() {
    let fx = Fixture::new("dry");
    let debug = fx.profile("debug");
    fx.ledger(&debug, HOUR);
    fx.old_unit(&debug, "idle", A, 72 * HOUR, 80 * HOUR);
    fx.old_unit(&debug, "used", B, HOUR, 80 * HOUR);
    let ledger = std::fs::read_to_string(debug.join(".xtask-prune")).unwrap();

    let report = fx.prune(Options { dry_run: true, ..Fixture::opts() });

    assert_eq!(report.idle.units, 1);
    assert!(alive(&debug, "idle", A));
    assert_ne!(accessed(&debug.join(format!(".fingerprint/used-{B}/lib-used"))), UNIX_EPOCH);
    assert_eq!(std::fs::read_to_string(debug.join(".xtask-prune")).unwrap(), ledger);
}

/// The premise the ledger rests on, on this Mac's APFS: a file whose access time was set back to
/// the epoch gets it stamped by the next read.
#[test]
fn a_read_stamps_an_access_time_set_back_to_the_epoch() {
    use std::io::Read as _;
    let fx = Fixture::new("atime");
    let file = fx.root.join("fingerprint");
    write(&file, 64, fx.ago(HOUR), fx.ago(HOUR));
    File::open(&file).unwrap().set_times(FileTimes::new().set_accessed(UNIX_EPOCH)).unwrap();
    assert_eq!(accessed(&file), UNIX_EPOCH);
    let mut text = String::new();
    File::open(&file).unwrap().read_to_string(&mut text).unwrap();
    assert!(accessed(&file) > fx.ago(60), "the read stamped it");
}

/// A limit's variable: unset or empty takes the default, a number its GB, anything else fails.
#[test]
fn a_limit_reads_its_variable_and_an_empty_one_is_unset() {
    let read = |value: Option<&str>| {
        let owned = value.map(str::to_owned);
        gigabytes("SLOPTY_DISK_FLOOR_GB", move |_| owned.clone(), 50)
    };
    assert_eq!(read(None).ok(), Some(50 * GB));
    assert_eq!(read(Some("")).ok(), Some(50 * GB), "CI's matrix leaves it empty");
    assert_eq!(read(Some(" 5 ")).ok(), Some(5 * GB));
    let error = read(Some("five")).map_err(|e| e.to_string()).err().unwrap_or_default();
    assert!(error.contains("is not a number"), "{error}");
}
