//! A project's budget as a person writes it and reads it back.
//!
//! `five-hour=80%` caps a plan window at a share of it, and `none` takes the budget away
//! (`docs/decisions/projects.md`, "A project may have a budget per plan window").

use std::collections::BTreeMap;

use slopty_proto::project::{Budget, Spend};

use crate::ToolError;

/// The words that take a budget away.
pub const NONE: &str = "none";

/// The budget `specs` write, each `window=cap` with the cap a percent of the window. `none`
/// alone is the empty budget, which the server reads as no budget.
///
/// # Errors
///
/// When a spec is not `window=cap`, a cap is not a share above nothing and at most 100 %, or a
/// window is named twice.
pub fn parse(specs: &[String]) -> Result<Budget, ToolError> {
    if let [only] = specs
        && only.trim() == NONE
    {
        return Ok(Budget::default());
    }
    let mut caps = BTreeMap::new();
    for spec in specs {
        let (window, cap) = spec.split_once('=').ok_or_else(|| {
            ToolError::invalid(format!(
                "{spec:?} is not window=cap: five-hour=80% caps the plan's five-hour window"
            ))
        })?;
        let window = window.trim();
        let cap = Budget::cap_of(cap).ok_or_else(|| {
            ToolError::invalid(format!(
                "{spec:?}: the cap is a share of the window above nothing and at most 100%, to \
                 the hundredth (80%)"
            ))
        })?;
        if caps.insert(window.to_owned(), cap).is_some() {
            return Err(ToolError::invalid(format!("{window} is capped twice")));
        }
    }
    Ok(Budget(caps))
}

/// How `spend` stands against `budget`, a window at a time, fullest first: `five-hour 60.00%
/// of 80.00% (75%)`.
#[must_use]
pub fn text(budget: &Budget, spend: &Spend) -> String {
    let shares = budget.against(spend);
    let mut lines: Vec<(u64, String)> = budget
        .0
        .iter()
        .map(|(window, cap)| {
            let used = spend.windows.get(window).copied().map_or(0, u64::from);
            let share = shares.iter().find(|(m, _)| m == window).map_or(0, |(_, s)| *s);
            let line = format!(
                "{window} {} of {} ({}%)",
                Budget::figure(used),
                Budget::figure(*cap),
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

    /// A window's percent to the hundredth is read; anything else, a window over its whole,
    /// dollars, nothing at all or a window twice is refused with what it wants; `none` alone
    /// takes the budget away.
    #[test]
    fn a_budget_is_written_in_percents_of_plan_windows() {
        let budget = parse(&specs(&["five-hour=80%", "seven-day=33.33"])).unwrap();
        assert_eq!(
            budget.0,
            BTreeMap::from([("five-hour".to_owned(), 8_000), ("seven-day".to_owned(), 3_333)])
        );
        assert_eq!(parse(&specs(&["none"])).unwrap(), Budget::default());
        for bad in ["five-hour", "five-hour=0", "five-hour=101%", "usd=$5", "x=ten", "x="] {
            let refused = parse(&specs(&[bad])).unwrap_err().to_string();
            assert!(refused.contains(bad.split('=').next().unwrap_or(bad)), "{bad}: {refused}");
        }
        let twice = parse(&specs(&["five-hour=1", "five-hour=2"])).unwrap_err();
        assert!(twice.to_string().contains("twice"));
    }

    /// The budget reads back a window at a time, fullest first, each with its figure, its cap
    /// and its share.
    #[test]
    fn a_budget_reads_back_fullest_first() {
        let budget = parse(&specs(&["seven-day=50", "five-hour=80"])).unwrap();
        let spend = Spend {
            windows: BTreeMap::from([
                ("five-hour".to_owned(), 6_000),
                ("seven-day".to_owned(), 1_000),
            ]),
        };
        assert_eq!(
            text(&budget, &spend),
            "five-hour 60.00% of 80.00% (75%), seven-day 10.00% of 50.00% (20%)"
        );
        assert_eq!(text(&Budget::default(), &spend), NONE);
    }
}
