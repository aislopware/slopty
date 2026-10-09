//! A clone a client asks of a worker straight (`ClientMsg::CloneRepo`): the repository another
//! machine has, made where the person is about to start work on this one.
//!
//! The worker clones with its own git and credentials, from an address with none in it
//! ([`crate::terminal::RepoId::url`]). It says how far it has come
//! (`WorkerMsg::RepoCloning`) and then how it went (`WorkerMsg::RepoCloned`).

use serde::{Deserialize, Serialize};

use crate::terminal::RepoId;

/// How a clone went.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum CloneOutcome {
    /// The clone is there: made, or found there already, a clone of the same origin.
    Cloned(ClonedRepo),
    /// Not tried, in Slopty's words: the place holds something that is no clone of that origin,
    /// the address names no remote, or the worker has no git.
    Refused {
        /// Why.
        why: String,
    },
    /// git said no, in its own words, or the clone took too long. Nothing is left behind.
    Failed {
        /// The end of what it said.
        said: String,
    },
}

/// A clone made, or found.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ClonedRepo {
    /// Where it is, absolute.
    pub path: String,
    /// Which repository it is, as the worker reads it.
    pub repo: RepoId,
}
