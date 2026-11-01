//! `xtask gitlab` against a scripted GitLab: the order a jmango360 job, our job and the guard's
//! polls happen in is forced moment by moment, never slept for; the runner's answers are recorded.

#![expect(clippy::arithmetic_side_effects, reason = "a scripted clock of a few polls")]

use super::*;

const MAC: Mac = MACS[0];

/// Our job's id in the scripted runs, and a jmango360 job's.
const OURS: u64 = 1;
const JMANGO: u64 = 900;

/// A runner that takes jobs, running `running` and with `pending` waiting.
fn runner(running: &[u64], pending: &[u64]) -> Runner {
    Runner {
        takes: true,
        running: running.to_vec(),
        running_count: running.len() as u64,
        pending: pending.to_vec(),
        pending_count: pending.len() as u64,
    }
}

/// One moment of a scripted run: what GitLab says until the next pause. No runner is GitLab
/// failing to answer.
#[derive(Clone)]
struct Moment {
    runner: Option<Runner>,
    status: &'static str,
    trace: &'static str,
    reason: Option<&'static str>,
}

fn moment(runner: Runner, status: &'static str, trace: &'static str) -> Moment {
    Moment { runner: Some(runner), status, trace, reason: None }
}

/// GitLab and the clock as a test scripts them: every answer comes from the current moment, one
/// for each [`POLL`] of the clock (staying on the last); what was asked is logged with the moment.
struct Fake {
    moments: Vec<Moment>,
    at: usize,
    clock: Duration,
    /// The project's jobs that wait or run.
    active: Vec<u64>,
    calls: Vec<String>,
}

impl Fake {
    fn new(moments: Vec<Moment>) -> Self {
        Self { moments, at: 0, clock: Duration::ZERO, active: vec![OURS], calls: Vec::new() }
    }

    fn current(&self) -> &Moment {
        &self.moments[self.at.min(self.moments.len() - 1)]
    }

    fn job_now(&self) -> Job {
        Job {
            id: OURS,
            name: JOB.to_owned(),
            status: self.current().status.to_owned(),
            web_url: "https://gitlab.jmango360.com/cong.tran/slopty-ci/-/jobs/1".to_owned(),
            failure_reason: self.current().reason.map(str::to_owned),
        }
    }

    /// The calls of `kind`, each with the moment it came at.
    fn calls(&self, kind: &str) -> Vec<&str> {
        self.calls.iter().filter(|c| c.starts_with(kind)).map(String::as_str).collect()
    }
}

impl Gitlab for Fake {
    fn runner(&mut self, mac: Mac) -> Result<Runner> {
        self.calls.push(format!("runner {} @{}", mac.runner, self.at));
        self.current().runner.clone().context("GitLab did not answer")
    }

    fn active(&mut self) -> Result<Vec<u64>> {
        self.calls.push(format!("active @{}", self.at));
        Ok(self.active.clone())
    }

    fn trigger(&mut self, vars: &[(&str, &str)]) -> Result<u64> {
        let vars: Vec<String> = vars.iter().map(|(k, v)| format!("{k}={v}")).collect();
        self.calls.push(format!("trigger {} @{}", vars.join(","), self.at));
        Ok(7)
    }

    fn jobs(&mut self, pipeline: u64) -> Result<Vec<Job>> {
        assert_eq!(pipeline, 7, "the triggered pipeline's jobs");
        Ok(vec![self.job_now()])
    }

    fn job(&mut self, id: u64) -> Result<Job> {
        assert_eq!(id, OURS, "only our job is asked about");
        Ok(self.job_now())
    }

    fn trace(&mut self, _id: u64) -> Result<String> {
        Ok(self.current().trace.to_owned())
    }

    fn cancel(&mut self, id: u64) -> Result<()> {
        self.calls.push(format!("cancel {id} @{}", self.at));
        Ok(())
    }

    fn now(&self) -> Duration {
        self.clock
    }

    fn pause(&mut self, span: Duration) -> Result<()> {
        self.clock += span;
        self.at = usize::try_from(self.clock.as_nanos() / POLL.as_nanos())?;
        Ok(())
    }
}

const COMMANDS: [&str; 2] = ["gate --ci --lane tests", "smoke run"];

fn plan(commands: &[String], prepare: Option<Duration>) -> Plan<'_> {
    Plan { sha: Some("abc123"), commands, macs: &MACS, vars: &[], prepare }
}

fn commands() -> Vec<String> {
    COMMANDS.map(str::to_owned).to_vec()
}

const BOOTED: &str = "2026-10-31T11:41:26.465656Z 00E Cloning a new VM...\n\
                      2026-10-31T11:41:37.000001Z 00E VM is ready.\n";
const TESTING: &str = "2026-10-31T11:41:26.465656Z 00E Cloning a new VM...\n\
                       2026-10-31T11:41:37.000001Z 00E VM is ready.\n\
                       2026-10-31T11:41:40.000001Z 00O ▶ xtask gate --ci --lane tests\n";

/// The proof of jmango360 first: our job is dispatched to the idle Mac, waits, starts, runs, and
/// the moment a job of another project waits for the Mac (moment 8) it is cancelled, in that same
/// moment, with no pause between: that job waits at most one poll for the Mac to be given back.
#[test]
fn a_job_waiting_for_the_mac_cancels_ours_in_the_moment_it_appears() {
    let mut fake = Fake::new(vec![
        moment(runner(&[], &[]), "pending", ""),
        moment(runner(&[], &[OURS]), "pending", ""),
        moment(runner(&[OURS], &[]), "running", BOOTED),
        moment(runner(&[OURS], &[]), "running", TESTING),
        moment(runner(&[OURS], &[]), "running", TESTING),
        moment(runner(&[OURS], &[]), "running", TESTING),
        moment(runner(&[OURS], &[]), "running", TESTING),
        moment(runner(&[OURS], &[]), "running", TESTING),
        moment(runner(&[OURS], &[JMANGO]), "running", TESTING),
        moment(runner(&[OURS], &[JMANGO]), "running", TESTING),
    ]);
    let commands = commands();
    let mut out = Vec::new();
    let outcome = dispatch(&mut fake, &plan(&commands, Some(PREPARE_LIMIT)), &mut out).unwrap();

    assert_eq!(outcome.mac, Some(MAC), "the first idle Mac");
    assert!(
        matches!(&outcome.verdict, Verdict::Yielded(why) if why.contains("another project")),
        "{:?}",
        outcome.verdict
    );
    assert_eq!(
        fake.calls("cancel"),
        ["cancel 1 @8"],
        "cancelled once, in the moment it was wanted"
    );
    assert_eq!(fake.clock, POLL * 8, "no pause after the wanted moment");
    assert_eq!(
        fake.calls("trigger"),
        ["trigger SLOPTY_TAG=office-mac-runner-02,SLOPTY_SHA=abc123,\
             SLOPTY_ARGS=gate --ci --lane tests\nsmoke run @0"],
        "the trigger's variables"
    );
    let log = String::from_utf8(out).unwrap();
    assert!(
        log.contains("\nVM is ready.\n▶ xtask gate --ci --lane tests\n"),
        "the job's log, unstamped: {log}"
    );
    assert_eq!(log.matches("VM is ready.").count(), 1, "each line shown once: {log}");
}

/// Our own job moving from waiting to running (seen in both lists, or in neither) and another job
/// of our own project never count as wanting the Mac.
#[test]
fn our_own_jobs_never_want_the_mac() {
    let mut fake = Fake::new(vec![
        moment(runner(&[], &[]), "pending", ""),
        moment(runner(&[OURS], &[OURS]), "pending", ""),
        moment(runner(&[], &[]), "running", BOOTED),
        moment(runner(&[OURS], &[2]), "running", TESTING),
        moment(runner(&[OURS], &[]), "success", TESTING),
    ]);
    fake.active = vec![OURS, 2];
    let commands = commands();
    let outcome =
        dispatch(&mut fake, &plan(&commands, Some(PREPARE_LIMIT)), &mut Vec::new()).unwrap();
    assert_eq!(outcome.verdict, Verdict::Passed, "ran to its end");
    assert!(fake.calls("cancel").is_empty(), "never cancelled: {:?}", fake.calls);
    assert_eq!(
        fake.calls("active"),
        ["active @3"],
        "the project's jobs read only when an unknown job shows"
    );
}

/// A Mac that is busy, or that starts another project's job before ours, is never waited for.
#[test]
fn a_busy_mac_is_never_waited_for() {
    let mut fake = Fake::new(vec![moment(runner(&[JMANGO], &[]), "pending", "")]);
    let commands = commands();
    let outcome =
        dispatch(&mut fake, &plan(&commands, Some(PREPARE_LIMIT)), &mut Vec::new()).unwrap();
    assert_eq!(outcome.verdict, Verdict::Yielded("no office Mac is idle".to_owned()), "none idle");
    assert!(fake.calls("trigger").is_empty(), "nothing dispatched");
    assert_eq!(fake.calls("runner").len(), 2, "both Macs asked");

    let mut fake = Fake::new(vec![
        moment(runner(&[], &[]), "pending", ""),
        moment(runner(&[JMANGO], &[OURS]), "pending", ""),
    ]);
    let outcome =
        dispatch(&mut fake, &plan(&commands, Some(PREPARE_LIMIT)), &mut Vec::new()).unwrap();
    assert!(matches!(outcome.verdict, Verdict::Yielded(_)), "{:?}", outcome.verdict);
    assert_eq!(fake.calls("cancel"), ["cancel 1 @1"], "ours gives way while still waiting");
}

/// A job the Mac leaves waiting, or whose guest does not come up, is cancelled at its limit;
/// `warm`, which pulls the image, has no limit on the guest.
#[test]
fn a_job_is_cancelled_at_its_limits() {
    let commands = commands();
    let mut fake = Fake::new(vec![
        moment(runner(&[], &[]), "pending", ""),
        moment(runner(&[], &[OURS]), "pending", ""),
    ]);
    let outcome =
        dispatch(&mut fake, &plan(&commands, Some(PREPARE_LIMIT)), &mut Vec::new()).unwrap();
    assert!(
        matches!(&outcome.verdict, Verdict::Yielded(why) if why.contains("did not take it")),
        "{:?}",
        outcome.verdict
    );
    let polls = PENDING_LIMIT.as_secs() / POLL.as_secs() + 1;
    assert_eq!(
        fake.calls("cancel"),
        [format!("cancel 1 @{polls}").as_str()],
        "cancelled at the first poll past the limit"
    );

    let pulling = "2026-10-31T11:41:26.465656Z 00E Cloning a new VM...\n";
    let mut fake = Fake::new(vec![
        moment(runner(&[], &[]), "running", pulling),
        moment(runner(&[OURS], &[]), "running", pulling),
    ]);
    let outcome =
        dispatch(&mut fake, &plan(&commands, Some(PREPARE_LIMIT)), &mut Vec::new()).unwrap();
    assert!(
        matches!(&outcome.verdict, Verdict::Yielded(why) if why.contains("gitlab warm")),
        "{:?}",
        outcome.verdict
    );
    assert!(
        fake.clock > PREPARE_LIMIT && fake.clock <= PREPARE_LIMIT + POLL,
        "at the limit: {:?}",
        fake.clock
    );

    let mut moments = vec![moment(runner(&[], &[]), "running", pulling); 100];
    moments.push(moment(runner(&[OURS], &[]), "success", BOOTED));
    let mut fake = Fake::new(moments);
    let warm = Plan { sha: None, commands: &[], macs: &MACS, vars: &[], prepare: None };
    let outcome = dispatch(&mut fake, &warm, &mut Vec::new()).unwrap();
    assert_eq!(outcome.verdict, Verdict::Passed, "a pull takes as long as it takes");
    assert!(fake.calls("trigger")[0].contains("SLOPTY_SHA=,"), "no commit: {:?}", fake.calls);
}

/// A job that ended reads as GitLab and its log say.
#[test]
fn a_verdict_reads_the_status_the_reason_and_the_mark() {
    let job = |status: &str, reason: Option<&str>| Job {
        id: OURS,
        name: JOB.to_owned(),
        status: status.to_owned(),
        web_url: String::new(),
        failure_reason: reason.map(str::to_owned),
    };
    assert_eq!(verdict(&job("success", None), ""), Some(Verdict::Passed), "passed");
    assert_eq!(
        verdict(&job("failed", Some("script_failure")), "error[E0308]\n"),
        Some(Verdict::Failed),
        "failed"
    );
    let echoed = "2026-10-31T11:41:40.000001Z 00O $ yield() { echo \"slopty-gitlab: yielded: \
                  $1\"; exit 75; }\n";
    assert_eq!(
        verdict(&job("failed", Some("script_failure")), echoed),
        Some(Verdict::Failed),
        "the script's own definition of a yield, echoed, is none"
    );
    assert_eq!(
        verdict(
            &job("failed", Some("script_failure")),
            "x\nslopty-gitlab: yielded: could not fetch abc\n"
        ),
        Some(Verdict::Yielded("could not fetch abc".to_owned())),
        "the job's own yield"
    );
    assert_eq!(
        verdict(&job("failed", Some("runner_system_failure")), ""),
        Some(Verdict::Yielded("GitLab ended it: runner_system_failure".to_owned())),
        "the Mac's failure is no verdict on the commit"
    );
    assert!(matches!(verdict(&job("canceled", None), ""), Some(Verdict::Yielded(_))), "cancelled");
    assert_eq!(verdict(&job("running", None), ""), None, "still running");
}

/// Work that a test drives: it ends at poll `ends_at` with status 3, unless stopped first.
struct FakeWork {
    polls: usize,
    ends_at: Option<usize>,
    stopped_at: Option<usize>,
}

impl Work for FakeWork {
    fn poll(&mut self) -> Result<Option<i32>> {
        self.polls += 1;
        Ok((Some(self.polls) == self.ends_at).then_some(3))
    }

    fn stop(&mut self) {
        self.stopped_at = Some(self.polls);
    }
}

/// The guard in the guest stops the work and yields the moment another project's job wants the
/// Mac, never because GitLab did not answer, and lets work that ends first end, seen within a
/// tick rather than a poll.
#[test]
fn the_guard_yields_in_the_moment_the_mac_is_wanted() {
    let ticks = usize::try_from(POLL.as_nanos() / TICK.as_nanos()).unwrap();
    let mut fake = Fake::new(vec![
        moment(runner(&[OURS], &[]), "running", ""),
        Moment { runner: None, status: "running", trace: "", reason: None },
        moment(runner(&[OURS], &[]), "running", ""),
        moment(runner(&[OURS], &[JMANGO]), "running", ""),
    ]);
    let mut work = FakeWork { polls: 0, ends_at: None, stopped_at: None };
    let mut out = Vec::new();
    assert_eq!(guard(&mut fake, MAC, OURS, &mut work, &mut out).unwrap(), None, "yielded");
    assert_eq!(
        fake.calls("runner"),
        ["runner 842 @0", "runner 842 @1", "runner 842 @2", "runner 842 @3"],
        "asked once a poll"
    );
    assert_eq!(fake.clock, POLL * 3, "no pause after the wanted moment's question");
    assert_eq!(work.stopped_at, Some(3 * ticks + 1), "stopped at once, the work seen every tick");
    let log = String::from_utf8(out).unwrap();
    assert!(log.contains("could not read office-mac-runner-02's queue"), "{log}");
    assert!(log.contains("slopty-gitlab: yielded: 1 job(s) of another project want"), "{log}");

    let mut fake = Fake::new(vec![moment(runner(&[OURS], &[]), "running", "")]);
    let mut work = FakeWork { polls: 0, ends_at: Some(3), stopped_at: None };
    assert_eq!(
        guard(&mut fake, MAC, OURS, &mut work, &mut Vec::new()).unwrap(),
        Some(3),
        "ended, with its status"
    );
    assert_eq!(fake.clock, TICK * 2, "seen at the third look, two ticks on");
    assert_eq!(fake.calls("runner").len(), 1, "GitLab asked once, at the start");
    assert_eq!(work.stopped_at, None, "never stopped");
}

/// The steps run in turn, a failed one no reason to skip the next.
#[test]
fn steps_run_in_turn() {
    assert_eq!(
        command_lines("gate --ci --lane tests\n\n  smoke   run \n"),
        [vec!["gate", "--ci", "--lane", "tests"], vec!["smoke", "run"]],
        "one command per line, split on whitespace"
    );
    let failed =
        run_steps(Utf8Path::new("/bin/sh"), &command_lines("-c false\n-c true\n-c exit\n"))
            .unwrap();
    assert_eq!(failed, ["-c false"], "the first failed, and only it");
}

/// Stopping a guarded command ends it and every process under it at once, those in process
/// groups of their own too.
#[test]
fn a_guarded_command_ends_with_all_it_started() {
    assert_eq!(
        descendants("  1     0\n 10     1\n 11    10\n 12    11\n 13     1\n 14    12\n x y\n", 10),
        [10, 11, 12, 14],
        "the tree under 10, by parent"
    );
    assert_eq!(descendants("", 7), [7], "the root alone when ps says nothing");

    // A shell with a child in a process group of its own, as the e2e harness starts its apps.
    let child = Command::new("/bin/sh")
        .args(["-c", "set -m; /bin/sleep 30 & /bin/sleep 31"])
        .spawn()
        .unwrap();
    let root = child.id();
    let mut work = Supervised { child };
    let started = Instant::now();
    let tree = loop {
        let tree = process_tree(root);
        if tree.len() >= 3 || started.elapsed() > Duration::from_secs(5) {
            break tree;
        }
    };
    assert_eq!(tree.len(), 3, "the shell and its two sleeps: {tree:?}");
    assert_eq!(work.poll().unwrap(), None, "running");
    let stopping = Instant::now();
    work.stop();
    assert!(
        stopping.elapsed() < Duration::from_secs(2),
        "SIGTERM ended it: {:?}",
        stopping.elapsed()
    );
    assert_eq!(work.poll().unwrap(), Some(143), "ended by SIGTERM, as a shell says it");
    // The orphaned sleeps are launchd's to reap: their pids go once it has.
    let reaped = Instant::now();
    let left = loop {
        let left: Vec<u32> = process_tree(1).into_iter().filter(|pid| tree.contains(pid)).collect();
        if left.is_empty() || reaped.elapsed() > Duration::from_secs(5) {
            break left;
        }
    };
    assert!(left.is_empty(), "{left:?} of {tree:?} still there");
}

/// Recorded on 2026-10-31 with the read token, while our probe job 356670 waited for runner 842
/// behind our job 356667: the ids show, the projects do not.
const RECORDED_842: &str = r#"{"data":{"p":{"count":1,"nodes":[{"id":"gid://gitlab/Ci::Build/356670","status":"PENDING","project":null}]},"r":{"status":"ONLINE","jobExecutionStatus":"ACTIVE","jobs":{"count":1,"nodes":[{"id":"gid://gitlab/Ci::Build/356667","project":null}]}}}}"#;

/// Recorded the same day: GitLab answers the running jobs of one runner per request.
const RECORDED_ERROR: &str = r#"{"errors":[{"message":"\"jobs\" field can be requested only for 1 CiRunner(s) at a time.","locations":[{"line":1,"column":286}],"path":["r842","jobs"]}],"data":{"r834":{"status":"ONLINE","jobExecutionStatus":"ACTIVE","jobs":{"count":1}}}}"#;

#[test]
fn a_runner_reads_from_gitlab_s_answer() {
    let busy = parse_runner(RECORDED_842).unwrap();
    assert_eq!(busy, runner(&[356_667], &[356_670]), "ids and counts");
    assert!(!busy.idle(), "busy");
    assert_eq!(busy.others(356_670, &[]), 1, "the running job is unknown");
    assert_eq!(busy.others(356_670, &[356_667]), 0, "and the project's own");

    let idle = r#"{"data":{"p":{"count":0,"nodes":[]},"r":{"status":"ONLINE","paused":false,"jobs":{"count":0,"nodes":[]}}}}"#;
    assert!(parse_runner(idle).unwrap().idle(), "idle");
    let paused = idle.replace("\"paused\":false", "\"paused\":true");
    assert!(!parse_runner(&paused).unwrap().idle(), "a paused runner takes nothing");

    let error = parse_runner(RECORDED_ERROR).unwrap_err().to_string();
    assert!(error.contains("only for 1 CiRunner"), "{error}");
    assert!(
        runner_query(842).contains("compatibleRunnerId: \"gid://gitlab/Ci::Runner/842\""),
        "the question"
    );
}

#[test]
fn a_log_shows_whole_lines_once_without_stamps() {
    let mut out = Vec::new();
    let trace =
        "2026-10-31T11:41:26.412686Z 00O Cloning a new VM...\n2026-10-31T11:41:27.1Z 00O+par";
    let shown = show(trace, 0, &mut out).unwrap();
    assert_eq!(String::from_utf8(out).unwrap(), "Cloning a new VM...\n", "the whole line");
    let mut out = Vec::new();
    let trace = format!("{trace}tial\nplain line\n");
    let shown = show(&trace, shown, &mut out).unwrap();
    assert_eq!(
        String::from_utf8(out).unwrap(),
        "2026-10-31T11:41:27.1Z 00O+partial\nplain line\n",
        "the rest, once"
    );
    assert_eq!(shown, trace.len(), "all shown");
    assert_eq!(
        untimed("2026-10-31T11:41:26.412686Z 00E+\u{1b}[0Kx"),
        "\u{1b}[0Kx",
        "a continuation"
    );
}

#[test]
fn curl_reads_a_token_and_a_body_quoted() {
    assert_eq!(
        config("header", "PRIVATE-TOKEN: a\"b\\c"),
        r#"header = "PRIVATE-TOKEN: a\"b\\c""#,
        "quoted"
    );
    assert_eq!(
        config("data-binary", "{\"q\":\"x\\\"y\"}\n"),
        r#"data-binary = "{\"q\":\"x\\\"y\"}\n""#,
        "a JSON body"
    );
}

/// gitlab/ci.yml reads every variable `run` sends, yields as this module reads a yield, and runs
/// `xtask gitlab job`.
#[test]
fn the_pipeline_matches_the_dispatcher() {
    for name in [VAR_TAG, VAR_SHA, VAR_ARGS, "SLOPTY_IMAGE", READ_TOKEN, YIELD_MARK] {
        assert!(CI.contains(name), "gitlab/ci.yml never names {name}");
    }
    assert!(CI.contains(&format!("exit_codes: [{YIELDED}]")), "a yield is allowed");
    assert!(CI.contains(&format!("exit {YIELDED};")), "the job's own yield");
    assert!(CI.contains(&format!("\n{JOB}:\n")), "the job is `{JOB}`");
    assert!(
        CI.contains("interruptible: true") && CI.contains("timeout: 1h"),
        "bounded and interruptible"
    );
    assert!(
        CI.contains(&format!(
            "- {GUARD} gitlab guard -- /bin/bash -eo pipefail -c \"$SLOPTY_STEPS\"\n"
        )),
        "the image's guard runs the steps"
    );
    assert!(CI.contains("expand: false"), "GitLab leaves the steps' shell variables to the shell");
    assert!(CI.contains("gitlab job --reports"), "the steps run the commands");
}

#[derive(clap::Parser)]
struct Cli {
    #[command(subcommand)]
    cmd: GitlabCmd,
}

#[test]
fn the_command_line_parses() {
    use clap::{CommandFactory as _, Parser as _};
    Cli::command().debug_assert();
    let cli = Cli::try_parse_from([
        "gitlab",
        "run",
        "--sha",
        "abc",
        "--job",
        "gate --ci",
        "--job",
        "smoke run",
        "--var",
        "SLOPTY_IMAGE=zot.jmango360.dev/slopty/macos-27-ci:latest",
        "--github-output",
    ])
    .unwrap();
    let GitlabCmd::Run(opts) = cli.cmd else { panic!("run") };
    assert_eq!(opts.jobs, ["gate --ci", "smoke run"], "two commands");
    assert_eq!(
        opts.vars,
        [("SLOPTY_IMAGE".to_owned(), "zot.jmango360.dev/slopty/macos-27-ci:latest".to_owned())],
        "a variable, split at its first ="
    );
    assert!(parse_var("=x").is_err() && parse_var("x").is_err(), "a variable needs a name and =");
    assert_eq!(candidates(Some("834")).unwrap(), [MACS[1]], "by id");
    assert_eq!(candidates(Some("office-mac-runner-02")).unwrap(), [MACS[0]], "by tag");
    assert!(candidates(Some("office-mac-runner-03")).is_err(), "no such Mac");
    assert_eq!(tags(&MACS), "office-mac-runner-02 office-mac-runner", "in order");
    let cli = Cli::try_parse_from(["gitlab", "guard", "--", "/bin/bash", "-c", "a --b"]).unwrap();
    let GitlabCmd::Guard { command } = cli.cmd else { panic!("guard") };
    assert_eq!(command, ["/bin/bash", "-c", "a --b"], "the command whole, after --");
}

#[test]
fn reports_are_copied_and_builds_skipped() {
    let root = Utf8PathBuf::try_from(std::env::temp_dir())
        .unwrap()
        .join(format!("slopty-gitlab-reports-{}", std::process::id()));
    let target = root.join("target");
    let make = |path: &str| {
        let path = target.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "x").unwrap();
    };
    make("nextest/ci/junit.xml");
    make("nextest/ci/ptys.log");
    make("e2e/case/run.log");
    make("debug/deps/junit.xml");
    make("debug/cargo-timings/cargo-timing.html");
    collect_reports(&target, &root.join("reports")).unwrap();
    let copied = |path: &str| root.join("reports").join(path).exists();
    assert!(copied("nextest/ci/junit.xml") && copied("nextest/ci/ptys.log"), "nextest's");
    assert!(copied("e2e/case/run.log"), "the e2e's");
    assert!(copied("debug/cargo-timings/cargo-timing.html"), "cargo's timings");
    assert!(!copied("debug/deps/junit.xml"), "never under a build's directories");
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn the_fake_moves_one_moment_per_poll() {
    let mut fake = Fake::new(vec![
        moment(runner(&[], &[]), "pending", ""),
        moment(runner(&[OURS], &[]), "running", ""),
        moment(runner(&[OURS], &[]), "success", ""),
    ]);
    assert_eq!(fake.job(OURS).unwrap().status, "pending", "first");
    fake.pause(POLL.checked_sub(TICK).unwrap()).unwrap();
    assert_eq!(fake.job(OURS).unwrap().status, "pending", "not yet a poll on");
    fake.pause(TICK).unwrap();
    assert_eq!(fake.job(OURS).unwrap().status, "running", "a poll on");
    fake.pause(POLL * 5).unwrap();
    assert_eq!(fake.job(OURS).unwrap().status, "success", "then the last, held");
}
