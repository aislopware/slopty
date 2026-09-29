//! The keyboard input source (layout or input method) of this Mac, by its TIS id
//! (`com.apple.keylayout.French`, `com.apple.inputmethod.VietnameseIM.VietnameseSimpleTelex`).
//!
//! The client reads its own ([`current`]) and hears when the person switches ([`Watch`]); the
//! worker selects the client's ([`select`]) so a key's position means there what it means on
//! the client, and puts its own back once no client needs it (`docs/decisions/input.md`, "Keys
//! go by position"). Text Input Sources Services is `HIToolbox`'s and answers on the main thread
//! only (it asserts the queue on macOS 14 and later), so every call here checks for it.

use std::ffi::c_void;
use std::ptr::NonNull;

use objc2::MainThreadMarker;
use objc2_core_foundation::{
    CFArray, CFBoolean, CFDictionary, CFNotificationCenter, CFNotificationName,
    CFNotificationSuspensionBehavior, CFRetained, CFString, CFType,
};

/// An input source (`TISInputSourceRef`), a CoreFoundation object.
type Source = c_void;

// `<HIToolbox/TextInputSources.h>`, in Carbon; objc2 binds none of it.
#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    static kTISPropertyInputSourceID: &'static CFString;
    static kTISPropertyInputSourceIsEnabled: &'static CFString;
    static kTISPropertyInputSourceIsSelectCapable: &'static CFString;
    static kTISNotifySelectedKeyboardInputSourceChanged: &'static CFString;
    fn TISCopyCurrentKeyboardInputSource() -> *mut Source;
    fn TISGetInputSourceProperty(source: *const Source, key: &CFString) -> *const c_void;
    fn TISCreateInputSourceList(properties: &CFDictionary, include_all: u8) -> *mut CFArray;
    fn TISEnableInputSource(source: *const Source) -> i32;
    fn TISDisableInputSource(source: *const Source) -> i32;
    fn TISSelectInputSource(source: *const Source) -> i32;
}

/// Why an input source was not selected.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SelectError {
    /// Called off the main thread.
    NotMain,
    /// No input source has that id on this Mac.
    Unknown,
    /// It exists but cannot be selected (a palette, or one macOS will not enable).
    NotSelectable,
    /// macOS refused: the `OSStatus` it answered.
    Refused(i32),
}

impl std::fmt::Display for SelectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotMain => f.write_str("input sources are selected on the main thread"),
            Self::Unknown => f.write_str("no such input source"),
            Self::NotSelectable => f.write_str("the input source cannot be selected"),
            Self::Refused(status) => write!(f, "macOS refused the input source ({status})"),
        }
    }
}

impl std::error::Error for SelectError {}

/// A `Copy`/`Create` answer, owned: released when dropped.
fn owned<T: objc2_core_foundation::Type>(ptr: *mut T) -> Option<CFRetained<T>> {
    // SAFETY: TIS's `Copy` and `Create` functions return a +1 reference or null (the Create
    // Rule), which `from_raw` takes over.
    NonNull::new(ptr).map(|ptr| unsafe { CFRetained::from_raw(ptr) })
}

/// The id of `source`.
fn id_of(source: &CFType) -> Option<String> {
    let source = std::ptr::from_ref(source).cast::<Source>();
    // SAFETY: `source` is a live input source; the key is HIToolbox's own constant, and the
    // answer is a CFString the source owns (the Get Rule), read before the source goes.
    let id = unsafe { TISGetInputSourceProperty(source, kTISPropertyInputSourceID) };
    // SAFETY: `kTISPropertyInputSourceID`'s value is a CFStringRef, or null.
    unsafe { id.cast::<CFString>().as_ref() }.map(ToString::to_string)
}

/// Whether the boolean property `key` of `source` is true.
fn flag(source: *const Source, key: &CFString) -> bool {
    // SAFETY: `source` is a live input source; boolean properties are CFBooleans it owns.
    let value = unsafe { TISGetInputSourceProperty(source, key) };
    // SAFETY: the value is a CFBooleanRef, or null.
    unsafe { value.cast::<CFBoolean>().as_ref() }.is_some_and(CFBoolean::as_bool)
}

/// This Mac's selected keyboard input source; `None` off the main thread.
#[must_use]
pub fn current() -> Option<String> {
    MainThreadMarker::new()?;
    // SAFETY: on the main thread, as TIS requires; the answer follows the Create Rule.
    let source = owned(unsafe { TISCopyCurrentKeyboardInputSource() }.cast::<CFType>())?;
    id_of(&source)
}

/// The installed input source `id`, enabled or not. Main thread only.
fn find(id: &str) -> Result<CFRetained<CFType>, SelectError> {
    MainThreadMarker::new().ok_or(SelectError::NotMain)?;
    // SAFETY: an immutable CFString HIToolbox defines and never frees.
    let key: &CFString = unsafe { kTISPropertyInputSourceID };
    let value = CFString::from_str(id);
    let filter = CFDictionary::<CFString, CFString>::from_slices(&[key], &[&value]);
    // SAFETY: on the main thread; `filter` maps a property key to its value as TIS asks, and
    // the list follows the Create Rule. Installed sources that are off are included (`1`).
    let list = owned(unsafe { TISCreateInputSourceList(filter.as_opaque(), 1) })
        .ok_or(SelectError::Unknown)?;
    // SAFETY: the list holds input sources, CoreFoundation objects.
    let list = unsafe { CFRetained::cast_unchecked::<CFArray<CFType>>(list) };
    list.get(0).ok_or(SelectError::Unknown)
}

/// Select the input source `id`; whether it had to be enabled first. Main thread only.
///
/// An installed source that is off (a bundled layout or input method the person never added)
/// is enabled, which the caller undoes with [`disable`] once it is done with it.
///
/// # Errors
///
/// Off the main thread, an id this Mac does not have, one it cannot select, or macOS's refusal.
pub fn select(id: &str) -> Result<bool, SelectError> {
    let source = find(id)?;
    let source = CFRetained::as_ptr(&source).as_ptr().cast::<Source>();
    // SAFETY: immutable CFStrings HIToolbox defines and never frees.
    let (selectable, enabled) =
        unsafe { (kTISPropertyInputSourceIsSelectCapable, kTISPropertyInputSourceIsEnabled) };
    if !flag(source, selectable) {
        return Err(SelectError::NotSelectable);
    }
    let enabling = !flag(source, enabled);
    if enabling {
        // SAFETY: on the main thread, with a live input source from the list.
        let status = unsafe { TISEnableInputSource(source) };
        if status != 0 {
            return Err(SelectError::Refused(status));
        }
    }
    // SAFETY: on the main thread, with a live input source from the list.
    match unsafe { TISSelectInputSource(source) } {
        0 => Ok(enabling),
        status => Err(SelectError::Refused(status)),
    }
}

/// Turn the input source `id` off again, as [`select`] found it. Main thread only.
///
/// # Errors
///
/// Off the main thread, an id this Mac does not have, or macOS's refusal.
pub fn disable(id: &str) -> Result<(), SelectError> {
    let source = find(id)?;
    let source = CFRetained::as_ptr(&source).as_ptr().cast::<Source>();
    // SAFETY: on the main thread, with a live input source from the list.
    match unsafe { TISDisableInputSource(source) } {
        0 => Ok(()),
        status => Err(SelectError::Refused(status)),
    }
}

/// Calls its closure whenever this Mac's keyboard input source changes, until dropped. Made
/// and dropped on the main thread, where the notification is delivered.
pub struct Watch {
    observer: NonNull<Box<dyn Fn()>>,
    center: CFRetained<CFNotificationCenter>,
}

impl std::fmt::Debug for Watch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Watch").finish_non_exhaustive()
    }
}

impl Watch {
    /// Call `changed` on each switch; `None` off the main thread.
    #[must_use]
    pub fn new(changed: Box<dyn Fn()>) -> Option<Self> {
        MainThreadMarker::new()?;
        let center = CFNotificationCenter::distributed_center()?;
        let observer = NonNull::from(Box::leak(Box::new(changed)));
        // SAFETY: `observer` stays alive until `Drop` removes it from the center; the name is
        // HIToolbox's own constant; `heard` matches `CFNotificationCallback`.
        unsafe {
            center.add_observer(
                observer.as_ptr().cast_const().cast(),
                Some(heard),
                Some(kTISNotifySelectedKeyboardInputSourceChanged),
                std::ptr::null(),
                CFNotificationSuspensionBehavior::DeliverImmediately,
            );
        }
        Some(Self { observer, center })
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        // SAFETY: removes every registration of this observer, so the center calls it no more.
        unsafe {
            self.center.remove_observer(
                self.observer.as_ptr().cast_const().cast(),
                None,
                std::ptr::null(),
            );
        }
        // SAFETY: the observer came from `Box::leak` in `new`, and nothing calls it now.
        drop(unsafe { Box::from_raw(self.observer.as_ptr()) });
    }
}

/// The notification's callback: the observer is the watch's closure.
unsafe extern "C-unwind" fn heard(
    _center: *mut CFNotificationCenter,
    observer: *mut c_void,
    _name: *const CFNotificationName,
    _object: *const c_void,
    _info: *const CFDictionary,
) {
    // SAFETY: `observer` is the `Box<dyn Fn()>` a live `Watch` registered.
    if let Some(changed) = unsafe { observer.cast::<Box<dyn Fn()>>().as_ref() } {
        changed();
    }
}

#[cfg(test)]
mod tests {
    use super::{SelectError, current, disable, select};

    /// Off the main thread (where the test harness runs tests) nothing is read or selected,
    /// rather than tripping `HIToolbox`'s queue assertion.
    #[test]
    fn off_the_main_thread_nothing_is_asked() {
        if objc2::MainThreadMarker::new().is_some() {
            return;
        }
        assert_eq!(current(), None);
        assert_eq!(select("com.apple.keylayout.US"), Err(SelectError::NotMain));
        assert_eq!(disable("com.apple.keylayout.US"), Err(SelectError::NotMain));
    }
}
