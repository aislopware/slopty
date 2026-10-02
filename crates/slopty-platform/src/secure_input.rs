//! Secure keyboard entry: macOS secure event input, Terminal's "Secure Keyboard Entry".
//!
//! While it is on, no other program on this Mac sees the keys typed into the app: event taps
//! (a keylogger's, a launcher's, a remapper's) and global shortcuts go blind to them. The app
//! turns it on while a terminal waits for a password or a remote window's password field has
//! the keyboard, or whenever its window is in front if the person asks (`[terminal]
//! secure_keyboard_entry`), and off as soon as that ends or the app goes to the back.
//!
//! `EnableSecureEventInput` and `DisableSecureEventInput` are counted per process
//! (`<HIToolbox/CarbonEvents.h>`): every enable needs its disable, and one left over keeps
//! every other program's shortcuts dead for as long as the app runs. So it is held through one
//! [`SecureInput`], whose every turn on is matched by one turn off, when it goes off and when
//! it drops, and which never turns on twice.

/// The two calls, or a stand-in that counts them: the seam the balance is tested at.
pub trait Switch {
    /// `EnableSecureEventInput`.
    fn enable(&self);
    /// `DisableSecureEventInput`.
    fn disable(&self);
}

impl<S: Switch + ?Sized> Switch for Box<S> {
    fn enable(&self) {
        (**self).enable();
    }

    fn disable(&self) {
        (**self).disable();
    }
}

/// Secure event input, on or off, balanced.
#[derive(Debug)]
pub struct SecureInput<S: Switch = System> {
    switch: S,
    on: bool,
}

impl Default for SecureInput {
    fn default() -> Self {
        Self::new(System)
    }
}

impl<S: Switch> SecureInput<S> {
    /// Off, over `switch`.
    pub const fn new(switch: S) -> Self {
        Self { switch, on: false }
    }

    /// Turn it on or off; only a change reaches the system, so it is never on twice.
    pub fn set(&mut self, on: bool) {
        if on == self.on {
            return;
        }
        if on {
            self.switch.enable();
        } else {
            self.switch.disable();
        }
        self.on = on;
    }

    /// Whether it is on.
    #[must_use]
    pub const fn is_on(&self) -> bool {
        self.on
    }
}

impl<S: Switch> Drop for SecureInput<S> {
    fn drop(&mut self) {
        self.set(false);
    }
}

/// This Mac's secure event input; nothing on iOS, where a secure text field does it itself.
#[derive(Clone, Copy, Debug, Default)]
pub struct System;

#[cfg(target_os = "macos")]
mod carbon {
    // `<HIToolbox/CarbonEvents.h>`, in Carbon; objc2 binds none of it. Both return an
    // `OSStatus` that is `noErr` but for a count gone below zero, which `SecureInput` never
    // lets happen.
    #[link(name = "Carbon", kind = "framework")]
    unsafe extern "C" {
        pub(super) fn EnableSecureEventInput() -> i32;
        pub(super) fn DisableSecureEventInput() -> i32;
    }
}

impl Switch for System {
    fn enable(&self) {
        // SAFETY: a documented call with no arguments, safe from any thread; its count is
        // balanced by `SecureInput`, which disables once for every enable.
        #[cfg(target_os = "macos")]
        let status = unsafe { carbon::EnableSecureEventInput() };
        #[cfg(target_os = "macos")]
        tracing::debug!(status, "secure keyboard entry on");
    }

    fn disable(&self) {
        // SAFETY: as for `enable`; called only after an enable of its own.
        #[cfg(target_os = "macos")]
        let status = unsafe { carbon::DisableSecureEventInput() };
        #[cfg(target_os = "macos")]
        tracing::debug!(status, "secure keyboard entry off");
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::{SecureInput, Switch};

    /// Counts the calls as the system's per-process count does.
    #[derive(Default)]
    struct Count {
        enabled: Cell<u32>,
        disabled: Cell<u32>,
    }

    impl Switch for &Count {
        fn enable(&self) {
            self.enabled.set(self.enabled.get().saturating_add(1));
        }

        fn disable(&self) {
            assert!(self.disabled.get() < self.enabled.get(), "a disable with no enable");
            self.disabled.set(self.disabled.get().saturating_add(1));
        }
    }

    impl Count {
        fn held(&self) -> u32 {
            self.enabled.get().saturating_sub(self.disabled.get())
        }
    }

    /// Every turn on is matched by one turn off, whatever the sequence of asks: the count is
    /// never above one, never below zero, and back at zero once it drops while on.
    #[test]
    fn secure_input_is_balanced_whatever_is_asked() {
        // Every sequence of four asks.
        for bits in 0_u8..16 {
            let count = Count::default();
            {
                let mut secure = SecureInput::new(&count);
                for at in 0..4 {
                    let on = bits & (1 << at) != 0;
                    secure.set(on);
                    assert_eq!(count.held(), u32::from(on), "after {bits:04b} to {at}");
                    assert_eq!(secure.is_on(), on);
                }
            }
            assert_eq!(count.held(), 0, "dropped after {bits:04b}");
        }
        let count = Count::default();
        let mut secure = SecureInput::new(&count);
        secure.set(true);
        secure.set(true);
        secure.set(false);
        secure.set(false);
        drop(secure);
        assert_eq!((count.enabled.get(), count.disabled.get()), (1, 1), "only changes reach it");
    }
}
