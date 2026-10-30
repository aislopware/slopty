// Derived from zeron (MIT, github.com/zeronsh/zeron @ 1bb6259), see THIRD_PARTY_NOTICES.md
//! Per-range hit targets share the text's shaped geometry, including wrapped links.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::clone_on_ref_ptr,
    clippy::indexing_slicing,
    clippy::renamed_function_params,
    clippy::type_complexity,
    clippy::unwrap_used,
    clippy::wildcard_imports,
    reason = "shell lint migration: slice C"
)]
use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{
    AnyElement, App, AvailableSpace, Bounds, ClickEvent, DispatchPhase, Element, ElementId,
    FocusHandle, GlobalElementId, InspectorElementId, LayoutId, MouseButton, Pixels, Point, Role,
    ScrollEvent, SharedString, TextLayout, Window, div, px,
};

use super::links::*;
use super::render::{LinkUi, activate_link, range_rects};
use crate::theme::Theme;
use crate::{icons, popover};

pub struct LinkRanges {
    pub id: SharedString,
    pub child: AnyElement,
    pub layout: TextLayout,
    pub links: Vec<(Range<usize>, LinkTarget)>,
    pub ui: Option<LinkUi>,
}
struct Interaction {
    targets: Vec<LinkTarget>,
    focus: Vec<FocusHandle>,
    menu_focus: [FocusHandle; 7],
    menu_focus_pending: Rc<Cell<bool>>,
    menu: Rc<RefCell<Option<(usize, Point<Pixels>)>>>,
    bounds: Bounds<Pixels>,
    epoch: Rc<Cell<u64>>,
    dismissed: Rc<Cell<bool>>,
    tooltip_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    focused: Option<usize>,
}
impl IntoElement for LinkRanges {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}
impl Element for LinkRanges {
    type PrepaintState = (
        Vec<AnyElement>,
        Rc<RefCell<Option<(usize, Point<Pixels>)>>>,
        Rc<Cell<u64>>,
        Rc<Cell<bool>>,
        Rc<Cell<Option<Bounds<Pixels>>>>,
    );
    type RequestLayoutState = ();

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone().into())
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        (self.child.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        (): &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        self.child.prepaint(window, cx);
        window.with_element_state::<Interaction, _>(id.unwrap(), |previous, window| {
            let targets: Vec<_> = self.links.iter().map(|(_, t)| t.clone()).collect();
            let mut state =
                previous.filter(|s| s.targets == targets).unwrap_or_else(|| Interaction {
                    focus: targets.iter().map(|_| cx.focus_handle().tab_stop(true)).collect(),
                    targets,
                    menu_focus: std::array::from_fn(|_| cx.focus_handle().tab_stop(true)),
                    menu_focus_pending: Rc::default(),
                    menu: Rc::default(),
                    bounds,
                    epoch: Rc::default(),
                    dismissed: Rc::default(),
                    tooltip_bounds: Rc::default(),
                    focused: None,
                });
            if state.bounds != bounds {
                state.menu.borrow_mut().take();
                state.epoch.set(state.epoch.get().wrapping_add(1));
                state.dismissed.set(true);
            }
            let focused = state.focus.iter().position(|focus| focus.is_focused(window));
            if focused != state.focused {
                state.dismissed.set(false);
            }
            state.focused = focused;
            state.bounds = bounds;
            state.tooltip_bounds.set(None);
            let theme = Theme::of(cx).clone();
            let mut overlays = Vec::new();
            for (index, (range, target)) in self.links.iter().enumerate() {
                for (part, rect) in range_rects(&self.layout, range, 0., 0.).into_iter().enumerate()
                {
                    let menu = state.menu.clone();
                    let keyboard_menu = menu.clone();
                    let focus = state.focus[index].clone();
                    let click_target = target.clone();
                    let destination = target.original.clone();
                    let tooltip_bounds = state.tooltip_bounds.clone();
                    let click_ui = self.ui.clone();
                    let tooltip_ui = self.ui.clone();
                    let menu_focus_pending = state.menu_focus_pending.clone();
                    let pointer_focus_pending = menu_focus_pending.clone();
                    let keyboard_dismissed = state.dismissed.clone();
                    let keyboard_epoch = state.epoch.clone();
                    let hit = div()
                        .id(format!("link-{index}-{part}-{}", state.epoch.get()))
                        // Removing the builder cancels both visible tooltips
                        // and GPUI's delayed show task while the menu owns input.
                        .when(state.menu.borrow().is_none(), |hit| {
                            hit.hoverable_tooltip(move |_, cx| {
                                let url = destination.clone();
                                let bounds = tooltip_bounds.clone();
                                let ui = tooltip_ui.clone();
                                cx.new(|_| {
                                    // A file link shows its decoded path —
                                    // root-relative inside the workspace,
                                    // absolute outside — not the raw href.
                                    let shown = ui
                                        .as_ref()
                                        .and_then(|ui| ui.file_link(&url))
                                        .map(|file| file.path)
                                        .unwrap_or(url);
                                    super::link_destination::Destination(shown, bounds)
                                })
                                .into()
                            })
                        })
                        .w(rect.size.width)
                        .h(rect.size.height)
                        .cursor_pointer()
                        .role(Role::Link)
                        .aria_label(target.label.clone())
                        .when(part == 0, |el| el.track_focus(&focus))
                        .focus_visible(|s| {
                            s.bg(theme.selection).border_1().border_color(theme.accent)
                        })
                        .on_click(move |event, window, cx| {
                            if click_is_activation(event)
                                && (matches!(event, ClickEvent::Keyboard(_))
                                    || super::selection::selected_text().is_none())
                            {
                                activate_link(
                                    click_target.clone(),
                                    LinkAction::Primary,
                                    click_ui.as_ref(),
                                    window,
                                    cx,
                                );
                            }
                        })
                        .on_mouse_down(MouseButton::Right, move |event, window, cx| {
                            *menu.borrow_mut() = Some((index, event.position));
                            pointer_focus_pending.set(true);
                            cx.stop_propagation();
                            window.refresh();
                        })
                        .on_key_down(move |event, window, cx| {
                            match event.keystroke.key.as_str() {
                                "tab" => {
                                    if event.keystroke.modifiers.shift {
                                        window.focus_prev(cx);
                                    } else {
                                        window.focus_next(cx);
                                    }
                                }
                                "f10" if event.keystroke.modifiers.shift => {
                                    *keyboard_menu.borrow_mut() = Some((index, rect.bottom_left()));
                                    menu_focus_pending.set(true);
                                    window.refresh();
                                }
                                "escape" => {
                                    keyboard_menu.borrow_mut().take();
                                    keyboard_dismissed.set(true);
                                    keyboard_epoch.set(keyboard_epoch.get().wrapping_add(1));
                                    window.refresh();
                                }
                                _ => {
                                    cx.propagate();
                                    return;
                                }
                            }
                            cx.stop_propagation();
                        });
                    let mut hit = hit.into_any_element();
                    hit.prepaint_as_root(
                        rect.origin,
                        rect.size.map(AvailableSpace::Definite),
                        window,
                        cx,
                    );
                    overlays.push(hit);
                }
            }
            if state.menu.borrow().is_none()
                && !state.dismissed.get()
                && let Some(index) = focused
                && let Some(rect) = range_rects(&self.layout, &self.links[index].0, 0., 0.).first()
            {
                let shown = self
                    .ui
                    .as_ref()
                    .and_then(|ui| ui.file_link(&state.targets[index].original))
                    .map_or_else(|| state.targets[index].original.clone(), |file| file.path);
                let card = super::link_destination::destination_card(
                    &shown,
                    state.tooltip_bounds.clone(),
                    window,
                    cx,
                );
                let dismissed = state.dismissed.clone();
                // This disclosure is not a menu: menu_at consumes every
                // outside press, preventing other controls from receiving it.
                // An outside press only hides it; the links keep their
                // ids, so a press on one of them still clicks it.
                let mut popup = gpui::deferred(
                    gpui::anchored()
                        .position(rect.bottom_left())
                        .anchor(gpui::Anchor::TopLeft)
                        .snap_to_window_with_margin(px(8.))
                        .child(
                            div()
                                .id("focused-link-destination")
                                .occlude()
                                .on_mouse_down_out(move |_, window, _| {
                                    dismissed.set(true);
                                    window.refresh();
                                })
                                .child(card),
                        ),
                )
                .priority(1)
                .into_any_element();
                popup.prepaint_as_root(
                    bounds.origin,
                    window.viewport_size().map(AvailableSpace::Definite),
                    window,
                    cx,
                );
                overlays.push(popup);
            }
            if let Some((index, position)) = *state.menu.borrow() {
                let theme = theme.for_popup();
                let menu = state.menu.clone();
                let dismiss_menu = state.menu.clone();
                let return_focus = state.focus[index].clone();
                let menu_focus = state.menu_focus.clone();
                let pending = state.menu_focus_pending.clone();
                let initial_focus = state.menu_focus[0].clone();
                // A resolved file link swaps the web actions for rows about
                // the file itself: "Open in Slopty" opens it in the file
                // viewer, and the system-level rows mount only when the
                // owning root lives on this device. The tab cycle visits
                // only mounted rows.
                let file =
                    self.ui.as_ref().and_then(|ui| ui.file_link(&state.targets[index].original));
                let cycle: &[usize] = match &file {
                    Some(file) if file.local => &[0, 1, 2, 3],
                    Some(_) => &[0, 3],
                    None => &[0, 4, 5, 6],
                };
                let file_resolved = file.is_some();
                let mut card = popover::popover_card(&theme)
                    .child(
                        gpui::canvas(
                            move |_, window, cx| {
                                // Claim focus only when the deferred menu mounts. The
                                // shell recovers focus from handles that are not mounted.
                                if pending.replace(false) {
                                    window.focus(&initial_focus, cx);
                                }
                            },
                            |_, (), _, _| {},
                        )
                        .absolute()
                        .w(px(0.))
                        .h(px(0.)),
                    )
                    .id("link-actions")
                    .on_key_down(move |event, window, cx| {
                        match event.keystroke.key.as_str() {
                            "escape" => {
                                dismiss_menu.borrow_mut().take();
                                window.focus(&return_focus, cx);
                                window.refresh();
                            }
                            "tab" | "down" | "up" => {
                                let current = cycle
                                    .iter()
                                    .position(|ix| menu_focus[*ix].is_focused(window))
                                    .unwrap_or(0);
                                let backwards = event.keystroke.key == "up"
                                    || (event.keystroke.key == "tab"
                                        && event.keystroke.modifiers.shift);
                                let next = if backwards {
                                    (current + cycle.len() - 1) % cycle.len()
                                } else {
                                    (current + 1) % cycle.len()
                                };
                                window.focus(&menu_focus[cycle[next]], cx);
                            }
                            _ => return,
                        }
                        cx.stop_propagation();
                    })
                    .w(px(260.))
                    .flex()
                    .flex_col()
                    .on_mouse_down_out(move |_, window, _| {
                        menu.borrow_mut().take();
                        window.refresh();
                    });
                // The menu's rows all share one shape: a focus slot, a
                // fade key, a click that closes the menu. Link actions go
                // through `activate_link`; file rows run a captured action.
                let action_row = |action: LinkAction,
                                  label: &'static str,
                                  icon: &'static str,
                                  slot: usize,
                                  selector: &'static str| {
                    let target = state.targets[index].clone();
                    let ui = self.ui.clone();
                    let menu = state.menu.clone();
                    let enabled = action == LinkAction::Copy
                        || target.navigation.is_ok()
                        || (action == LinkAction::Internal && file_resolved);
                    popover::menu_row(&theme, false, format!("{}-link-{index}-{selector}", self.id))
                        .id(label)
                        .debug_selector(move || selector.into())
                        .child(icons::icon(icon).size(px(16.)).text_color(theme.text_muted))
                        .child(label)
                        .track_focus(&state.menu_focus[slot])
                        .role(Role::Button)
                        .aria_label(label)
                        .when(!enabled, |el| el.opacity(0.45))
                        .focus_visible(|s| s.bg(crate::theme::card_selected_bg()))
                        .on_click(move |_, window, cx| {
                            if enabled {
                                activate_link(target.clone(), action, ui.as_ref(), window, cx);
                            }
                            menu.borrow_mut().take();
                            window.refresh();
                        })
                };
                let file_row = |label: &'static str,
                                icon: &'static str,
                                slot: usize,
                                selector: &'static str,
                                act: Rc<dyn Fn(&mut App)>| {
                    let menu = state.menu.clone();
                    popover::menu_row(&theme, false, format!("{}-link-{index}-{selector}", self.id))
                        .id(label)
                        .debug_selector(move || selector.into())
                        .child(icons::icon(icon).size(px(16.)).text_color(theme.text_muted))
                        .child(label)
                        .track_focus(&state.menu_focus[slot])
                        .role(Role::Button)
                        .aria_label(label)
                        .focus_visible(|s| s.bg(crate::theme::card_selected_bg()))
                        .on_click(move |_, window, cx| {
                            act(cx);
                            menu.borrow_mut().take();
                            window.refresh();
                        })
                };
                card = card.child(action_row(
                    LinkAction::Internal,
                    "Open in Slopty",
                    if file_resolved { icons::DOCUMENT } else { icons::GLOBE },
                    0,
                    "link-menu-open-slopty",
                ));
                if let Some(file) = &file {
                    if file.local {
                        // The system opener goes through a file:// URL —
                        // the resolved path is a filesystem path, not a web
                        // address.
                        if let Ok(url) = url::Url::from_file_path(&file.absolute) {
                            let url = url.to_string();
                            card = card.child(file_row(
                                "Open with default app",
                                icons::ARROW_UP_RIGHT,
                                1,
                                "link-menu-open-default",
                                Rc::new(move |cx| cx.open_url(url.as_str())),
                            ));
                        }
                        let path = file.absolute.clone();
                        card = card.child(file_row(
                            "Show in folder",
                            icons::FOLDER,
                            2,
                            "link-menu-show-in-folder",
                            Rc::new(move |cx| cx.reveal_path(&path)),
                        ));
                    }
                    // Copy the resolved absolute path — the root join, not
                    // the raw link text.
                    let path = file.absolute.to_string_lossy().into_owned();
                    card = card.child(file_row(
                        "Copy file path",
                        icons::COPY,
                        3,
                        "link-menu-copy-path",
                        Rc::new(move |cx| {
                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(path.clone()));
                        }),
                    ));
                }
                // Browser actions and the web-link preference mean nothing
                // for a file on disk.
                if !file_resolved {
                    card = card.child(action_row(
                        LinkAction::External,
                        "Open in external browser",
                        icons::ARROW_UP_RIGHT,
                        4,
                        "link-menu-open-external",
                    ));
                    card = card.child(action_row(
                        LinkAction::Copy,
                        "Copy link address",
                        icons::COPY,
                        5,
                        "link-menu-copy-address",
                    ));
                    let open_in_slopty = crate::settings::current(cx).open_web_links_in_slopty;
                    let menu = state.menu.clone();
                    card = card.child(popover::menu_separator()).child(
                        popover::menu_row(
                            &theme,
                            false,
                            format!("{}-link-{index}-default-destination", self.id),
                        )
                        .id("Open links in Slopty")
                        .debug_selector(|| "link-menu-default-destination".into())
                        .child(div().w(px(16.)).flex_none().when(open_in_slopty, |el| {
                            el.child(
                                icons::icon(icons::CHECK)
                                    .size(px(16.))
                                    .text_color(theme.text_muted),
                            )
                        }))
                        .child("Open links in Slopty")
                        .track_focus(&state.menu_focus[6])
                        .role(Role::Button)
                        .aria_label(if open_in_slopty {
                            "Open links in Slopty, checked"
                        } else {
                            "Open links in Slopty, unchecked"
                        })
                        .focus_visible(|s| s.bg(crate::theme::card_selected_bg()))
                        .on_click(move |_, window, cx| {
                            crate::settings::update(
                                crate::settings::SavePolicy::Immediate,
                                cx,
                                |settings| {
                                    settings.open_web_links_in_slopty = !open_in_slopty;
                                },
                            );
                            menu.borrow_mut().take();
                            cx.refresh_windows();
                            window.refresh();
                        }),
                    );
                }
                let mut popup = popover::menu_at(
                    "transcript-link-actions",
                    position,
                    card.into_any_element(),
                    None,
                );
                popup.prepaint_as_root(
                    bounds.origin,
                    window.viewport_size().map(AvailableSpace::Definite),
                    window,
                    cx,
                );
                overlays.push(popup);
            }
            (
                (
                    overlays,
                    state.menu.clone(),
                    state.epoch.clone(),
                    state.dismissed.clone(),
                    state.tooltip_bounds.clone(),
                ),
                state,
            )
        })
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        (): &mut (),
        paint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let menu = paint.1.clone();
        let epoch = paint.2.clone();
        let dismissed = paint.3.clone();
        let tooltip_bounds = paint.4.clone();
        window.on_mouse_event(move |event: &ScrollEvent, phase, window, _| {
            if phase == DispatchPhase::Capture
                && !tooltip_bounds.get().is_some_and(|rect| rect.contains(&event.position))
            {
                menu.borrow_mut().take();
                epoch.set(epoch.get().wrapping_add(1));
                dismissed.set(true);
                window.refresh();
            }
        });
        self.child.paint(window, cx);
        for overlay in &mut paint.0 {
            overlay.paint(window, cx);
        }
    }
}
fn click_is_activation(event: &ClickEvent) -> bool {
    match event {
        ClickEvent::Mouse(event) => {
            event.down.button == MouseButton::Left
                && event.down.click_count == 1
                && (event.up.position - event.down.position).magnitude() <= 4.
        }
        ClickEvent::Keyboard(_) | ClickEvent::Touch(_) => true,
    }
}

#[cfg(test)]
mod tests {
    use gpui::{MouseClickEvent, MouseDownEvent, MouseUpEvent, TestAppContext, point};

    use super::*;
    #[test]
    fn selection_drags_and_secondary_clicks_do_not_navigate() {
        let mut event = MouseClickEvent {
            down: MouseDownEvent {
                button: MouseButton::Left,
                click_count: 1,
                ..Default::default()
            },
            up: MouseUpEvent::default(),
        };
        assert!(click_is_activation(&ClickEvent::Mouse(event.clone())));
        event.up.position = point(px(20.), px(0.));
        assert!(!click_is_activation(&ClickEvent::Mouse(event.clone())));
        event.up.position = event.down.position;
        event.down.button = MouseButton::Right;
        assert!(!click_is_activation(&ClickEvent::Mouse(event.clone())));
        event.down.button = MouseButton::Left;
        event.down.click_count = 2;
        assert!(!click_is_activation(&ClickEvent::Mouse(event)));
    }
    #[gpui::test]
    fn all_actions_keep_the_destination(cx: &mut TestAppContext) {
        let window = cx.add_window(|_, _| gpui::Empty);
        window
            .update(cx, |_, window, cx| {
                let target = LinkTarget::new(
                    "A misleading label",
                    "https://example.com/full?query=yes#fragment",
                );
                activate_link(target.clone(), LinkAction::Copy, None, window, cx);
                assert_eq!(
                    cx.read_from_clipboard().unwrap().text().as_deref(),
                    Some(target.original.as_str())
                );
                for action in [LinkAction::Internal, LinkAction::External] {
                    let seen = Rc::new(RefCell::new(None));
                    let captured = seen.clone();
                    let ui = LinkUi {
                        source_session: Some("parent".into()),
                        source_local: false,
                        file_roots: None,
                        handler: Rc::new(move |a, _, _| {
                            *captured.borrow_mut() =
                                Some((a.target.clone(), a.action, a.source_session.clone()));
                            LinkOutcome::Rejected
                        }),
                    };
                    activate_link(target.clone(), action, Some(&ui), window, cx);
                    assert_eq!(
                        *seen.borrow(),
                        Some((target.clone(), action, Some("parent".into())))
                    );
                }
            })
            .unwrap();
    }
}
