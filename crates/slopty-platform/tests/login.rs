//! Opening the app at login (`slopty_platform::login`), read only: registering would post the
//! system's "Login item added" note and leave a login item behind.

#![cfg(target_os = "macos")]

#[cfg(test)]
mod tests {
    use slopty_platform::login::{Login, status};

    /// A test binary is no app bundle, so the system has no login item to make of it, and says
    /// so the same way every time it is asked.
    #[test]
    fn a_binary_outside_a_bundle_cannot_open_at_login() {
        let first = status();
        assert_eq!(first, Login::Unavailable, "a test binary is no app");
        assert_eq!(status(), first, "the same answer twice");
        assert!(!first.on());
    }
}
