//! Sleep policy for the host.
//!
//! The machine stays awake while a client is attached, and the display stays on while a window
//! or display is being streamed. A host that idles to sleep drops every session; a display that
//! sleeps captures nothing. Nothing is held when nobody is connected, so an unattended host
//! sleeps as its owner set it to. The policy is pure and counts; the assertions behind
//! [`Holds`] are the daemon's (`NSProcessInfo` activities through `slopty_platform::Activity`).

/// The two operating-system assertions the policy toggles.
pub trait Holds: Send {
    /// Hold (or release) the machine out of idle sleep.
    fn system(&mut self, hold: bool);
    /// Hold (or release) the display out of idle sleep.
    fn display(&mut self, hold: bool);
}

impl Holds for Box<dyn Holds> {
    fn system(&mut self, hold: bool) {
        (**self).system(hold);
    }

    fn display(&mut self, hold: bool) {
        (**self).display(hold);
    }
}

/// Attached clients and live streams, and the holds they imply.
pub struct Wake<H> {
    clients: usize,
    streams: usize,
    holds: H,
}

impl<H> std::fmt::Debug for Wake<H> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Wake")
            .field("clients", &self.clients)
            .field("streams", &self.streams)
            .finish_non_exhaustive()
    }
}

impl<H: Holds> Wake<H> {
    /// Nobody attached, nothing held.
    pub const fn new(holds: H) -> Self {
        Self { clients: 0, streams: 0, holds }
    }

    /// A client connected: the first one holds the machine awake.
    pub fn client_joined(&mut self) {
        self.clients = self.clients.saturating_add(1);
        if self.clients == 1 {
            self.holds.system(true);
        }
    }

    /// A client left: the last one lets the machine sleep again.
    pub fn client_left(&mut self) {
        self.clients = self.clients.saturating_sub(1);
        if self.clients == 0 {
            self.holds.system(false);
        }
    }

    /// How many streams are live now; the display is held while it is not zero.
    pub fn streams(&mut self, live: usize) {
        let was = self.streams;
        self.streams = live;
        match (was, live) {
            (0, 1..) => self.holds.display(true),
            (1.., 0) => self.holds.display(false),
            _ => {}
        }
    }

    /// `(clients, streams)` right now.
    #[must_use]
    pub const fn counts(&self) -> (usize, usize) {
        (self.clients, self.streams)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Log(Vec<&'static str>);

    impl Holds for Log {
        fn system(&mut self, hold: bool) {
            self.0.push(if hold { "system on" } else { "system off" });
        }

        fn display(&mut self, hold: bool) {
            self.0.push(if hold { "display on" } else { "display off" });
        }
    }

    /// Only the edges toggle the assertion: a second client changes nothing, and the machine is
    /// released when the last one leaves, not before.
    #[test]
    fn the_first_client_holds_the_machine_and_the_last_releases_it() {
        let mut wake = Wake::new(Log::default());
        wake.client_joined();
        wake.client_joined();
        wake.client_left();
        assert_eq!(wake.holds.0, ["system on"]);
        wake.client_left();
        assert_eq!(wake.holds.0, ["system on", "system off"]);
        assert_eq!(wake.counts(), (0, 0));
    }

    /// The display follows the live-stream count from the registry, with the same edge rule.
    #[test]
    fn the_display_is_held_while_any_stream_is_live() {
        let mut wake = Wake::new(Log::default());
        wake.streams(1);
        wake.streams(2);
        wake.streams(1);
        assert_eq!(wake.holds.0, ["display on"]);
        wake.streams(0);
        wake.streams(0);
        assert_eq!(wake.holds.0, ["display on", "display off"]);
    }

    /// A stray extra `client_left` (a connection torn down twice) cannot underflow or release
    /// what a still-attached client holds.
    #[test]
    fn leaving_more_often_than_joining_is_harmless() {
        let mut wake = Wake::new(Log::default());
        wake.client_left();
        wake.client_joined();
        assert_eq!(wake.counts(), (1, 0));
        assert_eq!(wake.holds.0, ["system off", "system on"]);
    }
}
