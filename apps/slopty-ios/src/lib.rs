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
    slopty_platform::playback_audio_session();
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
    gpui_ios::ios::ffi::run_app_with_assets(slopty_ui::icons::Assets);
    opened.get()
}

/// Logs go to stderr, which `simctl launch --console-pty` and `devicectl --console` stream.
/// Under the self-test (`SLOPTY_TEST_SOCKET` with `SLOPTY_DATA_DIR`) they are mirrored, with
/// any panic, into `<data dir>/app.log`: `simctl launch` without a console sends the app's
/// stderr nowhere a test can read, and a test that lost the socket needs the reason.
fn init_logging() {
    use tracing_subscriber::fmt::writer::MakeWriterExt as _;

    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let mirror = std::env::var_os(slopty_e2e::SOCKET_ENV)
        .and_then(|_| std::env::var_os("SLOPTY_DATA_DIR"))
        .map(|dir| std::path::PathBuf::from(dir).join("app.log"))
        .and_then(|path| std::fs::File::options().create(true).append(true).open(path).ok())
        .map(std::sync::Arc::new);
    let panics = mirror.clone();
    if let Some(file) = panics {
        let default = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            use std::io::Write as _;
            let _written = writeln!(&*file, "panic: {info}");
            default(info);
        }));
    }
    let builder = tracing_subscriber::fmt().with_env_filter(filter).with_ansi(false);
    let _already = match mirror {
        Some(file) => builder.with_writer(std::io::stderr.and(file)).try_init(),
        None => builder.with_writer(std::io::stderr).try_init(),
    };
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
