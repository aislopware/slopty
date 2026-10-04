//! A finish the person put off until later, held by the server so every client and a restart
//! agree (`docs/decisions/ui.md`, "Snooze is the server's, with presets").
//!
//! The person snoozes a finish from the inbox with [`crate::orchestration::Verb::Snooze`],
//! picking a preset ([`Until`]) that the server turns into a wall time in their zone. The
//! server keeps every snooze across restarts and sends the whole list to each client
//! ([`crate::server::FromServer::Snoozes`]) on connecting and on every change. A snooze ends
//! at its time, when the person ends it ([`crate::orchestration::Verb::Unsnooze`]), or early,
//! when its thread has news: it comes to need the person, fails, or finishes again. A thread
//! that needs the person now is never snoozed.

use serde::{Deserialize, Serialize};
use slopty_core::WallMs;

use crate::orchestration::TermRef;
use crate::thread::attention::ThreadAt;

/// The hour "This evening" wakes at, in the person's zone.
pub const EVENING_HOUR: i8 = 18;
/// The hour "Tomorrow" wakes at, in the person's zone.
pub const MORNING_HOUR: i8 = 9;
/// "This evening" is offered only while evening is more than this far off, in minutes.
pub const EVENING_LEAD_MIN: i64 = 60;
/// The most snoozes the server keeps; a new one past it is refused.
pub const SNOOZES_MAX: usize = 4096;

/// What is snoozed.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum SnoozeOf {
    /// An agent's thread, wherever it shows: the inbox row, its tile, the rail.
    Thread(ThreadAt),
    /// A terminal's finish: a shell's command, or an agent known only by its tile.
    Tile(TermRef),
}

/// Until when: a preset, or a time the person picked.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Until {
    /// An hour from now.
    InAnHour,
    /// [`EVENING_HOUR`] today, while that is more than [`EVENING_LEAD_MIN`] off.
    ThisEvening,
    /// [`MORNING_HOUR`] tomorrow.
    Tomorrow,
    /// This time, which must be ahead.
    At(WallMs),
}

/// A snooze the server keeps.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Snooze {
    /// What it hides.
    pub of: SnoozeOf,
    /// When it ends, by the server's clock.
    pub until_ms: WallMs,
    /// When the person set it.
    pub since_ms: WallMs,
}

impl Snooze {
    /// Whether it still holds at `now`.
    #[must_use]
    pub fn holds(&self, now: WallMs) -> bool {
        now < self.until_ms
    }
}
