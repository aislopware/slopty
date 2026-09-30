use slopty_proto::project::Preference;

use super::*;

fn text(t: &str) -> Fact {
    Fact::Text(t.to_owned())
}

fn worker(name: &str, os: &str, cpus: i64, load: f64) -> Candidate {
    let mut facts = Facts::new();
    facts.insert("name".to_owned(), text(name));
    facts.insert("os".to_owned(), text(os));
    facts.insert("cpus".to_owned(), Fact::Int(cpus));
    facts.insert("load".to_owned(), Fact::Float(load));
    Candidate {
        worker: WorkerId::new(),
        name: name.to_owned(),
        online: true,
        facts,
        live: 0,
        fleet_live: 0,
    }
}

/// Held to nothing but the default comprehension depth.
fn free() -> Ranking {
    Ranking { comprehensions: 2, ..Ranking::default() }
}

fn requiring(rules: &[&str]) -> Placement {
    Placement { require: rules.iter().map(|r| (*r).to_owned()).collect(), ..Placement::default() }
}

fn names(ranked: &[Suggestion]) -> Vec<&str> {
    ranked.iter().map(|s| s.name.as_str()).collect()
}

fn reason<'a>(s: &'a Suggestion, rule: &str) -> &'a Reason {
    s.reasons.iter().find(|r| r.rule == rule).unwrap_or_else(|| panic!("no {rule} in {s:?}"))
}

fn message(refused: &Outcome) -> &str {
    match refused {
        Outcome::Error { code: ErrorCode::BadExpression, message } => message,
        other => panic!("not a bad expression: {other:?}"),
    }
}

#[test]
fn rules_read_every_fact_by_its_name_and_in_facts() {
    let mut gpu = worker("gpu", "linux", 32, 1.0);
    let labels = BTreeMap::from([("fast-disk".to_owned(), Fact::Bool(true))]);
    gpu.facts.insert("labels".to_owned(), Fact::Map(labels));
    let targets = Fact::List(vec![text("wasm32-unknown-unknown")]);
    gpu.facts.insert("rust_targets".to_owned(), targets);
    let rules = [
        r#"os == "linux" && cpus >= 16"#,
        r#"labels["fast-disk"]"#,
        r#""wasm32-unknown-unknown" in rust_targets"#,
        "has(facts.labels) && !has(facts.gpus)",
        "load < 2",
    ];
    let ranked = rank(&requiring(&rules), &[gpu], &BTreeMap::new(), free()).unwrap();
    let only = ranked.first().unwrap();
    assert!(only.fits, "{only:?}");
    assert!(only.reasons.iter().all(|r| r.held), "{only:?}");
}

#[test]
fn a_rule_that_is_not_cel_is_refused_saying_where() {
    let refused = check(&requiring(&[r#"os == "linux" &&"#]), 1).unwrap_err();
    let said = message(&refused);
    assert!(said.contains("1:17") && said.contains("os =="), "{said}");
    for (rule, why) in [
        ("", "empty"),
        (&"x".repeat(EXPR_MAX + 1), "over the"),
        (&format!("{}1{}", "(".repeat(40), ")".repeat(40)), "nested"),
    ] {
        let said = message(&check(&requiring(&[rule]), 1).unwrap_err()).to_owned();
        assert!(said.contains(why), "{rule:.20}: {said}");
    }
    let many = Placement {
        prefer: vec![Preference { expr: "true".to_owned(), weight: 1 }; RULES_MAX + 1],
        ..Placement::default()
    };
    assert!(message(&check(&many, 1).unwrap_err()).contains("prefer"));
}

/// Rules come from agents: the worst a rule of the longest length can do is fail to compile or
/// fail to hold, on a runtime thread's stack.
#[test]
fn a_hostile_rule_fails_instead_of_overflowing_the_stack() {
    let terms = EXPR_MAX / 6;
    let hostile = [
        format!("{}true", "!".repeat(EXPR_MAX - 4)),
        format!("{}1", "-".repeat(EXPR_MAX - 1)),
        vec!["cpus"; terms].join("&&"),
        format!("{} > 0", vec!["1"; EXPR_MAX / 2].join("+")),
        format!("facts{}", ".a".repeat((EXPR_MAX - 5) / 2)),
        format!("{}1{}", "[".repeat(NESTING_MAX), "]".repeat(NESTING_MAX)),
        format!("{}1{}", "(".repeat(NESTING_MAX), ")".repeat(NESTING_MAX)),
    ];
    let judged = std::thread::Builder::new()
        .stack_size(2 << 20)
        .spawn(move || {
            let fleet = [worker("a", "linux", 8, 0.0)];
            for rule in hostile {
                let rule = rule.get(..EXPR_MAX).unwrap_or(&rule).to_owned();
                if let Ok(ranked) = rank(&requiring(&[&rule]), &fleet, &BTreeMap::new(), free()) {
                    assert_eq!(ranked.len(), 1);
                }
            }
        })
        .unwrap()
        .join();
    assert!(judged.is_ok(), "a rule overflowed a 2 MiB stack");
}

#[test]
fn a_missing_fact_or_a_rule_that_is_not_a_flag_does_not_hold_and_says_why() {
    let ranked = rank(
        &requiring(&["probes.cuda != \"\"", "cpus", "os == \"linux\""]),
        &[worker("box", "linux", 8, 0.0)],
        &BTreeMap::new(),
        free(),
    )
    .unwrap();
    let s = ranked.first().unwrap();
    assert!(!s.fits);
    assert!(reason(s, "probes.cuda != \"\"").detail.contains("probes"), "{s:?}");
    assert!(reason(s, "cpus").detail.contains("a number"), "{s:?}");
    assert!(reason(s, "os == \"linux\"").held);
    let why = choose(&requiring(&["probes.cuda != \"\""]), &ranked).unwrap_err();
    assert!(why.starts_with("box: ") && why.contains("probes"), "{why}");
}

#[test]
fn preferences_rank_the_workers_that_fit_with_their_points() {
    let fleet = [
        worker("mac", "macos", 12, 0.0),
        worker("big", "linux", 64, 30.0),
        worker("small", "linux", 8, 0.0),
    ];
    let placement = Placement {
        require: vec!["cpus >= 8".to_owned()],
        prefer: vec![
            Preference { expr: "os == \"linux\"".to_owned(), weight: 10 },
            Preference { expr: "cpus".to_owned(), weight: 1 },
            Preference { expr: "load / double(cpus)".to_owned(), weight: -20 },
        ],
        ..Placement::default()
    };
    let ranked = rank(&placement, &fleet, &BTreeMap::new(), free()).unwrap();
    assert_eq!(names(&ranked), ["big", "small", "mac"]);
    let big = ranked.first().unwrap();
    assert_eq!(big.score, 10 + 64 - 9);
    assert_eq!(reason(big, "cpus").points, 64);
    assert_eq!(choose(&placement, &ranked), Ok(big.worker));
}

#[test]
fn a_pin_is_honoured_over_every_rule_and_names_what_it_overrode() {
    let fleet = [worker("linux", "linux", 64, 0.0), worker("mac", "macos", 8, 0.0)];
    let mac = fleet.get(1).unwrap().worker;
    let placement = Placement { pin: Some(mac), ..requiring(&["os == \"linux\"", "cpus >= 32"]) };
    let ranked = rank(&placement, &fleet, &BTreeMap::new(), free()).unwrap();
    let first = ranked.first().unwrap();
    assert_eq!((first.name.as_str(), first.fits), ("mac", true));
    assert!(!reason(first, "cpus >= 32").held, "the overridden rule still shows");
    assert!(!ranked[1].fits, "nothing else fits a pinned task");
    assert_eq!(choose(&placement, &ranked), Ok(mac));

    let away = WorkerId::new();
    let lost = Placement { pin: Some(away), ..Placement::default() };
    let ranked = rank(&lost, &fleet, &BTreeMap::new(), free()).unwrap();
    assert!(choose(&lost, &ranked).unwrap_err().contains("does not know"));

    let mut offline = fleet;
    if let Some(m) = offline.get_mut(1) {
        m.online = false;
    }
    let ranked = rank(&placement, &offline, &BTreeMap::new(), free()).unwrap();
    let why = choose(&placement, &ranked).unwrap_err();
    assert_eq!(why, "mac: not online", "a pin never moves elsewhere");
}

#[test]
fn near_and_avoid_follow_where_the_other_tasks_run() {
    let fleet = [worker("a", "linux", 8, 0.0), worker("b", "linux", 8, 0.0)];
    let (a, b) = (fleet.first().unwrap().worker, fleet.get(1).unwrap().worker);
    let peers = BTreeMap::from([(TaskId(1), b), (TaskId(2), a)]);
    let near = Placement { near: vec![Peer::Task(TaskId(1))], ..Placement::default() };
    assert_eq!(names(&rank(&near, &fleet, &peers, free()).unwrap()), ["b", "a"]);
    let avoid =
        Placement { avoid: vec![Peer::Task(TaskId(1)), Peer::Worker(a)], ..Placement::default() };
    let ranked = rank(&avoid, &fleet, &peers, free()).unwrap();
    assert_eq!(ranked.iter().map(|s| s.score).collect::<Vec<_>>(), [-100, -100]);
    let both = Placement { avoid: vec![Peer::Task(TaskId(1))], ..near };
    let ranked = rank(&both, &fleet, &peers, free()).unwrap();
    assert!(ranked.iter().all(|s| s.fits), "near and avoid steer, never refuse");
}

#[test]
fn a_worker_at_the_project_s_cap_does_not_fit_even_pinned() {
    let mut fleet = [worker("full", "linux", 8, 0.0), worker("free", "linux", 8, 0.0)];
    if let Some(full) = fleet.first_mut() {
        full.live = 2;
    }
    let ranked = rank(
        &Placement::default(),
        &fleet,
        &BTreeMap::new(),
        Ranking { per_worker: Some(2), ..free() },
    )
    .unwrap();
    assert_eq!(names(&ranked), ["free", "full"]);
    let full = &ranked[1];
    assert!(reason(full, "live_per_worker").detail.contains("runs 2"), "{full:?}");
    let pinned = Placement { pin: Some(full.worker), ..Placement::default() };
    let ranked =
        rank(&pinned, &fleet, &BTreeMap::new(), Ranking { per_worker: Some(2), ..free() }).unwrap();
    let why = choose(&pinned, &ranked).unwrap_err();
    assert!(why.starts_with("full: runs 2"), "{why}");
}

#[test]
fn equal_workers_go_to_the_least_busy_then_by_name() {
    let fleet =
        [worker("c", "linux", 8, 4.0), worker("b", "linux", 16, 4.0), worker("a", "linux", 8, 4.0)];
    assert_eq!(
        names(&rank(&Placement::default(), &fleet, &BTreeMap::new(), free()).unwrap()),
        ["b", "a", "c"]
    );
}

/// Rules come from agents and run under the hub's clock: a rule's worst case is counted before
/// it runs, so comprehensions nested past the person's depth, a walk inside a walk of the facts,
/// a list rebuilt at each step, or rules too many together are refused naming why.
#[test]
fn a_rule_s_worst_case_is_counted_and_refused_before_it_runs() {
    let refuse = |rule: &str, depth: u8, why: &str| {
        let said = compile(rule, depth).unwrap_err();
        assert!(said.contains(why), "{rule}: {said}");
    };
    refuse("facts.all(k, facts.all(j, true))", 1, "nested 2 deep");
    refuse("facts.all(k, facts.all(j, true))", 2, "steps at its worst");
    refuse("rust_targets.map(x, x).size() > 0", 2, "steps at its worst");
    let (_, small) = compile("[1, 2, 3].all(x, x > 0)", 1).unwrap();
    let (_, scan) = compile(r#""wasm32" in rust_targets"#, 1).unwrap();
    assert!(small < 64, "a literal's walk costs its own: {small}");
    assert!(scan > 1024, "`in` scans the list a fact may hold: {scan}");

    let walk = r#"rust_targets.exists(x, x == "a" || x == "b" || x == "c")"#;
    let (_, each) = compile(walk, 1).unwrap();
    assert!(each < RULE_COST_MAX, "one walk alone is fine: {each}");
    let together = usize::try_from(PLACEMENT_COST_MAX / each + 1).unwrap();
    assert!(together <= RULES_MAX, "{together} walks");
    let many = Placement { require: vec![walk.to_owned(); together], ..Placement::default() };
    assert!(message(&check(&many, 1).unwrap_err()).contains("at their worst on each worker"));
}

/// A rule's answer never depends on where an error falls or how a map happens to be laid out:
/// an error a later element settles is absorbed as CEL says, and a rule past its deadline does
/// not hold and says so.
#[test]
fn a_rule_is_judged_the_same_every_time_and_not_past_its_deadline() {
    let fleet = [worker("a", "linux", 8, 0.0)];
    let rules = [
        "[0, 1].exists(x, 1 / x == 1)",
        "[1, 0].exists(x, 1 / x == 1)",
        r#"{"b": 1, "a": 0}.exists(k, 1 / {"b": 1, "a": 0}[k] == 1)"#,
        "!{\"b\": 0, \"a\": 2}.all(k, 1 / {\"b\": 0, \"a\": 2}[k] == 1)",
    ];
    for _ in 0..8 {
        let ranked = rank(&requiring(&rules), &fleet, &BTreeMap::new(), free()).unwrap();
        let only = ranked.first().unwrap();
        assert!(only.fits, "{only:?}");
    }
    let past = Ranking { until: Some(Instant::now()), ..free() };
    let ranked = rank(&requiring(&["cpus > 1"]), &fleet, &BTreeMap::new(), past).unwrap();
    let only = ranked.first().unwrap();
    assert!(!only.fits);
    assert!(reason(only, "cpus > 1").detail.contains("not judged"), "{only:?}");
}

/// A worker's facts are kept within their bounds as it reports them: a long text is cut at a
/// character, a long list keeps its first items, and past the bytes in all the rest is
/// dropped in name order.
#[test]
fn a_worker_s_facts_are_kept_within_their_bounds() {
    let mut facts = Facts::new();
    facts.insert("a_text".to_owned(), text(&"é".repeat(FACT_TEXT_MAX)));
    facts.insert("b_list".to_owned(), Fact::List(vec![Fact::Int(1); FACT_ITEMS_MAX * 2]));
    for n in 0..FACTS_MAX {
        facts.insert(format!("c{n:05}"), text(&"x".repeat(64)));
    }
    let kept = bounded(facts);
    let Some(Fact::Text(t)) = kept.get("a_text") else { panic!("{:?}", kept.get("a_text")) };
    assert_eq!(t.len(), FACT_TEXT_MAX, "cut on a character boundary");
    let Some(Fact::List(items)) = kept.get("b_list") else { panic!("no list") };
    assert_eq!(items.len(), FACT_ITEMS_MAX);
    let bytes: usize = kept.iter().map(|(k, f)| k.len().saturating_add(text_len(f))).sum();
    assert!(bytes <= FACTS_BYTES_MAX, "{bytes}");
    assert!(kept.len() < FACTS_MAX, "the bytes ran out first: {}", kept.len());
    let last = kept.keys().last().unwrap();
    assert!(kept.keys().all(|k| k <= last) && last.starts_with('c'), "dropped in name order");
}

/// The bytes of text a fact holds.
fn text_len(fact: &Fact) -> usize {
    match fact {
        Fact::Text(t) => t.len(),
        Fact::List(items) => items.iter().map(text_len).sum(),
        Fact::Map(parts) => parts.iter().map(|(k, f)| k.len().saturating_add(text_len(f))).sum(),
        _ => 0,
    }
}
