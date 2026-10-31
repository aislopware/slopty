//! Settings → Devices' Add device dialog.
//!
//! It adds this Mac shared, a machine installed over ssh (or the server set up there), or the
//! server this app follows. The app runs each (`AddDevice`); the dialog draws its steps, the host
//! key or password a run stops to ask for, this Mac's checklist with each line's fix, the servers
//! to choose between, and what was added. A look on the tailnet (`LookOnTailnet`) offers the
//! servers and machines that answer.

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Context, Entity, EventEmitter, Focusable as _, SharedString, Subscription,
    Task, Window, div, px,
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
    const fn offered() -> &'static [Self] {
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
#[derive(Debug, Clone, Copy)]
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
    run: Option<Task<anyhow::Result<()>>>,
    answer: Option<Task<anyhow::Result<()>>>,
    found: Option<Found>,
    look: Option<Task<anyhow::Result<()>>>,
    /// What had the keyboard before the dialog took it, given back as it closes.
    restore: Option<gpui::FocusHandle>,
    /// The card's own: it holds the keyboard while a run goes and the button that started the
    /// run is gone from it.
    focus: gpui::FocusHandle,
    /// The dialog's button (Go, Try again, Sign in, Done).
    go_focus: gpui::FocusHandle,
    /// The footer's quiet button (Cancel, Stop, Close), drawn in every state.
    cancel_focus: gpui::FocusHandle,
    _events: [Subscription; 3],
    /// Escape closes the dialog when the keyboard fell out of the card.
    _escape: Subscription,
}

impl EventEmitter<Closed> for AddDeviceDialog {}

impl std::fmt::Debug for AddDeviceDialog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AddDeviceDialog")
            .field("choice", &self.choice)
            .field("serve", &self.serve)
            .field("open", &self.open)
            .field("note", &self.note)
            .finish_non_exhaustive()
    }
}

impl AddDeviceDialog {
    /// The dialog on `choice` (the first offered when this device does not offer it), its field
    /// with the keyboard.
    pub fn new(
        state: Entity<AppState>,
        choice: Choice,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let choice = if Choice::offered().contains(&choice) {
            choice
        } else {
            Choice::offered().first().copied().unwrap_or(choice)
        };
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
        let ours = window.window_handle();
        let this = cx.weak_entity();
        let escape = cx.intercept_keystrokes(move |event, window, cx| {
            if event.keystroke.key != "escape" || window.window_handle() != ours {
                return;
            }
            let Some(this) = this.upgrade() else { return };
            if this.read(cx).keyboard_fell_out(window, cx) {
                this.update(cx, |dialog, cx| dialog.close(window, cx));
                cx.stop_propagation();
            }
        });
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
            focus: cx.focus_handle(),
            go_focus: cx.focus_handle().tab_index(0).tab_stop(true),
            cancel_focus: cx.focus_handle().tab_index(0).tab_stop(true),
            _events: events,
            _escape: escape,
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

    fn look_on_tailnet(&mut self, cx: &Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else { return };
        self.look = Some(cx.spawn(async move |this, cx| {
            let subscribed = engine
                .client()
                .subscribe_checked(methods::LOOK_ON_TAILNET, serde_json::json!({}))
                .await;
            let Ok(mut stream) = subscribed else { return Ok(()) };
            while let Some(value) = stream.recv().await {
                let Ok(found) = serde_json::from_value::<Found>(value) else { continue };
                this.update(cx, |dialog, cx| {
                    dialog.found = Some(found);
                    cx.notify();
                })?;
            }
            Ok(())
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

    /// The keyboard is where nothing hears Escape for the dialog while its card shows: on
    /// nothing, or on one of the dialog's own handles the card no longer draws. Inside the card
    /// its own listener hears it, and elsewhere in Settings the page does.
    fn keyboard_fell_out(&self, window: &Window, cx: &App) -> bool {
        // The card was drawn in the last frame: its Cancel is, in every state.
        if !self.focus.contains(&self.cancel_focus, window) {
            return false;
        }
        let Some(focused) = window.focused(cx) else { return true };
        if self.focus.contains(&focused, window) {
            return false;
        }
        // The field of the choice on show is drawn in this frame, though the last frame
        // drew the other choice's: a tab just chosen gave it the keyboard, which stays.
        if self.shown_field().is_some_and(|field| field.focus_handle(cx) == focused) {
            return false;
        }
        [
            self.go_focus.clone(),
            self.cancel_focus.clone(),
            self.target.focus_handle(cx),
            self.address.focus_handle(cx),
            self.password.focus_handle(cx),
        ]
        .contains(&focused)
    }

    /// The field the choice on show draws: the machine's for Over SSH, the server's for A
    /// server, none for this Mac.
    const fn shown_field(&self) -> Option<&Entity<InputState>> {
        match self.choice {
            Choice::ThisMac => None,
            Choice::Ssh => Some(&self.target),
            Choice::Server => Some(&self.address),
        }
    }

    /// The card takes the keyboard as a run starts or goes on: the button or field that started
    /// it leaves the card, and what the run asks next, or the button once it is back, takes the
    /// keyboard from the card as it draws.
    fn hold_keyboard(&self, window: &mut Window, cx: &mut App) {
        window.focus(&self.focus, cx);
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
            return self.answer(params, window, cx);
        }
        let (kind, typed) = match self.choice {
            Choice::ThisMac => ("thisMac", String::new()),
            Choice::Ssh => (
                if self.serve { "serveSsh" } else { "ssh" },
                self.target.read(cx).value().to_string(),
            ),
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
        self.start(kind, typed, window, cx);
    }

    /// Start a run of `kind` against `target`, following its stream until it ends.
    fn start(
        &mut self,
        kind: &'static str,
        target: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.hold_keyboard(window, cx);
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
                    return this.update(cx, |dialog, cx| {
                        dialog.open = false;
                        dialog.note = Some(err.to_string().into());
                        cx.notify();
                    });
                }
            };
            while let Some(value) = stream.recv().await {
                let Ok(progress) = serde_json::from_value::<Progress>(value) else { continue };
                this.update(cx, |dialog, cx| {
                    dialog.progress = Some(progress);
                    cx.notify();
                })?;
            }
            this.update(cx, |dialog, cx| {
                dialog.open = false;
                cx.notify();
            })
        }));
        cx.notify();
    }

    /// Send the person's answer to the run that asked; it goes on in its own stream.
    fn answer(&mut self, params: serde_json::Value, window: &mut Window, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else { return };
        self.hold_keyboard(window, cx);
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
        }));
        cx.notify();
    }

    /// A step's fix, or a server chosen, for the run that asked.
    fn answer_with(
        &mut self,
        key: &'static str,
        value: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let run = self.progress.as_ref().map_or(0, |p| p.run);
        self.answer(serde_json::json!({ "run": run, key: value }), window, cx);
    }

    /// A node the tailnet offered, pressed: a server is connected to, a machine installed on.
    fn press_found(
        &mut self,
        server: bool,
        at: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (field, choice) =
            if server { (&self.address, Choice::Server) } else { (&self.target, Choice::Ssh) };
        let field = field.clone();
        field.update(cx, |input, cx| input.set_value(at, window, cx));
        if self.choice != choice {
            self.choose(choice, window, cx);
        }
        self.go(window, cx);
    }

    fn render_tabs(&self, theme: &Theme, window: &mut Window, cx: &Context<Self>) -> AnyElement {
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
            let t =
                widgets::tab_selection_t(window, format!("add-device-tab-{ix}"), selected, reduced);
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
                // The shell's keyboard ring (M8).
                .map(|el| crate::surface_chrome::focus_ring(el, theme))
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, window, cx| this.choose(choice, window, cx)))
                .child(choice.label()),
            );
        }
        row.into_any_element()
    }

    /// The nodes the tailnet offered for the choice on show: servers to connect to, machines to
    /// install on.
    fn render_found(&self, theme: &Theme, cx: &Context<Self>) -> Option<AnyElement> {
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
            Some(if server {
                "No Slopty server found on your tailnet yet."
            } else {
                "No other machine with Slopty found on your tailnet."
            })
        } else {
            None
        };
        // Its label and words start on the paragraph's edge above them, one left edge
        // down the dialog; the rows keep their own inset, as a picker's do.
        let mut list = div().mt(px(12.0)).flex().flex_col().gap(px(2.0)).child(
            widgets::section_label(theme, "On your tailnet")
                .px(px(0.0))
                .debug_selector(|| "add-device-found-label".to_owned()),
        );
        if let Some(words) = words {
            // What the look found, read out as it changes.
            list = list.child(
                div()
                    .id("add-device-found-words")
                    .role(gpui::Role::Status)
                    .aria_label(words)
                    .debug_selector(|| "add-device-found-words".to_owned())
                    .text_size(crate::typography::ui_rems(12.5))
                    .text_color(theme.text_muted)
                    .child(words),
            );
        }
        for (ix, node) in nodes.iter().enumerate() {
            let at = node.at.clone();
            let detail = node.note.clone().unwrap_or_else(|| node.at.clone());
            let glyph = if server { crate::icons::REMOTE_SERVER } else { crate::icons::MONITOR };
            // zeron's picker row: the whole row acts, its hover fading in; a node that cannot be
            // joined says why and does not act.
            let mut row =
                popover::picker_row(theme, false, false, format!("add-device-found-{ix}"))
                    .id(("add-device-found", ix))
                    .debug_selector(move || format!("add-device-found-{ix}"))
                    .child(popover::picker_icon_slot(
                        crate::icons::icon(glyph)
                            .size(px(14.0))
                            .text_color(theme.text_muted)
                            .into_any_element(),
                    ))
                    .child(div().flex_none().max_w(px(180.0)).truncate().child(node.name.clone()))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(crate::typography::ui_rems(12.0))
                            .text_color(theme.text_muted)
                            .child(detail),
                    );
            if node.ready {
                row = row
                    .tab_index(0)
                    // The shell's keyboard ring, inside the row (M8).
                    .map(|el| crate::surface_chrome::focus_ring_inset(el, theme))
                    .role(gpui::Role::Button)
                    .aria_label(format!(
                        "{} {}",
                        if server { "Connect to" } else { "Install on" },
                        node.name
                    ))
                    .child(
                        div()
                            .flex_none()
                            .text_size(crate::typography::ui_rems(12.0))
                            .text_color(theme.text_muted)
                            .child(if server { "Connect" } else { "Install" }),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.press_found(server, at.clone(), window, cx);
                    }));
            } else {
                row = row.cursor_default().opacity(crate::theme::DISABLED_OPACITY);
            }
            list = list.child(row);
        }
        Some(list.into_any_element())
    }

    fn render_steps(&self, theme: &Theme, progress: &Progress, cx: &Context<Self>) -> AnyElement {
        let waits = self.open;
        let mut list = div().mt(px(16.0)).flex().flex_col().gap(px(2.0));
        for (ix, step) in progress.steps.iter().enumerate() {
            // One mark morphs from working to done, failed or waiting on the person
            // (`loaders::status_mark`), where a spinner, a check and a cross cut.
            let status_mark = |state| {
                crate::loaders::status_mark::StatusMark::new(
                    SharedString::from(format!("add-device-step-{ix}")),
                    state,
                )
                .size(14.0)
                .ground(popover::surface_bg(theme))
                .into_any_element()
            };
            let mark = match step.mark {
                Mark::Done => status_mark(crate::loaders::status_mark::MarkState::Done),
                Mark::Failed => status_mark(crate::loaders::status_mark::MarkState::Failed),
                Mark::NeedsYou => status_mark(crate::loaders::status_mark::MarkState::Waiting),
                // The run's share, on the step under way.
                Mark::Running if share_on(&progress.steps, progress.share) == Some(ix) => {
                    let percent = progress.share.map_or(0, share_percent);
                    crate::loaders::upload_progress_arc(
                        percent,
                        14.0,
                        theme.text_muted.opacity(0.25),
                        theme.text,
                    )
                }
                Mark::Running => status_mark(crate::loaders::status_mark::MarkState::Working),
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
                    // The shell's keyboard ring (M8).
                    .map(|el| crate::surface_chrome::focus_ring(el, theme))
                    .role(gpui::Role::Button)
                    .aria_label(fix.label)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.answer_with("fix", &id, window, cx);
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
                            .children(step.detail.clone().filter(|d| !d.is_empty()).map(
                                |detail| {
                                    div()
                                        .text_size(crate::typography::ui_rems(12.0))
                                        .text_color(theme.text_muted)
                                        .child(detail)
                                },
                            )),
                    )
                    .children(fix),
            );
        }
        for (ix, choose) in progress.choices.iter().enumerate().filter(|_| waits) {
            let at = choose.at.clone();
            list = list.child(
                popover::picker_row(theme, false, false, format!("add-device-choose-{ix}"))
                    .id(("add-device-choose", ix))
                    .debug_selector(move || format!("add-device-choose-{ix}"))
                    .tab_index(0)
                    // The shell's keyboard ring, inside the row (M8).
                    .map(|el| crate::surface_chrome::focus_ring_inset(el, theme))
                    .role(gpui::Role::Button)
                    .aria_label(format!("Join {}", choose.name))
                    .child(popover::picker_icon_slot(
                        crate::icons::icon(crate::icons::REMOTE_SERVER)
                            .size(px(14.0))
                            .text_color(theme.text_muted)
                            .into_any_element(),
                    ))
                    .child(div().flex_none().max_w(px(180.0)).truncate().child(choose.name.clone()))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(crate::typography::ui_rems(12.0))
                            .text_color(theme.text_muted)
                            .child(choose.at.clone()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(crate::typography::ui_rems(12.0))
                            .text_color(theme.text_muted)
                            .child("Join"),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.answer_with("choose", &at, window, cx);
                    })),
            );
        }
        list.into_any_element()
    }

    fn render_failure(theme: &Theme, failure: &Failure, cx: &App) -> AnyElement {
        let mono = crate::typography::code_effective_family_name(cx);
        let mut details: Vec<AnyElement> = Vec::new();
        if let Some(hint) = failure.hint.clone() {
            details.push(div().text_color(theme.text_muted).child(hint).into_any_element());
        }
        for line in failure.lines.iter().take(8) {
            details.push(
                div()
                    .font_family(mono.clone())
                    .text_size(crate::typography::ui_rems(11.5))
                    .text_color(theme.text_muted)
                    .child(line.clone())
                    .into_any_element(),
            );
        }
        widgets::error_strip_with(theme, failure.title.clone(), details)
            .mt(px(14.0))
            .id("add-device-failure")
            .debug_selector(|| "add-device-failure".into())
            .role(gpui::Role::Alert)
            .aria_label(failure.title.clone())
            .into_any_element()
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
            block = block.child(popover::dialog_body(theme, said)).child(text_field(
                &self.password,
                "Password",
                true,
                cx,
            ));
        }
        Some(block.into_any_element())
    }
}

impl Render for AddDeviceDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).for_popup();
        // gpui-base's Input is unstyled: its caret and selection are the shell's, as rename's.
        let style = crate::files::editor_adapter::editor_style(&theme);
        for input in [&self.target, &self.address, &self.password] {
            input.update(cx, |input, _| input.set_editor_style(style.clone()));
        }
        let running = self.running();
        let progress = self.progress.clone();
        let added = self.added().map(str::to_owned);
        let failure = progress.as_ref().and_then(|p| p.failure.clone()).filter(|_| added.is_none());
        let tabs = self.render_tabs(&theme, window, cx);
        let field = self.shown_field().map(|input| {
            let label = match self.choice {
                Choice::Server => "Server's name or address",
                _ => "Machine's name or address",
            };
            div().mt(px(12.0)).child(text_field(input, label, false, cx))
        });
        let serve_toggle = (self.choice == Choice::Ssh && !running).then(|| {
            let serve = self.serve;
            let label = if serve {
                "Install a machine there instead"
            } else {
                "Set up the server there instead"
            };
            // A link in the accent, underlined under the pointer, its words on the
            // body's edge: muted words alone did not read as something to press.
            div().mt(px(8.0)).flex().child(
                div()
                    .id("add-device-serve")
                    .tab_index(0)
                    .role(gpui::Role::Link)
                    .aria_label(label)
                    .rounded(px(4.0))
                    .cursor_pointer()
                    .text_size(crate::typography::ui_rems(12.5))
                    .text_color(theme.accent)
                    .hover(Styled::underline)
                    .map(|el| crate::surface_chrome::focus_ring(el, &theme))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.serve = !serve;
                        this.progress = None;
                        cx.notify();
                    }))
                    .child(label),
            )
        });
        let found = progress.is_none().then(|| self.render_found(&theme, cx)).flatten();
        let steps = progress.as_ref().map(|p| self.render_steps(&theme, p, cx));
        let failure_block = failure.as_ref().map(|f| Self::render_failure(&theme, f, cx));
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
        // The keyboard stays in the card. One that fell out of it (a button or field it held
        // went) comes back to the card, and the card hands it on to the password a run asks for
        // or the button once it is back: Try again after a failure. A host key to trust is read
        // first, so its button waits for a press.
        if self.keyboard_fell_out(window, cx) {
            self.hold_keyboard(window, cx);
        }
        if self.focus.is_focused(window) {
            let asks = self.asks_key_or_password();
            let next = if asks.is_some_and(|asks| asks.password.is_some()) {
                Some(self.password.focus_handle(cx))
            } else if asks.is_none() && primary.is_some() {
                Some(self.go_focus.clone())
            } else {
                None
            };
            if let Some(next) = next {
                window.focus(&next, cx);
            }
        }
        popover::dialog_card(&theme)
            .w(px(460.0))
            .id("add-device-card")
            .track_focus(&self.focus)
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
            .child(
                div()
                    .mt(px(12.0))
                    .debug_selector(|| "add-device-blurb".to_owned())
                    .child(popover::dialog_body(&theme, self.choice.blurb())),
            )
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
                            .track_focus(&self.cancel_focus)
                            .role(gpui::Role::Button)
                            .aria_label(quiet)
                            .map(|el| crate::surface_chrome::focus_ring_revealing(el, &theme))
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
                            .track_focus(&self.go_focus)
                            .role(gpui::Role::Button)
                            .aria_label(label) // named in the accessibility tree.
                            .map(|el| crate::surface_chrome::focus_ring_revealing(el, &theme))
                            .on_click(cx.listener(|this, _, window, cx| this.go(window, cx)))
                    })),
            )
    }
}

// gpui-base's Input draws no accessibility node; its frame here is the node a screen
// reader reads, and the keyboard on the field inside is announced on it.
/// One of the dialog's fields in the dialog's frame, named for a screen reader and saying its
/// words, the password's never.
fn text_field(
    input: &Entity<InputState>,
    label: &'static str,
    masked: bool,
    cx: &App,
) -> AnyElement {
    let value = input.read(cx).value();
    div()
        .id(label)
        .role(if masked { gpui::Role::PasswordInput } else { gpui::Role::TextInput })
        .aria_label(label)
        .when(!masked && !value.is_empty(), |el| el.aria_value(value))
        .child(popover::dialog_field(Input::new(input).into_any_element()))
        .into_any_element()
}

/// The step the run's share is drawn on: the first under way.
fn share_on(steps: &[Step], share: Option<f32>) -> Option<usize> {
    share.and_then(|_| steps.iter().position(|step| step.mark == Mark::Running))
}

/// A share of 0 to 1 as a whole percent, a share out of range held to it.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "clamped to 0..=1 and scaled, the rounded percent is a whole 0..=100"
)]
fn share_percent(share: f32) -> u8 {
    (share.clamp(0.0, 1.0) * 100.0).round() as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(mark: Mark) -> Step {
        Step { title: "Copy".to_owned(), detail: None, mark, fix: None }
    }

    fn node(name: &str, at: &str, ready: bool) -> Node {
        let note = (!ready).then(|| "Runs an older build".to_owned());
        Node { name: name.to_owned(), at: at.to_owned(), ready, note }
    }

    fn dialog(
        cx: &mut gpui::TestAppContext,
    ) -> (Entity<AddDeviceDialog>, &mut gpui::VisualTestContext) {
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::dark());
        });
        let (dialog, cx) = cx.add_window_view(|window, cx| {
            let state = cx.new(|_| AppState::new());
            AddDeviceDialog::new(state, Choice::Server, window, cx)
        });
        cx.update(|window, _| window.activate_window());
        (dialog, cx)
    }

    /// A server the tailnet offers is one picker row, pressed anywhere along it; one that cannot
    /// be joined says why and does not act.
    #[gpui::test]
    fn a_found_server_is_a_row_pressed_anywhere(cx: &mut gpui::TestAppContext) {
        let (dialog, cx) = dialog(cx);
        dialog.update(cx, |dialog, cx| {
            dialog.found = Some(Found {
                looking: false,
                running: true,
                servers: vec![node("studio", "100.64.0.1", true), node("old", "100.64.0.2", false)],
                workers: Vec::new(),
            });
            cx.notify();
        });
        cx.run_until_parked();
        let row = cx.debug_bounds("add-device-found-0").expect("the server's row");
        assert!(f32::from(row.size.height) >= popover::PICKER_ROW_HEIGHT - 0.5, "{row:?}");
        let refused = cx.debug_bounds("add-device-found-1").expect("the older server's row");
        cx.simulate_click(refused.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        let typed = dialog.read_with(cx, |dialog, cx| dialog.address.read(cx).value().to_string());
        assert_eq!(typed, "", "a node that cannot be joined does not act");
        // Near its left edge, far from any word that says what it does.
        let at = gpui::point(row.origin.x + px(4.0), row.center().y);
        cx.simulate_click(at, gpui::Modifiers::default());
        cx.run_until_parked();
        let typed = dialog.read_with(cx, |dialog, cx| dialog.address.read(cx).value().to_string());
        assert_eq!(typed, "100.64.0.1");
    }

    /// The server step's paragraph, its "On your tailnet" label and the words under it share
    /// one left edge.
    #[gpui::test]
    fn the_server_step_keeps_one_left_edge(cx: &mut gpui::TestAppContext) {
        let (dialog, cx) = dialog(cx);
        dialog.update(cx, |dialog, cx| {
            dialog.found = Some(Found {
                looking: false,
                running: true,
                servers: Vec::new(),
                workers: Vec::new(),
            });
            cx.notify();
        });
        cx.run_until_parked();
        let mut left = |name: &'static str| {
            cx.debug_bounds(name).map_or_else(|| panic!("{name}"), |b| f32::from(b.origin.x))
        };
        let blurb = left("add-device-blurb");
        let label = left("add-device-found-label");
        assert!((label - blurb).abs() < 0.5, "the label on the paragraph's edge: {label} {blurb}");
        let words = left("add-device-found-words");
        assert!((words - blurb).abs() < 0.5, "the words on the paragraph's edge: {words} {blurb}");
    }

    /// A run that failed shows the strip with its title, its hint and its output.
    #[gpui::test]
    fn a_failure_is_the_error_strip(cx: &mut gpui::TestAppContext) {
        let (dialog, cx) = dialog(cx);
        dialog.update(cx, |dialog, cx| {
            dialog.progress = Some(Progress {
                failure: Some(Failure {
                    title: "Could not reach nohost".to_owned(),
                    hint: Some("Check its name".to_owned()),
                    lines: vec!["ssh: Could not resolve hostname nohost".to_owned()],
                    ..Failure::default()
                }),
                ..Progress::default()
            });
            cx.notify();
        });
        cx.run_until_parked();
        let strip = cx.debug_bounds("add-device-failure").expect("the error strip");
        assert!(f32::from(strip.size.height) > 40.0, "title, hint and output: {strip:?}");
    }

    /// The focused node of the window's accessibility tree, as a screen reader names it.
    fn focused(cx: &mut gpui::VisualTestContext) -> (gpui::Role, String) {
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.set_a11y_active(true);
            window.refresh();
            window.draw(cx).clear(cx);
            let update = window.a11y_tree().expect("a tree");
            let node = update
                .nodes
                .iter()
                .find(|(id, _)| *id == update.focus)
                .map(|(_, node)| node)
                .expect("the focused node");
            (node.role(), node.label().unwrap_or_default().to_owned())
        })
    }

    /// Install pressed hands the keyboard to the card while the run goes and its button is
    /// gone; a run that failed hands it to Try again, where Return tries again and Escape
    /// closes, never to the window.
    #[gpui::test]
    fn a_failed_run_leaves_the_keyboard_on_try_again(cx: &mut gpui::TestAppContext) {
        let (dialog, cx) = dialog(cx);
        cx.run_until_parked();
        dialog.update_in(cx, |dialog, window, cx| {
            dialog.address.update(cx, |input, cx| input.set_value("nohost", window, cx));
            window.focus(&dialog.go_focus, cx);
            dialog.go(window, cx);
            // What the run's stream brings: under way, then failed.
            dialog.open = true;
            dialog.progress =
                Some(Progress { steps: vec![step(Mark::Running)], ..Progress::default() });
            cx.notify();
        });
        assert_eq!(focused(cx), (gpui::Role::Dialog, "Add device".to_owned()), "the card holds it");
        dialog.update(cx, |dialog, cx| {
            dialog.open = false;
            dialog.progress = Some(Progress {
                steps: vec![step(Mark::Failed)],
                failure: Some(Failure {
                    title: "Could not reach nohost".to_owned(),
                    ..Failure::default()
                }),
                ..Progress::default()
            });
            cx.notify();
        });
        assert_eq!(focused(cx), (gpui::Role::Button, "Try again".to_owned()));
        dialog.update_in(cx, |dialog, window, cx| {
            assert!(dialog.go_focus.is_focused(window));
            assert!(dialog.focus.contains_focused(window, cx));
        });
    }

    /// Choosing Over SSH from A server gives the machine's field the keyboard, and a screen
    /// reader reads it as a text field with what is typed in it; the password says no words.
    #[gpui::test]
    fn a_choice_chosen_gives_its_field_the_keyboard(cx: &mut gpui::TestAppContext) {
        let (dialog, cx) = dialog(cx);
        assert_eq!(focused(cx), (gpui::Role::TextInput, "Server's name or address".to_owned()));
        dialog.update_in(cx, |dialog, window, cx| dialog.choose(Choice::Ssh, window, cx));
        assert_eq!(focused(cx), (gpui::Role::TextInput, "Machine's name or address".to_owned()));
        cx.simulate_input("nohost");
        focused(cx);
        let typed = dialog.read_with(cx, |dialog, cx| dialog.target.read(cx).value().to_string());
        assert_eq!(typed, "nohost", "the keys land in the field");
        let value = cx.update(|window, _| {
            let update = window.a11y_tree().expect("a tree");
            update
                .nodes
                .iter()
                .find(|(id, _)| *id == update.focus)
                .and_then(|(_, node)| node.value().map(str::to_owned))
        });
        assert_eq!(value.as_deref(), Some("nohost"));
        dialog.update_in(cx, |dialog, window, cx| {
            assert!(dialog.target.focus_handle(cx).is_focused(window), "not moved to Install");
        });
    }

    /// Counts the times `dialog` says it is done.
    fn closes(
        dialog: &Entity<AddDeviceDialog>,
        cx: &mut gpui::VisualTestContext,
    ) -> std::rc::Rc<std::cell::Cell<u32>> {
        let closed = std::rc::Rc::new(std::cell::Cell::new(0_u32));
        let seen = std::rc::Rc::clone(&closed);
        cx.update(|_, cx| {
            cx.subscribe(dialog, move |_, _: &Closed, _| seen.set(seen.get().saturating_add(1)))
                .detach();
        });
        closed
    }

    /// Escape closes the dialog with the keyboard on nothing, where a button that held it and
    /// went leaves it.
    #[gpui::test]
    fn escape_closes_the_dialog_with_the_keyboard_on_nothing(cx: &mut gpui::TestAppContext) {
        let (dialog, cx) = dialog(cx);
        let closed = closes(&dialog, cx);
        focused(cx);
        cx.update(Window::blur);
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert_eq!(closed.get(), 1);
    }

    /// Escape closes the dialog with the keyboard on the card's own button once the card no
    /// longer draws it: Go, while a run goes.
    #[gpui::test]
    fn escape_closes_the_dialog_with_the_keyboard_on_a_button_gone(cx: &mut gpui::TestAppContext) {
        let (dialog, cx) = dialog(cx);
        let closed = closes(&dialog, cx);
        focused(cx);
        dialog.update_in(cx, |dialog, window, cx| {
            window.focus(&dialog.go_focus, cx);
            dialog.open = true;
            dialog.progress =
                Some(Progress { steps: vec![step(Mark::Running)], ..Progress::default() });
            cx.notify();
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert_eq!(closed.get(), 1);
    }

    /// The footer's buttons are named for a screen reader: Cancel as well as the one that goes.
    #[gpui::test]
    fn the_footers_buttons_are_named(cx: &mut gpui::TestAppContext) {
        let (_dialog, cx) = dialog(cx);
        cx.run_until_parked();
        let buttons = cx.update(|window, cx| {
            window.set_a11y_active(true);
            window.refresh();
            window.draw(cx).clear(cx);
            let update = window.a11y_tree().expect("a tree");
            update
                .nodes
                .iter()
                .filter(|(_, node)| node.role() == gpui::Role::Button)
                .map(|(_, node)| node.label().unwrap_or_default().to_owned())
                .collect::<Vec<_>>()
        });
        assert!(buttons.iter().any(|b| b == "Cancel"), "{buttons:?}");
        assert!(buttons.iter().any(|b| b == "Connect"), "{buttons:?}");
        assert!(!buttons.iter().any(String::is_empty), "no nameless button: {buttons:?}");
    }

    /// The run's share rides the step under way, not a bar of its own; with no step under way,
    /// or no share, it is drawn nowhere.
    #[test]
    fn the_share_is_drawn_on_the_running_step() {
        let steps = [step(Mark::Done), step(Mark::Running), step(Mark::Waiting)];
        assert_eq!(share_on(&steps, Some(0.4)), Some(1));
        assert_eq!(share_on(&steps, None), None);
        assert_eq!(share_on(&[step(Mark::Done), step(Mark::Failed)], Some(0.4)), None);
        assert_eq!(share_percent(0.426), 43);
        assert_eq!(share_percent(1.7), 100, "held to whole");
        assert_eq!(share_percent(-0.2), 0);
        assert_eq!(share_percent(f32::NAN), 0);
    }

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
