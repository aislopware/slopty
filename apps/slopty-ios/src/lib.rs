//! The Slopty iOS app.
//!
//! A static library linked by the UIKit shell in `app/main.m` (the one Objective-C file in the
//! tree: `UIApplicationMain` has to be driven from there). The scene delegate calls
//! `slopty_ios_run` once the window scene is connected; it starts the tokio runtime, registers
//! the GPUI application callback and runs GPUI embedded in UIKit's run loop. Everything the app
//! does lives in `slopty-app`, shared with the macOS app.

#![cfg(target_os = "ios")]

use std::cell::Cell;
use std::rc::Rc;

use gpui::WindowOptions;

/// Entry point for the UIKit shell. Returns whether the workspace window opened.
#[unsafe(no_mangle)]
pub extern "C" fn slopty_ios_run() -> bool {
    init_logging();
    let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(e) => {
            tracing::error!(error = %e, "tokio runtime");
            return false;
        }
    };
    // The runtime lives as long as the process; UIKit never returns from `UIApplicationMain`.
    let handle = Box::leak(Box::new(runtime)).handle().clone();
    let opened = Rc::new(Cell::new(false));
    let flag = Rc::clone(&opened);
    gpui_ios::ios::ffi::set_app_callback(Box::new(move |cx| {
        gpui_kit::init(cx);
        match slopty_app::open_workspace(cx, handle, WindowOptions::default()) {
            Ok(()) => flag.set(true),
            Err(e) => tracing::error!(error = %e, "open workspace"),
        }
    }));
    gpui_ios::ios::ffi::run_app();
    opened.get()
}

/// Logs go to stderr, which `simctl launch --console-pty` and `devicectl --console` stream.
fn init_logging() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,iroh::_events::path=debug"));
    let _already = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .try_init();
}

/// Stand-ins for CGL (macOS OpenGL) entry points that do not exist on iOS.
///
/// The `io-surface` crate, pulled in by `core-video` for the zero-copy video path, references
/// them from `IOSurface::bind_to_gl_texture`, which nothing calls on iOS. dyld binds every
/// import at load (chained fixups), so leaving them undefined kills the process at launch:
/// give the linker definitions that abort if ever reached.
mod cgl_stubs {
    use std::ffi::{c_char, c_int, c_void};

    fn missing(name: &str) -> ! {
        tracing::error!(symbol = name, "CGL is not available on iOS");
        std::process::abort()
    }

    #[unsafe(no_mangle)]
    extern "C" fn CGLErrorString(_error: c_int) -> *const c_char {
        missing("CGLErrorString")
    }

    #[unsafe(no_mangle)]
    extern "C" fn CGLGetCurrentContext() -> *mut c_void {
        missing("CGLGetCurrentContext")
    }

    #[unsafe(no_mangle)]
    extern "C" fn CGLTexImageIOSurface2D(
        _ctx: *mut c_void,
        _target: u32,
        _internal_format: u32,
        _width: i32,
        _height: i32,
        _format: u32,
        _kind: u32,
        _surface: *mut c_void,
        _plane: u32,
    ) -> c_int {
        missing("CGLTexImageIOSurface2D")
    }
}
