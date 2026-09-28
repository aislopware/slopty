//! Whether this process may read the general pasteboard without macOS asking the person first.
//!
//! Since macOS 15.4 a program that reads the general pasteboard's contents without a paste the
//! person made raises the paste alert, unless the person allowed it in System Settings
//! (`NSPasteboardAccessBehavior`, AppKit's `NSPasteboard.h`). A worker keeping the clipboard in
//! step reads it on every change, so it reads only while [`general`] says reads are free, and
//! otherwise reports that clipboard reads need permission (`docs/decisions/platform.md`,
//! "The worker's pasteboard alert").
//!
//! Reading the behaviour reads no contents: `accessBehavior` is a property of the pasteboard, and
//! `NSPasteboard.h` puts the alert on "programmatic pasteboard access", the contents. Named
//! pasteboards "default to always allow access". Linux and iOS have no such state; there the
//! answer is always [`Access::Allowed`] (iOS asks per paste, which
//! `pasteboard::Pasteboard::reads_ask` covers).

/// How reading the general pasteboard's contents goes for this process.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Access {
    /// Reads go through without a word: allowed in System Settings, or no such alert here.
    Allowed,
    /// No read has raised the alert yet; the first one will, and the person must answer it.
    NotAskedYet,
    /// Every read asks the person first; a paste they make does not.
    Asks,
    /// Every read is refused without a word; a paste they make is still let through.
    Denied,
}

impl Access {
    /// Whether a read the person did not make (keeping a clipboard in step) goes through
    /// without asking them.
    #[must_use]
    pub const fn reads_freely(self) -> bool {
        matches!(self, Self::Allowed)
    }

    /// What a health report says when reads are not free, naming where the person fixes it;
    /// `None` when they are.
    #[must_use]
    pub const fn problem(self) -> Option<&'static str> {
        match self {
            Self::Allowed => None,
            Self::NotAskedYet | Self::Asks => Some(
                "clipboard reads need permission: allow pasting from other apps for \
                 slopty-worker in System Settings > Privacy & Security",
            ),
            Self::Denied => Some(
                "clipboard reads are denied: allow pasting from other apps for slopty-worker \
                 in System Settings > Privacy & Security",
            ),
        }
    }
}

/// How reading the general pasteboard goes for this process now. Reads no contents and never
/// raises the alert; any thread.
#[must_use]
#[cfg_attr(
    not(target_os = "macos"),
    expect(clippy::missing_const_for_fn, reason = "constant here")
)]
pub fn general() -> Access {
    #[cfg(target_os = "macos")]
    {
        of(&objc2_app_kit::NSPasteboard::generalPasteboard())
    }
    #[cfg(not(target_os = "macos"))]
    {
        Access::Allowed
    }
}

/// How reading `board` goes for this process.
#[cfg(target_os = "macos")]
fn of(board: &objc2_app_kit::NSPasteboard) -> Access {
    use objc2_app_kit::NSPasteboardAccessBehavior as Behavior;
    match board.accessBehavior() {
        Behavior::AlwaysAllow => Access::Allowed,
        Behavior::Ask => Access::Asks,
        Behavior::AlwaysDeny => Access::Denied,
        // `Default` and any value a later SDK adds: the first read will ask.
        _ => Access::NotAskedYet,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only an allowed read is free, and every other state names its remedy.
    #[test]
    fn only_allowed_reads_are_free_and_the_rest_say_why() {
        for access in [Access::NotAskedYet, Access::Asks, Access::Denied] {
            assert!(!access.reads_freely(), "{access:?}");
            let problem = access.problem().unwrap_or_default();
            assert!(problem.contains("Privacy & Security"), "{access:?}: {problem}");
        }
        assert!(Access::Allowed.reads_freely());
        assert_eq!(Access::Allowed.problem(), None);
    }

    /// A pasteboard of the test's own name always allows reads, as `NSPasteboard.h` says of
    /// every pasteboard but the general one; the general one is never touched here.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_named_pasteboard_always_allows_reads() {
        let name = format!("com.aislopware.slopty.test.access.{}", std::process::id());
        let board = objc2_app_kit::NSPasteboard::pasteboardWithName(
            &objc2_foundation::NSString::from_str(&name),
        );
        assert_eq!(of(&board), Access::Allowed);
        // SAFETY: AppKit rule: `releaseGlobally` may be sent to any pasteboard; the object is
        // not used after it.
        unsafe {
            let () = objc2::msg_send![&*board, releaseGlobally];
        }
    }

    /// Whether this process reading the general pasteboard raises macOS's paste alert: the
    /// behaviour before and after one read of its contents. Live because the read may raise
    /// the alert on the screen of whoever is at this Mac, so it runs only by hand, on a Mac
    /// nobody is using (`cargo test -p slopty-platform --lib general_pasteboard -- --ignored
    /// --nocapture`). A read that turns "not asked yet" into "asks" raised the alert.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "live: reads the general pasteboard, which may raise the paste alert on this Mac"]
    fn reading_the_general_pasteboard_is_free_when_it_says_so() {
        use crate::pasteboard::{MacPasteboard, Pasteboard as _};

        let before = general();
        let board = MacPasteboard::general();
        let types = board.types();
        let read = types.first().and_then(|uti| board.data(uti)).map(|bytes| bytes.len());
        let after = general();
        println!(
            "general pasteboard: {before:?} -> {after:?}, {} types, first read {read:?}",
            types.len()
        );
        assert!(
            !(before == Access::NotAskedYet && after == Access::Asks),
            "the read raised the paste alert: reads must wait for permission ({before:?} -> {after:?})"
        );
        if before.reads_freely() && !types.is_empty() {
            assert!(read.is_some(), "an allowed read came back empty");
        }
    }
}
