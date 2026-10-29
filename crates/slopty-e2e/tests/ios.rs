//! The iOS app on the simulator, driven through its own test socket.
//!
//! Live (`#[ignore]`), run by `cargo xtask e2e ios [--sim iphone|ipad]`: the daemons
//! run on the Mac as for the app self-test, the app runs in the booted simulator named by
//! `SLOPTY_SIM_UDID` and binds its socket on the shared file system. The phone's anatomy (its
//! Chats page, a chat over the whole screen) is `compact`; its frames are compared with a
//! golden per device, `ios-phone-*.png` or `ios-pad-*.png`. A session there (its terminal, its
//! inspector, a shell, the files) is `screens`. A finger over a page
//! card, as UIKit delivers it, is `pages`.

/// The phone's anatomy: its Chats page, a chat over the whole screen.
#[cfg(test)]
#[path = "ios/compact.rs"]
mod compact;
/// A finger over a page card, as UIKit delivers it.
#[cfg(test)]
#[path = "ios/pages.rs"]
mod pages;
/// A session on the device: its terminal, its inspector, a shell, the files.
#[cfg(test)]
#[path = "ios/screens.rs"]
mod screens;
