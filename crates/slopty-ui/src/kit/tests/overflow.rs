//! Lints as tests for how text meets the edge of its room (`docs/decisions/ui.md`, "How
//! surfaces adapt to their room"): text that is cut ends in an ellipsis or fades, never
//! mid-glyph; an ellipsis is the text's own, not a box's around a `ChromeText`; and a chip of
//! words that never shrinks has a bound.

use super::{chrome_files, own_chain};

/// The `div()` chains of the chrome, each as `(file, line, its own calls, its whole text)`,
/// squeezed of whitespace.
fn chains() -> Vec<(String, usize, String, String)> {
    let mut out = Vec::new();
    for (file, lines) in chrome_files() {
        let code: Vec<String> = lines.iter().map(|(_, l)| l.split_whitespace().collect()).collect();
        let joined = code.concat();
        let mut at = 0_usize;
        for ((line_no, _), squeezed) in lines.iter().zip(&code) {
            let here = at;
            at = at.saturating_add(squeezed.len());
            for (found, _) in squeezed.match_indices("div()") {
                let rest = joined.get(here.saturating_add(found)..).unwrap_or_default();
                out.push((file.clone(), *line_no, own_chain(rest), whole_chain(rest)));
            }
        }
    }
    out
}

/// The chain at the start of `code` with its arguments, to where it ends.
fn whole_chain(code: &str) -> String {
    let mut depth = 0_u32;
    let mut out = String::new();
    for c in code.chars() {
        match c {
            ')' | ',' | ';' if depth == 0 => break,
            '(' => depth = depth.saturating_add(1),
            ')' => depth = depth.saturating_sub(1),
            _ => {}
        }
        out.push(c);
    }
    out
}

/// Whether a chain ends its own text in an ellipsis.
fn ellipsizes(own: &str) -> bool {
    [".text_ellipsis()", ".truncate()", ".text_overflow("].iter().any(|c| own.contains(c))
}

/// The children a chain is handed that are words outright: a literal, a `format!`, a
/// `SharedString`, or a `ChromeText` that does not fill its room.
fn holds_words(whole: &str) -> bool {
    whole.split(".child(").skip(1).any(|child| {
        let first = child.split(").child(").next().unwrap_or_default();
        let fills = first.contains(".fill()") || first.contains(".fill_from_start()");
        child.starts_with('"')
            || child.starts_with("format!(")
            || child.starts_with("SharedString::from(")
            || (child.starts_with("ChromeText::new(") && !fills)
    })
}

/// Words cut at the box's edge: kept to one line and clipped, with no ellipsis of their own.
fn cut_mid_glyph(own: &str, whole: &str) -> Option<&'static str> {
    (own.contains(".whitespace_nowrap()")
        && own.contains(".overflow_hidden()")
        && !ellipsizes(own)
        && holds_words(whole))
    .then_some("words clipped mid-glyph; end them in an ellipsis (`ChromeText::fill`, `truncate`) or fade them (`kit::fit_label`)")
}

/// An ellipsis asked of a box around a `ChromeText`, which lays out its own words and never
/// sees it: the words are clipped under it.
fn ellipsis_around_chrome_text(own: &str, whole: &str) -> Option<&'static str> {
    (ellipsizes(own) && whole.contains(".child(ChromeText::new("))
        .then_some("an ellipsis around a `ChromeText`, which ignores it; use `ChromeText::fill`")
}

/// A chip of words that never shrinks and has no bound: it takes its whole width whatever the
/// room, and what is beside it gives way to nothing.
fn unbounded_chip(own: &str, whole: &str) -> Option<&'static str> {
    (own.contains(".flex_none()")
        && !own.contains(".max_w(")
        && whole.split(".child(").skip(1).any(|child| {
            let first = child.split(").child(").next().unwrap_or_default();
            child.starts_with("ChromeText::new(") && !first.contains(".fill")
        }))
    .then_some(
        "a chip of words that never shrinks; bound it (`max_w`) or put it in a `kit::priority_row`",
    )
}

#[test]
fn the_overflow_checks_know_a_cut_from_an_ellipsis() {
    let clipped = "div().overflow_hidden().whitespace_nowrap().child(\"words\")";
    assert!(cut_mid_glyph(&own_chain(clipped), clipped).is_some());
    let ended = "div().overflow_hidden().whitespace_nowrap().text_ellipsis().child(\"words\")";
    assert!(cut_mid_glyph(&own_chain(ended), ended).is_none());
    let filled = "div().overflow_hidden().whitespace_nowrap().child(ChromeText::new(t,s,k).fill())";
    assert!(cut_mid_glyph(&own_chain(filled), filled).is_none(), "its own ellipsis");
    let row = "div().overflow_hidden().whitespace_nowrap().child(lead).child(name)";
    assert!(cut_mid_glyph(&own_chain(row), row).is_none(), "a row of things, not words");

    let around = "div().truncate().child(ChromeText::new(t,s,k))";
    assert!(ellipsis_around_chrome_text(&own_chain(around), around).is_some());
    assert!(ellipsis_around_chrome_text(&own_chain(filled), filled).is_none());

    let chip = "div().flex_none().child(icon).child(ChromeText::new(name,s,k))";
    assert!(unbounded_chip(&own_chain(chip), chip).is_some());
    let bounded = "div().flex_none().max_w(px(m)).child(ChromeText::new(name,s,k).fill())";
    assert!(unbounded_chip(&own_chain(bounded), bounded).is_none());
}

/// Run `check` over the chrome's chains, outside `awaiting` (files whose owners move them onto
/// the rule with their next change, each named with what waits).
fn flagged_chains(
    awaiting: &[&str],
    check: impl Fn(&str, &str) -> Option<&'static str>,
) -> Vec<String> {
    chains()
        .into_iter()
        .filter(|(file, ..)| !awaiting.iter().any(|f| file.ends_with(f)))
        .filter_map(|(file, line, own, whole)| {
            check(&own, &whole).map(|why| format!("{file}:{line}: {why}"))
        })
        .collect()
}

/// Words that do not fit end in an ellipsis or fade; nothing is clipped mid-glyph.
#[test]
fn no_words_are_cut_mid_glyph() {
    const AWAITING: [&str; 6] = [
        "slopty-ui/src/workspace/readouts.rs",
        "slopty-ui/src/workspace/tile.rs",
        "slopty-ui/src/workspace/miniature.rs",
        "slopty-ui/src/workspace/strip.rs",
        "slopty-ui/src/terminal/view.rs",
        "slopty-ui/src/project/view.rs",
    ];
    let wrong = flagged_chains(&AWAITING, cut_mid_glyph);
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// A `ChromeText` ends its own words: an ellipsis asked of the box around it does nothing.
#[test]
fn no_ellipsis_is_asked_around_chrome_text() {
    const AWAITING: [&str; 1] = ["slopty-ui/src/workspace/tile.rs"];
    let wrong = flagged_chains(&AWAITING, ellipsis_around_chrome_text);
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// A chip of words that never shrinks has a bound, or stands in a priority row.
#[test]
fn a_chip_of_words_has_a_bound() {
    const AWAITING: [&str; 1] = ["slopty-ui/src/workspace/tile.rs"];
    let wrong = flagged_chains(&AWAITING, unbounded_chip);
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}
