//! The board's budget: the panel under the header that sets what the project's agents may
//! spend, and the *Needs you* row that says a cap was reached (`docs/decisions/projects.md`,
//! "A project may have a budget per meter").
//!
//! The panel takes the cost cap in dollars and the plan windows' caps as `five-hour 80%`,
//! comma-parted, so a window an agent names tomorrow needs no new field. Both empty takes the
//! budget away.

use std::collections::BTreeMap;

use gpui::accesskit::Role;
use gpui::{
    AnyElement, AppContext as _, Context, Div, Entity, InteractiveElement as _, IntoElement as _,
    ParentElement as _, SharedString, Stateful, StatefulInteractiveElement as _, Styled as _,
    Subscription, Window, div,
};
use gpui_kit::component::input::{Escape, Input, InputEvent, InputState};
use slopty_proto::project::Budget;
use slopty_theme::Typography;

use super::{ProjectEvent, ProjectView};
use crate::a11y::tab_stop;
use crate::colors::hsla;
use crate::project::model::Board;

/// What the panel and its palette line are called.
pub(super) const BUDGET: &str = "Budget";
/// The cost cap's label.
const COST: &str = "Cost cap, estimated US dollars";
/// The windows' caps' label.
const WINDOWS: &str = "Plan window caps";
/// What the windows' field says while it is empty.
const WINDOWS_HINT: &str = "five-hour 80%, seven-day 50%";
/// What the cost field says while it is empty.
const COST_HINT: &str = "No cap";
/// The *Needs you* row's button.
pub(super) const RAISE: &str = "Raise budget";

/// The budget being set, while its panel is open.
pub(super) struct BudgetPanel {
    cost: Entity<InputState>,
    windows: Entity<InputState>,
    _enter: [Subscription; 2],
}

/// The budget `cost` and `windows` write: a cap in dollars, and `name share` pairs parted by
/// commas. Both empty is the empty budget, which takes it away.
///
/// # Errors
///
/// The words that say what was not read.
pub(super) fn typed_budget(cost: &str, windows: &str) -> Result<Budget, String> {
    let mut caps = BTreeMap::new();
    if !cost.trim().is_empty() {
        let cap = Budget::cap_of(Budget::USD, cost)
            .ok_or_else(|| format!("{cost:?} is not dollars above nothing, to the cent"))?;
        caps.insert(Budget::USD.to_owned(), cap);
    }
    for pair in windows.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let (name, share) = pair
            .rsplit_once(|c: char| c.is_whitespace() || c == '=')
            .map(|(n, s)| (n.trim(), s.trim()))
            .ok_or_else(|| format!("{pair:?} is not a window and its share, as five-hour 80%"))?;
        let cap = Budget::cap_of(name, share).filter(|_| name != Budget::USD).ok_or_else(|| {
            format!("{pair:?}: a window's cap is a share above nothing, at most 100%")
        })?;
        if caps.insert(name.to_owned(), cap).is_some() {
            return Err(format!("{name} is capped twice"));
        }
    }
    let budget = Budget(caps);
    if budget.fits() {
        Ok(budget)
    } else {
        Err(format!(
            "a window's name has no spaces, and a budget caps at most {} meters",
            Budget::METERS_MAX
        ))
    }
}

impl ProjectView {
    /// Open the budget panel, filled with the budget as it stands; the keyboard goes to the
    /// cost.
    pub(super) fn open_budget(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let budget = self.seen.board.as_ref().and_then(|b| b.project.limits.budget.clone());
        let budget = budget.unwrap_or_default();
        let cost = budget.0.get(Budget::USD).map(|c| Budget::figure(Budget::USD, *c));
        let cost = cost.map(|c| c.trim_start_matches('$').to_owned()).unwrap_or_default();
        let windows: Vec<String> = budget
            .0
            .iter()
            .filter(|(meter, _)| *meter != Budget::USD)
            .map(|(meter, cap)| format!("{meter} {}", Budget::figure(meter, *cap)))
            .collect();
        let field =
            |text: String, hint: &'static str, window: &mut Window, cx: &mut Context<Self>| {
                let input = cx.new(|cx| InputState::new(window, cx).placeholder(hint));
                input.update(cx, |input, cx| input.set_value(text, window, cx));
                input
            };
        let cost = field(cost, COST_HINT, window, cx);
        let windows = field(windows.join(", "), WINDOWS_HINT, window, cx);
        let enter = |input: &Entity<InputState>, window: &mut Window, cx: &mut Context<Self>| {
            cx.subscribe_in(input, window, |this, _input, event, window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.save_budget(window, cx);
                }
            })
        };
        let enters = [enter(&cost, window, cx), enter(&windows, window, cx)];
        cost.update(cx, |input, cx| input.focus(window, cx));
        self.budget = Some(BudgetPanel { cost, windows, _enter: enters });
        cx.notify();
    }

    /// Close the budget panel unsaved; the keyboard goes back to the board.
    pub(super) fn close_budget(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.budget.take().is_some() {
            window.focus(&self.focus, cx);
            cx.notify();
        }
    }

    /// Fill the open panel, as the person would type it.
    #[cfg(test)]
    pub(crate) fn type_budget(
        &self,
        cost: &str,
        windows: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(panel) = self.budget.as_ref() else { return };
        let (c, w) = (panel.cost.clone(), panel.windows.clone());
        c.update(cx, |input, cx| input.set_value(cost.to_owned(), window, cx));
        w.update(cx, |input, cx| input.set_value(windows.to_owned(), window, cx));
    }

    /// Whether the budget panel is open.
    #[cfg(test)]
    pub(crate) const fn budget_open(&self) -> bool {
        self.budget.is_some()
    }

    /// Say the panel's budget to the server and close it; what does not read is said and the
    /// panel stays.
    fn save_budget(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(panel) = self.budget.as_ref() else { return };
        let cost = panel.cost.read(cx).value().to_string();
        let windows = panel.windows.read(cx).value().to_string();
        match typed_budget(&cost, &windows) {
            Ok(budget) => {
                cx.emit(ProjectEvent::SetBudget(budget));
                self.close_budget(window, cx);
            }
            Err(why) => cx.emit(ProjectEvent::Say(why)),
        }
    }

    /// The panel under the header that sets the budget, while it is open.
    pub(super) fn budget_panel(&self, cx: &Context<Self>) -> Option<Stateful<Div>> {
        let panel = self.budget.as_ref()?;
        let theme = &self.theme;
        let s = &theme.surfaces;
        let sp = theme.spacing;
        let label = |text: &'static str| {
            div()
                .text_size(self.z(theme.typography.small()))
                .text_color(hsla(s.text_muted))
                .child(text)
        };
        let cancel = self
            .panel_button("project-budget-cancel", "Cancel")
            .text_color(hsla(s.text_secondary))
            .hover(move |el| el.bg(hsla(s.hover)).text_color(hsla(s.text)));
        let cancel = tab_stop(cancel, s.accent)
            .on_click(cx.listener(|this, _ev, window, cx| this.close_budget(window, cx)));
        let save =
            crate::kit::solid_pressable(self.panel_button("project-budget-save", "Save"), theme);
        let save = tab_stop(save, s.accent)
            .on_click(cx.listener(|this, _ev, window, cx| this.save_budget(window, cx)));
        Some(
            div()
                .id("project-budget-panel")
                .debug_selector(|| "project-budget-panel".to_owned())
                .role(Role::Group)
                .aria_label(BUDGET)
                .flex_none()
                .flex()
                .flex_col()
                .gap(self.z(sp.xs))
                .mx(self.z(sp.inset()))
                .mb(self.z(sp.sm))
                .p(self.z(sp.md))
                .rounded(self.z(theme.radii.md))
                .bg(hsla(s.panel))
                .on_action(
                    cx.listener(|this, _: &Escape, window, cx| this.close_budget(window, cx)),
                )
                .child(label(COST))
                .child(Input::new(&panel.cost).aria_label(COST))
                .child(label(WINDOWS))
                .child(Input::new(&panel.windows).aria_label(WINDOWS))
                .child(
                    div()
                        .flex()
                        .justify_end()
                        .gap(self.z(sp.xs))
                        .pt(self.z(sp.xs))
                        .child(cancel)
                        .child(save),
                ),
        )
    }

    /// A panel's text button, as the checks panel's.
    pub(super) fn panel_button(&self, id: &'static str, text: &'static str) -> Stateful<Div> {
        let theme = &self.theme;
        div()
            .id(id)
            .debug_selector(move || id.to_owned())
            .role(Role::Button)
            .aria_label(text)
            .flex_none()
            .flex()
            .items_center()
            .h(self.z(theme.density.control))
            .px(self.z(theme.spacing.md))
            .rounded(self.z(theme.radii.sm))
            .font_weight(gpui::FontWeight(Typography::MEDIUM_WEIGHT))
            .cursor_pointer()
            .child(text)
    }

    /// The *Needs you* row of a budget spent: what was reached, and the way to raise it.
    pub(super) fn budget_row(&self, board: &Board, cx: &Context<Self>) -> Option<AnyElement> {
        let said = board.over_budget()?;
        let theme = &self.theme;
        let s = &theme.surfaces;
        let sp = theme.spacing;
        let raise = self
            .panel_button("project-budget-raise", RAISE)
            .text_color(hsla(s.accent))
            .hover(move |el| el.bg(hsla(s.hover)));
        let raise = tab_stop(raise, s.accent)
            .on_click(cx.listener(|this, _ev, window, cx| this.open_budget(window, cx)));
        Some(
            div()
                .id("project-budget-reached")
                .debug_selector(|| "project-budget-reached".to_owned())
                .role(Role::Status)
                .aria_label(SharedString::from(said.clone()))
                .flex()
                .items_center()
                .gap(self.z(sp.sm))
                .px(self.z(sp.inset()))
                .py(self.z(sp.xxs))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_color(hsla(s.text))
                        .child(SharedString::from(said)),
                )
                .child(raise)
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use gpui::{Modifiers, TestAppContext, VisualTestContext, px, size};
    use slopty_proto::project::{Limits, Spend};
    use slopty_theme::Theme;

    use super::super::Seen;
    use super::*;
    use crate::project::fixtures::{project, snapshot, status};
    use crate::project::model::Projects;

    /// A board of a project whose agents spent `spent` against `budget`, 800 × 600 points,
    /// and what it asked of the workspace.
    fn board(
        cx: &mut TestAppContext,
        budget: Option<Budget>,
        spent: u64,
    ) -> (Entity<ProjectView>, &mut VisualTestContext, Rc<RefCell<Vec<ProjectEvent>>>) {
        let mut p = project("board", None);
        p.limits = Limits { budget, ..Limits::default() };
        p.spend = Spend { cost_micro_usd: spent, ..Spend::default() };
        let mut mirror = Projects::default();
        mirror.apply_part(snapshot(10, vec![status(p, Vec::new(), Vec::new())]));
        let board = mirror.get(&crate::project::fixtures::id("board")).cloned().expect("it");
        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.bind_keys(crate::workspace::key_bindings());
        });
        let heard: Rc<RefCell<Vec<ProjectEvent>>> = Rc::default();
        let into = Rc::clone(&heard);
        let (view, cx) = cx.add_window_view(|window, cx| {
            let mut view = ProjectView::new(board.project.id.clone(), Theme::default(), cx);
            view.set_seen(Seen { board: Some(board), ..Seen::default() }, cx);
            view.focus(window, cx);
            view
        });
        cx.update(|_w, cx| {
            cx.subscribe(&view, move |_v, event: &ProjectEvent, _cx| {
                into.borrow_mut().push(event.clone());
            })
            .detach();
        });
        cx.simulate_resize(size(px(800.0), px(600.0)));
        cx.run_until_parked();
        (view, cx, heard)
    }

    fn click(cx: &mut VisualTestContext, selector: &'static str) {
        let at = cx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector} is not drawn"));
        cx.simulate_click(at.center(), Modifiers::default());
        cx.run_until_parked();
    }

    /// A project whose agents spent its cap says so under *Needs you*, and Raise opens the
    /// panel with the budget as it stands; Save sends the raised one. Under the cap nothing
    /// is said, and the palette's Budget opens the same panel.
    #[gpui::test]
    fn a_spent_budget_needs_you_and_is_raised_from_the_board(cx: &mut TestAppContext) {
        let cap = Budget(BTreeMap::from([(Budget::USD.to_owned(), 10_000_000)]));
        let (view, cx, heard) = board(cx, Some(cap), 10_500_000);
        assert!(cx.debug_bounds("project-needs-you").is_some());
        let cost = cx.debug_bounds("project-cost");
        assert!(cost.is_some(), "the header says the spend beside its cap");
        let said = view.read_with(cx, |v, _| v.seen.board.as_ref().and_then(|b| b.over_budget()));
        assert_eq!(
            said.as_deref(),
            Some(
                "Spent an estimated $10.50 of its $10.00 budget: no task starts until you raise it"
            )
        );
        click(cx, "project-budget-raise");
        assert!(view.read_with(cx, |v, _| v.budget_open()), "Raise opens the panel");
        let filled = view
            .read_with(cx, |v, cx| v.budget.as_ref().map(|p| p.cost.read(cx).value().to_string()));
        assert_eq!(filled.as_deref(), Some("10.00"), "filled with the budget as it stands");
        view.update_in(cx, |v, window, cx| v.type_budget("20", "five-hour 80%", window, cx));
        click(cx, "project-budget-save");
        assert!(!view.read_with(cx, |v, _| v.budget_open()), "saved and closed");
        let raised = Budget(BTreeMap::from([
            ("five-hour".to_owned(), 8_000),
            (Budget::USD.to_owned(), 20_000_000),
        ]));
        assert!(
            matches!(heard.borrow().as_slice(), [ProjectEvent::SetBudget(b)] if *b == raised),
            "{:?}",
            heard.borrow()
        );
    }

    /// Under its cap the board says nothing of the budget; the palette's line opens the
    /// panel, a word it cannot read is said and the panel stays, and Esc closes it unsaved.
    #[gpui::test]
    fn a_budget_under_its_cap_is_quiet_and_set_from_the_palette(cx: &mut TestAppContext) {
        let (view, cx, heard) = board(cx, None, 2_000_000);
        assert!(cx.debug_bounds("project-budget-reached").is_none());
        cx.dispatch_action(crate::project::EditBudget);
        cx.run_until_parked();
        assert!(view.read_with(cx, |v, _| v.budget_open()));
        view.update_in(cx, |v, window, cx| v.type_budget("ten", "", window, cx));
        click(cx, "project-budget-save");
        assert!(view.read_with(cx, |v, _| v.budget_open()), "the panel stays");
        assert!(
            matches!(heard.borrow().as_slice(), [ProjectEvent::Say(why)] if why.contains("ten")),
            "{:?}",
            heard.borrow()
        );
        cx.simulate_keystrokes("escape");
        assert!(!view.read_with(cx, |v, _| v.budget_open()), "Esc closes it unsaved");
    }

    /// The panel reads dollars and windows by name and share; both empty takes the budget
    /// away; a word it cannot read is said.
    #[test]
    fn the_panel_reads_dollars_and_windows_by_name() {
        let budget = typed_budget("12.50", "five-hour 80%, seven-day=50").unwrap();
        assert_eq!(
            budget.0,
            BTreeMap::from([
                ("five-hour".to_owned(), 8_000),
                ("seven-day".to_owned(), 5_000),
                (Budget::USD.to_owned(), 12_500_000),
            ])
        );
        assert_eq!(typed_budget(" ", "").unwrap(), Budget::default());
        assert!(typed_budget("ten", "").unwrap_err().contains("ten"));
        assert!(typed_budget("", "five-hour").unwrap_err().contains("five-hour"));
        assert!(typed_budget("", "five-hour 120%").unwrap_err().contains("100%"));
        assert!(typed_budget("", "usd 5").is_err(), "the cost has its own field");
        assert!(typed_budget("", "a 5%, a 6%").unwrap_err().contains("twice"));
    }
}
