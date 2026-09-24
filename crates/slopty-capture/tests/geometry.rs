//! Window-list geometry against the live WindowServer; gated by `SLOPTY_SCREEN_E2E=1`.

#[cfg(test)]
mod tests {
    use objc2_core_foundation::{CFArray, CFDictionary, CFNumber, CFRetained, CFString, CFType};
    use objc2_core_graphics::{
        CGWindowListCopyWindowInfo, CGWindowListOption, kCGWindowNumber, kCGWindowOwnerPID,
    };
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
    /// One window-list description answers bounds, on-screen state and owner alike.
    #[test]
    fn window_state_agrees_with_the_single_reads() {
        if std::env::var_os("SLOPTY_SCREEN_E2E").is_none() {
            eprintln!("skipped: set SLOPTY_SCREEN_E2E=1");
            return;
        }
        let id = any_on_screen_window().expect("an on-screen window");
        let state = slopty_capture::window_state(id).expect("state");
        assert_eq!(Some(state.bounds), slopty_capture::window_bounds(id));
        assert_eq!(state.on_screen, slopty_capture::window_on_screen(id));
        assert_eq!(Some(state.owner_pid), slopty_capture::window_owner_pid(id));
        assert!(slopty_capture::window_state(WindowId(u32::MAX)).is_none());
    }

    /// The idle window from `slopty-e2e`, built beside this test binary; quit on drop.
    struct Idle(std::process::Child, std::path::PathBuf);

    impl Drop for Idle {
        fn drop(&mut self) {
            let _quit = std::fs::write(self.1.join("quit"), b"");
            let _reaped = self.0.wait();
        }
    }

    #[expect(clippy::disallowed_methods, reason = "a test thread polling another process")]
    fn idle_window(dir: &std::path::Path) -> (Idle, WindowId) {
        let exe = std::env::current_exe().unwrap();
        let profile = exe.parent().and_then(std::path::Path::parent).unwrap();
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let mut build = std::process::Command::new(cargo);
        build.args(["build", "-p", "slopty-e2e", "--bin", "slopty-idle-window"]);
        if profile.ends_with("release") {
            build.arg("--release");
        }
        assert!(build.status().unwrap().success(), "build the idle window");
        let child = std::process::Command::new(profile.join("slopty-idle-window"))
            .arg(dir)
            .arg("slopty geometry cost")
            .spawn()
            .unwrap();
        let pid = i32::try_from(child.id()).unwrap();
        let idle = Idle(child, dir.to_path_buf());
        let started = std::time::Instant::now();
        while !dir.join("ready").exists() {
            assert!(started.elapsed().as_secs() < 20, "the idle window never came up");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let list = CGWindowListCopyWindowInfo(CGWindowListOption::OptionOnScreenOnly, 0).unwrap();
        // SAFETY: the list is documented as dictionaries keyed by `kCGWindow*` strings.
        let list: CFRetained<CFArray<CFDictionary<CFString, CFType>>> =
            unsafe { CFRetained::cast_unchecked(list) };
        // SAFETY: framework constants.
        let (number, owner): (&CFString, &CFString) =
            unsafe { (kCGWindowNumber, kCGWindowOwnerPID) };
        let id = list
            .iter()
            .find_map(|d| {
                let of = |key: &CFString| d.get(key)?.downcast::<CFNumber>().ok()?.as_i64();
                if of(owner)? != i64::from(pid) {
                    return None;
                }
                Some(WindowId(u32::try_from(of(number)?).ok()?))
            })
            .expect("the idle window in the window list");
        (idle, id)
    }

    /// What one geometry tick of a window stream costs in window-server calls: the reads the
    /// stream made before (bounds, on-screen, owner each a description of their own, plus the
    /// cursor loop's own bounds read at the same rate) against one description for all three.
    /// Both sets include the occlusion list and the display lookup, which are unchanged.
    #[test]
    #[ignore = "measurement"]
    fn geometry_tick_cost() {
        let dir = tempfile::tempdir().unwrap();
        let (_idle, id) = idle_window(dir.path());
        let target = slopty_proto::screen::CaptureTarget::Window(id);
        let crop_inputs = |bounds: &slopty_capture::Rect, pid: i32| {
            let display =
                slopty_capture::display_enclosing(bounds).map(slopty_capture::display_bounds);
            (display, slopty_capture::occluded(id, bounds, pid))
        };
        let separate = || {
            let bounds = slopty_capture::target_bounds(target).unwrap();
            let on_screen = slopty_capture::window_on_screen(id);
            let pid = slopty_capture::window_owner_pid(id).unwrap();
            let crop = crop_inputs(&bounds, pid);
            let cursor = slopty_capture::target_bounds(target);
            std::hint::black_box((on_screen, crop, cursor));
        };
        let together = || {
            let state = slopty_capture::window_state(id).unwrap();
            let crop = crop_inputs(&state.bounds, state.owner_pid);
            std::hint::black_box((state.on_screen, crop));
        };
        let ticks: [(&str, &dyn Fn()); 2] =
            [("separate reads", &separate), ("one description", &together)];
        for (label, tick) in ticks {
            let mut took: Vec<f64> = std::iter::repeat_with(|| {
                let started = std::time::Instant::now();
                tick();
                started.elapsed().as_secs_f64() * 1e6
            })
            .take(2_000)
            .collect();
            took.sort_by(f64::total_cmp);
            let at = |q: usize| took.get(took.len().saturating_sub(1) * q / 100).copied();
            eprintln!(
                "{label}: p50 {:.1} / p99 {:.1} / max {:.1} µs per tick",
                at(50).unwrap_or_default(),
                at(99).unwrap_or_default(),
                at(100).unwrap_or_default(),
            );
        }
    }
}
