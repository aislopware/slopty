// Derived from zeron (MIT, github.com/zeronsh/zeron @ 053ce2e), see THIRD_PARTY_NOTICES.md
//! Settings → Devices (feature-inventory §1.5): the device registry — name,
//! platform, last-seen, an Online/Offline badge, a "This device" badge, click-to-copy id,
//! and an Add device dialog (AddDevice).
// A machine is named by its own name. Add device is Slopty's own dialog (`add_device`): this
// Mac, a machine over ssh, or the server. The page shows the server this app follows, with a way
// to change it.

use crate::motion::HoverFade as _;
use chrono::{DateTime, Utc};
use gpui::{
    AnyElement, ClipboardItem, Context, Entity, SharedString, Subscription, Task, Window, div,
    prelude::*, px,
};

// One name for what the server menu, this page and the dialog offer.
/// What a phone or an iPad joining the server is called, wherever it is offered.
pub(crate) const INVITE: &str = "Connect a phone or iPad";

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

/// When a machine last seen at `last_seen`, read at `now`, next reads differently: its
/// "Online" ends past the window, and "Last seen" moves on by the minute, the hour or the day
/// it says. `None` for a machine never seen.
pub fn presence_changes(last_seen: Option<DateTime<Utc>>, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    const MINUTE: i64 = 60;
    const HOUR: i64 = 60 * MINUTE;
    const DAY: i64 = 24 * HOUR;
    let at = last_seen?;
    if device_online(last_seen, now) {
        return at.checked_add_signed(chrono::Duration::seconds(DEVICE_ONLINE_WINDOW_SECS + 1));
    }
    let age = now.signed_duration_since(at).num_seconds();
    let step = if age < HOUR {
        MINUTE
    } else if age < DAY {
        HOUR
    } else {
        DAY
    };
    at.checked_add_signed(chrono::Duration::seconds((age / step + 1) * step))
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

/// What the page says it lists.
const DEVICES_SUBTITLE: &str = "The machines Slopty works on, each with its own agents, files and terminals.";

pub struct DevicesPage {
    state: Entity<AppState>,
    scroll: widgets::PageScroll,
    add: Option<(Entity<AddDeviceDialog>, Subscription)>,
    /// Open the dialog on this choice at the next draw, which has the window its fields need.
    add_wanted: Option<Choice>,
    /// The server address a link named, for the dialog's field when it opens.
    add_server: Option<String>,
    error: Option<SharedString>,
    // The action call under way.
    action_task: Option<Task<()>>,
    // The server's code is up ("Connect a phone").
    inviting: bool,
    // The code's dialog holds the keyboard while it shows.
    invite_focus: crate::surface_chrome::ModalFocus,
    /// The moment presence is read at: taken as the state moves and on
    /// [`Self::clock_tick`], never as the page is built, so a machine that stops answering
    /// turns from Online to Last seen without anything else moving.
    clock: DateTime<Utc>,
    /// The clock taken again when the soonest machine's presence next reads differently.
    clock_tick: Option<Task<()>>,
    _observe: Subscription,
}

/// The copy span's key for one device's ID.
fn copied_key(device_id: &str) -> String {
    format!("device-id-{device_id}")
}

impl DevicesPage {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let observe = cx.observe(&state, |page, _, cx| {
            page.take_clock(cx);
            cx.notify();
        });
        let mut page = Self {
            state,
            scroll: widgets::PageScroll::default(),
            add: None,
            add_wanted: None,
            add_server: None,
            error: None,
            action_task: None,
            inviting: false,
            invite_focus: crate::surface_chrome::ModalFocus::new(cx),
            clock: crate::clock::utc(cx),
            clock_tick: None,
            _observe: observe,
        };
        page.take_clock(cx);
        page
    }

    /// Take the clock now, and again when the soonest other machine's presence next
    /// reads differently ([`presence_changes`]), drawing the page then.
    fn take_clock(&mut self, cx: &mut Context<Self>) {
        self.clock = crate::clock::utc(cx);
        let now = self.clock;
        let state = self.state.read(cx);
        let next = state
            .devices
            .iter()
            .filter(|device| state.local_device_id.as_deref() != Some(device.id.as_str()))
            .filter_map(|device| presence_changes(device.last_seen_at, now))
            .min();
        self.clock_tick = next.map(|at| {
            // A hair past the change, so the clock taken then reads it.
            let wait = (at - now).to_std().unwrap_or_default() + std::time::Duration::from_millis(5);
            cx.spawn(async move |page, cx| {
                cx.background_executor().timer(wait).await;
                page.update(cx, |page, cx| {
                    page.take_clock(cx);
                    cx.notify();
                })
                .ok();
            })
        });
    }

    /// Open Settings → Devices' Add device dialog (the first run's "Add a device").
    pub fn open_add_device(&mut self, cx: &mut Context<Self>) {
        self.open_add_device_on(Choice::ThisMac, cx);
    }

    // The sidebar's server menu opens the server's code straight.
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
        // The server's code closes the same way.
        if self.add.take().is_none() && !std::mem::take(&mut self.inviting) {
            return false;
        }
        cx.notify();
        true
    }

    // A copy keeps the button's size and words: its glyph turns to a check and its
    // tooltip says "Copied" for the shared span (M5).
    fn copy_id(&mut self, device_id: String, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(device_id.clone()));
        crate::motion::copy_feedback(copied_key(&device_id), cx);
        cx.notify();
    }

    // A machine's (or the server's, `"server"`) action, run by the app; it says how
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

// A machine row's tile, by what it runs.
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

// The confirmed removal reads as destructive; asking first and the rest stay quiet.
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
        let now = self.clock;
        // Every machine is listed, whatever the scope.
        let (devices, local_id) = {
            let state = self.state.read(cx);
            (state.devices.clone(), state.local_device_id.clone())
        };
        if let Some(choice) = self.add_wanted {
            self.show_add_device(choice, window, cx);
        }
        let viewport = window.viewport_size();
        let dialog = self
            .add
            .as_ref()
            .map(|(dialog, _)| popover::modal_top("add-device-dialog", viewport, dialog.clone().into_any_element()));
        let server = self.state.read(cx).connectivity.server.clone();
        // The server's code, while it is up and the app still offers one.
        let invite = server.as_ref().and_then(|server| server.invite.clone());
        self.invite_focus.sync(invite.is_some() && self.inviting, window, cx);
        let invite_focus = self.invite_focus.handle().clone();
        let invite_dialog = invite.as_ref().filter(|_| self.inviting).map(|invite| {
            let link = invite.link.clone();
            let card = popover::dialog_card(&theme)
                .w(px(360.0))
                .id("invite-card")
                .track_focus(&invite_focus)
                .role(gpui::Role::Dialog)
                .aria_label(INVITE)
                .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                    if event.keystroke.key == "escape" {
                        this.inviting = false;
                        cx.notify();
                        cx.stop_propagation();
                    }
                }))
                .child(popover::dialog_title(&theme, INVITE))
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
                                .map(|el| crate::surface_chrome::focus_ring(el, &theme))
                                .on_click(cx.listener(move |_, _, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(link.clone()));
                                    crate::toast::success(cx, "Link copied");
                                }))
                        }))
                        // A dialog commits with the solid button, as Add device does.
                        .child(
                            widgets::text_action(&theme, widgets::ActionTone::Solid, "Done")
                                .id("invite-done")
                                .tab_index(0)
                                .role(gpui::Role::Button)
                                .aria_label("Done")
                                .map(|el| crate::surface_chrome::focus_ring(el, &theme))
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
            let id_copied = crate::motion::copied(&copied_key(&device.id), cx);
            let copy_id = device.id.clone();
            // A machine whose worker has not said what it runs shows no platform.
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
            // What the app offers to do to the machine, before Copy ID.
            let actions = device.actions.iter().enumerate().map(|(n, action)| {
                let (id, run) = (device.id.clone(), action.id.clone());
                widgets::text_action(&theme, action_tone(&action.id), action.label.clone())
                    .id(SharedString::from(format!("device-action-{ix}-{n}")))
                    .tab_index(0)
                    .role(gpui::Role::Button)
                    .aria_label(format!("{} {}", action.label, device.name))
                    .map(|el| crate::surface_chrome::focus_ring(el, &theme))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.run_action(id.clone(), run.clone(), cx);
                    }))
            });
            widgets::card_row(&theme, first)
                // Every row leads with its tile, so the blocks share one text edge.
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
                            // A row's action is its words alone, as its neighbours'.
                            widgets::action_button(&theme, widgets::ActionTone::Quiet)
                                .child(SharedString::from(if id_copied { "Copied" } else { "Copy ID" }))
                            .id(("device-id", ix))
                            .tab_index(0)
                            .role(gpui::Role::Button)
                            .tooltip(widgets::text_tooltip(if id_copied { "Copied" } else { "Copy this device's ID" }))
                            .tooltip_show_delay(widgets::TOOLTIP_DELAY)
                            .aria_label(format!("Copy device ID {}", device.id))
                            .map(|el| crate::surface_chrome::focus_ring(el, &theme))
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.copy_id(copy_id.clone(), cx);
                                },
                            )),
                        ),
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
        // The other machines, then Add device as the block's last row.
        let mut others_block = widgets::section_card(&theme).mt(px(8.0));
        let first_add = others.is_empty();
        for (n, (ix, device)) in others.into_iter().enumerate() {
            others_block = others_block.child(device_row(ix, device, n == 0));
        }
        // The row is the button, its words saying what it adds; a second "Add device"
        // beside them said it twice.
        let others_block = others_block.child(
            div()
                .id("device-add")
                .cursor_pointer()
                .tab_index(0)
                .role(gpui::Role::Button)
                .aria_label("Add a device")
                .bg(crate::motion::hover_blend("device-add", crate::theme::wash(0.0), theme.glass_hover()))
                .hover_fade("device-add")
                .map(|el| crate::surface_chrome::focus_ring_inset(el, &theme))
                .on_click(cx.listener(|this, _, window, cx| {
                    this.show_add_device(Choice::ThisMac, window, cx);
                }))
                .child(
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
                        ),
                ),
        );
        // The server this app follows, and a way to change it.
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
                            .map(|el| crate::surface_chrome::focus_ring(el, &theme))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.run_action("server".to_owned(), run.clone(), cx);
                            }))
                    }))
                    // The code a phone or iPad scans, where this device shows one.
                    .when(invite.is_some(), |row| {
                        row.child(
                            widgets::text_action(&theme, widgets::ActionTone::Quiet, INVITE)
                                .id("device-server-invite")
                                .flex_none()
                                .tab_index(0)
                                .role(gpui::Role::Button)
                                .aria_label(INVITE)
                                .map(|el| crate::surface_chrome::focus_ring(el, &theme))
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
                            .map(|el| crate::surface_chrome::focus_ring(el, &theme))
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
            // Slopty lists every machine its server knows.
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
                                // Slopty's machines, not a synced registry; one page head.
                                .child(widgets::page_head(&theme, "Devices", None, DEVICES_SUBTITLE, None))
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

    // Copy ID keeps its words; its glyph shows the check for the shared span (M5).
    #[gpui::test]
    fn copying_an_id_marks_it_copied_for_the_span(cx: &mut gpui::TestAppContext) {
        use gpui::AppContext as _;
        cx.update(|cx| cx.set_global(crate::theme::Theme::default()));
        let state = cx.new(|_| AppState::new());
        let page = cx.new(|cx| DevicesPage::new(state, cx));
        page.update(cx, |page, cx| page.copy_id("studio".into(), cx));
        assert_eq!(cx.read_from_clipboard().and_then(|item| item.text()).as_deref(), Some("studio"));
        assert!(cx.update(|cx| crate::motion::copied(&copied_key("studio"), cx)));
        cx.executor().advance_clock(crate::motion::COPIED_FOR);
        cx.run_until_parked();
        assert!(!cx.update(|cx| crate::motion::copied(&copied_key("studio"), cx)));
    }

    /// The server's code opens as a dialog that holds the keyboard, where Escape closes it, and
    /// closing gives the keyboard back to what had it.
    #[gpui::test]
    fn the_invite_dialog_takes_the_keyboard_and_gives_it_back(cx: &mut gpui::TestAppContext) {
        use gpui::AppContext as _;
        struct Host {
            page: Entity<DevicesPage>,
            before: gpui::FocusHandle,
        }
        impl Render for Host {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                div().size_full().track_focus(&self.before).child(self.page.clone())
            }
        }
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(crate::theme::Theme::default());
        });
        let (host, cx) = cx.add_window_view(|_, cx| {
            let state = cx.new(|_| {
                let mut state = AppState::new();
                state.connectivity.server = Some(slopty_view_proto::ServerPresence {
                    name: "studio".into(),
                    online: true,
                    invite: Some(slopty_view_proto::ServerInvite {
                        link: Some("slopty://connect?server=studio:7400".into()),
                        address: Some("studio:7400".into()),
                        note: "Scan it with the phone".into(),
                    }),
                    ..Default::default()
                });
                state
            });
            Host { page: cx.new(|cx| DevicesPage::new(state, cx)), before: cx.focus_handle() }
        });
        let (page, before) = host.read_with(cx, |host, _| (host.page.clone(), host.before.clone()));
        cx.update(|window, cx| {
            window.set_a11y_active(true);
            window.focus(&before, cx);
        });
        cx.run_until_parked();
        page.update(cx, |page, cx| page.open_invite(cx));
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        });
        let (found, focused) = cx.update(|window, _| {
            let update = window.a11y_tree().expect("a tree");
            let dialog = update.nodes.iter().find(|(_, node)| {
                node.role() == gpui::Role::Dialog && node.label() == Some("Connect a phone or iPad")
            });
            (dialog.is_some(), dialog.is_some_and(|(id, _)| *id == update.focus))
        });
        assert!(found, "the dialog has its node");
        assert!(focused, "the dialog holds the keyboard");
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert!(!page.read_with(cx, |page, _| page.inviting), "Escape closes it");
        cx.update(|window, _| assert!(before.is_focused(window), "the keyboard back where it was"));
    }

    /// The Add device dialog keeps its top edge as its tabs change what it holds: only its foot
    /// moves.
    #[gpui::test]
    fn the_add_device_dialog_keeps_its_top_across_tabs(cx: &mut gpui::TestAppContext) {
        use gpui::AppContext as _;
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(crate::theme::Theme::default());
            cx.set_reduce_motion(true);
        });
        let (page, cx) = cx.add_window_view(|_, cx| {
            let state = cx.new(|_| AppState::new());
            DevicesPage::new(state, cx)
        });
        cx.simulate_resize(gpui::size(px(1320.0), px(880.0)));
        page.update(cx, |page, cx| page.open_add_device_on(Choice::ThisMac, cx));
        let dialog = |cx: &mut gpui::VisualTestContext| {
            cx.run_until_parked();
            cx.update(|window, cx| {
                window.set_a11y_active(true);
                window.refresh();
                window.draw(cx).clear(cx);
                let update = window.a11y_tree().expect("a tree");
                let scale = f64::from(window.scale_factor());
                let at = |label: &str, role: gpui::Role| {
                    update.nodes.iter().find(|(_, n)| n.role() == role && n.label() == Some(label)).map(|(_, n)| {
                        let r = n.bounds().unwrap_or_default();
                        (r.y0 / scale, r.y1 / scale, (r.x0 + r.x1) / 2.0 / scale, (r.y0 + r.y1) / 2.0 / scale)
                    })
                };
                (at("Add device", gpui::Role::Dialog).expect("the dialog"), at("A server", gpui::Role::Tab))
            })
        };
        let (mac, server_tab) = dialog(cx);
        let (_, _, x, y) = server_tab.expect("the server's tab");
        cx.simulate_click(gpui::point(px(x as f32), px(y as f32)), gpui::Modifiers::default());
        let (server, _) = dialog(cx);
        assert!((mac.0 - server.0).abs() < 0.5, "the top stays: {mac:?} {server:?}");
        assert!((mac.0 - (880.0 - 480.0) / 2.0).abs() < 1.0, "where the palettes stand: {mac:?}");
    }

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

    // A machine that stops answering turns from Online to "Last seen" past the window,
    // by the page's own clock, which then moves on as the words do.
    #[gpui::test]
    fn a_silent_machine_goes_offline_by_the_page_s_own_clock(cx: &mut gpui::TestAppContext) {
        use gpui::AppContext as _;
        let now = cx.update(|cx| crate::clock::utc(cx));
        let seen = now - TimeDelta::seconds(60);
        let state = cx.new(|_| {
            let mut state = AppState::new();
            state.local_device_id = Some("here".into());
            state.devices = vec![
                serde_json::from_value(serde_json::json!({
                    "id": "here", "name": "Studio", "platform": "macos", "lastSeenAt": now - TimeDelta::days(3),
                }))
                .unwrap(),
                serde_json::from_value(serde_json::json!({
                    "id": "box", "name": "Box", "platform": "linux", "lastSeenAt": seen,
                }))
                .unwrap(),
            ];
            state
        });
        let page = cx.new(|cx| DevicesPage::new(state, cx));
        let drawn = std::rc::Rc::new(std::cell::Cell::new(0));
        let _drawn = cx.update(|cx| {
            let drawn = drawn.clone();
            cx.observe(&page, move |_, _| drawn.set(drawn.get() + 1))
        });
        let says = |cx: &mut gpui::TestAppContext| {
            page.read_with(cx, |page, _| {
                if device_online(Some(seen), page.clock) {
                    "Online".to_owned()
                } else {
                    format_last_seen(Some(seen), page.clock)
                }
            })
        };
        assert_eq!(says(cx), "Online");
        cx.executor().advance_clock(std::time::Duration::from_secs(10));
        assert_eq!((says(cx), drawn.get()), ("Online".to_owned(), 0), "nothing moves inside the window");
        cx.executor().advance_clock(std::time::Duration::from_secs(2));
        assert_eq!((says(cx), drawn.get()), ("1m ago".to_owned(), 1), "past the window, and the page drew");
        cx.executor().advance_clock(std::time::Duration::from_secs(60));
        assert_eq!((says(cx), drawn.get()), ("2m ago".to_owned(), 2), "the minute turned; this machine's own time never ticks");
    }

    #[test]
    fn presence_changes_when_its_words_do() {
        let now = Utc::now();
        assert_eq!(presence_changes(None, now), None);
        let seen = now - TimeDelta::seconds(10);
        assert_eq!(presence_changes(Some(seen), now), Some(seen + TimeDelta::seconds(71)));
        for age in [71, 90, 59 * 60 + 59, 3600, 5 * 3600 + 1, 86_400, 3 * 86_400 + 7] {
            let seen = now - TimeDelta::seconds(age);
            let next = presence_changes(Some(seen), now).unwrap();
            let shown = format_last_seen(Some(seen), now);
            assert_eq!(format_last_seen(Some(seen), next - TimeDelta::seconds(1)), shown, "{age}s: the same until then");
            assert_ne!(format_last_seen(Some(seen), next), shown, "{age}s: different from then");
        }
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

    // One tile per machine row, by platform; an unknown platform still gets one.
    #[test]
    fn every_machine_row_has_a_tile_by_platform() {
        assert_eq!(platform_icon("macos"), crate::icons::LAPTOP);
        assert_eq!(platform_icon("darwin"), crate::icons::LAPTOP);
        assert_eq!(platform_icon("linux"), crate::icons::REMOTE_SERVER);
        assert_eq!(platform_icon("ios"), crate::icons::SMARTPHONE);
        assert_eq!(platform_icon(""), crate::icons::REMOTE_SERVER, "not said yet");
    }

    // Only the confirmed removal is red; asking first is quiet.
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
