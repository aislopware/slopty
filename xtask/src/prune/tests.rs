//! `xtask prune` on fixture target directories: units, caches and objects laid out as cargo 1.98
//! and cargo 1.101 nightly (the 1.100 layout) write them, with the access and modification times
//! of the use each test stages. No test runs cargo.

#![expect(clippy::arithmetic_side_effects, reason = "fixture times and sizes of a few MB")]

use std::fs::{File, FileTimes};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use camino::{Utf8Path, Utf8PathBuf};

use super::{Limits, Options, Report, check_room, prune, size, tally};

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

    /// An incremental cache whose last session was compiled `compiled` seconds ago.
    fn cache(&self, profile: &Utf8Path, name: &str, compiled: u64) {
        let cache = profile.join("incremental").join(name);
        let session = cache.join("s-session");
        std::fs::create_dir_all(&session).unwrap();
        write(&session.join("dep-graph.bin"), MIB, self.ago(compiled), self.ago(compiled));
        stamp(&session, self.ago(compiled));
        stamp(&cache, self.ago(compiled));
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
    assert!(report.busy.is_empty());

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

/// A directory a build holds is left alone, whichever of cargo's two locks it holds.
#[test]
fn a_directory_a_build_holds_is_skipped() {
    for lock in [".cargo-lock", ".cargo-build-lock"] {
        let fx = Fixture::new("busy");
        let debug = fx.profile("debug");
        fx.ledger(&debug, HOUR);
        fx.old_unit(&debug, "idle", A, 72 * HOUR, 80 * HOUR);
        let held = File::options().write(true).open(debug.join(lock)).unwrap();
        held.lock_shared().unwrap();

        let report =
            fx.prune(Options { limits: Limits { budget: 1, floor: 0 }, ..Fixture::opts() });

        assert_eq!(report.busy, ["debug"], "{lock}");
        assert!(alive(&debug, "idle", A), "{lock}");
        drop(held);
        let report = fx.prune(Fixture::opts());
        assert_eq!(report.idle.units, 1, "{lock}");
    }
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
