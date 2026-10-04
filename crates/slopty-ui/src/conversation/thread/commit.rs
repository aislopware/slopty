//! The commit sheet: the repository a thread works in, as the person commits, pushes and opens
//! or merges its branch's pull request, from the thread's tile or its review's.
//!
//! It opens on the changed files, every one ticked, and an empty message: a commit takes the
//! person's words, so nothing is suggested. Over the files stands the branch's pull request, its
//! checks most pressing first, and a merge that is offered only while the forge says it is
//! ready, always for the head the person is looking at. What git or gh said when it refused is
//! shown in its own words, in the code face, under the buttons.
//!
//! It is drawn over its tile on a scrim of the tile alone, so the rest of the workspace stays
//! in reach. Its state is the hub's ([`super::git::GitBook`]): two tiles on one repository show
//! one answer.

use std::collections::HashSet;

use gpui::accesskit::{Role, Toggled};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, Context, Div, ElementId, Entity, EventEmitter, FocusHandle,
    Focusable, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Window, div, px,
    uniform_list,
};
use gpui_kit::component::input::{self, Input, InputState, Textarea, TextareaState};
use gpui_kit::component::{Sizable as _, Size};
use slopty_proto::RequestId;
use slopty_proto::git::{CheckBucket, GitOp, GitStatus, PullCheck, PullStanding, PullStatus};
use slopty_theme::{Rgb, Theme, Typography, alpha};

use super::git::{self, Method, Pull, Repo, Said};
use super::hub::{HubEvent, ThreadHub};
use crate::colors::hsla;
use crate::icons::{IconName, IconSize};
use crate::kit::{self, ButtonKind};

/// File rows shown before the list scrolls.
const FILES_SHOWN: usize = 8;

/// Checks shown, the most pressing first, before "+N more checks".
const CHECKS_SHOWN: usize = 6;

/// What the sheet tells its tile.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CommitEvent {
    /// The person closed it.
    Close,
}

/// Which face the sheet shows.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Page {
    /// The files, the message, commit and push, and the pull request over them.
    Commit,
    /// A pull request's title, description, base and draft, to open it.
    Open,
}

/// The commit sheet over a tile.
pub struct CommitSheet {
    hub: Entity<ThreadHub>,
    /// The folder the thread works in: the repository is the one holding it.
    repo: String,
    theme: Theme,
    page: Page,
    message: Entity<TextareaState>,
    title: Entity<InputState>,
    body: Entity<TextareaState>,
    base: Entity<InputState>,
    draft: bool,
    /// Files the person unticked, by path: every other one goes in the commit.
    unticked: HashSet<String>,
    method: Method,
    methods_open: bool,
    delete_branch: bool,
    /// The commit this sheet asked for: its message goes once it is made.
    committing: Option<RequestId>,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl std::fmt::Debug for CommitSheet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CommitSheet").field("repo", &self.repo).finish_non_exhaustive()
    }
}

impl EventEmitter<CommitEvent> for CommitSheet {}

impl Focusable for CommitSheet {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.message.focus_handle(cx)
    }
}

impl CommitSheet {
    /// The sheet for the repository holding `repo`, on `hub`'s worker: its status and its
    /// branch's pull request are asked at once, and the message field has the keyboard.
    pub fn new(
        hub: Entity<ThreadHub>,
        repo: String,
        theme: Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let message =
            cx.new(|cx| TextareaState::new(window, cx).placeholder("Message").auto_grow(2, 6));
        let title = cx.new(|cx| InputState::new(window, cx).placeholder("Title"));
        let body =
            cx.new(|cx| TextareaState::new(window, cx).placeholder("Description").auto_grow(3, 8));
        let base = cx.new(|cx| InputState::new(window, cx).placeholder("Base branch"));
        let watched = repo.clone();
        let hearing = cx.subscribe_in(&hub, window, move |this, _hub, event, window, cx| {
            if matches!(event, HubEvent::Git(r) if *r == watched) {
                this.heard(window, cx);
            }
        });
        let typing = [
            cx.observe(&message, |_, _, cx| cx.notify()),
            cx.observe(&title, |_, _, cx| cx.notify()),
            cx.observe(&body, |_, _, cx| cx.notify()),
        ];
        let sheet = Self {
            hub,
            repo,
            theme,
            page: Page::Commit,
            message,
            title,
            body,
            base,
            draft: false,
            unticked: HashSet::new(),
            method: Method::default(),
            methods_open: false,
            delete_branch: true,
            committing: None,
            focus: cx.focus_handle(),
            _subscriptions: std::iter::once(hearing).chain(typing).collect(),
        };
        sheet.refresh(cx);
        sheet.message.update(cx, |m, cx| m.focus(window, cx));
        sheet
    }

    /// The folder whose repository it works.
    #[must_use]
    pub fn repo(&self) -> &str {
        &self.repo
    }

    /// Draw in `theme`.
    pub fn set_theme(&mut self, theme: Theme, cx: &mut Context<Self>) {
        self.theme = theme;
        cx.notify();
    }

    /// Ask the status and the pull request again: the sheet opening, or the person's refresh.
    pub fn refresh(&self, cx: &mut Context<Self>) {
        let repo = self.repo.clone();
        self.hub.update(cx, |hub, cx| {
            let _status = hub.git_op(&repo, GitOp::Status, cx);
            let _pull = hub.git_op(&repo, GitOp::PullStatus, cx);
        });
    }

    fn state<'a>(&self, cx: &'a App) -> Option<&'a Repo> {
        self.hub.read(cx).git().repo(&self.repo)
    }

    fn busy<'a>(&self, cx: &'a App) -> Option<&'a GitOp> {
        self.hub.read(cx).git().busy(&self.repo)
    }

    /// The repository said something: a commit this sheet made takes its message away, and a
    /// pull request opened goes back to the files.
    fn heard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let said = self.state(cx).and_then(|r| r.said.clone());
        if let Some((request, said)) = said {
            if Some(request) == self.committing {
                self.committing = None;
                if matches!(said, Said::Committed { .. }) {
                    self.message.update(cx, |m, cx| m.set_value("", window, cx));
                }
            }
            if matches!(said, Said::Opened { .. }) && self.page == Page::Open {
                self.page = Page::Commit;
                for field in [&self.title, &self.base] {
                    field.update(cx, |f, cx| f.set_value("", window, cx));
                }
                self.body.update(cx, |b, cx| b.set_value("", window, cx));
            }
        }
        let present: HashSet<&str> = self
            .state(cx)
            .and_then(|r| r.status.as_deref())
            .map(|s| s.files.iter().map(|f| f.path.as_str()).collect())
            .unwrap_or_default();
        let unticked = std::mem::take(&mut self.unticked);
        self.unticked = unticked.into_iter().filter(|p| present.contains(p.as_str())).collect();
        cx.notify();
    }

    // ----- what the person does --------------------------------------------------------

    /// The paths the commit takes: every file ticked, a renamed one by both its paths.
    fn chosen(&self, status: &GitStatus) -> Vec<String> {
        status
            .files
            .iter()
            .filter(|f| !self.unticked.contains(&f.path))
            .flat_map(|f| f.from.iter().cloned().chain([f.path.clone()]))
            .collect()
    }

    fn toggle_file(&mut self, path: String, cx: &mut Context<Self>) {
        if !self.unticked.remove(&path) {
            self.unticked.insert(path);
        }
        cx.notify();
    }

    fn toggle_all(&mut self, cx: &mut Context<Self>) {
        let Some(status) = self.state(cx).and_then(|r| r.status.clone()) else { return };
        if self.unticked.is_empty() {
            self.unticked = status.files.iter().map(|f| f.path.clone()).collect();
        } else {
            self.unticked.clear();
        }
        cx.notify();
    }

    /// Commit the files ticked with the person's message, and push once it is made when
    /// `push` says.
    fn commit(&mut self, push: bool, cx: &mut Context<Self>) {
        let Some(status) = self.state(cx).and_then(|r| r.status.clone()) else { return };
        let paths = self.chosen(&status);
        let message = self.message.read(cx).value().to_string();
        if paths.is_empty() || message.trim().is_empty() || self.busy(cx).is_some() {
            return;
        }
        let repo = self.repo.clone();
        self.committing = self.hub.update(cx, |hub, cx| {
            if push {
                hub.commit_and_push(&repo, paths, message, cx)
            } else {
                hub.git_op(&repo, GitOp::Commit { paths, message }, cx)
            }
        });
    }

    fn op(&self, op: GitOp, cx: &mut Context<Self>) {
        if self.busy(cx).is_some() {
            return;
        }
        let repo = self.repo.clone();
        let _asked = self.hub.update(cx, |hub, cx| hub.git_op(&repo, op, cx));
    }

    fn open_pull_request(&self, cx: &mut Context<Self>) {
        let base = self.base.read(cx).value().trim().to_owned();
        self.op(
            GitOp::PullRequest {
                title: self.title.read(cx).value().trim().to_owned(),
                body: self.body.read(cx).value().to_string(),
                base: (!base.is_empty()).then_some(base),
                draft: self.draft,
            },
            cx,
        );
    }

    /// Merge the pull request the person is looking at, on their press alone.
    fn merge(&mut self, cx: &mut Context<Self>) {
        let Some(pull) = self.state(cx).and_then(|r| r.pull.status()) else { return };
        if pull.standing() != PullStanding::Ready {
            return;
        }
        let op = GitOp::Merge {
            method: self.method.wire().to_owned(),
            head: Some(pull.head_commit.clone()),
            delete_branch: self.delete_branch,
        };
        self.methods_open = false;
        self.op(op, cx);
    }

    fn show_page(&mut self, page: Page, window: &mut Window, cx: &mut Context<Self>) {
        self.page = page;
        let field = match page {
            Page::Commit => self.message.focus_handle(cx),
            Page::Open => self.title.focus_handle(cx),
        };
        window.focus(&field, cx);
        cx.notify();
    }

    fn close(cx: &mut Context<Self>) {
        cx.emit(CommitEvent::Close);
    }

    // ----- drawing: pieces -------------------------------------------------------------

    fn icon(&self, name: IconName, tone: Rgb) -> gpui::Svg {
        crate::icons::icon(&self.theme, name, IconSize::Inline, hsla(tone))
            .size(px(self.theme.typography.icon()))
    }

    fn mono(&self) -> SharedString {
        self.theme.typography.mono_families.first().cloned().unwrap_or_default().into()
    }

    /// A button that does nothing while `off`, set back so it reads so.
    fn button(
        &self,
        id: &'static str,
        label: &'static str,
        kind: ButtonKind,
        off: bool,
    ) -> gpui::Stateful<Div> {
        kit::button(&self.theme, id, label, kind)
            .when(off, |el| el.opacity(alpha::PRESSED).cursor_default())
    }

    /// A tick box and its words, pressed as one.
    fn tick(
        &self,
        id: impl Into<SharedString>,
        label: SharedString,
        on: bool,
    ) -> gpui::Stateful<Div> {
        let s = self.theme.surfaces;
        let id = id.into();
        let selector = id.to_string();
        crate::a11y::tab_stop(
            div()
                .id(ElementId::Name(id))
                .debug_selector(move || selector)
                .role(Role::CheckBox)
                .aria_label(label)
                .aria_toggled(if on { Toggled::True } else { Toggled::False })
                .flex_none()
                .flex()
                .items_center()
                .gap(px(self.theme.spacing.xs))
                .cursor_pointer()
                .child(kit::tick_box(&self.theme, on, 1.0)),
            s.accent,
        )
    }

    /// Words in the code face in a well ([`kit::inset`]): what git or gh said, as it said it.
    fn said_block(&self, id: &'static str, words: &str, tone: Rgb) -> Div {
        let theme = &self.theme;
        div()
            .debug_selector(move || id.to_owned())
            .w_full()
            .px(px(theme.spacing.sm))
            .py(px(theme.spacing.xs))
            .rounded(px(theme.radii.sm))
            .map(|el| kit::inset(el, theme))
            .font_family(self.mono())
            .text_size(px(theme.typography.small()))
            .text_color(hsla(tone))
            .whitespace_normal()
            .child(SharedString::from(words.trim_end().to_owned()))
    }

    /// A line under a part's head: quiet words in the meta size.
    fn quiet(&self, id: &'static str, words: impl Into<SharedString>) -> Div {
        kit::meta(div(), &self.theme)
            .debug_selector(move || id.to_owned())
            .min_w_0()
            .child(words.into())
    }

    // ----- drawing: the parts -----------------------------------------------------------

    fn head(&self, cx: &Context<Self>) -> Div {
        let theme = &self.theme;
        let s = theme.surfaces;
        let status = self.state(cx).and_then(|r| r.status.as_deref());
        let title = match self.page {
            Page::Commit => "Commit",
            Page::Open => "Open pull request",
        };
        kit::inset_x(div(), theme)
            .w_full()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(theme.spacing.sm))
            .min_h(px(kit::Row::Two.height(theme)))
            .child(kit::title(theme, title).flex_none())
            .child(
                div()
                    .debug_selector(|| "commit-branch".to_owned())
                    .min_w_0()
                    .flex_1()
                    .flex()
                    .items_center()
                    .gap(px(theme.spacing.xxs))
                    .text_size(px(theme.typography.small()))
                    .text_color(hsla(s.text_muted))
                    .children(status.map(|_| self.icon(IconName::GitBranch, s.text_muted)))
                    .children(status.map(|st| {
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .child(SharedString::from(git::branch_line(st)))
                    })),
            )
            .child(
                kit::icon_button(theme, "commit-refresh", IconName::RotateCw, "Refresh")
                    .on_click(cx.listener(|this, _ev, _w, cx| this.refresh(cx))),
            )
            .child(
                kit::icon_button(theme, "commit-close", IconName::X, "Close")
                    .on_click(cx.listener(|_this, _ev, _w, cx| Self::close(cx))),
            )
    }

    /// The branch's pull request: its number and title, where it stands, its checks, and the
    /// merge while it is ready.
    fn pull_part(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let theme = &self.theme;
        let s = theme.surfaces;
        let repo = self.state(cx)?;
        let reading = self.hub.read(cx).git().reading(&self.repo, true);
        let part = kit::inset_x(div(), theme)
            .debug_selector(|| "commit-pull".to_owned())
            .w_full()
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(theme.spacing.xs))
            .py(px(theme.spacing.sm));
        let part = match (&repo.pull, &repo.pull_unread) {
            (Pull::Known(pull), _) => part.child(self.pull_line(pull)).children(self.checks(pull)),
            (_, Some(why)) => {
                part.child(self.said_block("commit-pull-unread", why, s.text_secondary))
            }
            (Pull::None, None) => part.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(theme.spacing.xs))
                    .child(self.icon(IconName::GitPullRequest, s.text_muted))
                    .child(self.quiet("commit-no-pull", "No pull request for this branch")),
            ),
            (Pull::Unknown, None) if reading => {
                part.child(self.quiet("commit-pull-reading", "Reading the pull request\u{2026}"))
            }
            (Pull::Unknown, None) => return None,
        };
        let merge = repo.pull.status().and_then(|pull| self.merge_row(pull, repo, cx));
        Some(part.children(merge).into_any_element())
    }

    fn pull_line(&self, pull: &PullStatus) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let standing = pull.standing();
        let tone = standing_tone(theme, standing);
        let icon = match standing {
            PullStanding::Merged => IconName::GitMerge,
            PullStanding::Draft => IconName::GitPullRequestDraft,
            _ => IconName::GitPullRequest,
        };
        let url = pull.url.clone();
        div()
            .w_full()
            .flex()
            .items_center()
            .gap(px(theme.spacing.xs))
            .text_size(px(theme.typography.small()))
            .child(self.icon(icon, tone))
            .child(
                div()
                    .id("commit-pull-number")
                    .debug_selector(|| "commit-pull-number".to_owned())
                    .role(Role::Link)
                    .aria_label(SharedString::from(format!("Pull request {}", pull.number)))
                    .flex_none()
                    .cursor_pointer()
                    .text_color(hsla(s.text_secondary))
                    .hover(gpui::Styled::underline)
                    .child(SharedString::from(format!("#{}", pull.number)))
                    .on_click(move |_ev, _w, cx| cx.open_url(&url)),
            )
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .font_weight(FontWeight(Typography::MEDIUM_WEIGHT))
                    .child(SharedString::from(pull.title.clone())),
            )
            .child(
                kit::pill(theme, tone, 1.0)
                    .debug_selector(|| "commit-pull-standing".to_owned())
                    .child(SharedString::from(git::standing_words(pull))),
            )
            .into_any_element()
    }

    /// The checks, the most pressing first; each opens its page.
    fn checks(&self, pull: &PullStatus) -> Vec<AnyElement> {
        let theme = &self.theme;
        let s = theme.surfaces;
        let mut checks: Vec<&PullCheck> = pull.checks.iter().collect();
        checks.sort_by_key(|c| std::cmp::Reverse(c.bucket()));
        let past = u64::try_from(checks.len().saturating_sub(CHECKS_SHOWN)).unwrap_or(u64::MAX);
        let more = past.saturating_add(u64::from(pull.more_checks));
        let mut rows: Vec<AnyElement> = checks
            .into_iter()
            .take(CHECKS_SHOWN)
            .enumerate()
            .map(|(ix, check)| {
                let (icon, tone) = match check.bucket() {
                    CheckBucket::Failed => (IconName::CircleX, s.error),
                    CheckBucket::Running => (IconName::CircleDashed, s.text_secondary),
                    CheckBucket::Passed => (IconName::CircleCheck, s.text_muted),
                    CheckBucket::Skipped => (IconName::Minus, s.text_muted),
                };
                let link = check.link.clone();
                div()
                    .id(ElementId::Name(format!("commit-check-{ix}").into()))
                    .debug_selector(move || format!("commit-check-{ix}"))
                    .role(if link.is_some() { Role::Link } else { Role::ListItem })
                    .aria_label(SharedString::from(format!("{}, {}", check.name, check.state)))
                    .w_full()
                    .flex()
                    .items_center()
                    .gap(px(theme.spacing.xs))
                    .pl(px(theme.spacing.xxs))
                    .text_size(px(theme.typography.small()))
                    .text_color(hsla(s.text_secondary))
                    .child(self.icon(icon, tone))
                    .child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .child(SharedString::from(check.name.clone())),
                    )
                    .children(check.workflow.clone().map(|w| {
                        div()
                            .min_w_0()
                            .flex_shrink_1()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_color(hsla(s.text_muted))
                            .child(SharedString::from(w))
                    }))
                    .when_some(link, |el, link| {
                        el.cursor_pointer()
                            .hover(move |el| el.text_color(hsla(s.text)))
                            .on_click(move |_ev, _w, cx| cx.open_url(&link))
                    })
                    .into_any_element()
            })
            .collect();
        if more > 0 {
            let words = format!("+{more} more checks");
            rows.push(
                self.quiet("commit-more-checks", words)
                    .pl(px(theme.spacing.xxs))
                    .into_any_element(),
            );
        }
        rows
    }

    /// The merge, while the pull request is open: offered only while it is ready, else the
    /// reason it is not, in its standing's words.
    fn merge_row(&self, pull: &PullStatus, repo: &Repo, cx: &Context<Self>) -> Option<AnyElement> {
        let theme = &self.theme;
        let s = theme.surfaces;
        let standing = pull.standing();
        if matches!(standing, PullStanding::Merged | PullStanding::Closed) {
            return None;
        }
        if let Some(why) = &repo.no_gh {
            return Some(self.said_block("commit-no-gh", why, s.text_secondary).into_any_element());
        }
        if standing != PullStanding::Ready {
            let words = format!("Merge waits: {}", git::standing_words(pull).to_lowercase());
            return Some(self.quiet("commit-merge-waits", words).into_any_element());
        }
        let busy = self.busy(cx).is_some();
        let method = self.method;
        let split = div()
            .flex_none()
            .flex()
            .items_center()
            .gap(kit::hair(theme))
            .child(
                self.button("commit-merge", method.verb(), ButtonKind::Primary, busy)
                    .rounded_r(px(0.0))
                    .on_click(cx.listener(|this, _ev, _w, cx| this.merge(cx))),
            )
            .child(
                kit::solid_pressable(
                    div()
                        .id("commit-merge-methods")
                        .debug_selector(|| "commit-merge-methods".to_owned())
                        .role(Role::Button)
                        .aria_label("Merge method")
                        .aria_expanded(self.methods_open)
                        .flex_none()
                        .h(px(theme.density.control))
                        .px(px(theme.spacing.xs))
                        .flex()
                        .items_center()
                        .rounded_r(px(theme.radii.sm))
                        .cursor_pointer(),
                    theme,
                )
                .child(self.icon(IconName::ChevronDown, s.solid_ink))
                .on_click(cx.listener(|this, _ev, _w, cx| {
                    this.methods_open = !this.methods_open;
                    cx.notify();
                })),
            );
        let delete = self
            .tick("commit-delete-branch", "Delete branch".into(), self.delete_branch)
            .text_size(px(theme.typography.small()))
            .text_color(hsla(s.text_secondary))
            .child("Delete branch")
            .on_click(cx.listener(|this, _ev, _w, cx| {
                this.delete_branch = !this.delete_branch;
                cx.notify();
            }));
        let menu = self.methods_open.then(|| self.methods_menu(cx));
        Some(
            div()
                .w_full()
                .flex()
                .flex_col()
                .gap(px(theme.spacing.xs))
                .pt(px(theme.spacing.xs))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(theme.spacing.md))
                        .child(split)
                        .child(delete),
                )
                .children(menu)
                .into_any_element(),
        )
    }

    /// The merge's methods, under the split button: the one chosen ticked.
    fn methods_menu(&self, cx: &Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        kit::elevate(div(), theme)
            .id("commit-methods")
            .debug_selector(|| "commit-methods".to_owned())
            .role(Role::Menu)
            .aria_label("Merge method")
            .w(px(kit::Overlay::List.bounds().0 / 2.0))
            .p(px(kit::sheet_pad(theme)))
            .rounded(px(theme.radii.lg))
            .children(Method::ALL.into_iter().map(|method| {
                let on = method == self.method;
                kit::sheet_row(theme, kit::Row::One)
                    .id(ElementId::Name(format!("commit-method-{}", method.wire()).into()))
                    .debug_selector(move || format!("commit-method-{}", method.wire()))
                    .role(Role::MenuItemRadio)
                    .aria_label(method.verb())
                    .aria_toggled(if on { Toggled::True } else { Toggled::False })
                    .cursor_pointer()
                    .text_size(px(theme.typography.ui_size))
                    .hover(move |el| el.bg(hsla(s.hover)))
                    .child(div().flex_1().child(method.verb()))
                    .children(on.then(|| self.icon(IconName::Check, s.text_secondary)))
                    .on_click(cx.listener(move |this, _ev, _w, cx| {
                        this.method = method;
                        this.methods_open = false;
                        cx.notify();
                    }))
            }))
            .into_any_element()
    }

    /// The changed files, each ticked to go in the commit.
    fn files_part(&self, cx: &Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let repo = self.state(cx);
        let part = kit::inset_x(div(), theme)
            .debug_selector(|| "commit-files".to_owned())
            .w_full()
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(theme.spacing.xxs))
            .py(px(theme.spacing.sm));
        if let Some(why) = repo.and_then(|r| r.unread.as_deref()) {
            return part
                .child(self.said_block("commit-unread", why, s.text_secondary))
                .into_any_element();
        }
        let Some(status) = repo.and_then(|r| r.status.as_deref()) else {
            return part
                .child(self.quiet("commit-reading", "Reading the changes\u{2026}"))
                .into_any_element();
        };
        if status.files.is_empty() {
            return part.child(self.quiet("commit-clean", "Nothing to commit")).into_any_element();
        }
        let total = status.files.len();
        let chosen = status.files.iter().filter(|f| !self.unticked.contains(&f.path)).count();
        let count = if chosen == total {
            files_words(total)
        } else {
            format!("{chosen} of {}", files_words(total))
        };
        let all = self
            .tick("commit-all", "Every file".into(), chosen == total)
            .when(chosen > 0 && chosen < total, |el| el.aria_toggled(Toggled::Mixed))
            .text_size(px(theme.typography.small()))
            .text_color(hsla(s.text_muted))
            .child(SharedString::from(count))
            .on_click(cx.listener(|this, _ev, _w, cx| this.toggle_all(cx)));
        let row = px(kit::Row::One.height(theme));
        #[expect(clippy::cast_precision_loss, reason = "a handful of rows")]
        let shown = total.min(FILES_SHOWN) as f32;
        let list = uniform_list(
            "commit-file-rows",
            total,
            cx.processor(|this, range: std::ops::Range<usize>, _window, cx| {
                range.filter_map(|ix| this.file_row(ix, cx)).collect::<Vec<_>>()
            }),
        )
        .w_full()
        .h(row * shown);
        let more = (status.more > 0)
            .then(|| self.quiet("commit-more-files", format!("+{} more", status.more)));
        part.child(all).child(list).children(more).into_any_element()
    }

    fn file_row(&self, ix: usize, cx: &Context<Self>) -> Option<AnyElement> {
        let theme = &self.theme;
        let s = theme.surfaces;
        let file = self.state(cx)?.status.as_ref()?.files.get(ix)?;
        let on = !self.unticked.contains(&file.path);
        let letters = git::letters(&file.xy);
        let tone = match letters.as_str() {
            "D" => s.error,
            "new" | "A" => s.success,
            l if l.len() == 2 => s.warn,
            _ => s.text_muted,
        };
        let name = match &file.from {
            Some(from) => format!("{from} \u{2192} {}", file.path),
            None => file.path.clone(),
        };
        let path = file.path.clone();
        Some(
            self.tick(format!("commit-file-{ix}"), SharedString::from(name.clone()), on)
                .w_full()
                .h(px(kit::Row::One.height(theme)))
                .rounded(px(theme.radii.sm))
                .hover(move |el| el.bg(hsla(s.hover)))
                .text_size(px(theme.typography.small()))
                .child(
                    kit::tabular(div())
                        .flex_none()
                        .w(px(theme.spacing.xl))
                        .text_color(hsla(tone))
                        .child(SharedString::from(letters)),
                )
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .text_color(hsla(if on { s.text } else { s.text_muted }))
                        .child(SharedString::from(name)),
                )
                .on_click(cx.listener(move |this, _ev, _w, cx| this.toggle_file(path.clone(), cx)))
                .into_any_element(),
        )
    }

    /// A field on the sunk well, the kit's one inset round it.
    fn field(&self, child: impl IntoElement) -> Div {
        let theme = &self.theme;
        kit::sunk(div(), theme, theme.hair())
            .w_full()
            .rounded(px(theme.radii.md))
            .border(kit::hair(theme))
            .border_color(hsla(theme.surfaces.border))
            .px(px(theme.spacing.xs))
            .child(child)
    }

    /// The message and the buttons under it.
    fn commit_foot(&self, cx: &Context<Self>) -> Div {
        let theme = &self.theme;
        let repo = self.state(cx);
        let status = repo.and_then(|r| r.status.as_deref());
        let busy = self.busy(cx).is_some();
        let chosen = status.map(|st| self.chosen(st)).unwrap_or_default();
        let worded = !self.message.read(cx).value().trim().is_empty();
        let off = busy || chosen.is_empty() || !worded;
        // With nothing to commit and the branch ahead of its upstream, or with none, the second
        // way is the push alone.
        let push_only = chosen.is_empty()
            && status
                .is_some_and(|st| st.branch.is_some() && (st.ahead > 0 || st.upstream.is_none()));
        let no_pull = repo.is_some_and(|r| matches!(r.pull, Pull::None));
        let no_gh = repo.is_some_and(|r| r.no_gh.is_some());
        let second = if push_only {
            self.button("commit-push", "Push", ButtonKind::Secondary, busy)
                .on_click(cx.listener(|this, _ev, _w, cx| this.op(GitOp::Push, cx)))
        } else {
            self.button("commit-and-push", "Commit and push", ButtonKind::Secondary, off)
                .on_click(cx.listener(|this, _ev, _w, cx| this.commit(true, cx)))
        };
        kit::inset_x(div(), theme)
            .w_full()
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(theme.spacing.sm))
            .pt(px(theme.spacing.sm))
            .pb(px(theme.spacing.md))
            .child(
                self.field(
                    Textarea::new(&self.message)
                        .with_size(Size::Small)
                        .appearance(false)
                        .bordered(false)
                        .aria_label("Commit message"),
                ),
            )
            .child(
                div()
                    .w_full()
                    .flex()
                    .items_center()
                    .gap(px(theme.spacing.xs))
                    .when(no_pull, |el| {
                        el.child(
                            self.button(
                                "commit-open-pull",
                                "Open pull request",
                                ButtonKind::Ghost,
                                no_gh || busy,
                            )
                            .on_click(cx.listener(
                                move |this, _ev, window, cx| {
                                    if !no_gh {
                                        this.show_page(Page::Open, window, cx);
                                    }
                                },
                            )),
                        )
                    })
                    .child(div().flex_1())
                    .child(second)
                    .child(
                        self.button("commit-commit", "Commit", ButtonKind::Primary, off)
                            .on_click(cx.listener(|this, _ev, _w, cx| this.commit(false, cx))),
                    ),
            )
            .children(self.outcome(cx))
    }

    /// The pull request's title, description, base and draft, and the way to open it.
    fn open_page(&self, cx: &Context<Self>) -> Div {
        let theme = &self.theme;
        let s = theme.surfaces;
        let repo = self.state(cx);
        let busy = self.busy(cx).is_some();
        let no_gh = repo.and_then(|r| r.no_gh.clone());
        let draft = self
            .tick("commit-draft", "Open as a draft".into(), self.draft)
            .text_size(px(theme.typography.small()))
            .text_color(hsla(s.text_secondary))
            .child("Open as a draft")
            .on_click(cx.listener(|this, _ev, _w, cx| {
                this.draft = !this.draft;
                cx.notify();
            }));
        kit::inset_x(div(), theme)
            .w_full()
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(theme.spacing.sm))
            .py(px(theme.spacing.sm))
            .child(self.field(
                Input::new(&self.title).appearance(false).bordered(false).aria_label("Title"),
            ))
            .child(self.quiet(
                "commit-title-hint",
                "Left empty, the title and description come from the commits",
            ))
            .child(
                self.field(
                    Textarea::new(&self.body)
                        .with_size(Size::Small)
                        .appearance(false)
                        .bordered(false)
                        .aria_label("Description"),
                ),
            )
            .child(self.field(
                Input::new(&self.base).appearance(false).bordered(false).aria_label("Base branch"),
            ))
            .child(draft)
            .children(no_gh.map(|why| self.said_block("commit-no-gh", &why, s.text_secondary)))
            .child(
                div()
                    .w_full()
                    .flex()
                    .items_center()
                    .gap(px(theme.spacing.xs))
                    .child(div().flex_1())
                    .child(self.button("commit-back", "Back", ButtonKind::Ghost, false).on_click(
                        cx.listener(|this, _ev, window, cx| {
                            this.show_page(Page::Commit, window, cx);
                        }),
                    ))
                    .child(
                        self.button(
                            "commit-open",
                            "Open pull request",
                            ButtonKind::Primary,
                            busy || repo.is_some_and(|r| r.no_gh.is_some()),
                        )
                        .on_click(cx.listener(|this, _ev, _w, cx| this.open_pull_request(cx))),
                    ),
            )
            .children(self.outcome(cx))
    }

    /// What the last op came to, or what is on its way: under the buttons, a refusal in git's
    /// or gh's words.
    fn outcome(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let theme = &self.theme;
        let s = theme.surfaces;
        if let Some(op) = self.busy(cx) {
            let words = match op {
                GitOp::Commit { .. } => "Committing\u{2026}",
                GitOp::Push => "Pushing\u{2026}",
                GitOp::PullRequest { .. } => "Opening the pull request\u{2026}",
                GitOp::Merge { .. } => "Merging\u{2026}",
                GitOp::Status | GitOp::PullStatus => return None,
            };
            return Some(self.quiet("commit-busy", words).into_any_element());
        }
        let (_, said) = self.state(cx)?.said.as_ref()?;
        let line = |icon: IconName, words: String| {
            div()
                .id("commit-said")
                .debug_selector(|| "commit-said".to_owned())
                .role(Role::Status)
                .flex()
                .items_center()
                .gap(px(theme.spacing.xs))
                .text_size(px(theme.typography.small()))
                .text_color(hsla(s.text_secondary))
                .child(self.icon(icon, s.success))
                .child(SharedString::from(words))
        };
        Some(match said {
            Said::Committed { commit, files } => {
                let short: String = commit.chars().take(7).collect();
                line(
                    IconName::CircleCheck,
                    format!("Committed {short} \u{b7} {}", files_words(*files as usize)),
                )
                .into_any_element()
            }
            Said::Pushed { to } => {
                line(IconName::Upload, format!("Pushed to {to}")).into_any_element()
            }
            Said::Opened { url } => {
                let open = url.clone();
                line(IconName::GitPullRequest, "Opened".to_owned())
                    .child(
                        div()
                            .id("commit-opened")
                            .debug_selector(|| "commit-opened".to_owned())
                            .role(Role::Link)
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .cursor_pointer()
                            .text_color(hsla(s.text))
                            .hover(gpui::Styled::underline)
                            .child(SharedString::from(url.clone()))
                            .on_click(move |_ev, _w, cx| cx.open_url(&open)),
                    )
                    .into_any_element()
            }
            Said::Merged { said } => {
                self.said_block("commit-merged", said, s.text_secondary).into_any_element()
            }
            Said::Refused { why } => {
                self.said_block("commit-refused", why, s.error).into_any_element()
            }
            Said::Failed { said } => {
                self.said_block("commit-failed", said, s.error).into_any_element()
            }
        })
    }
}

impl Render for CommitSheet {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme.clone();
        let s = theme.surfaces;
        let body = match self.page {
            Page::Commit => div()
                .w_full()
                .min_h_0()
                .flex()
                .flex_col()
                .children(self.pull_part(cx))
                .child(kit::rule(&theme, s.border_subtle))
                .child(self.files_part(cx))
                .child(kit::rule(&theme, s.border_subtle))
                .child(self.commit_foot(cx)),
            Page::Open => self.open_page(cx),
        };
        // The tile under the sheet dims, and a press on it closes the sheet.
        div()
            .id("commit-scrim")
            .absolute()
            .inset_0()
            .flex()
            .flex_col()
            .items_center()
            .px(px(theme.spacing.md))
            .pt(px(theme.spacing.xxxl))
            .bg(kit::scrim(&theme))
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|_this, _ev, _w, cx| Self::close(cx)),
            )
            .child(
                kit::dialog(&theme, kit::Overlay::List)
                    .id("commit-sheet")
                    .debug_selector(|| "commit-sheet".to_owned())
                    .track_focus(&self.focus)
                    .role(Role::Dialog)
                    .aria_label("Commit")
                    .capture_action(cx.listener(|this, _: &input::Escape, _w, cx| {
                        if this.methods_open {
                            this.methods_open = false;
                            cx.notify();
                        } else {
                            Self::close(cx);
                        }
                        cx.stop_propagation();
                    }))
                    .overflow_y_scroll()
                    .child(self.head(cx))
                    .child(kit::rule(&theme, s.border_subtle))
                    .child(body),
            )
    }
}

/// "1 file", "3 files".
fn files_words(n: usize) -> String {
    kit::count(n as u64, "file", "files")
}

/// The tone a pull request's standing is drawn in: a failure or a conflict in the error tone,
/// changes asked for in the warning's, ready in the accent, the rest quiet.
pub(crate) const fn standing_tone(theme: &Theme, standing: PullStanding) -> Rgb {
    let s = theme.surfaces;
    match standing {
        PullStanding::Failing | PullStanding::Conflicting => s.error,
        PullStanding::ChangesRequested => s.warn,
        PullStanding::Ready | PullStanding::Merged => s.accent,
        PullStanding::Running
        | PullStanding::Waiting
        | PullStanding::Draft
        | PullStanding::Closed => s.text_muted,
    }
}
