//! Reading a Claude Code terminal title (OSC 0/2).
//!
//! While a turn runs, Claude Code repaints the title with an animated sparkle in front of it
//! (`✳ Claude Code`, `✻ …`); between turns the title carries the agent's name without one.
//! That makes the title the cheapest heartbeat there is — no file, no process table, just the
//! bytes the engine already parsed — but it is also the coarsest: it separates working from
//! idle and says nothing about tools or about what the agent is blocked on. It is only read
//! when neither a hook nor the transcript has spoken for the session.
//!
//! The glyph set is a table on purpose: a Claude Code that adds a frame shows up as a title
//! that stops being recognised, not as an agent that silently disappears.

/// What a title says about the agent, when it says anything.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TitleSignal {
    /// A turn is running (the title carries the animated sparkle).
    Working,
    /// The agent is there and between turns.
    Idle,
}

/// The sparkle frames Claude Code cycles through while a turn runs.
pub const SPINNER: [char; 6] = ['·', '✢', '✳', '∗', '✻', '✽'];

/// What `title` says about an agent in that terminal, or `None` when it says nothing.
#[must_use]
pub fn signal(title: &str) -> Option<TitleSignal> {
    let title = title.trim();
    if title.chars().next().is_some_and(|c| SPINNER.contains(&c)) {
        return Some(TitleSignal::Working);
    }
    // Only a title that *starts* with the agent's name counts; "claude" inside a path or a
    // file name is a shell showing its working directory, not an agent.
    let lower = title.to_lowercase();
    let first = lower.split(|c: char| c.is_whitespace()).next()?;
    (first == "claude").then_some(TitleSignal::Idle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_spinner_frame_reads_as_working() {
        for glyph in SPINNER {
            let title = format!("{glyph} Claude Code");
            assert_eq!(signal(&title), Some(TitleSignal::Working), "{title}");
        }
        assert_eq!(signal("  ✻ Deciphering… (12s · esc to interrupt)"), Some(TitleSignal::Working));
    }

    #[test]
    fn a_bare_name_reads_as_idle() {
        assert_eq!(signal("Claude Code"), Some(TitleSignal::Idle));
        assert_eq!(signal("claude"), Some(TitleSignal::Idle));
        assert_eq!(signal("claude — slopty"), Some(TitleSignal::Idle));
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
