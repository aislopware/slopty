// slopty: Slopty's own, drawn from zeron's dialog, settings and segmented-tab anatomy.
//! Settings → Devices' Add device dialog: this Mac shared, a machine installed over ssh (or the
//! server set up there), or the server this app follows. The app runs each (`AddDevice`); the
//! dialog draws its steps, the host key or password a run stops to ask for, this Mac's
//! checklist with each line's fix, the servers to choose between, and what was added. A look on
//! the tailnet (`LookOnTailnet`) offers the servers and machines that answer.

use gpui::{
    AnyElement, App, Context, Entity, EventEmitter, Focusable, SharedString, Subscription, Task, Window,
    div, prelude::*, px,
};
use gpui_base::input::{Input, InputEvent, InputState};
use serde::Deserialize;
use slopty_view_rpc::methods;

use crate::popover;
use crate::settings::widgets;
use crate::state::AppState;
use crate::theme::Theme;

/// What the dialog puts where.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Choice {
    /// This Mac, shared through the server.
    ThisMac,
    /// A machine reached with ssh.
    Ssh,
    /// The server this app follows.
    Server,
}

impl Choice {
    /// The choices this device offers: a Mac shares itself and reaches others over ssh.
    fn offered() -> &'static [Self] {
        if cfg!(target_os = "macos") {
            &[Self::ThisMac, Self::Ssh, Self::Server]
        } else {
            &[Self::Server]
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::ThisMac => "This Mac",
            Self::Ssh => "Over SSH",
            Self::Server => "A server",
        }
    }

    const fn blurb(self) -> &'static str {
        match self {
            Self::ThisMac => {
                "Work on this Mac. Slopty shares its shells, windows and agents through your server, so your other devices reach it too."
            }
            Self::Ssh => {
                "Slopty copies itself to a Mac or Linux machine you reach with ssh, using your ssh config and agent, and lists it here."
            }
            Self::Server => {
                "Your devices meet at one Slopty server on your tailnet or VPN. Connect to it by its name or address."
            }
        }
    }
}

/// One frame of a run, as the view model streams it.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Progress {
    run: u64,
    steps: Vec<Step>,
    share: Option<f32>,
    failure: Option<Failure>,
    choices: Vec<Choose>,
    added: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Step {
    title: String,
    detail: Option<String>,
    mark: Mark,
    fix: Option<Fix>,
}

#[derive(Clone, Debug, Deserialize)]
struct Fix {
    id: String,
    label: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
enum Mark {
    Done,
    Running,
    Failed,
    NeedsYou,
    Waiting,
}

#[derive(Clone, Debug, Deserialize)]
struct Choose {
    name: String,
    at: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Failure {
    title: String,
    hint: Option<String>,
    lines: Vec<String>,
    trust: Option<Vec<String>>,
    password: Option<String>,
    refused: bool,
}

/// What a look on the tailnet found.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Found {
    looking: bool,
    running: bool,
    servers: Vec<Node>,
    workers: Vec<Node>,
}

#[derive(Clone, Debug, Deserialize)]
struct Node {
    name: String,
    at: String,
    ready: bool,
    note: Option<String>,
}

/// The dialog is done: the page lets it go.
pub struct Closed;

/// The Add device dialog.
pub struct AddDeviceDialog {
    state: Entity<AppState>,
    choice: Choice,
    /// Over SSH sets up the server there rather than a machine.
    serve: bool,
    target: Entity<InputState>,
    address: Entity<InputState>,
    /// The machine's password, shown only once a run asks for one; emptied as it is sent.
    password: Entity<InputState>,
    progress: Option<Progress>,
    /// The run's stream is open: it is under way or waits for an answer.
    open: bool,
    /// Why the dialog cannot go on, said under the fields.
    note: Option<SharedString>,
    /// Reads the run's stream; dropped, it stops the run.
    run: Option<Task<()>>,
    answer: Option<Task<()>>,
    found: Option<Found>,
    look: Option<Task<()>>,
    /// What had the keyboard before the dialog took it, given back as it closes.
    restore: Option<gpui::FocusHandle>,
    _events: [Subscription; 3],
}

impl EventEmitter<Closed> for AddDeviceDialog {}

impl AddDeviceDialog {
    /// The dialog on `choice` (the first offered when this device does not offer it), its field
    /// with the keyboard.
    pub fn new(
        state: Entity<AppState>,
        choice: Choice,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let choice =
            if Choice::offered().contains(&choice) { choice } else { Choice::offered()[0] };
        let target = cx.new(|cx| {
            InputState::new(window, cx).placeholder("mac-mini, me@build-box or 100.64.0.7")
        });
        let address =
            cx.new(|cx| InputState::new(window, cx).placeholder("my-server or 100.64.0.1:7400"));
        let password =
            cx.new(|cx| InputState::new(window, cx).masked(true).placeholder("Password"));
        let go = |this: &mut Self,
                  _: &Entity<InputState>,
                  event: &InputEvent,
                  window: &mut Window,
                  cx: &mut Context<Self>| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.go(window, cx);
            }
        };
        let events = [
            cx.subscribe_in(&target, window, go),
            cx.subscribe_in(&address, window, go),
            cx.subscribe_in(&password, window, go),
        ];
        let restore = window.focused(cx);
        let mut dialog = Self {
            state,
            choice,
            serve: false,
            target,
            address,
            password,
            progress: None,
            open: false,
            note: None,
            run: None,
            answer: None,
            found: None,
            look: None,
            restore,
            _events: events,
        };
        dialog.choose(choice, window, cx);
        dialog
    }

    /// Show "A server" with `address` in its field, unless a run is under way.
    pub fn fill_server(&mut self, address: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.running() {
            return;
        }
        self.choose(Choice::Server, window, cx);
        self.address.update(cx, |input, cx| input.set_value(address, window, cx));
        cx.notify();
    }

    /// Show `choice`: its field takes the keyboard, and the tailnet is looked on once for what
    /// it offers.
    fn choose(&mut self, choice: Choice, window: &mut Window, cx: &mut Context<Self>) {
        if self.running() {
            return;
        }
        self.choice = choice;
        self.progress = None;
        self.note = None;
        self.open = false;
        self.run = None;
        let field = match choice {
            Choice::ThisMac => None,
            Choice::Ssh => Some(self.target.focus_handle(cx)),
            Choice::Server => Some(self.address.focus_handle(cx)),
        };
        if let Some(field) = field {
            window.focus(&field, cx);
        }
        if choice != Choice::ThisMac && self.look.is_none() && cfg!(target_os = "macos") {
            self.look_on_tailnet(cx);
        }
        cx.notify();
    }

    fn look_on_tailnet(&mut self, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else { return };
        self.look = Some(cx.spawn(async move |this, cx| {
            let subscribed =
                engine.client().subscribe_checked(methods::LOOK_ON_TAILNET, serde_json::json!({})).await;
            let Ok(mut stream) = subscribed else { return };
            while let Some(value) = stream.recv().await {
                let Ok(found) = serde_json::from_value::<Found>(value) else { continue };
                let kept = this.update(cx, |dialog, cx| {
                    dialog.found = Some(found);
                    cx.notify();
                });
                if kept.is_err() {
                    return;
                }
            }
        }));
    }

    /// The run is going, and not waiting on the person.
    fn running(&self) -> bool {
        self.open && !self.asks()
    }

    /// The run stopped to ask the person something and waits for the answer.
    fn asks(&self) -> bool {
        let Some(progress) = &self.progress else { return false };
        let failure = progress.failure.as_ref();
        self.open
            && (failure.is_some_and(|f| f.trust.is_some() || f.password.is_some())
                || !progress.choices.is_empty()
                || progress.steps.iter().any(|s| s.fix.is_some()))
    }

    /// What a run that stopped asks the dialog's button to answer: a host key or a password.
    fn asks_key_or_password(&self) -> Option<&Failure> {
        let failure = self.progress.as_ref()?.failure.as_ref()?;
        (self.open && (failure.trust.is_some() || failure.password.is_some())).then_some(failure)
    }

    fn added(&self) -> Option<&str> {
        self.progress.as_ref()?.added.as_deref()
    }

    /// Close, giving the keyboard back to what had it.
    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.restore.take() {
            Some(restore) => window.focus(&restore, cx),
            None => window.blur(cx),
        }
        self.run = None;
        cx.emit(Closed);
    }

    /// The dialog's one button, or Return: answer what the run asks, or start one.
    fn go(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.running() {
            return;
        }
        if self.added().is_some() {
            return self.close(window, cx);
        }
        if let Some(asks) = self.asks_key_or_password() {
            let run = self.progress.as_ref().map_or(0, |p| p.run);
            let params = if asks.trust.is_some() {
                serde_json::json!({ "run": run, "trust": true })
            } else {
                let typed = self.password.read(cx).value().to_string();
                if typed.is_empty() {
                    self.note = Some("Type the password first".into());
                    cx.notify();
                    return;
                }
                let password = self.password.clone();
                password.update(cx, |input, cx| input.set_value("", window, cx));
                serde_json::json!({ "run": run, "password": typed, "addKey": true })
            };
            return self.answer(params, cx);
        }
        let (kind, typed) = match self.choice {
            Choice::ThisMac => ("thisMac", String::new()),
            Choice::Ssh => {
                (if self.serve { "serveSsh" } else { "ssh" }, self.target.read(cx).value().to_string())
            }
            Choice::Server => ("server", self.address.read(cx).value().to_string()),
        };
        let typed = typed.trim().to_owned();
        if typed.is_empty() && self.choice != Choice::ThisMac {
            self.note = Some(
                match self.choice {
                    Choice::Server => "Type the server's name or address",
                    _ => "Type the machine's name or address",
                }
                .into(),
            );
            cx.notify();
            return;
        }
        self.start(kind, typed, cx);
    }

    /// Start a run of `kind` against `target`, following its stream until it ends.
    fn start(&mut self, kind: &'static str, target: String, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else { return };
        self.note = None;
        self.progress = None;
        self.open = true;
        self.run = Some(cx.spawn(async move |this, cx| {
            let params = serde_json::json!({ "kind": kind, "target": target });
            let subscribed = engine.client().subscribe_checked(methods::ADD_DEVICE, params).await;
            let mut stream = match subscribed {
                Ok(stream) => stream,
                Err(err) => {
                    this.update(cx, |dialog, cx| {
                        dialog.open = false;
                        dialog.note = Some(err.to_string().into());
                        cx.notify();
                    })
                    .ok();
                    return;
                }
            };
            while let Some(value) = stream.recv().await {
                let Ok(progress) = serde_json::from_value::<Progress>(value) else { continue };
                let kept = this.update(cx, |dialog, cx| {
                    dialog.progress = Some(progress);
                    cx.notify();
                });
                if kept.is_err() {
                    return;
                }
            }
            this.update(cx, |dialog, cx| {
                dialog.open = false;
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// Send the person's answer to the run that asked; it goes on in its own stream.
    fn answer(&mut self, params: serde_json::Value, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else { return };
        self.note = None;
        // The run goes on: what it asked is answered.
        if let Some(progress) = &mut self.progress {
            if let Some(failure) = &mut progress.failure {
                failure.trust = None;
                failure.password = None;
            }
            progress.choices.clear();
        }
        self.answer = Some(cx.spawn(async move |this, cx| {
            let result = engine.client().call(methods::ANSWER_ADD_DEVICE, params).await;
            this.update(cx, |dialog, cx| {
                if let Err(err) = result {
                    dialog.note = Some(err.to_string().into());
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// A step's fix, or a server chosen, for the run that asked.
    fn answer_with(&mut self, key: &'static str, value: String, cx: &mut Context<Self>) {
        let run = self.progress.as_ref().map_or(0, |p| p.run);
        self.answer(serde_json::json!({ "run": run, key: value }), cx);
    }

    /// A node the tailnet offered, pressed: a server is connected to, a machine installed on.
    fn press_found(&mut self, server: bool, at: String, window: &mut Window, cx: &mut Context<Self>) {
        let (field, choice) =
            if server { (&self.address, Choice::Server) } else { (&self.target, Choice::Ssh) };
        let field = field.clone();
        field.update(cx, |input, cx| input.set_value(at, window, cx));
        if self.choice != choice {
            self.choose(choice, window, cx);
        }
        self.go(window, cx);
    }

    fn render_tabs(&self, theme: &Theme, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let offered = Choice::offered();
        if offered.len() < 2 {
            return div().into_any_element();
        }
        let reduced = crate::motion::reduced_motion(cx);
        let mut row = div()
            .id("add-device-choices")
            .mt(px(14.0))
            .flex()
            .flex_row()
            .gap(px(4.0))
            .role(gpui::Role::TabList);
        for (ix, choice) in offered.iter().copied().enumerate() {
            let selected = self.choice == choice;
            let t = widgets::tab_selection_t(
                window,
                format!("add-device-tab-{ix}"),
                selected,
                reduced,
            );
            row = row.child(
                widgets::section_tab(
                    theme,
                    selected,
                    t,
                    format!("add-device-choice-{ix}"),
                    format!("add-device-choice-hover-{ix}"),
                )
                .role(gpui::Role::Tab)
                .aria_label(choice.label())
                .aria_selected(selected)
                .tab_index(0)
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, window, cx| this.choose(choice, window, cx)))
                .child(choice.label()),
            );
        }
        row.into_any_element()
    }

    /// The nodes the tailnet offered for the choice on show: servers to connect to, machines to
    /// install on.
    fn render_found(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let found = self.found.as_ref()?;
        let server = match self.choice {
            Choice::Server => true,
            Choice::Ssh if !self.serve => false,
            Choice::Ssh | Choice::ThisMac => return None,
        };
        let nodes = if server { &found.servers } else { &found.workers };
        let words = if found.looking {
            Some("Looking on your tailnet\u{2026}")
        } else if !found.running {
            Some("Tailscale is not running on this Mac, so type an address.")
        } else if nodes.is_empty() {
            Some(if server { "No Slopty server found on your tailnet yet." } else { "No other machine with Slopty found on your tailnet." })
        } else {
            None
        };
        let mut list = div()
            .mt(px(12.0))
            .flex()
            .flex_col()
            .gap(px(2.0))
            .child(widgets::section_label(theme, "On your tailnet"));
        if let Some(words) = words {
            list = list.child(
                div()
                    .px(px(4.0))
                    .text_size(crate::typography::ui_rems(12.5))
                    .text_color(theme.text_muted)
                    .child(words),
            );
        }
        for (ix, node) in nodes.iter().enumerate() {
            let at = node.at.clone();
            let detail = node.note.clone().unwrap_or_else(|| node.at.clone());
            let mut row = div()
                .id(("add-device-found", ix))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .px(px(8.0))
                .py(px(6.0))
                .rounded(px(8.0))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .text_size(crate::typography::ui_rems(13.0))
                                .text_color(theme.text)
                                .truncate()
                                .child(node.name.clone()),
                        )
                        .child(
                            div()
                                .text_size(crate::typography::ui_rems(12.0))
                                .text_color(theme.text_muted)
                                .truncate()
                                .child(detail),
                        ),
                );
            if node.ready {
                row = row.child(
                    widgets::text_action(
                        theme,
                        widgets::ActionTone::Filled,
                        if server { "Connect" } else { "Install" },
                    )
                    .id(("add-device-found-go", ix))
                    .tab_index(0)
                    .role(gpui::Role::Button)
                    .aria_label(format!(
                        "{} {}",
                        if server { "Connect to" } else { "Install on" },
                        node.name
                    ))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.press_found(server, at.clone(), window, cx);
                    })),
                );
            }
            list = list.child(row);
        }
        Some(list.into_any_element())
    }

    fn render_steps(&self, theme: &Theme, progress: &Progress, cx: &mut Context<Self>) -> AnyElement {
        let view = cx.entity_id();
        let waits = self.open;
        let mut list = div().mt(px(16.0)).flex().flex_col().gap(px(2.0));
        for (ix, step) in progress.steps.iter().enumerate() {
            let mark = match step.mark {
                Mark::Done => crate::icons::icon(crate::icons::CHECK)
                    .size(px(14.0))
                    .text_color(theme.success_muted)
                    .into_any_element(),
                Mark::Failed => crate::icons::icon(crate::icons::CLOSE)
                    .size(px(14.0))
                    .text_color(theme.danger_muted)
                    .into_any_element(),
                Mark::NeedsYou => div()
                    .size(px(6.0))
                    .rounded_full()
                    .bg(theme.warning)
                    .into_any_element(),
                Mark::Running => crate::loaders::mini_mono_spinner(
                    SharedString::from(format!("add-device-step-{ix}")),
                    2.0,
                    theme.text_muted,
                    view,
                    cx,
                )
                .into_any_element(),
                Mark::Waiting => div()
                    .size(px(4.0))
                    .rounded_full()
                    .bg(theme.text_muted.opacity(0.5))
                    .into_any_element(),
            };
            let quiet = step.mark == Mark::Waiting;
            let fix = step.fix.clone().filter(|_| waits).map(|fix| {
                let id = fix.id.clone();
                widgets::text_action(theme, widgets::ActionTone::Filled, fix.label.clone())
                    .id(("add-device-fix", ix))
                    .flex_none()
                    .tab_index(0)
                    .role(gpui::Role::Button)
                    .aria_label(fix.label)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.answer_with("fix", id.clone(), cx);
                    }))
            });
            list = list.child(
                div()
                    .id(("add-device-step", ix))
                    .role(gpui::Role::ListItem)
                    .aria_label(step.title.clone())
                    .min_h(px(28.0))
                    .py(px(2.0))
                    .flex()
                    .flex_row()
                    .items_start()
                    .gap(px(8.0))
                    .child(
                        div()
                            .flex_none()
                            .mt(px(2.0))
                            .size(px(16.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(mark),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_size(crate::typography::ui_rems(13.0))
                                    .text_color(if quiet { theme.text_muted } else { theme.text })
                                    .child(step.title.clone()),
                            )
                            .children(step.detail.clone().filter(|d| !d.is_empty()).map(|detail| {
                                div()
                                    .text_size(crate::typography::ui_rems(12.0))
                                    .text_color(theme.text_muted)
                                    .child(detail)
                            })),
                    )
                    .children(fix),
            );
        }
        if let Some(share) = progress.share {
            list = list.child(
                div()
                    .mt(px(6.0))
                    .h(px(2.0))
                    .w_full()
                    .rounded_full()
                    .bg(theme.text_muted.opacity(0.15))
                    .child(
                        div()
                            .h_full()
                            .rounded_full()
                            .bg(theme.accent)
                            .w(gpui::relative(share.clamp(0.0, 1.0))),
                    ),
            );
        }
        for (ix, choose) in progress.choices.iter().enumerate().filter(|_| waits) {
            let at = choose.at.clone();
            list = list.child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .pl(px(24.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(crate::typography::ui_rems(13.0))
                            .truncate()
                            .child(format!("{} \u{b7} {}", choose.name, choose.at)),
                    )
                    .child(
                        widgets::text_action(theme, widgets::ActionTone::Filled, "Join")
                            .id(("add-device-choose", ix))
                            .tab_index(0)
                            .role(gpui::Role::Button)
                            .aria_label(format!("Join {}", choose.name))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.answer_with("choose", at.clone(), cx);
                            })),
                    ),
            );
        }
        list.into_any_element()
    }

    fn render_failure(&self, theme: &Theme, failure: &Failure, cx: &App) -> AnyElement {
        let mono = crate::typography::code_effective_family_name(cx);
        let mut block = div()
            .mt(px(14.0))
            .px(px(12.0))
            .py(px(10.0))
            .rounded(px(10.0))
            .border_1()
            .border_color(theme.danger.opacity(0.2))
            .bg(theme.danger.opacity(0.06))
            .flex()
            .flex_col()
            .gap(px(4.0))
            .text_size(crate::typography::ui_rems(12.5))
            .child(div().text_color(theme.danger_muted.opacity(0.9)).child(failure.title.clone()));
        if let Some(hint) = failure.hint.clone() {
            block = block.child(div().text_color(theme.text_muted).child(hint));
        }
        for line in failure.lines.iter().take(8) {
            block = block.child(
                div()
                    .font_family(mono.clone())
                    .text_size(crate::typography::ui_rems(11.5))
                    .text_color(theme.text_muted)
                    .child(line.clone()),
            );
        }
        block.into_any_element()
    }

    fn render_ask(&self, theme: &Theme, cx: &App) -> Option<AnyElement> {
        let asks = self.asks_key_or_password()?;
        let mono = crate::typography::code_effective_family_name(cx);
        let mut block = div().mt(px(12.0)).flex().flex_col().gap(px(6.0));
        if let Some(keys) = &asks.trust {
            block = block.child(popover::dialog_body(
                theme,
                "ssh does not know this machine yet. Check that its host key is the one it shows itself, then trust it.",
            ));
            for key in keys {
                block = block.child(
                    div()
                        .font_family(mono.clone())
                        .text_size(crate::typography::ui_rems(11.5))
                        .text_color(theme.text)
                        .child(key.clone()),
                );
            }
        } else if let Some(who) = &asks.password {
            let said = if asks.refused {
                format!("{who} refused that password. Type it again.")
            } else {
                format!(
                    "{who} asks for a password. It is used for this install only, and your key is added there so it does not ask again."
                )
            };
            block = block
                .child(popover::dialog_body(theme, said))
                .child(popover::dialog_field(Input::new(&self.password).into_any_element()));
        }
        Some(block.into_any_element())
    }
}


impl Render for AddDeviceDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).for_popup();
        let running = self.running();
        let progress = self.progress.clone();
        let added = self.added().map(str::to_owned);
        let failure = progress.as_ref().and_then(|p| p.failure.clone()).filter(|_| added.is_none());
        let tabs = self.render_tabs(&theme, window, cx);
        let field = match self.choice {
            Choice::ThisMac => None,
            Choice::Ssh => Some(&self.target),
            Choice::Server => Some(&self.address),
        }
        .map(|input| {
            div().mt(px(12.0)).child(popover::dialog_field(Input::new(input).into_any_element()))
        });
        let serve_toggle = (self.choice == Choice::Ssh && !running).then(|| {
            let serve = self.serve;
            div()
                .id("add-device-serve")
                .mt(px(8.0))
                .text_size(crate::typography::ui_rems(12.5))
                .text_color(theme.text_muted)
                .cursor_pointer()
                .hover(|s| s.text_color(theme.text))
                .tab_index(0)
                .role(gpui::Role::Button)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.serve = !serve;
                    this.progress = None;
                    cx.notify();
                }))
                .child(if serve {
                    "Install a machine there instead"
                } else {
                    "Set up the server there instead"
                })
        });
        let found = progress.is_none().then(|| self.render_found(&theme, cx)).flatten();
        let steps = progress.as_ref().map(|p| self.render_steps(&theme, p, cx));
        let failure_block = failure.as_ref().map(|f| self.render_failure(&theme, f, cx));
        let ask = self.render_ask(&theme, cx);
        let added_line = added.clone().map(|added| {
            div()
                .mt(px(14.0))
                .flex()
                .flex_row()
                .items_start()
                .gap(px(8.0))
                .text_size(crate::typography::ui_rems(13.0))
                .text_color(theme.text)
                .child(
                    div().flex_none().mt(px(2.0)).child(
                        crate::icons::icon(crate::icons::CHECK)
                            .size(px(14.0))
                            .text_color(theme.success_muted),
                    ),
                )
                .child(div().min_w_0().child(added))
        });
        let note = self.note.clone().map(|note| {
            div()
                .mt(px(8.0))
                .text_size(crate::typography::ui_rems(12.5))
                .text_color(theme.danger_muted)
                .child(note)
        });
        let (primary, quiet): (Option<&str>, &str) = if running {
            (None, "Stop")
        } else if added.is_some() {
            (Some("Done"), "Close")
        } else if let Some(asks) = self.asks_key_or_password() {
            (Some(if asks.trust.is_some() { "Trust and continue" } else { "Sign in" }), "Cancel")
        } else if self.asks() {
            (None, "Cancel")
        } else if failure.is_some() {
            (Some("Try again"), "Cancel")
        } else {
            (
                Some(match self.choice {
                    Choice::ThisMac => "Use this Mac",
                    Choice::Ssh if self.serve => "Set up",
                    Choice::Ssh => "Install",
                    Choice::Server => "Connect",
                }),
                "Cancel",
            )
        };
        popover::dialog_card(&theme)
            .w(px(460.0))
            .id("add-device-card")
            .role(gpui::Role::Dialog)
            .aria_label("Add device")
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    this.close(window, cx);
                    cx.stop_propagation();
                }
            }))
            .child(popover::dialog_title(&theme, "Add device"))
            .child(tabs)
            .child(div().mt(px(12.0)).child(popover::dialog_body(&theme, self.choice.blurb())))
            .children(field)
            .children(serve_toggle)
            .children(found)
            .children(steps)
            .children(failure_block)
            .children(ask)
            .children(note)
            .children(added_line)
            .child(
                div()
                    .mt(px(16.0))
                    .flex()
                    .flex_row()
                    .justify_end()
                    .gap(px(8.0))
                    .child(
                        widgets::text_action(&theme, widgets::ActionTone::Quiet, quiet)
                            .id("add-device-cancel")
                            .tab_index(0)
                            .role(gpui::Role::Button)
                            .focus_visible(|s| s.border_2().border_color(theme.accent).opacity(1.0))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                if !running {
                                    return this.close(window, cx);
                                }
                                this.run = None;
                                this.open = false;
                                this.note = Some("Stopped. Start again to go on.".into());
                                cx.notify();
                            })),
                    )
                    .children(primary.map(|label| {
                        widgets::text_action(&theme, widgets::ActionTone::Solid, label)
                            .id("add-device-go")
                            .tab_index(0)
                            .role(gpui::Role::Button)
                            .focus_visible(|s| s.border_2().border_color(theme.accent).opacity(1.0))
                            .on_click(cx.listener(|this, _, window, cx| this.go(window, cx)))
                    })),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A frame of this Mac's checklist, as the view model sends it, reads whole: a line that
    /// needs the person with its fix, and the servers to choose between.
    #[test]
    fn a_checklist_frame_reads_with_its_fix_and_choices() {
        let frame = serde_json::json!({
            "run": 3,
            "steps": [
                { "title": "Screen Recording", "detail": "Not granted", "mark": "needsYou",
                  "fix": { "id": "grant-screen", "label": "Open Settings" } },
                { "title": "Installed", "detail": null, "mark": "done", "fix": null },
            ],
            "share": null,
            "failure": null,
            "choices": [{ "name": "studio", "at": "100.64.0.1:7400" }],
            "added": null,
        });
        let progress: Progress = serde_json::from_value(frame).unwrap();
        assert_eq!(progress.run, 3);
        assert_eq!(progress.steps[0].mark, Mark::NeedsYou);
        assert_eq!(progress.steps[0].fix.as_ref().map(|f| f.id.as_str()), Some("grant-screen"));
        assert_eq!(progress.choices[0].at, "100.64.0.1:7400");
    }

    #[test]
    fn only_a_mac_shares_itself_or_reaches_others_over_ssh() {
        assert!(Choice::offered().contains(&Choice::Server));
        assert_eq!(Choice::offered().contains(&Choice::ThisMac), cfg!(target_os = "macos"));
    }
}
