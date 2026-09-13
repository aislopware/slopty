//! Several hosts in one app: one link and one canvas per paired host, one shown at a time.

use std::time::Duration;

use gpui::Entity;
use slopty_client::HostLink;
use slopty_net::EndpointId;
use slopty_ui::canvas::CanvasView;

/// GPUI actions for the host switcher.
pub mod actions {
    #![expect(
        clippy::derive_partial_eq_without_eq,
        reason = "gpui::actions! derives PartialEq only"
    )]
    use gpui::actions;

    actions!(
        hosts,
        [
            /// Show the next host's canvas.
            NextHost,
            /// Show the previous host's canvas.
            PrevHost,
            /// Open the pairing panel to add a host.
            AddHost,
            /// Forget the host being shown.
            ForgetHost,
        ]
    );
}

/// Where a host's link stands.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum HostStatus {
    /// First attempt in progress.
    Connecting,
    /// Link up, canvas shown.
    Connected,
    /// Link lost or the attempt failed; retrying with the reason.
    Reconnecting(String),
    /// The host no longer knows this client: pair again with a fresh ticket.
    NeedsPairing,
}

impl HostStatus {
    /// Short text for the bar and the switcher rows.
    #[must_use]
    pub fn text(&self) -> String {
        match self {
            Self::Connecting => "connecting…".to_owned(),
            Self::Connected => "connected".to_owned(),
            Self::Reconnecting(why) => format!("{why}; reconnecting…"),
            Self::NeedsPairing => "needs pairing".to_owned(),
        }
    }
}

/// One paired host as the workspace sees it.
pub struct HostSlot {
    /// Transport identity (the pairing store's key).
    pub id: EndpointId,
    /// Display name (from the pairing, refreshed by each `HelloAck`).
    pub name: String,
    /// Link state.
    pub status: HostStatus,
    /// Its canvas while the link is up.
    pub canvas: Option<Entity<CanvasView>>,
    /// Camera zoom of its canvas (the bar's readout).
    pub zoom: f32,
    /// Agents on this host waiting on the human.
    pub needs_you: usize,
    /// Link RTT, relay use and silence, sampled once a second while connected.
    pub rtt: Option<Duration>,
    /// Whether the path goes through a relay.
    pub relayed: Option<bool>,
    /// How long the host has sent nothing, once past the warning bar.
    pub silent: Option<Duration>,
    /// The live link, to abandon it when the host is forgotten.
    pub link: Option<std::sync::Weak<HostLink>>,
    /// Canvas subscriptions; dropped with the canvas.
    pub subscriptions: Vec<gpui::Subscription>,
    /// Where the last canvas was when the link dropped (camera, active card), for the next
    /// one to resume at.
    pub resume: Option<(slopty_client::canvas::Camera, Option<slopty_core::ItemId>)>,
}

impl std::fmt::Debug for HostSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostSlot")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("status", &self.status)
            .finish_non_exhaustive()
    }
}

impl HostSlot {
    /// A slot for a paired host, before its first connection attempt.
    #[must_use]
    pub const fn new(id: EndpointId, name: String) -> Self {
        Self {
            id,
            name,
            status: HostStatus::Connecting,
            canvas: None,
            zoom: 1.0,
            needs_you: 0,
            rtt: None,
            relayed: None,
            silent: None,
            link: None,
            subscriptions: Vec::new(),
            resume: None,
        }
    }

    /// Drop the canvas and everything that hung off the link, keeping where the canvas was
    /// (`place`, from [`CanvasView::camera`] and `active_item`) for the next one.
    pub fn disconnect(
        &mut self,
        status: HostStatus,
        place: Option<(slopty_client::canvas::Camera, Option<slopty_core::ItemId>)>,
    ) {
        if place.is_some() {
            self.resume = place;
        }
        self.canvas = None;
        self.subscriptions.clear();
        self.link = None;
        self.needs_you = 0;
        self.rtt = None;
        self.relayed = None;
        self.silent = None;
        self.status = status;
    }
}
