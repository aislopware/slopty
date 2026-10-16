// Derived from zeron (MIT, github.com/zeronsh/zeron @ 053ce2e), see THIRD_PARTY_NOTICES.md
//! Settings → Devices (feature-inventory §1.5): the device registry — name,
//! platform, last-seen, an Online/Offline badge, a "This device" badge, click-to-copy id,
//! and an Add device dialog (AddDevice).
// slopty: no Rename: a machine is named by its own name. Add device is Slopty's own dialog
// (`add_device`): this Mac, a machine over ssh, or the server. The page shows the server this
// app follows, with a way to change it.

use chrono::{DateTime, Utc};
use gpui::{
    AnyElement, ClipboardItem, Context, Entity, SharedString, Subscription, Task, Window, div,
    prelude::*, px,
};
use std::time::Duration;

use slopty_view_proto::WorkspaceScope;

use crate::popover;
use crate::settings::add_device::{AddDeviceDialog, Choice, Closed};
use crate::settings::widgets;
use crate::state::AppState;
use crate::theme::Theme;

/// A device that pinged within this window shows a presence dot (engines
/// heartbeat every 15s; 70s tolerates a couple of missed beats).
pub const DEVICE_ONLINE_WINDOW_SECS: i64 = 70;

/// Presence: last-seen within the online window (future timestamps count). Pure.
pub fn device_online(last_seen: Option<DateTime<Utc>>, now: DateTime<Utc>) -> bool {
    last_seen
        .is_some_and(|at| now.signed_duration_since(at).num_seconds() <= DEVICE_ONLINE_WINDOW_SECS)
}

/// Compact last-seen line. Pure.
pub fn format_last_seen(last_seen: Option<DateTime<Utc>>, now: DateTime<Utc>) -> String {
    let Some(at) = last_seen else {
        return "never seen".to_string();
    };
    let secs = now.signed_duration_since(at).num_seconds();
    if secs < 60 {
        "just now".to_string()
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86_400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86_400)
    }
}

/// Scope-aware copy: a local registry describes only the active local
/// workspace and must not imply that account device metadata is already live.
pub fn devices_subtitle(scope: Option<WorkspaceScope>) -> &'static str {
    match scope {
        Some(WorkspaceScope::Local) => "Devices in this local workspace.",
        Some(WorkspaceScope::Synced) => "Devices synced to this workspace.",
        Some(WorkspaceScope::Development) | None => "Devices in this workspace.",
    }
}

// slopty: Add device in place of Rename.
/// What the page says it lists.
const DEVICES_SUBTITLE: &str = "The machines Slopty works on, each with its own agents, files and terminals.";

pub struct DevicesPage {
    state: Entity<AppState>,
    scroll: widgets::PageScroll,
    // slopty: Add device in place of Rename.
    add: Option<(Entity<AddDeviceDialog>, Subscription)>,
    /// Open the dialog on this choice at the next draw, which has the window its fields need.
    add_wanted: Option<Choice>,
    /// The server address a link named, for the dialog's field when it opens.
    add_server: Option<String>,
    /// Device id whose id-chip shows "Copied" right now.
    copied: Option<String>,
    error: Option<SharedString>,
    copy_task: Option<Task<()>>,
    // slopty: the action call under way.
    action_task: Option<Task<()>>,
    // slopty: the server's code is up ("Connect a phone").
    inviting: bool,
    _observe: Subscription,
}

impl DevicesPage {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let observe = cx.observe(&state, |_, _, cx| cx.notify());
        Self {
            state,
            scroll: widgets::PageScroll::default(),
            add: None,
            add_wanted: None,
            add_server: None,
            copied: None,
            error: None,
            copy_task: None,
            action_task: None,
            inviting: false,
            _observe: observe,
        }
    }

    // slopty: Add device in place of Rename.
    /// Open Settings → Devices' Add device dialog (the first run's "Add a device").
    pub fn open_add_device(&mut self, cx: &mut Context<Self>) {
        self.open_add_device_on(Choice::ThisMac, cx);
    }

    // slopty: the sidebar's server menu opens the server's code straight.
    /// Show the server's code for a phone or iPad to scan, while the server offers one.
    pub fn open_invite(&mut self, cx: &mut Context<Self>) {
        self.inviting = true;
        cx.notify();
    }

    /// Open the Add device dialog on `choice` (the menu's Add worker, Connect to server).
    pub fn open_add_device_on(&mut self, choice: Choice, cx: &mut Context<Self>) {
        self.add_wanted = Some(choice);
        cx.notify();
    }

    /// Open the Add device dialog on "A server" with `address` in its field (a link that names
    /// the server); connecting stays the person's press.
    pub fn open_add_device_with_server(&mut self, address: String, cx: &mut Context<Self>) {
        self.add_wanted = Some(Choice::Server);
        self.add_server = Some(address);
        cx.notify();
    }

    fn show_add_device(&mut self, choice: Choice, window: &mut Window, cx: &mut Context<Self>) {
        self.add_wanted = None;
        let server = self.add_server.take();
        if let Some((dialog, _)) = &self.add {
            if let Some(address) = server {
                dialog.update(cx, |dialog, cx| dialog.fill_server(address, window, cx));
            }
            return;
        }
        let state = self.state.clone();
        let dialog = cx.new(|cx| AddDeviceDialog::new(state, choice, window, cx));
        if let Some(address) = server {
            dialog.update(cx, |dialog, cx| dialog.fill_server(address, window, cx));
        }
        let closed = cx.subscribe(&dialog, |this, _, _: &Closed, cx| {
            this.add = None;
            cx.notify();
        });
        self.add = Some((dialog, closed));
        cx.notify();
    }

    /// Escape that reached Settings unclaimed closes the Add device dialog first,
    /// so it never closes Settings under the dialog. Returns whether it did.
    pub(crate) fn dismiss_on_escape(&mut self, cx: &mut Context<Self>) -> bool {
        // slopty: the server's code closes the same way.
        if self.add.take().is_none() && !std::mem::take(&mut self.inviting) {
            return false;
        }
        cx.notify();
        true
    }

    fn copy_id(&mut self, device_id: String, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(device_id.clone()));
        self.copied = Some(device_id);
        self.copy_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(1500))
                .await;
            this.update(cx, |page, cx| {
                page.copied = None;
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    // slopty: a machine's (or the server's, `"server"`) action, run by the app; it says how
    // that went in a toast, and a call that does not reach it is said here.
    fn run_action(&mut self, device_id: String, action: String, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else { return };
        self.action_task = Some(cx.spawn(async move |this, cx| {
            let params = serde_json::json!({ "deviceId": device_id, "action": action });
            let result =
                engine.client().call(slopty_view_rpc::methods::DEVICE_ACTION, params).await;
            this.update(cx, |page, cx| {
                if let Err(err) = result {
                    page.error = Some(err.to_string().into());
                    cx.notify();
                }
            })
            .ok();
        }));
    }

    fn on_scroll_hovered(&mut self, hovered: &bool, _: &mut Window, cx: &mut Context<Self>) {
        if self.scroll.set_list_hovered(*hovered) {
            cx.notify();
        }
    }
}

impl popover::ScrollRailHost for DevicesPage {
    fn rail_bar(&mut self) -> &mut popover::MenuScrollbarState {
        self.scroll.rail_bar()
    }

    fn rail_scroll(&self) -> Option<gpui::ScrollHandle> {
        self.scroll.rail_scroll()
    }
}

/// Human platform label (slopty settings.devices.tsx `platformLabel`).
pub fn platform_label(platform: &str) -> &str {
    match platform {
        "macos" | "darwin" => "macOS",
        "linux" => "Linux",
        "windows" => "Windows",
        "web" => "Web",
        "ios" => "iOS",
        "android" => "Android",
        other => other,
    }
}

// slopty: a machine row's tile, by what it runs.
/// The glyph that leads a machine's row: a laptop for a Mac (Slopty's own kind), a phone or
/// tablet for a handheld, a server for Linux or a machine that has not said.
pub fn platform_icon(platform: &str) -> &'static str {
    match platform {
        "macos" | "darwin" => crate::icons::LAPTOP,
        "windows" => crate::icons::MONITOR,
        "ios" | "android" => crate::icons::SMARTPHONE,
        _ => crate::icons::REMOTE_SERVER,
    }
}

// slopty: the confirmed removal reads as destructive; asking first and the rest stay quiet.
/// The tone of a machine action's button, by its id.
fn action_tone(id: &str) -> widgets::ActionTone {
    match id {
        "remove" => widgets::ActionTone::Danger,
        _ => widgets::ActionTone::Quiet,
    }
}

/// Short device id for the click-to-copy chip (`abcd1234…wxyz`).
pub fn short_id(id: &str) -> String {
    if id.len() > 12 {
        format!("{}…{}", &id[..8], &id[id.len() - 4..])
    } else {
        id.to_string()
    }
}

impl Render for DevicesPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::toast::take_error(&mut self.error, cx);
        let theme = Theme::of(cx).for_settings_surface();
        let now = Utc::now();
        // slopty: every machine is listed, whatever the scope; Add device in place of Rename.
        let (devices, local_id) = {
            let state = self.state.read(cx);
            (state.devices.clone(), state.local_device_id.clone())
        };
        let copied = self.copied.clone();
        if let Some(choice) = self.add_wanted {
            self.show_add_device(choice, window, cx);
        }
        let viewport = window.viewport_size();
        let dialog = self
            .add
            .as_ref()
            .map(|(dialog, _)| popover::modal("add-device-dialog", viewport, dialog.clone().into_any_element()));
        let server = self.state.read(cx).connectivity.server.clone();
        // slopty: the server's code, while it is up and the app still offers one.
        let invite = server.as_ref().and_then(|server| server.invite.clone());
        let invite_dialog = invite.as_ref().filter(|_| self.inviting).map(|invite| {
            let link = invite.link.clone();
            let card = popover::dialog_card(&theme)
                .w(px(360.0))
                .id("invite-card")
                .role(gpui::Role::Dialog)
                .aria_label("Connect a phone or iPad")
                .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                    if event.keystroke.key == "escape" {
                        this.inviting = false;
                        cx.notify();
                        cx.stop_propagation();
                    }
                }))
                .child(popover::dialog_title(&theme, "Connect a phone or iPad"))
                .child(crate::settings::invite::body(&theme, invite))
                .child(
                    div()
                        .mt(px(16.0))
                        .flex()
                        .flex_row()
                        .justify_end()
                        .gap(px(8.0))
                        .children(link.map(|link| {
                            widgets::text_action(&theme, widgets::ActionTone::Quiet, "Copy link")
                                .id("invite-copy")
                                .tab_index(0)
                                .role(gpui::Role::Button)
                                .aria_label("Copy link")
                                .focus_visible(|s| s.border_2().border_color(theme.accent))
                                .on_click(cx.listener(move |_, _, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(link.clone()));
                                    crate::toast::info(cx, "Link copied");
                                }))
                        }))
                        // slopty: a dialog commits with the solid button, as Add device does.
                        .child(
                            widgets::text_action(&theme, widgets::ActionTone::Solid, "Done")
                                .id("invite-done")
                                .tab_index(0)
                                .role(gpui::Role::Button)
                                .aria_label("Done")
                                .focus_visible(|s| s.border_2().border_color(theme.accent))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.inviting = false;
                                    cx.notify();
                                })),
                        ),
                );
            popover::modal("invite-dialog", viewport, card.into_any_element())
        });
        // Split into this device and the rest; each renders as rows in one
        // block, like every other settings page.
        let (local, others): (Vec<_>, Vec<_>) = devices
            .into_iter()
            .enumerate()
            .partition(|(_, device)| local_id.as_deref() == Some(device.id.as_str()));
        let device_row = |ix: usize, device: slopty_view_proto::Device, first: bool| {
            let online = device_online(device.last_seen_at, now);
            let is_local = local_id.as_deref() == Some(device.id.as_str());
            let id_copied = copied.as_deref() == Some(device.id.as_str());
            let copy_id = device.id.clone();
            // slopty: a machine whose worker has not said what it runs shows no platform.
            let mut meta: Vec<AnyElement> = (!device.platform.is_empty())
                .then(|| {
                    div()
                        .child(SharedString::from(
                            platform_label(&device.platform).to_string(),
                        ))
                        .into_any_element()
                })
                .into_iter()
                .collect();
            if let Some(version) = device.version.as_deref().filter(|v| !v.is_empty()) {
                meta.push(
                    div()
                        .child(SharedString::from(format!("v{version}")))
                        .into_any_element(),
                );
            }
            // Presence only says something about other devices.
            if !is_local {
                meta.push(if online {
                    div()
                        .text_color(theme.success_muted)
                        .child(SharedString::from("Online"))
                        .into_any_element()
                } else {
                    div()
                        .child(SharedString::from(format!(
                            "Last seen {}",
                            format_last_seen(device.last_seen_at, now)
                        )))
                        .into_any_element()
                });
            }
            // slopty: what the app offers to do to the machine, before Copy ID.
            let actions = device.actions.iter().enumerate().map(|(n, action)| {
                let (id, run) = (device.id.clone(), action.id.clone());
                widgets::text_action(&theme, action_tone(&action.id), action.label.clone())
                    .id(SharedString::from(format!("device-action-{ix}-{n}")))
                    .tab_index(0)
                    .role(gpui::Role::Button)
                    .aria_label(format!("{} {}", action.label, device.name))
                    .focus_visible(|s| s.border_2().border_color(theme.accent))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.run_action(id.clone(), run.clone(), cx);
                    }))
            });
            widgets::card_row(&theme, first)
                // slopty: every row leads with its tile, so the blocks share one text edge.
                .child(widgets::row_tile(&theme, platform_icon(&device.platform)))
                .child(
                    div()
                        .flex_1()
                        .min_w(px(160.0))
                        .child(widgets::row_title(&theme, device.name.clone()))
                        .child(widgets::meta_line(&theme, meta)),
                )
                .child(
                    div()
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(px(4.0))
                        .children(actions)
                        .child(
                            widgets::text_action(
                                &theme,
                                widgets::ActionTone::Quiet,
                                if id_copied { "Copied" } else { "Copy ID" },
                            )
                            .id(("device-id", ix))
                            .tab_index(0)
                            .role(gpui::Role::Button)
                            .aria_label(format!("Copy device ID {}", device.id))
                            .focus_visible(|s| s.border_2().border_color(theme.accent))
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.copy_id(copy_id.clone(), cx);
                                },
                            )),
                        ), // slopty: no Rename.
                )
                .into_any_element()
        };
        let local_block = (!local.is_empty()).then(|| {
            let mut block = widgets::section_card(&theme).mt(px(8.0));
            for (n, (ix, device)) in local.into_iter().enumerate() {
                block = block.child(device_row(ix, device, n == 0));
            }
            block
        });
        // slopty: the other machines, then Add device as the block's last row.
        let mut others_block = widgets::section_card(&theme).mt(px(8.0));
        let first_add = others.is_empty();
        for (n, (ix, device)) in others.into_iter().enumerate() {
            others_block = others_block.child(device_row(ix, device, n == 0));
        }
        let others_block = others_block.child(
            widgets::card_row(&theme, first_add)
                .child(widgets::row_tile(&theme, crate::icons::PLUS))
                .child(
                    div()
                        .flex_1()
                        .min_w(px(160.0))
                        .child(widgets::row_title(&theme, "Add a device"))
                        .child(widgets::meta_line(
                            &theme,
                            vec![div()
                                .child(SharedString::from(
                                    "This Mac, or a Mac or Linux machine you reach with ssh",
                                ))
                                .into_any_element()],
                        )),
                )
                .child(
                    widgets::text_action(&theme, widgets::ActionTone::Filled, "Add device")
                        .id("device-add")
                        .flex_none()
                        .tab_index(0)
                        .role(gpui::Role::Button)
                        .aria_label("Add device")
                        .focus_visible(|s| s.border_2().border_color(theme.accent))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.show_add_device(Choice::ThisMac, window, cx);
                        })),
                ),
        );
        // slopty: the server this app follows, and a way to change it.
        let server_actions = server.as_ref().map(|server| server.actions.clone()).unwrap_or_default();
        let server_block = {
            let (title, meta, action) = match &server {
                Some(server) => (
                    server.name.clone(),
                    if server.online {
                        div()
                            .text_color(theme.success_muted)
                            .child(SharedString::from("Online"))
                            .into_any_element()
                    } else {
                        div().child(SharedString::from("Offline")).into_any_element()
                    },
                    "Change",
                ),
                None => (
                    "No server yet".to_owned(),
                    div()
                        .child(SharedString::from("Your devices meet at one Slopty server"))
                        .into_any_element(),
                    "Connect",
                ),
            };
            widgets::section_card(&theme).mt(px(8.0)).child(
                widgets::card_row(&theme, true)
                    .child(widgets::row_tile(&theme, crate::icons::REMOTE_SERVER))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(160.0))
                            .child(widgets::row_title(&theme, title))
                            .child(widgets::meta_line(&theme, vec![meta])),
                    )
                    .children(server_actions.into_iter().enumerate().map(|(n, action)| {
                        let run = action.id.clone();
                        widgets::text_action(&theme, widgets::ActionTone::Quiet, action.label.clone())
                            .id(SharedString::from(format!("server-action-{n}")))
                            .flex_none()
                            .tab_index(0)
                            .role(gpui::Role::Button)
                            .aria_label(format!("{} server", action.label))
                            .focus_visible(|s| s.border_2().border_color(theme.accent))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.run_action("server".to_owned(), run.clone(), cx);
                            }))
                    }))
                    // slopty: the code a phone or iPad scans, where this device shows one.
                    .when(invite.is_some(), |row| {
                        row.child(
                            widgets::text_action(&theme, widgets::ActionTone::Quiet, "Connect a phone")
                                .id("device-server-invite")
                                .flex_none()
                                .tab_index(0)
                                .role(gpui::Role::Button)
                                .aria_label("Connect a phone or iPad")
                                .focus_visible(|s| s.border_2().border_color(theme.accent))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.inviting = true;
                                    cx.notify();
                                })),
                        )
                    })
                    .child(
                        widgets::text_action(&theme, widgets::ActionTone::Quiet, action)
                            .id("device-server")
                            .flex_none()
                            .tab_index(0)
                            .role(gpui::Role::Button)
                            .aria_label(format!("{action} server"))
                            .focus_visible(|s| s.border_2().border_color(theme.accent))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.show_add_device(Choice::Server, window, cx);
                            })),
                    ),
            )
        };
        let card = div()
            .flex()
            .flex_col()
            .child(widgets::section_label(&theme, "Server").mt(px(28.0)))
            .child(server_block)
            .when_some(local_block, |el, block| {
                el.child(widgets::section_label(&theme, "This device").mt(px(28.0)))
                    .child(block)
            })
            // slopty: Slopty lists every machine its server knows.
            .child(widgets::section_label(&theme, "Other devices").mt(px(28.0)))
            .child(others_block);

        let scrollbar = popover::rail(self, "devices-page-scrollbar", &theme, cx);
        div()
            .id("devices-page-host")
            .relative()
            .size_full()
            .on_hover(cx.listener(Self::on_scroll_hovered))
            .child(
                crate::edge_fade::edge_faded(
                    16.0,
                    true,
                    true,
                    div()
                        .id("devices-page")
                        .size_full()
                        .overflow_y_scroll()
                        .track_scroll(&self.scroll.scroll)
                        .child(
                            widgets::page_column()
                                .child(widgets::page_header(&theme, "Devices", None))
                                // slopty: Slopty's machines, not a synced registry.
                                .child(widgets::page_subtitle(&theme, DEVICES_SUBTITLE))
                                .child(card),
                        ),
                )
                .fade_overflow_y(&self.scroll.scroll),
            )
            .children(scrollbar)
            .when_some(dialog, |el, dialog| el.child(dialog))
            .when_some(invite_dialog, |el, dialog| el.child(dialog))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeDelta;

    #[test]
    fn presence_window() {
        let now = Utc::now();
        assert!(device_online(Some(now - TimeDelta::seconds(10)), now));
        assert!(device_online(Some(now - TimeDelta::seconds(70)), now));
        assert!(!device_online(Some(now - TimeDelta::seconds(71)), now));
        assert!(!device_online(None, now));
        // Clock skew (future) counts as online.
        assert!(device_online(Some(now + TimeDelta::seconds(30)), now));
    }

    #[test]
    fn last_seen_formatting() {
        let now = Utc::now();
        assert_eq!(format_last_seen(None, now), "never seen");
        assert_eq!(
            format_last_seen(Some(now - TimeDelta::seconds(30)), now),
            "just now"
        );
        assert_eq!(
            format_last_seen(Some(now - TimeDelta::minutes(5)), now),
            "5m ago"
        );
        assert_eq!(
            format_last_seen(Some(now - TimeDelta::hours(3)), now),
            "3h ago"
        );
        assert_eq!(
            format_last_seen(Some(now - TimeDelta::days(2)), now),
            "2d ago"
        );
    }

    // slopty: one tile per machine row, by platform; an unknown platform still gets one.
    #[test]
    fn every_machine_row_has_a_tile_by_platform() {
        assert_eq!(platform_icon("macos"), crate::icons::LAPTOP);
        assert_eq!(platform_icon("darwin"), crate::icons::LAPTOP);
        assert_eq!(platform_icon("linux"), crate::icons::REMOTE_SERVER);
        assert_eq!(platform_icon("ios"), crate::icons::SMARTPHONE);
        assert_eq!(platform_icon(""), crate::icons::REMOTE_SERVER, "not said yet");
    }

    // slopty: only the confirmed removal is red; asking first is quiet.
    #[test]
    fn only_remove_now_is_destructive() {
        assert!(matches!(action_tone("remove"), widgets::ActionTone::Danger));
        assert!(matches!(action_tone("ask-remove"), widgets::ActionTone::Quiet));
        assert!(matches!(action_tone("update"), widgets::ActionTone::Quiet));
    }

    #[test]
    fn local_subtitle_does_not_claim_synced_metadata() {
        let copy = devices_subtitle(Some(WorkspaceScope::Local));
        assert!(copy.contains("local workspace"));
        assert!(!copy.contains("synced"));
    }
}
