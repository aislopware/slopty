//! A request's answers, composed as one decision on one foot row: Deny and Allow at its right,
//! the other ways to deny (and the way back to the agent's own prompt) in a menu named for
//! them, and any standing grant at its left, its reach written out whole.
//!
//! A row of five like buttons ("Always allow /work; accept edits mode", "Deny", "Deny…", "Deny
//! and stop", "Allow") made the person read every one to find the two that matter, and put a
//! standing grant at the same weight as this once. Now the usual answers are the row's last two,
//! the solid one last as a dialog's default is, a base unit apart. What turns the request down
//! some other way waits behind the deny's chevron, and so does answering in the terminal, which
//! sat on the title's line as a third choice. A grant that holds past this once never sits
//! beside them: it leads the row from the other end, in a quiet button whose reach follows in
//! words. A section of its own under the row, headed "From now on", made the card two decisions
//! tall.

use std::rc::Rc;

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, Context, Div, InteractiveElement as _, IntoElement as _, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, div,
};
use slopty_proto::thread::{Choice, Effect, Request};

use super::ThreadView;
use super::denying::{deny_choice, plain_deny};
use crate::colors::hsla;
use crate::icons::Symbol;
use crate::kit::{self, ButtonKind};

/// What the deny's chevron opens, and its menu's name.
pub(crate) const OTHER_DENIALS: &str = "Other ways to deny";

/// The menu's row that denies with the person's reason.
pub(crate) const WITH_A_REASON: &str = "Deny with a reason\u{2026}";

/// What the standing grants are named by, for a screen reader.
pub(crate) const STANDING: &str = "Grants that last";

/// A request's answers, sorted by where each goes.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Arranged<'a> {
    /// The row, in order: the answers to a question and any other plain allow, quiet; then the
    /// plain deny; then the plain allow, the one green control ([`ButtonKind::Go`]), last.
    pub front: Vec<(&'a Choice, ButtonKind)>,
    /// Behind the deny's chevron: every other deny (one that reaches further, one that also
    /// stops the turn).
    pub denials: Vec<&'a Choice>,
    /// Under the row: each allow that holds past this once.
    pub standing: Vec<&'a Choice>,
}

/// Where each of `options` goes. The plain allow leads in the green; a grant that reaches
/// past this once never does, nor ever sits in the row, so the card never leads the person to
/// a standing grant.
pub(super) fn arrange(options: &[Choice]) -> Arranged<'_> {
    let lead = options.iter().position(|c| c.effect == Effect::Allow && c.scope.is_none());
    let plain = plain_deny(options).map(|c| c.id.as_str());
    let mut front: Vec<(&Choice, ButtonKind)> = Vec::new();
    let (mut denials, mut standing) = (Vec::new(), Vec::new());
    for (ix, choice) in options.iter().enumerate() {
        if Some(ix) == lead || Some(choice.id.as_str()) == plain {
            continue;
        }
        match choice.effect {
            Effect::Deny => denials.push(choice),
            Effect::Allow if choice.scope.is_some() => standing.push(choice),
            Effect::Allow | Effect::Answer => front.push((choice, ButtonKind::Ghost)),
        }
    }
    let plain = options.iter().find(|c| Some(c.id.as_str()) == plain);
    front.extend(plain.map(|c| (c, ButtonKind::Secondary)));
    front.extend(lead.and_then(|ix| options.get(ix)).map(|c| (c, ButtonKind::Go)));
    Arranged { front, denials, standing }
}

/// An answer's words on its button, with how far it reaches when its words do not say it
/// already: "Always allow · Bash(cargo test:*)", but "Always allow" for an `always` scope.
pub(super) fn answer_label(words: &str, scope: Option<&str>) -> String {
    scope.map_or_else(|| words.to_owned(), |scope| format!("{words} \u{b7} {scope}"))
}

/// How far an answer reaches, when its words do not say it already ([`answer_label`]).
pub(super) fn answer_scope(choice: &Choice) -> Option<&str> {
    choice
        .scope
        .as_deref()
        .map(str::trim)
        .filter(|scope| !scope.is_empty())
        .filter(|scope| !choice.label.to_lowercase().contains(&scope.to_lowercase()))
}

impl ThreadView {
    /// `request`'s answers as one decision on one row ([module](self)), or the reason's field
    /// while the person writes why it is denied. The way back to the agent's own prompt waits
    /// behind the deny's chevron; with no plain deny to hang it from, it leads the row.
    pub(super) fn decision(&self, request: &Request, cx: &Context<Self>) -> Option<AnyElement> {
        if let Some(row) = self.deny_row(request, cx) {
            return Some(row);
        }
        let theme = &self.theme;
        let arranged = arrange(&request.options);
        let plain = plain_deny(&request.options).map(|c| c.id.clone());
        let hung =
            plain.is_some() && arranged.front.iter().any(|(c, _)| Some(&c.id) == plain.as_ref());
        let release = self.release_words(cx);
        let leading = if hung { None } else { self.release_button(request, cx) };
        if arranged.front.is_empty() && arranged.standing.is_empty() && leading.is_none() {
            return None;
        }
        let mut row: Vec<AnyElement> = Vec::new();
        for (choice, kind) in &arranged.front {
            let button = self.choice_button(request, choice, *kind, cx);
            if plain.as_deref() == Some(choice.id.as_str()) {
                let release = release.clone();
                row.push(self.deny_split(request, button, &arranged.denials, release, cx));
            } else {
                row.push(button.into_any_element());
            }
        }
        let standing = (!arranged.standing.is_empty())
            .then(|| self.standing_grants(request, &arranged.standing, cx).into_any_element());
        let id = request.id.0.clone();
        Some(
            div()
                .debug_selector(move || format!("decision-{id}"))
                .key_context(crate::conversation::REQUEST_CTX)
                .w_full()
                .flex()
                // Where the answers do not fit beside the rest they take a line of their own,
                // and wrap among themselves, ending at the trailing edge with the green way on
                // last: a long answer never pushes the primary out of the card.
                .flex_wrap()
                .items_center()
                .justify_end()
                .gap_x(self.z(theme.spacing.md))
                .gap_y(self.z(theme.spacing.sm))
                .children(leading)
                .child(div().flex_1().min_w_0().children(standing))
                .child(
                    div()
                        .flex_none()
                        .max_w_full()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .justify_end()
                        .gap(self.z(theme.spacing.sm))
                        .children(row),
                )
                .into_any_element(),
        )
    }

    /// One answer's button: its words, then how far it reaches, muted; it answers on a press.
    pub(super) fn choice_button(
        &self,
        request: &Request,
        choice: &Choice,
        kind: ButtonKind,
        cx: &Context<Self>,
    ) -> gpui::Stateful<Div> {
        let (ask, id) = (request.id.clone(), choice.id.clone());
        self.answer_button(
            format!("answer-{}-{}", ask.0, choice.id),
            (choice.label.clone(), answer_scope(choice).map(str::to_owned)),
            kind,
        )
        .on_click(cx.listener(move |this, _ev, _w, cx| this.answer(ask.clone(), id.clone(), cx)))
    }

    /// The plain deny joined to a chevron that opens the other ways to deny: with a reason,
    /// every other deny the agent offers, and `release`, the way back to the agent's own prompt.
    /// A deny with no other way stands alone.
    fn deny_split(
        &self,
        request: &Request,
        deny: gpui::Stateful<Div>,
        denials: &[&Choice],
        release: Option<String>,
        cx: &Context<Self>,
    ) -> AnyElement {
        let why = deny_choice(request).map(|c| c.id.clone());
        if why.is_none() && denials.is_empty() && release.is_none() {
            return deny.into_any_element();
        }
        let theme = &self.theme;
        let s = theme.surfaces;
        let ask = request.id.clone();
        let open = self.denials_open.as_ref() == Some(&ask);
        let chevron = {
            let el = div()
                .id(SharedString::from(format!("denials-{}", ask.0)))
                .debug_selector({
                    let ask = ask.clone();
                    move || format!("denials-{}", ask.0)
                })
                .role(Role::Button)
                .aria_label(OTHER_DENIALS)
                .aria_expanded(open)
                .flex_none()
                .self_stretch()
                .flex()
                .items_center()
                .px(self.z(theme.spacing.xs))
                .rounded_r(self.z(theme.radii.sm))
                .cursor_pointer();
            crate::a11y::tab_stop(kit::secondary(el, theme), s.focus)
                .child(self.icon(Symbol::ChevronDown, s.text_secondary))
                .on_click(cx.listener(move |this, _ev, _w, cx| {
                    this.denials_open = if this.denials_open.as_ref() == Some(&ask) {
                        None
                    } else {
                        Some(ask.clone())
                    };
                    cx.notify();
                }))
        };
        let menu = open.then(|| self.denials_menu(request, why, denials, release, cx));
        div()
            .relative()
            .flex_none()
            .max_w_full()
            .flex()
            .items_stretch()
            .gap(kit::HAIR)
            // A long deny gives way to its chevron, wrapping its words.
            .child(deny.flex_shrink_1().min_w_0().rounded_r(self.z(0.0)))
            .child(chevron)
            .children(menu)
            .into_any_element()
    }

    /// The other ways to deny, hung from the deny's chevron: with a reason first, then each
    /// other deny in the agent's order, its reach in its words, and last, apart, the way back
    /// to the agent's own prompt.
    fn denials_menu(
        &self,
        request: &Request,
        why: Option<String>,
        denials: &[&Choice],
        release: Option<String>,
        cx: &Context<Self>,
    ) -> AnyElement {
        let this = cx.entity().downgrade();
        let ask = request.id.clone();
        let mut menu = kit::Menu::new();
        if let Some(choice) = why {
            let (to, ask) = (this.clone(), ask.clone());
            menu.push(kit::MenuItem::new("why", WITH_A_REASON, move |window, cx| {
                let (ask, choice) = (ask.clone(), choice.clone());
                let _gone = to.update(cx, |this, cx| this.start_deny(ask, choice, window, cx));
            }));
        }
        for choice in denials {
            let (to, ask, id) = (this.clone(), ask.clone(), choice.id.clone());
            let label = answer_label(&choice.label, answer_scope(choice));
            menu.push(kit::MenuItem::new(choice.id.clone(), label, move |_w, cx| {
                let (ask, id) = (ask.clone(), id.clone());
                let _gone = to.update(cx, |this, cx| this.answer(ask, id, cx));
            }));
        }
        if let Some(words) = release {
            menu.separate();
            let (to, ask) = (this.clone(), ask.clone());
            menu.push(kit::MenuItem::new("release", words, move |_w, cx| {
                let ask = ask.clone();
                let _gone = to.update(cx, |this, cx| {
                    this.denials_open = None;
                    this.release(ask, cx);
                });
            }));
        }
        let panel = kit::MenuPanel::new(
            format!("denials-menu-{}", ask.0),
            OTHER_DENIALS,
            Rc::new(menu),
            &self.theme,
            move |window, cx| {
                let _gone = this.update(cx, |this, cx| {
                    this.denials_open = None;
                    this.focus(window, cx);
                    cx.notify();
                });
            },
        );
        gpui::deferred(gpui::anchored().anchor(gpui::Anchor::BottomRight).child(panel))
            .with_priority(crate::palette::Layer::Submenu.priority())
            .into_any_element()
    }

    /// The grants that hold past this once, leading the row from its other end: each a quiet
    /// button in its words, then its reach written out whole in the readable size, wrapping
    /// rather than cut, since that reach is what the person grants.
    fn standing_grants(
        &self,
        request: &Request,
        standing: &[&Choice],
        cx: &Context<Self>,
    ) -> gpui::Stateful<Div> {
        let theme = &self.theme;
        let s = theme.surfaces;
        let ask = request.id.0.clone();
        div()
            .id(SharedString::from(format!("standing-{ask}")))
            .debug_selector(move || format!("standing-{ask}"))
            .role(Role::Group)
            .aria_label(STANDING)
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(self.z(theme.spacing.xxs))
            .children(standing.iter().map(|choice| {
                let reach = choice.scope.as_deref().map(str::trim).filter(|r| !r.is_empty());
                let (ask, id) = (request.id.clone(), choice.id.clone());
                let label = answer_label(&choice.label, reach);
                let button = self
                    .button_frame(
                        format!("answer-{}-{}", request.id.0, choice.id),
                        label,
                        ButtonKind::Ghost,
                    )
                    .flex_none()
                    .child(SharedString::from(choice.label.clone()))
                    .on_click(cx.listener(move |this, _ev, _w, cx| {
                        this.answer(ask.clone(), id.clone(), cx);
                    }));
                div()
                    .w_full()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(self.z(theme.spacing.xs))
                    .child(button)
                    .children(reach.map(|reach| {
                        div()
                            .min_w_0()
                            .flex_1()
                            .whitespace_normal()
                            .map(|el| kit::typed(el, theme.roles().metadata, self.zoom))
                            .text_color(hsla(s.text_secondary))
                            .child(SharedString::from(reach.to_owned()))
                    }))
            }))
    }
}

#[cfg(test)]
mod tests {
    use slopty_proto::thread::{Choice, Effect};

    use super::{answer_label, answer_scope, arrange};
    use crate::kit::ButtonKind;

    fn choice(id: &str, effect: Effect, scope: Option<&str>, stops: bool) -> Choice {
        Choice {
            id: id.to_owned(),
            label: id.to_owned(),
            effect,
            scope: scope.map(str::to_owned),
            stops,
        }
    }

    fn ids<'a>(choices: impl IntoIterator<Item = &'a Choice>) -> Vec<&'a str> {
        choices.into_iter().map(|c| c.id.as_str()).collect()
    }

    /// Claude Code's five answers: the row is Deny then Allow, the solid last; "Deny and stop"
    /// waits behind the deny's chevron; the standing grant stands under the row.
    #[test]
    fn allow_and_deny_lead_and_the_rest_go_where_they_belong() {
        let offered = [
            choice("always", Effect::Allow, Some("/work; accept edits mode"), false),
            choice("deny", Effect::Deny, None, false),
            choice("stop", Effect::Deny, None, true),
            choice("allow", Effect::Allow, None, false),
        ];
        let a = arrange(&offered);
        assert_eq!(
            a.front.iter().map(|(c, k)| (c.id.as_str(), *k)).collect::<Vec<_>>(),
            [("deny", ButtonKind::Secondary), ("allow", ButtonKind::Go)]
        );
        assert_eq!(ids(a.denials), ["stop"]);
        assert_eq!(ids(a.standing), ["always"]);
    }

    /// An ACP agent may offer "always" first: the plain allow still leads in the green, a
    /// standing deny is one of the other denials, and the row never holds a standing grant.
    #[test]
    fn a_standing_grant_never_leads_or_sits_in_the_row() {
        let offered = [
            choice("allow-always", Effect::Allow, Some("always"), false),
            choice("allow", Effect::Allow, None, false),
            choice("deny-always", Effect::Deny, Some("always"), false),
            choice("deny", Effect::Deny, None, false),
        ];
        let a = arrange(&offered);
        assert_eq!(ids(a.front.iter().map(|(c, _)| *c)), ["deny", "allow"]);
        assert_eq!(a.front.last().map(|(_, k)| *k), Some(ButtonKind::Go));
        assert_eq!(ids(a.denials), ["deny-always"]);
        assert_eq!(ids(a.standing), ["allow-always"]);
    }

    /// A question's answers keep their order in the row, all quiet: nothing there is a default.
    #[test]
    fn a_questions_answers_keep_their_order_and_none_leads() {
        let offered = [
            choice("split", Effect::Answer, None, false),
            choice("unified", Effect::Answer, None, false),
        ];
        let a = arrange(&offered);
        assert_eq!(
            a.front.iter().map(|(c, k)| (c.id.as_str(), *k)).collect::<Vec<_>>(),
            [("split", ButtonKind::Ghost), ("unified", ButtonKind::Ghost)]
        );
        assert!(a.denials.is_empty() && a.standing.is_empty());
    }

    /// An answer that reaches beyond this once says how far, unless its words already do.
    #[test]
    fn a_scoped_answer_says_how_far_it_reaches() {
        let labelled = |label: &str, scope: Option<&str>| Choice {
            id: "a".to_owned(),
            label: label.to_owned(),
            effect: Effect::Allow,
            scope: scope.map(str::to_owned),
            stops: false,
        };
        let said = |c: Choice| answer_label(&c.label, answer_scope(&c));
        assert_eq!(said(labelled("Allow", None)), "Allow");
        assert_eq!(
            said(labelled("Always allow", Some("Bash(cargo test:*)"))),
            "Always allow \u{b7} Bash(cargo test:*)"
        );
        assert_eq!(said(labelled("Always Allow", Some("always"))), "Always Allow");
    }
}
