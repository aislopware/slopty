//! What a request on its card asks the person to read before answering: the change an edit
//! would make, under the file it names, and a plan whole. Each stands in a well at most a
//! share of the window tall and scrolls past it, so the answers under it stay on screen.
//!
//! An agent whose request names its call (Codex, pi) is answered on the call's own line, where
//! the change already shows; this is for one whose request belongs to no call on show (Claude
//! Code's hooks), which put "Allow Edit?" with nothing to judge it by.

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, Context, ElementId, InteractiveElement as _, IntoElement as _, ParentElement as _,
    Pixels, SharedString, StatefulInteractiveElement as _, Styled as _, div, px, relative,
};
use slopty_proto::thread::{Clipped, ItemBody, ItemId, Patch, Request};

use super::tools::{opened_at, tidy};
use super::{ThreadView, ThreadViewEvent, path_patch};
use crate::colors::hsla;
use crate::conversation::lines;
use crate::kit;

/// The key a request's drawn change and its "Show all" are kept under, apart from any call's.
fn key(request: &Request) -> ItemId {
    ItemId(format!("ask-{}", request.id.0))
}

impl ThreadView {
    /// The file `request` would change: its call's, else that of the newest call in the thread
    /// carrying the same change (a hook names no call, but the transcript already holds it).
    pub(super) fn proposed_path(&self, request: &Request, cx: &gpui::App) -> Option<String> {
        let patch = request.proposed.as_ref()?;
        let state = self.state(cx)?;
        let of = |item: &slopty_proto::thread::Item| match &item.body {
            ItemBody::Tool(call) => path_patch(call).map(|(path, p)| (path.to_owned(), p == patch)),
            _ => None,
        };
        if let Some((path, _)) = request.item.as_ref().and_then(|id| state.item(id)).and_then(of) {
            return Some(path);
        }
        state.items.iter().rev().filter_map(of).find(|(_, same)| *same).map(|(path, _)| path)
    }

    /// The change `request` would make, under its file's line: the file's name (opening it),
    /// its folder muted, and how many lines it adds and takes away.
    pub(super) fn proposed_well(
        &self,
        request: &Request,
        patch: &Patch,
        most: Pixels,
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let cwd = self.state(cx).map(|st| st.meta.cwd.clone()).unwrap_or_default();
        let path = self.proposed_path(request, cx);
        let head = path.as_deref().map(|path| {
            let (name, dir) = lines::name_first(path);
            let dir = tidy(&format!("{dir}/"), &cwd).trim_end_matches('/').to_owned();
            let open = opened_at(path, &cwd);
            let selector = format!("proposed-file-{}", request.id.0);
            div()
                .w_full()
                .min_w_0()
                .flex()
                .items_center()
                .gap(px(theme.spacing.xs))
                .min_h(px(kit::Row::One.height(theme)))
                .text_size(px(theme.typography.small()))
                .child(crate::icons::symbol(
                    theme,
                    crate::icons::file_mark(path),
                    px(theme.typography.icon()),
                    hsla(s.text_muted),
                ))
                .child(
                    div()
                        .id(ElementId::Name(selector.clone().into()))
                        .debug_selector(move || selector)
                        .flex_none()
                        .max_w(relative(0.7))
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .text_color(hsla(s.text))
                        .role(Role::Link)
                        .aria_label(SharedString::from(format!("Open {open}")))
                        .cursor_pointer()
                        .hover(gpui::Styled::underline)
                        .child(SharedString::from(name.to_owned()))
                        .on_click(cx.listener(move |_this, _ev, _w, cx| {
                            cx.emit(ThreadViewEvent::OpenFile { path: open.clone() });
                        })),
                )
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .text_color(hsla(s.text_muted))
                        .child(SharedString::from(dir)),
                )
                .children(kit::changes(theme, patch.added, patch.removed))
        });
        let id = request.id.0.clone();
        div()
            .id(ElementId::Name(format!("proposed-{id}").into()))
            .debug_selector(move || format!("proposed-{id}"))
            .role(Role::Group)
            .aria_label(SharedString::from(match &path {
                Some(path) => format!("Change to {}", tidy(path, &cwd)),
                None => "The change".to_owned(),
            }))
            .flex_none()
            .min_h_0()
            .max_h(most)
            .overflow_y_scroll()
            .mx(px(theme.spacing.md))
            .mb(px(theme.spacing.md))
            .flex()
            .flex_col()
            .gap(px(theme.spacing.xxs))
            .children(head)
            .child(self.patch_well(
                &key(request),
                (path.as_deref().unwrap_or_default(), patch),
                false,
                cx,
            ))
            .into_any_element()
    }

    /// A plan put to the person with no card of its own on show, whole in a well at the prose
    /// size: it is the document being approved, never its last lines.
    pub(super) fn plan_well(&self, request: &Request, plan: &Clipped, most: Pixels) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let id = request.id.0.clone();
        let left_out = plan.full.as_ref().map(|_| {
            let shown = u64::try_from(plan.text.lines().count()).unwrap_or(u64::MAX);
            let more = u64::from(plan.lines).saturating_sub(shown);
            div()
                .pt(px(theme.spacing.xs))
                .text_size(px(theme.typography.small()))
                .text_color(hsla(s.text_muted))
                .child(SharedString::from(format!(
                    "{} more in the agent's terminal",
                    kit::count(more, "line", "lines")
                )))
        });
        kit::inset(div(), theme)
            .id(ElementId::Name(format!("request-plan-{id}").into()))
            .debug_selector(move || format!("request-plan-{id}"))
            .role(Role::Article)
            .aria_label("Plan")
            .flex_none()
            .min_h_0()
            .max_h(most)
            .overflow_y_scroll()
            .mx(px(theme.spacing.md))
            .mb(px(theme.spacing.md))
            .px(px(theme.spacing.md))
            .py(px(theme.spacing.sm))
            .rounded(px(theme.radii.sm))
            .text_size(px(theme.typography.prose()))
            .line_height(relative(theme.typography.prose_line_height))
            .text_color(hsla(s.text))
            .child(self.markdown(format!("request-plan-text-{}", request.id.0), &plan.text, false))
            .children(left_out)
            .when(plan.text.trim().is_empty(), |el| el.child(SharedString::from("No plan text")))
            .into_any_element()
    }
}
