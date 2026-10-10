//! Slopty's notification service extension, the phone's half of a push
//! (`docs/decisions/platform.md`, "Notes reach a pocketed phone").
//!
//! APNs wakes it for each push the server sends (`mutable-content`) before the note shows. It
//! opens the sealed body with the phone's key and the device token the app kept in the
//! Keychain they share, and hands back the note the app would have posted
//! (`slopty_platform::notify::pushed`). On any failure it hands back what it was given, whose
//! fixed words ("An agent needs you") the relay wrote.
//!
//! The bundle's `NSExtensionPrincipalClass` names `Service` (iOS only), which Foundation looks
//! up by name as the extension starts. A class `define_class!` makes is registered on its first
//! use, so an image initializer registers it as the binary loads, before anything looks; `main`
//! does too, for a link that enters there.

#![cfg(target_os = "ios")]

use std::ffi::{CString, c_char, c_int};
use std::ptr::NonNull;

use block2::{DynBlock, RcBlock};
use objc2::rc::Retained;
use objc2::{ClassType as _, define_class};
use objc2_foundation::{NSObject, NSString};
use objc2_user_notifications::{
    UNNotificationContent, UNNotificationRequest, UNNotificationServiceExtension,
};
use slopty_platform::notify::pushed;

unsafe extern "C" {
    /// Foundation's entry point for an app extension: it connects to the system and serves
    /// the principal class the bundle's `NSExtension` names, and never returns.
    fn NSExtensionMain(argc: c_int, argv: *mut *mut c_char) -> c_int;
}

/// The extension's entry, for a link that enters at `main` rather than at `NSExtensionMain`.
#[unsafe(no_mangle)]
pub extern "C" fn main(_argc: c_int, _argv: *const *const c_char) -> c_int {
    register();
    let args: Vec<CString> =
        std::env::args_os().filter_map(|arg| CString::new(arg.into_encoded_bytes()).ok()).collect();
    let mut argv: Vec<*mut c_char> = args
        .iter()
        .map(|arg| arg.as_ptr().cast_mut())
        .chain(std::iter::once(std::ptr::null_mut()))
        .collect();
    let argc = c_int::try_from(args.len()).unwrap_or(c_int::MAX);
    // SAFETY: Foundation's rule for `NSExtensionMain`: called once, from `main`, with the
    // process's arguments, a null-terminated array whose strings outlive the call, which
    // never returns while the extension runs.
    unsafe { NSExtensionMain(argc, argv.as_mut_ptr()) }
}

/// Register [`Service`] with the Objective-C runtime.
extern "C" fn register() {
    let _registered = Service::class();
}

/// [`register`], run by dyld as the binary loads.
///
/// SAFETY (the loader's rule): dyld calls every pointer in `__DATA,__mod_init_func` once, on
/// the loading thread, after the images this one links (libobjc, Foundation,
/// `UserNotifications`) have run their own initializers, so the runtime and the superclass are
/// there to register against. `register` takes no arguments and does not unwind.
#[used]
#[unsafe(link_section = "__DATA,__mod_init_func")]
static REGISTER: extern "C" fn() = register;

define_class!(
    // SAFETY:
    // - `UNNotificationServiceExtension` asks of a subclass only that it override the two
    //   methods below, and call the handler once.
    // - `Service` does not implement `Drop`.
    #[unsafe(super(UNNotificationServiceExtension, NSObject))]
    #[name = "SloptyNotificationService"]
    struct Service;

    impl Service {
        /// Open the push and hand back its note. A question's options as its buttons want
        /// their category registered first, which takes the centre a moment; anything else is
        /// handed back at once.
        #[unsafe(method(didReceiveNotificationRequest:withContentHandler:))]
        fn did_receive(
            &self,
            request: &UNNotificationRequest,
            handler: &DynBlock<dyn Fn(NonNull<UNNotificationContent>)>,
        ) {
            let given = request.content();
            let (content, note) = match note(&given) {
                Ok(note) => {
                    let content = slopty_platform::notify::content_of(&note);
                    (Retained::into_super(content), Some(note))
                }
                Err(why) => {
                    tracing::warn!(error = %why, "a push was shown as it came");
                    (given, None)
                }
            };
            let answer = Answer { handler: handler.copy(), content };
            match note {
                Some(note) => slopty_platform::notify::register_then(&note, move || answer.give()),
                None => answer.give(),
            }
        }

        /// The category's registration takes the centre well under the time the system
        /// grants. Should it never answer, the system shows the push as it came once the time
        /// is up, its relay's fixed words with no buttons.
        #[unsafe(method(serviceExtensionTimeWillExpire))]
        fn will_expire(&self) {}
    }
);

/// The note to hand back, and the handler it goes to, carried to the centre's queue when a
/// category is registered first.
struct Answer {
    handler: RcBlock<dyn Fn(NonNull<UNNotificationContent>)>,
    content: Retained<UNNotificationContent>,
}

#[expect(
    clippy::non_send_fields_in_send_ty,
    reason = "the handler is only called and the content only read, as UserNotifications allows \
              from any thread"
)]
// SAFETY: UserNotifications rule: a service extension's content handler may be called from any
// thread, once; the block is a heap copy, immutable once made. The content is a finished note
// that nothing changes after it is made, and is only handed to the handler.
unsafe impl Send for Answer {}

impl Answer {
    /// Hand the note to the system: called once.
    fn give(self) {
        self.handler.call((NonNull::from(&*self.content),));
    }
}

/// The note `given`'s sealed body opens to.
fn note(given: &UNNotificationContent) -> Result<slopty_platform::notify::Note, pushed::Kept> {
    let info = given.userInfo();
    pushed::note_from(|key| {
        let value = info.objectForKey(&NSString::from_str(key))?;
        value.downcast::<NSString>().ok().map(|s| s.to_string())
    })
}
