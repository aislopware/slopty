//! What the shell reads about agents and repositories that its source kept beside its engine:
//! the harness descriptors and title settings (`registry`), the history search (`repos`), the
//! Claude Code model catalogue (`claude`), and which agents write titles and how each installs
//! (`harness`). Slopty's workers run the agents; these are the shell's view of them.

pub mod claude;
pub mod harness;
pub mod registry;
pub mod repos;

/// The org a dev-mode engine runs under.
pub const DEFAULT_ORG_ID: &str = "dev-org";
