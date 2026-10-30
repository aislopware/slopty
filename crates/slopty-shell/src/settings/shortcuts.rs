// Derived from zeron (MIT, github.com/zeronsh/zeron @ eabad36), see THIRD_PARTY_NOTICES.md
//! Settings → Shortcuts: a table of the rebindable
//! bindings — click a combo to record (Esc cancels), live conflict detection,
//! per-row Reset and Restore defaults. Changes emit [`ShortcutsEvent`]; the
//! shell persists them and re-applies the app keymap.

use gpui::{
    Context, Entity, EventEmitter, FocusHandle, Keystroke, SharedString, Window, div, prelude::*,
    px,
};

use crate::popover;

use crate::settings::widgets;
use crate::settings::{
    KeyEquivalents, KeymapConfig, ShortcutId, canonical_key, combo_from_keystroke,
    key_equivalents,
};
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

/// `key` is what the layout reported; `equivalents` that layout's
/// [`KeyEquivalents`]. The combo stores the US key behind it, the spelling
/// defaults use (AZERTY ⌘& records as "mod-1"), so stored-combo conflict
/// checks keep seeing one chord per key. A key no stored combo can reach is
/// unusable.
pub fn record_key(
    key: &str,
    ctrl: bool,
    alt: bool,
    shift: bool,
    cmd: bool,
    equivalents: &KeyEquivalents,
) -> RecordOutcome {
    if key.eq_ignore_ascii_case("escape") {
        return RecordOutcome::Cancelled;
    }
    let Some(key) = canonical_key(key, equivalents) else {
        return RecordOutcome::Ignored;
    };
    match combo_from_keystroke(ctrl, alt, shift, cmd, &key) {
        Some(combo) => RecordOutcome::Set(combo),
        None => RecordOutcome::Ignored,
    }
}

#[derive(Debug, Clone)]
pub enum ShortcutsEvent {
    /// The keymap changed — persist + re-apply.
    KeymapChanged(KeymapConfig),
}

pub struct ShortcutsPage {
    scroll: crate::settings::widgets::PageScroll,
    /// Working copy (kept in sync with the shell via change events).
    keymap: KeymapConfig,
    recording: Option<ShortcutId>,
    recording_blur: Option<gpui::Subscription>,
    recording_interceptor: Option<gpui::Subscription>,
    /// A rejected record attempt ("{Combo} is already assigned to {label}.") —
    /// conflicts never persist; they're refused at record time.
    conflict_notice: Option<SharedString>,
    focus: FocusHandle,
    _state: Entity<AppState>,
}

impl EventEmitter<ShortcutsEvent> for ShortcutsPage {}

impl ShortcutsPage {
    pub fn new(
        state: Entity<AppState>,
        keymap: KeymapConfig,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            scroll: crate::settings::widgets::PageScroll::default(),
            keymap,
            recording: None,
            recording_blur: None,
            recording_interceptor: None,
            conflict_notice: None,
            focus: cx.focus_handle(),
            _state: state,
        }
    }


    fn start_recording(&mut self, id: ShortcutId, window: &mut Window, cx: &mut Context<Self>) {
        self.recording = Some(id);
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
    }

    fn commit(&mut self, cx: &mut Context<Self>) {
        cx.emit(ShortcutsEvent::KeymapChanged(self.keymap.clone()));
        cx.notify();
    }


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
            &key_equivalents(cx),
        ) {
            RecordOutcome::Cancelled => {
                self.stop_recording();
                cx.notify();
            }
            RecordOutcome::Ignored => {}
            RecordOutcome::Set(combo) => {
                self.stop_recording();
                self.conflict_notice = refusal(&self.keymap, recording, &combo);
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


    /// The shared rail under the id of whichever page is showing.
    fn render_scrollbar(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        popover::rail(self, "shortcuts-page-scrollbar", theme, cx)
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
        // The glyphs every other place writes a key in ("⇧⌘A").
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
                        // Recording draws its edge inside the chip, so the chip never grows.
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
/// persist; they're refused at record time, naming the owner ("… is already
/// assigned to …"). Pure.
fn refusal(keymap: &KeymapConfig, id: ShortcutId, combo: &str) -> Option<SharedString> {
    conflict_owner(keymap, id, combo).map(|owner| {
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
        .find(|&other| other != id && keymap.get(other) == combo)
}

/// The page's sections, in display order. [`group`] is a total match, so every
/// [`ShortcutId::ALL`] entry lands in exactly one — a shortcut added later
/// extends the match and appears on the page by construction
/// (`every_shortcut_lands_in_a_rendered_group` holds the other half: its group
/// name must be listed here).
// The keys used most lead; the one-key groups fold into one "Other" card at the foot.
const GROUP_ORDER: [&str; 4] = [
    "Sessions", // one word for them, as everywhere
    "Views",
    "Jump to session",
    "Other",
];

/// The section a shortcut's row renders under.
fn group(id: ShortcutId) -> &'static str {
    match id {
        ShortcutId::SaveFile | ShortcutId::BrowserReload => "Other",
        ShortcutId::ToggleSidebar
        | ShortcutId::ToggleChanges
        | ShortcutId::ToggleFiles
        | ShortcutId::ReopenClosedTab => "Views", // the sidebar and the views beside a session
        // A new project is how a session starts somewhere new.
        ShortcutId::NewProject
        | ShortcutId::NewSession
        | ShortcutId::NextSession
        | ShortcutId::PrevSession
        | ShortcutId::ArchiveSession
        | ShortcutId::NextNeedsYou // the next one waiting on the person
        | ShortcutId::Inbox => "Sessions", // everything waiting, behind the eye
        ShortcutId::JumpSession(_) => "Jump to session",
    }
}

impl Render for ShortcutsPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Feature pages (Voice) rebind their shortcut through the store. Read
        // it back so a later commit here never republishes an older keymap.
        if self.recording.is_none()
            && let Some(store) = cx.try_global::<crate::settings::SettingsStore>()
        {
            self.keymap.clone_from(&store.current.keymap);
        }
        let theme = Theme::of(cx).for_settings_surface();
        let recording = self.recording;
        let customized = self.keymap != KeymapConfig::default();
        // One block per group, each under its small section label — the flat
        // 16-row table read as one undifferentiated wall. `ix` (the id's
        // position in ALL) keys the interactive elements, so ids stay unique
        // across blocks. The section wrapper owns the spacing, so the block's
        // own top margin is zeroed.
        let mut groups: Vec<gpui::AnyElement> = Vec::new();
        for name in GROUP_ORDER {
            let mut card = widgets::section_card(&theme).mt_0();
            let ids = ShortcutId::ALL.into_iter().filter(|&id| group(id) == name);
            for (gx, id) in ids.enumerate() {
                let ix = ShortcutId::ALL.iter().position(|&a| a == id).unwrap_or(0);
                card = card.child(self.render_row(id, ix, gx, recording, &theme, cx));
            }
            groups.push(widgets::section(&theme, name, card).into_any_element());
        }

        // Helper line stays in the muted tone even for a rejected conflict —
        // the message names the specific clash.
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
                            // One page head (`widgets::page_head`), titled as the nav
                            // names the page (`SettingsSection::label`).
                            .child(widgets::page_head(
                                &theme,
                                crate::shell::SettingsSection::Shortcuts.label(),
                                None,
                                "Click a binding, then press the new key combination.",
                                Some(
                                    {
                                        // Dimmed when nothing differs or while recording.
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



    #[gpui::test]
    fn recorder_refuses_bound_actions_before_they_can_run(cx: &mut gpui::TestAppContext) {
        use std::{cell::Cell, rc::Rc};
        let fired = Rc::new(Cell::new(false));
        let observed = fired.clone();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            cx.bind_keys([crate::keys::binding(
                &crate::settings::platform_combo("mod-n"),
                crate::shell::NewSession,
                None,
                cx,
            )]);
            cx.on_action(move |_: &crate::shell::NewSession, _| observed.set(true));
        });
        let window = cx.add_window(|_, cx| {
            let state = cx.new(|_| AppState::new());
            ShortcutsPage::new(
                state,
                KeymapConfig::default(),
                cx,
            )
        });
        window
            .update(cx, |page, window, cx| {
                page.start_recording(ShortcutId::SaveFile, window, cx)
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
                        .contains("New session")
                );
                assert_eq!(
                    page.keymap.save_file,
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
        let us = KeyEquivalents::default();
        assert_eq!(
            record_key("escape", false, false, false, false, &us),
            RecordOutcome::Cancelled
        );
        assert_eq!(
            record_key("Escape", true, false, false, false, &us),
            RecordOutcome::Cancelled
        );
        assert_eq!(
            record_key("s", false, false, false, true, &us),
            RecordOutcome::Set("mod-s".into())
        );
        assert_eq!(
            record_key("k", false, true, true, true, &us),
            RecordOutcome::Set("mod-alt-shift-k".into())
        );
        // Apple-only: elsewhere ctrl IS the primary and records as "mod".
        #[cfg(target_vendor = "apple")] // an iPad's ⌘ too
        assert_eq!(
            record_key("tab", true, false, true, false, &us),
            RecordOutcome::Set("ctrl-shift-tab".into())
        );
        // Bare modifiers stay recording.
        assert_eq!(
            record_key("shift", false, false, true, false, &us),
            RecordOutcome::Ignored
        );
        assert_eq!(
            record_key("ctrl", true, false, false, false, &us),
            RecordOutcome::Ignored
        );
    }

    #[test]
    fn azerty_records_the_us_key_behind_each_chord() {
        let azerty = crate::settings::azerty_key_equivalents();
        // ⌘& is AZERTY's ⌘1: it records as the default's own spelling, so
        // the conflict check sees Jump to session 1 holding it.
        let RecordOutcome::Set(combo) = record_key("&", false, false, false, true, &azerty) else {
            panic!("expected Set");
        };
        assert_eq!(combo, "mod-1");
        assert_eq!(
            conflict_owner(&KeymapConfig::default(), ShortcutId::NewSession, &combo),
            Some(ShortcutId::JumpSession(0))
        );
        assert_eq!(
            record_key(":", false, false, false, true, &azerty),
            RecordOutcome::Set("mod-/".into())
        );
        // ⌘⇧è types 7, where AZERTY puts US ⌘&.
        assert_eq!(
            record_key("7", false, false, false, true, &azerty),
            RecordOutcome::Set("mod-&".into())
        );
        // Letters and keys AZERTY keeps in place record as typed.
        assert_eq!(
            record_key("é", false, false, false, true, &azerty),
            RecordOutcome::Set("mod-2".into())
        );
        assert_eq!(
            record_key("b", false, false, false, true, &azerty),
            RecordOutcome::Set("mod-b".into())
        );
        // No US key lands on AZERTY's @ key: nothing stored could reach it.
        assert_eq!(
            record_key("@", false, false, false, true, &azerty),
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

    // The page leads with the keys used most, and no card holds a lone key but the
    // "Other" card at its foot.
    #[test]
    fn the_page_leads_with_sessions_and_gathers_lone_keys_at_the_foot() {
        assert_eq!(GROUP_ORDER[0], "Sessions");
        assert_eq!(GROUP_ORDER.last(), Some(&"Other"));
        assert_eq!(group(ShortcutId::NewProject), "Sessions", "a new project starts a session");
        for name in &GROUP_ORDER[..GROUP_ORDER.len() - 1] {
            let rows = ShortcutId::ALL.into_iter().filter(|&id| group(id) == *name).count();
            assert!(rows > 1, "{name} holds a lone key: fold it into Other");
        }
    }

    #[test]
    fn conflicting_records_are_refused() {
        // A combo bound elsewhere is refused at record time (the
        // helper names the owner) — conflicts never persist into the keymap.
        let keymap = KeymapConfig::default();
        let RecordOutcome::Set(combo) =
            record_key("r", false, false, false, true, &KeyEquivalents::default())
        else {
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


}
