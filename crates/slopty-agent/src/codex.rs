//! Codex, its own TUI in a Slopty terminal (`docs/decisions/agents.md`, "TUI-first"): how the
//! worker starts it ([`tui()`]) and takes a thread up again.

pub mod tui;

pub use tui::{Choices, tui};

/// What a start of a Codex thread takes to take thread `native` up again rather than begin one:
/// `resume <native>`, the words of Codex's own `codex resume`.
pub const RESUME: &str = "resume";

/// The arguments of a start that takes Codex thread `native` up again.
#[must_use]
pub fn resume_args(native: &str) -> Vec<String> {
    vec![RESUME.to_owned(), native.to_owned()]
}

/// The Codex thread a start's `args` take up again, when they are [`resume_args`].
#[must_use]
pub fn resumed(args: &[String]) -> Option<&str> {
    match args {
        [word, native] if word == RESUME && !native.trim().is_empty() => Some(native),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A start takes a thread up again by Codex's own words, and only by them.
    #[test]
    fn a_start_resumes_a_thread_by_codex_s_own_words() {
        let args = resume_args("019a");
        assert_eq!(resumed(&args), Some("019a"));
        assert_eq!(resumed(&["resume".to_owned(), " ".to_owned()]), None);
        assert_eq!(resumed(&["fork".to_owned(), "019a".to_owned()]), None);
    }
}
