// Derived from zeron (MIT, github.com/zeronsh/zeron @ 053ce2e), see THIRD_PARTY_NOTICES.md
//! Per-agent composer completion preferences inside Settings → Providers.

use super::HarnessesPage;
use crate::{settings, settings::widgets, theme::Theme};
use gpui::{AnyElement, Context, div, prelude::*, px};
use slopty_view_proto::HarnessId;

impl HarnessesPage {
    fn toggle_completion(&mut self, harness: HarnessId, dollar: bool, cx: &mut Context<Self>) {
        settings::update(settings::SavePolicy::Immediate, cx, |settings| {
            let mut preferences = settings.skill_completion(harness);
            if dollar {
                preferences.dollar = !preferences.dollar;
            } else {
                preferences.separate_from_slash = !preferences.separate_from_slash;
            }
            settings
                .skill_completion_by_harness
                .insert(harness, preferences);
        });
        cx.notify();
    }

    // slopty: back to the agent's default ($ for skills, kept out of /) (M10).
    fn reset_completion(&mut self, harness: HarnessId, dollar: bool, cx: &mut Context<Self>) {
        let default = settings::SkillCompletionSettings::for_harness(harness);
        settings::update(settings::SavePolicy::Immediate, cx, |settings| {
            let mut preferences = settings.skill_completion(harness);
            if dollar {
                preferences.dollar = default.dollar;
            } else {
                preferences.separate_from_slash = default.separate_from_slash;
            }
            settings
                .skill_completion_by_harness
                .insert(harness, preferences);
        });
        cx.notify();
    }

    pub(super) fn render_completion_for(
        &self,
        harness: HarnessId,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let current = settings::current(cx);
        let preferences = current.skill_completion(harness);
        let defaults = settings::SkillCompletionSettings::for_harness(harness);
        let rows = [
            (
                true,
                "Use $ for skills",
                "Type $ in the composer to pick a skill.",
                preferences.dollar,
                defaults.dollar,
            ),
            (
                false,
                "Separate / commands",
                "Keep skills out of the / menu.",
                preferences.separate_from_slash,
                defaults.separate_from_slash,
            ),
        ]
        .into_iter()
        .enumerate()
        .map(|(ix, (dollar, label, description, enabled, default))| {
            let reset = widgets::reset_action(
                theme,
                format!("completion-reset-{harness:?}-{dollar}"),
                label,
                enabled == default,
            )
            .map(|reset| {
                reset.on_click(cx.listener(move |page, _, _, cx| {
                    cx.stop_propagation();
                    page.reset_completion(harness, dollar, cx);
                }))
            });
            div()
                .id(format!("completion-{harness:?}-{dollar}"))
                .min_h(px(52.0))
                .py(px(10.0))
                .when(ix > 0, |row| {
                    row.border_t_1().border_color(widgets::row_divider(theme))
                })
                .flex()
                .flex_row()
                .items_center()
                .gap(px(16.0))
                .role(gpui::Role::Switch)
                .aria_label(format!("{harness:?}: {label}"))
                .aria_toggled(if enabled {
                    gpui::Toggled::True
                } else {
                    gpui::Toggled::False
                })
                .tab_index(0)
                .cursor_pointer()
                // slopty: the whole row is the stop, its ring inside its edge (M8).
                .map(|el| crate::surface_chrome::focus_ring_inset(el, theme))
                .on_click(
                    cx.listener(move |page, _, _, cx| page.toggle_completion(harness, dollar, cx)),
                )
                .on_key_down(cx.listener(move |page, event: &gpui::KeyDownEvent, _, cx| {
                    if !event.is_held && matches!(event.keystroke.key.as_str(), "enter" | "space") {
                        page.toggle_completion(harness, dollar, cx);
                        cx.stop_propagation();
                    }
                }))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(widgets::row_title(theme, label))
                        .child(widgets::meta_line(
                            theme,
                            vec![div().child(description).into_any_element()],
                        )),
                )
                .children(reset)
                .child(widgets::toggle_switch(
                    theme,
                    enabled,
                    format!("completion-switch-{harness:?}-{dollar}"),
                ))
        });
        div()
            .flex()
            .flex_col()
            .child(widgets::details_label(theme, "Completion"))
            .children(rows)
            .into_any_element()
    }
}

#[cfg(test)]
mod completion_tests {
    use super::*;

    #[gpui::test]
    fn completion_preferences_save_independently_per_agent(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| settings::init(Default::default(), dir.path(), cx));
        let state = cx.new(|_| crate::state::AppState::new());
        let page = cx.new(|cx| HarnessesPage::new(state, cx));
        page.update(cx, |page, cx| {
            page.toggle_completion(HarnessId::CLAUDE_CODE, true, cx);
            page.toggle_completion(HarnessId::CLAUDE_CODE, false, cx);
            page.toggle_completion(HarnessId::OPENCODE, true, cx);
        });
        // Both default on, so each flip turns its own preference off.
        let loaded = settings::UiSettings::load(dir.path());
        assert!(!loaded.skill_completion(HarnessId::CLAUDE_CODE).dollar);
        assert!(
            !loaded
                .skill_completion(HarnessId::CLAUDE_CODE)
                .separate_from_slash
        );
        assert!(!loaded.skill_completion(HarnessId::OPENCODE).dollar);
        assert!(
            loaded
                .skill_completion(HarnessId::OPENCODE)
                .separate_from_slash
        );
        assert!(
            loaded
                .skill_completion_by_harness
                .contains_key(&HarnessId::CLAUDE_CODE)
        );
    }
}
