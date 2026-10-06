//! Where the person is, told to the server, and what the server says back about it
//! (`docs/decisions/ui.md`, "Notifications by presence" and "the server's notices lead").
//!
//! The workspace reads where the person is ([`WorkspaceView::presence`]); the app sends it on
//! the window's activation and resignation, after any change to the workspace (the workspace
//! in front, the tiles on screen, the focus), and on a slow clock that also lowers `active`
//! once this Mac has gone unused a while. The server's list of every client's presence decides
//! whether a handheld stays quiet, and its notices are the only agent moments that post.
//!
//! [`WorkspaceView::presence`]: slopty_ui::workspace::WorkspaceView::presence

use std::time::Duration;

use gpui::{Context, Window};
use slopty_proto::thread::attention::{Notice, NoticeKind, Presence, Present, Seat};

use crate::{Workspace, alert, settings};

/// How long this Mac goes with no input before the person counts as away from it.
#[cfg(target_os = "macos")]
const AWAY_AFTER: Duration = Duration::from_secs(120);

/// How often the away clock is read and where the person is told again: a change the
/// workspace made without telling (a tile scrolled into view) goes up by then at the latest.
const LOOK_EVERY: Duration = Duration::from_secs(5);

/// A change in the workspace goes up this long after it, once the frame that drew it is down,
/// and a burst of changes (a strip animating) goes up once.
const SETTLE: Duration = Duration::from_millis(150);

/// The app's side of the person's presence.
#[derive(Debug)]
pub(crate) struct Presenting {
    /// A send is waiting out [`SETTLE`].
    settling: bool,
    /// Whether the app still hears notices on its link: a phone stops just before the system
    /// suspends it.
    listening: bool,
}

impl Default for Presenting {
    fn default() -> Self {
        Self { settling: false, listening: true }
    }
}

impl Presenting {
    /// Whether the app still hears notices on its link, as the next presence says.
    pub(crate) const fn listening(&self) -> bool {
        self.listening
    }

    /// The app stops hearing notices on its link, or hears them again.
    #[cfg(target_os = "ios")]
    pub(crate) const fn set_listening(&mut self, listening: bool) {
        self.listening = listening;
    }
}

/// Whether the person is at another of their devices: this client is carried, and another
/// one, on a desk, is in use. `me` is the server's number for this client's link.
fn elsewhere(me: Option<u64>, mine: Option<&Presence>, present: &[Present]) -> bool {
    mine.is_some_and(|p| p.seat == Seat::Handheld)
        && present.iter().any(|other| {
            Some(other.link) != me && other.presence.seat == Seat::Desk && other.presence.active
        })
}

impl Workspace {
    /// Tell the server where the person is on a slow clock, for as long as the app runs.
    pub(crate) fn watch_presence(cx: &Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(LOOK_EVERY).await;
                if this.update(cx, |ws, cx| ws.tell_presence(cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    /// The workspace changed: tell the server once it is drawn.
    pub(crate) fn presence_changed(&mut self, cx: &Context<Self>) {
        if self.presenting.settling || self.server_caller().is_none() {
            return;
        }
        self.presenting.settling = true;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SETTLE).await;
            let _gone = this.update(cx, |ws, cx| {
                ws.presenting.settling = false;
                ws.tell_presence(cx);
            });
        })
        .detach();
    }

    /// Tell the server where the person is, read off the main window.
    pub(crate) fn tell_presence(&self, cx: &mut Context<Self>) {
        let (Some(caller), Some(handle)) = (self.server_caller(), self.window) else { return };
        let view = self.view.clone();
        let Ok(presence) = handle.update(cx, |_root, window, cx| view.read(cx).presence(window))
        else {
            return;
        };
        caller.presence(present_now(presence, self.presenting.listening()));
    }

    /// Tell the server where the person is, from inside `window`'s own callbacks.
    pub(crate) fn tell_presence_in(&self, window: &Window, cx: &Context<Self>) {
        if let Some(caller) = self.server_caller() {
            let presence = self.view.read(cx).presence(window);
            caller.presence(present_now(presence, self.presenting.listening()));
        }
    }

    /// The server's list of every client and where the person is on it.
    pub(crate) fn heard_present(&mut self, present: &[Present]) {
        let me = match self.directory.server() {
            slopty_client::directory::ServerState::Linked { link, .. } => Some(*link),
            _ => None,
        };
        let mine = self.server_caller().and_then(|c| c.presence_said());
        self.attention.set_present_elsewhere(elsewhere(me, mine.as_ref(), present));
    }

    /// The server picked this client to say `notice`. A project's, with the app in front, is a
    /// notice in the workspace rather than a note.
    pub(crate) fn heard_notice(&mut self, notice: &Notice, cx: &mut Context<Self>) {
        let Some(heard) = self.view.read(cx).heard(notice) else { return };
        if heard.stack.is_some() && self.attention.active() {
            self.show_notice(format!("{}: {}", heard.title, heard.body), cx);
            return;
        }
        let in_front = cx.active_window().is_some();
        if heard.kind == NoticeKind::NeedsYou && settings::alerts(&self.settings, in_front) {
            alert();
        }
        self.attention.notice(&heard);
    }

    /// The server link came up or went down: while it is up, its notices lead and its list of
    /// clients says whether the person is elsewhere.
    pub(crate) fn server_leads(&mut self, linked: bool, cx: &mut Context<Self>) {
        self.attention.set_server_led(linked);
        if linked {
            self.tell_presence(cx);
        } else {
            self.attention.set_present_elsewhere(false);
        }
    }
}

/// `presence` as the app says it: `listening` or not, and with `active` lowered once this Mac
/// has gone unused past [`AWAY_AFTER`]. On an iPhone or iPad the system locks the screen
/// itself, which resigns the app.
#[cfg(target_os = "macos")]
fn present_now(mut presence: Presence, listening: bool) -> Presence {
    presence.listening = listening;
    if slopty_platform::idle::since_input() >= AWAY_AFTER {
        presence.active = false;
    }
    presence
}

#[cfg(not(target_os = "macos"))]
const fn present_now(mut presence: Presence, listening: bool) -> Presence {
    presence.listening = listening;
    presence
}

#[cfg(test)]
mod tests {
    use super::*;

    fn presence(seat: Seat, active: bool) -> Presence {
        Presence {
            seat,
            active,
            workspace: None,
            showing: Vec::new(),
            focus: None,
            listening: true,
        }
    }

    fn client(link: u64, seat: Seat, active: bool) -> Present {
        Present { link, name: format!("client {link}"), presence: presence(seat, active) }
    }

    #[test]
    fn a_handheld_is_quiet_while_a_desk_is_in_use() {
        let phone = presence(Seat::Handheld, true);
        let at_desk = [client(1, Seat::Handheld, true), client(2, Seat::Desk, true)];
        assert!(elsewhere(Some(1), Some(&phone), &at_desk), "the Mac is in use");
        let left_desk = [client(1, Seat::Handheld, true), client(2, Seat::Desk, false)];
        assert!(!elsewhere(Some(1), Some(&phone), &left_desk), "the Mac is not");
        let mac = presence(Seat::Desk, true);
        assert!(!elsewhere(Some(2), Some(&mac), &at_desk), "a desk is never quieted");
        let two_desks = [client(2, Seat::Desk, true), client(3, Seat::Desk, true)];
        assert!(!elsewhere(Some(2), Some(&mac), &two_desks), "nor by another desk");
        let only_me = [client(1, Seat::Handheld, true)];
        assert!(!elsewhere(Some(1), Some(&phone), &only_me), "its own entry is not another");
        assert!(!elsewhere(Some(1), None, &at_desk), "nothing said yet");
    }
}
