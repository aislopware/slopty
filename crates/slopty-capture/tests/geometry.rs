//! Window-list geometry against the live WindowServer; gated by `SLOPTY_SCREEN_E2E=1`.

#[cfg(test)]
mod tests {
    use objc2_core_foundation::{CFArray, CFDictionary, CFNumber, CFRetained, CFString, CFType};
    use objc2_core_graphics::{CGWindowListCopyWindowInfo, CGWindowListOption, kCGWindowNumber};
    use slopty_core::WindowId;

    /// Some on-screen window id from the raw list, so the test does not depend on what is open.
    fn any_on_screen_window() -> Option<WindowId> {
        let list = CGWindowListCopyWindowInfo(CGWindowListOption::OptionOnScreenOnly, 0)?;
        // SAFETY: the list is documented as dictionaries keyed by `kCGWindow*` strings.
        let list: CFRetained<CFArray<CFDictionary<CFString, CFType>>> =
            unsafe { CFRetained::cast_unchecked(list) };
        // SAFETY: framework constant.
        let key: &CFString = unsafe { kCGWindowNumber };
        list.iter().find_map(|d| {
            let n: CFRetained<CFNumber> = d.get(key)?.downcast().ok()?;
            let id = u32::try_from(n.as_i64()?).ok()?;
            Some(WindowId(id))
        })
    }

    #[test]
    fn window_bounds_and_owner_come_back_for_a_real_window() {
        if std::env::var_os("SLOPTY_SCREEN_E2E").is_none() {
            eprintln!("skipped: set SLOPTY_SCREEN_E2E=1");
            return;
        }
        let id = any_on_screen_window().expect("an on-screen window");
        let bounds = slopty_capture::window_bounds(id).expect("bounds");
        assert!(bounds.w > 0.0 && bounds.h > 0.0, "{bounds:?}");
        let pid = slopty_capture::window_owner_pid(id).expect("owner pid");
        assert!(pid > 0);
        assert!(slopty_capture::window_bounds(WindowId(u32::MAX)).is_none());
    }
}
