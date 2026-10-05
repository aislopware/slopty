//! Opening Slopty at login: the app as its own login item (`SMAppService.mainApp`).
//!
//! The system holds whether the app opens at login, and the person can change it in System
//! Settings, under Login Items, as well as in Slopty. So [`status`] reads the system's answer
//! each time rather than a setting keeping a copy that would drift from it. [`set`] registers or
//! unregisters the app. [`open_settings`] goes to the Login Items list, where an app turned off
//! there is allowed again: registering it once more does not.
//!
//! Registering posts the system's own "Login item added" note, so no test calls [`set`];
//! [`status`] only reads. Each call makes its own service object, so a caller may run it on
//! any thread: registering asks a system daemon, which can take a moment.
//!
//! Only a Mac has login items; elsewhere there is only [`Login`], which says how one stands.

#[cfg(target_os = "macos")]
use objc2_service_management::{SMAppService, SMAppServiceStatus};

/// Whether the app opens at login.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Login {
    /// It opens at login.
    On,
    /// It does not.
    Off,
    /// It is registered, but turned off in System Settings, under Login Items: it opens at
    /// login once allowed there ([`open_settings`]).
    Blocked,
    /// This build is not an app the system can open at login: a binary outside a bundle.
    Unavailable,
}

impl Login {
    /// Whether it opens at login.
    #[must_use]
    pub const fn on(self) -> bool {
        matches!(self, Self::On)
    }
}

/// Whether the app opens at login, as the system has it now.
#[must_use]
#[cfg(target_os = "macos")]
pub fn status() -> Login {
    of(&main_app())
}

/// Open the app at login, or stop, and say how it stands after.
///
/// A login item already as asked is no failure, and neither is one turned off in System
/// Settings, which registering cannot allow again: its [`Login::Blocked`] says to go there.
///
/// # Errors
///
/// The system's words when it did not, as for an app whose signature is broken.
#[cfg(target_os = "macos")]
pub fn set(on: bool) -> Result<Login, String> {
    let service = main_app();
    let done = if on {
        // SAFETY: ServiceManagement rule (SMAppService.h): `register` takes no argument but the
        // error out-parameter objc2 passes, and may be called from any thread.
        unsafe { service.registerAndReturnError() }
    } else {
        // SAFETY: as `register`, its opposite.
        unsafe { service.unregisterAndReturnError() }
    };
    let now = of(&service);
    match done {
        Ok(()) => Ok(now),
        // Already registered or already gone (`kSMErrorAlreadyRegistered`,
        // `kSMErrorJobNotFound`), or registered and waiting on the person: the status says it.
        Err(_) if now.on() == on || now == Login::Blocked => Ok(now),
        Err(error) => Err(error.localizedDescription().to_string()),
    }
}

/// Open System Settings at Login Items, where a login item turned off there is allowed again.
#[cfg(target_os = "macos")]
pub fn open_settings() {
    // SAFETY: ServiceManagement rule (SMAppService.h): a class method with no arguments, which
    // only opens System Settings.
    unsafe {
        SMAppService::openSystemSettingsLoginItems();
    }
}

/// The service that is this app itself.
#[cfg(target_os = "macos")]
fn main_app() -> objc2::rc::Retained<SMAppService> {
    // SAFETY: ServiceManagement rule (SMAppService.h): `mainAppService` takes no arguments and
    // returns the service for the main bundle, whatever that bundle is.
    unsafe { SMAppService::mainAppService() }
}

/// How `service` stands.
#[cfg(target_os = "macos")]
fn of(service: &SMAppService) -> Login {
    // SAFETY: ServiceManagement rule (SMAppService.h): `status` only reads.
    match unsafe { service.status() } {
        SMAppServiceStatus::Enabled => Login::On,
        SMAppServiceStatus::NotRegistered => Login::Off,
        SMAppServiceStatus::RequiresApproval => Login::Blocked,
        _ => Login::Unavailable,
    }
}
