//! The title bar's breadcrumb: where the focused work is, `workspace ▾ / checkout ▾ / branch`,
//! as Zed's title bar names its project and branch.
//!
//! When the workspace holds tiles on more than one worker, the focused tile's worker follows
//! it, so a strip of several machines never reads as one.
//!
//! The workspace leads in the medium weight: its name, and a menu of every workspace with
//! something on it and a new one, which is how the bar switches between them. What waits in
//! another workspace shows as its rollup's mark on this segment, so it is not lost from view.
//! The checkout is the focused shell's repository (else its directory), with a menu of the
//! same repository's other checkouts in the layout, on any worker, when there are some; with
//! no menu it is left out when the workspace already goes by its name. The
//! branch follows with what its working tree changed, as every diff's size is drawn. A segment
//! has a chevron only when it opens something: no branch list reaches the client, so the branch
//! is words.

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, InteractiveElement as _, IntoElement as _, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, canvas, div, px,
};
use slopty_client::layout::{Column, Tile, TileRef, WorkerKey};
use slopty_proto::items::ItemKind;
use slopty_proto::terminal::{RepoChanges, RepoId};
use slopty_theme::Typography;

use super::rollup::{Rollup, rollup_slot};
use super::strip::NEW_WORKSPACE;
use super::tile::place_name;
use super::titlebar::MenuKind;
use super::{MenuEntry, MenuGroup, WorkspaceView};
use crate::a11y::tab_stop;
use crate::colors::hsla;
use crate::draw::Draw;
use crate::icons::{IconName, IconSize, icon};
use crate::kit;

/// A checkout: a repository's working tree on one worker (or a directory outside one).
#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) struct Checkout {
    /// The worker it is on.
    pub worker: WorkerKey,
    /// Its root: the repository's, else the shell's directory.
    pub root: String,
    /// What the breadcrumb calls it: the root's last component.
    pub name: String,
    /// A tile whose shell is in it, which choosing it in the menu goes to.
    pub tile: TileRef,
}

/// What the breadcrumb says after the workspace, for the focused tile.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub(super) struct Crumbs {
    /// The focused shell's checkout.
    pub checkout: Option<Checkout>,
    /// The same repository's other checkouts in the layout, the focused one's first.
    pub checkouts: Vec<Checkout>,
    /// The branch checked out there, and what its working tree changed.
    pub branch: Option<(String, Option<RepoChanges>)>,
}

impl WorkspaceView {
    /// The checkout a shell tile is in, from its session's summary, and which repository it
    /// is when the worker has said.
    fn checkout_of(&self, tile: TileRef) -> Option<(Checkout, Option<RepoId>)> {
        let ItemKind::Terminal { session } = self.item(tile)?.kind else { return None };
        let summary = self.summary(session)?;
        let cwd = summary.cwd.as_deref()?;
        let home = self.home_of(tile.worker);
        let repo = summary.repo.as_deref();
        let name = place_name(cwd, repo, home)?;
        let root = repo.unwrap_or(cwd).trim_end_matches('/').to_owned();
        Some((Checkout { worker: tile.worker, root, name, tile }, summary.repo_id.clone()))
    }

    /// What the breadcrumb says for the focused tile.
    pub(super) fn crumbs(&self) -> Crumbs {
        let Some(focused) = self.focused() else { return Crumbs::default() };
        let Some((checkout, repo)) = self.checkout_of(focused) else { return Crumbs::default() };
        let branch = match self.item(focused).map(|i| &i.kind) {
            Some(ItemKind::Terminal { session }) => self
                .summary(*session)
                .and_then(|s| s.branch.clone().map(|b| (b, s.changes.filter(|c| c.files > 0)))),
            _ => None,
        };
        let mut checkouts = vec![checkout.clone()];
        if let Some(repo) = repo {
            let tiles = self
                .layout
                .workspaces()
                .iter()
                .flat_map(|ws| ws.columns().iter().flat_map(Column::tiles).map(Tile::tile))
                .collect::<Vec<_>>();
            for tile in tiles {
                let Some((other, same)) = self.checkout_of(tile) else { continue };
                let known =
                    checkouts.iter().any(|c| c.worker == other.worker && c.root == other.root);
                if same.is_some_and(|same| same.same(&repo)) && !known {
                    checkouts.push(other);
                }
            }
        }
        Crumbs { checkout: Some(checkout), checkouts, branch }
    }

    /// The focused tile's worker, when the active workspace holds tiles on more than one.
    fn crumb_worker(&self) -> Option<String> {
        let focused = self.focused()?;
        let ws = self.layout.workspaces().get(self.layout.active_workspace())?;
        let mut tiles = ws.columns().iter().flat_map(Column::tiles).map(Tile::tile);
        tiles.any(|t| t.worker != focused.worker).then(|| self.worker_name(focused.worker))
    }

    /// What the workspaces other than the active one add up to: what waits out of view.
    fn elsewhere(&self) -> Rollup {
        let active = self.layout.active_workspace();
        let mut all = Rollup::default();
        for ix in (0..self.layout.workspaces().len()).filter(|ix| *ix != active) {
            let (rollup, _) = self.workspace_rollup(ix);
            all.needs_you = all.needs_you.saturating_add(rollup.needs_you);
            all.working = all.working.saturating_add(rollup.working);
            all.unseen = all.unseen.saturating_add(rollup.unseen);
            all.running = all.running.saturating_add(rollup.running);
        }
        all
    }

    /// The breadcrumb, laid in the bar after the navigator's toggle.
    pub(super) fn render_breadcrumb(&self, cx: &Draw<'_, Self>) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let spacing = theme.spacing;
        let crumbs = self.crumbs();
        let step = || {
            div()
                .flex_none()
                .text_color(hsla(s.text_muted))
                .child(SharedString::from("/"))
                .into_any_element()
        };
        let ix = self.layout.active_workspace();
        let name = SharedString::from(self.workspace_name_at(ix));
        let elsewhere = self.elsewhere();
        let label = match elsewhere.words() {
            Some(words) => format!("{name}, elsewhere {words}"),
            None => name.to_string(),
        };
        let mut parts = vec![
            self.crumb(MenuKind::Workspaces, "crumb-workspace", label.into(), cx)
                .child(
                    div()
                        .debug_selector(|| "crumb-workspace-name".to_owned())
                        .font_weight(gpui::FontWeight(Typography::MEDIUM_WEIGHT))
                        .text_color(hsla(s.text))
                        .child(name.clone()),
                )
                .child(self.chevron())
                .when(elsewhere.shown().is_some(), |el| {
                    el.child(rollup_slot(theme, "crumb-elsewhere".to_owned(), elsewhere, true))
                })
                .into_any_element(),
        ];
        // A workspace that holds tiles on more than one worker names the focused tile's, so a
        // strip of three machines' checkouts of one repository does not read as one machine.
        if let Some(worker) = self.crumb_worker() {
            let words = self
                .words("crumb-worker", SharedString::from(format!("on {worker}")))
                .gap(px(spacing.xs))
                .child(
                    icon(theme, IconName::Server, IconSize::Inline, hsla(s.text_muted))
                        .size(px(theme.typography.icon())),
                )
                .child(SharedString::from(worker));
            parts.extend([step(), words.into_any_element()]);
        }
        // A workspace is named after its first shell's checkout until it is named otherwise,
        // and that name said twice in a row is noise; a menu of checkouts keeps its segment.
        let more = crumbs.checkouts.len() > 1;
        let checkout = crumbs.checkout.as_ref().filter(|c| more || c.name != *name);
        if let Some(checkout) = checkout {
            let name = SharedString::from(checkout.name.clone());
            let segment = if more {
                self.crumb(MenuKind::Checkouts, "crumb-checkout", name.clone(), cx)
                    .child(name)
                    .child(self.chevron())
                    .into_any_element()
            } else {
                self.words("crumb-checkout", name.clone()).child(name).into_any_element()
            };
            parts.extend([step(), segment]);
        }
        if let Some((branch, changes)) = crumbs.branch {
            let branch = SharedString::from(branch);
            let changes = changes.and_then(|c| kit::changes(theme, c.added, c.removed));
            let words = self
                .words("crumb-branch", SharedString::from(format!("branch {branch}")))
                .gap(px(spacing.xs))
                .child(
                    icon(theme, IconName::GitBranch, IconSize::Inline, hsla(s.text_muted))
                        .size(px(theme.typography.icon())),
                )
                .child(branch)
                .when_some(changes, |el, size| {
                    el.child(
                        div()
                            .debug_selector(|| "crumb-changes".to_owned())
                            .ml(px(spacing.xs))
                            .child(size),
                    )
                });
            parts.extend([step(), words.into_any_element()]);
        }
        div()
            .id("breadcrumb")
            .debug_selector(|| "breadcrumb".to_owned())
            .role(Role::Group)
            .aria_label("Where")
            .flex_shrink(1.0)
            .min_w_0()
            .overflow_hidden()
            .flex()
            .items_center()
            .gap(px(spacing.xxs))
            .text_size(px(theme.typography.ui_size))
            .text_color(hsla(s.text_secondary))
            .children(parts)
            .into_any_element()
    }

    /// A segment that opens `which`: a quiet button a row tall, hovered as a row is, its left
    /// edge noted for the menu to hang from.
    fn crumb(
        &self,
        which: MenuKind,
        selector: &'static str,
        label: SharedString,
        cx: &Draw<'_, Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let theme = &self.theme;
        let s = theme.surfaces;
        let anchors = std::rc::Rc::clone(&self.anchors.at);
        let measure = canvas(
            move |bounds, _window, _cx| {
                anchors.borrow_mut().insert(which, f32::from(bounds.origin.x));
            },
            |_bounds, (), _window, _cx| {},
        )
        .absolute()
        .inset_0();
        let el = div()
            .id(selector)
            .debug_selector(move || selector.to_owned())
            .role(Role::Button)
            .aria_label(label)
            .aria_expanded(self.menu == Some(which))
            .relative()
            .flex_none()
            .min_w_0()
            .h(px(theme.density.row))
            .px(px(theme.spacing.sm))
            .flex()
            .items_center()
            .gap(px(theme.spacing.xs))
            .rounded(px(theme.radii.sm))
            .whitespace_nowrap()
            .cursor_pointer()
            .hover(move |el| el.bg(hsla(s.raised)))
            .when(self.menu == Some(which), move |el| el.bg(hsla(s.raised)))
            .child(measure)
            .on_mouse_down(gpui::MouseButton::Left, |_ev, _w, cx| cx.stop_propagation())
            .on_click(
                cx.listener(move |this, _ev, window, cx| this.toggle_menu(which, window, cx)),
            );
        tab_stop(el, s.accent)
    }

    /// A segment that opens nothing: its words, on the same grid as a button's.
    fn words(&self, selector: &'static str, label: SharedString) -> gpui::Stateful<gpui::Div> {
        let theme = &self.theme;
        div()
            .id(selector)
            .debug_selector(move || selector.to_owned())
            .role(Role::Label)
            .aria_label(label)
            .flex_none()
            .min_w_0()
            .h(px(theme.density.row))
            .px(px(theme.spacing.sm))
            .flex()
            .items_center()
            .whitespace_nowrap()
    }

    /// The chevron of a segment that opens a menu.
    fn chevron(&self) -> gpui::Svg {
        let theme = &self.theme;
        icon(theme, IconName::ChevronDown, IconSize::Inline, hsla(theme.surfaces.text_muted))
            .size(px(theme.typography.caption()))
    }

    /// The workspace segment's menu: every workspace with something on it (the active one
    /// ticked, the others saying what waits in them), then a new one.
    pub(super) fn workspace_entries(&self, entity: &gpui::WeakEntity<Self>) -> Vec<MenuEntry> {
        let active = self.layout.active_workspace();
        let mut entries: Vec<MenuEntry> = self
            .tabbed_workspaces()
            .into_iter()
            .map(|ix| {
                let (rollup, count) = self.workspace_rollup(ix);
                let detail = if ix == active {
                    "\u{2713}".to_owned()
                } else {
                    rollup.words().unwrap_or_else(|| {
                        format!("{count} {}", if count == 1 { "tile" } else { "tiles" })
                    })
                };
                let entity = entity.clone();
                MenuEntry {
                    group: MenuGroup::Places,
                    label: self.workspace_name_at(ix).into(),
                    detail: detail.into(),
                    run: std::rc::Rc::new(move |_window: &mut Window, cx: &mut gpui::App| {
                        let _gone = entity.update(cx, |this, cx| this.go_to_workspace(ix, cx));
                    }),
                }
            })
            .collect();
        let entity = entity.clone();
        entries.push(MenuEntry {
            group: MenuGroup::Workspaces,
            label: NEW_WORKSPACE.into(),
            detail: SharedString::default(),
            run: std::rc::Rc::new(move |_window: &mut Window, cx: &mut gpui::App| {
                let _gone = entity.update(cx, |this, cx| {
                    // The layout always keeps an empty workspace last.
                    let last = this.layout.workspaces().len().saturating_sub(1);
                    this.go_to_workspace(last, cx);
                });
            }),
        });
        entries
    }

    /// The checkout segment's menu: the same repository's checkouts in the layout, each named
    /// with its worker, the focused one ticked; choosing one goes to a shell in it.
    pub(super) fn checkout_entries(&self, entity: &gpui::WeakEntity<Self>) -> Vec<MenuEntry> {
        let crumbs = self.crumbs();
        let current = crumbs.checkout;
        crumbs
            .checkouts
            .into_iter()
            .map(|c| {
                let worker = self.worker_name(c.worker);
                let detail = if current.as_ref() == Some(&c) {
                    format!("{worker} \u{2713}")
                } else {
                    worker
                };
                let entity = entity.clone();
                let tile = c.tile;
                MenuEntry {
                    group: MenuGroup::Places,
                    label: c.name.into(),
                    detail: detail.into(),
                    run: std::rc::Rc::new(move |_window: &mut Window, cx: &mut gpui::App| {
                        let _gone = entity.update(cx, |this, cx| this.focus_tile(tile, cx));
                    }),
                }
            })
            .collect()
    }
}
