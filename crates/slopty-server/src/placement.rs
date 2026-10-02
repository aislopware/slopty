//! Where a task may run and where it had better (`docs/decisions/projects.md`): a task's
//! [`Placement`] rules, in CEL, evaluated against each worker's [`Facts`].
//!
//! Every fact is a variable of its own name (`os`, `cpus`, `labels`), and all of them are the
//! map `facts` too, so `has(facts.gpus)` asks whether a worker reports one. A rule that reads a
//! fact a worker does not have does not hold there, and says so.
//!
//! Pure: the hub gathers the candidates under its lock and ranks them outside it, on a
//! blocking thread with a deadline.
//!
//! A rule's cost is bounded before it runs. Its syntax tree is walked at compile time, and a
//! rule is refused with more than [`NODES_MAX`] nodes, a literal of more than [`LITERAL_MAX`]
//! elements, comprehensions (`all`, `exists`, `exists_one`, `map`, `filter`) nested deeper than
//! the person allows, or a worst case over [`RULE_COST_MAX`] steps; a placement's rules
//! together over [`PLACEMENT_COST_MAX`]. The worst case counts every node once per element of
//! each comprehension around it, a list from the facts as [`FACT_ITEMS_MAX`] elements, a
//! literal as its own, a concatenation as the sum of its parts, `in` as the list it scans, and
//! a `map` or `filter` as the square of its range, for the list it rebuilds at each step. The
//! facts it reads are bounded as a worker reports them ([`bounded`]): [`FACTS_MAX`] values,
//! texts of [`FACT_TEXT_MAX`], [`FACTS_BYTES_MAX`] in all.
//!
//! Ranking runs on the blocking pool, a few at a time, and stops judging at a deadline
//! ([`Ranking::until`]): a rule not judged by then does not hold. Evaluation is deterministic:
//! the CEL fork iterates maps in key order, and an error a later element overrides in `all`,
//! `exists` and `exists_one` is absorbed as the specification says.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Instant;

use cel::common::ast::{EntryExpr, Expr, operators};
use cel::{Context, IdedExpr, Program, Value};
use slopty_core::WorkerId;
use slopty_proto::orchestration::{ErrorCode, Outcome};
use slopty_proto::project::{
    EXPR_MAX, Fact, Facts, Peer, Placement, RULES_MAX, Reason, Suggestion, TaskId,
};

/// Brackets nested deeper than this are refused before parsing: the parser recurses on each.
const NESTING_MAX: usize = 32;
/// Most nodes one rule's syntax tree holds (a macro adds a few of its own).
pub(crate) const NODES_MAX: usize = 512;
/// Most elements one list or map literal holds.
pub(crate) const LITERAL_MAX: usize = 64;
/// Most items one list or map fact holds, as a worker reports it; the rest are dropped.
pub(crate) const FACT_ITEMS_MAX: usize = 1024;
/// Most facts one worker reports, counting those inside lists and maps.
pub(crate) const FACTS_MAX: usize = 4096;
/// Longest text fact, in bytes; a longer one is cut.
pub(crate) const FACT_TEXT_MAX: usize = 4096;
/// Most bytes of one worker's facts, names and texts together; what is past it is dropped.
/// It keeps every worker's facts within one link frame, many workers to an answer.
pub(crate) const FACTS_BYTES_MAX: usize = 64 * 1024;
/// Most steps one rule may take at its worst.
pub(crate) const RULE_COST_MAX: u64 = 1 << 16;
/// Most steps a placement's rules may take together at their worst, on one worker.
pub(crate) const PLACEMENT_COST_MAX: u64 = 1 << 18;

/// A worker as placement sees it.
#[derive(Clone, Debug)]
pub(crate) struct Candidate {
    /// Which.
    pub worker: WorkerId,
    /// Its name.
    pub name: String,
    /// Whether it is online now.
    pub online: bool,
    /// Whether it has reported its own facts yet: until it has, only the agents it registered
    /// with are known to be installed.
    pub reported: bool,
    /// Its facts, the server's and its own.
    pub facts: Facts,
    /// How many of the project's agents run on it, and are being started there.
    pub live: u16,
    /// How many agents run on it in all, and are being started there.
    pub fleet_live: u16,
}

/// What a ranking is held to.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Ranking {
    /// The project's cap on its agents per worker, when there is a project.
    pub per_worker: Option<u16>,
    /// The person's cap on every worker's agents.
    pub fleet_per_worker: Option<u16>,
    /// How deep comprehensions may nest in a rule.
    pub comprehensions: u8,
    /// When the ranking stops judging: a rule not judged by then does not hold.
    pub until: Option<Instant>,
    /// The agent the start runs, by its name among a worker's `agents` facts: a worker that
    /// does not report it installed does not fit, pinned or not.
    pub agent: Option<&'static str>,
}

/// A rule that compiled.
struct Rule {
    text: String,
    program: Program,
    cost: u64,
}

/// Compile a placement expression, with comprehensions nested at most `comprehensions` deep.
///
/// # Errors
/// Why it is not a rule: empty, too long, too deeply nested, too large, or not CEL.
pub(crate) fn compile(expr: &str, comprehensions: u8) -> Result<(Program, u64), String> {
    let expr = expr.trim();
    if expr.is_empty() {
        return Err("an empty rule".to_owned());
    }
    if expr.len() > EXPR_MAX {
        return Err(format!("{} bytes, over the {EXPR_MAX} a rule may take", expr.len()));
    }
    if nesting(expr) > NESTING_MAX {
        return Err(format!("brackets nested over {NESTING_MAX} deep"));
    }
    let program = Program::compile(expr).map_err(|e| e.to_string())?;
    let shape = shape(program.expression());
    if shape.nodes > NODES_MAX {
        return Err(format!("{} nodes, over the {NODES_MAX} a rule may take", shape.nodes));
    }
    if shape.literal > LITERAL_MAX {
        return Err(format!(
            "a literal of {} elements, over the {LITERAL_MAX} a rule may take",
            shape.literal
        ));
    }
    if shape.comprehensions > usize::from(comprehensions) {
        return Err(format!(
            "comprehensions (all, exists, exists_one, map, filter) nested {} deep, over the {} \
             the person allows (`[server.projects] comprehension_depth`)",
            shape.comprehensions, comprehensions
        ));
    }
    if shape.cost > RULE_COST_MAX {
        return Err(format!(
            "it may take {} steps at its worst, over the {RULE_COST_MAX} a rule may take: walk \
             fewer lists inside one another",
            shape.cost
        ));
    }
    Ok((program, shape.cost))
}

/// What a rule's syntax tree holds, as its cost depends on it.
#[derive(Debug, Default)]
struct Shape {
    nodes: usize,
    literal: usize,
    comprehensions: usize,
    /// Steps at its worst ([`RULE_COST_MAX`]).
    cost: u64,
}

/// Walk a rule's syntax tree without recursing: its nodes, its largest literal, how deep its
/// comprehensions nest, and its worst case in steps. A comprehension's range, first value and
/// result are evaluated once, so they are at its own depth; its condition and step run per
/// element, one deeper, as many times more as its range may hold.
fn shape(root: &IdedExpr) -> Shape {
    let mut shape = Shape::default();
    let mut stack: Vec<(&IdedExpr, usize, u64)> = vec![(root, 0, 1)];
    while let Some((e, depth, times)) = stack.pop() {
        shape.nodes = shape.nodes.saturating_add(1);
        shape.comprehensions = shape.comprehensions.max(depth);
        shape.cost = shape.cost.saturating_add(times);
        match &e.expr {
            Expr::Comprehension(c) => {
                let inside = depth.saturating_add(1);
                shape.comprehensions = shape.comprehensions.max(inside);
                let range = elements(&c.iter_range);
                let each = times.saturating_mul(range);
                // `map` and `filter` build their list anew at each step.
                if matches!(c.accu_init.expr, Expr::List(_)) {
                    shape.cost = shape.cost.saturating_add(each.saturating_mul(range));
                }
                stack.extend([(&c.iter_range, depth, times), (&c.accu_init, depth, times)]);
                stack.push((&c.result, depth, times));
                stack.extend([(&c.loop_cond, inside, each), (&c.loop_step, inside, each)]);
            }
            Expr::Call(call) => {
                if call.func_name == operators::IN
                    && let Some(list) = call.args.get(1)
                {
                    shape.cost = shape.cost.saturating_add(times.saturating_mul(elements(list)));
                }
                stack.extend(call.target.iter().map(|t| (&**t, depth, times)));
                stack.extend(call.args.iter().map(|a| (a, depth, times)));
            }
            Expr::List(list) => {
                shape.literal = shape.literal.max(list.elements.len());
                stack.extend(list.elements.iter().map(|x| (x, depth, times)));
            }
            Expr::Map(map) => {
                shape.literal = shape.literal.max(map.entries.len());
                entries(&map.entries, depth, times, &mut stack);
            }
            Expr::Struct(fields) => {
                shape.literal = shape.literal.max(fields.entries.len());
                entries(&fields.entries, depth, times, &mut stack);
            }
            Expr::Select(select) => stack.push((&select.operand, depth, times)),
            Expr::Unspecified | Expr::Ident(_) | Expr::Literal(_) => {}
        }
    }
    shape
}

fn entries<'e>(
    entries: &'e [cel::common::ast::IdedEntryExpr],
    depth: usize,
    times: u64,
    stack: &mut Vec<(&'e IdedExpr, usize, u64)>,
) {
    for entry in entries {
        match &entry.expr {
            EntryExpr::MapEntry(pair) => {
                stack.extend([(&pair.key, depth, times), (&pair.value, depth, times)]);
            }
            EntryExpr::StructField(field) => stack.push((&field.value, depth, times)),
        }
    }
}

/// The most elements the list or map `expr` makes may hold: a literal its own, a
/// concatenation its parts', a choice the larger, a `map` or `filter` its range's; anything
/// read from the facts, [`FACT_ITEMS_MAX`].
fn elements(expr: &IdedExpr) -> u64 {
    let facts = u64::try_from(FACT_ITEMS_MAX).unwrap_or(u64::MAX);
    let count = |n: usize| u64::try_from(n).unwrap_or(u64::MAX);
    let mut total = 0_u64;
    let mut stack = vec![expr];
    while let Some(e) = stack.pop() {
        let own = match &e.expr {
            Expr::List(list) => count(list.elements.len()),
            Expr::Map(map) => count(map.entries.len()),
            Expr::Comprehension(c) if matches!(c.accu_init.expr, Expr::List(_)) => {
                stack.push(&c.iter_range);
                0
            }
            Expr::Call(call) if call.func_name == operators::ADD && call.args.len() == 2 => {
                stack.extend(call.args.iter());
                0
            }
            Expr::Call(call) if call.func_name == operators::CONDITIONAL => {
                let branches = call.args.iter().skip(1);
                branches.map(elements).max().unwrap_or(facts)
            }
            _ => facts,
        };
        total = total.saturating_add(own);
    }
    total
}

/// A worker's facts as the server keeps them: lists and maps cut to [`FACT_ITEMS_MAX`] items,
/// texts to [`FACT_TEXT_MAX`] bytes, and [`FACTS_MAX`] facts and [`FACTS_BYTES_MAX`] bytes in
/// all, so no rule walks more and every worker's facts fit a frame. What is past a bound is
/// dropped, in name order.
pub(crate) fn bounded(facts: Facts) -> Facts {
    let mut budget = Budget { facts: FACTS_MAX, bytes: FACTS_BYTES_MAX };
    facts
        .into_iter()
        .map_while(|(name, fact)| {
            budget.take_bytes(name.len())?;
            Some((name, cut(fact, &mut budget)?))
        })
        .collect()
}

/// What is left of a worker's bounds on its facts.
struct Budget {
    facts: usize,
    bytes: usize,
}

impl Budget {
    fn take_bytes(&mut self, n: usize) -> Option<()> {
        self.bytes = self.bytes.checked_sub(n.saturating_add(8))?;
        Some(())
    }
}

fn cut(fact: Fact, budget: &mut Budget) -> Option<Fact> {
    budget.facts = budget.facts.checked_sub(1)?;
    Some(match fact {
        Fact::Text(t) => {
            let mut end = t.len().min(FACT_TEXT_MAX);
            while !t.is_char_boundary(end) {
                end = end.saturating_sub(1);
            }
            budget.take_bytes(end)?;
            Fact::Text(t.get(..end).unwrap_or_default().to_owned())
        }
        Fact::List(items) => Fact::List(
            items.into_iter().take(FACT_ITEMS_MAX).map_while(|f| cut(f, budget)).collect(),
        ),
        Fact::Map(parts) => Fact::Map(
            parts
                .into_iter()
                .take(FACT_ITEMS_MAX)
                .map_while(|(k, f)| {
                    budget.take_bytes(k.len())?;
                    Some((k, cut(f, budget)?))
                })
                .collect(),
        ),
        other => {
            budget.take_bytes(0)?;
            other
        }
    })
}

/// How deep `expr`'s brackets nest, strings aside.
fn nesting(expr: &str) -> usize {
    let (mut depth, mut deepest, mut quote, mut escaped) = (0_usize, 0_usize, None, false);
    for c in expr.chars() {
        if let Some(q) = quote {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '"' | '\'' => quote = Some(c),
            '(' | '[' | '{' => {
                depth = depth.saturating_add(1);
                deepest = deepest.max(depth);
            }
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    deepest
}

/// Check every rule of `placement` compiles, and it holds no more than [`RULES_MAX`] of each.
///
/// # Errors
/// [`ErrorCode::BadExpression`] naming the rule and why.
pub(crate) fn check(placement: &Placement, comprehensions: u8) -> Result<(), Outcome> {
    compiled(placement, comprehensions).map(|_| ())
}

/// A placement's rules, compiled.
struct Compiled {
    require: Vec<Rule>,
    prefer: Vec<(Rule, i32)>,
}

impl Compiled {
    fn cost(&self) -> u64 {
        let rules = self.require.iter().chain(self.prefer.iter().map(|(r, _)| r));
        rules.map(|r| r.cost).fold(0, u64::saturating_add)
    }
}

fn compiled(placement: &Placement, comprehensions: u8) -> Result<Compiled, Outcome> {
    let counts = [
        ("require", placement.require.len()),
        ("prefer", placement.prefer.len()),
        ("near", placement.near.len()),
        ("avoid", placement.avoid.len()),
    ];
    if let Some((kind, n)) = counts.into_iter().find(|(_, n)| *n > RULES_MAX) {
        return Err(bad(format!("{n} {kind} rules, over the {RULES_MAX} a placement may hold")));
    }
    let rule = |text: &str| {
        compile(text, comprehensions)
            .map(|(program, cost)| Rule { text: text.trim().to_owned(), program, cost })
            .map_err(|why| bad(format!("{text:?} is not a rule: {why}")))
    };
    let require = placement.require.iter().map(|r| rule(r)).collect::<Result<_, _>>()?;
    let prefer = placement
        .prefer
        .iter()
        .map(|p| rule(&p.expr).map(|r| (r, p.weight)))
        .collect::<Result<_, _>>()?;
    let compiled = Compiled { require, prefer };
    let cost = compiled.cost();
    if cost > PLACEMENT_COST_MAX {
        return Err(bad(format!(
            "the rules may take {cost} steps at their worst on each worker, over the \
             {PLACEMENT_COST_MAX} a placement may take: fewer or simpler rules"
        )));
    }
    Ok(compiled)
}

const fn bad(message: String) -> Outcome {
    Outcome::Error { code: ErrorCode::BadExpression, message }
}

/// Every candidate for `placement`, best first, each with its reasons.
///
/// `peers` says where each of the project's tasks runs now, for `near` and `avoid`; `ranking`
/// holds the caps per worker, how deep comprehensions may nest, and when judging stops.
///
/// # Errors
/// [`ErrorCode::BadExpression`] when a rule does not compile.
pub(crate) fn rank(
    placement: &Placement,
    candidates: &[Candidate],
    peers: &BTreeMap<TaskId, WorkerId>,
    ranking: Ranking,
) -> Result<Vec<Suggestion>, Outcome> {
    let Compiled { require, prefer } = compiled(placement, ranking.comprehensions)?;
    let mut ranked: Vec<(Suggestion, bool, u16, f64)> = candidates
        .iter()
        .map(|c| {
            let pinned = placement.pin == Some(c.worker);
            let s = judge(c, placement, (&require, &prefer), peers, ranking);
            (s, pinned, c.live, busy(&c.facts))
        })
        .collect();
    ranked.sort_by(|(a, a_pin, a_live, a_busy), (b, b_pin, b_live, b_busy)| {
        b.fits
            .cmp(&a.fits)
            .then(b_pin.cmp(a_pin))
            .then(b.score.cmp(&a.score))
            .then(a_live.cmp(b_live))
            .then(a_busy.total_cmp(b_busy))
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.worker.cmp(&b.worker))
    });
    Ok(ranked.into_iter().map(|(s, ..)| s).collect())
}

/// The worker to start on: the best that fits.
///
/// # Errors
/// The reason, in words, no worker fits, naming each with why.
pub(crate) fn choose(placement: &Placement, ranked: &[Suggestion]) -> Result<WorkerId, String> {
    if let Some(best) = ranked.first().filter(|s| s.fits) {
        return Ok(best.worker);
    }
    if let Some(pin) = placement.pin
        && !ranked.iter().any(|s| s.worker == pin)
    {
        return Err(format!("it is pinned to worker {pin}, which the server does not know"));
    }
    if ranked.is_empty() {
        return Err("the server knows no worker".to_owned());
    }
    let why: Vec<String> = ranked
        .iter()
        .filter(|s| placement.pin.is_none_or(|pin| pin == s.worker))
        .map(|s| {
            let failed = s.reasons.iter().find(|r| !r.held && blocks(&r.rule, placement));
            let why = failed.map_or_else(
                || "does not fit".to_owned(),
                |r| match &r.need {
                    Some(need) => format!("fails {need} ({})", r.rule),
                    None => r.detail.clone(),
                },
            );
            format!("{}: {why}", s.name)
        })
        .collect();
    Err(why.join("; "))
}

/// Whether a failed rule keeps a worker from running the task: a preference never does, and
/// with a pin only being online, having room and having the agent do.
fn blocks(rule: &str, placement: &Placement) -> bool {
    match rule {
        "online" | "live_per_worker" | "pin" | "fleet live_per_worker" | "agent" => true,
        _ if placement.pin.is_some() => false,
        _ => placement.require.iter().any(|r| r.trim() == rule),
    }
}

fn judge(
    c: &Candidate,
    placement: &Placement,
    (require, prefer): (&[Rule], &[(Rule, i32)]),
    peers: &BTreeMap<TaskId, WorkerId>,
    ranking: Ranking,
) -> Suggestion {
    let mut reasons = Vec::new();
    let mut fits = true;
    let mut held = |rule: &str, ok: bool, points: i64, detail: String| {
        reasons.push(Reason { rule: rule.to_owned(), held: ok, points, detail, need: None });
    };
    if let Some(pin) = placement.pin {
        let here = pin == c.worker;
        fits &= here;
        let detail = if here { String::new() } else { format!("the task is pinned to {pin}") };
        held("pin", here, 0, detail);
    }
    fits &= c.online;
    held("online", c.online, 0, if c.online { String::new() } else { "not online".to_owned() });
    if let Some(cap) = ranking.per_worker {
        let room = c.live < cap;
        fits &= room;
        let detail = if room {
            String::new()
        } else {
            format!("runs {} of the project's agents, its live_per_worker", c.live)
        };
        held("live_per_worker", room, 0, detail);
    }
    if let Some(cap) = ranking.fleet_per_worker {
        let room = c.fleet_live < cap;
        fits &= room;
        let detail = if room {
            String::new()
        } else {
            format!(
                "runs {} agents, the live_per_worker the person allows every worker",
                c.fleet_live
            )
        };
        held("fleet live_per_worker", room, 0, detail);
    }
    if let Some(agent) = ranking.agent {
        let has = matches!(c.facts.get("agents"), Some(Fact::Map(installed))
            if installed.contains_key(agent));
        let detail = match (has, c.reported) {
            (true, _) => String::new(),
            (false, true) => format!("{agent} is not installed"),
            (false, false) => format!("has not said yet whether {agent} is installed"),
        };
        fits &= has;
        held("agent", has, 0, detail);
    }
    let ctx = context(&c.facts);
    let pinned = placement.pin.is_some();
    let late = || ranking.until.is_some_and(|until| Instant::now() >= until);
    for rule in require {
        if late() {
            fits &= pinned;
            held(&rule.text, false, 0, not_judged());
            continue;
        }
        let (ok, detail) = match rule.program.execute(&ctx) {
            Ok(Value::Bool(true)) => (true, String::new()),
            Ok(Value::Bool(false)) => (false, "false here".to_owned()),
            Ok(other) => (false, format!("gives {}, not true or false", kind(&other))),
            Err(e) => (false, said(&e)),
        };
        // A pin is the orchestrator's own word, over any rule.
        fits &= ok || pinned;
        held(&rule.text, ok, 0, detail);
    }
    let mut score = 0_i64;
    for (rule, weight) in prefer {
        let weight = i64::from(*weight);
        if late() {
            held(&rule.text, false, 0, not_judged());
            continue;
        }
        let (ok, points, detail) = match rule.program.execute(&ctx) {
            Ok(Value::Bool(b)) => (b, if b { weight } else { 0 }, String::new()),
            Ok(Value::Int(n)) => (true, weight.saturating_mul(n), String::new()),
            Ok(Value::UInt(n)) => {
                (true, weight.saturating_mul(i64::try_from(n).unwrap_or(i64::MAX)), String::new())
            }
            Ok(Value::Float(f)) => (true, scaled(weight, f), String::new()),
            Ok(other) => (false, 0, format!("gives {}, not a flag or a number", kind(&other))),
            Err(e) => (false, 0, said(&e)),
        };
        score = score.saturating_add(points);
        held(&rule.text, ok, points, detail);
    }
    let runs = |peer: &Peer| match peer {
        Peer::Task(t) => peers.get(t) == Some(&c.worker),
        Peer::Worker(w) => *w == c.worker,
    };
    let peer_weight = i64::from(Placement::PEER_WEIGHT);
    for (list, rule, sign) in [(&placement.near, "near", 1), (&placement.avoid, "avoid", -1)] {
        for peer in list {
            let here = runs(peer);
            let points = if here { peer_weight.saturating_mul(sign) } else { 0 };
            score = score.saturating_add(points);
            held(&format!("{rule} {}", peer_name(peer)), here == (sign > 0), points, String::new());
        }
    }
    Suggestion { worker: c.worker, name: c.name.clone(), fits, score, reasons }
}

/// Why a rule did not evaluate, in at most [`DETAIL_MAX`] bytes: a message may echo the
/// facts it read.
fn said(e: &cel::ExecutionError) -> String {
    let text = e.to_string();
    let mut end = text.len().min(DETAIL_MAX);
    while !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    text.get(..end).unwrap_or_default().to_owned()
}

/// The longest reason a rule gives, in bytes.
const DETAIL_MAX: usize = 512;

fn not_judged() -> String {
    "not judged: placement ran past its time; make the rules simpler".to_owned()
}

fn peer_name(peer: &Peer) -> String {
    match peer {
        Peer::Task(t) => format!("task {t}"),
        Peer::Worker(w) => format!("worker {w}"),
    }
}

/// `weight` times `by`, rounded, within `i64`.
fn scaled(weight: i64, by: f64) -> i64 {
    #[expect(clippy::cast_precision_loss, reason = "a weight is an i32, exact in an f64")]
    let product = (weight as f64 * by).round();
    if product.is_nan() {
        0
    } else {
        #[expect(clippy::cast_possible_truncation, reason = "`as` saturates at i64's bounds")]
        let points = product as i64;
        points
    }
}

/// Load per cpu, for a tie between otherwise equal workers; unknown sorts last.
fn busy(facts: &Facts) -> f64 {
    let load = match facts.get("load") {
        Some(Fact::Float(l)) => *l,
        #[expect(clippy::cast_precision_loss, reason = "a load average is small")]
        Some(Fact::Int(l)) => *l as f64,
        _ => return f64::MAX,
    };
    let cpus = match facts.get("cpus") {
        #[expect(clippy::cast_precision_loss, reason = "a cpu count is small")]
        Some(Fact::Int(n)) if *n > 0 => *n as f64,
        _ => 1.0,
    };
    load / cpus
}

const fn kind(value: &Value) -> &'static str {
    match value {
        Value::List(_) => "a list",
        Value::Map(_) => "a map",
        Value::String(_) => "a string",
        Value::Null => "null",
        Value::Int(_) | Value::UInt(_) | Value::Float(_) => "a number",
        _ => "something else",
    }
}

/// A CEL context holding `facts`, each by its name and all in `facts`.
fn context(facts: &Facts) -> Context<'static, 'static> {
    let mut ctx = Context::default();
    let mut all = HashMap::with_capacity(facts.len());
    for (name, fact) in facts {
        let value = value(fact);
        all.insert(name.clone(), value.clone());
        ctx.add_variable_from_value(name.clone(), value);
    }
    ctx.add_variable_from_value("facts", all);
    ctx
}

fn value(fact: &Fact) -> Value {
    match fact {
        Fact::Bool(b) => Value::Bool(*b),
        Fact::Int(n) => Value::Int(*n),
        Fact::Float(f) => Value::Float(*f),
        Fact::Text(t) => Value::String(Arc::new(t.clone())),
        Fact::List(items) => Value::List(Arc::new(items.iter().map(value).collect())),
        Fact::Map(parts) => {
            let map: HashMap<String, Value> =
                parts.iter().map(|(k, v)| (k.clone(), value(v))).collect();
            map.into()
        }
    }
}

#[cfg(test)]
mod tests;
