//! `CloneRepo`: a new project cloned from a git address on a machine, by its worker's own clone
//! (`ClientMsg::CloneRepo`), then added as a project.
//!
//! The clone goes under the folder the person picked (`parentPath`, else the machine's home) in a
//! folder named for the repository. A clone already there of the same origin is taken as it is;
//! anything else there is refused by the worker, in its words.

use std::collections::HashMap;
use std::time::Duration;

use futures::FutureExt as _;
use gpui::{Context, Task};
use slopty_client::WorkerKey;
use slopty_proto::cloning::CloneOutcome;
use slopty_proto::{ClientMsg, RequestId, WorkerMsg};
use slopty_view_rpc::{RpcError, RpcReply};
use tokio::sync::oneshot;

use super::{param, value};
use crate::workspace::WorkspaceView;

/// The longest a clone is waited for: a large repository over a slow link takes minutes, and the
/// worker ends one that takes too long itself.
const CLONE_WITHIN: Duration = Duration::from_mins(20);

/// The clones on their way, by worker and number.
#[derive(Debug, Default)]
pub(super) struct Clones(HashMap<(WorkerKey, RequestId), oneshot::Sender<CloneOutcome>>);

impl Clones {
    /// The link to `key` went: its clones are answered never.
    pub(super) fn forget(&mut self, key: WorkerKey) {
        self.0.retain(|(k, _), _| *k != key);
    }
}

/// The folder a clone of `url` goes in: the repository's own name, its `.git` left off.
fn folder_name(url: &str) -> Option<String> {
    let name = url.trim().trim_end_matches('/').rsplit(['/', ':']).next()?;
    let name = name.strip_suffix(".git").unwrap_or(name);
    (!name.is_empty() && name != "." && name != "..").then(|| name.to_owned())
}

impl WorkspaceView {
    /// `CloneRepo` `{url, targetDeviceId?, parentPath?}`: the repository cloned on the machine
    /// and added as a project; answered `{spaceId, deviceId, path}` once it is there.
    pub(super) fn clone_repo(
        &mut self,
        params: &serde_json::Value,
        cx: &Context<Self>,
    ) -> Task<Result<RpcReply, RpcError>> {
        let Some(url) = param(params, "url").map(str::trim).map(str::to_owned) else {
            return Task::ready(Err(RpcError::BadParams("url is required".to_owned())));
        };
        let Some(name) = folder_name(&url) else {
            return Task::ready(Err(RpcError::Failed(format!("{url} names no repository"))));
        };
        let key = match param(params, "targetDeviceId").or_else(|| param(params, "deviceId")) {
            Some(device) => self.view_worker(device),
            None => self.context_worker(),
        };
        let Some(key) = key else {
            return Task::ready(Err(RpcError::Failed("no machine is connected".to_owned())));
        };
        let parent = param(params, "parentPath").unwrap_or("~").trim_end_matches('/');
        let into = format!("{}/{name}", if parent.is_empty() { "/" } else { parent });
        let request = self.view_model.surfaces.next();
        let (tx, rx) = oneshot::channel();
        if !self.surface_send(key, ClientMsg::CloneRepo { request, url, into }) {
            return Task::ready(Err(RpcError::Failed("the machine is not connected".to_owned())));
        }
        self.view_model.surfaces.clones.0.insert((key, request), tx);
        cx.spawn(async move |this, cx| {
            let timer = cx.background_executor().timer(CLONE_WITHIN);
            let outcome = futures::select_biased! {
                outcome = rx.fuse() => outcome.map_err(|_gone| super::unanswered())?,
                () = timer.fuse() => {
                    let _gone = this.update(cx, |view, _| {
                        view.view_model.surfaces.clones.0.remove(&(key, request))
                    });
                    return Err(RpcError::Failed("The clone took too long".to_owned()));
                }
            };
            let repo = match outcome {
                CloneOutcome::Cloned(repo) => repo,
                CloneOutcome::Refused { why } => return Err(RpcError::Failed(why)),
                CloneOutcome::Failed { said } => return Err(RpcError::Failed(said)),
            };
            let space = slopty_proto::thread::IntentId::new().to_string();
            let device = key.to_string();
            let added = serde_json::json!({
                "spaceId": space, "deviceId": device, "path": repo.path, "gitDetected": true,
            });
            this.update(cx, |view, cx| view.project_mutate("createSpace", &added, cx))
                .map_err(super::gone)?
                .unwrap_or_else(|| Err(RpcError::Failed("the project was not added".to_owned())))?;
            value(&serde_json::json!({ "spaceId": space, "deviceId": device, "path": repo.path }))
        })
    }

    /// A clone's answer from `key`; whether `msg` was one of the surfaces' clones.
    pub(super) fn clone_heard(&mut self, key: WorkerKey, msg: &WorkerMsg) -> bool {
        match msg {
            WorkerMsg::RepoCloned { request, outcome } => {
                self.view_model.surfaces.clones.0.remove(&(key, *request)).is_some_and(|tx| {
                    let _gone = tx.send(outcome.clone());
                    true
                })
            }
            WorkerMsg::RepoCloning { request, .. } => {
                self.view_model.surfaces.clones.0.contains_key(&(key, *request))
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use gpui::AppContext as _;
    use slopty_proto::cloning::{CloneOutcome, ClonedRepo};
    use slopty_proto::terminal::RepoId;
    use slopty_proto::{ClientMsg, WorkerMsg};
    use slopty_view_rpc::RpcReply;

    use super::folder_name;
    use crate::workspace::WorkspaceView;
    use crate::workspace::rig::{KEY, link, mac};

    /// A clone goes in a folder named for its repository.
    #[test]
    fn a_clone_is_named_for_its_repository() {
        assert_eq!(folder_name("https://github.com/o/slopty.git").as_deref(), Some("slopty"));
        assert_eq!(folder_name("git@github.com:o/notes").as_deref(), Some("notes"));
        assert_eq!(folder_name("https://github.com/o/r/").as_deref(), Some("r"));
        assert_eq!(folder_name("").as_deref(), None);
    }

    /// A clone asks the machine's worker under the folder picked, and once the worker made it,
    /// it is a project there; one the worker refuses says why.
    #[gpui::test]
    fn a_cloned_repository_becomes_a_project(cx: &mut gpui::TestAppContext) {
        let workspace = cx.new(|_| WorkspaceView::new(slopty_theme::Theme::default()));
        let cx = cx.add_empty_window();
        workspace.update(cx, |v, cx| v.add_worker(KEY, "studio".to_owned(), cx));
        let mut rx = link(&workspace, cx, mac(|_| {}), Vec::new());
        let ask = |url: &str, cx: &mut gpui::VisualTestContext| {
            let params = serde_json::json!({ "url": url, "parentPath": "~/dev" });
            workspace.update(cx, |v, cx| v.clone_repo(&params, cx))
        };
        let answer = ask("https://github.com/o/slopty.git", cx);
        cx.run_until_parked();
        let (request, into) = std::iter::from_fn(|| rx.try_recv().ok())
            .find_map(|msg| match msg {
                ClientMsg::CloneRepo { request, into, .. } => Some((request, into)),
                _ => None,
            })
            .expect("the clone asked");
        assert_eq!(into, "~/dev/slopty");
        let repo = ClonedRepo {
            path: "/Users/me/dev/slopty".to_owned(),
            repo: RepoId { root: None, origin: None, url: None },
        };
        let done = WorkerMsg::RepoCloned { request, outcome: CloneOutcome::Cloned(repo) };
        assert!(workspace.update(cx, |v, _| v.clone_heard(KEY, &done)));
        cx.run_until_parked();
        let Ok(RpcReply::Value(answered)) = futures::FutureExt::now_or_never(answer).expect("done")
        else {
            panic!("the clone is answered");
        };
        assert_eq!(answered["path"], "/Users/me/dev/slopty");
        let added = workspace.update(cx, |v, _| {
            v.added_project_at(&KEY.to_string(), "/Users/me/dev/slopty").map(|p| p.git)
        });
        assert_eq!(added, Some(true), "a project, in git");

        let refused = ask("https://github.com/o/other", cx);
        cx.run_until_parked();
        let request = std::iter::from_fn(|| rx.try_recv().ok())
            .find_map(|msg| match msg {
                ClientMsg::CloneRepo { request, .. } => Some(request),
                _ => None,
            })
            .expect("the second clone asked");
        let why = "~/dev/other holds something else".to_owned();
        let done = WorkerMsg::RepoCloned { request, outcome: CloneOutcome::Refused { why } };
        workspace.update(cx, |v, _| v.clone_heard(KEY, &done));
        cx.run_until_parked();
        let answer = futures::FutureExt::now_or_never(refused).expect("done");
        assert!(
            matches!(answer, Err(slopty_view_rpc::RpcError::Failed(why)) if why.contains("something else"))
        );
    }
}
