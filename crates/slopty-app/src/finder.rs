//! Each worker's home in Finder, the app's half.
//!
//! The app keeps one File Provider domain per worker the server lists
//! (`slopty_platform::files`, the extension in `apps/slopty-files`), and the palette's "Show
//! workers in Finder" opens them, or System Settings at their switch the first time. The
//! Mac's alone: on iOS the palette does not offer it and nothing follows the directory.

use std::path::{Path, PathBuf};

use gpui::Context;
use slopty_core::WorkerId;

pub mod actions {
    //! The palette's way to the workers in Finder.
    #![expect(
        clippy::derive_partial_eq_without_eq,
        reason = "gpui::actions! derives PartialEq only"
    )]
    use gpui::actions;

    actions!(
        files,
        [
            /// Show each worker's home in Finder, or where to switch Slopty's place there on.
            ShowWorkersInFinder,
        ]
    );
}

/// The palette's words for it.
pub const TITLE: &str = "Show workers in Finder";

/// Whether the palette offers it.
pub const OFFERED: bool = cfg!(target_os = "macos");

/// The notice while Slopty's place in Finder is switched off, beside System Settings opened at
/// Login Items & Extensions, where File Providers lists Slopty.
pub const SWITCH_ON: &str = "Turn on Slopty under File Providers, then try again";

/// One worker's domain, as the system has it: its worker and whether it is switched on.
pub type Switched = (WorkerId, bool);

/// What "Show workers in Finder" does.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Step {
    /// Open System Settings where the person switches Slopty's place in Finder on.
    SwitchOn,
    /// Open this folder in Finder: a worker's home, or the folder that holds them all while
    /// the system has not said where it put any.
    Open(PathBuf),
    /// Nothing to show: no server lists a worker.
    NoWorkers,
    /// This build has no extension: the team did not sign it.
    Unsigned,
    /// The system would not say, in these words.
    Failed(String),
}

impl Step {
    /// What the workspace says as it takes the step; `None` when Finder shows it.
    #[must_use]
    pub fn notice(&self) -> Option<String> {
        match self {
            Self::SwitchOn => Some(SWITCH_ON.to_owned()),
            Self::Open(_) => None,
            Self::NoWorkers => Some("Connect to a server to see its workers in Finder".to_owned()),
            Self::Unsigned => Some("This build of Slopty has no Finder extension".to_owned()),
            Self::Failed(why) => Some(format!("Finder: {why}")),
        }
    }
}

/// The step for the domains `domains`: their switch while any is off, else the first one's
/// home that `root` knows of, else `places`, the folder Finder keeps them all in.
#[must_use]
pub fn step(
    domains: &[Switched],
    root: impl Fn(WorkerId) -> Option<PathBuf>,
    places: &Path,
) -> Step {
    if domains.is_empty() {
        Step::NoWorkers
    } else if domains.iter().any(|(_, on)| !on) {
        Step::SwitchOn
    } else {
        domains
            .iter()
            .find_map(|(id, _)| root(*id))
            .map_or_else(|| Step::Open(places.to_path_buf()), Step::Open)
    }
}

impl crate::Workspace {
    /// "Show workers in Finder": the system is asked on the networking runtime, and the step
    /// is taken and said here.
    pub(crate) fn show_workers_in_finder(&self, cx: &Context<Self>) {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.runtime.spawn(async move {
            let _unheard = tx.send(next_step().await);
        });
        cx.spawn(async move |this, cx| {
            let Ok(step) = rx.await else { return };
            take(&step);
            if let Some(notice) = step.notice() {
                let _gone = this.update(cx, |ws, cx| ws.show_notice(notice, cx));
            }
        })
        .detach();
    }
}

#[cfg(target_os = "macos")]
pub(crate) use mac::{follow, next_step, take};

#[cfg(target_os = "macos")]
mod mac {
    use slopty_client::directory::Directory;
    use slopty_platform::files::{self, FilesError, Known};

    use super::Step;
    use crate::server::Cache;

    /// Every worker of `directory` as the extension reaches it, by name.
    fn known(directory: &Directory) -> Vec<Known> {
        let mut known: Vec<Known> = directory
            .workers()
            .map(|w| Known { id: w.worker, name: w.name.clone(), addrs: vec![w.address.clone()] })
            .collect();
        known.sort_by(|a, b| a.name.cmp(&b.name));
        known
    }

    /// Keep one domain per worker of the directory the app holds, as `directory` hears it
    /// change: added as a worker comes, renamed as it is, removed once it is forgotten or the
    /// server is let go. A change only of a worker's load or liveness asks nothing of the
    /// system.
    pub(crate) async fn follow(mut directory: tokio::sync::watch::Receiver<Cache>) {
        let mut shown: Option<Vec<Known>> = None;
        while directory.changed().await.is_ok() {
            let workers: Vec<Known> = match &*directory.borrow_and_update() {
                Cache::Keep(_, listed) => known(listed),
                Cache::Remove => Vec::new(),
            };
            if shown.as_ref() == Some(&workers) {
                continue;
            }
            match files::publish(&workers).await {
                Ok(()) => shown = Some(workers),
                // This build has no extension: the directory is not followed at all.
                Err(FilesError::NoContainer) => return,
                Err(e) => tracing::warn!(error = %e, "the workers in Finder"),
            }
        }
    }

    /// What "Show workers in Finder" does now, from the domains the system has.
    pub(crate) async fn next_step() -> Step {
        let Some(shared) = files::container() else { return Step::Unsigned };
        match files::domains().await {
            Ok(mut domains) => {
                domains.sort_by(|a, b| a.name.cmp(&b.name));
                let switched: Vec<_> = domains.iter().map(|d| (d.id, d.enabled)).collect();
                let places = slopty_platform::dirs::home().join("Library").join("CloudStorage");
                super::step(&switched, |id| files::root(&shared, id), &places)
            }
            Err(e) => Step::Failed(e.to_string()),
        }
    }

    /// Take `step`: open System Settings or Finder.
    pub(crate) fn take(step: &Step) {
        match step {
            Step::SwitchOn => files::switch_on(),
            Step::Open(folder) => files::show(folder),
            Step::NoWorkers | Step::Unsigned | Step::Failed(_) => {}
        }
    }
}

/// Elsewhere there is no Finder to show the workers in.
#[cfg(not(target_os = "macos"))]
#[expect(clippy::unused_async, reason = "the Mac's asks the system")]
async fn next_step() -> Step {
    Step::Unsigned
}

#[cfg(not(target_os = "macos"))]
const fn take(_step: &Step) {}

#[cfg(test)]
mod tests {
    use super::*;

    /// No domain is nothing to show; one switched off opens its switch, whatever the others;
    /// once all are on, the first worker whose home is known opens, and the folder that holds
    /// them while none is.
    #[test]
    fn the_switch_first_then_a_workers_home() {
        let places = Path::new("/Users/dev/Library/CloudStorage");
        let (studio, pro) = (WorkerId::new(), WorkerId::new());
        let pro_home = PathBuf::from("/Users/dev/Library/CloudStorage/Slopty-macbook-pro");
        let known = |id: WorkerId| (id == pro).then(|| pro_home.clone());
        assert_eq!(step(&[], known, places), Step::NoWorkers);
        assert_eq!(step(&[(studio, true), (pro, false)], known, places), Step::SwitchOn);
        assert_eq!(
            step(&[(studio, true), (pro, true)], known, places),
            Step::Open(pro_home.clone())
        );
        assert_eq!(
            step(&[(studio, true)], known, places),
            Step::Open(places.to_path_buf()),
            "the system has not said where it put the studio yet"
        );
    }

    /// Each step that Finder does not show itself is said in a sentence-case line short enough
    /// for one notice, and the switch's names the setting the person turns on.
    #[test]
    fn each_step_finder_does_not_show_is_said() {
        let said: Vec<String> = [
            Step::SwitchOn,
            Step::NoWorkers,
            Step::Unsigned,
            Step::Failed("the system did not answer".to_owned()),
        ]
        .iter()
        .map(|step| step.notice().expect("said"))
        .collect();
        for line in &said {
            let first = line.chars().next().expect("a word");
            assert!(first.is_uppercase(), "{line}");
            assert!(line.len() <= 60, "one notice's line: {line}");
            assert!(!line.ends_with('.'), "{line}");
        }
        assert!(said[0].contains("File Providers"));
        assert_eq!(Step::Open(PathBuf::from("/")).notice(), None);
    }
}
