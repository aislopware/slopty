//! The chrome's source lints, as tests: every crate the chrome is drawn in, read line by line
//! for what the theme tokens and the kit rule out (a literal colour, a pad off the spacing, a
//! shadow of its own, chrome text not in sentence case). Each check's own test proves it knows a
//! fault from a token, and its sweep runs over [`CHROME`], the kit included.

use slopty_kit::kit::*;

mod overflow;

/// Every crate the chrome is drawn in: the kit, the terminal, the views, and the app's own sheets.
const CHROME: [&str; 4] =
    ["slopty-kit/src", "slopty-terminal/src", "slopty-ui/src", "slopty-app/src"];

/// The chrome of the kit, the terminal and the views, without the app's.
fn ui_lines() -> Vec<(String, usize, String)> {
    ["slopty-kit/src", "slopty-terminal/src", "slopty-ui/src"]
        .into_iter()
        .flat_map(chrome_lines)
        .collect()
}

/// Chrome text is sentence case, whatever it is doing: a label, a button's accessible name,
/// a field's placeholder and an empty state are all written the same way. The stream HUD and
/// the agent pill are readouts of a number or a state, not chrome, and stay lowercase.
#[test]
fn chrome_text_is_sentence_case() {
    let chrome = [
        find::PLACEHOLDER,
        find::REPLACE_PLACEHOLDER,
        find::MATCH_CASE,
        find::WHOLE_WORD,
        find::REGEX,
        find::REPLACE_ALL,
        find::NO_MATCHES,
        find::BAD_PATTERN,
        crate::workspace::TAKE_OVER,
        crate::workspace::TAKE,
        crate::workspace::MUTE,
        crate::workspace::ATTACHING,
        crate::workspace::PAUSED,
        crate::workspace::OPENING,
        crate::workspace::READING,
        crate::file::CHANGED_ON_DISK,
        crate::file::RELOAD,
        crate::file::OVERWRITE,
        crate::folder::EMPTY_FOLDER,
        crate::folder::NOT_A_FOLDER,
        crate::folder::CANNOT_LIST,
        crate::file::TOO_LARGE,
        crate::file::NOT_TEXT,
        crate::file::CANNOT_READ,
        crate::file::OPEN_IN_EDITOR,
        crate::file::GO_TO_LINE,
        crate::file::OPEN_IN_PAGER,
        crate::screen::TRACKPAD_MODE,
        crate::folder::ENCLOSING_FOLDER,
        crate::settings_form::PRESS_KEYS,
        crate::settings_form::NO_KEYS,
        crate::settings_form::ADD_KEYS,
        crate::settings_form::RESET_KEYS,
        crate::workspace::COPY_COMMAND,
        crate::palette::NO_COMMAND_MATCHES,
        crate::palette::COMMAND_HINT,
        crate::picker::FILTER_PLACEHOLDER,
        crate::picker::NOTHING_MATCHES,
        crate::picker::NOTHING_TO_JUMP_TO,
        crate::picker::LOADING_WINDOWS,
        slopty_terminal::BACK_TO_LIVE,
        slopty_terminal::COPY_MODE,
        slopty_terminal::COPY_MODE_DONE,
        crate::workspace::RECONNECTING,
        crate::workspace::SESSION_ENDED,
        crate::workspace::CLOSE_TILE,
        crate::workspace::EMPTY_QUESTION,
        crate::workspace::NO_WORKERS,
        crate::workspace::NO_WORKERS_NEXT,
        crate::workspace::ADD_WORKER,
        crate::workspace::CHROME_WORDS[0],
        crate::screen::waiting_text(slopty_proto::screen::SourceState::Idle),
        crate::screen::waiting_text(slopty_proto::screen::SourceState::Live),
    ];
    for text in chrome {
        let first = text.chars().next().unwrap_or(' ');
        assert!(first.is_uppercase(), "chrome text starts lowercase: {text:?}");
        let rest: String = text.chars().skip(1).collect();
        assert!(!rest.contains(char::is_uppercase), "chrome text is title case: {text:?}");
    }
}

/// The chrome under `dir`, without its test modules: every line of Rust up to a
/// `#[cfg(test)]` over a `mod`, with its file and one-based line number. A test-only
/// accessor gated on its own does not end the file's scan.
fn chrome_lines(dir: &str) -> Vec<(String, usize, String)> {
    fn walk(dir: &std::path::Path, out: &mut Vec<(String, usize, String)>) {
        let entries = std::fs::read_dir(dir).expect("the crate's sources are readable");
        for entry in entries.flatten() {
            let path = entry.path();
            // Test modules in files of their own are not chrome either, nor are these lints.
            let is_tests = path.file_stem().is_some_and(|n| n == "tests" || n == "lints");
            if is_tests {
                continue;
            }
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let text = std::fs::read_to_string(&path).expect("a source file reads");
                let name = path.display().to_string();
                let lines: Vec<&str> = text.lines().collect();
                for (ix, line) in lines.iter().enumerate() {
                    let next = lines.get(ix.saturating_add(1)).copied().unwrap_or_default();
                    if line.trim() == "#[cfg(test)]" && next.trim_start().starts_with("mod ") {
                        break;
                    }
                    out.push((name.clone(), ix.saturating_add(1), (*line).to_owned()));
                }
            }
        }
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the crate sits in the workspace")
        .join(dir);
    let mut out = Vec::new();
    walk(&root, &mut out);
    assert!(!out.is_empty(), "found no sources under {dir}");
    out
}

/// A padding, margin or gap written as a literal rather than taken from `theme.spacing`.
///
/// Only these: a literal `w`/`h`/`size` is a measurement of something real (a hairline, an
/// icon box, a panel that has to be some width), while a literal pad is a rhythm chosen in
/// one place and nowhere else, which is how a scale of 2/4/8/12/16/24 quietly becomes a
/// scale of every number. Returns the offending call for the message.
fn literal_spacing(line: &str) -> Option<String> {
    const PAD: [&str; 13] = [
        ".p(", ".px(", ".py(", ".pt(", ".pb(", ".pl(", ".pr(", ".m(", ".mx(", ".my(", ".gap(",
        ".gap_x(", ".gap_y(",
    ];
    PAD.iter()
        .find(|call| {
            line.split(*call).skip(1).any(|rest| {
                rest.strip_prefix("px(")
                    .is_some_and(|n| n.starts_with(|c: char| c.is_ascii_digit()))
            })
        })
        .map(|call| format!("`{call}px(N)`"))
}

/// A floating layer lifted by hand: a shadow of its own, or a scrim that is not the
/// elevation's.
fn own_elevation(line: &str) -> Option<&'static str> {
    if line.trim_start().starts_with("//") {
        return None;
    }
    let named = [".shadow_", "::shadow_"]
        .iter()
        .any(|call| line.split(call).skip(1).any(|rest| !rest.starts_with("none")));
    if named || line.contains(".shadow(") {
        return Some("a shadow of its own, not `kit::elevate` or `kit::dialog`");
    }
    let dim = ["alpha::SCRIM", "elevation.scrim", "elevation.shade"];
    dim.iter().any(|token| line.contains(token)).then_some("a scrim of its own, not `kit::scrim`")
}

#[test]
fn the_elevation_check_knows_a_lift_from_a_token() {
    assert!(own_elevation(".bg(hsla(s.ground)).shadow_sm()").is_some());
    assert!(own_elevation(".shadow(vec![shadow])").is_some());
    assert!(own_elevation(".bg(hsla_alpha(s.ground, alpha::SCRIM))").is_some());
    assert!(own_elevation("kit::elevate(div(), theme).rounded(px(r))").is_none());
    assert!(own_elevation(".bg(kit::scrim(theme))").is_none());
    assert!(own_elevation(".when(floats, gpui::Styled::shadow_sm)").is_some());
    assert!(own_elevation(".shadow_none()").is_none(), "taking a shadow off is fine");
    assert!(own_elevation("/// no `.shadow_sm()` here").is_none(), "a comment");
    let dimmed = ".bg(hsla_alpha(theme.elevation.shade, alpha::DIM))";
    assert!(own_elevation(dimmed).is_some(), "the shade dimmed by hand");
    assert!(own_elevation(".bg(hsla_alpha(s.ground, t.elevation.scrim))").is_some());
}

/// What floats wears the floating elevation and what rests the resting one: [`elevate`],
/// [`card`], [`rests`] and [`scrim`] are the only places a shadow, a rim or a modal's
/// dim is chosen, so an overlay cannot sit below the content again nor a card grow a
/// shadow of its own.
#[test]
fn a_floating_layer_wears_the_one_elevation() {
    let mut wrong = Vec::new();
    for dir in CHROME {
        for (file, line_no, line) in chrome_lines(dir) {
            // The kit's own files draw the elevations and finishes the rest call.
            let waived = file.ends_with("slopty-kit/src/kit.rs")
                || file.ends_with("slopty-kit/src/kit/go.rs");
            if let Some(why) = own_elevation(&line).filter(|_| !waived) {
                wrong.push(format!("{file}:{line_no}: {why}"));
            }
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// The sweeps read the kit and the terminal as well as the views and the app: the kit's own lift,
/// the one place a scrim may be chosen, is what the elevation check flags in
/// `slopty-kit/src/kit.rs` once its waiver is left out, and every root yields its `lib.rs`. A
/// fault written anywhere in either fails the sweeps the same way (the terminal's own:
/// `overflow::the_sweeps_flag_the_terminal`).
#[test]
fn the_sweeps_read_the_kit_and_the_terminal() {
    let mut read = CHROME.into_iter().flat_map(chrome_lines);
    let lifted = read.any(|(file, _, line)| {
        file.ends_with("slopty-kit/src/kit.rs") && own_elevation(&line).is_some()
    });
    assert!(lifted, "the kit's own scrim is read, and flagged without its waiver");
    for dir in CHROME {
        assert!(chrome_lines(dir).iter().any(|(file, ..)| file.ends_with("lib.rs")), "{dir}");
    }
}

/// Chrome lines under [`CHROME`] that `wrong` flags, outside
/// `kit.rs` and the files in `awaiting` (call sites the next wave moves onto the kit), as
/// `file:line: why`.
fn flagged(awaiting: &[&str], wrong: impl Fn(&str) -> Option<&'static str>) -> Vec<String> {
    CHROME
        .into_iter()
        .flat_map(chrome_lines)
        .filter(|(file, ..)| {
            !file.ends_with("slopty-kit/src/kit.rs") && !awaiting.iter().any(|f| file.ends_with(f))
        })
        .filter_map(|(file, line_no, line)| {
            wrong(&line).map(|why| format!("{file}:{line_no}: {why}: {}", line.trim()))
        })
        .collect()
}

/// A diff's size spelled by hand: a minus sign written before a formatted figure, or drawn
/// apart from its figure. A diff's own sign column (`"\u{2212}"` beside its tone) is not a
/// size, and neither is a comment.
fn hand_rolled_changes(line: &str) -> Option<&'static str> {
    let signed = literals(line)
        .iter()
        .any(|l| ["\\u{2212}{", "−{", "\\u{2012}{", "‒{"].iter().any(|sign| l.contains(sign)));
    let apart =
        [".child(\"\\u{2212}\")", ".child(\"−\")", ".child(\"\\u{2012}\")", ".child(\"‒\")"]
            .iter()
            .any(|c| line.contains(c));
    (signed || apart).then_some("a diff's size by hand, not `kit::changes`")
}

#[test]
fn the_changes_check_knows_a_size_from_a_sign() {
    assert!(hand_rolled_changes(r#"format!("+{added} \u{2212}{removed}")"#).is_some());
    assert!(hand_rolled_changes(r#"format!("\u{2212}{}", changes.removed)"#).is_some());
    assert!(hand_rolled_changes(r#".child(div().text_color(red).child("\u{2212}"))"#).is_some());
    assert!(hand_rolled_changes(r#"Kind::Removed => (wash, "\u{2212}", s.error),"#).is_none());
    assert!(hand_rolled_changes("/// `+a −r` in the diff's tones").is_none(), "a comment");
    assert!(hand_rolled_changes("kit::changes(theme, added, removed)").is_none());
}

/// A count of changed lines is drawn by [`changes`] (or said by [`changes_text`]): the
/// signs in the diff's tones, the figures quiet, a zero side left out. Written by hand it
/// came out two ways, one of them a red "−0".
#[test]
fn a_diff_size_is_drawn_by_kit_changes() {
    const AWAITING: [&str; 0] = [];
    let wrong = flagged(&AWAITING, hand_rolled_changes);
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// A duration spelled by hand: a format with a figure then seconds (`"{secs} s"`,
/// `"{:.1}s"`), or minutes or hours followed by a second figure (`"{} m {:02} s"`). A
/// latency in milliseconds is a readout, not a duration, and an age ("5m") is its own
/// format; neither is flagged.
fn hand_rolled_duration(line: &str) -> Option<&'static str> {
    let spelled = |text: &String| {
        let chars: Vec<char> = text.chars().collect();
        chars.iter().enumerate().filter(|(_, c)| **c == '}').any(|(ix, _)| {
            let at = |n: usize| chars.get(ix.saturating_add(n)).copied();
            let skip = usize::from(at(1) == Some(' '));
            let unit = at(skip.saturating_add(1));
            let after = at(skip.saturating_add(2));
            let seconds = unit == Some('s') && !after.is_some_and(char::is_alphanumeric);
            let compound = matches!(unit, Some('m' | 'h'))
                && after == Some(' ')
                && at(skip.saturating_add(3)) == Some('{');
            seconds || compound
        })
    };
    literals(line).iter().any(spelled).then_some("a duration spelled by hand, not `kit::duration`")
}

#[test]
#[expect(
    clippy::literal_string_with_formatting_args,
    reason = "the check's cases are format strings as they stand in the source"
)]
fn the_duration_check_knows_a_duration_from_a_readout() {
    assert!(hand_rolled_duration(r#"1..60 => format!("{secs} s"),"#).is_some());
    assert!(hand_rolled_duration(r#"format!("{:.1} s", elapsed.as_secs_f64())"#).is_some());
    assert!(hand_rolled_duration(r#"format!("{} m {:02} s", secs / 60, secs % 60)"#).is_some());
    assert!(hand_rolled_duration(r#"format!("{}h {}m", h, m)"#).is_some());
    assert!(hand_rolled_duration(r#"format!("rtt {:.1} ms", ms(d))"#).is_none(), "latency");
    assert!(hand_rolled_duration(r#"format!("{}m", secs / 60)"#).is_none(), "an age");
    assert!(hand_rolled_duration(r#"format!("{n} steps")"#).is_none());
    assert!(hand_rolled_duration(r#"/// "Worked for 3 m 12 s""#).is_none(), "a comment");
    assert!(hand_rolled_duration("kit::duration(elapsed)").is_none());
}

/// How long something took is said by [`duration`]: three hand-made formats had put
/// "1 m 05 s" beside cargo's "1m 04s" on one line, and "6.0 s" in a header.
#[test]
fn a_duration_is_kit_duration() {
    const AWAITING: [&str; 0] = [];
    let wrong = flagged(&AWAITING, hand_rolled_duration);
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// A pill made by hand: its height grown from a pad at the chrome's zoom, or a tone's
/// faint fill at rest (a hover is not a pill).
fn hand_rolled_pill(line: &str) -> Option<&'static str> {
    if line.trim_start().starts_with("//") {
        return None;
    }
    let squeezed: String = line.split_whitespace().collect();
    if squeezed.contains(".py(px(theme.spacing.xxs*k))") {
        return Some("a pill's height from its pad, not `kit::pill_frame`");
    }
    let fill = squeezed.contains(".bg(hsla_alpha(") && squeezed.contains("alpha::FAINT");
    (fill && !squeezed.contains(".hover(")).then_some("a tone's pill by hand, not `kit::pill`")
}

#[test]
fn the_pill_check_knows_a_pill_from_a_hover() {
    assert!(hand_rolled_pill(".py(px(theme.spacing.xxs * k))").is_some());
    assert!(hand_rolled_pill(".bg(hsla_alpha(color, alpha::FAINT))").is_some());
    let hover = ".hover(move |el| el.bg(hsla_alpha(quiet, alpha::FAINT)))";
    assert!(hand_rolled_pill(hover).is_none(), "a hover fill");
    assert!(hand_rolled_pill("kit::pill(theme, s.warn)").is_none());
    assert!(hand_rolled_pill(".py(px(theme.spacing.xxs))").is_none(), "a hint, unzoomed");
    assert!(hand_rolled_pill("// .py(px(theme.spacing.xxs * k))").is_none(), "a comment");
}

/// A header's pill is [`pill`] (or [`pill_frame`] for its bare words): [`PILL_HEIGHT`]
/// tall whatever its text, so it sits centred in the header rather than filling it.
#[test]
fn a_pill_is_kit_pill() {
    const AWAITING: [&str; 0] = [];
    let wrong = flagged(&AWAITING, hand_rolled_pill);
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// A share written with a space before its sign: "150 %" beside the "34%" of the context
/// and the "↑ 42%" of an upload.
fn spaced_percent(line: &str) -> Option<&'static str> {
    literals(line)
        .iter()
        .any(|l| l.contains("} %"))
        .then_some("a share as \"{n} %\", where chrome writes \"{n}%\"")
}

#[test]
#[expect(
    clippy::literal_string_with_formatting_args,
    reason = "the check's cases are format strings as they stand in the source"
)]
fn the_percent_check_knows_a_share_from_a_modulo() {
    assert!(spaced_percent(r#"format!("{percent:.0} %")"#).is_some());
    assert!(spaced_percent(r#"format!("{percent:.0}%")"#).is_none());
    assert!(spaced_percent("let rest = secs % 60;").is_none(), "arithmetic");
    assert!(spaced_percent(r#"// format!("{n} %")"#).is_none(), "a comment");
}

/// One way to write a share: the figure and the sign together, as macOS and the context
/// readout write it.
#[test]
fn a_percent_sits_against_its_figure() {
    const AWAITING: [&str; 0] = [];
    let wrong = flagged(&AWAITING, spaced_percent);
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// The error's quiet ground is a control's fill only through [`ButtonKind::Destructive`]: drawn
/// here, and where the thread's answer row draws the kinds itself.
#[test]
fn the_destructive_red_is_the_kits() {
    const RULED: [&str; 1] = ["slopty-ui/src/conversation/thread/view.rs"];
    let red = |line: &str| {
        let code = !line.trim_start().starts_with("//");
        (code && line.contains("error_background"))
            .then_some("the destructive ground outside `ButtonKind::Destructive`")
    };
    let wrong = flagged(&RULED, red);
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// Chrome draws no gradient, `kit` included: a gradient is a wash, the first tell of
/// generated UI. Where something scrolls past an edge, the content itself fades per pixel
/// (`gpui::edge_fade`), over whatever lies behind it.
#[test]
fn chrome_draws_no_gradient() {
    let wrong: Vec<String> = CHROME
        .into_iter()
        .flat_map(chrome_lines)
        .filter(|(.., line)| {
            let code = !line.trim_start().starts_with("//");
            code && ["linear_gradient(", "linear_color_stop("].iter().any(|g| line.contains(g))
        })
        .map(|(file, line_no, line)| format!("{file}:{line_no}: a gradient: {}", line.trim()))
        .collect();
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// A field's text inset is [`FIELD_INSET`], gpui-kit's medium field padding, and nothing
/// keeps a copy of it, so what lines up with a field's text cannot drift from it.
#[test]
fn the_field_inset_is_the_kits() {
    const AWAITING: [&str; 0] = [];
    let medium = gpui_kit::component::Size::Medium.input_px();
    assert!((f32::from(medium) - FIELD_INSET).abs() < f32::EPSILON, "{medium:?}");
    let copy = |line: &str| line.contains("const FIELD_INSET").then_some("a second inset");
    let wrong = flagged(&AWAITING, copy);
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// A focused element told by an accent border: the field that turns its hairline blue
/// when it takes the caret. The keyboard's ring is `a11y::tab_stop`'s outline, not a border.
fn accent_focus_border(line: &str) -> bool {
    let squeezed: String = line.split_whitespace().collect();
    let accent = ["hsla(s.accent)", "hsla(theme.surfaces.accent)", "{s.accent}else"];
    squeezed.contains(".border_color(")
        && accent.iter().any(|a| squeezed.contains(a))
        && squeezed.contains("focus")
        && !squeezed.trim_start().starts_with("//")
}

#[test]
fn the_focus_border_check_knows_a_field_from_a_ring() {
    assert!(accent_focus_border(".when(focused, |el| el.border_color(hsla(s.accent)))"));
    assert!(accent_focus_border(".border_color(hsla(if focused { s.accent } else { s.border }))"));
    assert!(!accent_focus_border(".border_color(hsla(if picked { s.accent } else { s.border }))"));
    assert!(!accent_focus_border(".when(focused, |el| el.border_color(hsla(s.border)))"));
    assert!(!accent_focus_border(".when(active, |el| el.border_2().border_color(hsla(s.accent)))"));
}

/// The accent's text tone is never a fill: it is lifted for reading, not for carrying
/// words. Nor is it a focused field's border: a field shows focus by its caret, and the
/// accent is kept for the keyboard's ring, links in prose and the marks that say "live".
#[test]
fn the_accent_text_tone_is_never_a_fill() {
    const AWAITING: [&str; 0] = [];
    let mut wrong = Vec::new();
    for dir in CHROME {
        for (file, line_no, line) in chrome_lines(dir) {
            let squeezed: String = line.split_whitespace().collect();
            let fills =
                [".bg(hsla(s.accent))", ".bg(hsla(theme.surfaces.accent))", "{s.accent}else"];
            if fills.iter().any(|f| squeezed.contains(f)) && squeezed.contains(".bg(") {
                wrong.push(format!("{file}:{line_no}: {line}"));
            }
            let waived = AWAITING.iter().any(|f| file.ends_with(f));
            if accent_focus_border(&line) && !waived {
                wrong.push(format!("{file}:{line_no}: a focused field in the accent: {line}"));
            }
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// A control in the accent: words or a glyph in `accent_ink`, which only the accent's fill
/// carries, so the line paints a button, a ticked box or a key in it.
fn green_control(line: &str) -> Option<&'static str> {
    let code = !line.trim_start().starts_with("//");
    (code && line.contains("accent_ink"))
        .then_some("a control in the accent; a primary or an on state takes `kit::tinted`")
}

#[test]
fn the_green_control_check_knows_a_control_from_a_mark() {
    assert!(green_control(".bg(hsla(s.accent_fill)).text_color(hsla(s.accent_ink))").is_some());
    assert!(green_control("if lit { (s.accent_fill, s.accent_ink) } else { x }").is_some());
    assert!(green_control(".size(px(dot)).rounded_full().bg(hsla(s.accent_fill))").is_none());
    assert!(green_control("kit::tinted(el, theme)").is_none());
    assert!(green_control("// `accent_ink` was the ink").is_none(), "a comment");
}

/// The accent is a mark, never a control's fill: the primary action, a ticked box and an armed
/// key take Zed's tinted accent ([`slopty_kit::kit::tinted`]), its quiet ground inside its
/// line. The accent's fill carries words only on a badge's count (the title bar's) and a
/// switch that is on, which Zed fills with it (the settings form's).
#[test]
fn the_accent_is_never_a_control() {
    const RULED: [&str; 2] =
        ["slopty-ui/src/workspace/titlebar.rs", "slopty-ui/src/settings_form.rs"];
    // Call sites whose owners move them onto `kit::tinted` in their next change.
    const AWAITING: [&str; 0] = [];
    let waived: Vec<&str> = RULED.iter().chain(&AWAITING).copied().collect();
    let wrong = flagged(&waived, green_control);
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// Chrome's sources as whole files of lines, test modules and comments left out.
fn chrome_files() -> Vec<(String, Vec<(usize, String)>)> {
    let mut files: Vec<(String, Vec<(usize, String)>)> = Vec::new();
    let lines = CHROME.into_iter().flat_map(chrome_lines);
    for (file, line_no, line) in lines {
        let line = if line.trim_start().starts_with("//") { String::new() } else { line };
        match files.last_mut() {
            Some((last, lines)) if *last == file => lines.push((line_no, line)),
            _ => files.push((file, vec![(line_no, line)])),
        }
    }
    files
}

/// The methods a `div()` chain at the start of `code` calls on itself, its arguments left
/// out: `div().size(px(6.0)).bg(hsla(x))` is `div().size().bg()`. It ends where the
/// chain does, at a `,`, `;` or the parenthesis that closes what holds it.
fn own_chain(code: &str) -> String {
    let mut depth = 0_u32;
    let mut own = String::new();
    for c in code.chars() {
        match c {
            ')' | ',' | ';' if depth == 0 => break,
            '(' => {
                if depth == 0 {
                    own.push(c);
                }
                depth = depth.saturating_add(1);
            }
            ')' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    own.push(c);
                }
            }
            _ if depth == 0 => own.push(c),
            _ => {}
        }
    }
    own
}

/// A dot: a `div()` filled, rounded to a circle and holding nothing, its own chain read
/// from `code`.
fn is_dot(code: &str) -> bool {
    let own = own_chain(code);
    own.contains(".rounded_full()")
        && own.contains(".bg(")
        && !own.contains(".child(")
        && !own.contains(".children(")
}

#[test]
fn the_dot_check_knows_a_dot_from_a_glyph() {
    assert!(is_dot("div().size(px(6.0)).rounded_full().bg(hsla(s.accent_fill))"));
    assert!(is_dot("div().id(\"d\").size(px(d)).rounded_full().bg(hsla(fill)))"));
    assert!(is_dot("div().size(px(6.0)).bg(hsla(dot)).rounded_full(),.child(word)"));
    assert!(!is_dot("div().rounded_full().bg(hsla(s.warn_fill)).child(count)"), "a badge");
    assert!(!is_dot("div().rounded_full().border(HAIR).child(div().size(px(6.0)))"));
    assert!(!is_dot("div().size(px(6.0)).rounded_full().overflow_hidden()"), "no fill");
}

/// A state is a glyph, never a dot: what needs the person, what finished and what failed
/// each draw their own mark in its fill (`docs/decisions/ui.md`, "State is a glyph"). A dot
/// read as decoration, and a list of them as confetti. The ruled dots are not states.
#[test]
fn a_state_is_a_glyph_not_a_dot() {
    const RULED: [(&str, &str); 3] = [
        ("slopty-ui/src/settings_form.rs", "a switch's knob"),
        ("slopty-ui/src/project/view.rs", "a switch's knob"),
        ("slopty-ui/src/workspace/about.rs", "the brand mark's dots"),
    ];
    let mut wrong = Vec::new();
    for (file, lines) in chrome_files() {
        if RULED.iter().any(|(ruled, _)| file.ends_with(ruled)) {
            continue;
        }
        let code: Vec<String> = lines.iter().map(|(_, l)| l.split_whitespace().collect()).collect();
        let joined = code.concat();
        let mut at = 0_usize;
        for ((line_no, line), squeezed) in lines.iter().zip(&code) {
            let here = at;
            at = at.saturating_add(squeezed.len());
            let Some(found) = squeezed.find(".rounded_full()") else { continue };
            let before = joined.get(..here.saturating_add(found)).unwrap_or_default();
            let Some(div) = before.rfind("div()") else { continue };
            if is_dot(joined.get(div..).unwrap_or_default()) {
                wrong.push(format!("{file}:{line_no}: a state drawn as a dot: {}", line.trim()));
            }
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// A text tone named as a whole token: `s.warn` but not `s.warn_fill`.
fn names_text_tone(call: &str) -> bool {
    ["warn", "error", "success", "accent"].iter().any(|tone| {
        call.split(&format!(".{tone}"))
            .skip(1)
            .any(|after| !after.starts_with(|c: char| c == '_' || c.is_alphanumeric()))
    })
}

/// The call opened on `lines[at]` at `needle`, through its closing parenthesis, squeezed.
fn call_at(lines: &[(usize, String)], at: usize, needle: &str) -> String {
    let text: String = lines.iter().skip(at).take(12).map(|(_, l)| l.as_str()).collect();
    let Some((_, from)) = text.split_once(needle) else { return String::new() };
    let mut depth = 1_u32;
    let mut call = needle.to_owned();
    for c in from.chars() {
        call.push(c);
        match c {
            '(' => depth = depth.saturating_add(1),
            ')' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    break;
                }
            }
            _ => {}
        }
    }
    call.split_whitespace().collect()
}

/// A status glyph in a text tone: `status_icon` or a filled circle or triangle symbol
/// handed `warn`, `error`, `success` or `accent` rather than their fill step.
fn glyph_in_text_tone(call: &str) -> Option<&'static str> {
    let glyph = call.starts_with("status_icon(")
        || (call.starts_with("icon(")
            && (call.contains("CircleFill") || call.contains("TriangleFill")));
    (glyph && names_text_tone(call))
        .then_some("a status glyph in a text tone; it wears the `_fill` step (`Status::ink`)")
}

#[test]
fn the_glyph_tone_check_knows_a_fill_from_a_text_tone() {
    assert!(glyph_in_text_tone("status_icon(theme,status,side,hsla(s.warn))").is_some());
    assert!(glyph_in_text_tone("icon(theme,Symbol::XmarkCircleFill,hsla(s.error))").is_some());
    assert!(glyph_in_text_tone("status_icon(theme,status,side,hsla(s.warn_fill))").is_none());
    assert!(glyph_in_text_tone("icon(theme,Symbol::Bell,hsla(s.warn))").is_none());
    assert!(glyph_in_text_tone("status_icon(theme,status,side,hsla(ink))").is_none());
}

/// A status glyph wears its hue's fill step, never the text tone: the text steps are for
/// words, and a glyph in one read dimmer than its neighbours' (`docs/decisions/ui.md`,
/// "State is a glyph").
#[test]
fn a_status_glyph_wears_its_fill() {
    let mut wrong = Vec::new();
    for (file, lines) in chrome_files() {
        for (ix, (line_no, line)) in lines.iter().enumerate() {
            if line.trim_start().starts_with("//") {
                continue;
            }
            for needle in ["status_icon(", "icon("] {
                let whole = line.split_once(needle).is_some_and(|(before, _)| {
                    !before.ends_with(|c: char| c == '_' || c.is_alphanumeric())
                });
                if !whole {
                    continue;
                }
                if let Some(why) = glyph_in_text_tone(&call_at(&lines, ix, needle)) {
                    wrong.push(format!("{file}:{line_no}: {why}: {}", line.trim()));
                }
            }
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// A machine's or a project's own colour goes on the navigator's head and nowhere else: the
/// identity hues are read only through [`identity_ink`] and [`machine_ink`], and those are
/// called only there.
#[test]
fn identity_colour_stays_on_its_glyph() {
    // The navigator's machine and project heads, and nowhere else (`docs/decisions/ui.md`,
    // "Premium foundations"): a palette row, a tile's header, the breadcrumb and the empty
    // workspace draw a machine in its words' tier.
    const GLYPHS: [&str; 3] = [
        "slopty-ui/src/identity.rs",
        "slopty-ui/src/workspace/navigator.rs",
        // Until the board's task meta goes to its words' tier (lane A, the same ruling).
        "slopty-ui/src/project/view.rs",
    ];
    let mut wrong = Vec::new();
    for (file, line_no, line) in CHROME.into_iter().flat_map(chrome_lines) {
        let code = line.split("//").next().unwrap_or_default();
        let hues = code.contains("surfaces.identity") || code.contains("s.identity");
        let ink = code.contains("identity_ink(") || code.contains("machine_ink(");
        let mine = file.ends_with("slopty-ui/src/identity.rs");
        if (hues && !mine) || (ink && !GLYPHS.iter().any(|f| file.ends_with(f))) {
            wrong.push(format!("{file}:{line_no}: {}", line.trim()));
        }
    }
    assert!(wrong.is_empty(), "an identity colour off its glyph:\n{}", wrong.join("\n"));
}

/// A row's lead handed the muted tone: two tiers under its title.
fn muted_lead(call: &str) -> Option<&'static str> {
    call.contains(".text_muted").then_some(
        "a lead in `text_muted`; it is a tier under its title (`text_secondary`), muted \
         only when away or disabled",
    )
}

/// A lead sits one tier under its title, never two: `text_secondary` at rest and `text`
/// when chosen, so the navigator stops reading as grey specks beside black words
/// (`docs/decisions/ui.md`, "An icon takes its words' size, weight and tier"). Away and
/// disabled are muted through their own words (`Status::Away`), not at the call.
#[test]
fn a_lead_is_never_muted_at_rest() {
    const AWAITING: [&str; 0] = [];
    let mut wrong = Vec::new();
    for (file, lines) in chrome_files() {
        if AWAITING.iter().any(|f| file.ends_with(f)) {
            continue;
        }
        for (ix, (line_no, line)) in lines.iter().enumerate() {
            for needle in ["lead_slot(", "icon_slot("] {
                let whole = line.split_once(needle).is_some_and(|(before, _)| {
                    !before.ends_with(|c: char| c == '_' || c.is_alphanumeric())
                        && !before.trim_end().ends_with("fn")
                });
                if !whole {
                    continue;
                }
                // A fold chevron sits in a lead's slot but leads nothing.
                let call = call_at(&lines, ix, needle);
                if call.contains("disclosure(") {
                    continue;
                }
                if let Some(why) = muted_lead(&call) {
                    wrong.push(format!("{file}:{line_no}: {why}: {}", line.trim()));
                }
            }
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// A colour picked at a call site rather than taken from the theme: a hex or channel
/// literal handed to GPUI. The tokens derive every colour from the content, so a literal
/// is the one colour a custom background cannot move.
fn raw_colour(line: &str) -> Option<&'static str> {
    let code = !line.trim_start().starts_with("//");
    let literal = ["Rgb::hex(", "gpui::rgb(", "gpui::rgba(", "gpui::hsla(", "Rgb { r:"];
    (code && literal.iter().any(|l| line.contains(l)))
        .then_some("a colour literal, not a theme token")
}

#[test]
fn the_colour_check_knows_a_literal_from_a_token() {
    assert!(raw_colour(".bg(gpui::rgb(0x00ff_0000))").is_some());
    assert!(raw_colour("let white = Rgb::hex(0xff_ffff);").is_some());
    assert!(raw_colour(".bg(hsla(s.hover))").is_none());
    assert!(raw_colour("/// not `Rgb::hex(0)`").is_none(), "a comment");
}

/// Chrome paints in the theme's colours only. The waived lines compute a colour from the
/// theme's (a cursor's ink against the cursor the program chose), or are this file's lit
/// edge, which is white by definition.
#[test]
fn chrome_has_no_colour_literals() {
    const RULED: [&str; 1] = ["slopty-app/src/settings.rs"];
    let wrong = flagged(&RULED, raw_colour);
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// Whether `line` fills something with a state wash: the pointer's, the selection's or the
/// press's.
fn washes(line: &str) -> bool {
    let code = line.split("//").next().unwrap_or_default();
    code.contains(".bg(hsla(")
        && [".hover))", ".selected))", ".pressed))"].iter().any(|w| code.contains(w))
}

/// Whether a wash on `line`, after `before`, is laid in a state: inside a `hover` or `active`
/// closure, or under a condition (a `when`, an `if`) that names the state.
fn in_a_state(line: &str, before: &[&str]) -> bool {
    const STATES: [&str; 4] = [".hover(", ".active(", ".when(", "group_hover("];
    STATES.iter().any(|s| line.contains(s))
        || before.iter().any(|l| l.contains(".when(") || l.trim_start().starts_with("if "))
}

#[test]
fn the_wash_check_knows_a_state_from_a_surface() {
    assert!(washes("            .bg(hsla(s.hover))"));
    assert!(washes(".bg(hsla(theme.surfaces.selected))"));
    assert!(!washes(".bg(hsla(s.well))"), "a well");
    assert!(!washes("// .bg(hsla(s.hover)) once filled it"), "a comment");
    assert!(in_a_state(".hover(move |el| el.bg(hsla(s.hover)))", &[]));
    assert!(in_a_state("el.bg(hsla(s.hover))", &["        if shown {"]));
    assert!(in_a_state("el.bg(hsla(s.hover)).aria_selected(true)", &[".when(ix == at, |el| {"]));
    assert!(!in_a_state(".bg(hsla(s.hover))", &[".rounded(px(theme.radii.sm))"]));
}

/// A resting fill is raised or sunk, never a wash. The `hover`, `selected` and `pressed`
/// washes are for the pointer, the selection and the press: laid on something at rest they
/// lift it in dark and sink it in light, so a tray, an option or a field went grey on paper.
/// What rests is [`super::raised`], a well is [`super::inset`] and a field
/// [`super::field`]; this file, which draws those, is not checked.
#[test]
fn a_resting_fill_is_raised_or_sunk() {
    let mut wrong = Vec::new();
    for dir in CHROME {
        let lines = chrome_lines(dir);
        for (ix, (file, no, line)) in lines.iter().enumerate() {
            if file.ends_with("slopty-kit/src/kit.rs") || !washes(line) {
                continue;
            }
            let before: Vec<&str> = lines[ix.saturating_sub(2)..ix]
                .iter()
                .filter(|(f, ..)| f == file)
                .map(|(.., l)| l.as_str())
                .collect();
            if !in_a_state(line, &before) {
                wrong.push(format!("{file}:{no}: a wash at rest: {}", line.trim()));
            }
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// The thread is Delta's document (`docs/decisions/ui.md`, "The thread is Delta's"): type on
/// one plane, with only two things set apart. A call put to the person (a permission, a
/// question, a plan to approve) is Zed's tool card
/// ([`slopty_kit::kit::call_card`] with its [`slopty_kit::kit::card_head`]); code is a well
/// ([`slopty_kit::kit::well`], or the plain [`slopty_kit::kit::inset`] ground). No part raises a
/// card of its own, names the `elevated` plane, which is the floats'
/// ([`slopty_kit::kit::elevate`]), or lays the well's ground by hand.
#[test]
fn the_thread_has_no_card_but_calls_and_wells() {
    const BANNED: [&str; 7] = [
        "kit::raised(",
        "kit::card(",
        "kit::card_part(",
        "raised_part(",
        "elevated",
        "surfaces.well)",
        "s.well)",
    ];
    let mut wrong = Vec::new();
    for (file, no, line) in ui_lines() {
        let code = !line.trim_start().starts_with("//");
        if !code || !file.contains("conversation/thread/") {
            continue;
        }
        if let Some(banned) = BANNED.iter().find(|b| line.contains(*b)) {
            wrong.push(format!("{file}:{no}: `{banned}` in the thread: {}", line.trim()));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// A command in the palette is its words: no line an action runs leads with an icon. Only a
/// step that lists things (an agent, a machine, a folder, a checkout, a repository to clone,
/// a past session) marks its lines with what they are, and only there may `with_icon` follow
/// a command's line.
#[test]
fn a_command_is_its_words() {
    const THINGS: [&str; 2] = ["workspace/agent_start.rs", "workspace/clone_here.rs"];
    let bindings: Vec<gpui::KeyBinding> = Vec::new();
    let lines = crate::workspace::palette_items_of(&crate::keymap::Keymap::default())
        .into_iter()
        .chain(crate::conversation::palette_items(&bindings))
        .chain(crate::project::palette_items(&bindings))
        .chain(crate::review::palette_items(&bindings))
        .chain(crate::folder::folder_palette_items(&bindings))
        .chain(crate::file::editor_palette_items(&bindings));
    let marked: Vec<String> =
        lines.filter(|line| line.icon.is_some()).map(|line| line.label).collect();
    assert!(marked.is_empty(), "commands with an icon: {marked:?}");

    let mut wrong = Vec::new();
    let source = ui_lines();
    for (ix, (file, no, line)) in source.iter().enumerate() {
        if !line.contains(".with_icon(")
            || THINGS.iter().any(|things| file.ends_with(things))
            || file.ends_with("palette.rs")
        {
            continue;
        }
        let chain = source[ix.saturating_sub(4)..=ix].iter().filter(|(f, ..)| f == file);
        if chain.clone().any(|(.., l)| l.contains("PaletteItem::new(")) {
            wrong.push(format!("{file}:{no}: a command given an icon: {}", line.trim()));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// A width set to a share of its parent (`.w(relative(…))`) next to a share, a fraction, a
/// progress or the accent's fill: a progress bar drawn by hand.
fn hand_drawn_progress(lines: &[(String, usize, String)], ix: usize) -> bool {
    const NEAR: [&str; 4] = ["accent_fill", "progress", "fraction", "share"];
    let (file, _, line) = &lines[ix];
    let squeezed: String = line.split_whitespace().collect();
    let code = !line.trim_start().starts_with("//");
    if !code || !(squeezed.contains(".w(relative(") || squeezed.contains(".w(gpui::relative(")) {
        return false;
    }
    lines[ix.saturating_sub(6)..lines.len().min(ix.saturating_add(7))]
        .iter()
        .filter(|(f, ..)| f == file)
        .any(|(.., l)| NEAR.iter().any(|n| l.contains(n)))
}

/// Progress is drawn one way, by [`super::progress`]: a capsule on a quiet track that glides,
/// breathes when its length is unknown, waits before it shows and carries its value. Five
/// had been drawn by hand, three of them square lines along an edge that read as stray rules.
/// The files below draw theirs by hand until lane D's patches move them; each goes from the
/// list as its patch lands.
#[test]
fn a_progress_is_kit_progress() {
    const UNTIL_MOVED: [&str; 3] =
        ["workspace/tile.rs", "workspace/navigator.rs", "project/view.rs"];
    let mut wrong = Vec::new();
    for dir in CHROME {
        let lines = chrome_lines(dir);
        for (ix, (file, no, line)) in lines.iter().enumerate() {
            let own =
                file.ends_with("kit/progress.rs") || UNTIL_MOVED.iter().any(|f| file.ends_with(f));
            if !own && hand_drawn_progress(&lines, ix) {
                wrong.push(format!("{file}:{no}: progress by hand: {}", line.trim()));
            }
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

#[test]
fn the_progress_check_knows_a_bar_from_a_column() {
    let line = |l: &str| ("f.rs".to_owned(), 1, l.to_owned());
    let bar = [line("let share = done / total;"), line("div().w(relative(share))")];
    assert!(hand_drawn_progress(&bar, 1));
    let column = [line("let wide = true;"), line("div().w(relative(0.5))")];
    assert!(!hand_drawn_progress(&column, 1), "a half-width column is no progress");
}

/// A text size, a radius or a control's height written as a literal: a type size off the
/// scale, a corner off the radii, a row off the density.
fn raw_size(line: &str) -> Option<&'static str> {
    let code = !line.trim_start().starts_with("//");
    let squeezed: String = line.split_whitespace().collect();
    let literal = |call: &str| {
        squeezed.split(call).skip(1).any(|rest| {
            rest.strip_prefix("px(")
                .is_some_and(|n| n.starts_with(|c: char| c.is_ascii_digit() && c != '0'))
        })
    };
    if !code {
        return None;
    }
    if literal(".text_size(") {
        return Some("a text size off the type scale");
    }
    [".rounded(", ".rounded_t(", ".rounded_b(", ".rounded_l(", ".rounded_r("]
        .iter()
        .any(|call| literal(call))
        .then_some("a radius off `theme.radii`")
}

#[test]
fn the_size_check_knows_a_literal_from_a_token() {
    assert!(raw_size(".text_size(px(13.0))").is_some());
    assert!(raw_size(".rounded(px(6.0))").is_some());
    assert!(raw_size(".rounded_t(px(0.0))").is_none(), "square is no radius");
    assert!(raw_size(".text_size(px(theme.typography.small()))").is_none());
    assert!(raw_size(".rounded(px(theme.radii.sm * k))").is_none());
    assert!(raw_size("// .text_size(px(13.0))").is_none(), "a comment");
}

/// Chrome's type sizes come from the type scale and its corners from the radii.
#[test]
fn chrome_sizes_come_from_the_scale() {
    const AWAITING: [&str; 0] = [];
    let wrong = flagged(&AWAITING, raw_size);
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// What floats (a menu, a popover, a hint, a toast) is rounded at `radii.lg`, Zed's elevated
/// surfaces' 8; a modal (a dialog, the add-worker panel, through [`modal`]) rounds at
/// `radii.xl`, the same 8, as Zed's modals are floats like the rest. A chip keeps its
/// control's radius.
#[test]
fn a_floating_surface_is_rounded_lg() {
    const SHEETS: [(&str, &str); 4] = [
        ("slopty-kit/src/kit.rs", "let hint = elevate(div(), theme)"),
        ("slopty-kit/src/kit/find.rs", "super::elevate(div(), &theme)"),
        ("slopty-ui/src/conversation/thread/view/aside.rs", ".id(\"thread-aside\")"),
        ("slopty-kit/src/kit/menu.rs", "super::elevate(div(), &theme)"),
    ];
    let lines: Vec<_> = CHROME.into_iter().flat_map(chrome_lines).collect();
    for (sheet, marker) in SHEETS {
        let start = lines
            .iter()
            .position(|(f, _, l)| f.ends_with(sheet) && l.contains(marker))
            .unwrap_or_else(|| panic!("{sheet}: no {marker}"));
        let rounded = lines
            .iter()
            .skip(start)
            .take(30)
            .find(|(f, _, l)| f.ends_with(sheet) && l.contains(".rounded("))
            .unwrap_or_else(|| panic!("{sheet}: {marker} is not rounded"));
        assert!(rounded.2.contains("radii.lg"), "{}:{}: {}", rounded.0, rounded.1, rounded.2);
    }
}

/// The strong weight is for titles: a dialog's or a panel's (`kit::title` or the title
/// size), a page's heading, and the first run's wordmark. A name, a row or a tab that has
/// to stand out says so by its tone, as Zed's do; with 600 on the workspace's name, the
/// worker's name and the overview's names, nothing between a label and the loudest line could
/// say "this one".
#[test]
fn strong_weight_is_for_titles() {
    let mut wrong = Vec::new();
    for dir in CHROME {
        let lines = chrome_lines(dir);
        for (ix, (file, line_no, line)) in lines.iter().enumerate() {
            if !line.contains("STRONG_WEIGHT") || line.trim_start().starts_with("//") {
                continue;
            }
            let near = lines
                .iter()
                .skip(ix.saturating_sub(8))
                .take(12)
                .filter(|(f, ..)| f == file)
                .map(|(_, _, l)| l.as_str());
            let title = [
                "panel_title",
                "page_heading",
                "Role::Heading",
                "APP_NAME",
                "pub fn title(",
                "fn phone_title_role(",
            ];
            if !near.into_iter().any(|l| title.iter().any(|t| l.contains(t))) {
                wrong.push(format!("{file}:{line_no}: the strong weight off a title"));
            }
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// One type scale: a size is a role's ([`slopty_theme::TypeRoles`]) or the chrome's own
/// steps, never the parallel `title`, `heading`, `display` and `task_title` sizes, which
/// drew 15 pt titles where the roles said 16 and let sizes drift a point apart.
#[test]
fn one_type_scale() {
    let gone = ["title()", "heading()", "display()", "task_title()"];
    let mut wrong = Vec::new();
    for dir in CHROME {
        for (file, line_no, line) in chrome_lines(dir) {
            let code = line.split("//").next().unwrap_or_default();
            // The receiver is the typography itself: `typography`, or its usual `ty` and `t`.
            let parallel = gone.iter().any(|f| {
                code.match_indices(&format!(".{f}")).any(|(at, _)| {
                    let receiver = code
                        .get(..at)
                        .unwrap_or_default()
                        .rsplit(|c: char| !(c.is_alphanumeric() || c == '_'))
                        .next()
                        .unwrap_or_default();
                    matches!(receiver, "typography" | "ty" | "t")
                })
            });
            if parallel {
                wrong.push(format!("{file}:{line_no}: {}", line.trim()));
            }
        }
    }
    assert!(wrong.is_empty(), "a size off the one scale:\n{}", wrong.join("\n"));
}

/// Every tile stands on a panel ([`super::panel`]): in the files that draw tiles (the
/// strip, a tile, a tile starting, a tile whose machine is away) nothing paints the
/// content's surface as a ground of its own, square and flush, where the panel rounds,
/// rings and stands it on the canvas. A fill of it that is not a ground (a backing that
/// hides text under the header's controls) says so on the line before.
#[test]
fn a_tile_stands_on_a_panel() {
    let tiles = ["strip.rs", "tile.rs", "starting.rs", "kept_items.rs"]
        .map(|name| std::path::Path::new("workspace").join(name));
    let mut wrong = Vec::new();
    let mut before = String::new();
    for (file, line_no, line) in ui_lines() {
        let drawn = tiles.iter().any(|t| std::path::Path::new(&file).ends_with(t));
        let code = line.split("//").next().unwrap_or_default();
        // A fill whose colour is the content's surface, however it is reached.
        let ground = code.match_indices(".bg(").any(|(at, _)| {
            let fill = code.get(at..).unwrap_or_default();
            let fill = fill.split(".bg(").nth(1).unwrap_or_default();
            let fill = fill.split(").").next().unwrap_or(fill);
            fill.contains("content()") || fill.contains("terminal.bg")
        });
        if drawn && ground && !before.contains("Not a ground") {
            wrong.push(format!("{file}:{line_no}: {}", line.trim()));
        }
        before = line;
    }
    assert!(wrong.is_empty(), "a tile's ground not on a panel:\n{}", wrong.join("\n"));
}

/// Chrome context (a header's directory, the breadcrumb's path, the palette's column, a
/// row's second line) is in the UI face. The mono face is for an address read to judge it
/// (a held-back page's hint) and a diff's size, Delta's (`kit::changes`), both drawn in the
/// kit, and nothing else in the chrome calls for it: the settings file is a file tile's text.
#[test]
fn the_mono_face_is_for_addresses() {
    let uses: Vec<String> = CHROME
        .into_iter()
        .flat_map(chrome_lines)
        .filter(|(_, _, line)| line.contains("mono_family(") && !line.contains("fn mono_family"))
        .map(|(file, line_no, _)| format!("{file}:{line_no}"))
        .collect();
    let kit = |at: &String| at.contains("slopty-kit/src/kit.rs");
    assert!(
        uses.len() == 2 && uses.iter().all(kit),
        "mono outside the addresses and the diff's size: {uses:#?}"
    );
}

/// A tab's title, the title bar's and a pane's alike (and the lone header's, which is a
/// pane's), fades over its last 40 pt while it is cut, as `MonoCode`'s `TabLabel` does,
/// rather than losing its last letters to an ellipsis, and so does a navigator row's title, as
/// Zed's sidebar cuts one: the one fill that fades (`ChromeText::fill_fading`, whose own test
/// holds when it fades).
#[test]
fn a_tabs_title_fades_over_its_end() {
    let fades = |file: &str| {
        ui_lines()
            .into_iter()
            .filter(|(f, _, l)| {
                f.ends_with(file) && l.contains(".fill_fading(px(tab_look::TITLE_FADE))")
            })
            .count()
    };
    assert_eq!(fades("workspace/title_tabs.rs"), 1, "the title tab's");
    assert_eq!(fades("workspace/tile.rs"), 1, "the pane tab's and the lone header's");
    assert_eq!(fades("workspace/navigator.rs"), 1, "a navigator row's");
}

/// A list typed at (the palette, a picker) floats on the bare [`anchor`]; the modals that
/// hold the window until answered (adding a worker) dim it with the [`backdrop`]. Both open
/// at one place. The settings are a page in the panes' place, and dim nothing.
#[test]
fn a_list_floats_and_a_modal_dims() {
    let calls = |file: &str, call: &str| {
        let (ui, app) = (ui_lines(), chrome_lines("slopty-app/src"));
        ui.into_iter().chain(app).any(|(f, _, l)| f.ends_with(file) && l.contains(call))
    };
    for list in ["slopty-ui/src/palette.rs", "slopty-ui/src/picker.rs"] {
        let anchored = calls(list, "kit::anchor(") || calls(list, "palette::list_anchor(");
        assert!(anchored, "{list} lays out on the anchor");
        assert!(!calls(list, "kit::backdrop("), "{list} dims nothing");
    }
    assert!(calls("slopty-app/src/lib.rs", "kit::backdrop("), "adding a worker is a modal");
    assert!(!calls("slopty-ui/src/settings_editor.rs", "kit::backdrop("), "a page");
}

/// A border drawn a full point by hand (`border_1()`, `border_t_1()`, a `Styled::border_b_1`
/// handed on), or a box a point wide filled with a hairline's tint.
fn thick_hairline(line: &str) -> Option<&'static str> {
    if line.trim_start().starts_with("//") {
        return None;
    }
    let squeezed: String = line.split_whitespace().collect();
    let preset = ["border_1()", "border_2()", "Styled::border_1", "Styled::border_2"]
        .iter()
        .any(|p| squeezed.contains(p))
        || ["t", "b", "l", "r", "x", "y"]
            .iter()
            .any(|side| squeezed.contains(&format!("border_{side}_1")));
    let boxed = (squeezed.contains(".h(px(1.0))") || squeezed.contains(".w(px(1.0))"))
        && squeezed.contains(".bg(hsla(")
        && (squeezed.contains("border") || squeezed.contains("stroke"));
    (preset || boxed).then_some("a hairline a point wide, not `kit::HAIR` or `kit::rule`")
}

#[test]
fn the_hairline_check_knows_a_hairline_from_a_point() {
    assert!(thick_hairline(".border_b_1()").is_some());
    assert!(thick_hairline(".border_1().border_color(hsla(s.border))").is_some());
    assert!(thick_hairline(".when(first, gpui::Styled::border_t_1)").is_some());
    assert!(thick_hairline("div().h(px(1.0)).bg(hsla(s.stroke))").is_some());
    assert!(thick_hairline(".border_b(kit::HAIR)").is_none());
    assert!(thick_hairline(".border(px(slopty_theme::stroke::LINE))").is_none(), "an edge");
    assert!(thick_hairline(".border_x_0()").is_none(), "no border");
    assert!(thick_hairline(".when(x, gpui::Styled::border_dashed)").is_none(), "a style");
    assert!(thick_hairline("// .border_1()").is_none(), "a comment");
}

/// Every border chrome draws is a hairline, one device pixel ([`HAIR`]), or a named stroke that
/// marks something (`stroke::LINE`, `stroke::MARK`). At a full point each rule was two
/// pixels on a Retina screen.
#[test]
fn a_chrome_border_is_kit_hair() {
    // Call sites whose owners move them onto `kit::HAIR` in their next change.
    const AWAITING: [&str; 0] = [];
    let wrong: Vec<String> = CHROME
        .into_iter()
        .flat_map(chrome_lines)
        .filter(|(file, ..)| {
            !file.ends_with("slopty-kit/src/kit.rs") && !AWAITING.iter().any(|f| file.contains(f))
        })
        .filter_map(|(file, line_no, line)| {
            thick_hairline(&line).map(|why| format!("{file}:{line_no}: {why}: {}", line.trim()))
        })
        .collect();
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// Every hint keeps the warm group's timing: GPUI builds it at once ([`hint_timing`]) and
/// the [`Hint`] holds the wait, so a tooltip built on GPUI's own half-second delay would
/// wait twice and never join the group.
#[test]
fn a_hint_keeps_the_warm_timing() {
    // Call sites whose owners move them onto `kit::hint_timing` in their next change.
    const AWAITING: [&str; 0] = [];
    let mut wrong = Vec::new();
    for dir in CHROME {
        let lines = chrome_lines(dir);
        for (ix, (file, line_no, line)) in lines.iter().enumerate() {
            if !line.contains(".tooltip(") || AWAITING.iter().any(|f| file.contains(f)) {
                continue;
            }
            let before = ix.checked_sub(1).and_then(|b| lines.get(b)).map(|l| l.2.as_str());
            if !line.contains("hint_timing") && !before.unwrap_or_default().contains("hint_timing")
            {
                wrong.push(format!("{file}:{line_no}: {}", line.trim()));
            }
        }
    }
    assert!(wrong.is_empty(), "a hint on GPUI's own delay:\n{}", wrong.join("\n"));
}

/// The ruling in `docs/decisions/ui.md`, as a check rather than a paragraph: chrome takes
/// its transparencies from `alpha` and wears one elevation. A raw opacity or a second
/// shadow is how a design system becomes a pile of one-offs, so neither compiles.
#[test]
fn chrome_paints_from_the_tokens() {
    // Every step is written `0.` or `1.0`, so a digit after the comma is a raw opacity.
    let raw_alpha = |line: &str| {
        line.contains("hsla_alpha(")
            && (line.contains(", 0.") || line.contains(", 1.0") || line.contains("{ 1.0 }"))
    };
    let mut wrong = Vec::new();
    for dir in CHROME {
        for (file, line_no, line) in chrome_lines(dir) {
            if raw_alpha(&line) {
                wrong.push(format!("{file}:{line_no}: a raw opacity, not an `alpha` step"));
            }
            for other in ["shadow_md()", "shadow_lg()", "shadow_xl()", "shadow_2xl()"] {
                if line.contains(other) {
                    wrong.push(format!("{file}:{line_no}: a second elevation ({other})"));
                }
            }
            if let Some(call) = literal_spacing(&line) {
                wrong.push(format!("{file}:{line_no}: {call} off the spacing scale"));
            }
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// The string literals on a line of Rust, escapes left as written.
fn literals(line: &str) -> Vec<String> {
    let code = line.trim_start();
    if code.starts_with("//") {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut current: Option<String> = None;
    let mut escaped = false;
    for c in code.chars() {
        match current.as_mut() {
            None if c == '"' => current = Some(String::new()),
            None => {}
            Some(text) if escaped => {
                text.push(c);
                escaped = false;
            }
            Some(text) if c == '\\' => {
                text.push(c);
                escaped = true;
            }
            Some(_) if c == '"' => out.extend(current.take()),
            Some(text) => text.push(c),
        }
    }
    out
}

/// A chord written into chrome text: a literal holding a modifier glyph and anything
/// else. A lone glyph is a key cap on the phone's bar, not a chord.
fn literal_chord(line: &str) -> Option<String> {
    literals(line)
        .into_iter()
        .find(|text| text.contains(['⌘', '⌥', '⌃', '⇧']) && text.chars().count() > 1)
}

/// Where a string literal on a line is drawn as chrome text or read out as an accessible
/// name: the first literal after one of these calls. The helpers whose first argument is
/// an element id (`pill`, a text button, a key cap) draw a later literal instead.
const DRAWN: [&str; 8] = [
    ".child(\"",
    "ChromeText::new(\"",
    ".aria_label(\"",
    ".placeholder(\"",
    "muted_line(\"",
    "picture_wait(\"",
    "notice(\"",
    "title(\"",
];
const DRAWN_AFTER_ID: [&str; 5] = ["pill(", "button(", "heading(", "key_cap(", "bar_key("];
/// The helpers whose first argument is the theme draw the first literal after it.
const DRAWN_AFTER_THEME: [&str; 2] = ["kit::label(", "kit::title("];

/// A literal drawn as chrome text that starts lowercase: `"take"` on a pill, `"opening…"`
/// in a body. Sentence case is checked on the text drawn, not only on the names a screen
/// reader reads, so a lowercase label cannot come back unseen.
fn lowercase_label(line: &str) -> Option<String> {
    let code = line.trim_start();
    if code.starts_with("//") {
        return None;
    }
    // A word, not a name: a file (`settings.toml`), a host (`mac-studio`) or a number is
    // written as it is spelled.
    let lower = |text: &String| {
        let word = text.split_whitespace().next().unwrap_or_default();
        word.chars().next().is_some_and(char::is_lowercase)
            && !word.contains(['.', '-', '_', '/'])
            && !word.contains(|c: char| c.is_ascii_digit())
    };
    let drawn = DRAWN.iter().filter_map(|call| {
        line.split_once(call)
            .and_then(|(_, rest)| literals(&format!("\"{rest}")).into_iter().next())
    });
    let after_id = DRAWN_AFTER_ID.iter().flat_map(|call| {
        line.split_once(call)
            .map(|(_, rest)| literals(rest).into_iter().skip(1).collect::<Vec<_>>())
            .unwrap_or_default()
    });
    let after_theme = DRAWN_AFTER_THEME.iter().filter_map(|call| {
        line.split_once(call).and_then(|(_, rest)| literals(rest).into_iter().next())
    });
    drawn.chain(after_id).chain(after_theme).find(lower)
}

/// Whether `text` names a computer as a worker: "worker" or "workers", any case, as a word
/// of its own, not part of an identifier (`slopty-worker`, `{worker}`, `nav-worker-`).
fn says_worker(text: &str) -> bool {
    let edge = |c: Option<char>| c.is_none_or(|c| !(c.is_alphanumeric() || "_-.{}".contains(c)));
    let lower = text.to_lowercase();
    lower.match_indices("worker").any(|(at, word)| {
        let before = lower.get(..at).and_then(|b| b.chars().next_back());
        let rest = lower.get(at.saturating_add(word.len())..).unwrap_or_default();
        let rest = rest.strip_prefix('s').unwrap_or(rest);
        edge(before) && edge(rest.chars().next())
    })
}

/// The person's word for a computer Slopty runs on is "machine", as the tailnet's is: no
/// string the workspace's chrome or the palette shows says "worker". Code, wire and process
/// names keep it. A literal shaped as a key (lowercase, no space: an element's id, a
/// `userInfo` key, a selector's part) is not shown, nor is a `Debug` name or a log line.
#[test]
fn chrome_says_machine_not_worker() {
    assert!(says_worker("Add a worker") && says_worker("Workers") && says_worker("workers'"));
    assert!(!says_worker("slopty-worker") && !says_worker("on {worker}"));
    assert!(!says_worker("nav-worker-{key}") && !says_worker("workerd"));
    let mut lines = chrome_lines("slopty-ui/src/workspace");
    for file in ["workspace.rs", "palette.rs", "keymap.rs"] {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join(file);
        let text = std::fs::read_to_string(&path).expect("a source file reads");
        let name = path.display().to_string();
        let kept = text.lines().take_while(|l| l.trim() != "#[cfg(test)]");
        lines.extend(kept.enumerate().map(|(ix, l)| (name.clone(), ix + 1, l.to_owned())));
    }
    assert!(lines.iter().any(|(_, _, l)| l.contains("\"Machines\"")), "the chrome is scanned");
    let unshown = ["tracing::", ".id(", "debug_tuple(", "debug_struct(", ".field("];
    let mut said = Vec::new();
    for (file, line_no, line) in lines {
        let code = line.trim_start();
        if code.starts_with("//") || unshown.iter().any(|u| line.contains(u)) {
            continue;
        }
        let shown = line.split('"').skip(1).step_by(2).filter(|text| {
            let key = text.chars().all(|c| c.is_ascii_lowercase() || "._-".contains(c));
            !key && says_worker(text)
        });
        said.extend(shown.map(|text| format!("{file}:{line_no}: {text:?}")));
    }
    assert!(said.is_empty(), "say machine, not worker:\n{}", said.join("\n"));
}

/// Chrome text is drawn in sentence case, as the constants above are written.
#[test]
fn a_drawn_label_is_sentence_case() {
    let wrong: Vec<String> = CHROME
        .into_iter()
        .flat_map(chrome_lines)
        .filter_map(|(file, line_no, line)| {
            lowercase_label(&line).map(|text| format!("{file}:{line_no}: lowercase: {text:?}"))
        })
        .collect();
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

#[test]
fn the_label_check_knows_a_label_from_an_id() {
    assert!(lowercase_label(r#"pill("take", id, "take", tone, theme, chrome)"#).is_some());
    assert!(lowercase_label(r#"pill("take", id, TAKE, tone, theme, chrome)"#).is_none());
    assert!(lowercase_label(r#"muted_line("attaching…".into())"#).is_some());
    assert!(lowercase_label(r#".child("Nothing new")"#).is_none());
    assert!(lowercase_label(r#"button(copy_id, "Copy code", "copy")"#).is_some());
    assert!(lowercase_label(r#"button(copy_id, "Copy code", "Copy")"#).is_none());
    assert!(lowercase_label(r#"kit::button(theme, "new-shell", "New shell", kind)"#).is_none());
    assert!(lowercase_label(r#".aria_label("mute")"#).is_some());
    assert!(lowercase_label(r#"kit::label(theme, "workers")"#).is_some());
    assert!(lowercase_label(r#"kit::label(theme, "Workers")"#).is_none());
    assert!(lowercase_label(r#".key_cap("key-find".to_owned(), "find", on, small)"#).is_some());
    assert!(lowercase_label(r#"self.bar_key(format!("skey-{label}"), label, lit, f)"#).is_none());
    assert!(lowercase_label(r#".id("file-bar").child(text)"#).is_none(), "an id");
    assert!(lowercase_label(r#"// .child("a comment")"#).is_none());
    assert!(lowercase_label(r#".aria_label("settings.toml")"#).is_none(), "a file name");
    assert!(lowercase_label(r#".placeholder("mac-studio or 100.64.0.3")"#).is_none(), "a host");
}

/// The ruling in `docs/decisions/ui.md`: keys live in the palette, the menus and the
/// hints, all of which read them from the binding tables through `palette::keys_for`.
/// A chord typed into a button, an empty state or a menu row by hand drifts from the
/// binding (the "+" menu said `⌘⇧T` while the palette said `⇧⌘T`) and puts keys where
/// the palette should be, so none compiles.
#[test]
fn a_chord_is_spelled_only_by_the_key_tables() {
    let mut wrong = Vec::new();
    for dir in CHROME {
        for (file, line_no, line) in chrome_lines(dir) {
            if let Some(text) = literal_chord(&line) {
                wrong.push(format!("{file}:{line_no}: a chord written by hand: {text:?}"));
            }
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

#[test]
fn the_chord_check_knows_a_chord_from_a_key_cap() {
    assert!(literal_chord(r#"button("Save", "⌘↩", true)"#).is_some());
    assert!(literal_chord(r#".child("⌘T opens a shell")"#).is_some());
    assert!(literal_chord(r#"("⌘", "cmd", None),"#).is_none(), "a key cap");
    assert!(literal_chord("/// ⌘⌥← walks the columns").is_none(), "a comment");
    assert!(literal_chord(r#"out.push('⌘'); label("a")"#).is_none(), "a char, not text");
    assert!(literal_chord(r#"f("a\"b", "⇧x")"#).is_some(), "past an escaped quote");
}

/// The spacing check catches what it is for and leaves alone what it is not.
///
/// A lint that cannot fail is worse than no lint, because it reads like cover.
#[test]
fn the_spacing_check_knows_a_pad_from_a_measurement() {
    assert!(literal_spacing(".p(px(7.0))").is_some());
    assert!(literal_spacing("div().gap(px(10.0)).child(x)").is_some());
    assert!(literal_spacing(".py(px(3.))").is_some());
    // Taken from the scale: the whole point.
    assert!(literal_spacing(".p(px(theme.spacing.md))").is_none());
    assert!(literal_spacing(".gap(px(s.xs))").is_none());
    // A measurement of something real, not a rhythm.
    assert!(literal_spacing(".w(px(300.0))").is_none());
    assert!(literal_spacing(".border_b(px(1.0))").is_none());
    assert!(literal_spacing(".size(px(16.0))").is_none());
    // `.pr(` must not be found inside `.appear(` or any other word ending in those letters.
    assert!(literal_spacing("something.expr(px(4.0))").is_none());
}

/// The chrome draws its icons one way (`docs/decisions/ui.md`, "The chrome's icons are IBM
/// Carbon's at 16"): no SVG is drawn in the app's own code but the app's mark and the icon
/// modules, no icon name of another set is left, and a file's icon is a mask in the words' ink,
/// never a picture in its own colours.
#[test]
fn the_chrome_draws_its_own_icons() {
    let mut stray = Vec::new();
    for dir in CHROME {
        for (file, line, text) in chrome_lines(dir) {
            let code = text.split("//").next().unwrap_or_default();
            // The app's mark is a drawing of ours (`brand`), `icons.rs` names the kit's
            // drawings it stands glyphs in for and masks the files' icons, `icons/glyphs.rs`
            // and `icons/marks.rs` read Carbon's and the agents' owners' outlines into masks,
            // and `file_types.rs` keeps Material's files. A git glyph (`GitGlyph::`) is one
            // of the glyphs.
            let mark = code.contains("assets/icon.svg")
                || file.ends_with("icons.rs")
                || file.ends_with("icons/glyphs.rs")
                || file.ends_with("icons/marks.rs")
                || file.ends_with("file_types.rs");
            let code = code.replace("GitGlyph::", "");
            if !mark && ["svg()", ".svg\"", "IconName", "Glyph::"].iter().any(|b| code.contains(b))
            {
                stray.push(format!("{file}:{line}: {}", text.trim()));
            }
        }
    }
    assert!(stray.is_empty(), "an icon drawn another way:\n{}", stray.join("\n"));
    let icons = include_str!("../../slopty-kit/src/icons.rs");
    assert!(!icons.contains("paint_image("), "a file's icon is painted as a mask, in one ink");
}

/// An agent's mark wears no colour of its own (`docs/decisions/brand.md`, "Each agent
/// wears its owner's mark"): it is drawn as coverage alone and painted in the ink of the
/// words beside it, never in a brand's clay, green or coral, so colour keeps meaning state.
#[test]
fn an_agents_mark_wears_no_colour_of_its_own() {
    let marks = include_str!("../../slopty-kit/src/icons/marks.rs");
    let icons = include_str!("../../slopty-kit/src/icons.rs");
    let between = |from: &str, to: &str| {
        icons.split(from).nth(1).and_then(|rest| rest.split(to).next()).unwrap_or_default()
    };
    let paint = between("fn paint_agent(", "fn paint_file(");
    let centred = between("fn paint_centred(", "\n}\n");
    assert!(paint.contains("paint_centred("), "painted as a mask is");
    assert!(centred.contains("window.text_style().color"), "painted in the words' ink");
    let painters = [("icons.rs paint_agent", paint), ("icons.rs paint_centred", centred)];
    for (name, code) in std::iter::once(("icons/marks.rs", marks)).chain(painters) {
        for (n, line) in code.lines().enumerate() {
            let code = line.split("//").next().unwrap_or_default();
            let tones = ["surfaces", "Rgb", "rgb(", "hsla(", "Hsla {", "0x", "\"#"];
            assert!(
                !tones.iter().any(|t| code.contains(t)),
                "{name}:{}: a colour in an agent's mark: {}",
                n.saturating_add(1),
                line.trim()
            );
        }
    }
    for svg in [
        include_str!("../../slopty-kit/assets/agents/claude.svg"),
        include_str!("../../slopty-kit/assets/agents/openai.svg"),
    ] {
        assert!(svg.contains("fill=\"currentColor\""), "the outline carries no colour");
    }
}

/// An icon takes its words' size: a symbol is sized only in `icons.rs`, from the type
/// scale ([`slopty_kit::icons::Drawn`], the one icon slot), and painted only there,
/// so no call site picks a point size of its own.
#[test]
fn an_icon_takes_its_words_size() {
    let mut stray = Vec::new();
    for (file, line, text) in ui_lines() {
        if file.ends_with("icons.rs") {
            continue;
        }
        let code = text.split("//").next().unwrap_or_default();
        if ["SymbolSize", "paint_mask(", "rasterize("].iter().any(|bad| code.contains(bad)) {
            stray.push(format!("{file}:{line}: {}", text.trim()));
        }
    }
    assert!(stray.is_empty(), "a symbol sized apart from its words:\n{}", stray.join("\n"));
}
