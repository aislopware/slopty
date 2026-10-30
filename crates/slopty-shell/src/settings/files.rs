// Derived from zeron (MIT, github.com/zeronsh/zeron @ 053ce2e), see THIRD_PARTY_NOTICES.md
//! Settings → Files: local preferences for workspace-file editing.

use gpui::prelude::*;
use gpui::{Context, EventEmitter, Window, div, px};

use super::widgets;
use crate::popover;
use crate::theme::Theme;

const DELAY_OPTIONS: [u64; 5] = [300, 600, 900, 1_500, 3_000];

/// A Files preference the person changed, for the shell to keep and apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilesSettingsEvent {
    /// Autosave turned on or off.
    Autosave(bool),
    /// The autosave delay, in milliseconds.
    AutosaveDelay(u64),
    /// Word wrap in the editor turned on or off.
    WordWrap(bool),
    /// Whether the file tree shows ignored and hidden files.
    ShowAllFiles(bool),
}

/// Settings → Files.
pub struct FilesSettingsPage {
    scroll: widgets::PageScroll,
    autosave_enabled: bool,
    autosave_delay_ms: u64,
    word_wrap: bool,
    show_all_files: bool,
    delay_select: widgets::SelectState,
}

impl EventEmitter<FilesSettingsEvent> for FilesSettingsPage {}

impl std::fmt::Debug for FilesSettingsPage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FilesSettingsPage")
            .field("autosave_enabled", &self.autosave_enabled)
            .field("autosave_delay_ms", &self.autosave_delay_ms)
            .field("word_wrap", &self.word_wrap)
            .field("show_all_files", &self.show_all_files)
            .finish_non_exhaustive()
    }
}

impl FilesSettingsPage {
    /// The page, showing the preferences as kept.
    pub fn new(
        autosave_enabled: bool,
        autosave_delay_ms: u64,
        word_wrap: bool,
        show_all_files: bool,
        _cx: &mut Context<Self>,
    ) -> Self {
        Self {
            scroll: widgets::PageScroll::default(),
            autosave_enabled,
            autosave_delay_ms,
            word_wrap,
            show_all_files,
            delay_select: widgets::SelectState::default(),
        }
    }

    /// Show word wrap as `word_wrap`, changed elsewhere.
    pub fn set_word_wrap(&mut self, word_wrap: bool, cx: &mut Context<Self>) {
        if self.word_wrap == word_wrap {
            return;
        }
        self.word_wrap = word_wrap;
        cx.notify();
    }

    /// Show the file tree's filter as `show_all_files`, changed elsewhere.
    pub fn set_show_all_files(&mut self, show_all_files: bool, cx: &mut Context<Self>) {
        if self.show_all_files == show_all_files {
            return;
        }
        self.show_all_files = show_all_files;
        cx.notify();
    }

    // A row whose value is not its default offers "Reset", and only then (M10).
    fn reset(
        id: &'static str,
        label: &str,
        is_default: bool,
        theme: &Theme,
        cx: &Context<Self>,
        reset: impl Fn(&mut Self, &mut Context<Self>) + 'static,
    ) -> Option<gpui::Stateful<gpui::Div>> {
        widgets::reset_action(theme, id, label, is_default).map(|button| {
            button.on_click(cx.listener(move |this, _, _, cx| {
                reset(this, cx);
                cx.notify();
            }))
        })
    }
}

impl popover::ScrollRailHost for FilesSettingsPage {
    fn rail_bar(&mut self) -> &mut popover::MenuScrollbarState {
        self.scroll.rail_bar()
    }

    fn rail_scroll(&self) -> Option<gpui::ScrollHandle> {
        self.scroll.rail_scroll()
    }
}

impl Render for FilesSettingsPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).for_settings_surface();
        let autosave_enabled = self.autosave_enabled;
        let selected = self.autosave_delay_ms;
        let word_wrap = self.word_wrap;
        let show_all_files = self.show_all_files;
        let delay_control =
            widgets::select("files-autosave-delay", "Autosave delay", &theme, |page: &mut Self| {
                &mut page.delay_select
            })
            .options(
                DELAY_OPTIONS.into_iter().map(|delay| {
                    widgets::SelectOption::new(if delay >= 1_000 {
                        format!("{} s", std::time::Duration::from_millis(delay).as_secs_f32())
                    } else {
                        format!("{delay} ms")
                    })
                    .standing(widgets::Standing::of(
                        delay == crate::settings::FILES_AUTOSAVE_DELAY_DEFAULT_MS,
                        false,
                    ))
                }),
                DELAY_OPTIONS.iter().position(|delay| *delay == selected).unwrap_or_default(),
            )
            .width(112.0)
            .on_select(|page, ix, _, cx| {
                let Some(&delay) = DELAY_OPTIONS.get(ix) else {
                    return;
                };
                page.autosave_delay_ms = delay;
                cx.emit(FilesSettingsEvent::AutosaveDelay(delay));
                cx.notify();
            })
            .render(&self.delay_select, cx);
        let card = widgets::section_card(&theme)
            .child(
                widgets::card_row(&theme, true)
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(160.0))
                            .flex()
                            .flex_col()
                            .child(widgets::row_title(&theme, "Autosave")),
                    )
                    .children(Self::reset(
                        "files-autosave-reset",
                        "Autosave",
                        !autosave_enabled,
                        &theme,
                        cx,
                        |this, cx| {
                            this.autosave_enabled = false;
                            cx.emit(FilesSettingsEvent::Autosave(false));
                        },
                    ))
                    .child(
                        widgets::toggle_switch(&theme, autosave_enabled, "files-autosave")
                            .id("files-autosave-toggle")
                            .cursor_pointer()
                            .tab_index(0)
                            .role(gpui::Role::Switch)
                            .aria_label("Autosave")
                            .aria_toggled(if autosave_enabled {
                                gpui::Toggled::True
                            } else {
                                gpui::Toggled::False
                            })
                            .map(|el| crate::surface_chrome::focus_ring_revealing(el, &theme))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.autosave_enabled = !this.autosave_enabled;
                                cx.emit(FilesSettingsEvent::Autosave(this.autosave_enabled));
                                cx.notify();
                            })),
                    ),
            )
            .when(autosave_enabled, |card| {
                card.child(
                    widgets::card_row(&theme, true)
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(160.0))
                                .flex()
                                .flex_col()
                                .child(widgets::row_title(&theme, "Autosave delay")),
                        )
                        .children(Self::reset(
                            "files-autosave-delay-reset",
                            "Autosave delay",
                            selected == crate::settings::FILES_AUTOSAVE_DELAY_DEFAULT_MS,
                            &theme,
                            cx,
                            |this, cx| {
                                let delay = crate::settings::FILES_AUTOSAVE_DELAY_DEFAULT_MS;
                                this.autosave_delay_ms = delay;
                                cx.emit(FilesSettingsEvent::AutosaveDelay(delay));
                            },
                        ))
                        .child(delay_control),
                )
            })
            .child(
                widgets::card_row(&theme, false)
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(160.0))
                            .flex()
                            .flex_col()
                            .child(widgets::row_title(&theme, "Word wrap")),
                    )
                    .children(Self::reset(
                        "files-word-wrap-reset",
                        "Word wrap",
                        !word_wrap,
                        &theme,
                        cx,
                        |this, cx| {
                            this.word_wrap = false;
                            cx.emit(FilesSettingsEvent::WordWrap(false));
                        },
                    ))
                    .child(
                        widgets::toggle_switch(&theme, word_wrap, "files-word-wrap")
                            .id("files-word-wrap-toggle")
                            .cursor_pointer()
                            .tab_index(0)
                            .role(gpui::Role::Switch)
                            .aria_label("Word wrap")
                            .aria_toggled(if word_wrap {
                                gpui::Toggled::True
                            } else {
                                gpui::Toggled::False
                            })
                            .map(|el| crate::surface_chrome::focus_ring_revealing(el, &theme))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.word_wrap = !this.word_wrap;
                                cx.emit(FilesSettingsEvent::WordWrap(this.word_wrap));
                                cx.notify();
                            })),
                    ),
            )
            .child(
                widgets::card_row(&theme, false)
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(160.0))
                            .flex()
                            .flex_col()
                            .child(widgets::row_title(&theme, "Show hidden files"))
                            .child(widgets::meta_line(
                                &theme,
                                vec![
                                    div().child("Dotfiles and what git ignores").into_any_element(),
                                ],
                            )),
                    )
                    .children(Self::reset(
                        "files-show-all-reset",
                        "Show hidden files",
                        !show_all_files,
                        &theme,
                        cx,
                        |this, cx| {
                            this.show_all_files = false;
                            cx.emit(FilesSettingsEvent::ShowAllFiles(false));
                        },
                    ))
                    .child(
                        widgets::toggle_switch(&theme, show_all_files, "files-show-all")
                            .id("files-show-all-toggle")
                            .cursor_pointer()
                            .tab_index(0)
                            .role(gpui::Role::Switch)
                            .aria_label("Show hidden files")
                            .aria_toggled(if show_all_files {
                                gpui::Toggled::True
                            } else {
                                gpui::Toggled::False
                            })
                            .map(|el| crate::surface_chrome::focus_ring_revealing(el, &theme))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.show_all_files = !this.show_all_files;
                                cx.emit(FilesSettingsEvent::ShowAllFiles(this.show_all_files));
                                cx.notify();
                            })),
                    ),
            );

        let scrollbar = popover::rail(self, "files-settings-page-scrollbar", &theme, cx);
        div()
            .id("files-settings-page-host")
            .relative()
            .size_full()
            .on_hover(widgets::PageScroll::hover_listener(cx, |page| &mut page.scroll))
            .child(
                crate::edge_fade::edge_faded(
                    16.0,
                    true,
                    true,
                    div()
                        .id("files-settings-page")
                        .size_full()
                        .overflow_y_scroll()
                        .track_scroll(self.scroll.handle())
                        .child(
                            widgets::page_column()
                                // One page head (`widgets::page_head`).
                                .child(widgets::page_head(
                                    &theme,
                                    "Files",
                                    None,
                                    "How the file editor saves and shows files.",
                                    None,
                                ))
                                .child(card),
                        ),
                )
                .fade_overflow_y(self.scroll.handle()),
            )
            .children(scrollbar)
    }
}

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;

    use super::*;

    // A row offers "Reset" only while its value is not the default, and Reset puts the
    // default back (M10).
    #[gpui::test]
    fn a_row_offers_reset_only_off_its_default(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_global(Theme::default()));
        let (page, cx) = cx.add_window_view(|_, cx| {
            FilesSettingsPage::new(
                true,
                crate::settings::FILES_AUTOSAVE_DELAY_DEFAULT_MS,
                true,
                false,
                cx,
            )
        });
        let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let seen = std::rc::Rc::clone(&events);
        let _events = cx
            .update(|_, cx| cx.subscribe(&page, move |_, event, _| seen.borrow_mut().push(*event)));
        let draw = |cx: &mut gpui::VisualTestContext| {
            cx.update(|window, cx| {
                window.refresh();
                window.draw(cx).clear(cx);
            });
        };
        draw(cx);
        assert!(
            cx.debug_bounds("files-autosave-reset").is_some(),
            "autosave is on, off by default"
        );
        assert!(cx.debug_bounds("files-word-wrap-reset").is_some(), "wrap is on, off by default");
        assert!(
            cx.debug_bounds("files-autosave-delay-reset").is_none(),
            "the delay is the default"
        );
        assert!(cx.debug_bounds("files-show-all-reset").is_none(), "hidden files are off");
        let reset = cx.debug_bounds("files-word-wrap-reset").unwrap().center();
        cx.simulate_click(reset, gpui::Modifiers::default());
        draw(cx);
        assert!(cx.debug_bounds("files-word-wrap-reset").is_none());
        page.read_with(cx, |page, _| assert!(!page.word_wrap));
        assert_eq!(*events.borrow(), [FilesSettingsEvent::WordWrap(false)]);
    }
}
