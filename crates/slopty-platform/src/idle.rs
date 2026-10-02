//! How long since the person last used this Mac: a key, a click, a move or a scroll, in any app.
//!
//! The client says it is in use only while this is short, so the server sends a notice where
//! the person is rather than to a Mac left on in an empty room.
//!
//! The session's combined event state is the window server's own record, read without
//! Accessibility or Input Monitoring.

use std::time::Duration;

use objc2_core_graphics::{CGEventSource, CGEventSourceStateID, CGEventType};

/// `kCGAnyInputEventType` (`<CoreGraphics/CGEventTypes.h>`), a macro objc2 does not bind: every
/// input event type at once.
const ANY_INPUT: CGEventType = CGEventType(!0);

/// The time since the last input event in this login session.
#[must_use]
pub fn since_input() -> Duration {
    let seconds = CGEventSource::seconds_since_last_event_type(
        CGEventSourceStateID::CombinedSessionState,
        ANY_INPUT,
    );
    Duration::try_from_secs_f64(seconds).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_session_s_last_input_is_read() {
        // A finite time however long ago the machine was touched, so the clamp to zero in
        // `since_input` never hides a wrong event type or state.
        let seconds = CGEventSource::seconds_since_last_event_type(
            CGEventSourceStateID::CombinedSessionState,
            ANY_INPUT,
        );
        assert!(seconds.is_finite() && seconds >= 0.0, "{seconds}");
    }
}
