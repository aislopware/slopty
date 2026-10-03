//! A project's budget as a person writes it and reads it back.
//!
//! `usd=50` caps the estimated cost in dollars, and `five-hour=80%` caps a plan window at a
//! share of it. `none` takes the budget away (`docs/decisions/projects.md`, "A project may
//! have a budget per meter").

use std::collections::BTreeMap;

use slopty_proto::project::{Budget, Spend};

use crate::ToolError;

/// The words that take a budget away.
pub const NONE: &str = "none";

/// The budget `specs` write, each `meter=cap`: dollars for `usd`, a percent for a plan window.
/// `none` alone is the empty budget, which the server reads as no budget.
///
/// # Errors
///
/// When a spec is not `meter=cap`, a cap is not a number above nothing, a window's is over
/// 100 %, or a meter is named twice.
pub fn parse(specs: &[String]) -> Result<Budget, ToolError> {
    if let [only] = specs
        && only.trim() == NONE
    {
        return Ok(Budget::default());
    }
    let mut caps = BTreeMap::new();
    for spec in specs {
        let (meter, cap) = spec.split_once('=').ok_or_else(|| {
            ToolError::invalid(format!(
                "{spec:?} is not meter=cap: usd=50 caps the estimated cost, five-hour=80% a plan \
                 window"
            ))
        })?;
        let meter = meter.trim();
        let cap = Budget::cap_of(meter, cap)
            .ok_or_else(|| ToolError::invalid(format!("{spec:?}: {}", wanted(meter))))?;
        if caps.insert(meter.to_owned(), cap).is_some() {
            return Err(ToolError::invalid(format!("{meter} is capped twice")));
        }
    }
    Ok(Budget(caps))
}

fn wanted(meter: &str) -> &'static str {
    if meter == Budget::USD {
        "the cap is dollars above nothing, at most to the cent (12.50)"
    } else {
        "the cap is a share of the window above nothing and at most 100%, to the hundredth (80%)"
    }
}

/// How `spend` stands against `budget`, a meter at a time, fullest first: `usd $3.50 of $10.00
/// (35%)`. The figures are the agents' own estimates.
#[must_use]
pub fn text(budget: &Budget, spend: &Spend) -> String {
    let shares = budget.against(spend);
    let mut lines: Vec<(u64, String)> = budget
        .0
        .iter()
        .map(|(meter, cap)| {
            let used = if meter == Budget::USD {
                spend.cost_micro_usd
            } else {
                spend.windows.get(meter).copied().map_or(0, u64::from)
            };
            let share = shares.iter().find(|(m, _)| m == meter).map_or(0, |(_, s)| *s);
            let line = format!(
                "{meter} {} of {} ({}%)",
                Budget::figure(meter, used),
                Budget::figure(meter, *cap),
                share / 100
            );
            (share, line)
        })
        .collect();
    lines.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let mut said: Vec<String> = lines.into_iter().map(|(_, line)| line).collect();
    if said.is_empty() {
        said.push(NONE.to_owned());
    }
    said.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn specs(all: &[&str]) -> Vec<String> {
        all.iter().map(|s| (*s).to_owned()).collect()
    }

    /// Dollars to the cent and a window's percent to the hundredth are read; anything else,
    /// a window over its whole, nothing at all or a meter twice is refused with what it
    /// wants; `none` alone takes the budget away.
    #[test]
    fn a_budget_is_written_in_dollars_and_percents() {
        let budget = parse(&specs(&["usd=12.5", "five-hour=80%", "seven-day=33.33"])).unwrap();
        assert_eq!(
            budget.0,
            BTreeMap::from([
                ("five-hour".to_owned(), 8_000),
                ("seven-day".to_owned(), 3_333),
                (Budget::USD.to_owned(), 12_500_000),
            ])
        );
        assert_eq!(parse(&specs(&["usd=$50"])).unwrap().0[Budget::USD], 50_000_000);
        assert_eq!(parse(&specs(&["none"])).unwrap(), Budget::default());
        for bad in ["usd", "usd=0", "usd=1.234", "usd=-3", "five-hour=101%", "usd=ten", "x="] {
            let refused = parse(&specs(&[bad])).unwrap_err().to_string();
            assert!(refused.contains(bad.split('=').next().unwrap_or(bad)), "{bad}: {refused}");
        }
        assert!(parse(&specs(&["usd=1", "usd=2"])).unwrap_err().to_string().contains("twice"));
    }

    /// The budget reads back a meter at a time, fullest first, each with its figure, its cap
    /// and its share.
    #[test]
    fn a_budget_reads_back_fullest_first() {
        let budget = parse(&specs(&["usd=10", "five-hour=80"])).unwrap();
        let spend = Spend {
            cost_micro_usd: 3_500_000,
            windows: BTreeMap::from([("five-hour".to_owned(), 6_000)]),
        };
        assert_eq!(
            text(&budget, &spend),
            "five-hour 60.00% of 80.00% (75%), usd $3.50 of $10.00 (35%)"
        );
        assert_eq!(text(&Budget::default(), &spend), NONE);
    }
}
