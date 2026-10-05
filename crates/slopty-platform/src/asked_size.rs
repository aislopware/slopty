//! A window that keeps the size it was asked for on any screen, for the self-test only.
//!
//! AppKit fits a window into its screen's visible frame when it shows the window and when a
//! frame is set (`-[NSWindow constrainFrameRect:toScreen:]`). A hosted CI Mac's screen is
//! 1024 × 768, so every self-test window taller than 677 pt came out 677 pt tall there, and 31
//! renders differed from their goldens, which were taken on a taller screen (CI e2e runs
//! 37359580819 and 37379213283). The self-test answers that method on its window class with the
//! frame unchanged, so a render has its golden's size on any screen
//! (`docs/decisions/testing.md`, "A self-test window keeps its size on any screen"). A person's
//! build never calls this: a window larger than the screen is no use to a person.

/// Make the `NSWindow` subclass named `class` keep every frame it is given, on any screen.
///
/// Returns whether it now does: false when no class has that name, or when the class answers
/// the method itself already.
#[cfg(target_os = "macos")]
pub fn keep_asked_sizes(class: &std::ffi::CStr) -> bool {
    use objc2::encode::Encode as _;
    use objc2::runtime::{AnyClass, AnyObject, Imp, Sel};
    use objc2_foundation::NSRect;

    /// `-constrainFrameRect:toScreen:`, answering with the frame asked for.
    const extern "C-unwind" fn as_asked(
        _window: &AnyObject,
        _cmd: Sel,
        frame: NSRect,
        _screen: *mut AnyObject,
    ) -> NSRect {
        frame
    }

    let Some(found) = AnyClass::get(class) else { return false };
    let Ok(types) = std::ffi::CString::new(format!("{}@:{}@", NSRect::ENCODING, NSRect::ENCODING))
    else {
        return false;
    };
    let as_asked: extern "C-unwind" fn(&AnyObject, Sel, NSRect, *mut AnyObject) -> NSRect =
        as_asked;
    // SAFETY: Objective-C runtime rule: a method's implementation is called through `IMP`, a
    // pointer to a C function taking the receiver and the selector first, then the method's
    // arguments as `types` encodes them, which is `as_asked`'s signature (`objc/runtime.h`,
    // `class_addMethod`); objc2's own `ClassBuilder::add_method` casts the same way.
    let imp = unsafe {
        std::mem::transmute::<
            extern "C-unwind" fn(&AnyObject, Sel, NSRect, *mut AnyObject) -> NSRect,
            Imp,
        >(as_asked)
    };
    let selector = objc2::sel!(constrainFrameRect:toScreen:);
    // SAFETY: Objective-C runtime rule: `class_addMethod` adds the method to `found` alone, and
    // leaves one the class itself defines as it is (it returns NO then); `types` is a
    // NUL-terminated encoding that outlives the call, and the runtime copies it.
    let added = unsafe {
        objc2::ffi::class_addMethod(
            std::ptr::from_ref(found).cast_mut(),
            selector,
            imp,
            types.as_ptr(),
        )
    };
    added.as_bool()
}

/// No other platform fits a window into its screen for it.
#[cfg(not(target_os = "macos"))]
pub const fn keep_asked_sizes(_class: &std::ffi::CStr) -> bool {
    false
}
