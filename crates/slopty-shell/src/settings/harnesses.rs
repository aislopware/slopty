// Derived from zeron (MIT, github.com/zeronsh/zeron @ daf047b), see THIRD_PARTY_NOTICES.md
//! Settings → Providers: install and enable harnesses, one card row per agent.
//!
//! The state is PER-DEVICE and lives on the engine (`harness-prefs.json` in
//! its data dir): CLI installs are per-device, so enablement is too. The
//! page-header device switcher (the Accounts pattern) retargets both the
//! `ListHarnesses` probe and the `SetHarnessEnabled` writes at any registered
//! device over the relay-forwarded RPCs.
//!
//! Enablement follows DETECTION: every harness whose CLI probe passes is on
//! unless the user switched it off, so installing an agent is all it takes
//! for it to appear here and in the composer. A harness whose CLI is missing
//! on the target device renders dimmed with an install hint and is never
//! enabled (enabling an agent that can't run would only manufacture
//! NotInstalled errors at send time); an ENABLED agent can always be turned
//! OFF except the last one standing — the composer needs something to run.
//! Catalogs from engines predating the detection model can still stamp
//! enabled-but-uninstalled rows; the hint covers that too. The engine
//! enforces the same gates where the state lives, so a raced or stale toggle
//! self-corrects from the RPC reply.

use gpui::prelude::*;
use gpui::{AnyElement, Context, Entity, IntoElement, Render, SharedString, Task, Window, div, px};
use slopty_view_proto::HarnessId;
use slopty_view_rpc::methods;

use crate::catalog::registry::{HarnessDescriptor, descriptor_enabled};
use crate::pickers::visible_harnesses;
use crate::popover::{self, Loadable};
use crate::settings::widgets;
use crate::state::AppState;
use crate::theme::Theme;

/// Left inset of what opens under a provider (its installer): the row's padding, brand tile and
/// gap, so it starts on the title's edge.
const DETAILS_INSET: f32 = 4.0 + 36.0 + 12.0;

// How freely agents act (`[agents] autonomy`): the value, its label, and the note under
// the row title for the chosen one. The names are Claude Code's own for its permission modes, so
// the choice reads as its TUI says it.
const AUTONOMY: [(&str, &str, &str); 3] = [
    ("auto", "Auto", "Asks only about what looks risky."),
    ("full", "Bypass permissions", "Asks nothing: agents act without stopping."),
    ("ask", "Manual", "Asks before edits, commands and the network."),
];

/// Where `auto` sits in [`AUTONOMY`]: the default, and the one Slopty recommends.
const AUTONOMY_DEFAULT: usize = 0;
/// Where `full` sits: the one that asks nothing, so its note is a warning.
const AUTONOMY_FULL: usize = 1;

// What an installed agent's row says where nothing else does.
const INSTALLED: &str = "Installed";

fn offers_install(harness: HarnessId, installed: bool, can_install: bool) -> bool {
    harness != HarnessId::MOCK && !installed && can_install
}

fn install_hint(harness: HarnessId, enabled: bool, can_install: bool) -> String {
    if harness == HarnessId::ANTIGRAVITY {
        return if can_install {
            "Install Antigravity to enable"
        } else {
            "Set ANTIGRAVITY_ACP_EXECUTABLE to enable Antigravity"
        }
        .into();
    }
    let hint = if enabled {
        format!("{} CLI not installed — turn it off or install it", cli_name(harness))
    } else {
        format!("Install the {} CLI to enable", cli_name(harness))
    };
    // The command is drawn in its own mono span ([`install_command`]), not quoted.
    if install_command(harness, can_install).is_some() {
        format!("{hint}. Install with")
    } else {
        hint
    }
}

// The command a hint names, set in the code face beside it rather than in backticks.
/// The command the person runs to install `harness`, where Slopty does not install it.
fn install_command(harness: HarnessId, can_install: bool) -> Option<&'static str> {
    if harness == HarnessId::ANTIGRAVITY || can_install {
        return None;
    }
    crate::catalog::harness::install::manual_command(harness)
}

// How tall the installer's terminal stands under its row.
const INSTALL_TERMINAL_HEIGHT: f32 = 240.0;
// How often, and how many times, the agents are listed again after an installer ends.
const RELIST_EVERY: std::time::Duration = std::time::Duration::from_millis(1_500);
const RELIST_TRIES: u32 = 10;

// The installer's terminal, by the agent it installs.
struct InstallRun {
    harness: HarnessId,
    /// The terminal's id (`InstallHarness`'s `terminalId`).
    id: String,
    /// The status it ended with, once it ended.
    ended: Option<i32>,
    _watch: Option<gpui::Subscription>,
}

fn install_label(name: &str) -> String {
    format!("Installing {name}…")
}

fn install_params(harness: HarnessId, target: &Option<String>) -> serde_json::Value {
    serde_json::json!({"harness": harness, "targetDeviceId": target})
}

/// The CLI named in the not-installed hint.
pub fn cli_name(harness: HarnessId) -> &'static str {
    match harness {
        HarnessId::CLAUDE_CODE => "claude",
        HarnessId::CODEX => "codex",
        HarnessId::CURSOR => "cursor-agent",
        HarnessId::DEVIN => "devin",
        HarnessId::GROK => "grok",
        HarnessId::HERMES => "hermes",
        HarnessId::PI => "pi",
        HarnessId::OPENCODE => "opencode",
        HarnessId::ANTIGRAVITY => "Antigravity",
        HarnessId::MOCK => "mock",
        // Any other agent's program is named by its harness.
        other => other.as_str(),
    }
}

pub struct HarnessesPage {
    state: Entity<AppState>,
    scroll: widgets::PageScroll,
    harnesses: Loadable<Vec<HarnessDescriptor>>,
    /// Which device's harnesses are shown/edited; `None` = this device (no
    /// passthrough). Retargeted by the page-header device switcher.
    target_device: Option<String>,
    device_select: widgets::SelectState,
    /// Last refused/failed toggle (engine guards), shown in the error strip.
    error: Option<String>,
    load_task: Option<Task<()>>,
    toggle_task: Option<Task<()>>,
    /// A toggle the engine hasn't answered yet: the switch shows it at once.
    pending_toggle: Option<(HarnessId, bool)>,
    installing: Option<HarnessId>,
    install_task: Option<Task<()>>,
    // The installer's terminal on the target machine, while the page shows it.
    install_run: Option<InstallRun>,

    // The person's one autonomy setting (`[agents] autonomy`), its index in AUTONOMY.
    autonomy: Option<usize>,
    autonomy_select: widgets::SelectState,
    autonomy_task: Option<Task<()>>,
}

impl HarnessesPage {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let mut page = Self {
            state,
            scroll: widgets::PageScroll::default(),
            harnesses: Loadable::Idle,
            target_device: None,
            device_select: widgets::SelectState::default(),
            error: None,
            load_task: None,
            toggle_task: None,
            pending_toggle: None,
            installing: None,
            install_task: None,
            install_run: None,
            autonomy: None,
            autonomy_select: widgets::SelectState::default(),
            autonomy_task: None,
        };
        page.load(cx);
        page
    }

    // Read `[agents] autonomy`, after writing `set` (an index in AUTONOMY) when given.
    fn load_autonomy(
        &mut self,
        engine: crate::state::EngineHandle,
        set: Option<usize>,
        cx: &mut Context<Self>,
    ) {
        let (method, params) = match set.and_then(|ix| AUTONOMY.get(ix)) {
            Some((value, ..)) => {
                (methods::SET_AGENT_SETTINGS, serde_json::json!({ "autonomy": value }))
            }
            None => (methods::GET_AGENT_SETTINGS, serde_json::json!({})),
        };
        if set.is_some() {
            self.autonomy = set;
        }
        self.autonomy_task = Some(cx.spawn(async move |this, cx| {
            let result = engine.client().call(method, params).await;
            this.update(cx, |page, cx| {
                match result {
                    Ok(value) => {
                        let now = value.get("autonomy").and_then(serde_json::Value::as_str);
                        page.autonomy = AUTONOMY.iter().position(|(v, ..)| Some(*v) == now);
                    }
                    Err(error) => page.error = Some(error.to_string()),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    // The Autonomy section under the agents.
    fn render_autonomy(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let selected = self.autonomy.unwrap_or(AUTONOMY_DEFAULT);
        let select = widgets::select("agents-autonomy", "Autonomy", theme, |page: &mut Self| {
            &mut page.autonomy_select
        })
        // Auto is marked the default and the recommended one; Bypass permissions says
        // what it costs (M10).
        .options(
            AUTONOMY.iter().enumerate().map(|(ix, (_, label, _))| {
                let option = widgets::SelectOption::new(*label);
                if ix == AUTONOMY_FULL {
                    option.detail("Asks nothing")
                } else {
                    option.standing(widgets::Standing::of(
                        ix == AUTONOMY_DEFAULT,
                        ix == AUTONOMY_DEFAULT,
                    ))
                }
            }),
            selected,
        )
        .width(136.0)
        .on_select(|page, selected, _, cx| {
            if let Some(engine) = page.state.read(cx).engine().cloned() {
                page.load_autonomy(engine, Some(selected), cx);
            }
        })
        .render(&self.autonomy_select, cx);
        let note = AUTONOMY.get(selected).map_or("", |(.., note)| *note);
        let note_color = if selected == AUTONOMY_FULL { theme.warning } else { theme.text_muted };
        let reset = widgets::reset_action(
            theme,
            "agents-autonomy-reset",
            "How freely agents act",
            selected == AUTONOMY_DEFAULT,
        )
        .map(|reset| {
            reset.on_click(cx.listener(|page, _, _, cx| {
                if let Some(engine) = page.state.read(cx).engine().cloned() {
                    page.load_autonomy(engine, Some(AUTONOMY_DEFAULT), cx);
                }
            }))
        });
        widgets::section(
            theme,
            "Autonomy",
            widgets::section_card(theme).mt_0().child(
                widgets::card_row(theme, true)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(widgets::row_title(theme, "How freely agents act"))
                            .child(widgets::meta_line(
                                theme,
                                vec![div().text_color(note_color).child(note).into_any_element()],
                            )),
                    )
                    .children(reset)
                    .child(select),
            ),
        )
        .into_any_element()
    }

    /// Params with the `targetDeviceId` passthrough merged in.
    fn with_target(&self, mut value: serde_json::Value) -> serde_json::Value {
        if let (Some(target), Some(object)) = (&self.target_device, value.as_object_mut()) {
            object.insert("targetDeviceId".into(), serde_json::json!(target));
        }
        value
    }

    /// Retarget the page at another device: a different device is a different
    /// install/enablement world, so drop the rows and reload through it.

    pub(crate) fn set_target_device(&mut self, target: Option<String>, cx: &mut Context<Self>) {
        widgets::close_select(self, |page: &mut Self| &mut page.device_select, cx);

        if self.target_device == target {
            cx.notify();
            return;
        }
        self.installing = None;
        self.install_task = None;
        self.close_install(cx); // the installer's terminal is the old machine's
        self.target_device = target;
        self.error = None;
        self.harnesses = Loadable::Idle;
        self.load(cx);
        cx.notify();
    }

    /// `ListHarnesses` against the target device (installed probe + enabled
    /// set both come from where the CLIs actually live).
    fn load(&mut self, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        self.load_autonomy(engine.clone(), None, cx);
        let params = self.with_target(serde_json::json!({}));

        self.harnesses = Loadable::Loading;
        self.load_task = Some(cx.spawn(async move |this, cx| {
            let result = engine.client().call(methods::LIST_HARNESSES, params).await;
            this.update(cx, |page, cx| {
                page.harnesses = match result {
                    Ok(value) => match serde_json::from_value::<Vec<HarnessDescriptor>>(value) {
                        Ok(list) => Loadable::Ready(list),
                        Err(err) => Loadable::Error(err.to_string()),
                    },
                    Err(err) => Loadable::Error(err.to_string()),
                };
                cx.notify();
            })
            .ok();
        }));
    }

    /// Flip one harness on the target device. The reply carries the device's
    /// fresh catalog, so the rows repaint from the authoritative state in one
    /// round trip; refusals (engine guards) land in the error strip.
    fn toggle(&mut self, harness: HarnessId, enabled: bool, cx: &mut Context<Self>) {
        self.set_enabled(harness, enabled, cx);
    }

    // Cancel while the installer runs, Close once it ended: its terminal ends and goes.
    fn cancel_install(&mut self, cx: &mut Context<Self>) {
        self.close_install(cx);
        self.installing = None;
        self.install_task = None;
        cx.notify();
    }

    // Install runs the vendor's installer in a terminal on the target machine
    // (`InstallHarness` answers its `terminalId`), which the row shows until the person closes it.
    fn install(&mut self, harness: HarnessId, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        if self.installing.is_some() || self.install_run.is_some() {
            return;
        }
        let params = install_params(harness, &self.target_device);
        let target = self.target_device.clone();
        self.installing = Some(harness);
        self.error = None;
        self.install_task = Some(cx.spawn(async move |this, cx| {
            let result = engine.client().call(methods::INSTALL_HARNESS, params).await;
            this.update(cx, |page, cx| {
                if page.target_device != target || page.installing != Some(harness) {
                    return;
                }
                let terminal = result.map_err(|error| error.to_string()).and_then(|value| {
                    value
                        .get("terminalId")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned)
                        .ok_or_else(|| "its terminal did not open".to_owned())
                });
                match terminal {
                    Ok(id) => page.show_install(harness, id, cx),
                    Err(error) => {
                        page.installing = None;
                        page.error = Some(format!("Couldn't install the agent: {error}"));
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    // Follow the installer's terminal: once it ends, ask the machine again what it has.
    fn show_install(&mut self, harness: HarnessId, id: String, cx: &mut Context<Self>) {
        let page = cx.weak_entity();
        let watch = crate::live::watch(
            &id,
            Box::new(move |cx| {
                let _ = page.update(cx, |page, cx| page.follow_install(cx));
            }),
            cx,
        );
        self.install_run = Some(InstallRun { harness, id, ended: None, _watch: watch });
        self.follow_install(cx);
    }

    // What the installer's terminal says of itself now.
    fn follow_install(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.install_run.as_ref().map(|run| run.id.clone()) else {
            return;
        };
        let look = crate::live::TerminalLook::of(cx);
        let ended = crate::live::terminal(&id, &look, cx)
            .filter(|hosted| hosted.ended)
            .map(|hosted| hosted.exit.unwrap_or(0));
        if let Some(run) = self.install_run.as_mut()
            && run.ended.is_none()
            && let Some(exit) = ended
        {
            run.ended = Some(exit);
            self.installing = None;
            self.install_task = None;
            self.relist(RELIST_TRIES, cx);
        }
        cx.notify();
    }

    // List the machine's agents again until the one installed shows, its worker seeing
    // the install a beat after the installer ends; `tries` more times at most.
    fn relist(&mut self, tries: u32, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let Some(harness) = self.install_run.as_ref().map(|run| run.harness) else {
            return;
        };
        let params = self.with_target(serde_json::json!({}));
        self.load_task = Some(cx.spawn(async move |this, cx| {
            let listed = engine.client().call(methods::LIST_HARNESSES, params).await;
            let list = listed
                .ok()
                .and_then(|value| serde_json::from_value::<Vec<HarnessDescriptor>>(value).ok());
            let arrived = list
                .as_ref()
                .is_some_and(|list| list.iter().any(|d| d.id == harness && d.installed));
            let again = this
                .update(cx, |page, cx| {
                    if let Some(list) = list {
                        page.harnesses = Loadable::Ready(list);
                    }
                    cx.notify();
                    !arrived && tries > 0 && page.install_run.is_some()
                })
                .unwrap_or(false);
            if again {
                cx.background_executor().timer(RELIST_EVERY).await;
                this.update(cx, |page, cx| page.relist(tries - 1, cx)).ok();
            }
        }));
    }

    // End the installer's terminal on its machine (`CloseTerminal`) and stop showing it.
    fn close_install(&mut self, cx: &mut Context<Self>) {
        let Some(run) = self.install_run.take() else {
            return;
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let params = serde_json::json!({ "terminalId": run.id });
        cx.spawn(async move |this, cx| {
            if let Err(error) = engine.client().call(methods::CLOSE_TERMINAL, params).await {
                this.update(cx, |page, cx| {
                    page.error = Some(format!("The installer did not stop — {error}"));
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    // The installer's terminal under its agent's row, and once it ended, what it ended
    // with and Close.
    fn render_install(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let run = self.install_run.as_ref()?;
        let look = crate::live::TerminalLook::of(cx);
        let hosted = crate::live::terminal(&run.id, &look, cx);
        let body = match hosted {
            Some(hosted) => hosted.view.into_any_element(),
            None => {
                crate::live::chrome::loading("Opening the installer…", theme, cx.entity_id(), cx)
                    .into_any_element()
            }
        };
        let footer = run.ended.map(|exit| {
            let said = match exit {
                0 => "The installer finished".to_owned(),
                status => format!("The installer ended with status {status}"),
            };
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .gap(px(12.0))
                .child(
                    div()
                        .id("harness-install-ended")
                        .debug_selector(|| "harness-install-ended".into())
                        .role(gpui::Role::Status)
                        .aria_label(said.clone())
                        .text_size(crate::typography::ui_rems(12.0))
                        .text_color(if exit == 0 { theme.text_muted } else { theme.danger })
                        .child(said),
                )
                .child(
                    widgets::ghost_action(theme)
                        .id("harness-install-close")
                        .flex_none()
                        .role(gpui::Role::Button)
                        .aria_label("Close the installer")
                        .on_click(cx.listener(|this, _, _, cx| this.cancel_install(cx)))
                        .child("Close"),
                )
        });
        Some(
            div()
                .mx(px(16.0))
                .pl(px(DETAILS_INSET))
                .pb(px(16.0))
                .flex()
                .flex_col()
                .gap(px(8.0))
                .child(
                    div()
                        .id("harness-install-terminal")
                        .debug_selector(|| "harness-install-terminal".into())
                        .role(gpui::Role::Group)
                        .aria_label("Installer")
                        .h(px(INSTALL_TERMINAL_HEIGHT))
                        .overflow_hidden()
                        .rounded(px(8.0))
                        .border_1()
                        .border_color(theme.wash(0.08))
                        .bg(look.background)
                        .child(body),
                )
                .children(footer)
                .into_any_element(),
        )
    }

    fn set_enabled(&mut self, harness: HarnessId, enabled: bool, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let params = self.with_target(serde_json::json!({
            "harness": harness,
            "enabled": enabled,
        }));
        self.error = None;
        self.pending_toggle = Some((harness, enabled));
        cx.notify();
        self.toggle_task = Some(cx.spawn(async move |this, cx| {
            let result = engine.client().call(methods::SET_HARNESS_ENABLED, params).await;
            this.update(cx, |page, cx| {
                if page.pending_toggle == Some((harness, enabled)) {
                    page.pending_toggle = None;
                }
                match result {
                    Ok(value) => {
                        if let Ok(list) = serde_json::from_value::<Vec<HarnessDescriptor>>(value) {
                            page.harnesses = Loadable::Ready(list);
                        }
                    }
                    Err(err) => page.error = Some(err.to_string()),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// The page-header device switcher (the Accounts pattern): platform glyph
    /// · name · presence dot · sort glyph, opening a dropdown of every
    /// registered device.
    fn render_device_switcher(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        use crate::icons::{self, icon};
        let (mut devices, local_id) = {
            let s = self.state.read(cx);
            (s.devices.clone(), s.local_device_id.clone())
        };
        devices.sort_by(|a, b| a.created_at.cmp(&b.created_at).then_with(|| a.id.cmp(&b.id)));
        let effective = self.target_device.clone().or_else(|| local_id.clone());
        let platform_glyph = |platform: &str| match platform {
            "macos" | "darwin" => icons::LAPTOP,
            "ios" | "android" => icons::SMARTPHONE,
            _ => icons::MONITOR,
        };
        // Local device = no passthrough (calls stay direct).
        let mut targets: Vec<Option<String>> = Vec::new();
        let mut options = Vec::new();
        for device in &devices {
            let is_local = local_id.as_deref() == Some(device.id.as_str());
            let glyph = platform_glyph(&device.platform);
            let muted = theme.text_muted;
            let option = widgets::SelectOption::new(device.name.clone()).leading(move || {
                icon(glyph).size(px(16.0)).flex_none().text_color(muted).into_any_element()
            });
            options.push(if is_local { option.detail("You") } else { option });
            targets.push((!is_local).then(|| device.id.clone()));
        }
        let selected =
            match devices.iter().position(|d| Some(d.id.as_str()) == effective.as_deref()) {
                Some(ix) => ix,
                // Not registered (yet): keep the current target reachable.
                None => {
                    let muted = theme.text_muted;
                    options.push(widgets::SelectOption::new("This device").leading(move || {
                        icon(icons::LAPTOP)
                            .size(px(16.0))
                            .flex_none()
                            .text_color(muted)
                            .into_any_element()
                    }));
                    targets.push(self.target_device.clone());
                    options.len() - 1
                }
            };
        widgets::select("harnesses-device-switcher", "Device", theme, |page: &mut Self| {
            &mut page.device_select
        })
        .options(options, selected)
        .menu_width(260.0)
        .heading("Devices")
        .on_select(move |page, ix, _, cx| {
            if let Some(target) = targets.get(ix) {
                page.set_target_device(target.clone(), cx);
            }
        })
        .render(&self.device_select, cx)
        .into_any_element()
    }

    fn rows(&self, cx: &mut Context<Self>) -> Vec<gpui::AnyElement> {
        let theme = Theme::of(cx).for_settings_surface();
        let Loadable::Ready(list) = &self.harnesses else {
            return Vec::new();
        };
        let descriptors = visible_harnesses(list);
        let enabled_count = descriptors.iter().filter(|d| descriptor_enabled(d)).count();
        descriptors
            .into_iter()
            .enumerate()
            .map(|(ix, descriptor)| {
                let harness = descriptor.id;
                let installed = descriptor.installed;
                let enabled = match self.pending_toggle {
                    Some((pending, on)) if pending == harness => on,
                    _ => descriptor_enabled(&descriptor),
                };
                // The one enabled harness left can't be switched off — the
                // composer needs something to run — but only when it could
                // actually run: an uninstalled last harness stays toggleable
                // (its hint says to turn it off) and the composer handles the
                // resulting empty set (mirrors the engine guard).
                let last_enabled = enabled && enabled_count == 1 && installed;
                // Turning OFF never needs the CLI (a default-on agent the
                // user doesn't want must not be stuck on because it isn't
                // installed); turning ON still does.
                let interactive = !last_enabled && (enabled || installed);
                let (icon_path, tint) = crate::pickers::harness_brand_icon(harness);
                // An agent on or off against its default (installed and not the mock)
                // offers "Reset" (M10).
                let default_on = installed && harness != HarnessId::MOCK;
                let reset = widgets::reset_action(
                    &theme,
                    format!("harness-reset-{harness:?}"),
                    &descriptor.name,
                    enabled == default_on || last_enabled,
                )
                .map(|reset| {
                    reset.on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.toggle(harness, default_on, cx);
                    }))
                });

                let mut meta: Vec<gpui::AnyElement> = Vec::new();
                // Installing REPLACES the not-installed hint in place, so the
                // row's text never shifts while the install runs.

                if self.installing == Some(harness) {
                    meta.push(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.0))
                            .child(crate::loaders::mini_mono_spinner(
                                format!("harness-install-spinner-{harness:?}"),
                                1.5,
                                theme.text_muted,
                                cx.entity_id(),
                                cx,
                            ))
                            .child(SharedString::from(install_label(&descriptor.name)))
                            .into_any_element(),
                    );
                } else if !installed {
                    // The command in the code face, after the words.
                    let command = install_command(harness, descriptor.can_install).map(|command| {
                        div()
                            .font_family(crate::typography::code_effective_family_name(cx))
                            .text_color(theme.text_muted)
                            .child(SharedString::from(command))
                    });
                    meta.push(
                        div()
                            .flex()
                            .flex_row()
                            .flex_wrap()
                            .items_baseline()
                            .gap_x(px(4.0))
                            .text_color(theme.text_muted.opacity(0.75))
                            .child(SharedString::from(install_hint(
                                harness,
                                enabled,
                                descriptor.can_install,
                            )))
                            .children(command)
                            .into_any_element(),
                    );
                }
                // An installed agent says so, where nothing else speaks for it.
                if meta.is_empty() && installed {
                    meta.push(
                        div()
                            .debug_selector(move || format!("harness-installed-{ix}"))
                            .child(INSTALLED)
                            .into_any_element(),
                    );
                }
                let tile = div()
                    .flex_none()
                    .size(px(36.0))
                    .rounded(px(10.0))
                    .bg(theme.wash(0.06))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        crate::icons::icon(icon_path)
                            .size(px(16.0))
                            .text_color(tint.unwrap_or(theme.text_muted)),
                    );
                let header = widgets::card_row(&theme, ix == 0)
                    .id(("harness-row", ix))
                    .when(!installed && self.installing != Some(harness), |el| el.opacity(0.55))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(180.0))
                            .min_h(px(44.0))
                            .px(px(4.0))
                            .rounded(px(8.0))
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(12.0))
                            .child(tile)
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .child(widgets::row_title(&theme, descriptor.name.clone()))
                                    .when(!meta.is_empty(), |label| {
                                        label.child(widgets::meta_line(&theme, meta))
                                    }),
                            )
                            // Install / Cancel sit with the row's other action.
                            .when(
                                offers_install(harness, installed, descriptor.can_install)
                                    && self.installing != Some(harness),
                                |el| {
                                    el.child(
                                        widgets::ghost_action(&theme)
                                            .id(("harness-install", ix))
                                            .flex_none()
                                            .role(gpui::Role::Button)
                                            .aria_label(format!("Install {}", descriptor.name))
                                            .when(self.installing.is_none(), |el| {
                                                el.on_click(cx.listener(move |this, _, _, cx| {
                                                    cx.stop_propagation();
                                                    this.install(harness, cx)
                                                }))
                                            })
                                            .child("Install"),
                                    )
                                },
                            )
                            .when(self.installing == Some(harness), |el| {
                                el.child(
                                    widgets::ghost_action(&theme)
                                        .id(("harness-cancel-install", ix))
                                        .flex_none()
                                        .role(gpui::Role::Button)
                                        .aria_label(format!(
                                            "Cancel installing {}",
                                            descriptor.name
                                        ))
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            cx.stop_propagation();
                                            this.cancel_install(cx)
                                        }))
                                        .child("Cancel"),
                                )
                            }),
                    )
                    .children(reset)
                    .child(
                        widgets::toggle_switch(
                            &theme,
                            enabled,
                            format!("harness-switch-{harness:?}"),
                        )
                        .id(("harness-toggle", ix))
                        .when(!interactive && !enabled, |el| {
                            el.opacity(crate::theme::DISABLED_OPACITY)
                        })
                        .when(interactive, |el| {
                            el.cursor_pointer()
                                .tab_index(0)
                                .role(gpui::Role::Switch)
                                .aria_label(descriptor.name.clone())
                                .aria_toggled(if enabled {
                                    gpui::Toggled::True
                                } else {
                                    gpui::Toggled::False
                                })
                                .map(|el| crate::surface_chrome::focus_ring_revealing(el, &theme))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.toggle(harness, !enabled, cx);
                                }))
                                .on_key_down(cx.listener(
                                    move |this, event: &gpui::KeyDownEvent, _, cx| {
                                        if !event.is_held
                                            && matches!(
                                                event.keystroke.key.as_str(),
                                                "enter" | "space"
                                            )
                                        {
                                            this.toggle(harness, !enabled, cx);
                                            cx.stop_propagation();
                                        }
                                    },
                                ))
                        }),
                    );
                div()
                    .flex()
                    .flex_col()
                    .child(header)
                    // The installer's terminal, under the agent it installs.
                    .when(
                        self.install_run.as_ref().is_some_and(|run| run.harness == harness),
                        |row| row.children(self.render_install(&theme, cx)),
                    )
                    .into_any_element()
            })
            .collect()
    }

    fn on_scroll_hovered(&mut self, hovered: &bool, _: &mut Window, cx: &mut Context<Self>) {
        if self.scroll.set_list_hovered(*hovered) {
            cx.notify();
        }
    }
}

impl popover::ScrollRailHost for HarnessesPage {
    fn rail_bar(&mut self) -> &mut popover::MenuScrollbarState {
        self.scroll.rail_bar()
    }

    fn rail_scroll(&self) -> Option<gpui::ScrollHandle> {
        self.scroll.rail_scroll()
    }
}

impl Render for HarnessesPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).for_settings_surface();

        let body: gpui::AnyElement = match &self.harnesses {
            Loadable::Idle | Loadable::Loading => widgets::section_card(&theme)
                .p(px(16.0))
                .child(popover::skeleton_rows("harnesses-skeleton", &theme, 4, cx.entity_id(), cx))
                .into_any_element(),
            Loadable::Error(message) => {
                let message = message.clone();
                div()
                    .child(widgets::error_strip(&theme, message))
                    .child(
                        widgets::ghost_action(&theme)
                            .id("harnesses-retry")
                            .mt(px(8.0))
                            .tab_index(0)
                            .role(gpui::Role::Button)
                            .map(|el| crate::surface_chrome::focus_ring_revealing(el, &theme))
                            .on_click(cx.listener(|page, _, _, cx| {
                                page.load(cx);
                                cx.notify();
                            }))
                            .child(SharedString::from("Try again")),
                    )
                    .into_any_element()
            }
            Loadable::Ready(_) => {
                let rows = self.rows(cx);
                widgets::section_card(&theme).children(rows).into_any_element()
            }
        };
        crate::toast::take_error(&mut self.error, cx);
        let switcher = self.render_device_switcher(&theme, cx);
        let autonomy = self.render_autonomy(&theme, cx);

        let scrollbar = popover::rail(self, "harnesses-page-scrollbar", &theme, cx);

        div()
            .id("harnesses-page-host")
            .relative()
            .size_full()
            .on_hover(cx.listener(Self::on_scroll_hovered))
            .child(
                crate::edge_fade::edge_faded(
                    16.0,
                    true,
                    true,
                    div()
                        .id("harnesses-page")
                        .size_full()
                        .overflow_y_scroll()
                        .track_scroll(&self.scroll.scroll)
                        .child(
                            widgets::page_column()
                                // One page head (`widgets::page_head`).
                                .child(widgets::page_head(
                                    &theme,
                                    "Providers",
                                    None,
                                    "The coding agents each device can run.",
                                    Some(switcher.into_any_element()),
                                ))
                                .child(body)
                                .child(autonomy),
                        ),
                )
                .fade_overflow_y(&self.scroll.scroll),
            )
            .children(scrollbar)
    }
}

#[cfg(test)]
mod tests {
    // The autonomy choices read as the composer's mode menu reads them; Auto is the
    // default and the recommended one, and the one that asks nothing says so (M10).
    #[test]
    fn autonomy_names_its_default_and_what_full_costs() {
        use super::{AUTONOMY, AUTONOMY_DEFAULT, AUTONOMY_FULL};
        assert_eq!(AUTONOMY[AUTONOMY_DEFAULT].0, "auto");
        assert_eq!(AUTONOMY[AUTONOMY_DEFAULT].1, "Auto");
        assert_eq!(AUTONOMY[AUTONOMY_FULL].0, "full");
        assert_eq!(AUTONOMY[AUTONOMY_FULL].1, "Bypass permissions");
        assert!(AUTONOMY[AUTONOMY_FULL].2.starts_with("Asks nothing"));
        assert_eq!(
            crate::settings::widgets::Standing::of(true, true).map(|s| s.words()),
            Some("Default · Recommended")
        );
    }
}

// An installed agent's row says so; no row opens, there being nothing under it.
#[cfg(test)]
mod row_tests {
    use gpui::AppContext as _;

    #[gpui::test]
    fn an_installed_agent_says_so_and_no_row_opens(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            crate::settings::init(Default::default(), dir.path(), cx);
            gpui_base::init(cx);
            cx.set_global(crate::theme::Theme::default());
        });
        let (page, cx) = cx.add_window_view(|_, cx| {
            let state = cx.new(|_| crate::state::AppState::new());
            super::HarnessesPage::new(state, cx)
        });
        let descriptor =
            |id, name: &str, installed: bool| crate::catalog::registry::HarnessDescriptor {
                id,
                name: name.into(),
                supports_steering: false,
                steering_mode: slopty_view_proto::SteeringMode::TurnBoundary,
                reasoning_levels: Vec::new(),
                installed,
                can_install: true,
                enabled: Some(installed),
            };
        page.update(cx, |page, _| {
            page.harnesses = super::Loadable::Ready(vec![
                descriptor(slopty_view_proto::HarnessId::CLAUDE_CODE, "Claude Code", true),
                descriptor(slopty_view_proto::HarnessId::CODEX, "Codex", false),
            ]);
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(
            cx.debug_bounds("harness-installed-0").is_some(),
            "Claude Code says it is installed"
        );
        assert!(
            cx.debug_bounds("harness-installed-1").is_none(),
            "Codex says how to install instead"
        );
        assert!(cx.debug_bounds("harness-caret-0").is_none(), "no preferences to open");
    }
}

// Install runs the vendor's installer in a terminal on the machine, shown under the row;
// once it ends the agent arrives in the list, and Close ends the terminal. An install the
// machine refuses is said, and nothing is left running.
#[cfg(test)]
mod install_tests {
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    use gpui::AppContext as _;
    use slopty_view_proto::HarnessId;
    use slopty_view_rpc::{RpcError, RpcReply, methods};

    use super::HarnessesPage;

    /// A machine without Codex: its first install is refused, its second opens terminal 7, and
    /// Codex is there once `arrived`.
    #[derive(Default)]
    struct Machine {
        asked: Mutex<Vec<(String, serde_json::Value)>>,
        refused: AtomicBool,
        arrived: AtomicBool,
    }

    #[async_trait::async_trait]
    impl slopty_view_rpc::RpcService for Machine {
        async fn handle(
            &self,
            method: &str,
            params: serde_json::Value,
        ) -> Result<RpcReply, RpcError> {
            self.asked.lock().unwrap().push((method.to_owned(), params.clone()));
            let answer = match method {
                methods::LIST_HARNESSES => serde_json::json!([{
                    "id": "codex", "name": "Codex", "supportsSteering": true,
                    "steeringMode": "step-boundary", "reasoningLevels": [],
                    "installed": self.arrived.load(Ordering::SeqCst), "canInstall": true,
                    "enabled": self.arrived.load(Ordering::SeqCst),
                }]),
                methods::GET_AGENT_SETTINGS => serde_json::json!({ "autonomy": "auto" }),
                methods::INSTALL_HARNESS if !self.refused.swap(true, Ordering::SeqCst) => {
                    return Err(RpcError::Failed("studio is out of reach".into()));
                }
                methods::INSTALL_HARNESS => serde_json::json!({ "terminalId": "7" }),
                methods::CLOSE_TERMINAL => serde_json::json!({}),
                other => panic!("the page asked {other}"),
            };
            Ok(RpcReply::Value(answer))
        }
    }

    struct Blank;

    impl gpui::Render for Blank {
        fn render(
            &mut self,
            _: &mut gpui::Window,
            _: &mut gpui::Context<Self>,
        ) -> impl gpui::IntoElement {
            gpui::div()
        }
    }

    type Heard = Rc<RefCell<Vec<Box<dyn FnMut(&mut gpui::App)>>>>;

    #[gpui::test]
    fn install_shows_the_installer_and_lists_the_agent_once_it_ends(cx: &mut gpui::TestAppContext) {
        // The machine answers on this thread, between the app's turns: a current-thread runtime
        // driven here, so nothing wakes the app's executor from elsewhere.
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let _guard = runtime.enter();
        let dir = tempfile::tempdir().unwrap();
        let ended = Rc::new(RefCell::new(None::<i32>));
        let heard: Heard = Rc::default();
        cx.update(|cx| {
            crate::settings::init(Default::default(), dir.path(), cx);
            gpui_base::init(cx);
            cx.set_global(crate::theme::Theme::default());
            let (ended, heard) = (Rc::clone(&ended), Rc::clone(&heard));
            crate::live::install(
                cx,
                move |id, _, cx| {
                    assert_eq!(id, "7", "the terminal the machine opened");
                    let exit = *ended.borrow();
                    Some(crate::live::Hosted {
                        view: cx.new(|_| Blank).into(),
                        focus: cx.focus_handle(),
                        title: None,
                        ended: exit.is_some(),
                        exit,
                        progress: None,
                        drop: None,
                        ground: None,
                    })
                },
                |_, _| None,
                move |_, changed, _| {
                    heard.borrow_mut().push(changed);
                    None
                },
            );
        });
        let machine = Arc::new(Machine::default());
        let engine = crate::state::EngineHandle::from_test_client(slopty_view_rpc::memory_client(
            Arc::clone(&machine) as Arc<dyn slopty_view_rpc::RpcService>,
        ));
        let (page, cx) = cx.add_window_view(|_, cx| {
            let state = cx.new(|_| {
                let mut state = crate::state::AppState::new();
                state.set_test_engine(engine);
                state
            });
            HarnessesPage::new(state, cx)
        });
        let settle = |cx: &mut gpui::VisualTestContext, done: &dyn Fn(&HarnessesPage) -> bool| {
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
        let draw = |cx: &mut gpui::VisualTestContext| {
            cx.update(|window, cx| {
                window.refresh();
                window.draw(cx).clear(cx);
            });
        };
        let codex = |page: &HarnessesPage| {
            page.harnesses
                .ready()
                .and_then(|list| list.iter().find(|d| d.id == HarnessId::CODEX).cloned())
        };
        settle(cx, &|page| codex(page).is_some_and(|d| !d.installed && d.can_install));

        // Refused: said, and nothing runs.
        page.update(cx, |page, cx| page.install(HarnessId::CODEX, cx));
        settle(cx, &|page| page.installing.is_none());
        page.update(cx, |page, cx| {
            let said = page.error.clone().or_else(|| {
                crate::toast::shown(cx).into_iter().map(|(_, text)| text.to_string()).last()
            });
            let said = said.unwrap_or_default();
            assert!(said.contains("Couldn't install") && said.contains("out of reach"), "{said}");
            assert!(page.install_run.is_none());
        });

        // Opened: the installer's terminal stands under the row while it runs.
        page.update(cx, |page, cx| page.install(HarnessId::CODEX, cx));
        settle(cx, &|page| page.install_run.is_some());
        draw(cx);
        let terminal = cx.debug_bounds("harness-install-terminal").expect("the installer shows");
        assert!(f32::from(terminal.size.height) >= super::INSTALL_TERMINAL_HEIGHT - 1.0);
        assert!(cx.debug_bounds("harness-install-ended").is_none(), "still running");
        let install = machine
            .asked
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|(m, _)| m == methods::INSTALL_HARNESS)
            .cloned();
        assert_eq!(install.map(|(_, params)| params["harness"].clone()), Some("codex".into()));

        // Ended: its worker sees Codex a beat later, and the row lists it installed.
        *ended.borrow_mut() = Some(0);
        machine.arrived.store(true, Ordering::SeqCst);
        cx.update(|_, cx| heard.borrow_mut().iter_mut().for_each(|changed| changed(cx)));
        settle(cx, &|page| codex(page).is_some_and(|d| d.installed));
        page.read_with(cx, |page, _| {
            assert_eq!(page.install_run.as_ref().and_then(|run| run.ended), Some(0));
            assert_eq!(page.installing, None);
        });
        draw(cx);
        assert!(cx.debug_bounds("harness-install-ended").is_some(), "the end is said");

        // Close: the terminal ends on its machine and the row lets it go.
        page.update(cx, |page, cx| page.cancel_install(cx));
        let closed = || {
            machine
                .asked
                .lock()
                .unwrap()
                .iter()
                .any(|(m, params)| m == methods::CLOSE_TERMINAL && params["terminalId"] == "7")
        };
        settle(cx, &|page| page.install_run.is_none() && closed());
        assert!(closed(), "the installer's terminal was closed");
        draw(cx);
        assert!(cx.debug_bounds("harness-install-terminal").is_none());
    }
}

#[cfg(test)]
#[test]
fn install_visibility_and_hint_follow_target_capabilities() {
    for id in [
        HarnessId::ANTIGRAVITY,
        HarnessId::CODEX,
        HarnessId::OPENCODE,
        HarnessId::CLAUDE_CODE,
        HarnessId::CURSOR,
        HarnessId::PI,
        HarnessId::GROK,
        HarnessId::HERMES,
        HarnessId::DEVIN,
        HarnessId::MOCK,
    ] {
        for installed in [false, true] {
            for available in [false, true] {
                assert_eq!(
                    offers_install(id, installed, available),
                    id != HarnessId::MOCK && !installed && available
                );
            }
        }
    }
    assert_eq!(cli_name(HarnessId::ANTIGRAVITY), "Antigravity");
    assert_eq!(install_hint(HarnessId::ANTIGRAVITY, false, true), "Install Antigravity to enable");
    assert!(
        install_hint(HarnessId::ANTIGRAVITY, false, false).contains("ANTIGRAVITY_ACP_EXECUTABLE")
    );
}

#[cfg(test)]
#[test]
fn install_phase_copy_and_cancel_target_match_install() {
    assert_eq!(install_label("Claude Code"), "Installing Claude Code…");
    assert_eq!(install_label("Pi"), "Installing Pi…");
    for target in [None, Some("remote-device".to_string())] {
        let params = install_params(HarnessId::PI, &target);
        assert_eq!(params["harness"], "pi");
        assert_eq!(params["targetDeviceId"], serde_json::to_value(&target).unwrap());
    }
    // The command stands on its own, with no backticks around it.
    assert_eq!(
        install_hint(HarnessId::CODEX, false, false),
        "Install the codex CLI to enable. Install with"
    );
    assert_eq!(install_command(HarnessId::CODEX, false), Some("npm install -g @openai/codex"));
    assert_eq!(install_command(HarnessId::CODEX, true), None, "Slopty installs it");
    assert_eq!(install_command(HarnessId::ANTIGRAVITY, false), None);
    assert!(!install_hint(HarnessId::CODEX, false, false).contains('`'));
    assert!(
        install_hint(HarnessId::CODEX, true, false)
            .starts_with("codex CLI not installed — turn it off or install it.")
    );
}
