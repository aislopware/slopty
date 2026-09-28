//! The face's chrome around the list: the bar over a subagent's thread or the session's
//! changes, the way back to the newest row, the find bar, the line about the last permission
//! prompt, and the composer's floating shell, which holds the background work and the task
//! list over the field, and the permission prompt in the field's place while one is asked.

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, Bounds, Context, InteractiveElement as _, IntoElement as _, ObjectFit,
    ParentElement as _, PathBuilder, Pixels, SharedString, StatefulInteractiveElement as _,
    Styled as _, StyledImage as _, canvas, div, img, point, px,
};
use gpui_kit::component::input::{Input, Textarea};
use slopty_core::WallMs;
use slopty_proto::conversation::{ThreadId, ToolDetail, Verdict};
use slopty_theme::{Rgb, Theme};

use super::entries::READING;
use super::{ConversationView, Pane};
use crate::colors::hsla;
use crate::conversation::approval::{self, Outcome};
use crate::conversation::composer::Attachment;
use crate::conversation::rows;
use crate::icons::IconName;
use crate::kit::{self, ButtonKind};

/// The widest an attachment's chip grows, in points at zoom 1; a longer name is cut short.
const ATTACHMENT_WIDTH: f32 = 240.0;

/// The side of a pasted picture's chip, in points at zoom 1: a two-line row's height.
const THUMBNAIL: f32 = 40.0;

impl ConversationView {
    /// Over the list once the reader scrolled up: back to the newest row.
    pub(super) fn latest_pill(&self, cx: &Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let pill = div()
            .id("conversation-latest")
            .debug_selector(|| "conversation-latest".to_owned())
            .role(Role::Button)
            .aria_label("Jump to latest")
            .flex()
            .items_center()
            .gap(self.z(theme.spacing.xs))
            .px(self.z(theme.spacing.sm))
            .py(self.z(theme.spacing.xxs))
            .rounded(self.z(theme.radii.md))
            .text_size(self.z(theme.typography.small()))
            .text_color(hsla(s.text_secondary))
            .cursor_pointer()
            .hover(move |el| el.text_color(hsla(s.text)))
            .child(self.icon(IconName::ArrowDown, s.text_secondary))
            .child("Latest");
        let pill = crate::a11y::tab_stop(kit::elevate(pill, theme), s.accent)
            .on_click(cx.listener(|this, _ev, _w, cx| this.to_latest(cx)));
        div()
            .absolute()
            .left_0()
            .right_0()
            .bottom(self.z(theme.spacing.md))
            .flex()
            .justify_center()
            .child(kit::slide_fade(
                pill,
                "latest",
                theme.spacing.xs * self.zoom,
                kit::Pace::Fade,
                cx,
            ))
            .into_any_element()
    }

    /// How the last permission prompt ended; when the terminal took it back, the way there.
    fn settled_line(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let (tool, outcome) = self.approvals.last()?;
        let theme = &self.theme;
        let s = theme.surfaces;
        let (icon, tone) = match outcome {
            Outcome::Answered(Verdict::Deny { .. }) | Outcome::Elsewhere(Verdict::Deny { .. }) => {
                (IconName::CircleX, s.text_muted)
            }
            Outcome::Answered(_) | Outcome::Elsewhere(_) => (IconName::CircleCheck, s.success),
            Outcome::Released => (IconName::SquareTerminal, s.warn),
            Outcome::Withdrawn => (IconName::CirclePause, s.text_muted),
        };
        let text = SharedString::from(outcome.text(tool));
        let go = outcome.in_terminal().then(|| {
            kit::button(theme, "show-terminal", "Show the terminal", ButtonKind::Link).on_click(
                cx.listener(|_this, _ev, _w, cx| {
                    cx.emit(crate::conversation::FaceEvent::ShowTerminal);
                }),
            )
        });
        Some(
            div()
                .id("permission-settled")
                .debug_selector(|| "permission-settled".to_owned())
                .role(Role::Status)
                .aria_label(text.clone())
                .flex()
                .items_center()
                .gap(self.z(theme.spacing.sm))
                .px(self.z(theme.spacing.sm))
                .text_size(self.z(theme.typography.small()))
                .text_color(hsla(s.text_secondary))
                .child(self.icon(icon, tone))
                .child(div().flex_1().min_w_0().child(text))
                .children(go)
                .into_any_element(),
        )
    }

    /// While the worker is out of reach: that it is, and that the draft waits for it.
    fn away_line(&self) -> Option<AnyElement> {
        let name = self.away.as_deref()?;
        let theme = &self.theme;
        let s = theme.surfaces;
        let text = SharedString::from(format!("{name} is unreachable \u{b7} your draft is kept"));
        Some(
            div()
                .id("composer-away")
                .debug_selector(|| "composer-away".to_owned())
                .role(Role::Status)
                .aria_label(text.clone())
                .flex()
                .items_center()
                .gap(self.z(theme.spacing.sm))
                .px(self.z(theme.spacing.sm))
                .text_size(self.z(theme.typography.small()))
                .text_color(hsla(s.text_secondary))
                .child(self.icon(IconName::WifiOff, s.warn))
                .child(div().flex_1().min_w_0().child(text))
                .into_any_element(),
        )
    }

    /// Everything under the list, on the reading column: how the last prompt ended, and the
    /// composer's floating shell, which holds the background work and the task list over the
    /// field (or over a permission prompt while one is asked).
    pub(super) fn foot(&self, cx: &Context<Self>) -> Option<AnyElement> {
        if *self.pane() != Pane::Conversation {
            return None;
        }
        let (spacing, zoom) = (self.theme.spacing, self.zoom);
        let z = |v: f32| px(v * zoom);
        let tasks = self.tasks_card(cx);
        let tray = self.tray(cx);
        let settled = self.settled_line(cx);
        let away = self.away_line();
        let asking = self.approvals.prompt().is_some();
        let inside = match (asking, self.answering.is_some()) {
            (true, true) => self.question_card(cx),
            (true, false) => self.approval(cx),
            (false, _) => self.composer_box(cx),
        };
        let menu = (!asking).then(|| self.menu_section(cx)).flatten();
        // The shell stays; what it holds cross-fades between the composer and a prompt, on the
        // sheet's curve asked in, on the settle's answered. Reduce Motion swaps at once.
        let inside = match self.morph {
            Some((generation, asked)) if asked == asking && kit::motion(cx) => {
                let pace = if asked { kit::Pace::Sheet } else { kit::Pace::Settle };
                gpui::AnimationExt::with_animation(
                    div().child(inside),
                    ("composer-morph", generation),
                    pace.animation(),
                    // The new contents come in over the last two thirds, as T3's morph does.
                    |el, t| el.opacity(((t - 0.35) / 0.65).clamp(0.0, 1.0)),
                )
                .into_any_element()
            }
            _ => inside,
        };
        // The work in the background and the task list are the shell's top sections, over the
        // field, a hairline apart, as T3's composer holds pending work: one surface, not three.
        let s = self.theme.surfaces;
        // The menu is the section nearest the field it writes into.
        let sections = tray.into_iter().chain(tasks).chain(menu).map(|section| {
            div()
                .border_b_1()
                .border_color(hsla(s.border_subtle))
                .px(z(spacing.xs))
                .py(z(spacing.xxs))
                .child(section)
        });
        let shell = kit::elevate(div(), &self.theme)
            .id("composer-shell")
            .debug_selector(|| "composer-shell".to_owned())
            .w_full()
            .flex()
            .flex_col()
            .rounded(z(self.theme.radii.lg))
            .children(sections)
            .child(div().p(z(spacing.md)).child(inside));
        Some(
            div()
                .flex_none()
                .w_full()
                .flex()
                .justify_center()
                .px(z(spacing.inset()))
                .pb(z(spacing.md))
                .child(
                    div()
                        .w_full()
                        .max_w(z(READING))
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(z(spacing.sm))
                        .children(settled)
                        .children(away)
                        .child(shell),
                )
                .into_any_element(),
        )
    }

    /// The list's lower edge fading into the body's surface over the composer, so rows slide under
    /// it rather than stop at a line, and its top edge once the list is scrolled off its start,
    /// so a row is not sliced by the header: the one gradient the face draws, as a mask.
    pub(super) fn list_fade(&self) -> Vec<AnyElement> {
        let surface = self.theme.content();
        let bottom = kit::edge_fade(kit::Edge::Bottom, surface, self.z(self.theme.spacing.xl));
        let top = self.list.logical_scroll_top();
        let scrolled = top.item_ix > 0 || top.offset_in_item > px(0.0);
        let top = scrolled
            .then(|| kit::edge_fade(kit::Edge::Top, surface, self.z(self.theme.spacing.md)));
        std::iter::once(bottom).chain(top).map(gpui::IntoElement::into_any_element).collect()
    }

    /// The held permission prompt, in the composer's shell: a statement of what Claude wants,
    /// what exactly it would run or change, why, and the three answers at the right.
    fn approval(&self, cx: &Context<Self>) -> AnyElement {
        let Some(prompt) = self.approvals.prompt().cloned() else {
            return div().into_any_element();
        };
        let theme = self.theme.clone();
        let s = theme.surfaces;
        let statement = approval::statement(&prompt);
        let busy = self.approvals.answering().is_some();
        let what = self.asked_what(&prompt, cx);
        let reason = match &prompt.detail {
            ToolDetail::Bash(bash) => bash.description.clone(),
            _ => None,
        };
        let scope = approval::always_line(&prompt.suggestions);
        let mode = prompt
            .mode
            .as_deref()
            .filter(|m| *m != "default")
            .map(|m| format!("{} mode", approval::mode_label(m)));
        let facts: Vec<String> = mode.into_iter().collect();
        let wait = self
            .held
            .filter(|(ask, _)| *ask == prompt.ask)
            .and_then(|(_, since)| wait_left(&prompt, since, WallMs::now()));
        // A plan is approved or kept: never allowed for good, and kept with words for Claude.
        let plan = self.plan_asked();
        let (allow_label, deny_label) =
            if plan { ("Approve plan", "Keep planning") } else { ("Allow once", "Deny") };
        let allow = kit::button(&theme, "allow-once", allow_label, ButtonKind::Primary)
            .when(busy, |el| el.opacity(slopty_theme::alpha::PRESSED))
            .on_click(cx.listener(|this, _ev, _w, cx| this.answer(Verdict::Allow, cx)));
        let always = (!prompt.suggestions.is_empty() && !plan).then(|| {
            kit::button(&theme, "allow-always", "Always allow", ButtonKind::Secondary)
                .when(busy, |el| el.opacity(slopty_theme::alpha::PRESSED))
                .on_click(cx.listener(|this, _ev, _w, cx| this.answer(Verdict::AllowAlways, cx)))
        });
        let deny = if self.deny_open {
            kit::button(&theme, "deny-send", deny_label, ButtonKind::Secondary)
                .on_click(cx.listener(|this, _ev, _w, cx| this.deny(cx)))
        } else {
            kit::button(&theme, "deny", deny_label, ButtonKind::Ghost).on_click(cx.listener(
                |this, _ev, window, cx| {
                    this.deny_open = true;
                    this.deny.update(cx, |d, cx| d.focus(window, cx));
                    cx.notify();
                },
            ))
        };
        // What "Always allow" grants, said before it is pressed rather than folded away: one
        // paragraph that wraps as prose, the rules and paths in the mono face.
        let scope = (always.is_some() && !scope.is_empty()).then(|| {
            let (words_font, code_font) =
                (gpui::font(theme.typography.ui_family.clone()), gpui::font(self.mono()));
            let run = |said: &approval::Said| {
                let (font, tone) = match said {
                    approval::Said::Words(_) => (words_font.clone(), s.text_muted),
                    approval::Said::Code(_) => (code_font.clone(), s.text_secondary),
                };
                gpui::TextRun {
                    len: said.text().len(),
                    font,
                    color: hsla(tone),
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                }
            };
            let runs: Vec<gpui::TextRun> = scope.iter().map(run).collect();
            let plain =
                SharedString::from(scope.iter().map(approval::Said::text).collect::<String>());
            div()
                .id("always-scope")
                .debug_selector(|| "always-scope".to_owned())
                .role(Role::Note)
                .aria_label(plain.clone())
                .text_size(self.z(theme.typography.meta()))
                .child(gpui::StyledText::new(plain).with_runs(runs))
        });
        let reason_field = self
            .deny_open
            .then(|| div().w_full().child(Input::new(&self.deny).aria_label("Why, for Claude")));
        // The prompt's words start on the composer's text edge, so the swap does not jump.
        div()
            .id("approval")
            .debug_selector(|| "approval".to_owned())
            .role(Role::AlertDialog)
            .aria_label(SharedString::from(statement.clone()))
            .px(self.z(kit::FIELD_INSET))
            .flex()
            .flex_col()
            .gap(self.z(theme.spacing.sm))
            .text_size(self.z(theme.typography.ui_size))
            .child(
                // On a narrow tile the facts drop under the statement rather than squeeze it.
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(self.z(theme.spacing.sm))
                    .child(
                        div()
                            .flex_none()
                            .size(self.z(theme.spacing.sm))
                            .rounded_full()
                            .bg(hsla(s.warn_fill)),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_color(hsla(s.text))
                            .font_weight(gpui::FontWeight(slopty_theme::Typography::MEDIUM_WEIGHT))
                            .child(SharedString::from(statement)),
                    )
                    .child(div().flex_1())
                    .when(!facts.is_empty(), |el| {
                        el.child(
                            kit::tabular(div())
                                .flex_none()
                                .text_size(self.z(theme.typography.meta()))
                                .text_color(hsla(s.text_muted))
                                .child(SharedString::from(facts.join(" \u{b7} "))),
                        )
                    }),
            )
            .children(what)
            .children(reason.map(|r| {
                div()
                    .text_size(self.z(theme.typography.small()))
                    .text_color(hsla(s.text_secondary))
                    .child(SharedString::from(r))
            }))
            .children(reason_field)
            .children(scope)
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .justify_end()
                    .items_center()
                    .gap(self.z(theme.spacing.sm))
                    .children(wait.map(|wait| {
                        kit::tabular(div())
                            .debug_selector(|| "approval-wait".to_owned())
                            .flex_1()
                            .min_w_0()
                            .text_size(self.z(theme.typography.meta()))
                            .text_color(hsla(s.text_muted))
                            .child(SharedString::from(wait))
                    }))
                    .child(deny)
                    .children(always)
                    .child(allow),
            )
            .into_any_element()
    }

    /// Exactly what a held prompt would do: a command in the mono face on the raised plate,
    /// its first lines until asked for all; an edit's diff; another tool's input as keys and
    /// values.
    fn asked_what(
        &self,
        prompt: &slopty_proto::conversation::PermissionPrompt,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        let theme = &self.theme;
        let s = theme.surfaces;
        let call = slopty_proto::conversation::ToolCall {
            name: prompt.tool.clone(),
            detail: prompt.detail.clone(),
            result: None,
        };
        if let ToolDetail::Bash(bash) = &prompt.detail {
            let lines: Vec<&str> = bash.command.text.lines().collect();
            let hidden = lines.len().saturating_sub(ASK_LINES);
            let shown = if self.ask_all || hidden == 0 {
                bash.command.text.clone()
            } else {
                lines.iter().take(ASK_LINES).copied().collect::<Vec<_>>().join("\n")
            };
            let more = (hidden > 0 && !self.ask_all).then(|| {
                let label = SharedString::from(format!(
                    "Show all {}",
                    crate::conversation::tools::count(lines.len() as u64, "line", "lines")
                ));
                crate::a11y::tab_stop(
                    div()
                        .id("ask-all")
                        .debug_selector(|| "ask-all".to_owned())
                        .role(Role::Button)
                        .aria_label(label.clone())
                        .font_family(theme.typography.ui_family.clone())
                        .text_size(self.z(theme.typography.meta()))
                        .text_color(hsla(s.accent))
                        .cursor_pointer()
                        .hover(gpui::Styled::underline)
                        .child(label),
                    s.accent,
                )
                .on_click(cx.listener(|this, _ev, _w, cx| {
                    this.ask_all = true;
                    cx.notify();
                }))
            });
            return Some(
                div()
                    .debug_selector(|| "ask-command".to_owned())
                    .flex()
                    .flex_col()
                    .gap(self.z(theme.spacing.xxs))
                    .px(self.z(theme.spacing.sm))
                    .py(self.z(theme.spacing.xs))
                    .rounded(self.z(theme.radii.sm))
                    .bg(hsla(s.raised))
                    .child(self.code_text(&shown, s.text))
                    .children(more)
                    .into_any_element(),
            );
        }
        self.tool_body(
            &slopty_proto::conversation::Entry {
                id: format!("ask-{}", prompt.ask),
                at_ms: prompt.asked_ms,
                body: slopty_proto::conversation::Body::Tool(Box::new(call.clone())),
            },
            &call,
            rows::Level::Full,
            cx,
        )
        .map(|body| {
            div().max_h(self.z(ASK_HEIGHT)).overflow_hidden().child(body).into_any_element()
        })
    }

    /// The model the session runs, one name wherever the face says it: the one that answered
    /// the last turn, else (before any answer) the status line's.
    pub(super) fn running_model(&self) -> Option<String> {
        self.model
            .thread(&ThreadId::Main)
            .and_then(|t| t.last_turn())
            .and_then(|t| t.models.last())
            .map(|id| crate::conversation::figures::model_name(id))
            .or_else(|| self.model.meters().and_then(|m| m.model.clone()))
    }

    /// The chips of what is attached to the draft, in a wrapping row over the field: a pasted
    /// picture as the picture, a file by its name, each with a way to take it off the draft and,
    /// while it uploads, how far it got. `None` while nothing is attached. The chip is the one
    /// place the upload is said: the tile's header leaves an attachment's out.
    fn attachment_chips(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let chips = self.attachments();
        if chips.is_empty() {
            return None;
        }
        let row = div()
            .id("composer-attachments")
            .debug_selector(|| "composer-attachments".to_owned())
            .flex()
            .flex_wrap()
            .items_center()
            .gap(self.z(self.theme.spacing.xs))
            .pl(self.z(kit::FIELD_INSET))
            .children(chips.iter().map(|chip| match self.attachment_picture(chip.id) {
                Some(picture) => self.picture_chip(chip, picture, cx),
                None => self.file_chip(chip, cx),
            }));
        Some(row.into_any_element())
    }

    /// What a chip says to a screen reader: its name, and how far it got while it uploads.
    fn chip_label(chip: &Attachment) -> SharedString {
        if chip.landed() {
            SharedString::from(format!("Attached {}", chip.name))
        } else {
            SharedString::from(format!("Attaching {}, {}", chip.name, chip.progress()))
        }
    }

    /// The way to take attachment `id` off the draft, its ✕ `side` points square.
    fn chip_remove(&self, chip: &Attachment, side: f32, cx: &Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let id = chip.id;
        let remove = div()
            .id(SharedString::from(format!("attachment-remove-{id}")))
            .debug_selector(|| "composer-attachment-remove".to_owned())
            .role(Role::Button)
            .aria_label(SharedString::from(format!("Remove {}", chip.name)))
            .flex_none()
            .size(self.z(side))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .on_mouse_down(gpui::MouseButton::Left, |_ev, _w, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _ev, _w, cx| this.detach(id, cx)))
            .child(
                crate::icons::icon(
                    theme,
                    IconName::X,
                    crate::icons::IconSize::Inline,
                    hsla(s.text_muted),
                )
                .size(self.z(theme.typography.meta())),
            );
        crate::a11y::tab_stop(remove, s.accent).into_any_element()
    }

    /// A pasted picture's chip: the picture itself in a small square on the hairline, its ✕ on
    /// a lifted disc in the top corner, and while it uploads a progress line along its foot.
    fn picture_chip(
        &self,
        chip: &Attachment,
        picture: std::sync::Arc<gpui::Image>,
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let disc = theme.typography.icon_large();
        let remove = kit::elevate(div(), theme)
            .absolute()
            .top(self.z(theme.spacing.xxs))
            .right(self.z(theme.spacing.xxs))
            .rounded_full()
            .overflow_hidden()
            .hover(move |el| el.bg(hsla(s.raised)))
            .child(self.chip_remove(chip, disc, cx));
        let progress = (!chip.landed()).then(|| {
            div()
                .debug_selector(|| "composer-attachment-progress".to_owned())
                .absolute()
                .left_0()
                .bottom_0()
                .h(self.z(theme.spacing.xxs))
                .w(self.z(THUMBNAIL * chip.fraction.clamp(0.0, 1.0)))
                .bg(hsla(s.accent_fill))
        });
        div()
            .id(SharedString::from(format!("attachment-{}", chip.id)))
            .debug_selector(|| "composer-attachment".to_owned())
            .role(Role::Image)
            .aria_label(Self::chip_label(chip))
            .relative()
            .flex_none()
            .size(self.z(THUMBNAIL))
            .rounded(self.z(theme.radii.sm))
            .overflow_hidden()
            .border_1()
            .border_color(hsla(s.border_subtle))
            .bg(hsla(s.raised))
            .child(img(picture).size_full().object_fit(ObjectFit::Cover))
            .children(progress)
            .child(remove)
            .into_any_element()
    }

    /// A file's chip: its name on the pill, how far it got while it uploads, and its ✕.
    fn file_chip(&self, chip: &Attachment, cx: &Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let side = (-2.0_f32).mul_add(theme.spacing.xxs, kit::PILL_HEIGHT);
        let progress = (!chip.landed()).then(|| {
            kit::tabular(div())
                .flex_none()
                .text_color(hsla(s.text_muted))
                .child(SharedString::from(chip.progress()))
        });
        kit::pill_frame(theme, self.zoom)
            .id(SharedString::from(format!("attachment-{}", chip.id)))
            .debug_selector(|| "composer-attachment".to_owned())
            .role(Role::Status)
            .aria_label(Self::chip_label(chip))
            .max_w(self.z(ATTACHMENT_WIDTH))
            // The way off sits in the pill's own end, a pad's width from its edge.
            .pr(self.z(theme.spacing.xxs))
            .bg(hsla(s.raised))
            .text_color(hsla(s.text_secondary))
            .child(self.icon(IconName::File, s.text_muted))
            .child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(SharedString::from(chip.name.clone())),
            )
            .children(progress)
            .child(
                div()
                    .rounded(self.z(theme.radii.xs))
                    .hover(move |el| el.bg(hsla(s.overlay)))
                    .child(self.chip_remove(chip, side, cx)),
            )
            .into_any_element()
    }

    /// The field that types into the agent's terminal, and under it the permission mode and
    /// the model it runs (read-only: the terminal is where either changes), and the way to
    /// send (or, while the agent works and nothing is typed, to stop it).
    fn composer_box(&self, cx: &Context<Self>) -> AnyElement {
        let theme = self.theme.clone();
        let s = theme.surfaces;
        let empty = self.draft_empty(cx);
        let away = self.away.is_some();
        let stop = self.turn_running() && empty && self.attachments().is_empty() && !away;
        let lit = stop || (!empty && !self.attachments.uploading() && !away);
        let (icon, label) =
            if stop { (IconName::Square, "Stop") } else { (IconName::ArrowUp, "Send") };
        let send = div()
            .id("composer-send")
            .debug_selector(move || {
                if stop { "composer-stop".to_owned() } else { "composer-send".to_owned() }
            })
            .role(Role::Button)
            .aria_label(label)
            .flex_none()
            .size(self.z(kit::icon_button_side(&theme)))
            .flex()
            .items_center()
            .justify_center()
            .rounded_full()
            .cursor_pointer()
            .map(|el| if lit { el.bg(hsla(s.accent_fill)) } else { el.bg(hsla(s.raised)) })
            .child(
                crate::icons::icon(
                    &theme,
                    icon,
                    crate::icons::IconSize::Inline,
                    hsla(if lit { s.accent_ink } else { s.text_muted }),
                )
                .size(self.z(theme.typography.icon())),
            );
        let send = crate::a11y::tab_stop(send, s.accent).on_click(cx.listener(
            move |this, _ev, window, cx| {
                if stop {
                    this.interrupt(cx);
                } else {
                    this.submit(window, cx);
                }
            },
        ));
        // What the agent runs under, read-only: the terminal is where either changes.
        let mode = self
            .model
            .thread(&ThreadId::Main)
            .and_then(|t| t.last_turn())
            .and_then(|t| t.mode.clone())
            .unwrap_or_else(|| "default".to_owned());
        let model = self.running_model();
        let setting = div()
            .id("composer-setting")
            .debug_selector(|| "composer-setting".to_owned())
            .role(Role::Status)
            .aria_label(SharedString::from(match &model {
                Some(model) => {
                    format!("Permissions: {}, model: {model}", approval::mode_label(&mode))
                }
                None => format!("Permissions: {}", approval::mode_label(&mode)),
            }))
            .flex()
            .items_center()
            .gap(self.z(theme.spacing.xs))
            .min_w_0()
            .text_size(self.z(theme.typography.small()))
            .child(
                div()
                    .flex_none()
                    .text_color(hsla(s.text_secondary))
                    .child(SharedString::from(approval::mode_label(&mode).to_owned())),
            )
            .children(model.map(|model| {
                div()
                    .flex()
                    .items_center()
                    .gap(self.z(theme.spacing.xs))
                    .min_w_0()
                    .text_color(hsla(s.text_muted))
                    .child(kit::separator(&self.theme))
                    .child(self.model_button(model, cx))
            }));
        let attach = kit::icon_button_at(
            &theme,
            "composer-attach",
            IconName::Paperclip,
            "Attach files",
            self.zoom,
        )
        .on_click(cx.listener(|_this, _ev, _w, cx| Self::pick_attachments(cx)));
        let face = cx.weak_entity();
        div()
            .debug_selector(|| "composer".to_owned())
            .flex()
            .flex_col()
            .gap(self.z(theme.spacing.sm))
            .children(self.attachment_chips(cx))
            .child(
                div()
                    .text_size(self.z(theme.typography.prose()))
                    .when(away, |el| el.opacity(slopty_theme::alpha::PRESSED))
                    .child(
                        Textarea::new(&self.composer)
                            .appearance(false)
                            .bordered(false)
                            .aria_label("Message")
                            .on_paste(move |item, _window, cx| {
                                face.update(cx, |v, cx| v.paste_attachment(item, cx))
                                    .unwrap_or(false)
                            }),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(self.z(theme.spacing.xs))
                    .child(attach)
                    .child(setting)
                    .child(div().flex_1())
                    .child(send),
            )
            .into_any_element()
    }

    /// The find bar, floating at the list's top right: the field, which match is on show of
    /// how many, and the ways to the one before, the next, and out.
    pub(super) fn find_bar(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let (at, count) = self.found()?;
        let field = self.find_field()?;
        let theme = &self.theme;
        let s = theme.surfaces;
        let tally = if count == 0 {
            if field.read(cx).value().trim().is_empty() {
                String::new()
            } else {
                "No matches".to_owned()
            }
        } else {
            format!("{} of {count}", at.saturating_add(1))
        };
        let step = |id: &'static str, icon: IconName, label: &'static str, delta: i8| {
            kit::icon_button_at(theme, id, icon, label, self.zoom)
                .on_click(cx.listener(move |this, _ev, _w, cx| this.step_find(delta, cx)))
        };
        let close = kit::icon_button_at(theme, "find-close", IconName::X, "Close find", self.zoom)
            .on_click(cx.listener(|this, _ev, window, cx| this.close_find(window, cx)));
        let bar = kit::elevate(div(), theme)
            .id("conversation-find")
            .debug_selector(|| "conversation-find".to_owned())
            .role(Role::Search)
            .aria_label("Find in the conversation")
            .key_context("FileSearch")
            .w(self.z(FIND_WIDTH))
            .flex()
            .items_center()
            .gap(self.z(theme.spacing.xs))
            .pl(self.z(theme.spacing.sm))
            .pr(self.z(theme.spacing.xxs))
            .py(self.z(theme.spacing.xxs))
            .rounded(self.z(theme.radii.lg))
            .text_size(self.z(theme.typography.small()))
            .child(self.icon(IconName::Search, s.text_muted))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(Input::new(field).appearance(false).aria_label("Find")),
            )
            .child(
                kit::tabular(div())
                    .debug_selector(|| "find-count".to_owned())
                    .flex_none()
                    .text_size(self.z(theme.typography.meta()))
                    .text_color(hsla(s.text_muted))
                    .child(SharedString::from(tally)),
            )
            .child(step("find-prev", IconName::ChevronUp, "Previous match", -1))
            .child(step("find-next", IconName::ChevronDown, "Next match", 1))
            .child(close);
        Some(
            div()
                .absolute()
                .top(self.z(theme.spacing.sm))
                .right(self.z(theme.spacing.lg))
                .child(kit::slide_fade(
                    bar,
                    "conversation-find",
                    -theme.spacing.xs * self.zoom,
                    kit::Pace::Fade,
                    cx,
                ))
                .into_any_element(),
        )
    }
}

/// Lines of a held command shown before "Show all".
const ASK_LINES: usize = 5;

/// The tallest a held call's preview grows, in points: a diff past it is cut.
const ASK_HEIGHT: f32 = 320.0;

/// The find bar's width, in points.
const FIND_WIDTH: f32 = 320.0;

impl ConversationView {
    /// Over the list when it is not the session's own thread: the way back, and what shows
    /// (a subagent's thread, or the session's changes with their size).
    pub(super) fn thread_bar(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let theme = &self.theme;
        let s = theme.surfaces;
        let (name, kind, back_label): (String, Option<String>, &'static str) = match self.pane() {
            Pane::Changes => {
                let files = self.changed_files();
                let (added, removed) = files.iter().fold((0_u32, 0_u32), |(a, r), f| {
                    (a.saturating_add(f.added), r.saturating_add(f.removed))
                });
                (
                    "Changes".to_owned(),
                    Some(
                        std::iter::once(crate::conversation::tools::count(
                            files.len() as u64,
                            "file",
                            "files",
                        ))
                        .chain(kit::changes_text(added, removed))
                        .collect::<Vec<_>>()
                        .join(" \u{b7} "),
                    ),
                    "Back to the conversation",
                )
            }
            Pane::Conversation => {
                let ThreadId::Agent(id) = &self.thread else { return None };
                let (name, kind) = self.model.subagent(id);
                (name.unwrap_or_else(|| format!("Subagent {id}")), kind, "Back to the conversation")
            }
        };
        let changes = *self.pane() == Pane::Changes;
        let back =
            kit::icon_button_at(theme, "thread-back", IconName::ArrowLeft, back_label, self.zoom)
                .on_click(cx.listener(move |this, _ev, _w, cx| {
                    if changes {
                        this.close_changes(cx);
                    } else {
                        this.open_thread(ThreadId::Main, cx);
                    }
                }));
        Some(
            div()
                .id("thread-bar")
                .debug_selector(|| "thread-bar".to_owned())
                .role(Role::Navigation)
                .aria_label(SharedString::from(match self.pane() {
                    Pane::Changes => name.clone(),
                    Pane::Conversation => format!("Subagent {name}"),
                }))
                .flex_none()
                .w_full()
                .h(self.z(theme.density.row))
                .flex()
                .items_center()
                .gap(self.z(theme.spacing.xs))
                .px(self.z(theme.spacing.xs))
                .bg(hsla(theme.content()))
                .border_b_1()
                .border_color(hsla(s.border_subtle))
                .text_size(self.z(theme.typography.small()))
                .child(back)
                .child(div().text_color(hsla(s.text_muted)).child("Conversation"))
                .child(self.icon(IconName::ChevronRight, s.text_muted))
                .child(
                    div()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .font_weight(gpui::FontWeight(slopty_theme::Typography::MEDIUM_WEIGHT))
                        .child(SharedString::from(name)),
                )
                .children(kind.map(|k| {
                    kit::tabular(div())
                        .flex_none()
                        .text_size(self.z(theme.typography.meta()))
                        .text_color(hsla(s.text_muted))
                        .child(SharedString::from(k))
                }))
                .when(changes, |el| el.child(div().flex_1()).child(self.scope_switch(cx)))
                .into_any_element(),
        )
    }
}

impl ConversationView {
    /// What the tile's header says about the session, at the chrome's scale `k`: the lines
    /// the conversation changed (a click shows them), how full the context window is, and the
    /// model.
    #[must_use]
    pub fn header_chips(this: &gpui::Entity<Self>, k: f32, cx: &gpui::App) -> Vec<AnyElement> {
        let view = this.read(cx);
        let theme = &view.theme;
        let s = theme.surfaces;
        let size = px(theme.typography.small() * k);
        let chip = |id: &'static str| {
            div()
                .id(id)
                .debug_selector(move || id.to_owned())
                .flex_none()
                .flex()
                .items_center()
                .gap(px(theme.spacing.xxs * k))
                .text_size(size)
                .whitespace_nowrap()
        };
        let mut out = Vec::new();
        let (added, removed) = view.model.changed_lines();
        if added > 0 || removed > 0 {
            let entity = this.clone();
            let open = *view.pane() == Pane::Changes;
            out.push(
                crate::a11y::tab_stop(
                    kit::tabular(chip("chip-changes"))
                        .role(Role::Button)
                        .aria_label(SharedString::from(format!(
                            "Changes: {added} lines added, {removed} removed"
                        )))
                        .px(px(theme.spacing.xs * k))
                        .rounded(px(theme.radii.sm * k))
                        .cursor_pointer()
                        .when(open, |el| el.bg(hsla(s.raised)))
                        .hover(move |el| el.bg(hsla(s.raised)))
                        .children(kit::changes_at(theme, added, removed, k)),
                    s.accent,
                )
                .on_click(move |_ev, _window, cx| {
                    entity.update(cx, |view, cx| view.open_changes(None, cx));
                })
                .into_any_element(),
            );
        }
        if let Some(used) = view.model.meters().and_then(|m| m.context_used_pct) {
            let words = SharedString::from(format!("Context {used:.0}% used"));
            let (entity, at) = (this.clone(), std::rc::Rc::clone(&view.context_chip));
            let open = view.context_open;
            // Where the ring is drawn, for the popover to hang under it.
            let place = canvas(move |bounds, _window, _cx| at.set(bounds), |_, (), _, _| {})
                .absolute()
                .size_full();
            out.push(
                crate::a11y::tab_stop(
                    kit::tabular(chip("chip-context"))
                        .relative()
                        .role(Role::Button)
                        .aria_label(words)
                        .aria_expanded(open)
                        .px(px(theme.spacing.xs * k))
                        .rounded(px(theme.radii.sm * k))
                        .cursor_pointer()
                        .when(open, |el| el.bg(hsla(s.raised)))
                        .hover(move |el| el.bg(hsla(s.raised)))
                        .text_color(hsla(s.text_muted))
                        .child(place)
                        .child(context_ring(theme, used, theme.typography.small() * k))
                        .child(SharedString::from(format!("{used:.0}%"))),
                    s.accent,
                )
                .on_click(move |_ev, _window, cx| entity.update(cx, Self::toggle_context))
                .into_any_element(),
            );
        }
        if out.is_empty() {
            return out;
        }
        // The readouts stand apart from each other as the tile's other items do.
        vec![
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap(px(theme.spacing.sm * k))
                .children(out)
                .into_any_element(),
        ]
    }
}

/// The tone the share of the context window in use is drawn in: warn past 80 %, error past
/// 95 %.
#[must_use]
pub fn context_tone(theme: &Theme, used_pct: f64) -> Rgb {
    let s = theme.surfaces;
    match used_pct {
        p if p >= 95.0 => s.error,
        p if p >= 80.0 => s.warn,
        _ => s.text_secondary,
    }
}

/// The share of the context window in use as a ring, `side` points round: the track in the
/// quiet hairline, the used arc in the tone the share calls for (warn past 80 %, error past
/// 95 %).
#[must_use]
pub fn context_ring(theme: &Theme, used_pct: f64, side: f32) -> AnyElement {
    let s = theme.surfaces;
    let (track, arc) = (hsla(s.border), hsla(context_tone(theme, used_pct)));
    #[expect(clippy::cast_possible_truncation, reason = "a share on screen")]
    let share = (used_pct / 100.0).clamp(0.0, 1.0) as f32;
    canvas(
        |_bounds, _window, _cx| {},
        move |bounds: Bounds<Pixels>, (), window, _cx| {
            let width = (bounds.size.width.min(bounds.size.height) * 0.16).max(px(1.5));
            let r = (bounds.size.width.min(bounds.size.height) - width) / 2.0;
            let c = bounds.center();
            let at = |t: f32| {
                let a = t.mul_add(std::f32::consts::TAU, -std::f32::consts::FRAC_PI_2);
                point(c.x + r * a.cos(), c.y + r * a.sin())
            };
            let stroke = |from: f32, to: f32| {
                let mut path = PathBuilder::stroke(width);
                let steps = 48_u16;
                path.move_to(at(from));
                for step in 1..=steps {
                    let t = f32::from(step) / f32::from(steps);
                    path.line_to(at((to - from).mul_add(t, from)));
                }
                path.build().ok()
            };
            if let Some(path) = stroke(0.0, 1.0) {
                window.paint_path(path, track);
            }
            if share > 0.0
                && let Some(path) = stroke(0.0, share)
            {
                window.paint_path(path, arc);
            }
        },
    )
    .size(px(side))
    .flex_none()
    .into_any_element()
}

/// When the worker hands the prompt back to the terminal, once that is five minutes off or
/// less: "Falls back to the terminal in 4 min". The minutes count on the worker's own
/// window, counted from `since_ms`, when this client saw it.
#[must_use]
pub fn wait_left(
    prompt: &slopty_proto::conversation::PermissionPrompt,
    since_ms: WallMs,
    now_ms: WallMs,
) -> Option<String> {
    if prompt.until_ms < prompt.asked_ms {
        return None;
    }
    let window = prompt.until_ms.millis_since(prompt.asked_ms);
    let held = now_ms.millis_since(since_ms);
    let left = window.saturating_sub(held);
    let minutes = left.div_ceil(60_000);
    match minutes {
        1 => Some("Falls back to the terminal in under a minute".to_owned()),
        2..=5 => Some(format!("Falls back to the terminal in {minutes} min")),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use slopty_core::{SessionId, WallMs};

    use super::*;
    use crate::conversation::fixtures;

    /// The fallback shows from five minutes left, counts down a minute at a time on this
    /// client's clock from when the prompt came, and goes once the time is up.
    #[test]
    fn the_fallback_counts_down_from_five_minutes() {
        let mut prompt = fixtures::bash_prompt(SessionId::new(), 1);
        prompt.asked_ms = WallMs::from_millis(1_000_000);
        prompt.until_ms = prompt.asked_ms.saturating_add(std::time::Duration::from_mins(10));
        let seen = WallMs::from_millis(50);
        let at = |minutes: u64| seen.saturating_add(std::time::Duration::from_secs(minutes * 60));
        assert_eq!(wait_left(&prompt, seen, at(0)), None, "ten minutes off");
        assert_eq!(
            wait_left(&prompt, seen, at(5)).as_deref(),
            Some("Falls back to the terminal in 5 min")
        );
        assert_eq!(
            wait_left(&prompt, seen, at(8).saturating_add(std::time::Duration::from_millis(1)))
                .as_deref(),
            Some("Falls back to the terminal in 2 min")
        );
        assert_eq!(
            wait_left(&prompt, seen, at(9).saturating_add(std::time::Duration::from_secs(30)))
                .as_deref(),
            Some("Falls back to the terminal in under a minute")
        );
        assert_eq!(wait_left(&prompt, seen, at(10)), None, "the terminal has it");
    }
}
