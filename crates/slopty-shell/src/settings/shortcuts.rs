// Derived from zeron (MIT, github.com/zeronsh/zeron @ 75b7260), see THIRD_PARTY_NOTICES.md
//! Settings → Shortcuts (feature-inventory §1.4): a table of the rebindable
//! bindings — click a combo to record (Esc cancels), live conflict detection,
//! per-row Reset and Restore defaults. Changes emit [`ShortcutsEvent`]; the
//! shell persists them and re-applies the app keymap.

use gpui::{
    Context, Entity, EventEmitter, FocusHandle, Keystroke, SharedString, Window, div, prelude::*,
    px,
};

use crate::popover; // slopty: appshots removed.

use crate::settings::widgets;
use crate::settings::{
    ComposerSendBehavior, KeymapConfig, ShortcutId, combo_from_keystroke,
}; // slopty: the pull-request destination's row is gone
use crate::state::AppState;
use crate::theme::Theme;
use crate::motion::HoverFade as _;

/// Outcome of one keystroke while recording. Pure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordOutcome {
    /// Esc — abandon recording, keep the old combo.
    Cancelled,
    /// A bare modifier (or unusable key) — stay recording.
    Ignored,
    /// A full combo landed.
    Set(String),
}

pub fn record_key(key: &str, ctrl: bool, alt: bool, shift: bool, cmd: bool) -> RecordOutcome {
    if key.eq_ignore_ascii_case("escape") {
        return RecordOutcome::Cancelled;
    }
    match combo_from_keystroke(ctrl, alt, shift, cmd, key) {
        Some(combo) => RecordOutcome::Set(combo),
        None => RecordOutcome::Ignored,
    }
}

#[derive(Debug, Clone)]
pub enum ShortcutsEvent {
    /// The keymap changed — persist + re-apply.
    KeymapChanged(KeymapConfig),
    /// The Escape fallback changed — persist it locally.
    EscapeStopsActiveAgentChanged(bool),
    /// The composer send behavior changed — persist + re-apply.
    ComposerSendBehaviorChanged(ComposerSendBehavior),
    // slopty: appshots removed.
}

pub struct ShortcutsPage {
    // slopty: appshots removed.
    general_page: bool,
    scroll: crate::settings::widgets::PageScroll,
    /// Working copy (kept in sync with the shell via change events).
    keymap: KeymapConfig,
    escape_stops_active_agent: bool,
    composer_send_behavior: ComposerSendBehavior,
    recording: Option<ShortcutId>,
    recording_blur: Option<gpui::Subscription>,
    recording_interceptor: Option<gpui::Subscription>,
    /// A rejected record attempt ("{Combo} is already assigned to {label}.") —
    /// conflicts never persist; they're refused at record time, as in slopty.
    conflict_notice: Option<SharedString>,
    focus: FocusHandle,
    // slopty: appshots removed.
    send_select: widgets::SelectState,
    // slopty: no pull-request pages.
    // slopty: no thread naming: an agent's thread is titled by the agent.
    _state: Entity<AppState>,
}

impl EventEmitter<ShortcutsEvent> for ShortcutsPage {}

impl ShortcutsPage {
    pub fn new(
        state: Entity<AppState>,
        keymap: KeymapConfig,
        escape_stops_active_agent: bool,
        composer_send_behavior: ComposerSendBehavior,
        // slopty: appshots removed.
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            general_page: false,
            scroll: crate::settings::widgets::PageScroll::default(),
            keymap,
            escape_stops_active_agent,
            composer_send_behavior,
            recording: None,
            recording_blur: None,
            recording_interceptor: None,
            conflict_notice: None,
            focus: cx.focus_handle(),
            // slopty: appshots removed.
            send_select: widgets::SelectState::default(),
            // slopty: no pull-request pages.
            // slopty: no thread naming.
            _state: state,
        }
    }

    // slopty: dictation and appshots removed.

    pub fn show_section(&mut self, general: bool) { // slopty: appshots removed.
        if self.general_page != general {
            self.stop_recording();
            self.conflict_notice = None;
            self.general_page = general;
            // One scroll state serves these pages — rewind it so each opens
            // at the top instead of where the other was left.
            self.scroll.reset();
        }
    }

    fn start_recording(&mut self, id: ShortcutId, window: &mut Window, cx: &mut Context<Self>) {
        self.recording = Some(id);
        // slopty: appshots removed.
        let page = cx.entity().downgrade();
        // Bound actions run before Div key listeners. Intercept first so a
        // conflicting chord is recorded/refused instead of running its action.
        self.recording_interceptor = Some(cx.intercept_keystrokes(move |event, window, cx| {
            let _ = page.update(cx, |page, cx| {
                if page.focus.is_focused(window) {
                    page.record_keystroke(&event.keystroke, cx);
                }
            });
        }));
        self.recording_blur = Some(cx.on_blur(&self.focus, window, |this, _, cx| {
            this.stop_recording();
            cx.notify();
        }));
        self.conflict_notice = None;
        window.focus(&self.focus, cx);
        cx.notify();
    }

    fn stop_recording(&mut self) {
        self.recording = None;
        self.recording_blur = None;
        self.recording_interceptor = None;
        // slopty: appshots removed.
    }

    fn commit(&mut self, cx: &mut Context<Self>) {
        cx.emit(ShortcutsEvent::KeymapChanged(self.keymap.clone()));
        cx.notify();
    }

    fn set_escape_stops_active_agent(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if self.escape_stops_active_agent != enabled {
            self.escape_stops_active_agent = enabled;
            cx.emit(ShortcutsEvent::EscapeStopsActiveAgentChanged(enabled));
            cx.notify();
        }
    }

    fn set_composer_send_behavior(
        &mut self,
        behavior: ComposerSendBehavior,
        cx: &mut Context<Self>,
    ) {
        if self.composer_send_behavior != behavior {
            self.composer_send_behavior = behavior;
            self.conflict_notice = None;
            cx.emit(ShortcutsEvent::ComposerSendBehaviorChanged(behavior));
            cx.notify();
        }
    }

    // slopty: dictation and appshots removed.

    fn record_keystroke(&mut self, keystroke: &Keystroke, cx: &mut Context<Self>) {
        let Some(recording) = self.recording else {
            return;
        };
        let mods = &keystroke.modifiers;
        match record_key(
            &keystroke.key,
            mods.control,
            mods.alt,
            mods.shift,
            mods.platform,
        ) {
            RecordOutcome::Cancelled => {
                self.stop_recording();
                cx.notify();
            }
            RecordOutcome::Ignored => {}
            RecordOutcome::Set(combo) => {
                self.stop_recording();
                self.conflict_notice =
                    refusal(&self.keymap, self.composer_send_behavior, recording, &combo);
                if self.conflict_notice.is_none() {
                    self.keymap.set(recording, combo);
                    self.commit(cx);
                }
                cx.notify();
            }
        }
        cx.stop_propagation();
    }

    /// One shortcut row: label left, Reset when customized, and
    /// the click-to-record combo chip (recording inverts it to
    /// white-on-black). `ix` is the id's position in [`ShortcutId::ALL`]
    /// (unique element ids across the group cards); `gx` is the row's place
    /// in its own card (separator rule).
    fn render_row(
        &self,
        id: ShortcutId,
        ix: usize,
        gx: usize,
        recording: Option<ShortcutId>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        widgets::card_row(theme, gx == 0)
            .child(
                div()
                    .flex_1()
                    .min_w(px(160.0))
                    .flex()
                    .flex_col()
                    .child(widgets::row_title(theme, id.label()))
                    .when(
                        matches!(id, ShortcutId::NextSession | ShortcutId::PrevSession),
                        |row| {
                            row.child(widgets::meta_line(
                                theme,
                                vec![
                                    div()
                                        .child("Navigate within the focused pane.")
                                        .into_any_element(),
                                ],
                            ))
                        },
                    ),
            )
            .child(self.render_binding_control(id, ix, recording, theme, cx))
    }

    fn render_binding_control(
        &self,
        id: ShortcutId,
        ix: usize,
        recording: Option<ShortcutId>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let page = cx.entity().downgrade();
        let reset_page = page.clone();
        binding_control(
            id,
            ix,
            self.keymap.get(id),
            recording == Some(id),
            theme,
            move |_, cx| {
                reset_page
                    .update(cx, |this, cx| {
                        this.keymap.reset(id);
                        this.stop_recording();
                        this.commit(cx);
                    })
                    .ok();
            },
            move |window, cx| {
                page.update(cx, |this, cx| this.start_recording(id, window, cx))
                    .ok();
            },
        )
    }

    fn on_scroll_hovered(&mut self, hovered: &bool, _: &mut Window, cx: &mut Context<Self>) {
        if self.scroll.set_list_hovered(*hovered) {
            cx.notify();
        }
    }

    // slopty: dictation and appshots removed.

    /// The shared rail under the id of whichever page is showing.
    fn render_scrollbar(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        // slopty: appshots removed.
        let id = if self.general_page {
            "general-settings-page-scrollbar"
        } else {
            "shortcuts-page-scrollbar"
        };
        popover::rail(self, id, theme, cx)
    }
}

impl popover::ScrollRailHost for ShortcutsPage {
    fn rail_bar(&mut self) -> &mut popover::MenuScrollbarState {
        self.scroll.rail_bar()
    }

    fn rail_scroll(&self) -> Option<gpui::ScrollHandle> {
        self.scroll.rail_scroll()
    }
}

// slopty: dictation and appshots removed.

// slopty: dictation and appshots removed.

// slopty: dictation and appshots removed.

/// Reset (when customized) plus the click-to-record combo chip; recording
/// inverts it to the accent wash. Shared by Settings → Shortcuts and the
/// feature pages that own a shortcut ([`ShortcutField`]).
fn binding_control(
    id: ShortcutId,
    ix: usize,
    combo: &str,
    is_recording: bool,
    theme: &Theme,
    on_reset: impl Fn(&mut Window, &mut gpui::App) + 'static,
    on_record: impl Fn(&mut Window, &mut gpui::App) + 'static,
) -> gpui::Div {
    let accent = theme.accent;
    let combo = combo.to_string();
    let non_default = combo != id.default_combo();
    let chip_text: SharedString = if is_recording {
        "Press keys…".into()
    } else {
        // slopty: the glyphs every other place writes a key in ("⇧⌘A").
        crate::keys::combo_text(&combo)
    };
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(20.0))
        .when(non_default && !is_recording, |el| {
            el.child(
                div()
                    .id(("shortcut-reset", ix))
                    .role(gpui::Role::Button)
                    .aria_label(format!("Reset {} shortcut", id.label()))
                    .min_h(px(24.0))
                    .tab_index(0)
                    .map(|el| crate::surface_chrome::focus_ring(el, theme))
                    .text_size(crate::typography::ui_rems(11.0))
                    .text_color(theme.text_muted)
                    .cursor_pointer()
                    .hover(|s| s.text_color(theme.text))
                    .on_click(move |_, window, cx| on_reset(window, cx))
                    .child(SharedString::from("Reset")),
            )
        })
        .child(
            div()
                .id(("shortcut-combo", ix))
                .role(gpui::Role::Button)
                .aria_label(format!(
                    "Change {} shortcut: {}",
                    id.label(),
                    crate::keys::combo_text(&combo)
                ))
                .tab_index(0)
                .min_w(px(96.0))
                .h(px(widgets::SELECT_HEIGHT))
                .px(px(12.0))
                .rounded(px(8.0))
                .map(|el| crate::surface_chrome::focus_ring(el, theme))
                .flex()
                .items_center()
                .justify_center()
                .font_family(theme.font_mono.clone())
                .text_size(crate::typography::ui_rems(12.0))
                .cursor_pointer()
                .map(|el| {
                    if is_recording {
                        // slopty: recording draws its edge inside the chip, so the chip never grows.
                        el.bg(accent.opacity(0.16))
                            .outline_ring(gpui::Outline {
                                color: accent.opacity(0.55),
                                width: px(1.0),
                                offset: px(-1.0),
                            })
                            .text_color(theme.text)
                    } else {
                        // Same glass wash as the settings dropdowns.
                        let hover_key = format!("shortcut-combo-{ix}-hover");
                        el.bg(crate::motion::hover_blend(
                            &hover_key,
                            widgets::select_fill(theme, false),
                            widgets::select_fill(theme, true),
                        ))
                        .hover_fade(hover_key)
                        .text_color(theme.text)
                    }
                })
                .on_click(move |_, window, cx| on_record(window, cx))
                .child(chip_text),
        )
}

/// Why `combo` cannot be bound to `id`, if it cannot. Conflicts never
/// persist; they're refused at record time, naming the owner (slopty
/// settings.shortcuts.tsx: "… is already assigned to …"). Pure.
fn refusal(
    keymap: &KeymapConfig,
    behavior: ComposerSendBehavior,
    id: ShortcutId,
    combo: &str,
) -> Option<SharedString> {
    // slopty: appshots removed.
    if send_combo_is_reserved(behavior, combo) {
        return Some(format!("{} is reserved for the composer.", crate::keys::combo_text(combo)).into());
    }
    conflict_owner(keymap, id, combo).map(|owner| {
        // slopty: dictation and appshots removed.
        format!(
            "{} is already assigned to {}.",
            crate::keys::combo_text(combo),
            owner.label()
        )
        .into()
    })
}

/// The shortcut (other than `id`) already bound to `combo`, if any. Pure.
pub fn conflict_owner(keymap: &KeymapConfig, id: ShortcutId, combo: &str) -> Option<ShortcutId> {
    ShortcutId::ALL
        .into_iter()
        .find(|&other| other != id && keymap.get(other) == combo) // slopty: appshots removed.
}

/// slopty: compact mode's default: off.
const COMPACT_MODE_DEFAULT: bool = false;

pub fn send_combo_is_reserved(_behavior: ComposerSendBehavior, combo: &str) -> bool {
    combo == "mod-enter"
}

// slopty: the keys as every other key in the app reads ("⌘⏎"; "Ctrl+Enter" off the Mac).
pub fn modifier_send_label(is_macos: bool) -> String {
    crate::keys::combo_text_on(is_macos, "mod-enter")
}

/// The page's sections, in display order. [`group`] is a total match, so every
/// [`ShortcutId::ALL`] entry lands in exactly one — a shortcut added later
/// extends the match and appears on the page by construction
/// (`every_shortcut_lands_in_a_rendered_group` holds the other half: its group
/// name must be listed here).
// slopty: the keys used most lead; the one-key groups fold into one "Other" card at the foot.
const GROUP_ORDER: [&str; 4] = [ // slopty: dictation and appshots removed.
    "Chats", // slopty: one word for them, as everywhere (M17)
    "Panels",
    "Jump to chat",
    "Other",
];

/// The section a shortcut's row renders under.
fn group(id: ShortcutId) -> &'static str {
    match id {
        // slopty: appshots and the wallpaper shuffle removed; one-key groups fold into "Other".
        ShortcutId::SaveFile | ShortcutId::BrowserReload => "Other",
        ShortcutId::ToggleSidebar
        | ShortcutId::ToggleChanges
        | ShortcutId::ToggleFiles
        | ShortcutId::ToggleTerminal
        | ShortcutId::ReopenClosedTab => "Panels", // slopty: the right pane's last closed tab
        // slopty: dictation removed; a new project is how a chat starts somewhere new.
        ShortcutId::NewProject
        | ShortcutId::OpenModelPicker
        | ShortcutId::NewSession
        | ShortcutId::NextSession
        | ShortcutId::PrevSession
        | ShortcutId::ArchiveSession
        | ShortcutId::OpenBranchPicker // slopty: the composer's branch
        | ShortcutId::NextEffort // slopty: and its effort
        | ShortcutId::ResumeSession // slopty: a past Claude Code session
        | ShortcutId::NextNeedsYou => "Chats", // slopty: the next chat waiting on the person
        ShortcutId::JumpSession(_) => "Jump to chat",
    }
}

impl Render for ShortcutsPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement { // slopty: appshots removed.
        // Feature pages (Voice) rebind their shortcut through the store. Read
        // it back so a later commit here never republishes an older keymap.
        if self.recording.is_none()
            && let Some(store) = cx.try_global::<crate::settings::SettingsStore>()
        {
            self.keymap.clone_from(&store.current.keymap);
        }
        // slopty: appshots removed.
        let theme = Theme::of(cx).for_settings_surface();
        let recording = self.recording;
        let escape_stops_active_agent = self.escape_stops_active_agent;
        let send_behavior = self.composer_send_behavior;
        let compact_mode = crate::settings::transcript_compact_mode(cx);
        // slopty: every choice these pages hold counts, each against its default.
        let customized = self.keymap != KeymapConfig::default()
            || escape_stops_active_agent
            || send_behavior != ComposerSendBehavior::default()
            || compact_mode != COMPACT_MODE_DEFAULT;
        let mac = crate::keys::MOD_IS_COMMAND; // slopty: an iPad's ⌘ too
        let send_behaviors = [
            (ComposerSendBehavior::Enter, SharedString::from(crate::keys::combo_text_on(mac, "enter"))),
            (ComposerSendBehavior::ModEnter, SharedString::from(modifier_send_label(mac))),
        ];
        let send_behavior_control = widgets::select(
            "composer-send-behavior",
            "Send messages with",
            &theme,
            |page: &mut Self| &mut page.send_select,
        )
        .options(
            send_behaviors.iter().map(|(behavior, label)| {
                widgets::SelectOption::new(label.clone()).standing(
                    (*behavior == ComposerSendBehavior::default())
                        .then_some(widgets::Standing::Default),
                )
            }),
            send_behaviors
                .iter()
                .position(|(behavior, _)| *behavior == send_behavior)
                .unwrap_or_default(),
        )
        .width(128.0)
        .font_family(theme.font_mono.clone())
        .on_select(move |page, ix, _, cx| page.set_composer_send_behavior(send_behaviors[ix].0, cx))
        .render(&self.send_select, cx);
        // slopty: "Reset" on a row whose choice is not its default (Zed's rule).
        let send_reset = widgets::reset_action(
            &theme,
            "composer-send-behavior-reset",
            "Send messages with",
            send_behavior == ComposerSendBehavior::default(),
        )
        .map(|reset| {
            reset.on_click(cx.listener(|this, _, _, cx| {
                this.set_composer_send_behavior(ComposerSendBehavior::default(), cx);
            }))
        });
        let compact_mode_reset = widgets::reset_action(
            &theme,
            "transcript-compact-mode-reset",
            "Compact mode",
            compact_mode == COMPACT_MODE_DEFAULT,
        )
        .map(|reset| {
            reset.on_click(cx.listener(|_, _, _, cx| {
                crate::settings::set_transcript_compact_mode(COMPACT_MODE_DEFAULT, cx);
                cx.notify();
            }))
        });
        let escape_reset = widgets::reset_action(
            &theme,
            "escape-stops-active-agent-reset",
            "Stop agent with Escape",
            !escape_stops_active_agent,
        )
        .map(|reset| {
            reset.on_click(cx.listener(|this, _, _, cx| {
                this.set_escape_stops_active_agent(false, cx);
            }))
        });

        let send_behavior_row = widgets::card_row(&theme, true)
            .child(
                div()
                    .flex_1()
                    .min_w(px(160.0))
                    .child(widgets::row_title(&theme, "Send messages with")),
            )
            .children(send_reset)
            .child(send_behavior_control);
        let compact_mode_row = widgets::card_row(&theme, false)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(widgets::row_title(&theme, "Compact mode"))
                    .child(widgets::meta_line(
                        &theme,
                        vec![
                            div()
                                .child("Collapse thinking and tools.")
                                .into_any_element(),
                        ],
                    )),
            )
            .children(compact_mode_reset)
            .child(
                widgets::toggle_switch(&theme, compact_mode, "transcript-compact-mode")
                    .id("transcript-compact-mode-toggle")
                    .tab_index(0)
                    .role(gpui::Role::Switch)
                    .aria_label("Compact mode")
                    .aria_toggled(if compact_mode {
                        gpui::Toggled::True
                    } else {
                        gpui::Toggled::False
                    })
                    .map(|el| crate::surface_chrome::focus_ring(el, &theme))
                    .cursor_pointer()
                    .on_click(cx.listener(move |_, _, _, cx| {
                        crate::settings::set_transcript_compact_mode(!compact_mode, cx);
                        cx.notify();
                    })),
            );
        let escape_behavior_row = widgets::card_row(&theme, false)
            .child(
                div()
                    .flex_1()
                    .min_w(px(160.0))
                    .child(widgets::row_title(&theme, "Stop agent with Escape"))
                    .child(widgets::meta_line(
                        &theme,
                        vec![
                            div()
                                .child("When no dialog or menu is open.")
                                .into_any_element(),
                        ],
                    )),
            )
            .children(escape_reset)
            .child(
                widgets::toggle_switch(
                    &theme,
                    escape_stops_active_agent,
                    "escape-stops-active-agent",
                )
                .id("escape-stops-active-agent-toggle")
                .debug_selector(|| "escape-stops-active-agent-toggle".into())
                .tab_index(0)
                .role(gpui::Role::Switch)
                .aria_label("Stop active agent with Escape")
                .aria_toggled(if escape_stops_active_agent {
                    gpui::Toggled::True
                } else {
                    gpui::Toggled::False
                })
                .map(|el| crate::surface_chrome::focus_ring(el, &theme))
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.set_escape_stops_active_agent(!escape_stops_active_agent, cx);
                })),
            );
        if self.general_page {
            // slopty: no pull-request pages, so no choice of where a pull request opens.
            let scrollbar = self.render_scrollbar(&theme, cx);
            return div()
                .id("general-settings-page-host")
                .relative()
                .size_full()
                .on_hover(cx.listener(Self::on_scroll_hovered))
                .child(
                    crate::edge_fade::edge_faded(
                        16.0,
                        true,
                        true,
                        div()
                            .id("general-settings-page")
                            .size_full()
                            .overflow_y_scroll()
                            .track_scroll(&self.scroll.scroll)
                            .child(
                                widgets::page_column()
                                    // slopty: one page head (`widgets::page_head`).
                                    .child(widgets::page_head(
                                        &theme,
                                        "General",
                                        None,
                                        "How messages send and agents stop.",
                                        None,
                                    ))
                                    .child(
                                        widgets::section_card(&theme)
                                            .child(send_behavior_row)
                                            .child(compact_mode_row)
                                            .child(escape_behavior_row),
                                    ), // slopty: no thread naming.
                            ),
                    )
                    .fade_overflow_y(&self.scroll.scroll),
                )
                .children(scrollbar)
                .into_any_element();
        }
        // One block per group, each under its small section label — the flat
        // 16-row table read as one undifferentiated wall. `ix` (the id's
        // position in ALL) keys the interactive elements, so ids stay unique
        // across blocks. The section wrapper owns the spacing, so the block's
        // own top margin is zeroed.
        let mut groups: Vec<gpui::AnyElement> = Vec::new();
        for name in GROUP_ORDER {
            // slopty: dictation and appshots removed.
            let mut card = widgets::section_card(&theme).mt_0();
            let ids = ShortcutId::ALL.into_iter().filter(|&id| group(id) == name);
            for (gx, id) in ids.enumerate() {
                let ix = ShortcutId::ALL.iter().position(|&a| a == id).unwrap_or(0);
                card = card.child(self.render_row(id, ix, gx, recording, &theme, cx));
            }
            groups.push(widgets::section(&theme, name, card).into_any_element());
        }

        // Helper line stays in the muted tone even for a rejected conflict —
        // the message names the specific clash (slopty settings.shortcuts.tsx).
        let helper: SharedString = if recording.is_some() {
            "Press Escape to cancel.".into()
        } else if let Some(notice) = self.conflict_notice.clone() {
            notice
        } else {
            "Shortcuts must be unique.".into()
        };

        let scrollbar = self.render_scrollbar(&theme, cx);
        div()
            .id("shortcuts-page-host")
            .relative()
            .size_full()
            .on_hover(cx.listener(Self::on_scroll_hovered))
            .child(
                crate::edge_fade::edge_faded(16.0, true, true, div()
                    .id("shortcuts-page")
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll.scroll)
                    .track_focus(&self.focus)
                    .child(
                        widgets::page_column()
                            // slopty: one page head (`widgets::page_head`), titled as the nav
                            // names the page (`SettingsSection::label`).
                            .child(widgets::page_head(
                                &theme,
                                crate::shell::SettingsSection::Shortcuts.label(),
                                None,
                                "Click a binding, then press the new key combination.",
                                Some(
                                    {
                                        // slopty: dimmed when nothing differs or while recording.
                                        let enabled = customized && recording.is_none();
                                        widgets::restore_defaults(
                                            &theme,
                                            "shortcuts-restore-defaults",
                                            enabled,
                                        )
                                        .when(enabled, |el| {
                                            el.on_click(cx.listener(|this, _, _, cx| {
                                                this.keymap = KeymapConfig::default();
                                                this.stop_recording();
                                                this.conflict_notice = None;
                                                this.commit(cx);
                                                this.set_escape_stops_active_agent(false, cx);
                                                this.set_composer_send_behavior(
                                                    ComposerSendBehavior::default(),
                                                    cx,
                                                );
                                                crate::settings::set_transcript_compact_mode(
                                                    COMPACT_MODE_DEFAULT,
                                                    cx,
                                                );
                                            }))
                                        })
                                    }
                                    .into_any_element(),
                                ),
                            ))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .children(groups),
                            )
                            .child(
                                div()
                                    .mt(px(12.0))
                                    .px(px(4.0))
                                    .min_h(px(20.0))
                                    .flex()
                                    .justify_center()
                                    .text_size(crate::typography::ui_rems(12.0))
                                    .text_color(theme.text_muted)
                                    .child(helper),
                            )
                            ,
                    )).fade_overflow_y(&self.scroll.scroll),
            )
            .children(scrollbar)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // slopty: dictation and appshots removed.

    #[gpui::test]
    fn conversation_controls_work_after_moving_out_of_shortcuts(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
        });
        let (page, cx) = cx.add_window_view(|_, cx| {
            let state = cx.new(|_| AppState::new());
            let mut page = ShortcutsPage::new(
                state,
                KeymapConfig::default(),
                false,
                ComposerSendBehavior::Enter,
                // slopty: appshots removed.
                cx,
            );
            page.show_section(true);
            page
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let send = cx.debug_bounds("composer-send-behavior").unwrap();
        cx.simulate_click(send.center(), gpui::Modifiers::default());
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let option = cx.debug_bounds("composer-send-behavior-option-1").unwrap();
        cx.simulate_click(option.center(), gpui::Modifiers::default());
        page.update(cx, |page, _| assert!(!page.send_select.is_open()));
        // Let the menu's exit animation finish before clicking beneath it:
        // the reap compares wall-clock instants, so real time must pass too.
        std::thread::sleep(
            crate::motion::MENU_OUT
                .total()
                .mul_f32(crate::motion::speed_scale()),
        );
        cx.executor()
            .advance_clock(std::time::Duration::from_secs(1));
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(cx.debug_bounds("composer-send-behavior-option-1").is_none());
        let escape = cx.debug_bounds("escape-stops-active-agent-toggle").unwrap();
        cx.simulate_click(escape.center(), gpui::Modifiers::default());
        page.update(cx, |page, _| {
            assert_eq!(page.composer_send_behavior, ComposerSendBehavior::ModEnter);
            assert!(page.escape_stops_active_agent);
            page.show_section(false); // slopty: appshots removed.
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(cx.debug_bounds("composer-send-behavior").is_none());
        assert!(
            cx.debug_bounds("escape-stops-active-agent-toggle")
                .is_none()
        );
    }

    // slopty: dictation and appshots removed.

    #[gpui::test]
    fn recorder_refuses_bound_actions_before_they_can_run(cx: &mut gpui::TestAppContext) {
        use std::{cell::Cell, rc::Rc};
        let fired = Rc::new(Cell::new(false));
        let observed = fired.clone();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            cx.bind_keys([gpui::KeyBinding::new(
                &crate::settings::platform_combo("mod-n"),
                crate::shell::NewSession,
                None,
            )]);
            cx.on_action(move |_: &crate::shell::NewSession, _| observed.set(true));
        });
        let window = cx.add_window(|_, cx| {
            let state = cx.new(|_| AppState::new());
            ShortcutsPage::new(
                state,
                KeymapConfig::default(),
                false,
                ComposerSendBehavior::default(),
                // slopty: appshots removed.
                cx,
            )
        });
        window
            .update(cx, |page, window, cx| {
                page.start_recording(ShortcutId::SaveFile, window, cx) // slopty: appshots removed.
            })
            .unwrap();
        cx.simulate_keystrokes(window.into(), &crate::settings::platform_combo("mod-n"));
        window
            .update(cx, |page, _, _| {
                assert!(!fired.get(), "the existing action ran while recording");
                assert!(
                    page.conflict_notice
                        .as_deref()
                        .unwrap()
                        .contains("New chat")
                );
                assert_eq!(
                    page.keymap.save_file, // slopty: appshots removed.
                    ShortcutId::SaveFile.default_combo()
                );
                assert!(page.recording.is_none());
                assert!(page.recording_interceptor.is_none());
            })
            .unwrap();
        cx.simulate_keystrokes(window.into(), &crate::settings::platform_combo("mod-n"));
        assert!(
            fired.get(),
            "finishing recording must restore normal actions"
        );
    }

    #[test]
    fn recording_outcomes() {
        assert_eq!(
            record_key("escape", false, false, false, false),
            RecordOutcome::Cancelled
        );
        assert_eq!(
            record_key("Escape", true, false, false, false),
            RecordOutcome::Cancelled
        );
        assert_eq!(
            record_key("s", false, false, false, true),
            RecordOutcome::Set("mod-s".into())
        );
        assert_eq!(
            record_key("k", false, true, true, true),
            RecordOutcome::Set("mod-alt-shift-k".into())
        );
        // Apple-only: elsewhere ctrl IS the primary and records as "mod".
        #[cfg(target_vendor = "apple")] // slopty: an iPad's ⌘ too
        assert_eq!(
            record_key("tab", true, false, true, false),
            RecordOutcome::Set("ctrl-shift-tab".into())
        );
        // Bare modifiers stay recording.
        assert_eq!(
            record_key("shift", false, false, true, false),
            RecordOutcome::Ignored
        );
        assert_eq!(
            record_key("ctrl", true, false, false, false),
            RecordOutcome::Ignored
        );
    }

    #[test]
    fn every_shortcut_lands_in_a_rendered_group() {
        // The page renders GROUP_ORDER's cards and nothing else — a group()
        // arm returning a name missing from GROUP_ORDER would silently drop
        // its rows from Settings.
        for id in ShortcutId::ALL {
            assert!(
                GROUP_ORDER.contains(&group(id)),
                "{:?} is grouped under {:?}, which GROUP_ORDER does not render",
                id,
                group(id)
            );
        }
        // And every named group has at least one row — no empty cards.
        for name in GROUP_ORDER {
            assert!(
                ShortcutId::ALL.into_iter().any(|id| group(id) == name),
                "group {:?} would render an empty card",
                name
            );
        }
    }

    // slopty: the page leads with the keys used most, and no card holds a lone key but the
    // "Other" card at its foot.
    #[test]
    fn the_page_leads_with_chats_and_gathers_lone_keys_at_the_foot() {
        assert_eq!(GROUP_ORDER[0], "Chats");
        assert_eq!(GROUP_ORDER.last(), Some(&"Other"));
        assert_eq!(group(ShortcutId::NewProject), "Chats", "a new project starts a chat");
        for name in &GROUP_ORDER[..GROUP_ORDER.len() - 1] {
            let rows = ShortcutId::ALL.into_iter().filter(|&id| group(id) == *name).count();
            assert!(rows > 1, "{name} holds a lone key: fold it into Other");
        }
    }

    #[test]
    fn conflicting_records_are_refused() {
        // slopty parity: a combo bound elsewhere is refused at record time (the
        // helper names the owner) — conflicts never persist into the keymap.
        let keymap = KeymapConfig::default();
        let RecordOutcome::Set(combo) = record_key("r", false, false, false, true) else {
            panic!("expected Set");
        };
        assert_eq!(
            conflict_owner(&keymap, ShortcutId::ToggleSidebar, &combo),
            Some(ShortcutId::ToggleChanges)
        );
        // Re-recording a shortcut's own combo is not a conflict.
        assert_eq!(
            conflict_owner(&keymap, ShortcutId::ToggleChanges, &combo),
            None
        );
        // A free combo conflicts with nothing.
        assert_eq!(
            conflict_owner(&keymap, ShortcutId::ToggleSidebar, "mod-shift-x"),
            None
        );
    }

    // slopty: dictation and appshots removed.

    #[test]
    fn modifier_send_labels_are_platform_specific() {
        assert_eq!(modifier_send_label(true), "⌘⏎");
        assert_eq!(modifier_send_label(false), "Ctrl+Enter");
    }

    #[test]
    fn the_general_rows_reset_to_the_settings_defaults() {
        let defaults = crate::settings::UiSettings::default();
        assert_eq!(defaults.transcript_compact_mode, COMPACT_MODE_DEFAULT);
    }

    #[test]
    fn modifier_send_is_always_reserved_for_the_composer() {
        assert!(send_combo_is_reserved(
            ComposerSendBehavior::Enter,
            "mod-enter"
        ));
        assert!(send_combo_is_reserved(
            ComposerSendBehavior::ModEnter,
            "mod-enter"
        ));
        assert!(!send_combo_is_reserved(
            ComposerSendBehavior::ModEnter,
            "mod-shift-enter"
        ));
    }
}
