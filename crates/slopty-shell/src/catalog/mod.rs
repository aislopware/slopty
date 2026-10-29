//! What the shell reads about agents that its source kept beside its engine: the harness
//! descriptors (`registry`) and how each agent installs (`harness`). Slopty's workers run the
//! agents; these are the shell's view of them.

pub mod harness;
pub mod registry;

/// The org a dev-mode engine runs under.
pub const DEFAULT_ORG_ID: &str = "dev-org";
