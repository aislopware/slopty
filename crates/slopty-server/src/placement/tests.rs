use super::*;

fn text(t: &str) -> Fact {
    Fact::Text(t.to_owned())
}

fn worker(name: &str, cpus: i64, load: f64) -> Candidate {
    let mut facts = Facts::new();
    facts.insert("cpus".to_owned(), Fact::Int(cpus));
    facts.insert("load".to_owned(), Fact::Float(load));
    Candidate {
        worker: WorkerId::new(),
        name: name.to_owned(),
        online: true,
        reported: true,
        facts,
        fleet_live: 0,
        clone: false,
    }
}

fn installed(w: &mut Candidate, agents: &[&str]) {
    let map = agents.iter().map(|a| ((*a).to_owned(), text("1.0"))).collect();
    w.facts.insert("agents".to_owned(), Fact::Map(map));
}

/// An agent goes only where it is installed, named or not, and a worker that has not said
/// what it has says so.
#[test]
fn an_agent_goes_only_where_it_is_installed_even_pinned() {
    let mut fleet = [worker("has", 8, 1.0), worker("lacks", 16, 0.0), worker("quiet", 32, 0.0)];
    installed(&mut fleet[0], &["claude", "codex"]);
    installed(&mut fleet[1], &["claude"]);
    fleet[2].reported = false;
    let codex = Wanted { agent: Some(Installed::program("codex")), ..Wanted::default() };
    assert_eq!(choose(&fleet, &codex), Ok(fleet[0].worker), "the only one that fits");

    fleet[0].online = false;
    assert_eq!(
        choose(&fleet, &codex).unwrap_err(),
        "has: not online; lacks: codex is not installed; quiet: has not said yet whether codex \
         is installed"
    );

    let pinned = Wanted { pin: Some(fleet[1].worker), ..codex };
    assert_eq!(choose(&fleet, &pinned).unwrap_err(), "lacks: codex is not installed");
    let claude = Wanted { agent: Some(Installed::program("claude")), ..pinned };
    assert_eq!(choose(&fleet, &claude), Ok(fleet[1].worker));
    let unknown = Wanted { pin: Some(WorkerId::new()), ..Wanted::default() };
    assert!(choose(&fleet, &unknown).unwrap_err().contains("which the server does not know"));
    assert_eq!(choose(&[], &Wanted::default()).unwrap_err(), "the server knows no worker");
}

/// A named worker is taken over every other, busy or not.
#[test]
fn a_pin_is_honoured_over_a_quieter_worker() {
    let mut fleet = [worker("quiet", 32, 0.0), worker("busy", 4, 8.0)];
    fleet[1].fleet_live = 9;
    let pinned = Wanted { pin: Some(fleet[1].worker), ..Wanted::default() };
    assert_eq!(choose(&fleet, &pinned), Ok(fleet[1].worker));
}

/// With nothing named, a worker with a clone goes first when one is wanted, then the one
/// running the fewest agents, then the least busy per cpu, then by name.
#[test]
fn equal_workers_go_beside_a_clone_then_to_the_least_busy_then_by_name() {
    let mut fleet = [worker("c", 8, 4.0), worker("b", 16, 4.0), worker("a", 8, 4.0)];
    let any = Wanted::default();
    assert_eq!(choose(&fleet, &any), Ok(fleet[1].worker), "b has the most cpus for its load");
    fleet[1].fleet_live = 1;
    assert_eq!(choose(&fleet, &any), Ok(fleet[2].worker), "then by name");
    fleet[0].clone = true;
    assert_eq!(choose(&fleet, &any), Ok(fleet[2].worker), "a clone counts only when wanted");
    let beside = Wanted { clone: true, ..Wanted::default() };
    assert_eq!(choose(&fleet, &beside), Ok(fleet[0].worker));
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
