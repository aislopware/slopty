//! Reading a Claude Code terminal title (OSC 0/2).
//!
//! While a turn runs, Claude Code repaints the title with a spinning half circle in front of it
//! (`◐ Claude Code`, `◑ …`); between turns it paints a sparkle in front of the conversation's
//! summary instead (`✳ GPUI and gpui-kit upstream sync track`), or the bare name before there
//! is a summary. That makes the title the cheapest heartbeat there is — no file, no process
//! table, just the bytes the engine already parsed — but it is also the coarsest: it separates
//! working from idle and says nothing about tools or about what the agent is blocked on. It is
//! only read when neither a hook nor the transcript has spoken for the session.
//!
//! The glyph sets are tables on purpose: a Claude Code that adds a frame shows up as a title
//! that stops being recognised, not as an agent that silently disappears. Do not confuse them
//! with the frames in the pane itself (`✳ ✻ ✽ · ∗ ✢`), which are the spinner Claude Code draws
//! in its own output and never reach the title.

/// What a title says about the agent, when it says anything.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TitleSignal {
    /// A turn is running (the title carries a frame of the spinning circle).
    Working,
    /// The agent is there and between turns.
    Idle,
}

/// The frames the title cycles through while a turn runs.
pub const WORKING: [char; 4] = ['◐', '◑', '◒', '◓'];

/// The sparkle the title carries between turns, in front of the conversation's summary.
pub const IDLE: char = '✳';

/// What `title` says about an agent in that terminal, or `None` when it says nothing.
#[must_use]
pub fn signal(title: &str) -> Option<TitleSignal> {
    let title = title.trim();
    match title.chars().next() {
        Some(c) if WORKING.contains(&c) => return Some(TitleSignal::Working),
        Some(c) if c == IDLE => return Some(TitleSignal::Idle),
        _ => {}
    }
    // Otherwise only a title that *starts* with the agent's name counts; "claude" inside a path
    // or a file name is a shell showing its working directory, not an agent.
    let lower = title.to_lowercase();
    let first = lower.split(|c: char| c.is_whitespace()).next()?;
    (first == "claude").then_some(TitleSignal::Idle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_spinning_frame_reads_as_working() {
        for glyph in WORKING {
            let title = format!("{glyph} Claude Code");
            assert_eq!(signal(&title), Some(TitleSignal::Working), "{title}");
        }
        assert_eq!(signal("  ◐ Claude Code"), Some(TitleSignal::Working));
    }

    #[test]
    fn a_sparkle_or_a_bare_name_reads_as_idle() {
        // Between turns the title is the sparkle and the conversation's summary.
        assert_eq!(signal("✳ GPUI and gpui-kit upstream sync track"), Some(TitleSignal::Idle));
        assert_eq!(signal("✳ Claude Code"), Some(TitleSignal::Idle));
        assert_eq!(signal("Claude Code"), Some(TitleSignal::Idle));
        assert_eq!(signal("claude"), Some(TitleSignal::Idle));
        assert_eq!(signal("claude — slopty"), Some(TitleSignal::Idle));
    }

    #[test]
    fn the_in_pane_spinner_frames_are_not_the_titles() {
        // These are the frames Claude Code draws in its own output; a terminal whose *title*
        // is one of them is not Claude Code and must not be read as one.
        for glyph in ['·', '✢', '∗', '✻', '✽'] {
            let title = format!("{glyph} Deciphering… (12s · esc to interrupt)");
            assert_eq!(signal(&title), None, "{title}");
        }
    }

    #[test]
    fn other_titles_say_nothing() {
        assert_eq!(signal(""), None);
        assert_eq!(signal("zsh"), None);
        assert_eq!(signal("~/Workspace/oss/slopty"), None);
        assert_eq!(signal("vim claude.md"), None);
        assert_eq!(signal("cargo test -p slopty-agent"), None);
    }
}
