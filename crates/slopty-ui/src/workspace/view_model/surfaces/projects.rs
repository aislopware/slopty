//! New project: the folders of a machine the shell's picker browses, and the projects the
//! person adds, renames and removes.
//!
//! A project is a folder on a machine. One a thread works in is a project already; one the
//! person adds before any thread is kept here, by the id the shell gave it, with the name it
//! was given, in `interface/projects.json` under Slopty's data directory. A folder kept here
//! is known by that id everywhere, its threads' rows included.

use std::cell::OnceCell;
use std::collections::HashSet;
use std::path::PathBuf;

use gpui::{AppContext as _, Context, Task};
use serde::{Deserialize, Serialize};
use slopty_client::WorkerKey;
use slopty_proto::folder::Listing;
use slopty_proto::orchestration::FileKind;
use slopty_view_proto::{
    DriveEntry, DriveListing, FolderEntry, FolderListing, HomeFolderMatch, HomeFolderSearchResult,
};
use slopty_view_rpc::{RpcError, RpcReply};

use super::{param, value, within};
use crate::workspace::WorkspaceView;

/// A call refused before it is asked of a machine.
fn fail(error: RpcError) -> Task<Result<RpcReply, RpcError>> {
    Task::ready(Err(error))
}

/// The folders a worker found (under its home `home`, or on a drive), as the picker's matches;
/// `indexing` while its index is still being made, so the picker asks again.
fn home_folders(
    home: &str,
    found: &[slopty_proto::folder::FolderHit],
    indexing: bool,
) -> HomeFolderSearchResult {
    let home = home.trim_end_matches('/');
    let matches = found
        .iter()
        .map(|hit| {
            let under = hit.path.strip_prefix(home).and_then(|rest| rest.strip_prefix('/'));
            HomeFolderMatch {
                path: hit.path.clone(),
                relative: under.map_or_else(|| hit.path.clone(), str::to_owned),
                is_repo: hit.repo,
            }
        })
        .collect();
    HomeFolderSearchResult { matches, indexing }
}

/// A project the person added.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(in crate::workspace) struct Project {
    /// The id the shell gave it.
    pub id: String,
    /// Its machine, as a device id.
    pub device: String,
    /// Its folder there.
    pub path: String,
    /// The name the person gave it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Its folder is a git repository, as the picker or a look at the folder found: it has
    /// branches and worktrees to pick and changes to show before any thread works there.
    #[serde(default)]
    pub git: bool,
    /// When it was added, in ms since the epoch: the projects keep the order they came in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_ms: Option<i64>,
    /// Where its folder really is, its links resolved, as its machine last said: the folders
    /// threads work in are named so, and a project added through a link still holds them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub real: Option<String>,
}

/// The projects the person added, read from their file the first time they are asked of, and
/// kept there; a test's are its own.
#[derive(Debug)]
pub(in crate::workspace) struct Projects {
    all: OnceCell<Vec<Project>>,
    /// Where they are kept; `None` keeps them in memory alone.
    file: Option<PathBuf>,
    /// The projects looked at this run for being a repository.
    probed: HashSet<String>,
}

impl Default for Projects {
    fn default() -> Self {
        Self { all: OnceCell::new(), file: Some(projects_path()), probed: HashSet::default() }
    }
}

/// Where the added projects are kept.
fn projects_path() -> PathBuf {
    slopty_platform::dirs::data_dir().join("interface").join("projects.json")
}

impl Projects {
    /// Projects held in memory alone, starting with `all`.
    #[cfg(test)]
    pub(in crate::workspace) fn held(all: Vec<Project>) -> Self {
        Self { all: OnceCell::from(all), file: None, probed: HashSet::default() }
    }

    /// The added projects, read from their file the first time.
    pub(in crate::workspace) fn all(&self) -> &[Project] {
        self.all.get_or_init(|| {
            let Some(path) = self.file.clone() else { return Vec::new() };
            match std::fs::read(&path) {
                Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                    tracing::warn!(path = %path.display(), %e, "the projects do not read");
                    Vec::new()
                }),
                Err(e) => {
                    if e.kind() != std::io::ErrorKind::NotFound {
                        tracing::warn!(path = %path.display(), %e, "the projects do not read");
                    }
                    Vec::new()
                }
            }
        })
    }

    /// The project added as `id`.
    pub(in crate::workspace) fn by_id(&self, id: &str) -> Option<&Project> {
        self.all().iter().find(|project| project.id == id)
    }

    /// The project added for the folder `path` on `device`.
    pub(in crate::workspace) fn at(&self, device: &str, path: &str) -> Option<&Project> {
        self.all().iter().find(|project| project.device == device && project.path == path)
    }

    /// Change the projects with `change`; whether that changed anything.
    fn change(&mut self, change: impl FnOnce(&mut Vec<Project>)) -> bool {
        self.all();
        let Some(projects) = self.all.get_mut() else { return false };
        let before = projects.clone();
        change(projects);
        *projects != before
    }
}

/// A folder listed for the picker: its folders and files, hidden ones left out.
fn folder_listing(
    dir: String,
    entries: &[slopty_proto::folder::FolderEntry],
    total: u32,
) -> FolderListing {
    let shown: Vec<FolderEntry> = entries
        .iter()
        .filter(|entry| !entry.hidden)
        .map(|entry| FolderEntry {
            name: entry.name.clone(),
            is_dir: entry.kind == FileKind::Dir,
            is_repo: entry.repo,
        })
        .collect();
    let listed = u32::try_from(entries.len()).unwrap_or(u32::MAX);
    FolderListing { path: dir, entries: shown, truncated: total > listed }
}

/// The places to browse beyond home: the system root, and on a Mac each mounted volume.
fn drives(volumes: Option<&[slopty_proto::folder::FolderEntry]>) -> DriveListing {
    let mut drives = vec![DriveEntry { name: "System".to_owned(), path: "/".to_owned() }];
    drives.extend(
        volumes
            .unwrap_or_default()
            .iter()
            .filter(|entry| entry.kind == FileKind::Dir && !entry.link && !entry.hidden)
            .map(|entry| DriveEntry {
                name: entry.name.clone(),
                path: format!("/Volumes/{}", entry.name),
            }),
    );
    DriveListing { drives }
}

impl WorkspaceView {
    /// The projects the person added.
    pub(in crate::workspace) fn added_projects(&self) -> &[Project] {
        self.view_model.surfaces.projects.all()
    }

    /// The project the person added as `id`.
    pub(in crate::workspace) fn added_project(&self, id: &str) -> Option<&Project> {
        self.view_model.surfaces.projects.by_id(id)
    }

    /// The project the person added for the folder `path` on `device`.
    pub(in crate::workspace) fn added_project_at(
        &self,
        device: &str,
        path: &str,
    ) -> Option<&Project> {
        self.view_model.surfaces.projects.at(device, path)
    }

    /// The machine a picker's call names: `targetDeviceId`, else this Mac's own worker, else
    /// the worker in front.
    fn picker_worker(&self, params: &serde_json::Value) -> Result<WorkerKey, RpcError> {
        if let Some(device) = param(params, "targetDeviceId") {
            return self
                .view_worker(device)
                .ok_or_else(|| RpcError::Failed(format!("no machine {device}")));
        }
        self.surface_device(params)
            .or_else(|| self.context_worker())
            .ok_or_else(|| RpcError::Failed("no machine is connected".to_owned()))
    }

    /// `ListFolders`: a folder on a machine, home when no `path` is given.
    pub(super) fn list_folders(
        &mut self,
        params: &serde_json::Value,
        cx: &Context<Self>,
    ) -> Task<Result<RpcReply, RpcError>> {
        let key = match self.picker_worker(params) {
            Ok(key) => key,
            Err(error) => return fail(error),
        };
        let path = param(params, "path").filter(|path| !path.is_empty()).unwrap_or("~");
        let rx = self.surface_list(key, path.to_owned());
        cx.spawn(async move |_, cx| match within(cx, rx).await? {
            Listing::Listed { dir, entries, total, .. } => {
                value(&folder_listing(dir, &entries, total))
            }
            Listing::NotFolder => Err(RpcError::Failed("not a folder".to_owned())),
            Listing::Missing { error } => Err(RpcError::Failed(error)),
        })
    }

    /// `SearchHomeFolders`: the folders a new project on the machine may be, as its worker
    /// finds them (zoxide, Spotlight, fzf's scoring): those the person goes to most when nothing is
    /// typed, else any under the home whose path the query matches, however deep.
    pub(super) fn search_home_folders(
        &mut self,
        params: &serde_json::Value,
        cx: &Context<Self>,
    ) -> Task<Result<RpcReply, RpcError>> {
        let key = match self.picker_worker(params) {
            Ok(key) => key,
            Err(error) => return fail(error),
        };
        let query = params.get("query").and_then(serde_json::Value::as_str).unwrap_or_default();
        let limit = params.get("limit").and_then(serde_json::Value::as_u64).unwrap_or(50);
        let limit = u16::try_from(limit).unwrap_or(u16::MAX);
        let found = self.surface_find_folders(key, query.trim().to_owned(), limit);
        cx.spawn(async move |_, cx| {
            let (home, folders, indexing) = within(cx, found).await?;
            value(&home_folders(&home, &folders, indexing))
        })
    }

    /// `ListDrives`: the system root and the machine's mounted volumes.
    pub(super) fn list_drives(
        &mut self,
        params: &serde_json::Value,
        cx: &Context<Self>,
    ) -> Task<Result<RpcReply, RpcError>> {
        let key = match self.picker_worker(params) {
            Ok(key) => key,
            Err(error) => return fail(error),
        };
        let rx = self.surface_list(key, "/Volumes".to_owned());
        cx.spawn(async move |_, cx| match within(cx, rx).await? {
            Listing::Listed { entries, .. } => value(&drives(Some(&entries))),
            Listing::NotFolder | Listing::Missing { .. } => value(&drives(None)),
        })
    }

    /// `createSpace`, `renameSpace` and `deleteSpace`; `None` for every other operation. The
    /// projects as they are now show at once.
    pub(in crate::workspace) fn project_mutate(
        &mut self,
        op: &str,
        params: &serde_json::Value,
        cx: &Context<Self>,
    ) -> Option<Result<serde_json::Value, RpcError>> {
        let space = param(params, "spaceId").map(str::to_owned);
        let changed = match op {
            "createSpace" => {
                let (Some(id), Some(device), Some(path)) =
                    (space, param(params, "deviceId"), param(params, "path"))
                else {
                    return Some(Err(RpcError::BadParams("spaceId, deviceId and path".to_owned())));
                };
                if self.view_worker(device).is_none() {
                    return Some(Err(RpcError::Failed(format!("no machine {device}"))));
                }
                let git = params.get("gitDetected").and_then(serde_json::Value::as_bool);
                let project = Project {
                    id,
                    device: device.to_owned(),
                    path: path.trim_end_matches('/').to_owned(),
                    name: None,
                    git: git == Some(true),
                    created_ms: Some(chrono::Utc::now().timestamp_millis()),
                    real: None,
                };
                self.view_model.surfaces.projects.change(|projects| {
                    if !projects
                        .iter()
                        .any(|p| p.device == project.device && p.path == project.path)
                    {
                        projects.push(project);
                    }
                })
            }
            "renameSpace" => {
                let Some(id) = space else {
                    return Some(Err(RpcError::BadParams("spaceId".to_owned())));
                };
                let name = param(params, "name").map(str::trim).filter(|name| !name.is_empty());
                let Some((key, path)) = self.view_space(&id) else {
                    return Some(Err(RpcError::Failed(format!("no project {id}"))));
                };
                let device = key.to_string();
                self.view_model.surfaces.projects.change(|projects| {
                    if let Some(project) = projects.iter_mut().find(|p| p.id == id) {
                        project.name = name.map(str::to_owned);
                    } else {
                        let name = name.map(str::to_owned);
                        projects.push(Project {
                            id,
                            device,
                            path,
                            name,
                            git: false,
                            created_ms: None,
                            real: None,
                        });
                    }
                })
            }
            "deleteSpace" => {
                let Some(id) = space else {
                    return Some(Err(RpcError::BadParams("spaceId".to_owned())));
                };
                self.view_model.surfaces.projects.change(|projects| projects.retain(|p| p.id != id))
            }
            _ => return None,
        };
        if changed {
            self.keep_projects(cx);
            self.view_changed(cx);
        }
        Some(Ok(serde_json::json!({})))
    }

    /// Keep the added projects where they outlive the app.
    fn keep_projects(&self, cx: &Context<Self>) {
        let Some(path) = self.view_model.surfaces.projects.file.clone() else { return };
        let kept = self.view_model.surfaces.projects.all().to_vec();
        cx.background_spawn(async move {
            let written = path.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|()| {
                let text = serde_json::to_vec(&kept).map_err(std::io::Error::other)?;
                std::fs::write(&path, text)
            });
            if let Err(e) = written {
                tracing::warn!(path = %path.display(), %e, "the projects were not kept");
            }
        })
        .detach();
    }

    /// Look once a run at each added project's folder, on its machine once it is linked: where
    /// it really is (its links resolved) and whether it is a repository, a folder with `.git`
    /// in it (a clone's directory, a worktree's file). The picker's word on a folder can be
    /// missing or stale, and a folder added through a link is named otherwise by its threads.
    pub(in crate::workspace) fn probe_projects(&mut self, cx: &Context<Self>) {
        let unseen: Vec<(String, WorkerKey, String)> = self
            .added_projects()
            .iter()
            .filter(|p| !self.view_model.surfaces.projects.probed.contains(&p.id))
            .filter_map(|p| {
                let key = self.view_worker(&p.device)?;
                let linked = self.workers.get(&key).is_some_and(|w| w.link.is_some());
                linked.then(|| (p.id.clone(), key, p.path.clone()))
            })
            .collect();
        for (id, key, path) in unseen {
            self.view_model.surfaces.projects.probed.insert(id.clone());
            let rx = self.surface_list(key, path);
            cx.spawn(async move |this, cx| {
                let Ok(Listing::Listed { real, entries, .. }) = within(cx, rx).await else {
                    return;
                };
                let git = entries.iter().any(|entry| entry.name == ".git");
                let _gone = this.update(cx, |view, cx| {
                    let changed = view.view_model.surfaces.projects.change(|projects| {
                        if let Some(project) = projects.iter_mut().find(|p| p.id == id) {
                            project.git = git;
                            project.real = (real != project.path).then_some(real);
                        }
                    });
                    if changed {
                        view.keep_projects(cx);
                        view.view_changed(cx);
                    }
                });
            })
            .detach();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_home_search_offers_the_folders_by_their_place_under_the_home() {
        let hit = |path: &str, repo| slopty_proto::folder::FolderHit {
            path: path.to_owned(),
            repo,
            visited: false,
        };
        let found = [hit("/Users/me/code/slopty", true), hit("/Volumes/x/p", false)];
        let result = home_folders("/Users/me/", &found, false);
        let shown: Vec<(&str, &str, bool)> = result
            .matches
            .iter()
            .map(|m| (m.path.as_str(), m.relative.as_str(), m.is_repo))
            .collect();
        assert_eq!(
            shown,
            [
                ("/Users/me/code/slopty", "code/slopty", true),
                ("/Volumes/x/p", "/Volumes/x/p", false)
            ]
        );
    }

    fn entry(name: &str, kind: FileKind, hidden: bool) -> slopty_proto::folder::FolderEntry {
        slopty_proto::folder::FolderEntry {
            name: name.to_owned(),
            kind,
            link: false,
            hidden,
            size: 0,
            items: None,
            repo: false,
            modified_ms: slopty_core::WallMs::default(),
        }
    }

    #[test]
    fn a_listing_leaves_hidden_entries_out_and_says_when_it_was_cut() {
        let entries = [
            slopty_proto::folder::FolderEntry { repo: true, ..entry("app", FileKind::Dir, false) },
            entry("src", FileKind::Dir, false),
            entry(".git", FileKind::Dir, true),
            entry("Cargo.toml", FileKind::File, false),
        ];
        let listing = folder_listing("/home/a/repo".to_owned(), &entries, 5);
        assert_eq!(listing.path, "/home/a/repo");
        let names: Vec<(&str, bool, bool)> =
            listing.entries.iter().map(|e| (e.name.as_str(), e.is_dir, e.is_repo)).collect();
        assert_eq!(
            names,
            [("app", true, true), ("src", true, false), ("Cargo.toml", false, false)],
            "a repository says so"
        );
        assert!(listing.truncated, "five entries, four listed");
        assert!(!folder_listing(String::new(), &entries, 4).truncated);
    }

    #[test]
    fn the_drives_are_the_root_and_each_volume() {
        let volumes = [
            entry("Lacie", FileKind::Dir, false),
            slopty_proto::folder::FolderEntry {
                link: true,
                ..entry("Macintosh HD", FileKind::Dir, false)
            },
        ];
        let paths: Vec<String> =
            drives(Some(&volumes)).drives.into_iter().map(|d| d.path).collect();
        assert_eq!(paths, ["/", "/Volumes/Lacie"]);
        assert_eq!(drives(None).drives.len(), 1, "a machine without /Volumes has its root");
    }

    #[test]
    fn a_project_is_found_by_its_id_and_by_its_folder() {
        let mut projects = Projects::held(Vec::new());
        let project = Project {
            id: "p1".into(),
            device: "d".into(),
            path: "/a".into(),
            name: None,
            git: false,
            created_ms: None,
            real: None,
        };
        assert!(projects.change(|all| all.push(project.clone())));
        assert_eq!(projects.at("d", "/a").map(|p| p.id.as_str()), Some("p1"));
        assert!(projects.change(|all| {
            if let Some(p) = all.iter_mut().find(|p| p.id == "p1") {
                p.name = Some("A".into());
            }
        }));
        assert_eq!(projects.by_id("p1").and_then(|p| p.name.as_deref()), Some("A"));
        assert!(!projects.change(|_| {}));
        assert!(projects.change(|all| all.retain(|p| p.id != "p1")));
        assert_eq!(projects.all().len(), 0, "removed");
    }
}

#[cfg(test)]
mod served {
    use std::sync::Arc;
    use std::time::Duration;

    use gpui::AppContext as _;
    use slopty_proto::thread::Cursor;
    use slopty_proto::thread::wire::TableFrame;

    use super::{Project, Projects};
    use crate::view_model::{Feed, ViewModel};
    use crate::workspace::WorkspaceView;
    use crate::workspace::rig::{KEY, link, mac};

    /// A workspace linked to one worker with no threads, its projects `projects`, serving a
    /// feed.
    fn serving(
        projects: Vec<Project>,
        cx: &mut gpui::TestAppContext,
    ) -> (gpui::Entity<WorkspaceView>, &mut gpui::VisualTestContext, Arc<Feed>) {
        let workspace = cx.new(|_| WorkspaceView::new(slopty_theme::Theme::default()));
        let cx = cx.add_empty_window();
        workspace.update(cx, |v, cx| v.add_worker(KEY, "studio".to_owned(), cx));
        let _rx = link(&workspace, cx, mac(|_| {}), Vec::new());
        let (_, feed, _asks) = ViewModel::new("me".to_owned());
        workspace.update(cx, |v, cx| {
            v.view_model.feed = Some(Arc::clone(&feed));
            v.view_model.surfaces.projects = Projects::held(projects);
            v.threads_linked(KEY, cx);
            let frame =
                TableFrame::Snapshot { cursor: Cursor { epoch: 1, seq: 1 }, rows: Vec::new() };
            v.thread_table(KEY, &frame, cx);
        });
        (workspace, cx, feed)
    }

    fn project(id: &str, path: &str, created_ms: Option<i64>) -> Project {
        Project {
            id: id.to_owned(),
            device: KEY.to_string(),
            path: path.to_owned(),
            name: None,
            git: false,
            created_ms,
            real: None,
        }
    }

    /// A repository added as a project has its branches, worktrees and changes from the first,
    /// before any thread works there.
    #[gpui::test]
    fn a_repository_added_is_a_repository(cx: &mut gpui::TestAppContext) {
        let (workspace, cx, feed) = serving(Vec::new(), cx);
        let params = serde_json::json!({
            "op": "createSpace", "spaceId": "p1", "deviceId": KEY.to_string(),
            "path": "/w/app", "gitDetected": true,
        });
        workspace.update(cx, |v, cx| {
            let made = v.project_mutate("createSpace", &params, cx);
            assert!(matches!(made, Some(Ok(_))), "{made:?}");
            v.view_publish(cx);
        });
        let spaces = feed.said_spaces().expect("said");
        let added = spaces.iter().find(|space| space.id == "p1").expect("the project");
        assert!(added.git_detected);
    }

    /// A project renamed shows its new name at once, not at the next heartbeat.
    #[gpui::test]
    fn a_project_renamed_shows_at_once(cx: &mut gpui::TestAppContext) {
        let (workspace, cx, feed) = serving(vec![project("p1", "/w/app", Some(1))], cx);
        let params = serde_json::json!({ "op": "renameSpace", "spaceId": "p1", "name": "App" });
        workspace.update(cx, |v, cx| {
            let renamed = v.project_mutate("renameSpace", &params, cx);
            assert!(matches!(renamed, Some(Ok(_))), "{renamed:?}");
        });
        cx.executor().advance_clock(Duration::from_millis(50));
        cx.run_until_parked();
        let spaces = feed.said_spaces().expect("said");
        let name = spaces.iter().find(|space| space.id == "p1").and_then(|s| s.name.clone());
        assert_eq!(name.as_deref(), Some("App"));
    }

    /// Projects keep the order they were added in.
    #[gpui::test]
    fn projects_keep_the_order_they_came_in(cx: &mut gpui::TestAppContext) {
        let projects = vec![
            project("late", "/w/b", Some(1_760_000_002_000)),
            project("early", "/w/a", Some(1_760_000_001_000)),
        ];
        let (workspace, cx, feed) = serving(projects, cx);
        workspace.update(cx, WorkspaceView::view_publish);
        let mut spaces = feed.said_spaces().expect("said").to_vec();
        slopty_view_proto::view::sort_spaces(&mut spaces);
        let order: Vec<(&str, i64)> =
            spaces.iter().map(|s| (s.id.as_str(), s.created_at.timestamp_millis())).collect();
        assert_eq!(order, [("early", 1_760_000_001_000), ("late", 1_760_000_002_000)]);
    }
}
