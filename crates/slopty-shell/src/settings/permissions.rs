// slopty: Slopty's own page (not in zeron).
//! Settings → Permissions: an agent's permission rules for a folder on a machine, as the agent
//! reads them (`slopty.WatchRules`): what it may do without asking, what it always asks, what
//! it may never do, each with where it comes from; a rule removed or added in one of the scopes
//! the agent writes (`slopty.ChangeRule`). The agent's own settings files hold them; Slopty
//! writes only through the worker, which writes nothing else.

use gpui::{
    AnyElement, App, Context, Entity, SharedString, Subscription, Task, Window, div, prelude::*, px,
};

use slopty_view_doc::calls;
use slopty_view_doc::rules::{PermissionRuleRow, PermissionRules};

use crate::composer::ComposerInput;
use crate::popover;
use crate::settings::widgets;
use crate::state::AppState;
use crate::theme::Theme;

/// The behaviours a rule has, in the order the page lists them, with their section's words.
pub const BEHAVIORS: [(&str, &str, &str); 3] = [
    ("allow", "Allowed", "Runs without asking."),
    ("ask", "Asks first", "Always asks, even where something else would allow it."),
    ("deny", "Denied", "Never runs."),
];

/// A place whose rules the page shows: a folder on a machine, or a machine's newest thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Place {
    pub device_id: String,
    pub cwd: Option<String>,
    pub label: String,
}

/// The places to pick from: every folder the person works in, then each machine as a whole.
pub fn places(state: &AppState) -> Vec<Place> {
    let device_name = |id: &str| {
        state.devices.iter().find(|d| d.id == id).map_or_else(|| id.to_owned(), |d| d.name.clone())
    };
    let mut places: Vec<Place> = state
        .spaces
        .iter()
        .map(|space| Place {
            device_id: space.device_id.clone(),
            cwd: Some(space.path.clone()),
            label: format!("{} · {}", space.display_name(), device_name(&space.device_id)),
        })
        .collect();
    places.extend(state.devices.iter().map(|device| Place {
        device_id: device.id.clone(),
        cwd: None,
        label: format!("{} · any folder", device.name),
    }));
    places
}

/// How long the page waits before it subscribes again after its `attempt`th failure in a row:
/// 1 s, doubling, at most 15 s, as the Agents page's watch does.
pub fn retry_after(attempt: u32) -> std::time::Duration {
    std::time::Duration::from_secs(1_u64.checked_shl(attempt).unwrap_or(u64::MAX).min(15))
}

/// The updates of the rules that did not read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Skipped {
    /// How many.
    pub count: u32,
    /// Why the last did not.
    pub last: Option<String>,
}

impl Skipped {
    fn note(&mut self, why: String) {
        self.count = self.count.saturating_add(1);
        self.last = Some(why);
    }

    /// What the page says of them; `None` while there are none.
    pub fn words(&self) -> Option<String> {
        let last = self.last.as_deref()?;
        let what = if self.count == 1 { "1 update".to_owned() } else { format!("{} updates", self.count) };
        Some(format!("Couldn't read {what} of the rules, so they are not shown: {last}"))
    }
}

/// The scope a rule from `source` lives in, as `slopty.ChangeRule` names it: the scope whose id
/// begins the source (`localSettings` is `local`), else the source itself.
pub fn scope_of(rules: &PermissionRules, source: &str) -> String {
    rules
        .scopes
        .iter()
        .find(|scope| source.starts_with(scope.id.as_str()))
        .map_or_else(|| source.to_owned(), |scope| scope.id.clone())
}

pub struct PermissionsPage {
    state: Entity<AppState>,
    scroll: widgets::PageScroll,
    place_select: widgets::SelectState,
    behavior_select: widgets::SelectState,
    scope_select: widgets::SelectState,
    /// The place shown, by its index in [`places`].
    place: Option<Place>,
    rules: Option<PermissionRules>,
    watch: Option<Task<()>>,
    rule_input: Entity<ComposerInput>,
    new_behavior: usize,
    new_scope: usize,
    /// A change under way, by its rule.
    busy: Option<String>,
    error: Option<SharedString>,
    /// Why the rules cannot be read now, while the page asks again ([`retry_after`]).
    unreachable: Option<SharedString>,
    /// Updates of the rules that did not read, since the place was shown, and the last one's
    /// reason: said under the rules, never dropped silently.
    skipped: Skipped,
    _observe: Subscription,
}

impl PermissionsPage {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let observe = cx.observe(&state, |_, _, cx| cx.notify());
        let rule_input = cx.new(|cx| {
            let mut input = ComposerInput::new("Bash(cargo test:*)", cx).with_single_line();
            input.mono = true;
            input
        });
        let mut page = Self {
            state,
            scroll: widgets::PageScroll::default(),
            place_select: widgets::SelectState::default(),
            behavior_select: widgets::SelectState::default(),
            scope_select: widgets::SelectState::default(),
            place: None,
            rules: None,
            watch: None,
            rule_input,
            new_behavior: 0,
            new_scope: 0,
            busy: None,
            error: None,
            unreachable: None,
            skipped: Skipped::default(),
            _observe: observe,
        };
        let first = page.default_place(cx);
        if let Some(place) = first {
            page.show(place, cx);
        }
        page
    }

    /// The selected chat's folder, else the first place.
    fn default_place(&self, cx: &App) -> Option<Place> {
        let state = self.state.read(cx);
        let places = places(state);
        let space = state.selected_space.as_deref().and_then(|id| state.spaces.iter().find(|s| s.id == id));
        space
            .and_then(|space| {
                places
                    .iter()
                    .find(|p| p.device_id == space.device_id && p.cwd.as_deref() == Some(space.path.as_str()))
                    .cloned()
            })
            .or_else(|| places.into_iter().next())
    }

    /// Watch `place`'s rules: subscribed again with a growing wait ([`retry_after`]) while the
    /// subscribe fails or the stream ends, and every update that does not read counted and said.
    fn show(&mut self, place: Place, cx: &mut Context<Self>) {
        self.rules = None;
        self.error = None;
        self.unreachable = None;
        self.skipped = Skipped::default();
        self.place = Some(place.clone());
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.watch = None;
            return;
        };
        let mut params = serde_json::json!({ "deviceId": place.device_id });
        if let Some(cwd) = &place.cwd {
            params["cwd"] = serde_json::Value::String(cwd.clone());
        }
        self.watch = Some(cx.spawn(async move |this, cx| {
            let mut attempt = 0_u32;
            loop {
                let why: SharedString =
                    match engine.client().subscribe_checked(calls::WATCH_RULES, params.clone()).await {
                        Ok(mut rx) => {
                            while let Some(value) = rx.recv().await {
                                attempt = 0;
                                let read = serde_json::from_value::<PermissionRules>(value);
                                let gone = this
                                    .update(cx, |page, cx| {
                                        page.unreachable = None;
                                        match read {
                                            Ok(rules) => page.rules = Some(rules),
                                            Err(err) => page.skipped.note(err.to_string()),
                                        }
                                        cx.notify();
                                    })
                                    .is_err();
                                if gone {
                                    return;
                                }
                            }
                            "The machine stopped saying its rules".into()
                        }
                        Err(err) => format!("Couldn't read the rules: {err}").into(),
                    };
                let wait = retry_after(attempt);
                attempt = attempt.saturating_add(1);
                let words: SharedString = format!("{why}. Trying again in {}s.", wait.as_secs()).into();
                if this
                    .update(cx, |page, cx| {
                        page.unreachable = Some(words);
                        cx.notify();
                    })
                    .is_err()
                {
                    return;
                }
                cx.background_executor().timer(wait).await;
            }
        }));
        cx.notify();
    }

    /// Add or remove a rule (`slopty.ChangeRule`).
    fn change(&mut self, add: bool, behavior: String, rule: String, scope: String, cx: &mut Context<Self>) {
        let (Some(rules), Some(engine)) = (self.rules.as_ref(), self.state.read(cx).engine().cloned()) else {
            return;
        };
        if rule.trim().is_empty() {
            self.error = Some("Write the rule to add.".into());
            cx.notify();
            return;
        }
        let params = serde_json::json!({
            "chatId": rules.chat_id, "add": add, "behavior": behavior, "rule": rule, "scope": scope,
        });
        self.busy = Some(rule);
        self.error = None;
        cx.spawn(async move |this, cx| {
            let result = engine.client().call(calls::CHANGE_RULE, params).await;
            this.update(cx, |page, cx| {
                page.busy = None;
                match result {
                    Ok(_) if add => page.rule_input.update(cx, |input, cx| input.set_text("", cx)),
                    Ok(_) => {}
                    Err(err) => page.error = Some(format!("Couldn't change the rule: {err}").into()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn add_rule(&mut self, cx: &mut Context<Self>) {
        let Some(rules) = self.rules.as_ref() else { return };
        let Some(scope) = rules.scopes.get(self.new_scope).map(|s| s.id.clone()) else { return };
        let behavior = BEHAVIORS[self.new_behavior.min(BEHAVIORS.len() - 1)].0.to_owned();
        let rule = self.rule_input.read(cx).text().trim().to_owned();
        self.change(true, behavior, rule, scope, cx);
    }

    fn on_scroll_hovered(&mut self, hovered: &bool, _: &mut Window, cx: &mut Context<Self>) {
        if self.scroll.set_list_hovered(*hovered) {
            cx.notify();
        }
    }

    fn rule_row(&self, ix: usize, row: &PermissionRuleRow, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let busy = self.busy.as_deref() == Some(row.rule.as_str());
        let scope = self.rules.as_ref().map(|rules| scope_of(rules, &row.source)).unwrap_or_default();
        let (behavior, rule) = (row.behavior.clone(), row.rule.clone());
        // slopty: the subtitle says where the rule comes from, never the rule again in other
        // words; the agent's own reading of it is the title's tooltip (lane E's refusal), and a
        // reading that is only the rule again is said nowhere.
        let description = row.description.clone().filter(|d| !d.trim().is_empty() && d.trim() != row.rule.trim());
        let mut meta = vec![SharedString::from(row.source_label.clone())];
        if !row.in_effect {
            meta.push("Overridden".into());
        }
        let words = match &description {
            Some(description) => format!("{}: {}, {description}", row.rule, meta.join(", ")),
            None => format!("{}: {}", row.rule, meta.join(", ")),
        };
        widgets::card_row(theme, ix == 0)
            .id(SharedString::from(format!("permission-rule-{}-{ix}", row.behavior)))
            .role(gpui::Role::ListItem)
            .aria_label(SharedString::from(words))
            .when(!row.in_effect, |el| el.opacity(0.6))
            .child(
                div()
                    .flex_1()
                    .min_w(px(160.0))
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .child(
                        div()
                            .id(SharedString::from(format!("permission-rule-title-{}-{ix}", row.behavior)))
                            .min_w_0()
                            .when_some(description, |title, description| {
                                title
                                    .tooltip(widgets::text_tooltip(description))
                                    .tooltip_show_delay(widgets::TOOLTIP_DELAY)
                            })
                            .child(widgets::row_title(theme, row.rule.clone()).font_family(theme.font_mono.clone())),
                    )
                    .child(
                        div()
                            .text_size(crate::typography::ui_rems(12.0))
                            .text_color(theme.text_muted)
                            .child(SharedString::from(meta.iter().map(SharedString::as_ref).collect::<Vec<_>>().join(" · "))),
                    ),
            )
            .when(row.editable, |el| {
                el.child(
                    widgets::ghost_action(theme)
                        .id(SharedString::from(format!("permission-remove-{}-{ix}", row.behavior)))
                        .role(gpui::Role::Button)
                        .aria_label(SharedString::from(format!("Remove {}", row.rule)))
                        .tab_index(0)
                        .when(busy, |el| el.opacity(crate::theme::DISABLED_OPACITY).cursor_default())
                        .focus_visible(|s| s.border_2().border_color(theme.accent))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.change(false, behavior.clone(), rule.clone(), scope.clone(), cx);
                        }))
                        .child(if busy { "Removing…" } else { "Remove" }),
                )
            })
            .into_any_element()
    }

    fn add_row(&self, rules: &PermissionRules, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let behavior = widgets::select("permission-new-behavior", "Behaviour", theme, |page: &mut Self| {
            &mut page.behavior_select
        })
        .options(BEHAVIORS.iter().map(|(_, label, _)| widgets::SelectOption::new(*label)), self.new_behavior)
        .on_select(|page, ix, _, cx| {
            page.new_behavior = ix;
            cx.notify();
        })
        .render(&self.behavior_select, cx);
        let scope = widgets::select("permission-new-scope", "Where it goes", theme, |page: &mut Self| {
            &mut page.scope_select
        })
        .options(
            rules.scopes.iter().map(|s| widgets::SelectOption::new(s.label.clone()).detail(s.file.clone())),
            self.new_scope.min(rules.scopes.len().saturating_sub(1)),
        )
        .on_select(|page, ix, _, cx| {
            page.new_scope = ix;
            cx.notify();
        })
        .render(&self.scope_select, cx);
        widgets::section(
            theme,
            "Add a rule",
            widgets::section_card(theme).mt(px(0.0)).child(
                widgets::card_row(theme, true)
                    .id("permission-add")
                    .role(gpui::Role::Group)
                    .aria_label("Add a rule")
                    .child(behavior)
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(180.0))
                            .h(px(30.0))
                            .px(px(10.0))
                            .flex()
                            .items_center()
                            .rounded(px(8.0))
                            .border_1()
                            .border_color(crate::theme::hairline(0.1))
                            .bg(crate::theme::ink(0.03))
                            .child(self.rule_input.clone()),
                    )
                    .when(!rules.scopes.is_empty(), |el| el.child(scope))
                    .child(
                        widgets::action_button_when(
                            theme,
                            widgets::ActionTone::Filled,
                            !rules.scopes.is_empty(),
                        )
                        .id("permission-add-rule")
                        .role(gpui::Role::Button)
                        .aria_label("Add rule")
                        .aria_disabled(rules.scopes.is_empty())
                        .tab_index(0)
                        .focus_visible(|s| s.border_2().border_color(theme.accent))
                        .on_click(cx.listener(|this, _, _, cx| this.add_rule(cx)))
                        .child("Add"),
                    ),
            ),
        )
        .into_any_element()
    }
}

impl popover::ScrollRailHost for PermissionsPage {
    fn rail_bar(&mut self) -> &mut popover::MenuScrollbarState {
        self.scroll.rail_bar()
    }

    fn rail_scroll(&self) -> Option<gpui::ScrollHandle> {
        self.scroll.rail_scroll()
    }
}

impl Render for PermissionsPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::toast::take_error(&mut self.error, cx);
        let theme = Theme::of(cx).for_settings_surface();
        let places = places(self.state.read(cx));
        if self.place.is_none()
            && let Some(first) = self.default_place(cx)
        {
            self.show(first, cx);
        }
        let selected = self.place.as_ref().and_then(|p| places.iter().position(|q| q == p)).unwrap_or(0);
        let picker = (!places.is_empty()).then(|| {
            let choices = places.clone();
            widgets::select("permission-place", "Folder", &theme, |page: &mut Self| &mut page.place_select)
                .options(places.iter().map(|p| widgets::SelectOption::new(p.label.clone())), selected)
                .menu_width(320.0)
                .on_select(move |page, ix, _, cx| {
                    if let Some(place) = choices.get(ix).cloned() {
                        page.show(place, cx);
                    }
                })
                .render(&self.place_select, cx)
        });
        let mut body: Vec<AnyElement> = Vec::new();
        // slopty: why the rules are not read now, and what was skipped, each said where the
        // rules are.
        if let Some(why) = self.unreachable.clone().filter(|_| self.rules.is_some()) {
            body.push(notice("permissions-unreachable", why, &theme));
        }
        if let Some(words) = self.skipped.words() {
            body.push(notice("permissions-skipped", words.into(), &theme));
        }
        match self.rules.clone() {
            None if places.is_empty() => body.push(empty(&theme, "No machines yet", "Add a machine to see its agents' rules.")),
            None => match self.unreachable.clone() {
                Some(why) => body.push(empty(&theme, "Couldn't read the rules", &why)),
                None => body.push(empty(&theme, "Reading the rules…", "")),
            },
            Some(rules) if !rules.known => body.push(empty(
                &theme,
                "No rules yet",
                "The agent says its rules once a session runs in this folder.",
            )),
            Some(rules) => {
                for (behavior, label, about) in BEHAVIORS {
                    let rows: Vec<AnyElement> = rules
                        .rules
                        .iter()
                        .filter(|row| row.behavior == behavior)
                        .enumerate()
                        .map(|(ix, row)| self.rule_row(ix, row, &theme, cx))
                        .collect();
                    let block = if rows.is_empty() {
                        widgets::section_card(&theme).mt(px(0.0)).child(
                            widgets::card_row(&theme, true).child(
                                div()
                                    .text_size(crate::typography::ui_rems(12.5))
                                    .text_color(theme.text_muted)
                                    .child(SharedString::from(format!("None. {about}"))),
                            ),
                        )
                        .into_any_element()
                    } else {
                        widgets::section_card(&theme)
                            .mt(px(0.0))
                            .id(SharedString::from(format!("permission-{behavior}")))
                            .role(gpui::Role::List)
                            .aria_label(SharedString::from(label))
                            .children(rows)
                            .into_any_element()
                    };
                    body.push(widgets::section(&theme, label, block).into_any_element());
                }
                if !rules.directories.is_empty() {
                    body.push(
                        widgets::section(
                            &theme,
                            "Other folders it may work in",
                            widgets::section_card(&theme).mt(px(0.0)).children(rules.directories.iter().enumerate().map(
                                |(ix, dir)| {
                                    widgets::card_row(&theme, ix == 0)
                                        .child(widgets::row_title(&theme, dir.clone()).font_family(theme.font_mono.clone()))
                                },
                            )),
                        )
                        .into_any_element(),
                    );
                }
                body.push(self.add_row(&rules, &theme, cx));
            }
        }
        let scrollbar = popover::rail(self, "permissions-page-scrollbar", &theme, cx);
        div()
            .id("permissions-page-host")
            .relative()
            .size_full()
            .on_hover(cx.listener(Self::on_scroll_hovered))
            .child(
                crate::edge_fade::edge_faded(
                    16.0,
                    true,
                    true,
                    div()
                        .id("permissions-page")
                        .size_full()
                        .overflow_y_scroll()
                        .track_scroll(&self.scroll.scroll)
                        .child(
                            widgets::page_column()
                                .child(
                                    div()
                                        .flex()
                                        .flex_row()
                                        .items_end()
                                        .gap(px(16.0))
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .child(widgets::page_header(&theme, "Permissions", None))
                                                .child(widgets::page_subtitle(
                                                    &theme,
                                                    "What agents may do without asking, by folder, as their own settings say.",
                                                )),
                                        )
                                        .children(picker),
                                )
                                .children(body),
                        ),
                )
                .fade_overflow_y(&self.scroll.scroll),
            )
            .children(scrollbar)
    }
}

/// A line above the rules saying what is amiss with them, in the settings' error strip.
fn notice(id: &'static str, words: SharedString, theme: &Theme) -> AnyElement {
    widgets::error_strip(theme, words.clone())
        .id(id)
        .role(gpui::Role::Status)
        .aria_label(words)
        .into_any_element()
}

/// A quiet centred note in place of the rules.
fn empty(theme: &Theme, title: &str, words: &str) -> AnyElement {
    div()
        .id("permissions-empty")
        .role(gpui::Role::Status)
        .aria_label(SharedString::from(format!("{title}. {words}")))
        .mt(px(96.0))
        .flex()
        .flex_col()
        .items_center()
        .text_center()
        .text_color(theme.text_muted)
        .child(
            crate::icons::icon(crate::icons::LOCK)
                .size(px(28.0))
                .text_color(theme.text_muted.opacity(0.2)),
        )
        .child(
            div()
                .mt(px(12.0))
                .text_size(crate::typography::ui_rems(14.0))
                .child(SharedString::from(title.to_owned())),
        )
        .when(!words.is_empty(), |el| {
            el.child(
                div()
                    .mt(px(4.0))
                    .text_size(crate::typography::ui_rems(12.0))
                    .child(SharedString::from(words.to_owned())),
            )
        })
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;
    use slopty_view_doc::rules::RuleScope;

    /// A machine that turns the first watch away, then says its rules after an update that does
    /// not read.
    struct Rules(Arc<AtomicU32>);

    #[async_trait::async_trait]
    impl slopty_view_rpc::RpcService for Rules {
        async fn handle(
            &self,
            method: &str,
            _: serde_json::Value,
        ) -> Result<slopty_view_rpc::RpcReply, slopty_view_rpc::RpcError> {
            use futures::StreamExt as _;
            assert_eq!(method, calls::WATCH_RULES);
            if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
                return Err(slopty_view_rpc::RpcError::Failed("the machine is away".into()));
            }
            let rules = PermissionRules { chat_id: "c".into(), known: true, ..PermissionRules::default() };
            let frames = [serde_json::json!({ "known": "not a flag" }), serde_json::to_value(rules).unwrap()];
            let frames = futures::stream::iter(frames).chain(futures::stream::pending()).boxed();
            Ok(slopty_view_rpc::RpcReply::Stream(frames))
        }
    }

    /// The page asks again after 1 s, doubling to 15 s; a watch turned away says why and when it
    /// asks again, and an update that does not read is counted and said, not dropped.
    #[gpui::test]
    fn the_rules_are_asked_again_and_what_did_not_read_is_said(cx: &mut gpui::TestAppContext) {
        let waits: Vec<u64> = (0..6).map(|attempt| retry_after(attempt).as_secs()).collect();
        assert_eq!(waits, [1, 2, 4, 8, 15, 15]);
        assert_eq!(retry_after(u32::MAX).as_secs(), 15);

        // The machine answers on this thread, between the app's turns: a current-thread runtime
        // driven here, so nothing wakes the app's executor from elsewhere.
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let _guard = runtime.enter();
        let asked = Arc::new(AtomicU32::new(0));
        let engine = crate::state::EngineHandle::from_test_client(slopty_view_rpc::memory_client(
            Arc::new(Rules(Arc::clone(&asked))),
        ));
        let state = cx.new(|_| {
            let mut state = AppState::new();
            state.set_test_engine(engine);
            state
        });
        let page = cx.new(|cx| PermissionsPage::new(state, cx));
        let place = Place { device_id: "m".into(), cwd: Some("/w".into()), label: "w · m".into() };
        page.update(cx, |page, cx| page.show(place, cx));
        let settle = |cx: &mut gpui::TestAppContext, done: &dyn Fn(&PermissionsPage) -> bool| {
            for _ in 0..300 {
                cx.run_until_parked();
                runtime.block_on(async {
                    for _ in 0..20 {
                        tokio::task::yield_now().await;
                    }
                });
                cx.run_until_parked();
                if page.read_with(cx, |page, _| done(page)) {
                    return;
                }
            }
            panic!("the page never settled");
        };
        settle(cx, &|page| page.unreachable.is_some());
        page.read_with(cx, |page, _| {
            let why = page.unreachable.as_deref().unwrap_or_default();
            assert!(why.contains("the machine is away") && why.contains("again in 1s"), "{why}");
            assert!(page.rules.is_none());
        });
        cx.executor().advance_clock(retry_after(0));
        settle(cx, &|page| page.rules.is_some());
        assert_eq!(asked.load(Ordering::SeqCst), 2, "asked again once");
        page.read_with(cx, |page, _| {
            assert_eq!(page.unreachable, None, "reading again");
            assert_eq!(page.skipped.count, 1);
            let said = page.skipped.words().unwrap_or_default();
            assert!(said.starts_with("Couldn't read 1 update of the rules"), "{said}");
        });
    }

    /// A rule's scope is the one its source names, else the source as given.
    #[test]
    fn a_rule_goes_back_to_the_scope_it_came_from() {
        let rules = PermissionRules {
            scopes: ["local", "project", "user"]
                .iter()
                .map(|id| RuleScope { id: (*id).to_owned(), ..RuleScope::default() })
                .collect(),
            ..PermissionRules::default()
        };
        assert_eq!(scope_of(&rules, "localSettings"), "local");
        assert_eq!(scope_of(&rules, "userSettings"), "user");
        assert_eq!(scope_of(&rules, "session"), "session");
    }
}
